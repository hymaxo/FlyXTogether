//! Cockpit spike (dev only): what the C172 cockpit is made of and which
//! parts a plugin can sync. Four tools, each reporting to FlyXTogether.log:
//!
//! - **Watch** (toggle): logs every press and release of the commands the
//!   loaded aircraft's cockpit uses, and every dataref that changes after a
//!   5 s baseline (datarefs that change on their own during the baseline
//!   are ignored). Click through the cockpit while it runs.
//! - **Write test**: writes a changed value to each candidate dataref and
//!   checks one and thirty frames later whether it stayed, then restores it.
//! - **Replay test**: checks whether command handlers run synchronously
//!   inside `XPLMCommandOnce`/`XPLMCommandBegin`.
//! - **Follower test**: holds the aircraft with the flight-model override,
//!   writes flight-control inputs with and without the input overrides,
//!   then replays a held START on the key under the override.

use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;
use std::path::Path;
use std::rc::Rc;

use flyx_xplm::command::{Command, CommandHandler, Phase as CommandPhase};
use flyx_xplm::dataref::{ArrayRef, DataRef};
use flyx_xplm::dynref::{DynRef, Value, all_datarefs};
use tracing::{info, warn};

// ---------------------------------------------------------------- watch

/// Dataref name prefixes the watch polls.
const WATCH_PREFIXES: &[&str] = &[
    "sim/cockpit2/",
    "sim/cockpit/",
    "sim/flightmodel/weight/",
    "sim/operation/override/",
    "laminar/",
];
/// Array elements polled per array dataref.
const WATCH_MAX_ELEMENTS: usize = 16;
const BASELINE_FRAMES: u32 = 300;
/// A change is reported once the value has been stable this many frames.
const SETTLE_FRAMES: u32 = 20;
/// A key reported more often than this within `NOISY_WINDOW` is muted.
const NOISY_REPORTS: u32 = 6;
const NOISY_WINDOW: u32 = 600;

struct WatchKey {
    label: String,
    handle: DynRef,
    last: Value,
    noisy: bool,
    /// Value before the change being settled, and the frame of the latest change.
    settling: Option<(Value, u32)>,
    reports: u32,
    window_start: u32,
}

type CommandLog = Rc<RefCell<Vec<(usize, CommandPhase)>>>;

pub struct Watch {
    frame: u32,
    keys: Vec<WatchKey>,
    commands: Vec<String>,
    _handlers: Vec<CommandHandler>,
    log: CommandLog,
    baseline_done: bool,
}

impl Watch {
    pub fn start() -> Watch {
        let commands = cockpit_commands();
        let log: CommandLog = Rc::default();
        let mut handlers = Vec::new();
        let mut missing = 0;
        for (i, name) in commands.iter().enumerate() {
            let Some(command) = Command::find(name) else {
                missing += 1;
                continue;
            };
            let log = log.clone();
            handlers.push(CommandHandler::register(command, true, move |phase| {
                if phase != CommandPhase::Continue {
                    log.borrow_mut().push((i, phase));
                }
                true
            }));
        }
        let mut keys = Vec::new();
        for info in all_datarefs() {
            if !info.is_numeric() || !WATCH_PREFIXES.iter().any(|p| info.name.starts_with(p)) {
                continue;
            }
            if info.is_array() {
                let Ok(first) = DynRef::find(&info.name, Some(0)) else {
                    continue;
                };
                for index in 0..first.len().min(WATCH_MAX_ELEMENTS) {
                    if let Ok(handle) = DynRef::find(&info.name, Some(index)) {
                        keys.push(watch_key(format!("{}[{index}]", info.name), handle));
                    }
                }
            } else if let Ok(handle) = DynRef::find(&info.name, None) {
                keys.push(watch_key(info.name.clone(), handle));
            }
        }
        info!(
            commands = handlers.len(),
            commands_missing = missing,
            datarefs = keys.len(),
            "cockpit watch: started; learning the baseline for 5 s, do not touch anything"
        );
        Watch {
            frame: 0,
            keys,
            commands,
            _handlers: handlers,
            log,
            baseline_done: false,
        }
    }

    pub fn frame(&mut self) {
        self.frame += 1;
        let frame = self.frame;
        for (i, phase) in self.log.borrow_mut().drain(..) {
            info!(command = %self.commands[i], ?phase, frame, "cockpit watch: command");
        }
        let baseline = frame <= BASELINE_FRAMES;
        for key in self.keys.iter_mut().filter(|k| !k.noisy) {
            let value = key.handle.get();
            if differs(value, key.last) {
                if baseline {
                    key.noisy = true;
                    continue;
                }
                if key.settling.is_none() {
                    key.settling = Some((key.last, frame));
                }
                if let Some((_, changed)) = key.settling.as_mut() {
                    *changed = frame;
                }
                key.last = value;
            } else if let Some((before, changed)) = key.settling
                && frame - changed >= SETTLE_FRAMES
            {
                key.settling = None;
                if frame - key.window_start > NOISY_WINDOW {
                    key.window_start = frame;
                    key.reports = 0;
                }
                key.reports += 1;
                if key.reports > NOISY_REPORTS {
                    key.noisy = true;
                    info!(dataref = %key.label, "cockpit watch: muted, changes too often");
                    continue;
                }
                info!(
                    dataref = %key.label,
                    from = %show(before),
                    to = %show(value),
                    writable = key.handle.is_writable(),
                    frame,
                    "cockpit watch: changed"
                );
            }
        }
        if baseline && frame == BASELINE_FRAMES {
            self.baseline_done = true;
            let noisy = self.keys.iter().filter(|k| k.noisy).count();
            info!(
                noisy,
                watched = self.keys.len() - noisy,
                "cockpit watch: baseline done, operate the cockpit now"
            );
        }
    }

    pub fn stop(self) {
        info!(frames = self.frame, "cockpit watch: stopped");
    }
}

fn watch_key(label: String, handle: DynRef) -> WatchKey {
    WatchKey {
        label,
        last: handle.get(),
        handle,
        noisy: false,
        settling: None,
        reports: 0,
        window_start: 0,
    }
}

fn differs(a: Value, b: Value) -> bool {
    match (a, b) {
        (Value::Int(a), Value::Int(b)) => a != b,
        _ => (a.as_f64() - b.as_f64()).abs() > 1e-5,
    }
}

fn show(value: Value) -> String {
    match value {
        Value::Int(v) => v.to_string(),
        Value::Float(v) => format!("{v:.4}"),
        Value::Double(v) => format!("{v:.4}"),
    }
}

/// Every command named by a manipulator in the loaded aircraft's cockpit
/// object (`<acf name>_cockpit.obj` next to the `.acf`).
fn cockpit_commands() -> Vec<String> {
    let (_, acf) = flyx_xplm::aircraft_model(0);
    let obj = acf.with_file_name(format!(
        "{}_cockpit.obj",
        acf.file_stem().unwrap_or_default().to_string_lossy()
    ));
    let commands = match std::fs::read(&obj) {
        Ok(bytes) => parse_manipulator_commands(&String::from_utf8_lossy(&bytes)),
        Err(e) => {
            warn!(path = %obj.display(), %e, "cockpit watch: cannot read the cockpit object");
            BTreeSet::new()
        }
    };
    info!(path = %path_text(&obj), count = commands.len(), "cockpit watch: cockpit commands");
    commands.into_iter().collect()
}

fn path_text(path: &Path) -> String {
    path.display().to_string()
}

/// Command names in `ATTR_manip_command*` lines.
fn parse_manipulator_commands(obj: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for line in obj.lines() {
        let mut tokens = line.split_whitespace();
        let Some(attr) = tokens.next() else { continue };
        if !attr.starts_with("ATTR_manip_command") {
            continue;
        }
        for token in tokens {
            if token.contains('/') && token.chars().all(|c| c.is_ascii_graphic()) {
                out.insert(token.to_owned());
            }
        }
    }
    out
}

// ----------------------------------------------------------- write test

/// Candidate datarefs for `shared`, `derived` and `state`.
const WRITE_CANDIDATES: &[(&str, Option<usize>)] = &[
    ("sim/cockpit2/electrical/battery_on", Some(0)),
    ("sim/cockpit2/electrical/battery_on", Some(1)),
    ("sim/cockpit2/electrical/generator_on", Some(0)),
    ("sim/cockpit2/switches/avionics_power_on", None),
    ("sim/cockpit2/electrical/cross_tie", None),
    ("sim/cockpit2/switches/beacon_on", None),
    ("sim/cockpit2/switches/navigation_lights_on", None),
    ("sim/cockpit2/switches/strobe_lights_on", None),
    ("sim/cockpit2/switches/taxi_light_on", None),
    ("sim/cockpit2/switches/landing_lights_on", None),
    ("sim/cockpit2/ice/ice_pitot_heat_on_pilot", None),
    ("sim/cockpit2/engine/actuators/fuel_pump_on", Some(0)),
    ("sim/cockpit2/controls/flap_ratio", None),
    ("sim/cockpit2/controls/flap_handle_request_ratio", None),
    ("sim/cockpit2/controls/elevator_trim", None),
    ("sim/cockpit2/controls/parking_brake_ratio", None),
    ("sim/cockpit2/engine/actuators/mixture_ratio", Some(0)),
    ("sim/cockpit2/engine/actuators/carb_heat_ratio", Some(0)),
    ("sim/cockpit2/switches/panel_brightness_ratio", Some(0)),
    ("sim/cockpit2/switches/instrument_brightness_ratio", Some(0)),
    ("sim/cockpit2/switches/alternate_static_air_ratio", None),
    ("sim/cockpit2/switches/door_open", Some(0)),
    ("sim/cockpit2/autopilot/heading_dial_deg_mag_pilot", None),
    ("sim/cockpit2/autopilot/vvi_dial_fpm", None),
    ("sim/cockpit2/autopilot/altitude_dial_ft", None),
    ("sim/cockpit/autopilot/autopilot_state", None),
    ("sim/cockpit2/radios/actuators/nav1_obs_deg_mag_pilot", None),
    (
        "sim/cockpit2/radios/actuators/nav2_obs_deg_mag_copilot",
        None,
    ),
    (
        "sim/cockpit2/radios/actuators/adf1_card_heading_deg_mag_pilot",
        None,
    ),
    (
        "sim/cockpit2/gauges/actuators/barometer_setting_in_hg_pilot",
        None,
    ),
    (
        "sim/cockpit2/gauges/actuators/barometer_setting_in_hg_copilot",
        None,
    ),
    (
        "sim/cockpit2/gauges/actuators/artificial_horizon_adjust_deg_pilot",
        None,
    ),
    ("sim/cockpit/gyros/dg_drift_vac_deg", None),
    ("sim/cockpit2/radios/actuators/transponder_code", None),
    ("sim/cockpit2/radios/actuators/transponder_mode", None),
    ("sim/cockpit2/radios/actuators/adf1_power", None),
    ("sim/cockpit2/radios/actuators/adf1_frequency_hz", None),
    (
        "sim/cockpit2/radios/actuators/adf1_standby_frequency_hz",
        None,
    ),
    (
        "sim/cockpit2/radios/actuators/HSI_source_select_pilot",
        None,
    ),
    ("sim/cockpit2/radios/actuators/com1_frequency_hz_833", None),
    (
        "sim/cockpit2/radios/actuators/com1_standby_frequency_hz_833",
        None,
    ),
    ("sim/cockpit2/radios/actuators/com2_frequency_hz_833", None),
    (
        "sim/cockpit2/radios/actuators/com2_standby_frequency_hz_833",
        None,
    ),
    ("sim/cockpit2/radios/actuators/nav1_frequency_hz", None),
    (
        "sim/cockpit2/radios/actuators/nav1_standby_frequency_hz",
        None,
    ),
    ("sim/cockpit2/radios/actuators/nav2_frequency_hz", None),
    (
        "sim/cockpit2/radios/actuators/nav2_standby_frequency_hz",
        None,
    ),
    ("sim/cockpit2/radios/actuators/audio_volume_com1", None),
    ("sim/cockpit2/engine/actuators/ignition_key", Some(0)),
    ("sim/cockpit2/fuel/fuel_tank_selector", None),
    ("sim/cockpit2/clock_timer/timer_mode", None),
    ("sim/cockpit2/controls/water_rudder_handle_ratio", None),
    ("sim/cockpit2/controls/gear_handle_down", None),
    ("sim/flightmodel/weight/m_fuel", Some(0)),
    ("sim/flightmodel/weight/m_fuel", Some(1)),
    ("sim/flightmodel/weight/m_fixed", None),
    ("sim/flightmodel/weight/m_stations", Some(0)),
    ("sim/cockpit/electrical/battery_charge_watt_hr", Some(0)),
    ("sim/flightmodel/engine/ENGN_oil_temp_c", Some(0)),
    ("sim/flightmodel/engine/ENGN_CHT_c", Some(0)),
    ("laminar/c172/knob_EGT", None),
    ("laminar/c172/knob_TAS", None),
    ("laminar/c172/knob_OAT", None),
    ("laminar/c172/fuel/fuel_tank_selector", None),
    ("laminar/c172/fuel/fuel_cutoff_selector", None),
];
const WRITE_BATCH: usize = 8;
const WRITE_CHECK_LATE: u32 = 30;

struct WriteProbe {
    label: String,
    handle: DynRef,
    original: Value,
    written: Value,
    after_one: Option<Value>,
}

pub struct WriteTest {
    next: usize,
    frame: u32,
    batch: Vec<WriteProbe>,
}

impl WriteTest {
    pub fn start() -> WriteTest {
        info!(
            candidates = WRITE_CANDIDATES.len(),
            "cockpit write test: started"
        );
        WriteTest {
            next: 0,
            frame: 0,
            batch: Vec::new(),
        }
    }

    /// Returns `false` when finished.
    pub fn frame(&mut self) -> bool {
        if self.batch.is_empty() {
            if self.next >= WRITE_CANDIDATES.len() {
                info!("cockpit write test: finished");
                return false;
            }
            let end = (self.next + WRITE_BATCH).min(WRITE_CANDIDATES.len());
            for &(name, index) in &WRITE_CANDIDATES[self.next..end] {
                let label = match index {
                    Some(i) => format!("{name}[{i}]"),
                    None => name.to_owned(),
                };
                match DynRef::find(name, index) {
                    Ok(handle) => {
                        let original = handle.get();
                        let written = perturb(original);
                        handle.set(written);
                        self.batch.push(WriteProbe {
                            label,
                            handle,
                            original,
                            written,
                            after_one: None,
                        });
                    }
                    Err(e) => info!(dataref = %label, %e, "cockpit write test: not usable"),
                }
            }
            self.next = end;
            self.frame = 0;
            return true;
        }
        self.frame += 1;
        if self.frame == 1 {
            for probe in &mut self.batch {
                probe.after_one = Some(probe.handle.get());
            }
        } else if self.frame == WRITE_CHECK_LATE {
            for probe in self.batch.drain(..) {
                let late = probe.handle.get();
                let one = probe.after_one.unwrap_or(late);
                info!(
                    dataref = %probe.label,
                    kind = ?probe.handle.kind(),
                    writable = probe.handle.is_writable(),
                    original = %show(probe.original),
                    written = %show(probe.written),
                    after_1_frame = %show(one),
                    after_30_frames = %show(late),
                    verdict = verdict(probe.written, one, late),
                    "cockpit write test"
                );
                probe.handle.set(probe.original);
            }
        }
        true
    }

    /// Restores whatever is still being tested.
    pub fn stop(&mut self) {
        for probe in self.batch.drain(..) {
            probe.handle.set(probe.original);
        }
    }
}

fn verdict(written: Value, after_one: Value, late: Value) -> &'static str {
    match (differs(written, after_one), differs(written, late)) {
        (false, false) => "kept",
        (false, true) => "kept, then changed",
        (true, _) => "overwritten",
    }
}

fn perturb(value: Value) -> Value {
    match value {
        Value::Int(0) => Value::Int(1),
        Value::Int(1) => Value::Int(0),
        Value::Int(v) => Value::Int(v + 1),
        other => {
            let v = other.as_f64();
            let changed = if v.abs() < 1.0 {
                if v < 0.5 { v + 0.25 } else { v - 0.25 }
            } else {
                v + 10.0
            };
            match other {
                Value::Double(_) => Value::Double(changed),
                _ => Value::Float(changed as f32),
            }
        }
    }
}

// ---------------------------------------------------------- replay test

/// Checks whether handlers run inside `XPLMCommandOnce`/`Begin`/`End`.
pub fn replay_test() {
    for name in [
        "sim/instruments/timer_reset",
        "laminar/c172/fuel_selector_up",
        "laminar/c172/fuel_selector_dwn",
    ] {
        let Some(command) = Command::find(name) else {
            info!(command = name, "cockpit replay test: command not found");
            continue;
        };
        let calls: Rc<Cell<u32>> = Rc::default();
        let seen = calls.clone();
        let handler = CommandHandler::register(command, true, move |phase| {
            if phase != CommandPhase::Continue {
                seen.set(seen.get() + 1);
            }
            true
        });
        let selector = DynRef::find("laminar/c172/fuel/fuel_tank_selector", None).ok();
        let before = selector.map(|s| s.get());
        command.once();
        let after_once = calls.get();
        command.begin();
        let after_begin = calls.get();
        command.end();
        let after_end = calls.get();
        let after = selector.map(|s| s.get());
        drop(handler);
        info!(
            command = name,
            handler_calls_during_once = after_once,
            handler_calls_during_begin = after_begin - after_once,
            handler_calls_during_end = after_end - after_begin,
            fuel_selector_before = ?before.map(show),
            fuel_selector_after = ?after.map(show),
            "cockpit replay test (2 calls each for once, 1 for begin and end mean synchronous)"
        );
    }
}

// -------------------------------------------------------- follower test

const INPUT_OVERRIDES: &[&str] = &[
    "sim/operation/override/override_joystick",
    "sim/operation/override/override_throttles",
    "sim/operation/override/override_toe_brakes",
    "sim/operation/override/override_autopilot",
];
const INPUTS: &[(&str, Option<usize>)] = &[
    ("sim/cockpit2/controls/yoke_pitch_ratio", None),
    ("sim/cockpit2/controls/yoke_roll_ratio", None),
    ("sim/cockpit2/controls/yoke_heading_ratio", None),
    ("sim/cockpit2/controls/left_brake_ratio", None),
    ("sim/cockpit2/controls/right_brake_ratio", None),
    ("sim/cockpit2/engine/actuators/throttle_ratio", Some(0)),
];
const PHASE_A_FRAMES: u32 = 480;
const PHASE_B_FRAMES: u32 = 360;
const PHASE_C_FRAMES: u32 = 720;
const START_HOLD_FRAMES: u32 = 180;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FollowerPhase {
    /// Input overrides on, inputs written every frame.
    Overridden,
    /// Input overrides off, inputs still written every frame.
    NotOverridden,
    /// Key to BOTH, then START held for 3 s, under the flight-model override.
    Starter,
}

struct InputProbe {
    label: &'static str,
    handle: DynRef,
    last_written: Option<f32>,
    kept: u32,
    overwritten: u32,
}

pub struct FollowerTest {
    phase: FollowerPhase,
    frame: u32,
    planepath: ArrayRef<i32>,
    position: [DataRef<f64>; 3],
    held_at: [f64; 3],
    velocity: [DataRef<f32>; 3],
    overrides: Vec<(&'static str, DataRef<i32>)>,
    inputs: Vec<InputProbe>,
    key: Option<DynRef>,
    running: Option<DynRef>,
    rpm: Option<DynRef>,
    starter: Option<DynRef>,
    ignition_up: Option<Command>,
    start_held: bool,
}

impl FollowerTest {
    pub fn start() -> Option<FollowerTest> {
        let planepath = ArrayRef::find("sim/operation/override/override_planepath")?;
        let position = [
            DataRef::find("sim/flightmodel/position/local_x")?,
            DataRef::find("sim/flightmodel/position/local_y")?,
            DataRef::find("sim/flightmodel/position/local_z")?,
        ];
        let velocity = [
            DataRef::find("sim/flightmodel/position/local_vx")?,
            DataRef::find("sim/flightmodel/position/local_vy")?,
            DataRef::find("sim/flightmodel/position/local_vz")?,
        ];
        let held_at = position.map(|p| p.get());
        let overrides = INPUT_OVERRIDES
            .iter()
            .filter_map(|&name| DataRef::find(name).map(|r| (name, r)))
            .collect();
        let inputs = INPUTS
            .iter()
            .filter_map(|&(name, index)| {
                DynRef::find(name, index).ok().map(|handle| InputProbe {
                    label: name,
                    handle,
                    last_written: None,
                    kept: 0,
                    overwritten: 0,
                })
            })
            .collect();
        let find = |name, index| DynRef::find(name, index).ok();
        planepath.set_one(0, 1);
        let mut test = FollowerTest {
            phase: FollowerPhase::Overridden,
            frame: 0,
            planepath,
            position,
            held_at,
            velocity,
            overrides,
            inputs,
            key: find("sim/cockpit2/engine/actuators/ignition_key", Some(0)),
            running: find("sim/flightmodel/engine/ENGN_running", Some(0)),
            rpm: find("sim/cockpit2/engine/indicators/engine_speed_rpm", Some(0)),
            starter: find("sim/flightmodel2/engines/starter_is_running", Some(0)),
            ignition_up: Command::find("laminar/c172/ignition_up"),
            start_held: false,
        };
        test.set_overrides(true);
        info!(
            "cockpit follower test: phase 1 of 3 (8 s), input overrides ON; move your \
             joystick, pedals and throttle now"
        );
        Some(test)
    }

    fn set_overrides(&mut self, on: bool) {
        for (name, handle) in &self.overrides {
            handle.set(on as i32);
            info!(
                override_ = name,
                on,
                kept = handle.get() == on as i32,
                "cockpit follower test: override"
            );
        }
    }

    /// Returns `false` when finished.
    pub fn frame(&mut self) -> bool {
        self.frame += 1;
        self.hold();
        match self.phase {
            FollowerPhase::Overridden | FollowerPhase::NotOverridden => {
                self.check_and_write_inputs();
                let limit = if self.phase == FollowerPhase::Overridden {
                    PHASE_A_FRAMES
                } else {
                    PHASE_B_FRAMES
                };
                if self.frame >= limit {
                    self.report_inputs();
                    self.frame = 0;
                    if self.phase == FollowerPhase::Overridden {
                        self.set_overrides(false);
                        self.phase = FollowerPhase::NotOverridden;
                        info!(
                            "cockpit follower test: phase 2 of 3 (6 s), input overrides OFF; \
                             keep moving your controls"
                        );
                    } else {
                        self.phase = FollowerPhase::Starter;
                        info!(
                            "cockpit follower test: phase 3 of 3 (12 s), starter replay under \
                             the flight-model override"
                        );
                    }
                }
                true
            }
            FollowerPhase::Starter => self.starter_frame(),
        }
    }

    fn hold(&self) {
        for (p, v) in self.position.iter().zip(self.held_at) {
            p.set(v);
        }
        for v in &self.velocity {
            v.set(0.0);
        }
    }

    fn check_and_write_inputs(&mut self) {
        let t = self.frame as f32 / 60.0;
        for (i, probe) in self.inputs.iter_mut().enumerate() {
            let now = probe.handle.get().as_f64() as f32;
            if let Some(w) = probe.last_written {
                if (now - w).abs() < 1e-4 {
                    probe.kept += 1;
                } else {
                    probe.overwritten += 1;
                }
            }
            let value = match i {
                0 => 0.5 * (t * 1.5).sin(),
                1 => 0.5 * (t * 1.5).cos(),
                2 => 0.3 * (t * 1.1).sin(),
                3 | 4 => 0.5,
                _ => 0.5 + 0.4 * (t * 0.8).sin(),
            };
            probe.handle.set(Value::Float(value));
            probe.last_written = Some(value);
        }
    }

    fn report_inputs(&mut self) {
        for probe in &mut self.inputs {
            info!(
                phase = ?self.phase,
                input = probe.label,
                kept = probe.kept,
                overwritten = probe.overwritten,
                "cockpit follower test: input"
            );
            probe.kept = 0;
            probe.overwritten = 0;
            probe.last_written = None;
        }
    }

    fn starter_frame(&mut self) -> bool {
        let key = self.key.map(|k| k.get().as_f64() as i32).unwrap_or(-1);
        // Turn the key up to BOTH (3), one click every 10 frames.
        if self.frame < 60
            && self.frame % 10 == 0
            && (0..3).contains(&key)
            && let Some(c) = self.ignition_up
        {
            c.once();
        }
        if self.frame == 60
            && let Some(c) = self.ignition_up
        {
            info!(key, "cockpit follower test: holding START");
            c.begin();
            self.start_held = true;
        }
        if self.frame == 60 + START_HOLD_FRAMES
            && let Some(c) = self.ignition_up
        {
            c.end();
            self.start_held = false;
            info!("cockpit follower test: released START");
        }
        if self.frame % 30 == 0 {
            let get = |r: Option<DynRef>| r.map(|r| show(r.get())).unwrap_or_default();
            info!(
                frame = self.frame,
                key,
                engine_running = %get(self.running),
                rpm = %get(self.rpm),
                starter = %get(self.starter),
                "cockpit follower test: engine"
            );
        }
        if self.frame >= PHASE_C_FRAMES {
            self.stop();
            info!("cockpit follower test: finished");
            return false;
        }
        true
    }

    /// Releases everything the test set.
    pub fn stop(&mut self) {
        if self.start_held
            && let Some(c) = self.ignition_up
        {
            c.end();
            self.start_held = false;
        }
        self.set_overrides(false);
        self.planepath.set_one(0, 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_manipulator_commands() {
        let obj = "ATTR_manip_command button sim/lights/beacon_lights_on Beacon\n\
                   ATTR_manip_command_knob rotate_large laminar/c172/fuel_selector_up laminar/c172/fuel_selector_dwn FUEL SELECTOR\n\
                   ATTR_manip_drag_axis hand 0 1 0 0 1 sim/cockpit2/controls/flap_ratio Flaps\n\
                   ATTR_manip_command_switch_up_down up_down sim/systems/avionics_on sim/systems/avionics_off Avionics\n";
        let found: Vec<_> = parse_manipulator_commands(obj).into_iter().collect();
        assert_eq!(
            found,
            [
                "laminar/c172/fuel_selector_dwn",
                "laminar/c172/fuel_selector_up",
                "sim/lights/beacon_lights_on",
                "sim/systems/avionics_off",
                "sim/systems/avionics_on",
            ]
        );
    }

    #[test]
    fn perturbs_every_value() {
        assert_eq!(perturb(Value::Int(0)), Value::Int(1));
        assert_eq!(perturb(Value::Int(1)), Value::Int(0));
        assert_eq!(perturb(Value::Int(1200)), Value::Int(1201));
        assert_eq!(perturb(Value::Float(0.0)), Value::Float(0.25));
        assert_eq!(perturb(Value::Float(0.9)), Value::Float(0.65));
        assert_eq!(perturb(Value::Double(29.92)), Value::Double(39.92));
    }
}

//! Cockpit sync inside X-Plane: builds the loaded aircraft's sync
//! definition, resolves it against the running simulator, and connects it
//! to the shared-value register, command forwarding and drift repair from
//! `flyx_sync::cockpit`.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::path::Path;
use std::rc::Rc;
use std::time::Instant;

use flyx_net::{NetCommand, NetHandle};
use flyx_protocol::{CommandPhase, Control, MAX_INPUTS, Seat, Value};
use flyx_sync::cockpit::{Action, Commands, HandlerPhase, Register, Repair};
use flyx_sync::definition::{
    Aircraft, Class, Definition, Manipulators, Profile, Target, builtin, manip,
};
use flyx_xplm::command::{Command, CommandHandler, Phase};
use flyx_xplm::dataref::DataRef;
use flyx_xplm::dynref::{DynRef, Value as XValue};
use tracing::{debug, info, warn};

/// Systems datagrams per second while pilot flying.
const SYSTEMS_INTERVAL_S: f64 = 0.5;
/// Unresolved entries are retried this often...
const RESOLVE_INTERVAL_S: f64 = 1.0;
/// ...for this long after the aircraft loaded.
const RESOLVE_FOR_S: f64 = 30.0;

fn to_x(value: Value) -> XValue {
    match value {
        Value::Int(v) => XValue::Int(v),
        Value::Float(v) => XValue::Float(v),
        Value::Double(v) => XValue::Double(v),
    }
}

fn from_x(value: XValue) -> Value {
    match value {
        XValue::Int(v) => Value::Int(v),
        XValue::Float(v) => Value::Float(v),
        XValue::Double(v) => Value::Double(v),
    }
}

/// Cockpit sync for one loaded aircraft.
pub struct CockpitSync {
    pub definition: Definition,
    datarefs: BTreeMap<u16, DynRef>,
    commands: BTreeMap<u16, Command>,
    handlers: Vec<CommandHandler>,
    /// Command phases seen by the handlers, drained every frame.
    pressed: Rc<RefCell<Vec<(u16, HandlerPhase)>>>,
    /// Set while replaying a command from the other seat.
    replaying: Rc<Cell<bool>>,
    /// Flight-control inputs, in definition order.
    inputs: Vec<u16>,
    overrides: Vec<u16>,
    /// Entries not resolved yet, retried for a while after loading.
    pending: Vec<u16>,
    next_resolve: f64,
    active: Option<Active>,
    monitoring: bool,
    origin: Instant,
    frame: u64,
}

/// Per-session state.
struct Active {
    register: Register,
    commands: Commands,
    repair: Repair,
    next_systems: f64,
}

impl CockpitSync {
    /// Builds and resolves the sync definition of the user's aircraft.
    /// `profiles_dir` holds the optional profile files.
    pub fn load(acf_path: &Path, folder: &str, acf: &str, profiles_dir: &Path) -> Self {
        let manipulators = read_manipulators(acf_path);
        let profiles = read_profiles(profiles_dir);
        let engines = DataRef::<i32>::find("sim/aircraft/engine/acf_num_engines")
            .map_or(1, |r| r.get().clamp(0, 8) as usize);
        let definition = Definition::build(
            &Aircraft {
                folder,
                acf,
                engines,
                manipulators: &manipulators,
            },
            &builtin(),
            &profiles,
        );

        // Slots are fixed by the definition, so both seats agree even if
        // one cannot resolve an input.
        let inputs: Vec<u16> = definition
            .of_class(Class::Input)
            .map(|(k, _)| k as u16)
            .take(MAX_INPUTS)
            .collect();
        let overrides = definition
            .of_class(Class::MonitorOverride)
            .map(|(k, _)| k as u16)
            .collect();
        let pending = (0..definition.entries.len() as u16).collect();
        let mut sync = Self {
            definition,
            datarefs: BTreeMap::new(),
            commands: BTreeMap::new(),
            handlers: Vec::new(),
            pressed: Rc::default(),
            replaying: Rc::default(),
            inputs,
            overrides,
            pending,
            next_resolve: 0.0,
            active: None,
            monitoring: false,
            origin: Instant::now(),
            frame: 0,
        };
        sync.resolve_pending();
        info!(
            aircraft = acf,
            manipulators = manipulators.count,
            commands = sync.definition.count(Class::Command),
            shared = sync.definition.count(Class::Shared),
            state = sync.definition.count(Class::State),
            inputs = sync.inputs.len(),
            resolved = sync.datarefs.len() + sync.commands.len(),
            pending = sync.pending.len(),
            verified = sync.definition.verified,
            profile = ?sync.definition.profile,
            "sync definition built"
        );
        sync
    }

    /// Looks up the entries X-Plane did not know yet. Aircraft plugins
    /// (such as the C172's xlua scripts) create their datarefs and
    /// commands after the aircraft-loaded message, so this is retried.
    fn resolve_pending(&mut self) {
        let pending = std::mem::take(&mut self.pending);
        for key in pending {
            let Some(entry) = self.definition.entries.get(key as usize) else {
                continue;
            };
            match &entry.target {
                Target::Dataref(d) => match DynRef::find(&d.name, d.index) {
                    Ok(r) => {
                        self.datarefs.insert(key, r);
                    }
                    Err(_) => self.pending.push(key),
                },
                Target::Command(name) => match Command::find(name) {
                    Some(command) => {
                        self.commands.insert(key, command);
                        let pressed = self.pressed.clone();
                        let replaying = self.replaying.clone();
                        self.handlers
                            .push(CommandHandler::register(command, true, move |phase| {
                                if !replaying.get() {
                                    let phase = match phase {
                                        Phase::Begin => HandlerPhase::Begin,
                                        Phase::Continue => HandlerPhase::Continue,
                                        Phase::End => HandlerPhase::End,
                                    };
                                    if phase != HandlerPhase::Continue {
                                        pressed.borrow_mut().push((key, phase));
                                    }
                                }
                                true
                            }));
                    }
                    None => self.pending.push(key),
                },
            }
        }
    }

    /// Retries unresolved entries every [`RESOLVE_INTERVAL_S`] for
    /// [`RESOLVE_FOR_S`] after loading, then logs what stayed unknown.
    fn retry_pending(&mut self, now: f64) {
        if self.pending.is_empty() || now < self.next_resolve {
            return;
        }
        self.next_resolve = now + RESOLVE_INTERVAL_S;
        let before = self.pending.len();
        self.resolve_pending();
        let resolved = before - self.pending.len();
        if resolved > 0 {
            info!(
                resolved,
                pending = self.pending.len(),
                "late sync definition entries resolved"
            );
        }
        if now >= RESOLVE_FOR_S && !self.pending.is_empty() {
            let names: Vec<String> = self.pending.iter().map(|&k| self.name(k)).collect();
            warn!(
                count = names.len(),
                "sync definition entries X-Plane does not know: {}",
                names.join(", ")
            );
            self.pending.clear();
        }
    }

    pub fn identity(&self) -> [u8; 32] {
        self.definition.identity
    }

    pub fn verified(&self) -> bool {
        self.definition.verified
    }

    fn now(&self) -> f64 {
        self.origin.elapsed().as_secs_f64()
    }

    fn keys(&self, class: Class) -> impl Iterator<Item = u16> + '_ {
        self.definition
            .of_class(class)
            .map(|(k, _)| k as u16)
            .filter(|k| self.datarefs.contains_key(k))
    }

    fn read(&self, key: u16) -> Option<Value> {
        self.datarefs.get(&key).map(|r| from_x(r.get()))
    }

    fn write(&self, key: u16, value: Value) {
        match self.datarefs.get(&key) {
            Some(r) if r.is_writable() => r.set(to_x(value)),
            _ => debug!(key, "not writable here"),
        }
    }

    fn name(&self, key: u16) -> String {
        self.definition
            .entries
            .get(key as usize)
            .map_or_else(|| format!("#{key}"), |e| e.target.to_string())
    }

    /// A session connected. The host sends the join snapshot.
    pub fn connect(&mut self, seat: Seat, net: &NetHandle) {
        let shared: Vec<(u16, f32)> = self
            .definition
            .of_class(Class::Shared)
            .map(|(k, e)| (k as u16, e.epsilon))
            .filter(|(k, _)| self.datarefs.contains_key(k))
            .collect();
        let mut register = Register::new(seat, shared.iter().copied());
        for &(key, _) in &shared {
            if let Some(value) = self.read(key) {
                register.wrote(key, value);
            }
        }
        if seat == Seat::Host {
            let mut values = register.snapshot();
            values.extend(
                self.keys(Class::State)
                    .filter_map(|k| self.read(k).map(|v| (k, v))),
            );
            info!(values = values.len(), "sending the join snapshot");
            net.send(NetCommand::SendCockpit(Control::Snapshot { values }));
        }
        self.pressed.borrow_mut().clear();
        self.active = Some(Active {
            register,
            commands: Commands::default(),
            repair: Repair::default(),
            next_systems: 0.0,
        });
    }

    /// The session ended: release commands the other seat held and the
    /// pilot-monitoring overrides.
    pub fn disconnect(&mut self) {
        if let Some(mut active) = self.active.take() {
            for key in active.commands.end_all() {
                if let Some(c) = self.commands.get(&key) {
                    info!(command = %self.name(key), "ending a command the other seat held");
                    self.replaying.set(true);
                    c.end();
                    self.replaying.set(false);
                }
            }
        }
        self.set_monitoring(false);
    }

    /// While following, the pilot monitoring's own hardware, autopilot and
    /// engine must not fight the values it is given.
    pub fn set_monitoring(&mut self, on: bool) {
        if self.monitoring == on {
            return;
        }
        self.monitoring = on;
        for &key in &self.overrides {
            self.write(key, Value::Int(on as i32));
        }
        info!(
            on,
            overrides = self.overrides.len(),
            "pilot monitoring overrides"
        );
    }

    /// The pilot flying's flight-control inputs.
    pub fn sample_inputs(&self) -> [f32; MAX_INPUTS] {
        let mut out = [0.0; MAX_INPUTS];
        for (slot, &key) in out.iter_mut().zip(&self.inputs) {
            if let Some(v) = self.read(key) {
                *slot = match v {
                    Value::Int(i) => i as f32,
                    Value::Float(f) => f,
                    Value::Double(d) => d as f32,
                };
            }
        }
        out
    }

    /// Pilot monitoring: shows the pilot flying's inputs.
    pub fn write_inputs(&self, inputs: &[f32; MAX_INPUTS]) {
        for (&key, &value) in self.inputs.iter().zip(inputs) {
            self.write(key, Value::Float(value));
        }
    }

    fn apply(&mut self, actions: Vec<Action>, net: &NetHandle) {
        for action in actions {
            match action {
                Action::Write { key, value } => self.write(key, value),
                Action::Send(message) => net.send(NetCommand::SendCockpit(message)),
                Action::Muted { key } => {
                    warn!(dataref = %self.name(key), "muted: it changes on its own")
                }
            }
        }
    }

    /// Runs once per frame, connected or not.
    pub fn tick(&mut self) {
        let now = self.now();
        self.retry_pending(now);
    }

    /// Runs once per frame while connected. `flying` is the control epoch
    /// while this seat is the pilot flying.
    pub fn frame(&mut self, flying: Option<u32>, net: &NetHandle) {
        self.frame += 1;
        let now = self.now();
        let frame = self.frame;
        let Some(mut active) = self.active.take() else {
            self.pressed.borrow_mut().clear();
            return;
        };
        let pressed: Vec<_> = self.pressed.borrow_mut().drain(..).collect();
        for (key, phase) in pressed {
            if let Some(message) = active.commands.local(key, phase) {
                debug!(command = %self.name(key), ?phase, "forwarding");
                active.repair.activity(now);
                net.send(NetCommand::SendCockpit(message));
            }
        }
        let mut actions = Vec::new();
        for (key, r) in self
            .keys(Class::Shared)
            .filter_map(|k| self.datarefs.get(&k).map(|r| (k, r)))
        {
            actions.extend(active.register.observe(key, from_x(r.get()), frame, now));
        }
        actions.extend(active.register.flush(now));
        if actions.iter().any(|a| matches!(a, Action::Send(_))) {
            active.repair.activity(now);
        }
        if let Some(epoch) = flying
            && now >= active.next_systems
        {
            active.next_systems = now + SYSTEMS_INTERVAL_S;
            let state = self
                .keys(Class::State)
                .filter_map(|k| self.read(k).map(|v| (k, v)))
                .collect();
            let repair = active.repair.next_chunk(&active.register.snapshot());
            net.send(NetCommand::SendSystems {
                epoch,
                state,
                repair,
            });
        }
        self.active = Some(active);
        self.apply(actions, net);
    }

    /// A cockpit message from the other seat.
    pub fn receive(&mut self, message: Control, net: &NetHandle) {
        let now = self.now();
        let frame = self.frame;
        let Some(active) = self.active.as_mut() else {
            return;
        };
        let actions = match message {
            Control::Change { key, value, req } => {
                active.repair.activity(now);
                active.register.receive_change(key, value, req)
            }
            Control::Set { key, value, ack } => {
                active.repair.activity(now);
                active.register.receive_set(key, value, ack)
            }
            Control::Snapshot { values } => {
                info!(values = values.len(), "join snapshot received");
                let (shared, state): (Vec<_>, Vec<_>) = values.into_iter().partition(|(k, _)| {
                    self.definition
                        .entries
                        .get(*k as usize)
                        .is_some_and(|e| e.class == Class::Shared)
                });
                for (key, value) in state {
                    self.write(key, value);
                }
                let actions = match self.active.as_mut() {
                    Some(a) => a.register.apply_snapshot(&shared),
                    None => vec![],
                };
                self.apply(actions, net);
                return;
            }
            Control::Command { key, phase } => {
                active.repair.activity(now);
                if active.commands.remote(key, phase)
                    && let Some(command) = self.commands.get(&key)
                {
                    active.register.note_replay(frame);
                    self.replaying.set(true);
                    match phase {
                        CommandPhase::Begin => command.begin(),
                        CommandPhase::End => command.end(),
                    }
                    self.replaying.set(false);
                }
                return;
            }
            other => {
                debug!(?other, "unexpected cockpit message");
                return;
            }
        };
        self.apply(actions, net);
    }

    /// Systems state and a drift-repair slice from the pilot flying.
    pub fn systems(&mut self, state: &[(u16, Value)], repair: &[(u16, Value)], net: &NetHandle) {
        let now = self.now();
        for &(key, value) in state {
            let is_state = self
                .definition
                .entries
                .get(key as usize)
                .is_some_and(|e| e.class == Class::State);
            if is_state {
                self.write(key, value);
            }
        }
        let Some(active) = self.active.as_mut() else {
            return;
        };
        let datarefs = &self.datarefs;
        let entries = &self.definition.entries;
        let selection = active.repair.select(now, repair, |key| {
            let entry = entries.get(key as usize)?;
            if entry.class != Class::Shared {
                return None;
            }
            let r = datarefs.get(&key).filter(|r| r.is_writable())?;
            Some((from_x(r.get()), entry.epsilon))
        });
        for key in &selection.unwritable {
            debug!(key, "cannot repair here");
        }
        let mut actions = Vec::new();
        for (key, value) in selection.repair {
            info!(dataref = %entries[key as usize].target, ?value, "drift repair");
            actions.extend(active.register.repair(key, value, now));
        }
        self.apply(actions, net);
    }
}

fn read_manipulators(acf_path: &Path) -> Manipulators {
    let mut all = Manipulators::default();
    let Ok(bytes) = std::fs::read(acf_path) else {
        warn!(path = %acf_path.display(), "cannot read the aircraft file");
        return all;
    };
    let acf = String::from_utf8_lossy(&bytes);
    for path in manip::object_paths(acf_path, &acf) {
        match std::fs::read(&path) {
            Ok(bytes) => all.extend(manip::parse_object(&String::from_utf8_lossy(&bytes))),
            Err(e) => debug!(path = %path.display(), %e, "object not read"),
        }
    }
    if all.count == 0 {
        warn!(path = %acf_path.display(), "no cockpit manipulators found");
    }
    all
}

fn read_profiles(dir: &Path) -> Vec<Profile> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        info!(path = %dir.display(), "no profiles folder");
        return vec![];
    };
    let mut profiles = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("toml"))
        {
            continue;
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        match std::fs::read_to_string(&path) {
            Ok(text) => match Profile::parse(&name, &text) {
                Ok(p) => profiles.push(p),
                Err(e) => warn!(%e, "profile skipped"),
            },
            Err(e) => warn!(%name, %e, "cannot read profile"),
        }
    }
    info!(count = profiles.len(), "profiles loaded");
    profiles
}

/// The pilot-monitoring overrides of the built-in list, cleared by the
/// emergency release after an internal error.
pub fn release_overrides() {
    for entry in builtin().monitor_override {
        if let Ok(r) = DynRef::find(&entry.dataref, None) {
            r.set(XValue::Int(0));
        }
    }
}

//! Flight-model override spike (dev only): what keeps running on the C172 while the
//! flight-model path is overridden, and which visual datarefs a follower
//! can drive.
//!
//! The aircraft is frozen in place. In three phases of a few seconds it
//! records which datarefs change on their own, then writes test values each
//! frame and checks at the start of the next frame whether X-Plane or the
//! aircraft's scripts overwrote them. A report goes to FlyXTogether.log.

use std::fmt::Write as _;

use flyx_xplm::dataref::{ArrayRef, DataRef, Element};
use tracing::{info, warn};

/// Frames per phase (about 6 s at 60 fps).
const PHASE_FRAMES: u32 = 360;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Only the flight-model path is overridden; nothing else is written.
    Observe,
    /// Also write the visual candidates every frame.
    Write,
    /// Also override control surfaces while writing.
    WriteWithSurfaceOverride,
}

impl Phase {
    fn next(self) -> Option<Phase> {
        match self {
            Phase::Observe => Some(Phase::Write),
            Phase::Write => Some(Phase::WriteWithSurfaceOverride),
            Phase::WriteWithSurfaceOverride => None,
        }
    }
}

/// A scalar float dataref to write and verify.
struct Written {
    name: &'static str,
    handle: DataRef<f32>,
    value: fn(f32) -> f32,
    last_written: Option<f32>,
    kept: u32,
    overwritten: u32,
    example: Option<(f32, f32)>,
}

/// An element of an array dataref to write and verify.
struct WrittenElement<T: Element> {
    name: &'static str,
    handle: ArrayRef<T>,
    index: usize,
    value: fn(f32) -> T,
    last_written: Option<T>,
    kept: u32,
    overwritten: u32,
    example: Option<(T, T)>,
}

/// A float array observed for changes.
struct Observed {
    name: &'static str,
    handle: ArrayRef<f32>,
    count: usize,
    min: Vec<f32>,
    max: Vec<f32>,
}

struct Hold {
    x: DataRef<f64>,
    y: DataRef<f64>,
    z: DataRef<f64>,
    position: (f64, f64, f64),
    velocity: [DataRef<f32>; 3],
}

pub struct Spike {
    phase: Phase,
    frame: u32,
    time: f32,
    planepath: ArrayRef<i32>,
    surfaces_override: DataRef<i32>,
    hold: Hold,
    floats: Vec<Written>,
    float_elements: Vec<WrittenElement<f32>>,
    int_elements: Vec<WrittenElement<i32>>,
    observed: Vec<Observed>,
    report: String,
}

fn find<T: flyx_xplm::dataref::Scalar>(name: &str) -> Option<DataRef<T>> {
    let found = DataRef::find(name);
    if found.is_none() {
        warn!(name, "spike: dataref not found");
    }
    found
}

fn find_array<T: Element>(name: &str) -> Option<ArrayRef<T>> {
    let found = ArrayRef::find(name);
    if found.is_none() {
        warn!(name, "spike: dataref not found");
    }
    found
}

impl Spike {
    pub fn start() -> Option<Spike> {
        let x = find::<f64>("sim/flightmodel/position/local_x")?;
        let y = find::<f64>("sim/flightmodel/position/local_y")?;
        let z = find::<f64>("sim/flightmodel/position/local_z")?;
        let hold = Hold {
            position: (x.get(), y.get(), z.get()),
            x,
            y,
            z,
            velocity: [
                find("sim/flightmodel/position/local_vx")?,
                find("sim/flightmodel/position/local_vy")?,
                find("sim/flightmodel/position/local_vz")?,
            ],
        };
        let float = |name: &'static str, value: fn(f32) -> f32| {
            find::<f32>(name).map(|handle| Written {
                name,
                handle,
                value,
                last_written: None,
                kept: 0,
                overwritten: 0,
                example: None,
            })
        };
        let floats: Vec<Written> = [
            float("sim/flightmodel/controls/lail1def", |t| 15.0 * t.sin()),
            float("sim/flightmodel/controls/rail1def", |t| -15.0 * t.sin()),
            float("sim/flightmodel/controls/hstab1_elv1def", |t| {
                10.0 * t.cos()
            }),
            float("sim/flightmodel/controls/vstab1_rud1def", |t| 8.0 * t.sin()),
            float("sim/flightmodel/controls/flaprat", |_| 0.5),
        ]
        .into_iter()
        .flatten()
        .collect();
        let float_element = |name: &'static str, index: usize, value: fn(f32) -> f32| {
            find_array::<f32>(name).map(|handle| WrittenElement {
                name,
                handle,
                index,
                value,
                last_written: None,
                kept: 0,
                overwritten: 0,
                example: None,
            })
        };
        let float_elements: Vec<WrittenElement<f32>> = [
            float_element("sim/flightmodel/engine/POINT_tacrad", 0, |_| 250.0),
            float_element("sim/flightmodel2/engines/prop_rotation_angle_deg", 0, |t| {
                (t * 1500.0) % 360.0
            }),
            float_element("sim/flightmodel2/wing/aileron1_deg", 0, |t| 12.0 * t.sin()),
            float_element("sim/flightmodel2/wing/elevator1_deg", 0, |t| 8.0 * t.cos()),
            float_element("sim/flightmodel2/gear/tire_steer_actual_deg", 0, |t| {
                10.0 * t.sin()
            }),
        ]
        .into_iter()
        .flatten()
        .collect();
        let int_elements: Vec<WrittenElement<i32>> =
            find_array::<i32>("sim/flightmodel/engine/ENGN_running")
                .map(|handle| WrittenElement {
                    name: "sim/flightmodel/engine/ENGN_running",
                    handle,
                    index: 0,
                    value: |_| 1,
                    last_written: None,
                    kept: 0,
                    overwritten: 0,
                    example: None,
                })
                .into_iter()
                .collect();
        let observe = |name: &'static str, count: usize| {
            find_array::<f32>(name).map(|handle| Observed {
                name,
                handle,
                count,
                min: vec![f32::MAX; count],
                max: vec![f32::MIN; count],
            })
        };
        let observed: Vec<Observed> = [
            observe("sim/flightmodel2/wing/aileron1_deg", 8),
            observe("sim/flightmodel2/wing/elevator1_deg", 8),
            observe("sim/flightmodel2/wing/rudder1_deg", 8),
            observe("sim/flightmodel2/wing/flap1_deg", 8),
            observe("sim/flightmodel2/engines/prop_rotation_speed_rad_sec", 1),
            observe("sim/flightmodel2/engines/prop_rotation_angle_deg", 1),
            observe("sim/flightmodel2/gear/tire_rotation_angle_deg", 3),
            observe("sim/flightmodel2/gear/tire_vertical_deflection_mtr", 3),
            observe("sim/cockpit2/controls/yoke_roll_ratio", 1),
            observe("sim/cockpit2/controls/yoke_pitch_ratio", 1),
        ]
        .into_iter()
        .flatten()
        .collect();

        let planepath = find_array::<i32>("sim/operation/override/override_planepath")?;
        let surfaces_override = find::<i32>("sim/operation/override/override_control_surfaces")?;
        planepath.set_one(0, 1);
        info!(
            position = ?hold.position,
            "spike: started, flight-model path overridden, holding position"
        );
        Some(Spike {
            phase: Phase::Observe,
            frame: 0,
            time: 0.0,
            planepath,
            surfaces_override,
            hold,
            floats,
            float_elements,
            int_elements,
            observed,
            report: String::new(),
        })
    }

    /// Runs one frame. Returns `false` when the spike has finished.
    pub fn frame(&mut self, dt: f32) -> bool {
        self.time += dt;
        let writing = self.phase != Phase::Observe;

        // Check last frame's writes before anything else.
        if writing {
            for w in &mut self.floats {
                if let Some(expected) = w.last_written {
                    let actual = w.handle.get();
                    if (actual - expected).abs() < 1e-3 {
                        w.kept += 1;
                    } else {
                        w.overwritten += 1;
                        w.example.get_or_insert((expected, actual));
                    }
                }
            }
            check_elements(&mut self.float_elements, |a, b| (a - b).abs() < 1e-3);
            check_elements(&mut self.int_elements, |a, b| a == b);
        }
        for o in &mut self.observed {
            let mut values = vec![0.0f32; o.count];
            let n = o.handle.get(0, &mut values).min(o.count);
            for (i, value) in values.iter().take(n).enumerate() {
                o.min[i] = o.min[i].min(*value);
                o.max[i] = o.max[i].max(*value);
            }
        }

        // Hold the aircraft still.
        let (x, y, z) = self.hold.position;
        self.hold.x.set(x);
        self.hold.y.set(y);
        self.hold.z.set(z);
        for v in &self.hold.velocity {
            v.set(0.0);
        }

        if writing {
            let t = self.time;
            for w in &mut self.floats {
                let value = (w.value)(t);
                w.handle.set(value);
                w.last_written = Some(value);
            }
            write_elements(&mut self.float_elements, t);
            write_elements(&mut self.int_elements, t);
        }

        self.frame += 1;
        if self.frame < PHASE_FRAMES {
            return true;
        }
        self.finish_phase();
        match self.phase.next() {
            Some(next) => {
                if next == Phase::WriteWithSurfaceOverride {
                    self.surfaces_override.set(1);
                }
                info!(phase = ?next, "spike: next phase");
                self.phase = next;
                self.frame = 0;
                true
            }
            None => {
                self.stop();
                false
            }
        }
    }

    fn finish_phase(&mut self) {
        let mut r = String::new();
        let _ = writeln!(r, "spike phase {:?} ({} frames):", self.phase, self.frame);
        if self.phase != Phase::Observe {
            for w in &mut self.floats {
                let _ = writeln!(
                    r,
                    "  write {:<50} kept {:>4} overwritten {:>4} {}",
                    w.name,
                    w.kept,
                    w.overwritten,
                    w.example
                        .map(|(e, a)| format!("(wrote {e:.2}, read {a:.2})"))
                        .unwrap_or_default()
                );
                w.kept = 0;
                w.overwritten = 0;
                w.example = None;
                w.last_written = None;
            }
            report_elements(&mut r, &mut self.float_elements);
            report_elements(&mut r, &mut self.int_elements);
        }
        for o in &mut self.observed {
            let ranges: Vec<String> = o
                .min
                .iter()
                .zip(&o.max)
                .map(|(lo, hi)| {
                    if lo > hi {
                        "-".to_owned()
                    } else if (hi - lo).abs() < 1e-3 {
                        format!("{lo:.2}")
                    } else {
                        format!("{lo:.2}..{hi:.2}")
                    }
                })
                .collect();
            let _ = writeln!(r, "  observe {:<50} [{}]", o.name, ranges.join(", "));
            o.min.fill(f32::MAX);
            o.max.fill(f32::MIN);
        }
        info!("{r}");
        self.report.push_str(&r);
    }

    /// Releases every override the spike set.
    pub fn stop(&mut self) {
        self.surfaces_override.set(0);
        self.planepath.set_one(0, 0);
        info!("spike: finished, overrides released");
    }
}

fn check_elements<T: Element + std::fmt::Debug>(
    elements: &mut [WrittenElement<T>],
    same: impl Fn(T, T) -> bool,
) {
    for w in elements {
        if let Some(expected) = w.last_written {
            let actual = w.handle.get_one(w.index);
            if same(actual, expected) {
                w.kept += 1;
            } else {
                w.overwritten += 1;
                w.example.get_or_insert((expected, actual));
            }
        }
    }
}

fn write_elements<T: Element>(elements: &mut [WrittenElement<T>], t: f32) {
    for w in elements {
        let value = (w.value)(t);
        w.handle.set_one(w.index, value);
        w.last_written = Some(value);
    }
}

fn report_elements<T: Element + std::fmt::Debug>(
    r: &mut String,
    elements: &mut [WrittenElement<T>],
) {
    for w in elements {
        let _ = writeln!(
            r,
            "  write {:<46}[{}] kept {:>4} overwritten {:>4} {}",
            w.name,
            w.index,
            w.kept,
            w.overwritten,
            w.example
                .as_ref()
                .map(|(e, a)| format!("(wrote {e:?}, read {a:?})"))
                .unwrap_or_default()
        );
        w.kept = 0;
        w.overwritten = 0;
        w.example = None;
        w.last_written = None;
    }
}

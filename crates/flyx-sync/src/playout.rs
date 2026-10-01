//! The follower's playout buffer: turns flight-state samples
//! that arrive with network jitter and loss into a smooth pose per frame.
//!
//! Samples are placed on the local clock using the smallest observed
//! offset between arrival time and authority simulator time, and rendered
//! a small adaptive delay behind the newest one. Between samples the
//! position follows a Hermite curve (samples' velocities as tangents) and
//! the attitude a quaternion slerp. When samples stop, the pose is
//! extrapolated for up to 500 ms and then held; when they resume, the
//! pose blends back over 250 ms instead of snapping.

use std::collections::VecDeque;

use flyx_protocol::{FlightState, Visuals, WING_PARTS};

/// Shortest and longest render delay behind the newest sample, seconds.
pub const MIN_DELAY: f64 = 0.050;
pub const MAX_DELAY: f64 = 0.150;
/// How long the pose is extrapolated after the newest sample.
pub const MAX_EXTRAPOLATION: f64 = 0.5;
/// How long a recovery blend takes.
pub const BLEND_TIME: f64 = 0.25;
/// After a handover, how long the blend from this seat's own last pose
/// takes.
pub const HANDOVER_BLEND_TIME: f64 = 1.0;
/// After a handover, the shortest time over which the lead decays.
pub const HANDOVER_DECAY_MIN: f64 = 4.0;
/// The lead decays slowly enough that the shown speed differs from the
/// true speed by at most this fraction.
pub const HANDOVER_MAX_SPEED_ERROR: f64 = 0.04;
/// Largest lead measured after a handover, seconds.
const HANDOVER_MAX_LEAD: f64 = 1.5;
/// Below this ground speed no lead is measured (along-track lag is
/// meaningless when parked).
const HANDOVER_MIN_SPEED: f64 = 3.0;
/// Window over which the clock offset minimum and jitter are taken.
const CLOCK_WINDOW: f64 = 2.0;
/// How fast the render delay may change, seconds per second.
const DELAY_SLEW: f64 = 0.05;
/// Samples older than this behind the render time are dropped.
const KEEP_BEHIND: f64 = 1.0;

const EARTH_RADIUS_M: f64 = 6_371_000.0;

/// What the buffer did for the last pose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// No sample yet.
    Empty,
    Interpolating,
    /// Past the newest sample, predicting ahead.
    Extrapolating,
    /// Extrapolated as far as allowed; holding still.
    Holding,
    /// The authority is paused.
    Paused,
}

#[derive(Debug, Clone, Copy)]
struct Arrival {
    received_at: f64,
    offset: f64,
}

/// An error term that fades out over `duration`.
#[derive(Debug, Clone, Copy)]
struct Blend {
    started_at: f64,
    duration: f64,
    /// Position error east/north/up, metres.
    position: [f64; 3],
    /// Attitude error: `output = error * target`.
    attitude: Quat,
}

#[derive(Debug)]
pub struct Playout {
    samples: VecDeque<FlightState>,
    arrivals: VecDeque<Arrival>,
    delay: f64,
    mode: Mode,
    paused: bool,
    last_output: Option<FlightState>,
    /// The sample being extrapolated from, while predicting.
    predicting_from: Option<FlightState>,
    blend: Option<Blend>,
    last_now: Option<f64>,
    /// Only samples of this control epoch are accepted, when set.
    epoch: Option<u32>,
    /// This seat flew until a handover: its own last pose, used for the
    /// first output.
    handover_from: Option<FlightState>,
    /// Local time of the first output after a handover.
    handover_started: Option<f64>,
    /// The stream is shown ahead by `lead` seconds, decaying to zero.
    lead: Option<Lead>,
}

/// Lead after a handover (see [`Playout::after_handover`]).
#[derive(Debug, Clone, Copy)]
struct Lead {
    initial: f64,
    started_at: f64,
    decay: f64,
}

impl Lead {
    fn at(&self, now: f64) -> f64 {
        self.initial * (1.0 - (now - self.started_at) / self.decay).max(0.0)
    }
}

impl Default for Playout {
    fn default() -> Self {
        Self::new()
    }
}

impl Playout {
    pub fn new() -> Self {
        Self {
            samples: VecDeque::new(),
            arrivals: VecDeque::new(),
            delay: MAX_DELAY,
            mode: Mode::Empty,
            paused: false,
            last_output: None,
            predicting_from: None,
            blend: None,
            last_now: None,
            epoch: None,
            handover_from: None,
            handover_started: None,
            lead: None,
        }
    }

    /// A playout buffer for a seat that flew until now and follows
    /// `epoch` after a handover. The new pilot flying starts from the
    /// state it was showing, which trails this seat's own aircraft by
    /// the round trip and both playout delays. Instead of jumping back,
    /// the stream is shown ahead by that measured lag, decaying to zero
    /// slowly enough to change the shown speed by at most
    /// [`HANDOVER_MAX_SPEED_ERROR`], and the first second blends from
    /// `own`, this seat's last pose.
    pub fn after_handover(epoch: u32, own: FlightState) -> Self {
        Self {
            handover_from: Some(own),
            ..Self::for_epoch(epoch)
        }
    }

    /// A playout buffer that ignores samples from any other control epoch,
    /// such as late samples from the previous pilot flying.
    pub fn for_epoch(epoch: u32) -> Self {
        Self {
            epoch: Some(epoch),
            ..Self::new()
        }
    }

    /// After a handover: the measured lead (seconds) and the time it
    /// decays over, once the stream reached the render time.
    pub fn handover_lead(&self) -> Option<(f64, f64)> {
        self.lead.map(|l| (l.initial, l.decay))
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// The current render delay, seconds.
    pub fn delay(&self) -> f64 {
        self.delay
    }

    /// Adds a sample that arrived at local time `received_at` (seconds).
    /// Samples older than the newest one are stale and dropped.
    pub fn push(&mut self, state: FlightState, received_at: f64) {
        if self.epoch.is_some_and(|e| e != state.epoch) {
            return;
        }
        if let Some(newest) = self.samples.back()
            && (state.seq <= newest.seq || state.sim_time <= newest.sim_time)
        {
            return;
        }
        self.arrivals.push_back(Arrival {
            received_at,
            offset: received_at - state.sim_time,
        });
        while self
            .arrivals
            .front()
            .is_some_and(|a| received_at - a.received_at > CLOCK_WINDOW)
        {
            self.arrivals.pop_front();
        }
        self.samples.push_back(state);
    }

    /// The authority paused or resumed. While paused the output freezes;
    /// on resume the clock mapping restarts because the authority's
    /// simulator time stood still.
    pub fn set_paused(&mut self, paused: bool) {
        if self.paused && !paused {
            self.arrivals.clear();
        }
        self.paused = paused;
    }

    /// The pose to show at local time `now` (seconds).
    pub fn sample(&mut self, now: f64) -> Option<FlightState> {
        let dt = self.last_now.map(|t| (now - t).max(0.0)).unwrap_or(0.0);
        self.last_now = Some(now);
        if self.paused {
            self.mode = Mode::Paused;
            return self.last_output;
        }
        let Some(&newest) = self.samples.back() else {
            // After a handover, keep this seat's own aircraft moving by
            // dead reckoning until the new pilot flying's samples arrive.
            let own = self.handover_from?;
            let started = *self.handover_started.get_or_insert(now);
            return Some(extrapolate(&own, now - started));
        };
        let offset = self
            .arrivals
            .iter()
            .map(|a| a.offset)
            .fold(f64::INFINITY, f64::min);
        if !offset.is_finite() {
            // Resumed after a pause and nothing new yet.
            return self.last_output;
        }

        self.update_delay(offset, dt);
        let render_time = now - offset - self.delay;
        if let Some(own) = self.handover_from
            && self
                .samples
                .front()
                .is_some_and(|first| render_time < first.sim_time)
        {
            // The stream does not reach the render time yet.
            let started = *self.handover_started.get_or_insert(now);
            return Some(extrapolate(&own, now - started));
        }
        while self.samples.len() > 2
            && self
                .samples
                .get(1)
                .is_some_and(|s| s.sim_time < render_time - KEEP_BEHIND)
        {
            self.samples.pop_front();
        }

        let (target, mode) = if render_time <= newest.sim_time {
            (self.interpolate(render_time), Mode::Interpolating)
        } else {
            let ahead = render_time - newest.sim_time;
            if ahead <= MAX_EXTRAPOLATION {
                (extrapolate(&newest, ahead), Mode::Extrapolating)
            } else {
                (extrapolate(&newest, MAX_EXTRAPOLATION), Mode::Holding)
            }
        };

        // Coming back from prediction: blend from where the prediction
        // would be now (not from last frame, which would stall a frame).
        if mode == Mode::Interpolating
            && let Some(from) = self.predicting_from
        {
            let predicted =
                extrapolate(&from, (render_time - from.sim_time).min(MAX_EXTRAPOLATION));
            self.blend = Some(Blend {
                started_at: now,
                duration: BLEND_TIME,
                position: enu(&target, &predicted),
                attitude: attitude(&predicted).mul(attitude(&target).conjugate()),
            });
        }
        self.predicting_from = (mode != Mode::Interpolating).then_some(newest);
        self.mode = mode;

        let target = self.lead_target(target, now);
        let output = match self.blend {
            Some(blend) if now - blend.started_at < blend.duration => {
                let remaining = 1.0 - smoothstep((now - blend.started_at) / blend.duration);
                apply_blend(&target, &blend, remaining)
            }
            _ => {
                self.blend = None;
                target
            }
        };
        self.last_output = Some(output);
        Some(output)
    }

    /// Applies the handover lead to `target`; on the first output after a
    /// handover, measures the lead and starts the blend from the own pose.
    fn lead_target(&mut self, target: FlightState, now: f64) -> FlightState {
        if let Some(own) = self.handover_from.take() {
            let since = now - self.handover_started.unwrap_or(now);
            let own = extrapolate(&own, since);
            let v = velocity_enu(&target);
            let speed2 = v[0] * v[0] + v[1] * v[1];
            let initial = if speed2 >= HANDOVER_MIN_SPEED * HANDOVER_MIN_SPEED {
                // Along-track time from the shown state to our own.
                let d = enu(&target, &own);
                ((d[0] * v[0] + d[1] * v[1]) / speed2).clamp(0.0, HANDOVER_MAX_LEAD)
            } else {
                0.0
            };
            if initial > 0.0 {
                self.lead = Some(Lead {
                    initial,
                    started_at: now,
                    decay: (initial / HANDOVER_MAX_SPEED_ERROR).max(HANDOVER_DECAY_MIN),
                });
            }
            let led = self.apply_lead(target, now);
            self.blend = Some(Blend {
                started_at: now,
                duration: HANDOVER_BLEND_TIME,
                position: enu(&led, &own),
                attitude: attitude(&own).mul(attitude(&led).conjugate()),
            });
            return led;
        }
        self.apply_lead(target, now)
    }

    fn apply_lead(&mut self, target: FlightState, now: f64) -> FlightState {
        match self.lead {
            Some(lead) if lead.at(now) > 0.0 => extrapolate(&target, lead.at(now)),
            Some(_) => {
                self.lead = None;
                target
            }
            None => target,
        }
    }

    /// Delay target: twice the jitter plus one sample interval, within
    /// [`MIN_DELAY`], [`MAX_DELAY`], approached at a limited rate.
    fn update_delay(&mut self, min_offset: f64, dt: f64) {
        let jitter = self
            .arrivals
            .iter()
            .map(|a| a.offset - min_offset)
            .fold(0.0, f64::max);
        let interval = self.sample_interval();
        let target = (2.0 * jitter + interval).clamp(MIN_DELAY, MAX_DELAY);
        let step = DELAY_SLEW * dt;
        self.delay += (target - self.delay).clamp(-step, step);
    }

    /// Median time between recent samples.
    fn sample_interval(&self) -> f64 {
        let mut gaps: Vec<f64> = self
            .samples
            .iter()
            .zip(self.samples.iter().skip(1))
            .map(|(a, b)| b.sim_time - a.sim_time)
            .collect();
        if gaps.is_empty() {
            return 1.0 / 30.0;
        }
        gaps.sort_by(f64::total_cmp);
        gaps[gaps.len() / 2]
    }

    fn interpolate(&self, t: f64) -> FlightState {
        let first = self.samples.front().expect("samples present");
        if t <= first.sim_time {
            return with_time(*first, t);
        }
        let index = self
            .samples
            .iter()
            .position(|s| s.sim_time >= t)
            .unwrap_or(self.samples.len() - 1)
            .max(1);
        let a = &self.samples[index - 1];
        let b = &self.samples[index];
        let span = b.sim_time - a.sim_time;
        let u = ((t - a.sim_time) / span).clamp(0.0, 1.0);

        // Hermite in east/north/up metres relative to `a`.
        let p1 = enu(a, b);
        let v0 = velocity_enu(a);
        let v1 = velocity_enu(b);
        let (h00, h10, h01, h11) = hermite(u);
        let mut offset = [0.0; 3];
        for i in 0..3 {
            offset[i] = h10 * v0[i] * span + h01 * p1[i] + h11 * v1[i] * span;
            let _ = h00; // p0 is the origin
        }
        let mut out = move_by(a, offset);
        let q = attitude(a).slerp(attitude(b), u);
        set_attitude(&mut out, q);
        out.sim_time = t;
        out.seq = b.seq;
        out.velocity = lerp3(a.velocity, b.velocity, u as f32);
        out.acceleration = lerp3(a.acceleration, b.acceleration, u as f32);
        out.rates_deg = lerp3(a.rates_deg, b.rates_deg, u as f32);
        out.height_agl_m = lerp(a.height_agl_m, b.height_agl_m, u as f32);
        out.on_ground = if u < 0.5 { a.on_ground } else { b.on_ground };
        out.visuals = lerp_visuals(&a.visuals, &b.visuals, u as f32);
        out.controls = std::array::from_fn(|i| lerp_value(a.controls[i], b.controls[i], u as f32));
        out
    }
}

fn with_time(mut s: FlightState, t: f64) -> FlightState {
    s.sim_time = t;
    s
}

/// Dead reckoning `dt` seconds past `s` with its velocity, acceleration
/// and turn rate.
fn extrapolate(s: &FlightState, dt: f64) -> FlightState {
    let v = velocity_enu(s);
    let a = acceleration_enu(s);
    let offset = [
        v[0] * dt + 0.5 * a[0] * dt * dt,
        v[1] * dt + 0.5 * a[1] * dt * dt,
        v[2] * dt + 0.5 * a[2] * dt * dt,
    ];
    let mut out = move_by(s, offset);
    // Heading rate from body rates: (Q sin(phi) + R cos(phi)) / cos(theta).
    let phi = (s.phi_deg as f64).to_radians();
    let theta = (s.theta_deg as f64).to_radians();
    let q = s.rates_deg[1] as f64;
    let r = s.rates_deg[2] as f64;
    let heading_rate = (q * phi.sin() + r * phi.cos()) / theta.cos().max(0.1);
    out.psi_deg = (s.psi_deg as f64 + heading_rate * dt).rem_euclid(360.0) as f32;
    let dtf = dt as f32;
    for i in 0..3 {
        out.velocity[i] = s.velocity[i] + s.acceleration[i] * dtf;
    }
    out.sim_time = s.sim_time + dt;
    out
}

fn apply_blend(target: &FlightState, blend: &Blend, remaining: f64) -> FlightState {
    let offset = blend.position.map(|p| p * remaining);
    let mut out = move_by(target, offset);
    let error = Quat::IDENTITY.slerp(blend.attitude, remaining);
    set_attitude(&mut out, error.mul(attitude(target)));
    out
}

fn smoothstep(x: f64) -> f64 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

fn hermite(u: f64) -> (f64, f64, f64, f64) {
    let u2 = u * u;
    let u3 = u2 * u;
    (
        2.0 * u3 - 3.0 * u2 + 1.0,
        u3 - 2.0 * u2 + u,
        -2.0 * u3 + 3.0 * u2,
        u3 - u2,
    )
}

/// East/north/up metres from `from` to `to` (local tangent approximation,
/// accurate for the short distances between samples).
fn enu(from: &FlightState, to: &FlightState) -> [f64; 3] {
    let mid_lat = ((from.latitude_deg + to.latitude_deg) / 2.0).to_radians();
    [
        (to.longitude_deg - from.longitude_deg).to_radians() * EARTH_RADIUS_M * mid_lat.cos(),
        (to.latitude_deg - from.latitude_deg).to_radians() * EARTH_RADIUS_M,
        to.elevation_m - from.elevation_m,
    ]
}

/// `s` moved by east/north/up metres.
fn move_by(s: &FlightState, offset: [f64; 3]) -> FlightState {
    let mut out = *s;
    out.latitude_deg += (offset[1] / EARTH_RADIUS_M).to_degrees();
    let mid_lat = ((s.latitude_deg + out.latitude_deg) / 2.0).to_radians();
    out.longitude_deg += (offset[0] / (EARTH_RADIUS_M * mid_lat.cos())).to_degrees();
    out.elevation_m += offset[2];
    out
}

/// Local-frame (x east, y up, z south) vector as east/north/up.
fn local_to_enu(v: [f32; 3]) -> [f64; 3] {
    [v[0] as f64, -(v[2] as f64), v[1] as f64]
}

fn velocity_enu(s: &FlightState) -> [f64; 3] {
    local_to_enu(s.velocity)
}

fn acceleration_enu(s: &FlightState) -> [f64; 3] {
    local_to_enu(s.acceleration)
}

fn lerp(a: f32, b: f32, u: f32) -> f32 {
    a + (b - a) * u
}

fn lerp3(a: [f32; 3], b: [f32; 3], u: f32) -> [f32; 3] {
    [
        lerp(a[0], b[0], u),
        lerp(a[1], b[1], u),
        lerp(a[2], b[2], u),
    ]
}

fn lerp_parts(a: &[f32; WING_PARTS], b: &[f32; WING_PARTS], u: f32) -> [f32; WING_PARTS] {
    std::array::from_fn(|i| lerp(a[i], b[i], u))
}

/// Interpolates a per-frame value. Values more than 180 apart are headings
/// crossing north (ratios and attitudes never jump that far between two
/// samples), so they take the short way around the circle.
fn lerp_value(a: f32, b: f32, u: f32) -> f32 {
    let d = b - a;
    if d.abs() <= 180.0 {
        return lerp(a, b, u);
    }
    let short = (d + 540.0).rem_euclid(360.0) - 180.0;
    (a + short * u).rem_euclid(360.0)
}

fn lerp_visuals(a: &Visuals, b: &Visuals, u: f32) -> Visuals {
    Visuals {
        aileron_deg: lerp_parts(&a.aileron_deg, &b.aileron_deg, u),
        elevator_deg: lerp_parts(&a.elevator_deg, &b.elevator_deg, u),
        rudder_deg: lerp_parts(&a.rudder_deg, &b.rudder_deg, u),
        flap_deg: lerp_parts(&a.flap_deg, &b.flap_deg, u),
        nosewheel_steer_deg: lerp(a.nosewheel_steer_deg, b.nosewheel_steer_deg, u),
        engine_running: if u < 0.5 {
            a.engine_running
        } else {
            b.engine_running
        },
        prop_speed_rad_s: std::array::from_fn(|i| {
            lerp(a.prop_speed_rad_s[i], b.prop_speed_rad_s[i], u)
        }),
        gear_deploy: std::array::from_fn(|i| lerp(a.gear_deploy[i], b.gear_deploy[i], u)),
    }
}

fn attitude(s: &FlightState) -> Quat {
    Quat::from_euler(s.psi_deg as f64, s.theta_deg as f64, s.phi_deg as f64)
}

fn set_attitude(s: &mut FlightState, q: Quat) {
    let (psi, theta, phi) = q.to_euler();
    s.psi_deg = psi.rem_euclid(360.0) as f32;
    s.theta_deg = theta as f32;
    s.phi_deg = phi as f32;
}

/// A unit quaternion for heading/pitch/roll (Z-Y-X) rotations.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Quat {
    w: f64,
    x: f64,
    y: f64,
    z: f64,
}

impl Quat {
    const IDENTITY: Quat = Quat {
        w: 1.0,
        x: 0.0,
        y: 0.0,
        z: 0.0,
    };

    fn from_euler(psi_deg: f64, theta_deg: f64, phi_deg: f64) -> Quat {
        let (sy, cy) = (psi_deg.to_radians() / 2.0).sin_cos();
        let (sp, cp) = (theta_deg.to_radians() / 2.0).sin_cos();
        let (sr, cr) = (phi_deg.to_radians() / 2.0).sin_cos();
        Quat {
            w: cr * cp * cy + sr * sp * sy,
            x: sr * cp * cy - cr * sp * sy,
            y: cr * sp * cy + sr * cp * sy,
            z: cr * cp * sy - sr * sp * cy,
        }
    }

    /// (heading, pitch, roll) in degrees.
    fn to_euler(self) -> (f64, f64, f64) {
        let Quat { w, x, y, z } = self;
        let roll = (2.0 * (w * x + y * z)).atan2(1.0 - 2.0 * (x * x + y * y));
        let pitch = (2.0 * (w * y - z * x)).clamp(-1.0, 1.0).asin();
        let heading = (2.0 * (w * z + x * y)).atan2(1.0 - 2.0 * (y * y + z * z));
        (heading.to_degrees(), pitch.to_degrees(), roll.to_degrees())
    }

    fn mul(self, o: Quat) -> Quat {
        Quat {
            w: self.w * o.w - self.x * o.x - self.y * o.y - self.z * o.z,
            x: self.w * o.x + self.x * o.w + self.y * o.z - self.z * o.y,
            y: self.w * o.y - self.x * o.z + self.y * o.w + self.z * o.x,
            z: self.w * o.z + self.x * o.y - self.y * o.x + self.z * o.w,
        }
    }

    fn conjugate(self) -> Quat {
        Quat {
            w: self.w,
            x: -self.x,
            y: -self.y,
            z: -self.z,
        }
    }

    fn dot(self, o: Quat) -> f64 {
        self.w * o.w + self.x * o.x + self.y * o.y + self.z * o.z
    }

    fn scale(self, k: f64) -> Quat {
        Quat {
            w: self.w * k,
            x: self.x * k,
            y: self.y * k,
            z: self.z * k,
        }
    }

    fn add(self, o: Quat) -> Quat {
        Quat {
            w: self.w + o.w,
            x: self.x + o.x,
            y: self.y + o.y,
            z: self.z + o.z,
        }
    }

    fn normalized(self) -> Quat {
        self.scale(1.0 / self.dot(self).sqrt())
    }

    /// Spherical interpolation the short way round.
    fn slerp(self, mut o: Quat, u: f64) -> Quat {
        let mut cos = self.dot(o);
        if cos < 0.0 {
            o = o.scale(-1.0);
            cos = -cos;
        }
        if cos > 0.9995 {
            return self.scale(1.0 - u).add(o.scale(u)).normalized();
        }
        let angle = cos.acos();
        let sin = angle.sin();
        self.scale(((1.0 - u) * angle).sin() / sin)
            .add(o.scale((u * angle).sin() / sin))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trajectory::{Kind, Trajectory};

    const RATE: f64 = 30.0;
    const FPS: f64 = 60.0;

    fn trajectory(kind: Kind) -> Trajectory {
        Trajectory {
            kind,
            latitude_deg: 47.448,
            longitude_deg: -122.309,
            ground_elevation_m: 132.0,
        }
    }

    fn distance(a: &FlightState, b: &FlightState) -> f64 {
        let d = enu(a, b);
        (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt()
    }

    /// Deterministic pseudo-random numbers in [0, 1).
    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> f64 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (self.0 >> 11) as f64 / (1u64 << 53) as f64
        }
    }

    struct Network {
        latency: f64,
        jitter: f64,
        loss: f64,
        /// Sender-time windows in which nothing is delivered.
        outages: Vec<(f64, f64)>,
    }

    struct Frame {
        now: f64,
        output: FlightState,
        mode: Mode,
    }

    /// Streams `traj` for `duration` seconds through `net`, renders at 60
    /// fps from time `start_render`, and returns every rendered frame.
    fn simulate(traj: &Trajectory, net: &Network, duration: f64, seed: u64) -> Vec<Frame> {
        let mut rng = Lcg(seed);
        let mut arrivals: Vec<(f64, FlightState)> = Vec::new();
        let count = (duration * RATE) as u32;
        for seq in 0..count {
            let t = seq as f64 / RATE;
            if rng.next() < net.loss || net.outages.iter().any(|(a, b)| t >= *a && t < *b) {
                continue;
            }
            let delay = net.latency + net.jitter * (rng.next() * 2.0 - 1.0).max(-1.0);
            arrivals.push((t + delay.max(0.0), traj.state_at(t, seq)));
        }
        arrivals.sort_by(|a, b| a.0.total_cmp(&b.0));

        let mut playout = Playout::new();
        let mut frames = Vec::new();
        let mut next = 0;
        let mut now = 0.0;
        while now < duration {
            while next < arrivals.len() && arrivals[next].0 <= now {
                playout.push(arrivals[next].1, arrivals[next].0);
                next += 1;
            }
            if let Some(output) = playout.sample(now) {
                frames.push(Frame {
                    now,
                    output,
                    mode: playout.mode(),
                });
            }
            now += 1.0 / FPS;
        }
        frames
    }

    /// Largest per-frame deviation from moving at the true speed: a jump
    /// shows up as extra distance beyond speed * frame time.
    fn max_jump(frames: &[Frame], speed: f64) -> f64 {
        frames
            .windows(2)
            .map(|w| (distance(&w[0].output, &w[1].output) - speed / FPS).abs())
            .fold(0.0, f64::max)
    }

    fn settled(frames: &[Frame]) -> &[Frame] {
        // Skip the first 3 s while the delay settles.
        let start = frames.iter().position(|f| f.now >= 3.0).unwrap();
        &frames[start..]
    }

    /// Scenario: Handover in a steady climb (design D3). This seat flew
    /// east at 40 m/s; the new pilot flying's stream trails it by 0.35 s.
    #[test]
    fn handover_is_continuous_and_keeps_the_speed() {
        let speed = 40.0;
        let lag = 0.35;
        let latency = 0.05;
        let handover_at = 10.0;
        let base = FlightState {
            latitude_deg: 47.0,
            longitude_deg: 8.0,
            elevation_m: 1000.0,
            psi_deg: 90.0,
            velocity: [speed as f32, 0.0, 0.0],
            ..FlightState::default()
        };
        let at_x = |x: f64| move_by(&base, [x, 0.0, 0.0]);
        let own = at_x(speed * handover_at);
        let mut p = Playout::after_handover(1, own);

        // The new pilot flying's simulator clock starts at 100 s.
        let mut next_sample = 0;
        let mut previous: Option<(f64, FlightState)> = None;
        let mut window: Option<(f64, FlightState)> = None;
        let mut max_step: f64 = 0.0;
        let mut speeds = Vec::new();
        let frames = (12.0 * FPS) as u32;
        for frame in 0..frames {
            let now = handover_at + frame as f64 / FPS;
            // Deliver samples sent up to `now - latency`.
            while handover_at + next_sample as f64 / RATE + latency <= now {
                let sent = next_sample as f64 / RATE;
                let mut s = at_x(speed * (handover_at + sent - lag));
                s.epoch = 1;
                s.seq = next_sample;
                s.sim_time = 100.0 + sent;
                p.push(s, handover_at + sent + latency);
                next_sample += 1;
            }
            let out = p.sample(now).expect("always a pose after a handover");
            if frame == 0 {
                assert!(distance(&out, &own) < 0.01, "first pose is our own");
            }
            if let Some((_, prev)) = previous {
                max_step = max_step.max(distance(&prev, &out));
            }
            previous = Some((now, out));
            match window {
                Some((t0, start)) if now - t0 >= 0.25 => {
                    speeds.push(distance(&start, &out) / (now - t0));
                    window = Some((now, out));
                }
                None => window = Some((now, out)),
                _ => {}
            }
        }
        // No jump: one frame never moves much more than a frame of flight.
        assert!(max_step < 1.5 * speed / FPS, "largest step {max_step:.2} m");
        // The shown speed stays within 5% of the real speed.
        for (i, v) in speeds.iter().enumerate() {
            assert!(
                (v / speed - 1.0).abs() < 0.05,
                "window {i}: {v:.1} m/s instead of {speed}"
            );
        }
    }

    #[test]
    fn headings_interpolate_across_north() {
        assert!((lerp_value(0.2, 0.6, 0.5) - 0.4).abs() < 1e-6);
        assert!((lerp_value(350.0, 10.0, 0.5) % 360.0).abs() < 1e-3);
        assert!((lerp_value(10.0, 350.0, 0.25) - 5.0).abs() < 1e-3);
        assert!((lerp_value(359.0, 1.0, 0.75) - 0.5).abs() < 1e-3);
    }

    // Scenario: Samples from the previous pilot flying.
    #[test]
    fn samples_from_another_epoch_are_ignored() {
        let mut p = Playout::for_epoch(2);
        let sample = |epoch, seq: u32| FlightState {
            epoch,
            seq,
            sim_time: seq as f64 / 30.0,
            ..FlightState::default()
        };
        p.push(sample(1, 50), 0.0);
        assert_eq!(p.mode(), Mode::Empty);
        p.push(sample(2, 0), 0.0);
        p.push(sample(1, 51), 0.01);
        p.push(sample(2, 1), 0.04);
        assert_eq!(p.samples.len(), 2);
        assert!(p.samples.iter().all(|s| s.epoch == 2));
    }
    #[test]
    fn steady_turn_is_accurate_smooth_and_close_behind() {
        let traj = trajectory(Kind::Circuit);
        let net = Network {
            latency: 0.030,
            jitter: 0.004,
            loss: 0.0,
            outages: vec![],
        };
        let frames = simulate(&traj, &net, 20.0, 1);
        for f in settled(&frames) {
            assert_eq!(f.mode, Mode::Interpolating);
            let truth = traj.state_at(f.output.sim_time, 0);
            assert!(distance(&f.output, &truth) < 0.1, "error at {:.2}", f.now);
            let dpsi = (f.output.psi_deg - truth.psi_deg + 540.0).rem_euclid(360.0) - 180.0;
            assert!(dpsi.abs() < 0.1);
            assert!((f.output.phi_deg - truth.phi_deg).abs() < 0.1);
            // Spec: no more than 150 ms behind the authority.
            assert!(
                f.now - f.output.sim_time <= 0.150,
                "lag {}",
                f.now - f.output.sim_time
            );
        }
        assert!(max_jump(settled(&frames), 50.0) < 0.05);
    }

    #[test]
    fn jitter_and_loss_stay_smooth() {
        let traj = trajectory(Kind::Circuit);
        let net = Network {
            latency: 0.075,
            jitter: 0.020,
            loss: 0.05,
            outages: vec![],
        };
        let frames = simulate(&traj, &net, 30.0, 7);
        let frames = settled(&frames);
        for f in frames {
            let truth = traj.state_at(f.output.sim_time, 0);
            assert!(distance(&f.output, &truth) < 0.5, "error at {:.2}", f.now);
        }
        assert!(
            max_jump(frames, 50.0) < 0.2,
            "jump {}",
            max_jump(frames, 50.0)
        );
    }

    #[test]
    fn stale_samples_are_dropped() {
        let traj = trajectory(Kind::Circuit);
        let mut playout = Playout::new();
        playout.push(traj.state_at(1.0, 30), 1.03);
        playout.push(traj.state_at(0.9, 27), 1.04);
        assert_eq!(playout.samples.len(), 1);
    }

    #[test]
    fn brief_loss_extrapolates_along_the_turn_and_converges() {
        let traj = trajectory(Kind::Circuit);
        let net = Network {
            latency: 0.030,
            jitter: 0.002,
            loss: 0.0,
            outages: vec![(10.0, 10.3)],
        };
        let frames = simulate(&traj, &net, 16.0, 3);
        let frames = settled(&frames);
        assert!(frames.iter().any(|f| f.mode == Mode::Extrapolating));
        assert!(frames.iter().all(|f| f.mode != Mode::Holding));
        for f in frames {
            let truth = traj.state_at(f.output.sim_time, 0);
            assert!(
                distance(&f.output, &truth) < 1.0,
                "error at {:.2} ({:?})",
                f.now,
                f.mode
            );
        }
        assert!(
            max_jump(frames, 50.0) < 0.3,
            "jump {}",
            max_jump(frames, 50.0)
        );
        // Back to interpolating afterwards.
        assert_eq!(frames.last().unwrap().mode, Mode::Interpolating);
    }

    #[test]
    fn long_outage_holds_after_500_ms_then_resumes() {
        let traj = trajectory(Kind::Circuit);
        let net = Network {
            latency: 0.030,
            jitter: 0.002,
            loss: 0.0,
            outages: vec![(10.0, 12.0)],
        };
        let frames = simulate(&traj, &net, 18.0, 5);
        let frames = settled(&frames);
        let holding: Vec<&Frame> = frames.iter().filter(|f| f.mode == Mode::Holding).collect();
        assert!(!holding.is_empty());
        // Holding does not move.
        for w in holding.windows(2) {
            assert!(distance(&w[0].output, &w[1].output) < 1e-6);
        }
        // Holding starts about 500 ms after the last sample before the gap.
        let last_before = 10.0 - 1.0 / RATE;
        assert!((holding[0].output.sim_time - (last_before + MAX_EXTRAPOLATION)).abs() < 0.02);
        let last = frames.last().unwrap();
        assert_eq!(last.mode, Mode::Interpolating);
        let truth = traj.state_at(last.output.sim_time, 0);
        assert!(distance(&last.output, &truth) < 0.1);
    }

    #[test]
    fn taxi_circle_is_accurate() {
        let traj = trajectory(Kind::Taxi);
        let net = Network {
            latency: 0.040,
            jitter: 0.010,
            loss: 0.05,
            outages: vec![],
        };
        let frames = simulate(&traj, &net, 20.0, 11);
        for f in settled(&frames) {
            let truth = traj.state_at(f.output.sim_time, 0);
            assert!(distance(&f.output, &truth) < 0.05);
            assert!(f.output.on_ground);
        }
    }

    #[test]
    fn pause_freezes_output() {
        let traj = trajectory(Kind::Circuit);
        let mut playout = Playout::new();
        for seq in 0..60u32 {
            let t = seq as f64 / RATE;
            playout.push(traj.state_at(t, seq), t + 0.03);
        }
        let before = playout.sample(2.0).unwrap();
        playout.set_paused(true);
        assert_eq!(playout.sample(2.5), Some(before));
        assert_eq!(playout.sample(30.0), Some(before));
        assert_eq!(playout.mode(), Mode::Paused);
        // Resume: the authority's sim time continues where it stopped, while
        // local time moved on 28 s. The output continues without a jump.
        playout.set_paused(false);
        assert_eq!(playout.sample(30.0), Some(before));
        for seq in 60..90u32 {
            let t = seq as f64 / RATE;
            playout.push(traj.state_at(t, seq), 28.0 + t + 0.03);
            let out = playout.sample(28.0 + t + 0.03).unwrap();
            assert!(distance(&out, &traj.state_at(out.sim_time, 0)) < 0.5);
        }
    }

    #[test]
    fn quaternion_round_trip_and_slerp() {
        let q = Quat::from_euler(123.0, -10.0, 30.0);
        let (psi, theta, phi) = q.to_euler();
        assert!(
            (psi - 123.0).abs() < 1e-9 && (theta + 10.0).abs() < 1e-9 && (phi - 30.0).abs() < 1e-9
        );
        let a = Quat::from_euler(350.0, 0.0, 0.0);
        let b = Quat::from_euler(10.0, 0.0, 0.0);
        let (mid, _, _) = a.slerp(b, 0.5).to_euler();
        assert!(mid.abs() < 1e-6, "short way round through north, got {mid}");
    }
}

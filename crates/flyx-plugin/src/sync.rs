//! Flight-state sync inside X-Plane: the authority samples
//! and streams its aircraft; the follower shows the playout buffer's pose.
//!
//! The follower's flight model keeps running: every frame, after it ran,
//! the aircraft is put at the played-out position, attitude, velocity and
//! rotation rates. Its engine, gyros, air-data instruments, electrics and
//! avionics then simulate themselves from the same state and the same
//! cockpit as the pilot flying's, and nothing has to be copied gauge by
//! gauge.

use std::time::Instant;

use flyx_net::{NetCommand, NetHandle, ReceivedState};
use flyx_protocol::{FlightState, MAX_ENGINES, MAX_GEAR, MAX_INPUTS, Visuals, WING_PARTS};
use flyx_sync::playout::Playout;
use flyx_xplm::command::Command;
use flyx_xplm::dataref::{ArrayRef, DataRef};
use flyx_xplm::scenery::{TerrainProbe, world_to_local};
use tracing::{info, warn};

/// Samples sent per second of (unpaused) simulator time.
const SEND_RATE: f64 = 30.0;
/// Below this height above ground the follower sits on its own terrain.
const CLAMP_BOTTOM_M: f64 = 15.0;
/// Above this height the authority's MSL elevation is used unchanged.
const CLAMP_TOP_M: f64 = 50.0;

/// Seconds the follower's engine may run while the pilot flying's is
/// stopped, or the other way round, before it is set to match: enough for
/// a forwarded start or shutdown to take effect by itself.
const ENGINE_MISMATCH_S: f64 = 3.0;
/// Times an engine is started or stopped to match before giving up until
/// the pilot flying's engine starts or stops again.
const ENGINE_MATCH_ATTEMPTS: u8 = 2;
/// Longest the starter is held for one start attempt.
const CRANK_MAX_S: f64 = 10.0;

const CONTROL_SURFACES: &str = "sim/operation/override/override_control_surfaces";

/// Every dataref the sync reads or writes, looked up once.
pub struct Refs {
    latitude: DataRef<f64>,
    longitude: DataRef<f64>,
    elevation: DataRef<f64>,
    local: [DataRef<f64>; 3],
    psi: DataRef<f32>,
    theta: DataRef<f32>,
    phi: DataRef<f32>,
    true_psi: DataRef<f32>,
    true_theta: DataRef<f32>,
    true_phi: DataRef<f32>,
    q: ArrayRef<f32>,
    velocity: [DataRef<f32>; 3],
    acceleration: [DataRef<f32>; 3],
    rates: [DataRef<f32>; 3],
    /// The same rotation rates in rad/s, which the flight model integrates.
    rates_rad: [DataRef<f32>; 3],
    y_agl: DataRef<f32>,
    on_ground: DataRef<i32>,
    paused: DataRef<i32>,
    aileron: ArrayRef<f32>,
    elevator: ArrayRef<f32>,
    rudder: ArrayRef<f32>,
    flap: ArrayRef<f32>,
    steer: ArrayRef<f32>,
    engine_running: ArrayRef<i32>,
    prop_speed: ArrayRef<f32>,
    num_engines: DataRef<i32>,
    /// Gear deployment as X-Plane reports it (read-only)...
    gear_deploy: ArrayRef<f32>,
    /// ...and the deployment state the follower writes.
    gear_deploy_set: ArrayRef<f32>,
    control_surfaces: DataRef<i32>,
    crashed: DataRef<i32>,
}

fn scalar<T: flyx_xplm::dataref::Scalar>(name: &str) -> Result<DataRef<T>, String> {
    DataRef::find(name).ok_or_else(|| format!("dataref {name} not found"))
}

fn array<T: flyx_xplm::dataref::Element>(name: &str) -> Result<ArrayRef<T>, String> {
    ArrayRef::find(name).ok_or_else(|| format!("dataref {name} not found"))
}

impl Refs {
    pub fn find() -> Result<Refs, String> {
        let pos = |n: &str| scalar::<f32>(&format!("sim/flightmodel/position/{n}"));
        Ok(Refs {
            latitude: scalar("sim/flightmodel/position/latitude")?,
            longitude: scalar("sim/flightmodel/position/longitude")?,
            elevation: scalar("sim/flightmodel/position/elevation")?,
            local: [
                scalar("sim/flightmodel/position/local_x")?,
                scalar("sim/flightmodel/position/local_y")?,
                scalar("sim/flightmodel/position/local_z")?,
            ],
            psi: pos("psi")?,
            theta: pos("theta")?,
            phi: pos("phi")?,
            true_psi: pos("true_psi")?,
            true_theta: pos("true_theta")?,
            true_phi: pos("true_phi")?,
            q: array("sim/flightmodel/position/q")?,
            velocity: [pos("local_vx")?, pos("local_vy")?, pos("local_vz")?],
            acceleration: [pos("local_ax")?, pos("local_ay")?, pos("local_az")?],
            rates: [pos("P")?, pos("Q")?, pos("R")?],
            rates_rad: [pos("Prad")?, pos("Qrad")?, pos("Rrad")?],
            y_agl: pos("y_agl")?,
            on_ground: scalar("sim/flightmodel/failures/onground_any")?,
            paused: scalar("sim/time/paused")?,
            aileron: array("sim/flightmodel2/wing/aileron1_deg")?,
            elevator: array("sim/flightmodel2/wing/elevator1_deg")?,
            rudder: array("sim/flightmodel2/wing/rudder1_deg")?,
            flap: array("sim/flightmodel2/wing/flap1_deg")?,
            steer: array("sim/flightmodel2/gear/tire_steer_actual_deg")?,
            engine_running: array("sim/flightmodel/engine/ENGN_running")?,
            prop_speed: array("sim/flightmodel/engine/POINT_tacrad")?,
            num_engines: scalar("sim/aircraft/engine/acf_num_engines")?,
            gear_deploy: array("sim/flightmodel2/gear/deploy_ratio")?,
            gear_deploy_set: array("sim/aircraft/parts/acf_gear_deploy")?,
            control_surfaces: scalar(CONTROL_SURFACES)?,
            crashed: scalar("sim/flightmodel2/misc/has_crashed")?,
        })
    }

    fn is_paused(&self) -> bool {
        self.paused.get() != 0
    }

    fn parts(array: &ArrayRef<f32>) -> [f32; WING_PARTS] {
        let mut out = [0.0; WING_PARTS];
        array.get(0, &mut out);
        out
    }

    /// Engines of the loaded aircraft whose visuals are synced.
    fn engines(&self) -> usize {
        (self.num_engines.get().max(0) as usize).min(MAX_ENGINES)
    }

    /// The user's aircraft as a flight-state sample.
    /// Height of the aircraft's reference point over the terrain below it.
    /// (`y_agl` is measured from the lowest point of the aircraft, about
    /// zero when parked, so it cannot place the reference point.)
    fn height_over_terrain(&self, probe: &TerrainProbe) -> Option<f64> {
        let (x, y, z) = (
            self.local[0].get(),
            self.local[1].get(),
            self.local[2].get(),
        );
        probe.ground_y(x, y, z).map(|ground| y - ground)
    }

    fn sample(&self, epoch: u32, seq: u32, sim_time: f64, probe: &TerrainProbe) -> FlightState {
        let get3 = |r: &[DataRef<f32>; 3]| [r[0].get(), r[1].get(), r[2].get()];
        let engines = self.engines();
        let mut running = [0i32; MAX_ENGINES];
        self.engine_running.get(0, &mut running[..engines]);
        let mut prop_speed = [0.0f32; MAX_ENGINES];
        self.prop_speed.get(0, &mut prop_speed[..engines]);
        let mut gear_deploy = [0.0f32; MAX_GEAR];
        self.gear_deploy.get(0, &mut gear_deploy);
        FlightState {
            epoch,
            seq,
            sim_time,
            latitude_deg: self.latitude.get(),
            longitude_deg: self.longitude.get(),
            elevation_m: self.elevation.get(),
            psi_deg: self.true_psi.get(),
            theta_deg: self.true_theta.get(),
            phi_deg: self.true_phi.get(),
            velocity: get3(&self.velocity),
            acceleration: get3(&self.acceleration),
            rates_deg: get3(&self.rates),
            height_agl_m: self
                .height_over_terrain(probe)
                .map_or(self.y_agl.get(), |h| h as f32),
            on_ground: self.on_ground.get() != 0,
            visuals: Visuals {
                aileron_deg: Self::parts(&self.aileron),
                elevator_deg: Self::parts(&self.elevator),
                rudder_deg: Self::parts(&self.rudder),
                flap_deg: Self::parts(&self.flap),
                nosewheel_steer_deg: self.steer.get_one(0),
                engine_running: running.map(|r| r != 0),
                prop_speed_rad_s: prop_speed,
                gear_deploy,
            },
            controls: [0.0; MAX_INPUTS],
        }
    }

    fn set_overrides(&self, on: bool) {
        self.control_surfaces.set(on as i32);
    }

    fn engines_running(&self) -> [bool; MAX_ENGINES] {
        let mut running = [0i32; MAX_ENGINES];
        self.engine_running.get(0, &mut running[..self.engines()]);
        running.map(|r| r != 0)
    }

    /// The user's aircraft height above ground right now, if it is
    /// resting on the ground.
    fn resting_height(&self, probe: &TerrainProbe) -> Option<f64> {
        if self.on_ground.get() == 0 {
            return None;
        }
        self.height_over_terrain(probe)
    }

    /// Puts the user's aircraft at `pose`, after this frame's flight model
    /// ran. The next frame's flight model continues from there, so the
    /// position, attitude, velocity and rotation rates are all set; its
    /// forces and engines are its own. On the ground the aircraft sits
    /// `ground_height` above this simulator's terrain when known (its own
    /// measured resting height), else the height the authority reported.
    fn apply(&self, pose: &FlightState, probe: &TerrainProbe, ground_height: Option<f64>) {
        let (x, mut y, z) = world_to_local(pose.latitude_deg, pose.longitude_deg, pose.elevation_m);
        let agl = match ground_height {
            Some(h) if pose.on_ground => h,
            _ => pose.height_agl_m as f64,
        };
        if (pose.on_ground || agl < CLAMP_TOP_M)
            && let Some(ground) = probe.ground_y(x, y, z)
        {
            let on_terrain = ground + agl;
            let weight = if pose.on_ground {
                0.0
            } else {
                ((agl - CLAMP_BOTTOM_M) / (CLAMP_TOP_M - CLAMP_BOTTOM_M)).clamp(0.0, 1.0)
            };
            y = weight * y + (1.0 - weight) * on_terrain;
        }
        self.local[0].set(x);
        self.local[1].set(y);
        self.local[2].set(z);

        // The pose is earth-relative; this simulator's OpenGL frame is
        // rotated by the grid convergence at this spot, which its own
        // psi - true_psi measures.
        let convergence = wrap_180(self.psi.get() - self.true_psi.get());
        let psi = (pose.psi_deg + convergence).rem_euclid(360.0);
        self.psi.set(psi);
        self.theta.set(pose.theta_deg);
        self.phi.set(pose.phi_deg);
        self.q
            .set(0, &ogl_quaternion(psi, pose.theta_deg, pose.phi_deg));

        for i in 0..3 {
            self.velocity[i].set(pose.velocity[i]);
            self.rates[i].set(pose.rates_deg[i]);
            self.rates_rad[i].set(pose.rates_deg[i].to_radians());
        }
        let v = &pose.visuals;
        self.aileron.set(0, &v.aileron_deg);
        self.elevator.set(0, &v.elevator_deg);
        self.rudder.set(0, &v.rudder_deg);
        self.flap.set(0, &v.flap_deg);
        self.steer.set_one(0, v.nosewheel_steer_deg);
        self.gear_deploy_set.set(0, &v.gear_deploy);
    }
}

fn wrap_180(deg: f32) -> f32 {
    (deg + 540.0).rem_euclid(360.0) - 180.0
}

/// X-Plane's `sim/flightmodel/position/q` for heading, pitch and roll in
/// the local OpenGL frame (formula from the X-Plane SDK documentation).
fn ogl_quaternion(psi_deg: f32, theta_deg: f32, phi_deg: f32) -> [f32; 4] {
    let half = std::f32::consts::PI / 360.0;
    let (sp, cp) = (psi_deg * half).sin_cos();
    let (st, ct) = (theta_deg * half).sin_cos();
    let (sr, cr) = (phi_deg * half).sin_cos();
    [
        cp * ct * cr + sp * st * sr,
        cp * ct * sr - sp * st * cr,
        cp * st * cr + sp * ct * sr,
        -cp * st * sr + sp * ct * cr,
    ]
}

/// The authority's sampling loop.
pub struct Authority {
    /// Control epoch stamped on every sample.
    epoch: u32,
    /// Seconds of unpaused simulator time since streaming started.
    clock: f64,
    next_send: f64,
    seq: u32,
    paused: bool,
    probe: TerrainProbe,
}

impl Authority {
    pub fn new(epoch: u32) -> Self {
        info!(epoch, "streaming flight state");
        Self {
            epoch,
            clock: 0.0,
            next_send: 0.0,
            seq: 0,
            paused: false,
            probe: TerrainProbe::new(),
        }
    }

    /// The control epoch this seat streams.
    pub fn epoch(&self) -> u32 {
        self.epoch
    }

    /// Samples and sends at the send rate. `controls` are the pilot
    /// flying's flight-control inputs.
    pub fn frame(&mut self, refs: &Refs, dt: f32, net: &NetHandle, controls: [f32; MAX_INPUTS]) {
        let paused = refs.is_paused();
        if paused != self.paused {
            self.paused = paused;
            net.send(NetCommand::SendPaused(paused));
        }
        if paused {
            return;
        }
        self.clock += dt as f64;
        if self.clock < self.next_send {
            return;
        }
        let mut state = refs.sample(self.epoch, self.seq, self.clock, &self.probe);
        state.controls = controls;
        net.send(NetCommand::SendFlightState(state));
        self.seq = self.seq.wrapping_add(1);
        self.next_send += 1.0 / SEND_RATE;
        if self.next_send <= self.clock {
            // Frame rate below the send rate: send every frame.
            self.next_send = self.clock + 1.0 / SEND_RATE;
        }
    }
}

/// The follower: shows the authority's motion with its own flight model
/// running.
pub struct Follower {
    playout: Playout,
    origin: Instant,
    probe: TerrainProbe,
    last_pose: Option<FlightState>,
    /// This aircraft's own height above ground when parked, measured before
    /// taking over. Both seats fly the same aircraft, so it is the right
    /// height on the ground regardless of terrain or reporting differences.
    resting_height: Option<f64>,
    /// Whether the handover lead was logged.
    lead_logged: bool,
    /// Per engine: since when (seconds) it disagrees with the pilot
    /// flying's about running.
    engine_mismatch: [Option<f64>; MAX_ENGINES],
    /// Per engine: the pilot flying's state the attempts are for, and how
    /// many were made.
    engine_attempts: [(bool, u8); MAX_ENGINES],
    /// Per engine: since when its starter is held.
    cranking: [Option<f64>; MAX_ENGINES],
    /// Starter commands to begin (`true`) or end, run by the cockpit sync
    /// so they are not forwarded to the other seat.
    starter_requests: Vec<(usize, bool)>,
    /// This simulator was paused because the pilot flying paused.
    paused_with_pilot_flying: bool,
    crash_logged: bool,
}

impl Follower {
    /// Starts following samples of control epoch `control_epoch`. After a
    /// handover (`after_handover`), this seat flew until now, and the
    /// playout eases from its own aircraft into the new stream.
    /// `heading_inputs` marks the per-frame values that are headings.
    pub fn start(
        refs: &Refs,
        control_epoch: u32,
        after_handover: bool,
        heading_inputs: u32,
    ) -> Self {
        let probe = TerrainProbe::new();
        let resting_height = refs.resting_height(&probe);
        let playout = if after_handover {
            Playout::after_handover(control_epoch, refs.sample(control_epoch, 0, 0.0, &probe))
        } else {
            Playout::for_epoch(control_epoch)
        }
        .with_heading_inputs(heading_inputs);
        refs.set_overrides(true);
        if let Some(h) = resting_height {
            info!(height_m = h, "measured resting height on the ground");
        }
        info!(
            control_epoch,
            after_handover, "following: aircraft placed every frame, flight model running"
        );
        Self {
            playout,
            origin: Instant::now(),
            probe,
            last_pose: None,
            resting_height,
            lead_logged: !after_handover,
            engine_mismatch: [None; MAX_ENGINES],
            engine_attempts: [(false, 0); MAX_ENGINES],
            cranking: [None; MAX_ENGINES],
            starter_requests: Vec::new(),
            paused_with_pilot_flying: false,
            crash_logged: false,
        }
    }

    pub fn push(&mut self, received: ReceivedState) {
        let at = received
            .received_at
            .saturating_duration_since(self.origin)
            .as_secs_f64();
        self.playout.push(received.state, at);
    }

    /// The pilot flying paused or resumed: the playout holds, and this
    /// simulator pauses with it, so its flight model and engines wait too.
    pub fn set_paused(&mut self, paused: bool, refs: &Refs) {
        self.playout.set_paused(paused);
        if paused != refs.is_paused() {
            run_command(if paused {
                "sim/operation/pause_on"
            } else {
                "sim/operation/pause_off"
            });
        }
        self.paused_with_pilot_flying = paused;
    }

    /// Shows the next pose; returns it.
    pub fn frame(&mut self, refs: &Refs) -> Option<&FlightState> {
        let now = self.origin.elapsed().as_secs_f64();
        if let Some(pose) = self.playout.sample(now) {
            refs.apply(&pose, &self.probe, self.resting_height);
            self.match_engines(refs, &pose, now);
            self.last_pose = Some(pose);
        }
        if !self.crash_logged && refs.crashed.get() != 0 {
            self.crash_logged = true;
            warn!("this simulator's aircraft crashed while following");
        }
        if !self.lead_logged && self.playout.mode() == flyx_sync::playout::Mode::Interpolating {
            self.lead_logged = true;
            match self.playout.handover_lead() {
                Some((lead, decay)) => info!(
                    lead_ms = (lead * 1000.0).round(),
                    decay_s = decay,
                    "handover: following with a lead"
                ),
                None => info!("handover: following without a lead (slow or stopped)"),
            }
        }
        self.last_pose.as_ref()
    }

    /// Engines normally start and stop by themselves, because their
    /// controls and the forwarded starter are the pilot flying's. One that
    /// still disagrees after [`ENGINE_MISMATCH_S`] (for example because
    /// this seat joined with its engine off) is started with its starter
    /// until it runs, or stopped, at most [`ENGINE_MATCH_ATTEMPTS`] times:
    /// an engine whose own controls cannot keep it running is not cranked
    /// over and over.
    fn match_engines(&mut self, refs: &Refs, pose: &FlightState, now: f64) {
        let local = refs.engines_running();
        for i in 0..refs.engines() {
            let wanted = pose.visuals.engine_running[i];
            if self.engine_attempts[i].0 != wanted {
                self.engine_attempts[i] = (wanted, 0);
            }
            if let Some(since) = self.cranking[i] {
                if local[i] || !wanted || now - since >= CRANK_MAX_S {
                    self.cranking[i] = None;
                    self.starter_requests.push((i, false));
                    info!(engine = i, started = local[i], "starter released");
                }
                continue;
            }
            if local[i] == wanted || self.engine_attempts[i].1 > ENGINE_MATCH_ATTEMPTS {
                self.engine_mismatch[i] = None;
                continue;
            }
            let since = *self.engine_mismatch[i].get_or_insert(now);
            if now - since < ENGINE_MISMATCH_S {
                continue;
            }
            self.engine_mismatch[i] = None;
            self.engine_attempts[i].1 += 1;
            if self.engine_attempts[i].1 > ENGINE_MATCH_ATTEMPTS {
                warn!(
                    engine = i,
                    running = wanted,
                    "engine does not stay like the pilot flying's; mixture, magnetos or fuel differ"
                );
                continue;
            }
            if wanted {
                info!(
                    engine = i,
                    "engine stopped while the pilot flying's runs: cranking it"
                );
                self.cranking[i] = Some(now);
                self.starter_requests.push((i, true));
            } else {
                info!(
                    engine = i,
                    "engine runs while the pilot flying's is stopped: stopping it"
                );
                let mut running = local.map(|r| r as i32);
                running[i] = 0;
                refs.engine_running.set(0, &running[..refs.engines()]);
            }
        }
    }

    /// Starter commands to run now: (engine, begin or end).
    pub fn take_starter_requests(&mut self) -> Vec<(usize, bool)> {
        std::mem::take(&mut self.starter_requests)
    }

    /// Gives the aircraft back to X-Plane: the last pose and its velocities
    /// are written, then the overrides cleared. The flight model was
    /// running all along, so it continues from there without a jump.
    /// Returns the starters still held, which the caller releases.
    pub fn release(mut self, refs: &Refs) -> Vec<(usize, bool)> {
        if let Some(pose) = &self.last_pose {
            refs.apply(pose, &self.probe, self.resting_height);
        }
        refs.set_overrides(false);
        if self.paused_with_pilot_flying && refs.is_paused() {
            run_command("sim/operation/pause_off");
        }
        info!("stopped following: aircraft handed back to X-Plane");
        for (i, cranking) in self.cranking.iter().enumerate() {
            if cranking.is_some() {
                self.starter_requests.push((i, false));
            }
        }
        self.starter_requests
    }
}

fn run_command(name: &str) {
    match Command::find(name) {
        Some(command) => command.once(),
        None => warn!(name, "command not found"),
    }
}

/// Clears the overrides without any other state; used by the emergency
/// teardown after an internal error.
pub fn emergency_release() {
    match DataRef::<i32>::find(CONTROL_SURFACES) {
        Some(surfaces) => surfaces.set(0),
        None => warn!("emergency release: override dataref not found"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quaternion_matches_level_flight_headings() {
        // Level, heading 0: identity.
        let q = ogl_quaternion(0.0, 0.0, 0.0);
        assert!((q[0] - 1.0).abs() < 1e-6 && q[1].abs() < 1e-6);
        // Heading 90: rotation about the vertical by 90 degrees.
        let q = ogl_quaternion(90.0, 0.0, 0.0);
        let s = std::f32::consts::FRAC_1_SQRT_2;
        assert!((q[0] - s).abs() < 1e-6 && (q[3] - s).abs() < 1e-6);
        // Unit length for arbitrary attitudes.
        let q = ogl_quaternion(123.0, -8.0, 35.0);
        let n: f32 = q.iter().map(|c| c * c).sum();
        assert!((n - 1.0).abs() < 1e-5);
    }

    #[test]
    fn convergence_wraps() {
        assert_eq!(wrap_180(359.0), -1.0);
        assert_eq!(wrap_180(-359.0), 1.0);
        assert_eq!(wrap_180(0.5), 0.5);
    }
}

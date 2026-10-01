//! Flight-state sync inside X-Plane: the authority samples
//! and streams its aircraft; the follower shows the playout buffer's pose.

use std::time::Instant;

use flyx_net::{NetCommand, NetHandle, ReceivedState};
use flyx_protocol::{FlightState, MAX_ENGINES, MAX_GEAR, MAX_INPUTS, Visuals, WING_PARTS};
use flyx_sync::playout::Playout;
use flyx_xplm::dataref::{ArrayRef, DataRef};
use flyx_xplm::scenery::{TerrainProbe, world_to_local};
use tracing::{info, warn};

/// Samples sent per second of (unpaused) simulator time.
const SEND_RATE: f64 = 30.0;
/// Below this height above ground the follower sits on its own terrain.
const CLAMP_BOTTOM_M: f64 = 15.0;
/// Above this height the authority's MSL elevation is used unchanged.
const CLAMP_TOP_M: f64 = 50.0;

const PLANEPATH: &str = "sim/operation/override/override_planepath";
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
    planepath: ArrayRef<i32>,
    control_surfaces: DataRef<i32>,
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
            planepath: array(PLANEPATH)?,
            control_surfaces: scalar(CONTROL_SURFACES)?,
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
    fn sample(&self, epoch: u32, seq: u32, sim_time: f64) -> FlightState {
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
            height_agl_m: self.y_agl.get(),
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
        self.planepath.set_one(0, on as i32);
        self.control_surfaces.set(on as i32);
    }

    /// Moves the user's aircraft to `pose`.
    /// The user's aircraft height above ground right now, if it is
    /// resting on the ground.
    fn resting_height(&self) -> Option<f64> {
        (self.on_ground.get() != 0).then(|| self.y_agl.get() as f64)
    }

    /// Moves the user's aircraft to `pose`. On the ground the aircraft sits
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
            self.acceleration[i].set(pose.acceleration[i]);
            self.rates[i].set(pose.rates_deg[i]);
        }
        let v = &pose.visuals;
        self.aileron.set(0, &v.aileron_deg);
        self.elevator.set(0, &v.elevator_deg);
        self.rudder.set(0, &v.rudder_deg);
        self.flap.set(0, &v.flap_deg);
        self.steer.set_one(0, v.nosewheel_steer_deg);
        let engines = self.engines();
        let running = v.engine_running.map(|r| r as i32);
        self.engine_running.set(0, &running[..engines]);
        self.prop_speed.set(0, &v.prop_speed_rad_s[..engines]);
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
        }
    }

    pub fn frame(&mut self, refs: &Refs, dt: f32, net: &NetHandle) {
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
        net.send(NetCommand::SendFlightState(
            refs.sample(self.epoch, self.seq, self.clock),
        ));
        self.seq = self.seq.wrapping_add(1);
        self.next_send += 1.0 / SEND_RATE;
        if self.next_send <= self.clock {
            // Frame rate below the send rate: send every frame.
            self.next_send = self.clock + 1.0 / SEND_RATE;
        }
    }
}

/// The follower: overrides the flight model and shows the authority's
/// motion (tasks 5.4-5.6).
pub struct Follower {
    playout: Playout,
    epoch: Instant,
    probe: TerrainProbe,
    last_pose: Option<FlightState>,
    /// This aircraft's own height above ground when parked, measured before
    /// taking over. Both seats fly the same aircraft, so it is the right
    /// height on the ground regardless of terrain or reporting differences.
    resting_height: Option<f64>,
}

impl Follower {
    pub fn start(refs: &Refs) -> Self {
        let resting_height = refs.resting_height();
        refs.set_overrides(true);
        if let Some(h) = resting_height {
            info!(height_m = h, "measured resting height on the ground");
        }
        info!("following: flight-model path and control surfaces overridden");
        Self {
            playout: Playout::new(),
            epoch: Instant::now(),
            probe: TerrainProbe::new(),
            last_pose: None,
            resting_height,
        }
    }

    pub fn push(&mut self, received: ReceivedState) {
        let at = received
            .received_at
            .saturating_duration_since(self.epoch)
            .as_secs_f64();
        self.playout.push(received.state, at);
    }

    pub fn set_paused(&mut self, paused: bool) {
        self.playout.set_paused(paused);
    }

    pub fn frame(&mut self, refs: &Refs) {
        let now = self.epoch.elapsed().as_secs_f64();
        if let Some(pose) = self.playout.sample(now) {
            refs.apply(&pose, &self.probe, self.resting_height);
            self.last_pose = Some(pose);
        }
    }

    /// Gives the aircraft back to X-Plane in one frame: the last pose and
    /// its velocities are written, then the overrides cleared, so the
    /// flight model continues from there without a jump.
    pub fn release(self, refs: &Refs) {
        if let Some(pose) = &self.last_pose {
            refs.apply(pose, &self.probe, self.resting_height);
        }
        refs.set_overrides(false);
        info!("stopped following: aircraft handed back to X-Plane");
    }
}

/// Clears the overrides without any other state; used by the emergency
/// teardown after an internal error.
pub fn emergency_release() {
    match (
        ArrayRef::<i32>::find(PLANEPATH),
        DataRef::<i32>::find(CONTROL_SURFACES),
    ) {
        (Some(planepath), Some(surfaces)) => {
            planepath.set_one(0, 0);
            surfaces.set(0);
        }
        _ => warn!("emergency release: override datarefs not found"),
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

//! Synthetic flights for testing without a second simulator: a steady turn
//! in the air, a taxi circle on the ground, and a parked aircraft. Used by
//! `flyx-peer` and by the interpolation tests.

use flyx_protocol::{FlightState, MAX_GEAR, MAX_INPUTS, Visuals};

const EARTH_RADIUS_M: f64 = 6_371_000.0;
const G: f64 = 9.806_65;
/// Height of a Cessna 172's reference point above the ground when parked.
pub const C172_GROUND_HEIGHT_M: f64 = 1.7;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    /// A level circle at constant speed and bank.
    Circuit,
    /// A slow circle on the ground at 15 kt.
    Taxi,
    /// Parked with the engine running.
    Parked,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Trajectory {
    pub kind: Kind,
    /// Centre of the circle (or the parking spot).
    pub latitude_deg: f64,
    pub longitude_deg: f64,
    /// Ground elevation at the centre, metres MSL.
    pub ground_elevation_m: f64,
}

impl Trajectory {
    fn radius_m(&self) -> f64 {
        match self.kind {
            Kind::Circuit => 1500.0,
            Kind::Taxi => 40.0,
            Kind::Parked => 0.0,
        }
    }

    fn speed_mps(&self) -> f64 {
        match self.kind {
            Kind::Circuit => 50.0,          // about 97 kt
            Kind::Taxi => 15.0 * 0.514_444, // 15 kt
            Kind::Parked => 0.0,
        }
    }

    fn height_agl_m(&self) -> f64 {
        match self.kind {
            Kind::Circuit => 450.0,
            // A Cessna 172's reference point sits about 1.7 m above the
            // ground on its wheels (main gear 5.5 ft below it in the .acf).
            Kind::Taxi | Kind::Parked => C172_GROUND_HEIGHT_M,
        }
    }

    /// The state at simulator time `t` seconds.
    pub fn state_at(&self, t: f64, seq: u32) -> FlightState {
        let r = self.radius_m();
        let v = self.speed_mps();
        // Angle around the circle, clockwise seen from above.
        let omega = if r > 0.0 { v / r } else { 0.0 };
        let angle = omega * t;
        let (s, c) = angle.sin_cos();
        // Position relative to the centre: start north of it, fly clockwise.
        let east = r * s;
        let north = r * c;
        // Heading is tangent to the circle, clockwise: 90° at the start.
        let heading = (angle.to_degrees() + 90.0).rem_euclid(360.0);
        // Velocity in east/north, then in X-Plane's local frame (x east,
        // y up, z south).
        let v_east = v * c;
        let v_north = -v * s;
        // Centripetal acceleration points at the centre.
        let a = v * omega;
        let a_east = -a * s;
        let a_north = -a * c;
        let bank = (v * v / (G * r.max(1.0))).atan();
        let on_ground = self.kind != Kind::Circuit;

        let lat = self.latitude_deg + (north / EARTH_RADIUS_M).to_degrees();
        let lon = self.longitude_deg
            + (east / (EARTH_RADIUS_M * self.latitude_deg.to_radians().cos())).to_degrees();

        FlightState {
            epoch: 0,
            seq,
            sim_time: t,
            latitude_deg: lat,
            longitude_deg: lon,
            elevation_m: self.ground_elevation_m + self.height_agl_m(),
            psi_deg: heading as f32,
            theta_deg: if on_ground { 0.0 } else { 2.0 },
            phi_deg: if on_ground {
                0.0
            } else {
                bank.to_degrees() as f32
            },
            velocity: [v_east as f32, 0.0, -v_north as f32],
            acceleration: [a_east as f32, 0.0, -a_north as f32],
            rates_deg: [
                0.0,
                (omega * bank.sin()).to_degrees() as f32,
                (omega * bank.cos()).to_degrees() as f32,
            ],
            height_agl_m: self.height_agl_m() as f32,
            on_ground,
            visuals: Visuals {
                // A little aileron holding the bank, a little up elevator;
                // nose wheel steering into the taxi turn.
                aileron_deg: if on_ground {
                    [0.0; 6]
                } else {
                    [2.0, -2.0, 2.0, -2.0, 0.0, 0.0]
                },
                elevator_deg: if on_ground {
                    [0.0; 6]
                } else {
                    [0.0, 0.0, 0.0, 0.0, -1.5, -1.5]
                },
                rudder_deg: [0.0; 6],
                flap_deg: [0.0; 6],
                nosewheel_steer_deg: if self.kind == Kind::Taxi { 12.0 } else { 0.0 },
                engine_running: [true, false, false, false, false, false, false, false],
                prop_speed_rad_s: [
                    match self.kind {
                        Kind::Circuit => 251.3, // 2400 rpm
                        Kind::Taxi => 115.2,    // 1100 rpm
                        Kind::Parked => 83.8,   // 800 rpm
                    },
                    0.0,
                    0.0,
                    0.0,
                    0.0,
                    0.0,
                    0.0,
                    0.0,
                ],
                gear_deploy: [1.0; MAX_GEAR],
            },
            controls: [0.0; MAX_INPUTS],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(kind: Kind) -> Trajectory {
        Trajectory {
            kind,
            latitude_deg: 47.448,
            longitude_deg: -122.309,
            ground_elevation_m: 132.0,
        }
    }

    /// Distance in metres between two samples (flat-earth approximation).
    fn distance(a: &FlightState, b: &FlightState) -> f64 {
        let north = (b.latitude_deg - a.latitude_deg).to_radians() * EARTH_RADIUS_M;
        let east = (b.longitude_deg - a.longitude_deg).to_radians()
            * EARTH_RADIUS_M
            * a.latitude_deg.to_radians().cos();
        (north * north + east * east).sqrt()
    }

    #[test]
    fn circuit_moves_at_its_speed_and_matches_velocity() {
        let t = at(Kind::Circuit);
        let a = t.state_at(10.0, 0);
        let b = t.state_at(10.1, 1);
        let moved = distance(&a, &b);
        assert!((moved - 5.0).abs() < 0.05, "moved {moved} m in 0.1 s");
        let speed = (a.velocity[0].powi(2) + a.velocity[2].powi(2)).sqrt();
        assert!((speed - 50.0).abs() < 0.01);
        // Velocity points along the direction of travel (east, -south).
        let predicted_east = a.velocity[0] as f64 * 0.1;
        let actual_east = (b.longitude_deg - a.longitude_deg).to_radians()
            * EARTH_RADIUS_M
            * a.latitude_deg.to_radians().cos();
        assert!((predicted_east - actual_east).abs() < 0.1);
        assert!(!a.on_ground);
        assert!(a.phi_deg > 5.0);
    }

    #[test]
    fn taxi_is_on_the_ground_at_15_knots() {
        let t = at(Kind::Taxi);
        let s = t.state_at(3.0, 0);
        assert!(s.on_ground);
        assert_eq!(s.height_agl_m, C172_GROUND_HEIGHT_M as f32);
        let speed = (s.velocity[0].powi(2) + s.velocity[2].powi(2)).sqrt();
        assert!((speed - 7.716).abs() < 0.01);
        // A quarter circle takes (pi/2 * r) / v seconds; heading turns 90°.
        let quarter = std::f64::consts::FRAC_PI_2 * 40.0 / 7.716_66;
        let h0 = t.state_at(0.0, 0).psi_deg;
        let h1 = t.state_at(quarter, 1).psi_deg;
        assert!(((h1 - h0).rem_euclid(360.0) - 90.0).abs() < 0.5);
    }

    #[test]
    fn parked_does_not_move() {
        let t = at(Kind::Parked);
        let a = t.state_at(0.0, 0);
        let b = t.state_at(60.0, 1);
        assert_eq!(distance(&a, &b), 0.0);
        assert!(a.visuals.engine_running[0]);
    }
}

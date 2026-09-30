//! FlyXTogether wire protocol: message types and framing.
//!
//! A session uses one QUIC connection. Handshake and session control
//! messages travel on one bidirectional stream as length-prefixed frames
//! ([`encode_frame`] / [`FrameDecoder`]). Flight state travels as QUIC
//! datagrams ([`Datagram`]), where stale samples may be dropped.

use serde::{Deserialize, Serialize};

/// Bumped whenever a message layout or the handshake changes.
///
/// Compatibility rule: `Control::Hello` (variant 0), `Control::Reject`
/// (variant 6) and `RejectReason::VersionMismatch` (variant 2) keep their
/// layout forever, so that any two versions can tell each other which
/// versions they run.
pub const PROTOCOL_VERSION: u16 = 1;

/// QUIC ALPN identifier. It never changes: versions are compared in
/// `Control::Hello` so that mismatches can be reported to the user instead
/// of failing inside the TLS handshake.
pub const ALPN: &[u8] = b"flyx";

/// Largest control frame accepted, to bound memory use.
pub const MAX_FRAME_LEN: usize = 64 * 1024;

/// The aircraft loaded in a simulator: its folder name and `.acf` file name,
/// e.g. `Cessna 172 SP` / `Cessna_172SP_G1000.acf`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AircraftId {
    pub folder: String,
    pub acf: String,
}

/// Argon2id parameters chosen by the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct KdfParams {
    /// Memory cost in KiB.
    pub memory_kib: u32,
    /// Number of passes.
    pub iterations: u32,
    /// Degree of parallelism.
    pub parallelism: u32,
}

impl KdfParams {
    /// Parameters the host uses: 19 MiB, 2 passes (OWASP minimum for Argon2id).
    pub const DEFAULT: KdfParams = KdfParams {
        memory_kib: 19 * 1024,
        iterations: 2,
        parallelism: 1,
    };

    /// Whether a joiner should accept these parameters. Caps protect the
    /// joiner from a host that asks for absurd amounts of memory or time.
    pub fn is_acceptable(&self) -> bool {
        (8 * 1024..=64 * 1024).contains(&self.memory_kib)
            && (1..=8).contains(&self.iterations)
            && (1..=4).contains(&self.parallelism)
    }
}

/// Messages on the control stream, in handshake order first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Control {
    /// Joiner -> host, first message. Only versions are revealed before the
    /// password proof.
    Hello {
        protocol_version: u16,
        plugin_version: String,
    },
    /// Host -> joiner: parameters for deriving the password key.
    Challenge { salt: [u8; 16], kdf: KdfParams },
    /// Joiner -> host: proof of the password, bound to this TLS session.
    ClientProof { mac: [u8; 32] },
    /// Host -> joiner: the host's proof of the same password.
    HostProof { mac: [u8; 32] },
    /// Joiner -> host, after both proofs: who is joining with what aircraft.
    Join {
        display_name: String,
        aircraft: AircraftId,
    },
    /// Host -> joiner: admitted to the session.
    Welcome {
        display_name: String,
        aircraft: AircraftId,
    },
    /// Host -> joiner: not admitted. The connection closes after this.
    Reject(RejectReason),
    /// Authority -> follower: simulator paused or resumed.
    Paused(bool),
    /// Either side: leaving the session on purpose.
    Bye(ByeReason),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RejectReason {
    BadPassword,
    /// Too many failed attempts from this address; try again shortly.
    TooManyAttempts,
    VersionMismatch {
        host_protocol_version: u16,
        host_plugin_version: String,
    },
    /// The joiner has a different aircraft loaded than the host.
    AircraftMismatch {
        host_aircraft: AircraftId,
    },
    /// The joiner's aircraft is not supported by this version.
    UnsupportedAircraft,
    SessionFull,
    /// The host is not accepting joins (e.g. shutting down).
    NotHosting,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ByeReason {
    /// The user left the session (or cancelled joining).
    Left,
    /// The host stopped hosting.
    StoppedHosting,
    /// A different aircraft was loaded, or the current one reloaded.
    AircraftChanged,
    /// The plugin was disabled or X-Plane is shutting down.
    PluginStopped,
    /// The plugin stopped after an internal error.
    InternalError,
}

/// Messages sent as QUIC datagrams.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Datagram {
    FlightState(FlightState),
}

/// One sample of the authority's aircraft state.
///
/// Position and attitude are geodetic and true, because each simulator's
/// local OpenGL frame has its own origin. Velocities, accelerations and
/// rates use the authority's local frame, which is treated as east-up-south
/// near the aircraft.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct FlightState {
    /// Increases by one per sample; used to drop stale datagrams.
    pub seq: u32,
    /// Authority simulator time in seconds (stops while paused).
    pub sim_time: f64,
    pub latitude_deg: f64,
    pub longitude_deg: f64,
    pub elevation_m: f64,
    /// True heading, pitch and roll in degrees.
    pub psi_deg: f32,
    pub theta_deg: f32,
    pub phi_deg: f32,
    /// Local-frame velocity in m/s (x east, y up, z south).
    pub velocity: [f32; 3],
    /// Local-frame acceleration in m/s².
    pub acceleration: [f32; 3],
    /// Body rates P, Q, R in degrees per second.
    pub rates_deg: [f32; 3],
    /// Height of the aircraft's reference point above ground, in metres.
    pub height_agl_m: f32,
    pub on_ground: bool,
    pub visuals: Visuals,
}

/// Number of wing parts whose control surfaces are synced. The Cessna
/// 172 SP uses parts 0-5.
pub const WING_PARTS: usize = 6;

/// Values that make the follower's aircraft look like the authority's.
///
/// Surfaces are X-Plane's per-wing-part deflections
/// (`sim/flightmodel2/wing/*1_deg`), which the aircraft's 3D model animates
/// from. Both seats fly the same aircraft, so the part layout matches.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Visuals {
    pub aileron_deg: [f32; WING_PARTS],
    pub elevator_deg: [f32; WING_PARTS],
    pub rudder_deg: [f32; WING_PARTS],
    pub flap_deg: [f32; WING_PARTS],
    /// Nose wheel steering angle, degrees, positive right.
    pub nosewheel_steer_deg: f32,
    pub engine_running: bool,
    /// Propeller speed in radians per second; the follower's X-Plane
    /// animates the propeller from it.
    pub prop_speed_rad_s: f32,
}

/// Errors when decoding frames or datagrams.
#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("frame of {0} bytes exceeds the {MAX_FRAME_LEN}-byte limit")]
    FrameTooLarge(usize),
    #[error("malformed message: {0}")]
    Malformed(#[from] postcard::Error),
}

/// Encodes a control message as a frame: 4-byte little-endian length, then
/// the postcard-encoded message.
pub fn encode_frame(message: &Control) -> Vec<u8> {
    let body = postcard::to_allocvec(message).expect("control messages always serialise");
    let mut frame = Vec::with_capacity(4 + body.len());
    frame.extend_from_slice(&(body.len() as u32).to_le_bytes());
    frame.extend_from_slice(&body);
    frame
}

/// Splits a byte stream into control messages.
#[derive(Debug, Default)]
pub struct FrameDecoder {
    buffer: Vec<u8>,
}

impl FrameDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds received bytes.
    pub fn push(&mut self, bytes: &[u8]) {
        self.buffer.extend_from_slice(bytes);
    }

    /// Returns the next complete message, if one has arrived.
    pub fn next_message(&mut self) -> Result<Option<Control>, DecodeError> {
        if self.buffer.len() < 4 {
            return Ok(None);
        }
        let len = u32::from_le_bytes(self.buffer[..4].try_into().unwrap()) as usize;
        if len > MAX_FRAME_LEN {
            return Err(DecodeError::FrameTooLarge(len));
        }
        if self.buffer.len() < 4 + len {
            return Ok(None);
        }
        let message = postcard::from_bytes(&self.buffer[4..4 + len])?;
        self.buffer.drain(..4 + len);
        Ok(Some(message))
    }
}

impl Datagram {
    pub fn encode(&self) -> Vec<u8> {
        postcard::to_allocvec(self).expect("datagrams always serialise")
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        Ok(postcard::from_bytes(bytes)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c172() -> AircraftId {
        AircraftId {
            folder: "Cessna 172 SP".into(),
            acf: "Cessna_172SP.acf".into(),
        }
    }

    fn sample_state() -> FlightState {
        FlightState {
            seq: 4_000_000_000,
            sim_time: 12345.678,
            latitude_deg: 47.449_888_123,
            longitude_deg: -122.311_777_456,
            elevation_m: 1234.5,
            psi_deg: 359.9,
            theta_deg: -12.5,
            phi_deg: 45.0,
            velocity: [55.1, -3.2, -20.7],
            acceleration: [0.1, 9.8, -0.3],
            rates_deg: [1.0, -2.0, 3.0],
            height_agl_m: 300.25,
            on_ground: false,
            visuals: Visuals {
                aileron_deg: [-15.0, 12.0, 11.5, -14.0, 0.1, -0.1],
                elevator_deg: [5.5, 5.5, 5.4, 5.4, 0.0, 0.0],
                rudder_deg: [-3.0, 3.0, -3.0, 3.0, 2.9, -2.9],
                flap_deg: [15.0, 15.0, 15.0, 15.0, 15.0, 15.0],
                nosewheel_steer_deg: -7.5,
                engine_running: true,
                prop_speed_rad_s: 251.3,
            },
        }
    }

    fn all_control_messages() -> Vec<Control> {
        vec![
            Control::Hello {
                protocol_version: PROTOCOL_VERSION,
                plugin_version: "0.1.0".into(),
            },
            Control::Challenge {
                salt: [7; 16],
                kdf: KdfParams::DEFAULT,
            },
            Control::ClientProof { mac: [1; 32] },
            Control::HostProof { mac: [2; 32] },
            Control::Join {
                display_name: "Alex".into(),
                aircraft: c172(),
            },
            Control::Welcome {
                display_name: "Sam".into(),
                aircraft: c172(),
            },
            Control::Reject(RejectReason::BadPassword),
            Control::Reject(RejectReason::TooManyAttempts),
            Control::Reject(RejectReason::VersionMismatch {
                host_protocol_version: 2,
                host_plugin_version: "0.2.0".into(),
            }),
            Control::Reject(RejectReason::AircraftMismatch {
                host_aircraft: c172(),
            }),
            Control::Reject(RejectReason::UnsupportedAircraft),
            Control::Reject(RejectReason::SessionFull),
            Control::Reject(RejectReason::NotHosting),
            Control::Paused(true),
            Control::Bye(ByeReason::Left),
            Control::Bye(ByeReason::StoppedHosting),
            Control::Bye(ByeReason::AircraftChanged),
            Control::Bye(ByeReason::PluginStopped),
            Control::Bye(ByeReason::InternalError),
        ]
    }

    #[test]
    fn every_control_message_round_trips() {
        for message in all_control_messages() {
            let frame = encode_frame(&message);
            let mut decoder = FrameDecoder::new();
            decoder.push(&frame);
            assert_eq!(decoder.next_message().unwrap(), Some(message));
            assert_eq!(decoder.next_message().unwrap(), None);
        }
    }

    #[test]
    fn decoder_handles_split_and_concatenated_frames() {
        let messages = all_control_messages();
        let stream: Vec<u8> = messages.iter().flat_map(encode_frame).collect();
        let mut decoder = FrameDecoder::new();
        let mut decoded = Vec::new();
        // Feed one byte at a time.
        for byte in stream {
            decoder.push(&[byte]);
            while let Some(m) = decoder.next_message().unwrap() {
                decoded.push(m);
            }
        }
        assert_eq!(decoded, messages);
    }

    #[test]
    fn frozen_variants_keep_their_wire_indices() {
        let hello = postcard::to_allocvec(&Control::Hello {
            protocol_version: 1,
            plugin_version: String::new(),
        })
        .unwrap();
        assert_eq!(hello[0], 0);
        let reject = postcard::to_allocvec(&Control::Reject(RejectReason::VersionMismatch {
            host_protocol_version: 1,
            host_plugin_version: String::new(),
        }))
        .unwrap();
        assert_eq!(&reject[..2], &[6, 2]);
    }

    #[test]
    fn oversized_frame_is_rejected() {
        let mut decoder = FrameDecoder::new();
        decoder.push(&((MAX_FRAME_LEN as u32) + 1).to_le_bytes());
        assert!(matches!(
            decoder.next_message(),
            Err(DecodeError::FrameTooLarge(_))
        ));
    }

    #[test]
    fn garbage_frame_is_malformed() {
        let mut decoder = FrameDecoder::new();
        decoder.push(&3u32.to_le_bytes());
        decoder.push(&[0xff, 0xff, 0xff]);
        assert!(matches!(
            decoder.next_message(),
            Err(DecodeError::Malformed(_))
        ));
    }

    #[test]
    fn flight_state_round_trips_and_fits_in_256_bytes() {
        let datagram = Datagram::FlightState(sample_state());
        let bytes = datagram.encode();
        assert!(
            bytes.len() < 256,
            "FlightState datagram is {} bytes",
            bytes.len()
        );
        assert_eq!(Datagram::decode(&bytes).unwrap(), datagram);
    }

    #[test]
    fn default_kdf_params_are_acceptable_and_caps_apply() {
        assert!(KdfParams::DEFAULT.is_acceptable());
        let greedy = KdfParams {
            memory_kib: 1024 * 1024,
            ..KdfParams::DEFAULT
        };
        assert!(!greedy.is_acceptable());
        let weak = KdfParams {
            memory_kib: 64,
            ..KdfParams::DEFAULT
        };
        assert!(!weak.is_acceptable());
    }
}

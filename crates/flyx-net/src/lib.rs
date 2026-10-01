//! QUIC transport for FlyXTogether sessions.
//!
//! [`spawn`] starts a network task on a tokio runtime and returns a
//! [`NetHandle`]. The simulator thread sends [`NetCommand`]s and polls
//! events and flight-state samples without ever blocking.

mod address;
mod auth;
mod control;
mod endpoint;
mod host;
mod join;

use std::sync::mpsc as std_mpsc;
use std::time::{Duration, Instant};

use flyx_protocol::{AircraftId, ByeReason, FlightState};
use flyx_sync::Password;
use flyx_sync::session::Event;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

pub use address::{AddressError, parse_address};

/// Default UDP port for hosting.
pub const DEFAULT_PORT: u16 = 49700;

/// Longest a handshake may take once connected (key derivation included).
pub(crate) const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(20);
/// Time allowed for close frames to reach the peer when a session ends.
pub(crate) const CLOSE_FLUSH: Duration = Duration::from_millis(300);
/// Longest display name accepted from a peer, in characters.
const MAX_NAME_CHARS: usize = 32;

/// What this seat tells the other seat about itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalInfo {
    pub plugin_version: String,
    pub display_name: String,
    pub aircraft: AircraftId,
    /// Identity of this seat's sync definition for `aircraft`.
    pub definition: [u8; 32],
}

#[derive(Debug)]
pub enum NetCommand {
    /// Start listening. Replaces any current activity.
    Host {
        port: u16,
        password: Password,
        local: LocalInfo,
    },
    /// Connect and join. Replaces any current activity.
    Join {
        address: String,
        password: Password,
        local: LocalInfo,
    },
    /// End the current session or attempt, telling the peer why.
    Disconnect { reason: ByeReason },
    /// Authority: send one flight-state sample to the follower.
    SendFlightState(FlightState),
    /// Authority: tell the follower the simulator paused or resumed.
    SendPaused(bool),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetEvent {
    /// Feed into `flyx_sync::session::Session`.
    Session(Event),
    /// Follower: the authority paused or resumed.
    Paused(bool),
}

/// A flight-state sample with the moment it arrived, for jitter estimates.
#[derive(Debug, Clone, Copy)]
pub struct ReceivedState {
    pub state: FlightState,
    pub received_at: Instant,
}

/// Commands from the manager to the running host or join task.
#[derive(Debug)]
// Flight state is the hot path and must not allocate; the other variants
// are rare, so the size difference is accepted.
#[allow(clippy::large_enum_variant)]
pub(crate) enum ActivityCommand {
    FlightState(FlightState),
    Paused(bool),
    Disconnect(ByeReason),
}

#[derive(Debug, Clone)]
pub(crate) struct Outputs {
    events: std_mpsc::Sender<NetEvent>,
    samples: std_mpsc::Sender<ReceivedState>,
}

impl Outputs {
    pub(crate) fn session(&self, event: Event) {
        let _ = self.events.send(NetEvent::Session(event));
    }

    pub(crate) fn paused(&self, paused: bool) {
        let _ = self.events.send(NetEvent::Paused(paused));
    }

    pub(crate) fn sample(&self, state: FlightState) {
        let _ = self.samples.send(ReceivedState {
            state,
            received_at: Instant::now(),
        });
    }
}

/// The simulator thread's side of the network task. All methods are
/// non-blocking except [`NetHandle::shutdown`].
pub struct NetHandle {
    commands: mpsc::UnboundedSender<NetCommand>,
    events: std_mpsc::Receiver<NetEvent>,
    samples: std_mpsc::Receiver<ReceivedState>,
    finished: std_mpsc::Receiver<()>,
}

/// A cloneable, thread-safe way to send commands, e.g. from an emergency
/// teardown that may run on any thread.
#[derive(Debug, Clone)]
pub struct Commander(mpsc::UnboundedSender<NetCommand>);

impl Commander {
    pub fn send(&self, command: NetCommand) {
        let _ = self.0.send(command);
    }
}

impl NetHandle {
    pub fn send(&self, command: NetCommand) {
        let _ = self.commands.send(command);
    }

    pub fn commander(&self) -> Commander {
        Commander(self.commands.clone())
    }

    pub fn try_event(&self) -> Option<NetEvent> {
        self.events.try_recv().ok()
    }

    pub fn try_sample(&self) -> Option<ReceivedState> {
        self.samples.try_recv().ok()
    }

    /// Ends any session with `reason` and waits up to `timeout` for the
    /// goodbye to go out. Call before shutting down the runtime.
    pub fn shutdown(self, reason: ByeReason, timeout: Duration) {
        let _ = self.commands.send(NetCommand::Disconnect { reason });
        drop(self.commands);
        let _ = self.finished.recv_timeout(timeout);
    }
}

/// Starts the network task on `runtime`.
pub fn spawn(runtime: &tokio::runtime::Handle) -> NetHandle {
    let (commands_tx, commands_rx) = mpsc::unbounded_channel();
    let (events_tx, events_rx) = std_mpsc::channel();
    let (samples_tx, samples_rx) = std_mpsc::channel();
    let (finished_tx, finished_rx) = std_mpsc::channel();
    let outputs = Outputs {
        events: events_tx,
        samples: samples_tx,
    };
    runtime.spawn(async move {
        manager(commands_rx, outputs).await;
        let _ = finished_tx.send(());
    });
    NetHandle {
        commands: commands_tx,
        events: events_rx,
        samples: samples_rx,
        finished: finished_rx,
    }
}

struct Activity {
    commands: mpsc::UnboundedSender<ActivityCommand>,
    task: JoinHandle<()>,
}

async fn manager(mut commands: mpsc::UnboundedReceiver<NetCommand>, out: Outputs) {
    let mut activity: Option<Activity> = None;
    while let Some(command) = commands.recv().await {
        match command {
            NetCommand::Host {
                port,
                password,
                local,
            } => {
                stop(&mut activity, ByeReason::StoppedHosting).await;
                let (tx, rx) = mpsc::unbounded_channel();
                let task = tokio::spawn(host::run(port, password, local, rx, out.clone()));
                activity = Some(Activity { commands: tx, task });
            }
            NetCommand::Join {
                address,
                password,
                local,
            } => {
                stop(&mut activity, ByeReason::Left).await;
                let (tx, rx) = mpsc::unbounded_channel();
                let task = tokio::spawn(join::run(address, password, local, rx, out.clone()));
                activity = Some(Activity { commands: tx, task });
            }
            NetCommand::Disconnect { reason } => stop(&mut activity, reason).await,
            NetCommand::SendFlightState(state) => {
                if let Some(a) = &activity {
                    let _ = a.commands.send(ActivityCommand::FlightState(state));
                }
            }
            NetCommand::SendPaused(paused) => {
                if let Some(a) = &activity {
                    let _ = a.commands.send(ActivityCommand::Paused(paused));
                }
            }
        }
    }
    stop(&mut activity, ByeReason::PluginStopped).await;
}

async fn stop(activity: &mut Option<Activity>, reason: ByeReason) {
    if let Some(a) = activity.take() {
        let _ = a.commands.send(ActivityCommand::Disconnect(reason));
        let _ = tokio::time::timeout(Duration::from_secs(1), a.task).await;
    }
}

/// A display name received from a peer: trimmed, control characters
/// removed, at most 32 characters.
pub(crate) fn clean_name(name: &str) -> String {
    let cleaned: String = name
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_NAME_CHARS)
        .collect();
    if cleaned.is_empty() {
        "Pilot".to_owned()
    } else {
        cleaned
    }
}

#[cfg(test)]
mod tests;

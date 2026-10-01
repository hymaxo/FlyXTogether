//! The session state machine: what this seat is doing, driven by user
//! requests and network events. It owns no sockets and no simulator; it
//! returns [`Effect`]s for the plugin to carry out and a [`Notice`] to show.

use flyx_protocol::{AircraftId, ByeReason, PROTOCOL_VERSION, RejectReason, Seat};

use crate::aircraft;

/// Shown on both seats when their sync definitions differ.
const DEFINITION_MISMATCH: &str = "The aircraft files or profiles differ between the two seats. \
     Both pilots need the same aircraft version and the same FlyXTogether release.";

/// This seat's role in a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// The pilot flying: its simulator flies the aircraft.
    Authority,
    /// The pilot monitoring: follows the pilot flying's aircraft.
    Follower,
}

/// Who has the controls, from which control epoch on. The host starts as
/// the pilot flying; every handover increases the epoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Controls {
    pub epoch: u32,
    pub pilot_flying: Seat,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    Idle,
    /// Hosting was requested; the listener is starting.
    StartingHost {
        port: u16,
    },
    /// Listening for crew. `crew` is the connected follower, if any.
    Hosting {
        port: u16,
        addresses: Vec<String>,
        crew: Option<String>,
    },
    /// Connecting to a host and running the handshake.
    Joining {
        address: String,
    },
    /// Admitted to a host's session as the follower.
    Joined {
        address: String,
        host: String,
    },
}

impl State {
    /// This seat and the other seat's name while a session is connected.
    fn connected(&self) -> Option<(Seat, &str)> {
        match self {
            State::Hosting {
                crew: Some(name), ..
            } => Some((Seat::Host, name)),
            State::Joined { host, .. } => Some((Seat::Crew, host)),
            _ => None,
        }
    }
}

/// A message for the user about the latest change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notice {
    Info(String),
    Error(String),
}

/// What the plugin must do after an event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Start listening on this UDP port.
    StartHost {
        port: u16,
    },
    /// Connect to this address and join.
    Join {
        address: String,
    },
    /// Tell the peer (if any) why, then close connections and the listener.
    Disconnect {
        reason: ByeReason,
    },
    /// Pilot flying: start sending flight state, stamped with `epoch`.
    StartStreaming {
        epoch: u32,
    },
    StopStreaming,
    /// Pilot monitoring: take over the aircraft from the local flight model
    /// and follow samples of `epoch`. `after_handover` is set when this
    /// seat was flying until now, so the follower blends in gently.
    StartFollowing {
        epoch: u32,
        after_handover: bool,
    },
    /// Pilot monitoring: give the aircraft back to the local flight model.
    StopFollowing,
    /// Host: tell the crew who has the controls.
    SendControls {
        controls: Controls,
    },
    /// Crew: ask the host for the controls.
    SendTakeControls {
        seen_epoch: u32,
    },
}

/// Why hosting could not start or stopped working.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostFailure {
    PortUnavailable,
    Other(String),
}

/// Why a join attempt failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JoinFailure {
    /// No answer from the address within the timeout.
    Unreachable,
    /// The host answered with a rejection.
    Rejected(RejectReason),
    /// The host could not prove it knows the password.
    HostProofFailed,
    /// The connection or handshake failed for another reason.
    Other(String),
}

/// A join attempt the host refused, reported so the host can see it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    BadPassword,
    VersionMismatch {
        joiner_plugin_version: String,
        joiner_protocol_version: u16,
    },
    AircraftMismatch {
        joiner_aircraft: AircraftId,
        host_aircraft: AircraftId,
    },
    SessionFull,
    /// The joiner's sync definition differs from the host's.
    DefinitionMismatch,
}

/// Why the other seat left.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerGone {
    Said(ByeReason),
    /// No traffic within the timeout.
    Lost,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    // Requests from the user or the simulator.
    HostRequested {
        port: u16,
        aircraft: AircraftId,
    },
    JoinRequested {
        address: String,
        aircraft: AircraftId,
    },
    LeaveRequested,
    /// A different aircraft was loaded, or the current one reloaded.
    AircraftChanged,
    /// The plugin is being disabled.
    PluginStopping,

    // Host-side network events.
    Listening {
        addresses: Vec<String>,
    },
    HostFailed(HostFailure),
    CrewJoined {
        name: String,
    },
    CrewRefused(Refusal),
    CrewGone(PeerGone),

    // Joiner-side network events.
    Joined {
        host: String,
    },
    JoinFailed(JoinFailure),
    HostGone(PeerGone),

    // Handover.
    /// The user pressed Take controls (or its command).
    TakeControlsRequested,
    /// Host: the crew asks for the controls.
    TakeControlsReceived {
        seen_epoch: u32,
    },
    /// Crew: the host says who has the controls.
    ControlsReceived(Controls),
}

/// Result of handling one event.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    pub effects: Vec<Effect>,
    /// `Some` replaces the notice shown to the user.
    pub notice: Option<Notice>,
    /// Whether the previous notice should be cleared (when `notice` is None).
    pub clear_notice: bool,
}

#[derive(Debug)]
pub struct Session {
    state: State,
    /// Set while a session is connected.
    controls: Option<Controls>,
    plugin_version: String,
}

impl Session {
    pub fn new(plugin_version: impl Into<String>) -> Self {
        Self {
            state: State::Idle,
            controls: None,
            plugin_version: plugin_version.into(),
        }
    }

    pub fn state(&self) -> &State {
        &self.state
    }

    /// Who has the controls, while connected.
    pub fn controls(&self) -> Option<Controls> {
        self.controls
    }

    /// This seat's role while connected.
    pub fn role(&self) -> Option<Role> {
        let (me, _) = self.state.connected()?;
        let controls = self.controls?;
        Some(if controls.pilot_flying == me {
            Role::Authority
        } else {
            Role::Follower
        })
    }

    /// "You have the controls" or "<name> has the controls", while connected.
    pub fn controls_line(&self) -> Option<String> {
        let (_, other) = self.state.connected()?;
        Some(match self.role()? {
            Role::Authority => "You have the controls".to_owned(),
            Role::Follower => format!("{other} has the controls"),
        })
    }

    /// Whether Take controls does anything right now.
    pub fn can_take_controls(&self) -> bool {
        self.role() == Some(Role::Follower)
    }

    pub fn handle(&mut self, event: Event) -> Outcome {
        use Effect::*;
        match event {
            Event::TakeControlsRequested => return self.take_controls_requested(),
            Event::TakeControlsReceived { seen_epoch } => {
                return self.take_controls_received(seen_epoch);
            }
            Event::ControlsReceived(controls) => return self.controls_received(controls),
            _ => {}
        }
        let mut out = Outcome::default();
        let state = std::mem::replace(&mut self.state, State::Idle);
        self.state = match (state, event) {
            // Leaving the plugin ends everything.
            (state, Event::PluginStopping) => {
                out.effects.extend(self.stop_effects(&state));
                if state != State::Idle {
                    out.effects.push(Disconnect {
                        reason: ByeReason::PluginStopped,
                    });
                }
                State::Idle
            }

            // --- Idle ---
            (State::Idle, Event::HostRequested { port, aircraft }) => {
                if aircraft::is_loaded(&aircraft) {
                    out.clear_notice = true;
                    out.effects.push(StartHost { port });
                    State::StartingHost { port }
                } else {
                    out.notice = Some(Notice::Error("Load an aircraft before hosting.".into()));
                    State::Idle
                }
            }
            (State::Idle, Event::JoinRequested { address, aircraft }) => {
                if aircraft::is_loaded(&aircraft) {
                    out.clear_notice = true;
                    out.effects.push(Join {
                        address: address.clone(),
                    });
                    State::Joining { address }
                } else {
                    out.notice = Some(Notice::Error("Load an aircraft before joining.".into()));
                    State::Idle
                }
            }

            // --- Starting to host ---
            (State::StartingHost { port }, Event::Listening { addresses }) => State::Hosting {
                port,
                addresses,
                crew: None,
            },
            (State::StartingHost { port }, Event::HostFailed(failure))
            | (State::Hosting { port, .. }, Event::HostFailed(failure)) => {
                out.effects.extend(self.stop_for(Seat::Host));
                out.effects.push(Disconnect {
                    reason: ByeReason::StoppedHosting,
                });
                out.notice = Some(Notice::Error(match failure {
                    HostFailure::PortUnavailable => format!(
                        "Port {port} is already in use. Close the program using it or choose another port."
                    ),
                    HostFailure::Other(e) => format!("Hosting on port {port} failed: {e}"),
                }));
                State::Idle
            }

            // --- Hosting ---
            (
                State::Hosting {
                    port,
                    addresses,
                    crew: None,
                },
                Event::CrewJoined { name },
            ) => {
                out.clear_notice = true;
                let controls = Controls {
                    epoch: 0,
                    pilot_flying: Seat::Host,
                };
                self.controls = Some(controls);
                out.effects.push(StartStreaming { epoch: 0 });
                State::Hosting {
                    port,
                    addresses,
                    crew: Some(name),
                }
            }
            (state @ State::Hosting { .. }, Event::CrewRefused(refusal)) => {
                out.notice = Some(self.refusal_notice(&refusal));
                state
            }
            (
                State::Hosting {
                    port,
                    addresses,
                    crew: Some(name),
                },
                Event::CrewGone(gone),
            ) => {
                out.effects.extend(self.stop_for(Seat::Host));
                out.notice = Some(Notice::Info(match gone {
                    PeerGone::Lost => format!("Lost connection to {name}."),
                    PeerGone::Said(ByeReason::AircraftChanged) => {
                        format!("{name} changed aircraft and left the session.")
                    }
                    PeerGone::Said(ByeReason::PluginStopped | ByeReason::InternalError) => {
                        format!("{name}'s FlyXTogether stopped.")
                    }
                    PeerGone::Said(_) => format!("{name} left the session."),
                }));
                State::Hosting {
                    port,
                    addresses,
                    crew: None,
                }
            }

            // --- Joining ---
            (State::Joining { address }, Event::Joined { host }) => {
                out.clear_notice = true;
                self.controls = Some(Controls {
                    epoch: 0,
                    pilot_flying: Seat::Host,
                });
                out.effects.push(StartFollowing {
                    epoch: 0,
                    after_handover: false,
                });
                State::Joined { address, host }
            }
            (State::Joining { address }, Event::JoinFailed(failure)) => {
                out.notice = Some(Notice::Error(self.join_failure_text(&address, &failure)));
                State::Idle
            }

            // --- Joined ---
            (state @ State::Joined { .. }, Event::HostGone(gone)) => {
                out.effects.extend(self.stop_effects(&state));
                out.notice = Some(match gone {
                    PeerGone::Lost => Notice::Error("Lost connection to the host.".into()),
                    PeerGone::Said(ByeReason::AircraftChanged) => {
                        Notice::Info("Session ended: the host changed aircraft.".into())
                    }
                    PeerGone::Said(ByeReason::PluginStopped | ByeReason::InternalError) => {
                        Notice::Info("Session ended: the host's FlyXTogether stopped.".into())
                    }
                    PeerGone::Said(_) => Notice::Info("The host ended the session.".into()),
                });
                State::Idle
            }

            // --- Leaving or changing aircraft, from any active state ---
            (state, Event::LeaveRequested) if state != State::Idle => {
                out.clear_notice = true;
                out.effects.extend(self.stop_effects(&state));
                out.effects.push(Disconnect {
                    reason: if matches!(state, State::Hosting { .. } | State::StartingHost { .. }) {
                        ByeReason::StoppedHosting
                    } else {
                        ByeReason::Left
                    },
                });
                State::Idle
            }
            (state, Event::AircraftChanged) if state != State::Idle => {
                out.effects.extend(self.stop_effects(&state));
                out.effects.push(Disconnect {
                    reason: ByeReason::AircraftChanged,
                });
                out.notice = Some(Notice::Info(
                    "Session ended because you changed aircraft.".into(),
                ));
                State::Idle
            }

            // Anything else is stale or irrelevant in the current state.
            (state, _) => state,
        };
        if self.state.connected().is_none() {
            self.controls = None;
        }
        out
    }

    /// Stops streaming or following for a connected state being left: the
    /// pilot flying keeps flying, the pilot monitoring gets its aircraft back.
    fn stop_effects(&self, state: &State) -> Vec<Effect> {
        match state.connected() {
            Some((me, _)) => self.stop_for(me),
            None => vec![],
        }
    }

    fn stop_for(&self, me: Seat) -> Vec<Effect> {
        match self.controls {
            Some(c) if c.pilot_flying == me => vec![Effect::StopStreaming],
            Some(_) => vec![Effect::StopFollowing],
            None => vec![],
        }
    }

    fn take_controls_requested(&mut self) -> Outcome {
        let mut out = Outcome::default();
        let (Some((me, _)), Some(controls)) = (self.state.connected(), self.controls) else {
            return out;
        };
        if controls.pilot_flying == me {
            return out;
        }
        match me {
            Seat::Host => {
                let controls = Controls {
                    epoch: controls.epoch + 1,
                    pilot_flying: Seat::Host,
                };
                self.controls = Some(controls);
                out.effects = vec![
                    Effect::StopFollowing,
                    Effect::StartStreaming {
                        epoch: controls.epoch,
                    },
                    Effect::SendControls { controls },
                ];
                out.notice = Some(Notice::Info("You took the controls.".into()));
            }
            Seat::Crew => out.effects.push(Effect::SendTakeControls {
                seen_epoch: controls.epoch,
            }),
        }
        out
    }

    fn take_controls_received(&mut self, seen_epoch: u32) -> Outcome {
        let mut out = Outcome::default();
        let (Some((Seat::Host, crew)), Some(controls)) = (self.state.connected(), self.controls)
        else {
            return out;
        };
        // A request made before the latest handover is stale: the host
        // took the controls in the meantime, and keeps them.
        if seen_epoch != controls.epoch || controls.pilot_flying == Seat::Crew {
            return out;
        }
        let controls = Controls {
            epoch: controls.epoch + 1,
            pilot_flying: Seat::Crew,
        };
        out.notice = Some(Notice::Info(format!("{crew} took the controls.")));
        self.controls = Some(controls);
        out.effects = vec![
            Effect::StopStreaming,
            Effect::StartFollowing {
                epoch: controls.epoch,
                after_handover: true,
            },
            Effect::SendControls { controls },
        ];
        out
    }

    fn controls_received(&mut self, new: Controls) -> Outcome {
        let mut out = Outcome::default();
        let (Some((Seat::Crew, host)), Some(old)) = (self.state.connected(), self.controls) else {
            return out;
        };
        if new.epoch <= old.epoch {
            return out;
        }
        let host = host.to_owned();
        self.controls = Some(new);
        match (old.pilot_flying, new.pilot_flying) {
            (Seat::Host, Seat::Crew) => {
                out.effects = vec![
                    Effect::StopFollowing,
                    Effect::StartStreaming { epoch: new.epoch },
                ];
                out.notice = Some(Notice::Info("You took the controls.".into()));
            }
            (Seat::Crew, Seat::Host) => {
                out.effects = vec![
                    Effect::StopStreaming,
                    Effect::StartFollowing {
                        epoch: new.epoch,
                        after_handover: true,
                    },
                ];
                out.notice = Some(Notice::Info(format!("{host} took the controls.")));
            }
            // Same pilot flying in a newer epoch: only the epoch changes.
            (_, Seat::Crew) => out
                .effects
                .push(Effect::StartStreaming { epoch: new.epoch }),
            (_, Seat::Host) => out.effects.push(Effect::StartFollowing {
                epoch: new.epoch,
                after_handover: false,
            }),
        }
        out
    }

    fn refusal_notice(&self, refusal: &Refusal) -> Notice {
        match refusal {
            Refusal::BadPassword => {
                Notice::Info("Someone tried to join with a wrong password.".into())
            }
            Refusal::VersionMismatch {
                joiner_plugin_version,
                joiner_protocol_version,
            } => Notice::Error(format!(
                "A crew member tried to join with FlyXTogether {joiner_plugin_version} \
                 (protocol {joiner_protocol_version}), but you have {} (protocol {}). \
                 The versions are incompatible.",
                self.plugin_version, PROTOCOL_VERSION
            )),
            Refusal::AircraftMismatch {
                joiner_aircraft,
                host_aircraft,
            } => Notice::Error(format!(
                "A crew member tried to join with the {}, but you are flying the {}.",
                aircraft::display_name(joiner_aircraft),
                aircraft::display_name(host_aircraft)
            )),
            Refusal::SessionFull => {
                Notice::Info("Someone tried to join, but the session is full.".into())
            }
            Refusal::DefinitionMismatch => Notice::Error(DEFINITION_MISMATCH.into()),
        }
    }

    fn join_failure_text(&self, address: &str, failure: &JoinFailure) -> String {
        match failure {
            JoinFailure::Unreachable => format!(
                "Could not reach the host at {address}. Check the address, and ask the host \
                 to check that their UDP port is forwarded to their computer."
            ),
            JoinFailure::HostProofFailed => "The host could not prove it knows the session \
                 password. You may not be talking to the real host."
                .into(),
            JoinFailure::Other(e) => format!("Joining {address} failed: {e}"),
            JoinFailure::Rejected(reason) => match reason {
                RejectReason::BadPassword => "Wrong password".into(),
                RejectReason::TooManyAttempts => {
                    "Too many attempts. Wait a few seconds and try again.".into()
                }
                RejectReason::VersionMismatch {
                    host_protocol_version,
                    host_plugin_version,
                } => format!(
                    "The FlyXTogether versions are incompatible: the host has \
                     {host_plugin_version} (protocol {host_protocol_version}), you have {} \
                     (protocol {}).",
                    self.plugin_version, PROTOCOL_VERSION
                ),
                RejectReason::AircraftMismatch { host_aircraft } => format!(
                    "The host is flying the {}. Load the same aircraft and join again.",
                    aircraft::display_name(host_aircraft)
                ),
                RejectReason::UnsupportedAircraft => {
                    "The host does not accept your aircraft.".into()
                }
                RejectReason::DefinitionMismatch => DEFINITION_MISMATCH.into(),
                RejectReason::SessionFull => "Session full".into(),
                RejectReason::NotHosting => "The host is not accepting crew right now.".into(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c172() -> AircraftId {
        AircraftId {
            folder: "Cessna 172 SP".into(),
            acf: "Cessna_172SP.acf".into(),
            name: "Cessna 172 SP".into(),
        }
    }

    fn seaplane() -> AircraftId {
        AircraftId {
            folder: "Cessna 172 SP".into(),
            acf: "Cessna_172SP_seaplane.acf".into(),
            name: "Cessna 172 SP Seaplane".into(),
        }
    }

    fn no_aircraft() -> AircraftId {
        AircraftId {
            folder: String::new(),
            acf: String::new(),
            name: String::new(),
        }
    }

    fn session() -> Session {
        Session::new("0.2.0")
    }

    fn hosting_waiting() -> Session {
        let mut s = session();
        s.handle(Event::HostRequested {
            port: 49700,
            aircraft: c172(),
        });
        s.handle(Event::Listening {
            addresses: vec!["192.168.1.20:49700".into()],
        });
        s
    }

    fn hosting_with_crew() -> Session {
        let mut s = hosting_waiting();
        s.handle(Event::CrewJoined {
            name: "Alex".into(),
        });
        s
    }

    fn joined() -> Session {
        let mut s = session();
        s.handle(Event::JoinRequested {
            address: "203.0.113.7:49700".into(),
            aircraft: c172(),
        });
        s.handle(Event::Joined { host: "Sam".into() });
        s
    }

    fn notice_text(out: &Outcome) -> &str {
        match out.notice.as_ref().expect("a notice") {
            Notice::Info(t) | Notice::Error(t) => t,
        }
    }

    // Scenario: Start hosting.
    #[test]
    fn start_hosting_then_waiting_for_crew() {
        let mut s = session();
        let out = s.handle(Event::HostRequested {
            port: 49700,
            aircraft: c172(),
        });
        assert_eq!(out.effects, vec![Effect::StartHost { port: 49700 }]);
        assert_eq!(s.state(), &State::StartingHost { port: 49700 });
        s.handle(Event::Listening {
            addresses: vec!["192.168.1.20:49700".into()],
        });
        assert_eq!(
            s.state(),
            &State::Hosting {
                port: 49700,
                addresses: vec!["192.168.1.20:49700".into()],
                crew: None
            }
        );
        assert_eq!(s.role(), None);
    }

    // Scenario: Port unavailable.
    #[test]
    fn port_unavailable_names_port() {
        let mut s = session();
        s.handle(Event::HostRequested {
            port: 49700,
            aircraft: c172(),
        });
        let out = s.handle(Event::HostFailed(HostFailure::PortUnavailable));
        assert_eq!(s.state(), &State::Idle);
        assert!(matches!(out.notice, Some(Notice::Error(ref t)) if t.contains("49700")));
    }

    // Every aircraft is supported; only having none loaded stops hosting.
    #[test]
    fn hosting_needs_an_aircraft_but_any_will_do() {
        let mut s = session();
        let out = s.handle(Event::HostRequested {
            port: 49700,
            aircraft: no_aircraft(),
        });
        assert_eq!(s.state(), &State::Idle);
        assert!(out.effects.is_empty());
        assert_eq!(notice_text(&out), "Load an aircraft before hosting.");

        let baron = AircraftId {
            folder: "Beechcraft Baron 58".into(),
            acf: "Baron_58.acf".into(),
            name: "Baron 58".into(),
        };
        let mut s = session();
        s.handle(Event::HostRequested {
            port: 49700,
            aircraft: baron,
        });
        assert_eq!(s.state(), &State::StartingHost { port: 49700 });
    }

    // Scenario: Successful join (both sides).
    #[test]
    fn join_and_crew_joined_show_names_and_roles() {
        let host = hosting_with_crew();
        assert_eq!(host.role(), Some(Role::Authority));
        assert!(matches!(host.state(), State::Hosting { crew: Some(n), .. } if n == "Alex"));

        let mut s = session();
        let out = s.handle(Event::JoinRequested {
            address: "203.0.113.7:49700".into(),
            aircraft: c172(),
        });
        assert_eq!(
            out.effects,
            vec![Effect::Join {
                address: "203.0.113.7:49700".into()
            }]
        );
        let out = s.handle(Event::Joined { host: "Sam".into() });
        assert_eq!(
            out.effects,
            vec![Effect::StartFollowing {
                epoch: 0,
                after_handover: false
            }]
        );
        assert_eq!(s.role(), Some(Role::Follower));
    }

    #[test]
    fn crew_joining_starts_streaming() {
        let mut s = hosting_waiting();
        let out = s.handle(Event::CrewJoined {
            name: "Alex".into(),
        });
        assert_eq!(out.effects, vec![Effect::StartStreaming { epoch: 0 }]);
    }

    fn controls(epoch: u32, pilot_flying: Seat) -> Controls {
        Controls {
            epoch,
            pilot_flying,
        }
    }

    // Scenario: After joining (control-handover).
    #[test]
    fn host_has_the_controls_after_joining() {
        let host = hosting_with_crew();
        assert_eq!(
            host.controls_line().as_deref(),
            Some("You have the controls")
        );
        assert!(!host.can_take_controls());
        let crew = joined();
        assert_eq!(
            crew.controls_line().as_deref(),
            Some("Sam has the controls")
        );
        assert!(crew.can_take_controls());
        assert_eq!(session().controls_line(), None);
    }

    // Scenario: Crew takes the controls.
    #[test]
    fn crew_takes_the_controls() {
        let mut crew = joined();
        let out = crew.handle(Event::TakeControlsRequested);
        assert_eq!(
            out.effects,
            vec![Effect::SendTakeControls { seen_epoch: 0 }]
        );
        assert_eq!(
            crew.role(),
            Some(Role::Follower),
            "nothing changes until the host agrees"
        );

        let mut host = hosting_with_crew();
        let out = host.handle(Event::TakeControlsReceived { seen_epoch: 0 });
        assert_eq!(
            out.effects,
            vec![
                Effect::StopStreaming,
                Effect::StartFollowing {
                    epoch: 1,
                    after_handover: true
                },
                Effect::SendControls {
                    controls: controls(1, Seat::Crew)
                },
            ]
        );
        assert_eq!(notice_text(&out), "Alex took the controls.");
        assert_eq!(host.role(), Some(Role::Follower));
        assert_eq!(
            host.controls_line().as_deref(),
            Some("Alex has the controls")
        );

        let out = crew.handle(Event::ControlsReceived(controls(1, Seat::Crew)));
        assert_eq!(
            out.effects,
            vec![Effect::StopFollowing, Effect::StartStreaming { epoch: 1 }]
        );
        assert_eq!(notice_text(&out), "You took the controls.");
        assert_eq!(crew.role(), Some(Role::Authority));
        assert_eq!(
            crew.controls_line().as_deref(),
            Some("You have the controls")
        );
    }

    #[test]
    fn host_takes_the_controls_back() {
        let mut host = hosting_with_crew();
        host.handle(Event::TakeControlsReceived { seen_epoch: 0 });
        let out = host.handle(Event::TakeControlsRequested);
        assert_eq!(
            out.effects,
            vec![
                Effect::StopFollowing,
                Effect::StartStreaming { epoch: 2 },
                Effect::SendControls {
                    controls: controls(2, Seat::Host)
                },
            ]
        );
        assert_eq!(notice_text(&out), "You took the controls.");

        let mut crew = joined();
        crew.handle(Event::ControlsReceived(controls(1, Seat::Crew)));
        let out = crew.handle(Event::ControlsReceived(controls(2, Seat::Host)));
        assert_eq!(
            out.effects,
            vec![
                Effect::StopStreaming,
                Effect::StartFollowing {
                    epoch: 2,
                    after_handover: true
                }
            ]
        );
        assert_eq!(notice_text(&out), "Sam took the controls.");
        assert_eq!(crew.role(), Some(Role::Follower));
    }

    // Scenario: Both press at once. The crew asks for the controls (epoch
    // 0) while the host, which already flies, presses too; then, with the
    // crew flying, both press at once again.
    #[test]
    fn both_pressing_at_once_ends_with_one_pilot_flying() {
        let mut host = hosting_with_crew();
        let mut crew = joined();

        // Round 1: the host already flies, so its press does nothing and
        // the crew's request (made at epoch 0) wins.
        let request = crew.handle(Event::TakeControlsRequested);
        assert_eq!(
            request.effects,
            vec![Effect::SendTakeControls { seen_epoch: 0 }]
        );
        assert_eq!(
            host.handle(Event::TakeControlsRequested),
            Outcome::default()
        );
        let grant = host.handle(Event::TakeControlsReceived { seen_epoch: 0 });
        let Some(Effect::SendControls { controls: sent }) = grant.effects.last().cloned() else {
            panic!("no SendControls in {grant:?}");
        };
        crew.handle(Event::ControlsReceived(sent));
        assert_eq!(crew.role(), Some(Role::Authority));
        assert_eq!(host.controls(), crew.controls());

        // Round 2: the crew flies; the host presses, and so does the crew
        // (no-op, it flies). The host's handover reaches the crew.
        let host_out = host.handle(Event::TakeControlsRequested);
        assert!(crew.handle(Event::TakeControlsRequested).effects.is_empty());
        let Some(Effect::SendControls { controls: sent }) = host_out.effects.last().cloned() else {
            panic!("no SendControls in {host_out:?}");
        };
        crew.handle(Event::ControlsReceived(sent));
        assert_eq!(host.role(), Some(Role::Authority));
        assert_eq!(crew.role(), Some(Role::Follower));
        assert_eq!(host.controls(), crew.controls());

        // Round 3: the crew asks at epoch 2 while the host already moved on.
        host.handle(Event::TakeControlsReceived { seen_epoch: 2 });
        host.handle(Event::TakeControlsRequested);
        let late = host.handle(Event::TakeControlsReceived { seen_epoch: 2 });
        assert_eq!(
            late,
            Outcome::default(),
            "a request from an old epoch is dropped"
        );
        assert_eq!(host.role(), Some(Role::Authority));
    }

    #[test]
    fn stale_requests_are_dropped() {
        let mut host = hosting_with_crew();
        host.handle(Event::TakeControlsReceived { seen_epoch: 0 });
        host.handle(Event::TakeControlsRequested);
        // Requests the crew made at epoch 0 or 1 arrive late.
        for seen_epoch in [0, 1] {
            let out = host.handle(Event::TakeControlsReceived { seen_epoch });
            assert_eq!(out, Outcome::default());
        }
        assert_eq!(host.controls(), Some(controls(2, Seat::Host)));
        // An old Controls message changes nothing on the crew either.
        let mut crew = joined();
        crew.handle(Event::ControlsReceived(controls(2, Seat::Host)));
        let out = crew.handle(Event::ControlsReceived(controls(1, Seat::Crew)));
        assert_eq!(out, Outcome::default());
        assert_eq!(crew.role(), Some(Role::Follower));
    }

    // Scenario: Pilot flying presses Take controls.
    #[test]
    fn taking_the_controls_while_flying_does_nothing() {
        let mut host = hosting_with_crew();
        assert_eq!(
            host.handle(Event::TakeControlsRequested),
            Outcome::default()
        );
        let mut crew = joined();
        crew.handle(Event::ControlsReceived(controls(1, Seat::Crew)));
        assert_eq!(
            crew.handle(Event::TakeControlsRequested),
            Outcome::default()
        );
    }

    #[test]
    fn taking_the_controls_needs_a_connected_session() {
        for mut s in [session(), hosting_waiting()] {
            assert!(!s.can_take_controls());
            assert_eq!(s.handle(Event::TakeControlsRequested), Outcome::default());
            assert_eq!(
                s.handle(Event::TakeControlsReceived { seen_epoch: 0 }),
                Outcome::default()
            );
        }
    }

    // Scenario: Crew member leaves while flying.
    #[test]
    fn crew_leaving_while_flying_hands_the_host_its_aircraft_back() {
        let mut host = hosting_with_crew();
        host.handle(Event::TakeControlsReceived { seen_epoch: 0 });
        let out = host.handle(Event::CrewGone(PeerGone::Said(ByeReason::Left)));
        assert_eq!(out.effects, vec![Effect::StopFollowing]);
        assert!(matches!(host.state(), State::Hosting { crew: None, .. }));
        assert_eq!(host.controls(), None);
        // The next crew starts with the host flying again.
        let out = host.handle(Event::CrewJoined { name: "Kim".into() });
        assert_eq!(out.effects, vec![Effect::StartStreaming { epoch: 0 }]);
    }

    #[test]
    fn crew_flying_keeps_flying_when_the_host_goes() {
        for gone in [PeerGone::Lost, PeerGone::Said(ByeReason::StoppedHosting)] {
            let mut crew = joined();
            crew.handle(Event::ControlsReceived(controls(1, Seat::Crew)));
            let out = crew.handle(Event::HostGone(gone));
            assert_eq!(out.effects, vec![Effect::StopStreaming]);
            assert_eq!(crew.state(), &State::Idle);
            assert_eq!(crew.controls(), None);
        }
    }

    #[test]
    fn leaving_or_changing_aircraft_stops_by_role() {
        let mut crew = joined();
        crew.handle(Event::ControlsReceived(controls(1, Seat::Crew)));
        let out = crew.handle(Event::LeaveRequested);
        assert_eq!(out.effects[0], Effect::StopStreaming);
        let mut host = hosting_with_crew();
        host.handle(Event::TakeControlsReceived { seen_epoch: 0 });
        let out = host.handle(Event::AircraftChanged);
        assert_eq!(out.effects[0], Effect::StopFollowing);
        let mut host = hosting_with_crew();
        host.handle(Event::TakeControlsReceived { seen_epoch: 0 });
        let out = host.handle(Event::HostFailed(HostFailure::Other("x".into())));
        assert_eq!(out.effects[0], Effect::StopFollowing);
    }

    // Scenario: Host unreachable.
    #[test]
    fn unreachable_host_suggests_port_forwarding() {
        let mut s = session();
        s.handle(Event::JoinRequested {
            address: "203.0.113.7".into(),
            aircraft: c172(),
        });
        let out = s.handle(Event::JoinFailed(JoinFailure::Unreachable));
        assert_eq!(s.state(), &State::Idle);
        let text = notice_text(&out);
        assert!(text.contains("Could not reach the host at 203.0.113.7"));
        assert!(text.contains("forwarded"));
    }

    // Scenario: Wrong password (both sides).
    #[test]
    fn wrong_password_joiner_sees_it_host_keeps_waiting() {
        let mut j = session();
        j.handle(Event::JoinRequested {
            address: "h".into(),
            aircraft: c172(),
        });
        let out = j.handle(Event::JoinFailed(JoinFailure::Rejected(
            RejectReason::BadPassword,
        )));
        assert_eq!(out.notice, Some(Notice::Error("Wrong password".into())));

        let mut h = hosting_waiting();
        let before = h.state().clone();
        h.handle(Event::CrewRefused(Refusal::BadPassword));
        assert_eq!(h.state(), &before);
    }

    // Scenario: Protocol version mismatch (both windows name both versions).
    #[test]
    fn version_mismatch_names_both_versions_on_both_sides() {
        let mut j = session();
        j.handle(Event::JoinRequested {
            address: "h".into(),
            aircraft: c172(),
        });
        let out = j.handle(Event::JoinFailed(JoinFailure::Rejected(
            RejectReason::VersionMismatch {
                host_protocol_version: 3,
                host_plugin_version: "0.3.0".into(),
            },
        )));
        let text = notice_text(&out);
        assert!(text.contains("0.3.0") && text.contains("0.2.0"), "{text}");

        let mut h = hosting_waiting();
        let out = h.handle(Event::CrewRefused(Refusal::VersionMismatch {
            joiner_plugin_version: "0.3.0".into(),
            joiner_protocol_version: 3,
        }));
        let text = notice_text(&out);
        assert!(text.contains("0.3.0") && text.contains("0.2.0"), "{text}");
    }

    // Scenario: Different aircraft (both windows name the host's aircraft).
    #[test]
    fn aircraft_mismatch_names_host_aircraft_on_both_sides() {
        let mut j = session();
        j.handle(Event::JoinRequested {
            address: "h".into(),
            aircraft: seaplane(),
        });
        let out = j.handle(Event::JoinFailed(JoinFailure::Rejected(
            RejectReason::AircraftMismatch {
                host_aircraft: c172(),
            },
        )));
        assert!(notice_text(&out).contains("The host is flying the Cessna 172 SP."));

        let mut h = hosting_waiting();
        let out = h.handle(Event::CrewRefused(Refusal::AircraftMismatch {
            joiner_aircraft: seaplane(),
            host_aircraft: c172(),
        }));
        assert!(notice_text(&out).contains("you are flying the Cessna 172 SP"));
    }

    #[test]
    fn joining_without_an_aircraft_is_refused_locally() {
        let mut s = session();
        let out = s.handle(Event::JoinRequested {
            address: "h".into(),
            aircraft: no_aircraft(),
        });
        assert!(out.effects.is_empty());
        assert_eq!(s.state(), &State::Idle);
        assert_eq!(notice_text(&out), "Load an aircraft before joining.");
    }

    // Scenario: Third seat refused.
    #[test]
    fn third_seat_refused_existing_session_unaffected() {
        let mut j = session();
        j.handle(Event::JoinRequested {
            address: "h".into(),
            aircraft: c172(),
        });
        let out = j.handle(Event::JoinFailed(JoinFailure::Rejected(
            RejectReason::SessionFull,
        )));
        assert_eq!(out.notice, Some(Notice::Error("Session full".into())));

        let mut h = hosting_with_crew();
        let before = h.state().clone();
        let out = h.handle(Event::CrewRefused(Refusal::SessionFull));
        assert_eq!(h.state(), &before);
        assert!(out.effects.is_empty());
    }

    // Scenario: Follower leaves.
    #[test]
    fn follower_leaving_returns_host_to_waiting() {
        let mut h = hosting_with_crew();
        let out = h.handle(Event::CrewGone(PeerGone::Said(ByeReason::Left)));
        assert_eq!(out.effects, vec![Effect::StopStreaming]);
        assert_eq!(
            out.notice,
            Some(Notice::Info("Alex left the session.".into()))
        );
        assert!(matches!(h.state(), State::Hosting { crew: None, .. }));

        let mut f = joined();
        let out = f.handle(Event::LeaveRequested);
        assert_eq!(
            out.effects,
            vec![
                Effect::StopFollowing,
                Effect::Disconnect {
                    reason: ByeReason::Left
                }
            ]
        );
        assert_eq!(f.state(), &State::Idle);
    }

    // Scenario: Authority lost.
    #[test]
    fn authority_lost_releases_follower() {
        let mut f = joined();
        let out = f.handle(Event::HostGone(PeerGone::Lost));
        assert_eq!(out.effects, vec![Effect::StopFollowing]);
        assert_eq!(
            out.notice,
            Some(Notice::Error("Lost connection to the host.".into()))
        );
        assert_eq!(f.state(), &State::Idle);
    }

    // Scenario: Host stops hosting.
    #[test]
    fn host_stopping_notifies_follower() {
        let mut h = hosting_with_crew();
        let out = h.handle(Event::LeaveRequested);
        assert_eq!(
            out.effects,
            vec![
                Effect::StopStreaming,
                Effect::Disconnect {
                    reason: ByeReason::StoppedHosting
                }
            ]
        );
        assert_eq!(h.state(), &State::Idle);

        let mut f = joined();
        let out = f.handle(Event::HostGone(PeerGone::Said(ByeReason::StoppedHosting)));
        assert_eq!(out.effects, vec![Effect::StopFollowing]);
        assert_eq!(
            out.notice,
            Some(Notice::Info("The host ended the session.".into()))
        );
    }

    // Scenario: Host reloads aircraft (both sessions end, both explain why).
    #[test]
    fn aircraft_change_ends_session_on_both_sides() {
        let mut h = hosting_with_crew();
        let out = h.handle(Event::AircraftChanged);
        assert_eq!(
            out.effects,
            vec![
                Effect::StopStreaming,
                Effect::Disconnect {
                    reason: ByeReason::AircraftChanged
                }
            ]
        );
        assert!(notice_text(&out).contains("changed aircraft"));
        assert_eq!(h.state(), &State::Idle);

        let mut f = joined();
        let out = f.handle(Event::HostGone(PeerGone::Said(ByeReason::AircraftChanged)));
        assert!(notice_text(&out).contains("changed aircraft"));
        assert_eq!(f.state(), &State::Idle);
    }

    #[test]
    fn aircraft_change_while_idle_does_nothing() {
        let mut s = session();
        let out = s.handle(Event::AircraftChanged);
        assert_eq!(out, Outcome::default());
    }

    #[test]
    fn leave_while_joining_cancels() {
        let mut s = session();
        s.handle(Event::JoinRequested {
            address: "h".into(),
            aircraft: c172(),
        });
        let out = s.handle(Event::LeaveRequested);
        assert_eq!(
            out.effects,
            vec![Effect::Disconnect {
                reason: ByeReason::Left
            }]
        );
        assert_eq!(s.state(), &State::Idle);
    }

    #[test]
    fn plugin_stopping_ends_active_session() {
        let mut f = joined();
        let out = f.handle(Event::PluginStopping);
        assert_eq!(
            out.effects,
            vec![
                Effect::StopFollowing,
                Effect::Disconnect {
                    reason: ByeReason::PluginStopped
                }
            ]
        );
        let mut idle = session();
        assert!(idle.handle(Event::PluginStopping).effects.is_empty());
    }

    #[test]
    fn stale_network_events_are_ignored() {
        let mut s = session();
        assert_eq!(
            s.handle(Event::CrewJoined { name: "X".into() }),
            Outcome::default()
        );
        assert_eq!(
            s.handle(Event::HostGone(PeerGone::Lost)),
            Outcome::default()
        );
        let mut h = hosting_waiting();
        let before = h.state().clone();
        h.handle(Event::Joined { host: "Y".into() });
        assert_eq!(h.state(), &before);
    }

    #[test]
    fn crew_lost_and_crew_plugin_stopped_messages() {
        let mut h = hosting_with_crew();
        let out = h.handle(Event::CrewGone(PeerGone::Lost));
        assert_eq!(
            out.notice,
            Some(Notice::Info("Lost connection to Alex.".into()))
        );
        let mut h = hosting_with_crew();
        let out = h.handle(Event::CrewGone(PeerGone::Said(ByeReason::AircraftChanged)));
        assert!(notice_text(&out).contains("changed aircraft"));
    }

    /// Every notice the window can show is listed word for word in
    /// docs/hosting.md, with the example values used there.
    #[test]
    fn hosting_doc_lists_every_notice() {
        let doc = include_str!("../../../docs/hosting.md");
        let mut texts = Vec::new();
        let mut collect = |out: Outcome| {
            if let Some(Notice::Info(t) | Notice::Error(t)) = out.notice {
                texts.push(t);
            }
        };
        let address = "203.0.113.7:49700".to_string();
        let joining = || {
            let mut s = session();
            s.handle(Event::JoinRequested {
                address: "203.0.113.7:49700".into(),
                aircraft: c172(),
            });
            s
        };

        // Hosting.
        let mut s = session();
        s.handle(Event::HostRequested {
            port: 49700,
            aircraft: c172(),
        });
        collect(s.handle(Event::HostFailed(HostFailure::PortUnavailable)));
        collect(session().handle(Event::HostRequested {
            port: 49700,
            aircraft: no_aircraft(),
        }));
        for refusal in [
            Refusal::BadPassword,
            Refusal::VersionMismatch {
                joiner_plugin_version: "0.1.0".into(),
                joiner_protocol_version: 1,
            },
            Refusal::AircraftMismatch {
                joiner_aircraft: seaplane(),
                host_aircraft: c172(),
            },
            Refusal::SessionFull,
            Refusal::DefinitionMismatch,
        ] {
            collect(hosting_waiting().handle(Event::CrewRefused(refusal)));
        }
        for gone in [
            PeerGone::Said(ByeReason::Left),
            PeerGone::Lost,
            PeerGone::Said(ByeReason::AircraftChanged),
            PeerGone::Said(ByeReason::PluginStopped),
        ] {
            collect(hosting_with_crew().handle(Event::CrewGone(gone)));
        }

        // Joining.
        collect(session().handle(Event::JoinRequested {
            address: address.clone(),
            aircraft: no_aircraft(),
        }));
        let failures = [
            JoinFailure::Unreachable,
            JoinFailure::Rejected(RejectReason::BadPassword),
            JoinFailure::Rejected(RejectReason::TooManyAttempts),
            JoinFailure::Rejected(RejectReason::VersionMismatch {
                host_protocol_version: 1,
                host_plugin_version: "0.1.0".into(),
            }),
            JoinFailure::Rejected(RejectReason::AircraftMismatch {
                host_aircraft: c172(),
            }),
            JoinFailure::Rejected(RejectReason::UnsupportedAircraft),
            JoinFailure::Rejected(RejectReason::SessionFull),
            JoinFailure::Rejected(RejectReason::NotHosting),
            JoinFailure::Rejected(RejectReason::DefinitionMismatch),
            JoinFailure::HostProofFailed,
            JoinFailure::Other("the host did not finish the handshake".into()),
        ];
        for failure in failures {
            collect(joining().handle(Event::JoinFailed(failure)));
        }

        // Session end.
        for gone in [
            PeerGone::Said(ByeReason::StoppedHosting),
            PeerGone::Lost,
            PeerGone::Said(ByeReason::AircraftChanged),
            PeerGone::Said(ByeReason::PluginStopped),
        ] {
            collect(joined().handle(Event::HostGone(gone)));
        }
        collect(joined().handle(Event::AircraftChanged));

        // Taking the controls.
        let mut host = hosting_with_crew();
        collect(host.handle(Event::TakeControlsReceived { seen_epoch: 0 }));
        collect(host.handle(Event::TakeControlsRequested));
        let mut crew = joined();
        collect(crew.handle(Event::ControlsReceived(Controls {
            epoch: 1,
            pilot_flying: Seat::Crew,
        })));
        collect(crew.handle(Event::ControlsReceived(Controls {
            epoch: 2,
            pilot_flying: Seat::Host,
        })));
        texts.extend(hosting_with_crew().controls_line());
        texts.extend(joined().controls_line());

        assert_eq!(texts.len(), 34);
        for text in texts {
            assert!(doc.contains(&text), "docs/hosting.md is missing: {text}");
        }
    }

    #[test]
    fn listener_failure_while_hosting_with_crew_stops_everything() {
        let mut h = hosting_with_crew();
        let out = h.handle(Event::HostFailed(HostFailure::Other(
            "socket closed".into(),
        )));
        assert!(out.effects.contains(&Effect::StopStreaming));
        assert_eq!(h.state(), &State::Idle);
    }
}

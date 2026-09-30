//! The session state machine: what this seat is doing, driven by user
//! requests and network events. It owns no sockets and no simulator; it
//! returns [`Effect`]s for the plugin to carry out and a [`Notice`] to show.

use flyx_protocol::{AircraftId, ByeReason, PROTOCOL_VERSION, RejectReason};

use crate::aircraft;

/// This seat's role in a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Hosts the session; its simulator flies the aircraft.
    Authority,
    /// Joined the session; follows the authority's aircraft.
    Follower,
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
    /// This seat's role while a session is active.
    pub fn role(&self) -> Option<Role> {
        match self {
            State::Hosting { crew: Some(_), .. } => Some(Role::Authority),
            State::Joined { .. } => Some(Role::Follower),
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
    /// Authority: start sending flight state to the follower.
    StartStreaming,
    StopStreaming,
    /// Follower: take over the aircraft from the local flight model.
    StartFollowing,
    /// Follower: give the aircraft back to the local flight model.
    StopFollowing,
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
    UnsupportedAircraft {
        joiner_aircraft: AircraftId,
    },
    SessionFull,
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
    plugin_version: String,
}

impl Session {
    pub fn new(plugin_version: impl Into<String>) -> Self {
        Self {
            state: State::Idle,
            plugin_version: plugin_version.into(),
        }
    }

    pub fn state(&self) -> &State {
        &self.state
    }

    pub fn handle(&mut self, event: Event) -> Outcome {
        use Effect::*;
        let mut out = Outcome::default();
        let state = std::mem::replace(&mut self.state, State::Idle);
        self.state = match (state, event) {
            // Leaving the plugin ends everything.
            (state, Event::PluginStopping) => {
                out.effects.extend(stop_effects(&state));
                if state != State::Idle {
                    out.effects.push(Disconnect {
                        reason: ByeReason::PluginStopped,
                    });
                }
                State::Idle
            }

            // --- Idle ---
            (State::Idle, Event::HostRequested { port, aircraft }) => {
                if aircraft::is_supported(&aircraft) {
                    out.clear_notice = true;
                    out.effects.push(StartHost { port });
                    State::StartingHost { port }
                } else {
                    out.notice = Some(Notice::Error(format!(
                        "Hosting needs a supported aircraft: {}. You have the {} loaded.",
                        aircraft::supported_list(),
                        aircraft::display_name(&aircraft)
                    )));
                    State::Idle
                }
            }
            (State::Idle, Event::JoinRequested { address, aircraft }) => {
                if aircraft::is_supported(&aircraft) {
                    out.clear_notice = true;
                    out.effects.push(Join {
                        address: address.clone(),
                    });
                    State::Joining { address }
                } else {
                    out.notice = Some(Notice::Error(format!(
                        "Joining needs a supported aircraft: {}. You have the {} loaded.",
                        aircraft::supported_list(),
                        aircraft::display_name(&aircraft)
                    )));
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
                out.effects.push(StopStreaming);
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
                out.effects.push(StartStreaming);
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
                out.effects.push(StopStreaming);
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
                out.effects.push(StartFollowing);
                State::Joined { address, host }
            }
            (State::Joining { address }, Event::JoinFailed(failure)) => {
                out.notice = Some(Notice::Error(self.join_failure_text(&address, &failure)));
                State::Idle
            }

            // --- Joined ---
            (State::Joined { .. }, Event::HostGone(gone)) => {
                out.effects.push(StopFollowing);
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
                out.effects.extend(stop_effects(&state));
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
                out.effects.extend(stop_effects(&state));
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
            Refusal::UnsupportedAircraft { joiner_aircraft } => Notice::Error(format!(
                "A crew member tried to join with the {}, which is not supported.",
                aircraft::display_name(joiner_aircraft)
            )),
            Refusal::SessionFull => {
                Notice::Info("Someone tried to join, but the session is full.".into())
            }
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
                RejectReason::UnsupportedAircraft => format!(
                    "Your aircraft is not supported. Supported aircraft: {}.",
                    aircraft::supported_list()
                ),
                RejectReason::SessionFull => "Session full".into(),
                RejectReason::NotHosting => "The host is not accepting crew right now.".into(),
            },
        }
    }
}

/// Stops streaming or following for the state being left.
fn stop_effects(state: &State) -> Vec<Effect> {
    match state {
        State::Hosting { crew: Some(_), .. } => vec![Effect::StopStreaming],
        State::Joined { .. } => vec![Effect::StopFollowing],
        _ => vec![],
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

    fn seaplane() -> AircraftId {
        AircraftId {
            folder: "Cessna 172 SP".into(),
            acf: "Cessna_172SP_seaplane.acf".into(),
        }
    }

    fn a321() -> AircraftId {
        AircraftId {
            folder: "ToLissA321".into(),
            acf: "a321.acf".into(),
        }
    }

    fn session() -> Session {
        Session::new("0.1.0")
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
        assert_eq!(s.state().role(), None);
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

    // Scenario: Unsupported aircraft (host side).
    #[test]
    fn hosting_refused_with_unsupported_aircraft_lists_supported() {
        let mut s = session();
        let out = s.handle(Event::HostRequested {
            port: 49700,
            aircraft: a321(),
        });
        assert_eq!(s.state(), &State::Idle);
        assert!(out.effects.is_empty());
        let text = notice_text(&out);
        assert!(text.contains("Cessna 172 SP, Cessna 172 SP G1000, Cessna 172 SP Seaplane"));
    }

    // Scenario: Successful join (both sides).
    #[test]
    fn join_and_crew_joined_show_names_and_roles() {
        let host = hosting_with_crew();
        assert_eq!(host.state().role(), Some(Role::Authority));
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
        assert_eq!(out.effects, vec![Effect::StartFollowing]);
        assert_eq!(s.state().role(), Some(Role::Follower));
    }

    #[test]
    fn crew_joining_starts_streaming() {
        let mut s = hosting_waiting();
        let out = s.handle(Event::CrewJoined {
            name: "Alex".into(),
        });
        assert_eq!(out.effects, vec![Effect::StartStreaming]);
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
                host_protocol_version: 2,
                host_plugin_version: "0.2.0".into(),
            },
        )));
        let text = notice_text(&out);
        assert!(text.contains("0.2.0") && text.contains("0.1.0"), "{text}");

        let mut h = hosting_waiting();
        let out = h.handle(Event::CrewRefused(Refusal::VersionMismatch {
            joiner_plugin_version: "0.3.0".into(),
            joiner_protocol_version: 3,
        }));
        let text = notice_text(&out);
        assert!(text.contains("0.3.0") && text.contains("0.1.0"), "{text}");
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
    fn joining_with_unsupported_aircraft_is_refused_locally() {
        let mut s = session();
        let out = s.handle(Event::JoinRequested {
            address: "h".into(),
            aircraft: a321(),
        });
        assert!(out.effects.is_empty());
        assert_eq!(s.state(), &State::Idle);
        assert!(notice_text(&out).contains("Supported") || notice_text(&out).contains("supported"));
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
            aircraft: a321(),
        }));
        for refusal in [
            Refusal::BadPassword,
            Refusal::VersionMismatch {
                joiner_plugin_version: "0.2.0".into(),
                joiner_protocol_version: 2,
            },
            Refusal::AircraftMismatch {
                joiner_aircraft: seaplane(),
                host_aircraft: c172(),
            },
            Refusal::UnsupportedAircraft {
                joiner_aircraft: a321(),
            },
            Refusal::SessionFull,
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
            aircraft: a321(),
        }));
        let failures = [
            JoinFailure::Unreachable,
            JoinFailure::Rejected(RejectReason::BadPassword),
            JoinFailure::Rejected(RejectReason::TooManyAttempts),
            JoinFailure::Rejected(RejectReason::VersionMismatch {
                host_protocol_version: 2,
                host_plugin_version: "0.2.0".into(),
            }),
            JoinFailure::Rejected(RejectReason::AircraftMismatch {
                host_aircraft: c172(),
            }),
            JoinFailure::Rejected(RejectReason::UnsupportedAircraft),
            JoinFailure::Rejected(RejectReason::SessionFull),
            JoinFailure::Rejected(RejectReason::NotHosting),
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

        assert_eq!(texts.len(), 27);
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

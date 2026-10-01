//! Hosting: accept crew, run the host side of the handshake, and stream
//! flight state to the admitted follower.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use flyx_protocol::{ByeReason, Control, Datagram, KdfParams, PROTOCOL_VERSION, RejectReason};
use flyx_sync::Password;
use flyx_sync::aircraft;
use flyx_sync::session::{Event, HostFailure, PeerGone, Refusal};
use quinn::{Connection, SendStream, VarInt};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::auth;
use crate::control::{
    self, CLOSE_PROTOCOL_ERROR, CLOSE_REJECTED, ControlIn, ControlReader, close_code,
};
use crate::endpoint::{self, ListenError};
use crate::{ActivityCommand, CLOSE_FLUSH, HANDSHAKE_TIMEOUT, LocalInfo, Outputs, clean_name};

/// After a wrong password, further attempts from the same address are
/// refused for this long.
const RETRY_DELAY: Duration = Duration::from_secs(2);

struct HostContext {
    password: Password,
    local: LocalInfo,
    crew_present: AtomicBool,
    failures: Mutex<HashMap<IpAddr, Instant>>,
    out: Outputs,
}

impl HostContext {
    fn recently_failed(&self, ip: IpAddr) -> bool {
        let failures = self.failures.lock().unwrap_or_else(|e| e.into_inner());
        failures.get(&ip).is_some_and(|t| t.elapsed() < RETRY_DELAY)
    }

    fn record_failure(&self, ip: IpAddr) {
        let mut failures = self.failures.lock().unwrap_or_else(|e| e.into_inner());
        if failures.len() > 1000 {
            failures.retain(|_, t| t.elapsed() < RETRY_DELAY);
        }
        failures.insert(ip, Instant::now());
    }
}

/// A joiner that passed the handshake, not yet admitted.
struct Pending {
    conn: Connection,
    send: SendStream,
    reader: ControlReader,
    name: String,
}

/// The admitted follower.
struct Crew {
    conn: Connection,
    send: SendStream,
    control: mpsc::UnboundedReceiver<ControlIn>,
}

pub(crate) async fn run(
    port: u16,
    password: Password,
    local: LocalInfo,
    mut commands: mpsc::UnboundedReceiver<ActivityCommand>,
    out: Outputs,
) {
    let endpoint = match endpoint::server(port) {
        Ok(e) => e,
        Err(ListenError::PortInUse(_)) => {
            out.session(Event::HostFailed(HostFailure::PortUnavailable));
            return;
        }
        Err(ListenError::Other(e)) => {
            out.session(Event::HostFailed(HostFailure::Other(e)));
            return;
        }
    };
    info!(port, "hosting");
    out.session(Event::Listening {
        addresses: endpoint::local_addresses(port),
    });

    let ctx = Arc::new(HostContext {
        password,
        local,
        crew_present: AtomicBool::new(false),
        failures: Mutex::new(HashMap::new()),
        out: out.clone(),
    });
    let (admitted_tx, mut admitted_rx) = mpsc::unbounded_channel::<Pending>();
    let mut crew: Option<Crew> = None;

    let reason = loop {
        tokio::select! {
            incoming = endpoint.accept() => {
                let Some(incoming) = incoming else {
                    out.session(Event::HostFailed(HostFailure::Other("the listener stopped".into())));
                    break ByeReason::StoppedHosting;
                };
                let ctx = ctx.clone();
                let tx = admitted_tx.clone();
                tokio::spawn(async move {
                    match tokio::time::timeout(HANDSHAKE_TIMEOUT, handshake(incoming, &ctx)).await {
                        Ok(Some(pending)) => {
                            let _ = tx.send(pending);
                        }
                        Ok(None) => {}
                        Err(_) => debug!("handshake timed out"),
                    }
                });
            }
            Some(pending) = admitted_rx.recv() => {
                if crew.is_some() {
                    // Two joiners finished the handshake at the same time.
                    let Pending { conn, mut send, .. } = pending;
                    reject(&conn, &mut send, RejectReason::SessionFull).await;
                    out.session(Event::CrewRefused(Refusal::SessionFull));
                } else {
                    match admit(pending, &ctx.local).await {
                        Ok((name, admitted)) => {
                            info!(%name, "crew joined");
                            ctx.crew_present.store(true, Ordering::Release);
                            crew = Some(admitted);
                            out.session(Event::CrewJoined { name });
                        }
                        Err(e) => warn!(%e, "admitting crew failed"),
                    }
                }
            }
            input = next_crew_input(&mut crew) => {
                let gone = match input {
                    ControlIn::Message(Control::Bye(reason)) => Some(PeerGone::Said(reason)),
                    ControlIn::Message(other) => {
                        debug!(?other, "ignoring message from crew");
                        None
                    }
                    ControlIn::Ended(gone) => Some(gone),
                };
                if let Some(gone) = gone
                    && let Some(c) = crew.take()
                {
                    info!(?gone, "crew gone");
                    c.conn.close(VarInt::from_u32(0), b"");
                    ctx.crew_present.store(false, Ordering::Release);
                    out.session(Event::CrewGone(gone));
                }
            }
            command = commands.recv() => match command {
                Some(ActivityCommand::FlightState(state)) => {
                    if let Some(c) = &crew
                        && let Err(e) = c.conn.send_datagram(Datagram::FlightState(state).encode().into())
                    {
                        debug!(%e, "flight state not sent");
                    }
                }
                Some(ActivityCommand::Paused(paused)) => {
                    if let Some(c) = &mut crew {
                        let _ = control::send(&mut c.send, &Control::Paused(paused)).await;
                    }
                }
                Some(ActivityCommand::Disconnect(reason)) => break reason,
                None => break ByeReason::PluginStopped,
            }
        }
    };

    if let Some(c) = crew.take() {
        say_bye(c.conn, c.send, reason).await;
    }
    endpoint.close(close_code(reason), b"");
    let _ = tokio::time::timeout(CLOSE_FLUSH, endpoint.wait_idle()).await;
    info!(?reason, "stopped hosting");
}

async fn next_crew_input(crew: &mut Option<Crew>) -> ControlIn {
    match crew {
        Some(c) => c
            .control
            .recv()
            .await
            .unwrap_or(ControlIn::Ended(PeerGone::Lost)),
        None => std::future::pending().await,
    }
}

/// Runs the host side of the handshake. Refusals are reported to the host
/// user from here; only a joiner that passed every check is returned.
async fn handshake(incoming: quinn::Incoming, ctx: &HostContext) -> Option<Pending> {
    let conn = match incoming.await {
        Ok(c) => c,
        Err(e) => {
            debug!(%e, "incoming connection failed");
            return None;
        }
    };
    let ip = endpoint::canonical_ip(conn.remote_address());
    let (mut send, recv) = match conn.accept_bi().await {
        Ok(s) => s,
        Err(e) => {
            debug!(%e, "no control stream");
            return None;
        }
    };
    let mut reader = ControlReader::new(recv);

    let (protocol_version, plugin_version) = match control::expect(&mut reader).await {
        Ok(Control::Hello {
            protocol_version,
            plugin_version,
        }) => (protocol_version, plugin_version),
        other => return protocol_error(&conn, other),
    };
    if protocol_version != PROTOCOL_VERSION {
        reject(
            &conn,
            &mut send,
            RejectReason::VersionMismatch {
                host_protocol_version: PROTOCOL_VERSION,
                host_plugin_version: ctx.local.plugin_version.clone(),
            },
        )
        .await;
        ctx.out
            .session(Event::CrewRefused(Refusal::VersionMismatch {
                joiner_plugin_version: plugin_version,
                joiner_protocol_version: protocol_version,
            }));
        return None;
    }
    if ctx.crew_present.load(Ordering::Acquire) {
        reject(&conn, &mut send, RejectReason::SessionFull).await;
        ctx.out.session(Event::CrewRefused(Refusal::SessionFull));
        return None;
    }
    if ctx.recently_failed(ip) {
        reject(&conn, &mut send, RejectReason::TooManyAttempts).await;
        return None;
    }

    let salt = auth::random_salt().ok()?;
    let kdf = KdfParams::DEFAULT;
    control::send(&mut send, &Control::Challenge { salt, kdf })
        .await
        .ok()?;
    let password = ctx.password.clone();
    let key = tokio::task::spawn_blocking(move || auth::derive_key(password.expose(), &salt, kdf))
        .await
        .ok()?
        .ok()?;
    let exporter = auth::exporter(&conn).ok()?;

    let mac = match control::expect(&mut reader).await {
        Ok(Control::ClientProof { mac }) => mac,
        other => return protocol_error(&conn, other),
    };
    if !auth::verify(&key, auth::CLIENT_LABEL, &exporter, &mac) {
        info!(%ip, "join refused: wrong password");
        ctx.record_failure(ip);
        reject(&conn, &mut send, RejectReason::BadPassword).await;
        ctx.out.session(Event::CrewRefused(Refusal::BadPassword));
        return None;
    }
    let mac = auth::prove(&key, auth::HOST_LABEL, &exporter);
    control::send(&mut send, &Control::HostProof { mac })
        .await
        .ok()?;

    let (name, joiner_aircraft) = match control::expect(&mut reader).await {
        Ok(Control::Join {
            display_name,
            aircraft,
        }) => (clean_name(&display_name), aircraft),
        other => return protocol_error(&conn, other),
    };
    if !aircraft::same_aircraft(&joiner_aircraft, &ctx.local.aircraft) {
        reject(
            &conn,
            &mut send,
            RejectReason::AircraftMismatch {
                host_aircraft: ctx.local.aircraft.clone(),
            },
        )
        .await;
        ctx.out
            .session(Event::CrewRefused(Refusal::AircraftMismatch {
                joiner_aircraft,
                host_aircraft: ctx.local.aircraft.clone(),
            }));
        return None;
    }
    Some(Pending {
        conn,
        send,
        reader,
        name,
    })
}

async fn admit(pending: Pending, local: &LocalInfo) -> Result<(String, Crew), String> {
    let Pending {
        conn,
        mut send,
        reader,
        name,
    } = pending;
    control::send(
        &mut send,
        &Control::Welcome {
            display_name: local.display_name.clone(),
            aircraft: local.aircraft.clone(),
        },
    )
    .await?;
    Ok((
        name,
        Crew {
            conn,
            send,
            control: reader.spawn(),
        },
    ))
}

/// Sends a rejection and closes the connection once the joiner has had a
/// chance to read it.
async fn reject(conn: &Connection, send: &mut SendStream, reason: RejectReason) {
    debug!(?reason, "rejecting joiner");
    if control::send(send, &Control::Reject(reason)).await.is_ok() {
        let _ = send.finish();
        // The joiner closes the connection after reading the rejection.
        let _ = tokio::time::timeout(Duration::from_secs(2), conn.closed()).await;
    }
    conn.close(VarInt::from_u32(CLOSE_REJECTED), b"rejected");
}

fn protocol_error<T>(conn: &Connection, got: Result<Control, control::ReadError>) -> Option<T> {
    debug!(?got, "handshake protocol error");
    conn.close(VarInt::from_u32(CLOSE_PROTOCOL_ERROR), b"protocol error");
    None
}

/// Tells the peer why the session ends, then closes the connection.
pub(crate) async fn say_bye(conn: Connection, mut send: SendStream, reason: ByeReason) {
    let _ = tokio::time::timeout(
        Duration::from_millis(200),
        control::send(&mut send, &Control::Bye(reason)),
    )
    .await;
    conn.close(close_code(reason), b"bye");
}

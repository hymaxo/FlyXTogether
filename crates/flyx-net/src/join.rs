//! Joining: connect to a host, run the joiner side of the handshake, then
//! receive flight state as the follower.

use flyx_protocol::{Control, Datagram, PROTOCOL_VERSION};
use flyx_sync::Password;
use flyx_sync::session::{Event, JoinFailure, PeerGone};
use quinn::{Connection, ConnectionError, Endpoint, SendStream, VarInt};
use tokio::sync::mpsc;
use tracing::{debug, info};

use crate::auth;
use crate::control::{self, CLOSE_PROTOCOL_ERROR, ControlIn, ControlReader, ReadError, gone_from};
use crate::endpoint;
use crate::host::say_bye;
use crate::{
    ActivityCommand, CLOSE_FLUSH, DEFAULT_PORT, HANDSHAKE_TIMEOUT, LocalInfo, Outputs, clean_name,
    parse_address,
};

struct Joined {
    endpoint: Endpoint,
    conn: Connection,
    send: SendStream,
    reader: ControlReader,
    host_name: String,
}

pub(crate) async fn run(
    address: String,
    password: Password,
    local: LocalInfo,
    mut commands: mpsc::UnboundedReceiver<ActivityCommand>,
    out: Outputs,
) {
    let attempt = connect_and_join(&address, &password, &local);
    tokio::pin!(attempt);
    let result = loop {
        tokio::select! {
            result = &mut attempt => break result,
            command = commands.recv() => match command {
                // Cancelled while joining: dropping the attempt closes it.
                Some(ActivityCommand::Disconnect(_)) | None => return,
                Some(_) => {}
            },
        }
    };
    let joined = match result {
        Ok(j) => j,
        Err(failure) => {
            info!(?failure, "join failed");
            out.session(Event::JoinFailed(failure));
            return;
        }
    };
    info!(host = %joined.host_name, "joined");
    out.session(Event::Joined {
        host: joined.host_name.clone(),
    });

    let Joined {
        endpoint,
        conn,
        send,
        reader,
        ..
    } = joined;
    let mut control = reader.spawn();
    let reason = loop {
        tokio::select! {
            datagram = conn.read_datagram() => match datagram {
                Ok(bytes) => match Datagram::decode(&bytes) {
                    Ok(Datagram::FlightState(state)) => out.sample(state),
                    Ok(other) => debug!(?other, "ignoring datagram from host"),
                    Err(e) => debug!(%e, "bad datagram"),
                },
                Err(e) => {
                    out.session(Event::HostGone(gone_from(&e)));
                    break None;
                }
            },
            input = control.recv() => match input {
                Some(ControlIn::Message(Control::Paused(paused))) => out.paused(paused),
                Some(ControlIn::Message(Control::Bye(reason))) => {
                    out.session(Event::HostGone(PeerGone::Said(reason)));
                    break None;
                }
                Some(ControlIn::Message(other)) => debug!(?other, "ignoring message from host"),
                Some(ControlIn::Ended(gone)) => {
                    out.session(Event::HostGone(gone));
                    break None;
                }
                None => {
                    out.session(Event::HostGone(PeerGone::Lost));
                    break None;
                }
            },
            command = commands.recv() => match command {
                Some(ActivityCommand::Disconnect(reason)) => break Some(reason),
                None => break Some(flyx_protocol::ByeReason::PluginStopped),
                Some(_) => {}
            },
        }
    };
    match reason {
        Some(reason) => say_bye(conn, send, reason).await,
        None => conn.close(VarInt::from_u32(0), b""),
    }
    let _ = tokio::time::timeout(CLOSE_FLUSH, endpoint.wait_idle()).await;
}

async fn connect_and_join(
    address: &str,
    password: &Password,
    local: &LocalInfo,
) -> Result<Joined, JoinFailure> {
    let other = |e: &dyn std::fmt::Display| JoinFailure::Other(e.to_string());
    let (host, port) = parse_address(address, DEFAULT_PORT).map_err(|e| other(&e))?;
    let target = tokio::net::lookup_host((host.as_str(), port))
        .await
        .map_err(|e| JoinFailure::Other(format!("cannot resolve {host}: {e}")))?
        // Prefer IPv4, which home routers forward most reliably.
        .min_by_key(|a| a.is_ipv6())
        .ok_or_else(|| JoinFailure::Other(format!("{host} has no address")))?;
    debug!(%target, "connecting");

    let endpoint = endpoint::client(target).map_err(|e| other(&e))?;
    let conn = endpoint
        .connect(target, endpoint::SERVER_NAME)
        .map_err(|e| other(&e))?
        .await
        .map_err(|e| match e {
            ConnectionError::TimedOut => JoinFailure::Unreachable,
            e => other(&e),
        })?;
    let (mut send, recv) = conn.open_bi().await.map_err(|e| other(&e))?;
    let mut reader = ControlReader::new(recv);

    let handshake = handshake(&conn, &mut send, &mut reader, password, local);
    let result = tokio::time::timeout(HANDSHAKE_TIMEOUT, handshake)
        .await
        .unwrap_or_else(|_| {
            Err(JoinFailure::Other(
                "the host did not finish the handshake".into(),
            ))
        });
    match result {
        Ok(host_name) => Ok(Joined {
            endpoint,
            conn,
            send,
            reader,
            host_name,
        }),
        Err(failure) => {
            conn.close(VarInt::from_u32(CLOSE_PROTOCOL_ERROR), b"");
            let _ = tokio::time::timeout(CLOSE_FLUSH, endpoint.wait_idle()).await;
            Err(failure)
        }
    }
}

/// The joiner side of the handshake; returns the host's display name.
async fn handshake(
    conn: &Connection,
    send: &mut SendStream,
    reader: &mut ControlReader,
    password: &Password,
    local: &LocalInfo,
) -> Result<String, JoinFailure> {
    let failed = |e: String| JoinFailure::Other(e);
    control::send(
        send,
        &Control::Hello {
            protocol_version: PROTOCOL_VERSION,
            plugin_version: local.plugin_version.clone(),
        },
    )
    .await
    .map_err(failed)?;

    let (salt, kdf) = match next(reader).await? {
        Control::Challenge { salt, kdf } => (salt, kdf),
        other => return Err(unexpected(other)),
    };
    if !kdf.is_acceptable() {
        return Err(JoinFailure::Other(
            "the host asked for unusual key-derivation settings".into(),
        ));
    }
    let secret = password.clone();
    let key = tokio::task::spawn_blocking(move || auth::derive_key(secret.expose(), &salt, kdf))
        .await
        .map_err(|e| JoinFailure::Other(e.to_string()))?
        .map_err(|e| JoinFailure::Other(e.to_string()))?;
    let exporter = auth::exporter(conn).map_err(|e| JoinFailure::Other(e.to_string()))?;
    let mac = auth::prove(&key, auth::CLIENT_LABEL, &exporter);
    control::send(send, &Control::ClientProof { mac })
        .await
        .map_err(failed)?;

    match next(reader).await? {
        Control::HostProof { mac } => {
            if !auth::verify(&key, auth::HOST_LABEL, &exporter, &mac) {
                return Err(JoinFailure::HostProofFailed);
            }
        }
        other => return Err(unexpected(other)),
    }

    control::send(
        send,
        &Control::Join {
            display_name: local.display_name.clone(),
            aircraft: local.aircraft.clone(),
            definition: local.definition,
        },
    )
    .await
    .map_err(failed)?;
    match next(reader).await? {
        Control::Welcome { display_name, .. } => Ok(clean_name(&display_name)),
        other => Err(unexpected(other)),
    }
}

/// Next handshake message; a `Reject` or a broken stream becomes an error.
async fn next(reader: &mut ControlReader) -> Result<Control, JoinFailure> {
    match control::expect(reader).await {
        Ok(Control::Reject(reason)) => Err(JoinFailure::Rejected(reason)),
        Ok(message) => Ok(message),
        Err(ReadError::Connection(ConnectionError::TimedOut)) => Err(JoinFailure::Other(
            "the connection was lost during the handshake".into(),
        )),
        Err(e) => Err(JoinFailure::Other(e.to_string())),
    }
}

fn unexpected(message: Control) -> JoinFailure {
    JoinFailure::Other(format!("unexpected message from the host: {message:?}"))
}

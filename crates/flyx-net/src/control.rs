//! The control stream: framed messages, close codes and peer loss.

use flyx_protocol::{ByeReason, Control, DecodeError, FrameDecoder, encode_frame};
use flyx_sync::session::PeerGone;
use quinn::{ConnectionError, RecvStream, SendStream, VarInt};
use tokio::sync::mpsc;

/// Close code after a `Reject`.
pub(crate) const CLOSE_REJECTED: u32 = 16;
/// Close code for a peer that broke the protocol.
pub(crate) const CLOSE_PROTOCOL_ERROR: u32 = 17;

/// The QUIC close code that carries a leave reason, so the peer learns it
/// even if the `Bye` message on the stream is lost in the close.
pub(crate) fn close_code(reason: ByeReason) -> VarInt {
    VarInt::from_u32(match reason {
        ByeReason::Left => 1,
        ByeReason::StoppedHosting => 2,
        ByeReason::AircraftChanged => 3,
        ByeReason::PluginStopped => 4,
        ByeReason::InternalError => 5,
    })
}

fn bye_from_code(code: VarInt) -> Option<ByeReason> {
    Some(match code.into_inner() {
        1 => ByeReason::Left,
        2 => ByeReason::StoppedHosting,
        3 => ByeReason::AircraftChanged,
        4 => ByeReason::PluginStopped,
        5 => ByeReason::InternalError,
        _ => return None,
    })
}

/// How the peer went away, judged from how the connection ended.
pub(crate) fn gone_from(error: &ConnectionError) -> PeerGone {
    match error {
        ConnectionError::ApplicationClosed(close) => bye_from_code(close.error_code)
            .map(PeerGone::Said)
            .unwrap_or(PeerGone::Lost),
        _ => PeerGone::Lost,
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ReadError {
    #[error("connection ended")]
    Connection(#[source] ConnectionError),
    #[error("control stream failed: {0}")]
    Stream(String),
    #[error(transparent)]
    Decode(#[from] DecodeError),
}

/// Reads control messages from a stream.
pub(crate) struct ControlReader {
    recv: RecvStream,
    decoder: FrameDecoder,
}

impl ControlReader {
    pub(crate) fn new(recv: RecvStream) -> Self {
        Self {
            recv,
            decoder: FrameDecoder::new(),
        }
    }

    /// The next message, or `None` when the peer finished the stream.
    pub(crate) async fn next(&mut self) -> Result<Option<Control>, ReadError> {
        loop {
            if let Some(message) = self.decoder.next_message()? {
                return Ok(Some(message));
            }
            let mut buf = [0u8; 2048];
            match self.recv.read(&mut buf).await {
                Ok(Some(n)) => self.decoder.push(&buf[..n]),
                Ok(None) => return Ok(None),
                Err(quinn::ReadError::ConnectionLost(e)) => return Err(ReadError::Connection(e)),
                Err(e) => return Err(ReadError::Stream(e.to_string())),
            }
        }
    }

    /// Hands the stream to a task that forwards messages until it ends.
    pub(crate) fn spawn(mut self) -> mpsc::UnboundedReceiver<ControlIn> {
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            loop {
                match self.next().await {
                    Ok(Some(message)) => {
                        if tx.send(ControlIn::Message(message)).is_err() {
                            return;
                        }
                    }
                    Ok(None) => {
                        let _ = tx.send(ControlIn::Ended(PeerGone::Said(ByeReason::Left)));
                        return;
                    }
                    Err(ReadError::Connection(e)) => {
                        let _ = tx.send(ControlIn::Ended(gone_from(&e)));
                        return;
                    }
                    Err(e) => {
                        tracing::warn!(%e, "control stream failed");
                        let _ = tx.send(ControlIn::Ended(PeerGone::Lost));
                        return;
                    }
                }
            }
        });
        rx
    }
}

/// What the control-stream task reports.
#[derive(Debug)]
pub(crate) enum ControlIn {
    Message(Control),
    Ended(PeerGone),
}

/// Writes one control message.
pub(crate) async fn send(stream: &mut SendStream, message: &Control) -> Result<(), String> {
    stream
        .write_all(&encode_frame(message))
        .await
        .map_err(|e| e.to_string())
}

/// Receives the next message of an in-progress handshake, treating the end
/// of the stream as an error.
pub(crate) async fn expect(reader: &mut ControlReader) -> Result<Control, ReadError> {
    match reader.next().await? {
        Some(message) => Ok(message),
        None => Err(ReadError::Stream("stream ended during handshake".into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn close_codes_round_trip() {
        for reason in [
            ByeReason::Left,
            ByeReason::StoppedHosting,
            ByeReason::AircraftChanged,
            ByeReason::PluginStopped,
            ByeReason::InternalError,
        ] {
            assert_eq!(bye_from_code(close_code(reason)), Some(reason));
        }
        assert_eq!(bye_from_code(VarInt::from_u32(CLOSE_REJECTED)), None);
    }

    #[test]
    fn timeout_means_lost() {
        assert_eq!(gone_from(&ConnectionError::TimedOut), PeerGone::Lost);
    }
}

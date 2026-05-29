//! A generic client face: read delimited request frames, dispatch them to a
//! [`ClientDialect`], and write replies, while concurrently fanning out state-change
//! notifications the face has subscribed to (auto-information).
//!
//! `run_face` is transport-agnostic (`AsyncRead + AsyncWrite`): it serves a real serial
//! port, a TCP socket, or an in-memory duplex in tests. [`open_serial`] opens a real COM /
//! tty port for production wiring.

use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::broadcast::error::RecvError;

use crate::dialect::{ClientDialect, FaceContext};
use crate::state::RadioEvent;

/// Serve one client connection until the transport closes.
///
/// Inbound bytes are split on `delim` into request frames; each is handed to `dialect`.
/// Concurrently, every [`StateChange`](crate::model::StateChange) the face is subscribed to
/// is rendered by the dialect's notification formatter and written out (gated by the face's
/// virtualized auto-information flag inside the dialect).
pub(crate) async fn run_face<T>(
    transport: T,
    dialect: Arc<dyn ClientDialect>,
    ctx: FaceContext,
    delim: u8,
) where
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + 'static,
{
    let (mut reader, mut writer) = tokio::io::split(transport);
    let mut notifications = ctx.state.subscribe();
    let mut frame: Vec<u8> = Vec::with_capacity(64);
    let mut chunk = [0u8; 512];

    loop {
        tokio::select! {
            read = reader.read(&mut chunk) => {
                match read {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let slice = chunk.get(..n).unwrap_or(&[]);
                        for &byte in slice {
                            frame.push(byte);
                            if byte == delim {
                                let request = std::mem::take(&mut frame);
                                tracing::trace!(
                                    face = ctx.face_id,
                                    req = %String::from_utf8_lossy(&request),
                                    "face request"
                                );
                                let reply = dialect.handle(&request, &ctx).await;
                                tracing::trace!(
                                    face = ctx.face_id,
                                    reply = %String::from_utf8_lossy(&reply),
                                    "face reply"
                                );
                                if !reply.is_empty() && writer.write_all(&reply).await.is_err() {
                                    return;
                                }
                                let _ = writer.flush().await;
                            }
                        }
                    }
                }
            }
            change = notifications.recv() => {
                match change {
                    Ok(event) => {
                        let bytes = match &event {
                            RadioEvent::Change(change) => {
                                dialect.format_notification(change, &ctx)
                            }
                            RadioEvent::Raw(raw) => dialect.format_passthrough(raw, &ctx),
                        };
                        if let Some(bytes) = bytes {
                            tracing::trace!(
                                face = ctx.face_id,
                                note = %String::from_utf8_lossy(&bytes),
                                "face notify"
                            );
                            if writer.write_all(&bytes).await.is_err() {
                                return;
                            }
                            let _ = writer.flush().await;
                        }
                    }
                    // A lagged subscriber simply skips missed frames; the next poll re-syncs.
                    Err(RecvError::Lagged(_)) => {}
                    Err(RecvError::Closed) => break,
                }
            }
        }
    }
}

/// Open a real serial port for a serial face.
pub(crate) fn open_serial(
    name: &str,
    port: &str,
    baud: u32,
) -> std::io::Result<serial2_tokio::SerialPort> {
    tracing::info!(face = name, port, baud, "opening serial face");
    serial2_tokio::SerialPort::open(port, baud)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::backend::loopback::LoopbackBackend;
    use crate::backend::RadioBackend;
    use crate::dialect::kenwood::ts590::Ts590Dialect;
    use crate::model::{RadioEventSource, StateChange, Vfo};
    use crate::permissions::FacePermissions;
    use crate::ptt::PttManager;
    use crate::radio::{detached_link, spawn_scheduler};
    use crate::state::StateHandle;
    use std::time::Duration;
    use tokio::io::DuplexStream;

    fn spawn_ts590_face(perms: FacePermissions) -> (DuplexStream, StateHandle) {
        let backend = LoopbackBackend::new();
        let caps = backend.capabilities();
        let arc: Arc<dyn RadioBackend> = Arc::new(backend);
        let state = StateHandle::new();
        let radio = spawn_scheduler(arc, detached_link(), state.clone());
        let ptt = PttManager::new(Duration::from_secs(300));
        let ctx = FaceContext::new(1, perms, state.clone(), radio, ptt, caps);
        let (client, server) = tokio::io::duplex(1024);
        tokio::spawn(run_face(
            server,
            Arc::new(Ts590Dialect::new()) as Arc<dyn ClientDialect>,
            ctx,
            b';',
        ));
        (client, state)
    }

    async fn read_frame(client: &mut DuplexStream) -> Vec<u8> {
        let mut frame = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            let n = tokio::time::timeout(Duration::from_secs(2), client.read(&mut byte))
                .await
                .expect("timely reply")
                .expect("read");
            if n == 0 {
                break;
            }
            frame.push(byte[0]);
            if byte[0] == b';' {
                break;
            }
        }
        frame
    }

    #[tokio::test]
    async fn serves_a_read_request() {
        let (mut client, state) = spawn_ts590_face(FacePermissions::read_only());
        state.record(
            StateChange::Freq {
                vfo: Vfo::A,
                hz: 7_030_000,
            },
            RadioEventSource::PollDiff,
        );
        client.write_all(b"FA;").await.expect("write");
        assert_eq!(read_frame(&mut client).await, b"FA00007030000;");
    }

    #[tokio::test]
    async fn fans_out_notifications_to_subscribed_face() {
        let (mut client, state) = spawn_ts590_face(FacePermissions::read_only());
        // Enable auto-info on the face.
        client.write_all(b"AI2;").await.expect("write");
        tokio::time::sleep(Duration::from_millis(20)).await;
        // A state change is pushed without the client polling.
        state.record(
            StateChange::Freq {
                vfo: Vfo::A,
                hz: 14_123_000,
            },
            RadioEventSource::PollDiff,
        );
        assert_eq!(read_frame(&mut client).await, b"FA00014123000;");
    }

    #[tokio::test]
    async fn relays_unmodeled_radio_frame_to_subscribed_face() {
        let (mut client, state) = spawn_ts590_face(FacePermissions::read_only());
        // Enable auto-info on the face.
        client.write_all(b"AI2;").await.expect("write");
        tokio::time::sleep(Duration::from_millis(20)).await;
        // The radio echoes a noise-blanker change the backend does not model; it must reach
        // the client verbatim so its NB state machine advances (regression for the ARCP-590
        // NB cycle that stalled when these echoes were dropped).
        state.record_raw(b"NB1;");
        assert_eq!(read_frame(&mut client).await, b"NB1;");
    }
}

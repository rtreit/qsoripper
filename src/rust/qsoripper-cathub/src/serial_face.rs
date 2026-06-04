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

    'serve: loop {
        tokio::select! {
            read = reader.read(&mut chunk) => {
                match read {
                    Ok(0) | Err(_) => break 'serve,
                    Ok(n) => {
                        let slice = chunk.get(..n).unwrap_or(&[]);
                        for &byte in slice {
                            // Bound the partial-frame buffer: a client that streams bytes
                            // without ever sending the delimiter must not grow the buffer
                            // without limit (OOM/DoS). Drop the malformed partial frame.
                            if frame.len() >= MAX_FRAME_LEN {
                                tracing::warn!(
                                    face = ctx.face_id,
                                    "request frame exceeded {MAX_FRAME_LEN} bytes without a \
                                     delimiter; discarding partial frame"
                                );
                                frame.clear();
                            }
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
                                    break 'serve;
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
                            RadioEvent::RawNative(raw) => {
                                dialect.format_native_passthrough(raw, &ctx)
                            }
                        };
                        if let Some(bytes) = bytes {
                            tracing::trace!(
                                face = ctx.face_id,
                                note = %String::from_utf8_lossy(&bytes),
                                "face notify"
                            );
                            if writer.write_all(&bytes).await.is_err() {
                                break 'serve;
                            }
                            let _ = writer.flush().await;
                        }
                    }
                    // The face fell behind the broadcast ring and missed one or more events.
                    // Skipping them is unsafe: a one-shot change (a mode or VFO switch) that
                    // was evicted from the ring is lost forever, leaving the client rendering
                    // permanently stale state. Re-synchronize by replaying the full current
                    // snapshot through the dialect's notification formatter so the client is
                    // restored to the live radio state (gated by the face's auto-info flag,
                    // so an AI-off face still emits nothing).
                    Err(RecvError::Lagged(skipped)) => {
                        tracing::warn!(
                            face = ctx.face_id,
                            skipped,
                            "face lagged the broadcast ring; re-syncing full snapshot"
                        );
                        let snapshot = ctx.snapshot();
                        for bytes in dialect.resync(&snapshot, &ctx) {
                            if writer.write_all(&bytes).await.is_err() {
                                break 'serve;
                            }
                        }
                        let _ = writer.flush().await;
                    }
                    Err(RecvError::Closed) => break 'serve,
                }
            }
        }
    }

    // The transport closed: never leave the radio keyed on behalf of a face that vanished.
    ctx.release_ptt_on_disconnect().await;
}

/// Maximum bytes buffered for a single in-progress request frame before the partial frame
/// is discarded. A real CAT frame is tens of bytes; this only bounds a misbehaving or
/// malicious client that never sends the delimiter.
const MAX_FRAME_LEN: usize = 4096;

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
    use crate::dialect::kenwood::mode_to_digit;
    use crate::dialect::kenwood::ts590::Ts590Dialect;
    use crate::model::{RadioEventSource, StateChange, Vfo};
    use crate::permissions::FacePermissions;
    use crate::ptt::PttManager;
    use crate::radio::{detached_link, spawn_scheduler};
    use crate::state::StateHandle;
    use std::time::Duration;
    use tokio::io::DuplexStream;

    fn spawn_ts590_face(perms: FacePermissions) -> (DuplexStream, StateHandle) {
        let (client, state, _ptt) = spawn_ts590_face_with_ptt(perms);
        (client, state)
    }

    fn spawn_ts590_face_with_ptt(
        perms: FacePermissions,
    ) -> (DuplexStream, StateHandle, PttManager) {
        let backend = LoopbackBackend::new();
        let caps = backend.capabilities();
        let arc: Arc<dyn RadioBackend> = Arc::new(backend);
        let state = StateHandle::new();
        let radio = spawn_scheduler(arc, detached_link(), state.clone());
        let ptt = PttManager::new(Duration::from_secs(300));
        let ctx = FaceContext::new(1, perms, state.clone(), radio, ptt.clone(), caps);
        let (client, server) = tokio::io::duplex(1024);
        tokio::spawn(run_face(
            server,
            Arc::new(Ts590Dialect::new()) as Arc<dyn ClientDialect>,
            ctx,
            b';',
        ));
        (client, state, ptt)
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

    #[tokio::test]
    async fn dropping_a_keyed_client_releases_the_ptt_lease() {
        // A client keys the transmitter, then its transport vanishes (crash/cable pull).
        // The hub must drop TX immediately, not hold it until the safety ceiling fires.
        let (mut client, _state, ptt) =
            spawn_ts590_face_with_ptt(FacePermissions::from_tokens(&["read", "ptt"]));
        client.write_all(b"TX;").await.expect("write");
        // Let the key reach the lease.
        for _ in 0..50 {
            if ptt.owner() == Some(1) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(ptt.owner(), Some(1), "client should hold the PTT lease");

        // Close the transport.
        drop(client);

        // The face must release the lease promptly on disconnect.
        for _ in 0..50 {
            if ptt.owner().is_none() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(
            ptt.owner(),
            None,
            "PTT lease must be released when a keyed client disconnects"
        );
    }

    #[tokio::test]
    async fn lagged_face_does_not_permanently_lose_a_one_shot_mode_change() {
        // Drive the face past the broadcast ring capacity so it lags, after first changing
        // the mode. A lagged face that merely skips would render the old mode forever; the
        // re-sync must restore the current mode to the client.
        let (mut client, state) = spawn_ts590_face(FacePermissions::read_only());
        client.write_all(b"AI2;").await.expect("write");
        tokio::time::sleep(Duration::from_millis(20)).await;

        // The one-shot change we must not lose: switch to CW.
        state.record(
            StateChange::Mode {
                vfo: Vfo::A,
                mode: crate::model::Mode::Cw,
            },
            RadioEventSource::NativePush,
        );
        // Flood the ring well past its 256-entry capacity so the face lags and the CW change
        // is evicted before it is read.
        for i in 0..2_000u64 {
            state.record(
                StateChange::Freq {
                    vfo: Vfo::A,
                    hz: 7_000_000 + i,
                },
                RadioEventSource::PollDiff,
            );
        }

        // Read frames until the re-sync emits the current CW mode (MD3;).
        let mut saw_cw = false;
        for _ in 0..64 {
            let Ok(frame) =
                tokio::time::timeout(Duration::from_secs(2), read_frame(&mut client)).await
            else {
                break;
            };
            if frame == vec![b'M', b'D', mode_to_digit(crate::model::Mode::Cw), b';'] {
                saw_cw = true;
                break;
            }
        }
        assert!(
            saw_cw,
            "after lagging, the face must be re-synced to the current CW mode"
        );
    }
}

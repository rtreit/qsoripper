//! A serial face: one virtual COM port (or in-memory transport) bound to a
//! client dialect. Reads client commands, frames them on the Kenwood
//! terminator, dispatches them to the dialect, writes replies, and pushes
//! auto-information notifications when the face has AI enabled.

use std::sync::Arc;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::RecvError;

use crate::dialect::{ClientDialect, FaceContext};
use crate::error::FaceError;
use crate::state::StateChange;

/// Kenwood command terminator used for framing.
const TERMINATOR: u8 = b';';

/// Open a virtual serial port for a face.
pub(crate) fn open_serial(
    name: &str,
    port: &str,
    baud: u32,
) -> Result<serial2_tokio::SerialPort, FaceError> {
    serial2_tokio::SerialPort::open(port, baud).map_err(|error| FaceError::Bind {
        name: name.to_string(),
        endpoint: port.to_string(),
        message: error.to_string(),
    })
}

/// Drive one face over the given transport until the client disconnects.
///
/// `changes` is the caller-provided change subscription; subscribing before the
/// task is spawned guarantees no state change is missed between spawn and the
/// first poll of the receiver.
pub(crate) async fn run_face<T>(
    mut transport: T,
    dialect: Arc<dyn ClientDialect>,
    ctx: FaceContext,
    mut changes: broadcast::Receiver<StateChange>,
) -> Result<(), FaceError>
where
    T: AsyncRead + AsyncWrite + Unpin,
{
    let mut pending = Vec::new();
    let mut chunk = [0u8; 256];

    loop {
        tokio::select! {
            read = transport.read(&mut chunk) => {
                let n = read.map_err(|source| FaceError::Io {
                    name: ctx.name().to_string(),
                    source,
                })?;
                if n == 0 {
                    return Ok(());
                }
                if let Some(slice) = chunk.get(..n) {
                    pending.extend_from_slice(slice);
                }
                drain_frames(&mut transport, &mut pending, dialect.as_ref(), &ctx).await?;
            }
            change = changes.recv() => {
                match change {
                    Ok(change) => {
                        if let Some(frame) = dialect.format_notification(&change, &ctx) {
                            write_all(&mut transport, &frame, &ctx).await?;
                        }
                    }
                    Err(RecvError::Lagged(_)) => {}
                    Err(RecvError::Closed) => return Ok(()),
                }
            }
        }
    }
}

async fn drain_frames<T>(
    transport: &mut T,
    pending: &mut Vec<u8>,
    dialect: &dyn ClientDialect,
    ctx: &FaceContext,
) -> Result<(), FaceError>
where
    T: AsyncWrite + Unpin,
{
    while let Some(pos) = pending.iter().position(|&b| b == TERMINATOR) {
        let frame: Vec<u8> = pending.drain(..=pos).collect();
        let reply = dialect.handle(&frame, ctx).await;
        if !reply.is_empty() {
            write_all(transport, &reply, ctx).await?;
        }
    }
    Ok(())
}

async fn write_all<T>(transport: &mut T, bytes: &[u8], ctx: &FaceContext) -> Result<(), FaceError>
where
    T: AsyncWrite + Unpin,
{
    transport
        .write_all(bytes)
        .await
        .map_err(|source| FaceError::Io {
            name: ctx.name().to_string(),
            source,
        })?;
    transport.flush().await.map_err(|source| FaceError::Io {
        name: ctx.name().to_string(),
        source,
    })
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::unused_async
)]
mod tests {
    use super::*;
    use crate::backend::loopback::LoopbackBackend;
    use crate::dialect::kenwood::Ts590Dialect;
    use crate::dialect::Permissions;
    use crate::model::Vfo;
    use crate::state::{run_mutation_dispatcher, StateHandle};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn face_answers_read_and_applies_write() {
        let (state, inbox) = StateHandle::new(16);
        let backend = Arc::new(LoopbackBackend::new());
        let dispatcher = tokio::spawn(run_mutation_dispatcher(
            inbox,
            backend.clone(),
            state.clone(),
        ));
        state.set_frequency(Vfo::A, 14_074_000).await;

        let ctx = FaceContext::new(
            "n1mm",
            Permissions::default(),
            state.clone(),
            backend.clone(),
        );
        let dialect: Arc<dyn ClientDialect> = Arc::new(Ts590Dialect::new());
        let (mut client, face_side) = tokio::io::duplex(1024);
        let changes = state.subscribe();
        let face = tokio::spawn(run_face(face_side, dialect, ctx, changes));

        client.write_all(b"FA;").await.expect("write read cmd");
        let mut reply = vec![0u8; 14];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(&reply, b"FA00014074000;");

        client
            .write_all(b"FA00021074000;")
            .await
            .expect("write set cmd");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert_eq!(state.snapshot().await.freq_a, 21_074_000);

        drop(client);
        face.await.expect("face task").expect("face ok");
        dispatcher.abort();
    }

    #[tokio::test]
    async fn ai_subscribed_face_receives_notifications() {
        let (state, inbox) = StateHandle::new(16);
        let backend = Arc::new(LoopbackBackend::new());
        let dispatcher = tokio::spawn(run_mutation_dispatcher(
            inbox,
            backend.clone(),
            state.clone(),
        ));

        let ctx = FaceContext::new(
            "n1mm",
            Permissions::default(),
            state.clone(),
            backend.clone(),
        );
        ctx.set_ai_enabled(true);
        let dialect: Arc<dyn ClientDialect> = Arc::new(Ts590Dialect::new());
        let (mut client, face_side) = tokio::io::duplex(1024);
        let changes = state.subscribe();
        let face = tokio::spawn(run_face(face_side, dialect, ctx, changes));

        state.set_frequency(Vfo::A, 18_100_000).await;
        let mut reply = vec![0u8; 14];
        client.read_exact(&mut reply).await.expect("notification");
        assert_eq!(&reply, b"FA00018100000;");

        drop(client);
        face.await.expect("face task").expect("face ok");
        dispatcher.abort();
    }
}

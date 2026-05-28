//! The radio task: the single owner of the serial transport. All radio access
//! is serialized through its command inbox so no two clients ever interleave
//! bytes on the wire. The task is generic over the transport so tests drive it
//! with an in-memory `tokio::io::duplex` pipe instead of a real port.

use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot};

use crate::error::BackendError;

/// Kenwood-family command/reply terminator.
const TERMINATOR: u8 = b';';

/// One serialized radio command and its reply slot.
pub(crate) struct RadioCommand {
    /// Raw bytes to write to the radio (already terminated).
    pub(crate) bytes: Vec<u8>,
    /// Whether to wait for and return a reply frame.
    pub(crate) expect_reply: bool,
    /// Where to deliver the reply (or error).
    pub(crate) reply: oneshot::Sender<Result<Vec<u8>, BackendError>>,
}

/// Cloneable handle used by backends to submit commands to the radio task.
#[derive(Clone)]
pub(crate) struct RadioHandle {
    tx: mpsc::Sender<RadioCommand>,
}

impl RadioHandle {
    /// Submit a command, optionally waiting for a terminated reply frame.
    pub(crate) async fn send(
        &self,
        bytes: Vec<u8>,
        expect_reply: bool,
    ) -> Result<Vec<u8>, BackendError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(RadioCommand {
                bytes,
                expect_reply,
                reply,
            })
            .await
            .map_err(|_| BackendError::Transport("radio task stopped".to_string()))?;
        rx.await
            .map_err(|_| BackendError::Transport("radio task dropped reply".to_string()))?
    }
}

/// Create a radio command channel, returning the client handle and the inbox
/// receiver to hand to [`run_radio_task`].
pub(crate) fn channel(capacity: usize) -> (RadioHandle, mpsc::Receiver<RadioCommand>) {
    let (tx, rx) = mpsc::channel(capacity);
    (RadioHandle { tx }, rx)
}

/// Run the radio task over the given transport until the inbox is closed.
///
/// Commands are serviced strictly in order. For commands that expect a reply,
/// bytes are read until the Kenwood terminator or the per-command timeout.
pub(crate) async fn run_radio_task<T>(
    mut transport: T,
    mut inbox: mpsc::Receiver<RadioCommand>,
    reply_timeout: Duration,
) where
    T: AsyncRead + AsyncWrite + Unpin,
{
    while let Some(command) = inbox.recv().await {
        let result = service_command(&mut transport, &command, reply_timeout).await;
        let _ = command.reply.send(result);
    }
}

async fn service_command<T>(
    transport: &mut T,
    command: &RadioCommand,
    reply_timeout: Duration,
) -> Result<Vec<u8>, BackendError>
where
    T: AsyncRead + AsyncWrite + Unpin,
{
    transport
        .write_all(&command.bytes)
        .await
        .map_err(|e| BackendError::Transport(e.to_string()))?;
    transport
        .flush()
        .await
        .map_err(|e| BackendError::Transport(e.to_string()))?;

    if !command.expect_reply {
        return Ok(Vec::new());
    }

    match tokio::time::timeout(reply_timeout, read_frame(transport)).await {
        Ok(result) => result,
        Err(_) => Err(BackendError::Timeout(format!(
            "no reply within {reply_timeout:?}"
        ))),
    }
}

async fn read_frame<T>(transport: &mut T) -> Result<Vec<u8>, BackendError>
where
    T: AsyncRead + Unpin,
{
    let mut frame = Vec::new();
    loop {
        let byte = transport
            .read_u8()
            .await
            .map_err(|e| BackendError::Transport(e.to_string()))?;
        frame.push(byte);
        if byte == TERMINATOR {
            return Ok(frame);
        }
    }
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

    async fn spawn_with_fake() -> (RadioHandle, tokio::io::DuplexStream) {
        let (handle, inbox) = channel(8);
        let (client_side, radio_side) = tokio::io::duplex(1024);
        tokio::spawn(run_radio_task(
            radio_side,
            inbox,
            Duration::from_millis(200),
        ));
        (handle, client_side)
    }

    #[tokio::test]
    async fn command_with_reply_reads_until_terminator() {
        let (handle, mut fake) = spawn_with_fake().await;
        let writer = tokio::spawn(async move {
            let mut got = vec![0u8; 3];
            fake.read_exact(&mut got).await.expect("read request");
            assert_eq!(&got, b"FA;");
            fake.write_all(b"FA00014074000;")
                .await
                .expect("write reply");
            fake.flush().await.expect("flush");
        });
        let reply = handle.send(b"FA;".to_vec(), true).await.expect("reply");
        assert_eq!(reply, b"FA00014074000;".to_vec());
        writer.await.expect("writer task");
    }

    #[tokio::test]
    async fn command_without_reply_returns_empty() {
        let (handle, mut fake) = spawn_with_fake().await;
        let reader = tokio::spawn(async move {
            let mut got = vec![0u8; 14];
            fake.read_exact(&mut got).await.expect("read request");
            assert_eq!(&got, b"FA00021074000;");
        });
        let reply = handle
            .send(b"FA00021074000;".to_vec(), false)
            .await
            .expect("ok");
        assert!(reply.is_empty());
        reader.await.expect("reader task");
    }

    #[tokio::test]
    async fn missing_reply_times_out() {
        let (handle, _fake) = spawn_with_fake().await;
        let err = handle
            .send(b"FA;".to_vec(), true)
            .await
            .expect_err("timeout");
        assert!(matches!(err, BackendError::Timeout(_)));
    }
}

//! Kenwood TS-590 backend. Maps universal [`StateMutation`]s to native CAT
//! commands and parses Kenwood replies (`FA`, `FB`, `MD`) back into universal
//! state. Phase 1 wires frequency, mode, and PTT to the wire.

use async_trait::async_trait;

use crate::backend::{RadioBackend, StateMutation};
use crate::error::BackendError;
use crate::model::{Mode, Vfo};
use crate::radio::RadioHandle;
use crate::state::StateHandle;

/// Backend driving a Kenwood TS-590 over a serialized radio handle.
pub(crate) struct Ts590Backend {
    radio: RadioHandle,
}

impl Ts590Backend {
    /// Construct a TS-590 backend over the given radio handle.
    pub(crate) fn new(radio: RadioHandle) -> Self {
        Self { radio }
    }

    fn freq_set_command(vfo: Vfo, hz: u64) -> Vec<u8> {
        let verb = match vfo {
            Vfo::A => "FA",
            Vfo::B => "FB",
        };
        format!("{verb}{hz:011};").into_bytes()
    }

    fn freq_query_command(vfo: Vfo) -> Vec<u8> {
        match vfo {
            Vfo::A => b"FA;".to_vec(),
            Vfo::B => b"FB;".to_vec(),
        }
    }
}

#[async_trait]
impl RadioBackend for Ts590Backend {
    async fn poll(&self, state: &StateHandle) -> Result<(), BackendError> {
        let fa = self
            .radio
            .send(Self::freq_query_command(Vfo::A), true)
            .await?;
        if let Some(hz) = parse_freq(&fa, b"FA") {
            state.set_frequency(Vfo::A, hz).await;
        }
        let fb = self
            .radio
            .send(Self::freq_query_command(Vfo::B), true)
            .await?;
        if let Some(hz) = parse_freq(&fb, b"FB") {
            state.set_frequency(Vfo::B, hz).await;
        }
        let md = self.radio.send(b"MD;".to_vec(), true).await?;
        if let Some(mode) = parse_mode(&md) {
            state.set_mode(Vfo::A, mode).await;
        }
        Ok(())
    }

    async fn apply(
        &self,
        mutation: StateMutation,
        state: &StateHandle,
    ) -> Result<(), BackendError> {
        match mutation {
            StateMutation::Frequency { vfo, hz } => {
                self.radio
                    .send(Self::freq_set_command(vfo, hz), false)
                    .await?;
                state.set_frequency(vfo, hz).await;
            }
            StateMutation::Mode { vfo, mode } => {
                let cmd = format!("MD{};", mode.to_kenwood_digit()).into_bytes();
                self.radio.send(cmd, false).await?;
                state.set_mode(vfo, mode).await;
            }
            StateMutation::Ptt { keyed } => {
                let cmd = if keyed {
                    b"TX;".to_vec()
                } else {
                    b"RX;".to_vec()
                };
                self.radio.send(cmd, false).await?;
            }
        }
        Ok(())
    }

    async fn passthrough(&self, raw: &[u8]) -> Result<Vec<u8>, BackendError> {
        self.radio.send(raw.to_vec(), is_query(raw)).await
    }
}

/// Whether a raw command is a query (expects a reply): a verb followed only by
/// the terminator, with no parameter payload.
fn is_query(raw: &[u8]) -> bool {
    let trimmed = raw.strip_suffix(b";").unwrap_or(raw);
    !trimmed.is_empty() && trimmed.iter().all(u8::is_ascii_alphabetic)
}

/// Parse a Kenwood frequency frame (`FA`/`FB` + 11 digits + `;`).
fn parse_freq(frame: &[u8], verb: &[u8]) -> Option<u64> {
    let body = frame.strip_prefix(verb)?;
    let digits = body.strip_suffix(b";")?;
    if digits.len() != 11 || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(digits).ok()?.parse::<u64>().ok()
}

/// Parse a Kenwood mode frame (`MD` + 1 digit + `;`).
fn parse_mode(frame: &[u8]) -> Option<Mode> {
    let body = frame.strip_prefix(b"MD")?;
    let digits = body.strip_suffix(b";")?;
    let &[digit] = digits else {
        return None;
    };
    Mode::from_kenwood_digit(digit as char)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::radio::{self, run_radio_task};
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn backend_with_fake() -> (Ts590Backend, tokio::io::DuplexStream) {
        let (handle, inbox) = radio::channel(8);
        let (client, radio_side) = tokio::io::duplex(1024);
        tokio::spawn(run_radio_task(
            radio_side,
            inbox,
            Duration::from_millis(200),
        ));
        (Ts590Backend::new(handle), client)
    }

    #[test]
    fn parse_freq_reads_eleven_digits() {
        assert_eq!(parse_freq(b"FA00014074000;", b"FA"), Some(14_074_000));
        assert_eq!(parse_freq(b"FB00007030000;", b"FB"), Some(7_030_000));
        assert_eq!(parse_freq(b"FA0001407400;", b"FA"), None);
        assert_eq!(parse_freq(b"MD2;", b"FA"), None);
    }

    #[test]
    fn parse_mode_reads_single_digit() {
        assert_eq!(parse_mode(b"MD2;"), Some(Mode::Usb));
        assert_eq!(parse_mode(b"MD3;"), Some(Mode::Cw));
        assert_eq!(parse_mode(b"MD;"), None);
    }

    #[test]
    fn is_query_distinguishes_reads_from_writes() {
        assert!(is_query(b"FA;"));
        assert!(is_query(b"MD;"));
        assert!(!is_query(b"FA00014074000;"));
        assert!(!is_query(b"MD2;"));
    }

    #[test]
    fn freq_set_command_zero_pads() {
        assert_eq!(
            Ts590Backend::freq_set_command(Vfo::A, 14_074_000),
            b"FA00014074000;".to_vec()
        );
        assert_eq!(
            Ts590Backend::freq_set_command(Vfo::B, 7_030_000),
            b"FB00007030000;".to_vec()
        );
    }

    #[tokio::test]
    async fn apply_freq_writes_native_command_and_records_state() {
        let (backend, mut fake) = backend_with_fake();
        let reader = tokio::spawn(async move {
            let mut got = vec![0u8; 14];
            fake.read_exact(&mut got).await.expect("read set");
            assert_eq!(&got, b"FA00021074000;");
        });
        let (handle, _inbox) = StateHandle::new(16);
        backend
            .apply(
                StateMutation::Frequency {
                    vfo: Vfo::A,
                    hz: 21_074_000,
                },
                &handle,
            )
            .await
            .expect("apply");
        assert_eq!(handle.snapshot().await.freq_a, 21_074_000);
        reader.await.expect("reader");
    }

    #[tokio::test]
    async fn poll_parses_replies_into_state() {
        let (backend, mut fake) = backend_with_fake();
        let responder = tokio::spawn(async move {
            let mut buf = vec![0u8; 3];
            fake.read_exact(&mut buf).await.expect("FA?");
            assert_eq!(&buf, b"FA;");
            fake.write_all(b"FA00014074000;").await.expect("FA reply");
            let mut buf = vec![0u8; 3];
            fake.read_exact(&mut buf).await.expect("FB?");
            assert_eq!(&buf, b"FB;");
            fake.write_all(b"FB00007030000;").await.expect("FB reply");
            let mut buf = vec![0u8; 3];
            fake.read_exact(&mut buf).await.expect("MD?");
            assert_eq!(&buf, b"MD;");
            fake.write_all(b"MD3;").await.expect("MD reply");
        });
        let (handle, _inbox) = StateHandle::new(16);
        backend.poll(&handle).await.expect("poll");
        let snap = handle.snapshot().await;
        assert_eq!(snap.freq_a, 14_074_000);
        assert_eq!(snap.freq_b, 7_030_000);
        assert_eq!(snap.mode_a, Mode::Cw);
        responder.await.expect("responder");
    }

    #[tokio::test]
    async fn ptt_keys_the_transmitter() {
        let (backend, mut fake) = backend_with_fake();
        let reader = tokio::spawn(async move {
            let mut buf = vec![0u8; 3];
            fake.read_exact(&mut buf).await.expect("TX");
            assert_eq!(&buf, b"TX;");
        });
        let (handle, _inbox) = StateHandle::new(16);
        backend
            .apply(StateMutation::Ptt { keyed: true }, &handle)
            .await
            .expect("apply ptt");
        reader.await.expect("reader");
    }

    #[tokio::test]
    async fn passthrough_query_returns_reply() {
        let (backend, mut fake) = backend_with_fake();
        let responder = tokio::spawn(async move {
            let mut buf = vec![0u8; 3];
            fake.read_exact(&mut buf).await.expect("PS?");
            assert_eq!(&buf, b"PS;");
            fake.write_all(b"PS1;").await.expect("PS reply");
        });
        let reply = backend.passthrough(b"PS;").await.expect("passthrough");
        assert_eq!(reply, b"PS1;".to_vec());
        responder.await.expect("responder");
    }
}

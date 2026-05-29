//! The first-class native TS-590 backend.
//!
//! It speaks Kenwood `;`-terminated ASCII CAT directly and is the certified-native path.
//! Crucially, its poll command set contains **no `FR`/`FT` VFO-select commands** — polling
//! never retargets a VFO, which is the root-cause fix for the A/B oscillation seen when a
//! status poll toggled the receive VFO (design §8.8). Sets are fire-and-forget (Kenwood
//! radios do not acknowledge a set), and the radio's auto-information stream (`AI2;`) drives
//! native push.

use async_trait::async_trait;

use crate::backend::{
    BackendCapabilities, BackendError, Framing, NativeCommandFamily, RadioBackend, SplitStyle,
    TrustTier,
};
use crate::model::{Mode, PttSource, RadioEventSource, StateMutation, Vfo};
use crate::radio::{Expect, RadioLink};
use crate::state::StateHandle;

/// The baseline poll command set. Read-only queries only: **never** `FR`/`FT` (§8.8).
const POLL_COMMANDS: &[&[u8]] = &[b"FA;", b"FB;", b"MD;", b"DA;"];

/// The native TS-590 backend.
#[derive(Clone, Default)]
pub(crate) struct Ts590Backend;

impl Ts590Backend {
    /// Create the backend.
    pub(crate) fn new() -> Self {
        Ts590Backend
    }
}

/// Split a `;`-terminated frame into its leading alphabetic verb and the remaining payload.
fn split_frame(frame: &[u8]) -> Option<(&[u8], &[u8])> {
    let body = match frame.iter().position(|&b| b == b';') {
        Some(end) => frame.get(..end)?,
        None => frame,
    };
    let verb_len = body
        .iter()
        .position(|b| !b.is_ascii_alphabetic())
        .unwrap_or(body.len());
    if verb_len == 0 {
        return None;
    }
    let verb = body.get(..verb_len)?;
    let payload = body.get(verb_len..).unwrap_or(&[]);
    Some((verb, payload))
}

fn parse_u64(bytes: &[u8]) -> Option<u64> {
    std::str::from_utf8(bytes).ok()?.trim().parse::<u64>().ok()
}

/// Build an `FA`/`FB` frequency set frame.
fn freq_set(vfo: Vfo, hz: u64) -> Vec<u8> {
    let verb = match vfo {
        Vfo::A => "FA",
        Vfo::B => "FB",
    };
    format!("{verb}{hz:011};").into_bytes()
}

/// Whether a passthrough frame is a query (bare verb then `;`) versus a set.
fn is_query(frame: &[u8]) -> bool {
    matches!(split_frame(frame), Some((_, payload)) if payload.is_empty())
}

#[async_trait]
impl RadioBackend for Ts590Backend {
    async fn poll(&self, link: &RadioLink, state: &StateHandle) -> Result<(), BackendError> {
        for &cmd in POLL_COMMANDS {
            let verb = cmd.get(..2).unwrap_or(cmd).to_vec();
            let reply = link.submit(cmd.to_vec(), Expect::Reply(vec![verb])).await?;
            if let Some(mutation) = self.parse_event(&reply) {
                state.record(mutation.into_change(), RadioEventSource::PollDiff);
            }
        }
        Ok(())
    }

    async fn apply(
        &self,
        mutation: StateMutation,
        link: &RadioLink,
        state: &StateHandle,
    ) -> Result<(), BackendError> {
        let bytes = match mutation {
            StateMutation::SetVfoFreq { vfo, hz } => freq_set(vfo, hz),
            StateMutation::SetMode { mode, .. } => {
                vec![b'M', b'D', mode.to_kenwood_digit(), b';']
            }
            // The DATA sub-mode is an independent flag on the TS-590, set with `DA1;`/`DA0;`
            // and read back with `DA;`. It composes with the base `MD` mode so a USB+DATA
            // (PKTUSB) request is `MD2;` followed by `DA1;`.
            StateMutation::SetDataMode { on, .. } => {
                if on {
                    b"DA1;".to_vec()
                } else {
                    b"DA0;".to_vec()
                }
            }
            StateMutation::SetPtt { keyed, source } => {
                if keyed {
                    // Mirror Hamlib's TS-590 mapping so digital clients modulate from the
                    // DATA/USB path (`TX1;`) and the radio does not emit the data beep that
                    // a bare `TX;` triggers on the TS-590.
                    match source {
                        PttSource::Generic => b"TX;".to_vec(),
                        PttSource::Mic => b"TX0;".to_vec(),
                        PttSource::Data => b"TX1;".to_vec(),
                    }
                } else {
                    b"RX;".to_vec()
                }
            }
            StateMutation::SetSplit { enabled, tx_vfo } => {
                // Split is a deliberate, modeled write: receive on A, transmit on the chosen
                // VFO. This is the only path that issues FR/FT, never a status poll.
                let tx = match tx_vfo.unwrap_or(Vfo::B) {
                    Vfo::A => b'0',
                    Vfo::B => b'1',
                };
                if enabled {
                    [b"FR0;".as_slice(), &[b'F', b'T', tx, b';']].concat()
                } else {
                    b"FR0;FT0;".to_vec()
                }
            }
            StateMutation::SetRit { enabled, .. } => {
                if enabled {
                    b"RT1;".to_vec()
                } else {
                    b"RT0;".to_vec()
                }
            }
            StateMutation::SetXit { enabled, .. } => {
                if enabled {
                    b"XT1;".to_vec()
                } else {
                    b"XT0;".to_vec()
                }
            }
        };
        // Kenwood sets are not acknowledged: fire and forget.
        link.submit(bytes, Expect::NoReply).await?;
        state.record(mutation.into_change(), RadioEventSource::OptimisticWrite);
        Ok(())
    }

    fn parse_event(&self, frame: &[u8]) -> Option<StateMutation> {
        let (verb, payload) = split_frame(frame)?;
        match verb {
            b"FA" => Some(StateMutation::SetVfoFreq {
                vfo: Vfo::A,
                hz: parse_u64(payload)?,
            }),
            b"FB" => Some(StateMutation::SetVfoFreq {
                vfo: Vfo::B,
                hz: parse_u64(payload)?,
            }),
            b"MD" => Some(StateMutation::SetMode {
                vfo: Vfo::A,
                mode: Mode::from_kenwood_digit(*payload.first()?),
            }),
            b"DA" => Some(StateMutation::SetDataMode {
                vfo: Vfo::A,
                on: *payload.first()? == b'1',
            }),
            _ => None,
        }
    }

    async fn passthrough(&self, raw: &[u8], link: &RadioLink) -> Result<Vec<u8>, BackendError> {
        let expect = if is_query(raw) {
            let verb = split_frame(raw)
                .map(|(v, _)| v.to_vec())
                .unwrap_or_default();
            Expect::Reply(vec![verb])
        } else {
            Expect::NoReply
        };
        link.submit(raw.to_vec(), expect).await
    }

    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            model: "TS-590".to_string(),
            vfo_count: 2,
            has_rit: true,
            has_xit: true,
            has_smeter: true,
            split: SplitStyle::VfoPair,
            native_push: true,
            native_command_family: Some(NativeCommandFamily::Kenwood),
            framing: Framing::SemicolonTerminated,
            freq_min_hz: 30_000,
            freq_max_hz: 60_000_000,
            trust: TrustTier::CertifiedNative,
        }
    }

    fn native_push_enable(&self) -> Option<Vec<u8>> {
        Some(b"AI2;".to_vec())
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::radio::{link_channel, run_transport};
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn poll_commands_never_retarget_a_vfo() {
        for cmd in POLL_COMMANDS {
            assert!(
                !cmd.starts_with(b"FR") && !cmd.starts_with(b"FT"),
                "poll set must not contain a VFO-select command"
            );
        }
    }

    #[test]
    fn parse_event_reads_freq_and_mode() {
        let backend = Ts590Backend::new();
        assert_eq!(
            backend.parse_event(b"FA00007030000;"),
            Some(StateMutation::SetVfoFreq {
                vfo: Vfo::A,
                hz: 7_030_000
            })
        );
        assert_eq!(
            backend.parse_event(b"FB00014250000;"),
            Some(StateMutation::SetVfoFreq {
                vfo: Vfo::B,
                hz: 14_250_000
            })
        );
        assert_eq!(
            backend.parse_event(b"MD3;"),
            Some(StateMutation::SetMode {
                vfo: Vfo::A,
                mode: Mode::Cw
            })
        );
        assert_eq!(
            backend.parse_event(b"DA1;"),
            Some(StateMutation::SetDataMode {
                vfo: Vfo::A,
                on: true
            })
        );
        assert_eq!(
            backend.parse_event(b"DA0;"),
            Some(StateMutation::SetDataMode {
                vfo: Vfo::A,
                on: false
            })
        );
        assert_eq!(backend.parse_event(b"ZZ;"), None);
    }

    #[test]
    fn capabilities_are_certified_native_with_push() {
        let caps = Ts590Backend::new().capabilities();
        assert_eq!(caps.trust, TrustTier::CertifiedNative);
        assert!(caps.native_push);
        assert!(caps.supports_passthrough());
        assert_eq!(
            Ts590Backend::new().native_push_enable(),
            Some(b"AI2;".to_vec())
        );
    }

    #[tokio::test]
    async fn apply_freq_writes_kenwood_frame() {
        let (link, raw_rx) = link_channel();
        let backend = Arc::new(Ts590Backend::new());
        let arc: Arc<dyn RadioBackend> = backend.clone();
        let state = StateHandle::new();
        let (mut radio_side, server) = tokio::io::duplex(1024);
        tokio::spawn(run_transport(server, arc, state.clone(), raw_rx));

        backend
            .apply(
                StateMutation::SetVfoFreq {
                    vfo: Vfo::A,
                    hz: 7_030_000,
                },
                &link,
                &state,
            )
            .await
            .expect("apply");

        let mut buf = vec![0u8; 14];
        radio_side.read_exact(&mut buf).await.expect("read frame");
        assert_eq!(&buf, b"FA00007030000;");
        assert_eq!(state.snapshot().vfo(Vfo::A).freq_hz, 7_030_000);
    }

    #[tokio::test]
    async fn apply_ptt_keys_and_unkeys() {
        let (link, raw_rx) = link_channel();
        let backend = Arc::new(Ts590Backend::new());
        let arc: Arc<dyn RadioBackend> = backend.clone();
        let state = StateHandle::new();
        let (mut radio_side, server) = tokio::io::duplex(1024);
        tokio::spawn(run_transport(server, arc, state.clone(), raw_rx));

        backend
            .apply(
                StateMutation::SetPtt {
                    keyed: true,
                    source: PttSource::Generic,
                },
                &link,
                &state,
            )
            .await
            .expect("key");
        let mut buf = vec![0u8; 3];
        radio_side.read_exact(&mut buf).await.expect("read tx");
        assert_eq!(&buf, b"TX;");
        assert!(state.snapshot().ptt);
    }

    #[tokio::test]
    async fn ptt_data_source_keys_with_tx1() {
        let (link, raw_rx) = link_channel();
        let backend = Arc::new(Ts590Backend::new());
        let arc: Arc<dyn RadioBackend> = backend.clone();
        let state = StateHandle::new();
        let (mut radio_side, server) = tokio::io::duplex(1024);
        tokio::spawn(run_transport(server, arc, state.clone(), raw_rx));

        backend
            .apply(
                StateMutation::SetPtt {
                    keyed: true,
                    source: PttSource::Data,
                },
                &link,
                &state,
            )
            .await
            .expect("key");
        let mut buf = vec![0u8; 4];
        radio_side.read_exact(&mut buf).await.expect("read tx");
        assert_eq!(&buf, b"TX1;");
        assert!(state.snapshot().ptt);
    }

    #[tokio::test]
    async fn poll_reads_back_freq_and_mode() {
        let (link, raw_rx) = link_channel();
        let backend = Arc::new(Ts590Backend::new());
        let arc: Arc<dyn RadioBackend> = backend.clone();
        let state = StateHandle::new();
        let (radio_side, server) = tokio::io::duplex(1024);
        tokio::spawn(run_transport(server, arc, state.clone(), raw_rx));

        // A fake radio that answers the three poll queries.
        tokio::spawn(async move {
            let (mut rd, mut wr) = tokio::io::split(radio_side);
            let mut frame = Vec::new();
            let mut byte = [0u8; 1];
            loop {
                if rd.read(&mut byte).await.unwrap_or(0) == 0 {
                    break;
                }
                frame.push(byte[0]);
                if byte[0] == b';' {
                    let answer: &[u8] = match frame.as_slice() {
                        b"FA;" => b"FA00007030000;",
                        b"FB;" => b"FB00014250000;",
                        b"MD;" => b"MD3;",
                        b"DA;" => b"DA1;",
                        _ => b"",
                    };
                    let _ = wr.write_all(answer).await;
                    frame.clear();
                }
            }
        });

        backend.poll(&link, &state).await.expect("poll");
        let snap = state.snapshot();
        assert_eq!(snap.vfo(Vfo::A).freq_hz, 7_030_000);
        assert_eq!(snap.vfo(Vfo::B).freq_hz, 14_250_000);
        assert_eq!(snap.vfo(Vfo::A).mode, Mode::Cw);
        assert!(snap.vfo(Vfo::A).data, "DA; reply should set the DATA flag");
    }

    #[tokio::test]
    async fn apply_data_mode_writes_da_frame() {
        let (link, raw_rx) = link_channel();
        let backend = Arc::new(Ts590Backend::new());
        let arc: Arc<dyn RadioBackend> = backend.clone();
        let state = StateHandle::new();
        let (mut radio_side, server) = tokio::io::duplex(1024);
        tokio::spawn(run_transport(server, arc, state.clone(), raw_rx));

        backend
            .apply(
                StateMutation::SetDataMode {
                    vfo: Vfo::A,
                    on: true,
                },
                &link,
                &state,
            )
            .await
            .expect("set data on");
        let mut buf = vec![0u8; 4];
        radio_side.read_exact(&mut buf).await.expect("read da on");
        assert_eq!(&buf, b"DA1;");
        assert!(state.snapshot().vfo(Vfo::A).data);

        backend
            .apply(
                StateMutation::SetDataMode {
                    vfo: Vfo::A,
                    on: false,
                },
                &link,
                &state,
            )
            .await
            .expect("set data off");
        let mut buf = vec![0u8; 4];
        radio_side.read_exact(&mut buf).await.expect("read da off");
        assert_eq!(&buf, b"DA0;");
        assert!(!state.snapshot().vfo(Vfo::A).data);
    }

    #[test]
    fn query_detection_distinguishes_get_from_set() {
        assert!(is_query(b"FA;"));
        assert!(!is_query(b"FA00007030000;"));
        assert!(!is_query(b"EX0050000;"));
    }
}

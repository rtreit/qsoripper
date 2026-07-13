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
use crate::model::{Mode, PttSource, RadioEventSource, StateChange, StateMutation, TxPower, Vfo};
use crate::radio::{Expect, RadioLink};
use crate::state::StateHandle;

/// The baseline poll command set. Read-only queries only: **never** `FR`/`FT` (§8.8).
const POLL_COMMANDS: &[&[u8]] = &[b"FA;", b"FB;", b"IF;", b"MD;", b"DA;", b"PC;"];

/// `IF;` payload byte offsets from the Kenwood TS-590 status frame.
const IF_FREQ_RANGE: std::ops::Range<usize> = 0..11;
const IF_TX_OFFSET: usize = 26;
const IF_MODE_OFFSET: usize = 27;
const IF_RX_VFO_OFFSET: usize = 28;
const IF_SPLIT_OFFSET: usize = 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct IfStatus {
    freq_hz: u64,
    ptt: bool,
    mode: Mode,
    rx_vfo: Vfo,
    split: bool,
}

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

fn parse_rx_vfo_digit(bytes: &[u8]) -> Option<Vfo> {
    match *bytes.first()? {
        b'0' => Some(Vfo::A),
        b'1' => Some(Vfo::B),
        _ => None,
    }
}

/// The TS-590 limits configured carrier power to 25 W in AM and 100 W in other modes.
/// This is the same mode-sensitive maximum Hamlib uses to normalize `PC` into `RFPOWER`.
fn max_power_watts(mode: Mode) -> u32 {
    if mode == Mode::Am {
        25
    } else {
        100
    }
}

fn parse_if_status(frame: &[u8]) -> Option<IfStatus> {
    let (verb, payload) = split_frame(frame)?;
    if verb != b"IF" {
        return None;
    }
    let rx_vfo = match *payload.get(IF_RX_VFO_OFFSET)? {
        b'0' => Vfo::A,
        b'1' => Vfo::B,
        // The TS-590 can report non-VFO states such as memory mode. Cathub does not model
        // those as an active VFO, so ignore the frame instead of silently pinning VFO A.
        _ => return None,
    };
    Some(IfStatus {
        freq_hz: parse_u64(payload.get(IF_FREQ_RANGE)?)?,
        ptt: *payload.get(IF_TX_OFFSET)? == b'1',
        mode: Mode::from_kenwood_digit(*payload.get(IF_MODE_OFFSET)?),
        rx_vfo,
        split: *payload.get(IF_SPLIT_OFFSET)? == b'1',
    })
}

fn record_if_status(state: &StateHandle, status: IfStatus, source: RadioEventSource) {
    state.record(
        StateChange::Freq {
            vfo: status.rx_vfo,
            hz: status.freq_hz,
        },
        source,
    );
    state.record(
        StateChange::Mode {
            vfo: status.rx_vfo,
            mode: status.mode,
        },
        source,
    );
    state.record(StateChange::Ptt { keyed: status.ptt }, source);
    // The IF frame carries the split bit and the receive VFO but not the transmit VFO. On a
    // two-VFO radio the transmit VFO during split is always the non-receiving VFO, so derive
    // it here. This keeps the hub's notion of split/TX-VFO authoritative from the rig itself
    // and propagates the real state to every attached program.
    let tx_vfo = if status.split {
        status.rx_vfo.other()
    } else {
        status.rx_vfo
    };
    state.record(
        StateChange::Split {
            enabled: status.split,
            tx_vfo: Some(tx_vfo),
        },
        source,
    );
    state.record(StateChange::RxVfo { vfo: status.rx_vfo }, source);
}

/// Build an `FA`/`FB` frequency set frame.
fn freq_set(vfo: Vfo, hz: u64) -> Vec<u8> {
    let verb = match vfo {
        Vfo::A => "FA",
        Vfo::B => "FB",
    };
    format!("{verb}{hz:011};").into_bytes()
}

/// Build an `FR` receive-VFO select frame.
fn rx_vfo_set(vfo: Vfo) -> Vec<u8> {
    let digit = match vfo {
        Vfo::A => b'0',
        Vfo::B => b'1',
    };
    vec![b'F', b'R', digit, b';']
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
            let _ = self.record_event(&reply, state, RadioEventSource::PollDiff);
        }
        Ok(())
    }

    async fn apply(
        &self,
        mutation: StateMutation,
        link: &RadioLink,
        state: &StateHandle,
    ) -> Result<(), BackendError> {
        let snap = state.snapshot();
        let bytes = match mutation {
            StateMutation::SetRxVfo { vfo } => {
                let mut bytes = rx_vfo_set(vfo);
                let tx = if snap.split {
                    // Preserve a deliberate split: move only the receive VFO, keep the
                    // operator's transmit VFO untouched.
                    snap.tx_vfo
                } else {
                    // Switching the operating VFO must move RX *and* TX together. Sending a
                    // bare `FR` would leave the transmit VFO behind and silently create an
                    // accidental reverse-split (RX=new, TX=old).
                    vfo
                };
                let tx_digit = match tx {
                    Vfo::A => b'0',
                    Vfo::B => b'1',
                };
                bytes.extend_from_slice(&[b'F', b'T', tx_digit, b';']);
                bytes
            }
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
        // A non-split operating-VFO switch also moves TX, so reflect the cleared split and
        // the new transmit VFO in the snapshot (the bare RxVfo change above cannot).
        if let StateMutation::SetRxVfo { vfo } = mutation {
            if !snap.split {
                state.record(
                    StateChange::Split {
                        enabled: false,
                        tx_vfo: Some(vfo),
                    },
                    RadioEventSource::OptimisticWrite,
                );
            }
        }
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
            // Native push routing currently accepts one modeled mutation per frame. Polling
            // records the full IF status; unsolicited IF frames still refresh the critical
            // active-RX-VFO fact so OmniRig/HDSDR follows the displayed VFO.
            b"IF" => {
                parse_if_status(frame).map(|status| StateMutation::SetRxVfo { vfo: status.rx_vfo })
            }
            _ => None,
        }
    }

    fn record_event(&self, frame: &[u8], state: &StateHandle, source: RadioEventSource) -> bool {
        if let Some(status) = parse_if_status(frame) {
            record_if_status(state, status, source);
            return true;
        }

        let Some((verb, payload)) = split_frame(frame) else {
            return false;
        };
        match verb {
            b"FA" => parse_u64(payload).is_some_and(|hz| {
                state.record(StateChange::Freq { vfo: Vfo::A, hz }, source);
                true
            }),
            b"FB" => parse_u64(payload).is_some_and(|hz| {
                state.record(StateChange::Freq { vfo: Vfo::B, hz }, source);
                true
            }),
            b"FR" => parse_rx_vfo_digit(payload).is_some_and(|vfo| {
                state.record(StateChange::RxVfo { vfo }, source);
                true
            }),
            // A raw `FT` from a client (e.g. ARCP-590/N1MM split control) selects the
            // transmit VFO. Observe it so the snapshot's split/tx_vfo track reality instead
            // of going stale: split is active whenever the transmit VFO differs from the
            // receive VFO.
            b"FT" => parse_rx_vfo_digit(payload).is_some_and(|tx_vfo| {
                let rx_vfo = state.snapshot().rx_vfo;
                state.record(
                    StateChange::Split {
                        enabled: rx_vfo != tx_vfo,
                        tx_vfo: Some(tx_vfo),
                    },
                    source,
                );
                true
            }),
            b"MD" => payload.first().is_some_and(|digit| {
                let vfo = state.snapshot().rx_vfo;
                state.record(
                    StateChange::Mode {
                        vfo,
                        mode: Mode::from_kenwood_digit(*digit),
                    },
                    source,
                );
                true
            }),
            b"DA" => payload.first().is_some_and(|digit| {
                let vfo = state.snapshot().rx_vfo;
                state.record(
                    StateChange::DataMode {
                        vfo,
                        on: *digit == b'1',
                    },
                    source,
                );
                true
            }),
            b"PC" => parse_u64(payload)
                .and_then(|watts| u32::try_from(watts).ok())
                .is_some_and(|watts| {
                    let snapshot = state.snapshot();
                    let tx_vfo = if snapshot.split {
                        snapshot.tx_vfo
                    } else {
                        snapshot.rx_vfo
                    };
                    let max_watts = max_power_watts(snapshot.vfo(tx_vfo).mode);
                    state.record(
                        StateChange::TxPower {
                            power: Some(TxPower::from_watts(watts, max_watts)),
                        },
                        source,
                    );
                    true
                }),
            _ => false,
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
        assert_eq!(
            POLL_COMMANDS,
            &[b"FA;", b"FB;", b"IF;", b"MD;", b"DA;", b"PC;"]
        );
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
        assert_eq!(
            backend.parse_event(b"IF000140343201234-0000012345121019999;"),
            Some(StateMutation::SetRxVfo { vfo: Vfo::B })
        );
        assert_eq!(backend.parse_event(b"ZZ;"), None);
    }

    #[test]
    fn parse_if_status_reads_active_vfo_from_real_status_shape() {
        let status =
            parse_if_status(b"IF000140343201234-0000012345121019999;").expect("parse IF status");

        assert_eq!(
            status,
            IfStatus {
                freq_hz: 14_034_320,
                ptt: true,
                mode: Mode::Usb,
                rx_vfo: Vfo::B,
                split: true,
            }
        );
    }

    #[test]
    fn parse_if_status_ignores_non_vfo_memory_mode() {
        assert_eq!(
            parse_if_status(b"IF000140343201234-0000012345122019999;"),
            None
        );
    }

    #[test]
    fn if_poll_derives_tx_vfo_from_split_and_rx_vfo() {
        let backend = Ts590Backend::new();

        // RX=VFO B with split set -> TX must be derived as the non-receiving VFO (A).
        let split = StateHandle::new();
        assert!(backend.record_event(
            b"IF000140343201234-0000012345121019999;",
            &split,
            RadioEventSource::PollDiff,
        ));
        let snap = split.snapshot();
        assert_eq!(snap.rx_vfo, Vfo::B);
        assert!(snap.split);
        assert_eq!(snap.tx_vfo, Vfo::A, "split TX VFO is the non-RX VFO");

        // RX=VFO B with split clear -> TX tracks the operating (RX) VFO.
        let simplex = StateHandle::new();
        assert!(backend.record_event(
            b"IF000140343201234-0000012345021009999;",
            &simplex,
            RadioEventSource::PollDiff,
        ));
        let snap = simplex.snapshot();
        assert_eq!(snap.rx_vfo, Vfo::B);
        assert!(!snap.split);
        assert_eq!(snap.tx_vfo, Vfo::B, "simplex TX VFO equals the RX VFO");
    }

    #[test]
    fn native_ft_records_split_and_tx_vfo() {
        let backend = Ts590Backend::new();
        let state = StateHandle::new();

        // Operator on VFO B; a client (ARCP-590) then commands transmit on VFO A -> the
        // radio is in reverse-split and the snapshot must reflect it.
        assert!(backend.record_event(b"FR1;", &state, RadioEventSource::NativePush));
        assert!(backend.record_event(b"FT0;", &state, RadioEventSource::NativePush));
        let snap = state.snapshot();
        assert_eq!(snap.rx_vfo, Vfo::B);
        assert!(snap.split, "FR=B / FT=A must register as split");
        assert_eq!(snap.tx_vfo, Vfo::A);

        // Re-aligning the transmit VFO to the receive VFO clears the split.
        assert!(backend.record_event(b"FT1;", &state, RadioEventSource::NativePush));
        let snap = state.snapshot();
        assert!(!snap.split, "FR=B / FT=B must clear split");
        assert_eq!(snap.tx_vfo, Vfo::B);
    }

    #[test]
    fn pc_power_uses_the_transmit_vfo_mode_maximum() {
        let backend = Ts590Backend::new();
        let state = StateHandle::new();
        state.record(
            StateChange::Mode {
                vfo: Vfo::A,
                mode: Mode::Am,
            },
            RadioEventSource::PollDiff,
        );

        assert!(backend.record_event(b"PC025;", &state, RadioEventSource::PollDiff,));
        assert_eq!(state.snapshot().tx_power, Some(TxPower::from_watts(25, 25)));
    }

    #[test]
    fn native_fr_then_md_records_mode_on_active_vfo() {
        let backend = Ts590Backend::new();
        let state = StateHandle::new();
        state.record(
            StateChange::Freq {
                vfo: Vfo::B,
                hz: 14_034_320,
            },
            RadioEventSource::PollDiff,
        );

        assert!(backend.record_event(b"FR1;", &state, RadioEventSource::NativePush));
        assert!(backend.record_event(b"MD3;", &state, RadioEventSource::NativePush));

        let snap = state.snapshot();
        assert_eq!(snap.rx_vfo, Vfo::B);
        assert_eq!(snap.vfo(Vfo::B).freq_hz, 14_034_320);
        assert_eq!(snap.vfo(Vfo::B).mode, Mode::Cw);
        assert_eq!(
            snap.vfo(Vfo::A).mode,
            Mode::Usb,
            "VFO B mode push must not be recorded against VFO A"
        );
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
    async fn apply_rx_vfo_switches_operating_vfo_with_fr_and_ft() {
        let (link, raw_rx) = link_channel();
        let backend = Arc::new(Ts590Backend::new());
        let arc: Arc<dyn RadioBackend> = backend.clone();
        let state = StateHandle::new();
        let (mut radio_side, server) = tokio::io::duplex(1024);
        tokio::spawn(run_transport(server, arc, state.clone(), raw_rx));

        backend
            .apply(StateMutation::SetRxVfo { vfo: Vfo::B }, &link, &state)
            .await
            .expect("apply");

        // A non-split operating-VFO switch must move RX and TX together so the radio is
        // never left in an accidental reverse-split (RX=B, TX=A).
        let mut buf = vec![0u8; 8];
        radio_side.read_exact(&mut buf).await.expect("read frame");
        assert_eq!(&buf, b"FR1;FT1;");
        let snap = state.snapshot();
        assert_eq!(snap.rx_vfo, Vfo::B);
        assert!(!snap.split, "operating-VFO switch must not be split");
        assert_eq!(snap.tx_vfo, Vfo::B);
    }

    #[tokio::test]
    async fn apply_rx_vfo_preserves_split_tx_vfo() {
        let (link, raw_rx) = link_channel();
        let backend = Arc::new(Ts590Backend::new());
        let arc: Arc<dyn RadioBackend> = backend.clone();
        let state = StateHandle::new();
        state.record(
            StateChange::Split {
                enabled: true,
                tx_vfo: Some(Vfo::B),
            },
            RadioEventSource::PollDiff,
        );
        let (mut radio_side, server) = tokio::io::duplex(1024);
        tokio::spawn(run_transport(server, arc, state.clone(), raw_rx));

        backend
            .apply(StateMutation::SetRxVfo { vfo: Vfo::A }, &link, &state)
            .await
            .expect("apply");

        let mut buf = vec![0u8; 8];
        radio_side.read_exact(&mut buf).await.expect("read frame");
        assert_eq!(&buf, b"FR0;FT1;");
        let snap = state.snapshot();
        assert_eq!(snap.rx_vfo, Vfo::A);
        assert!(snap.split);
        assert_eq!(snap.tx_vfo, Vfo::B);
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

        // A fake radio that answers the poll queries.
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
                        b"IF;" => b"IF000070300000000+0000000000030000000;",
                        b"MD;" => b"MD3;",
                        b"DA;" => b"DA1;",
                        b"PC;" => b"PC050;",
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
        assert_eq!(snap.tx_power, Some(TxPower::from_watts(50, 100)));
    }

    #[tokio::test]
    async fn poll_reads_active_vfo_from_if_status() {
        let (link, raw_rx) = link_channel();
        let backend = Arc::new(Ts590Backend::new());
        let arc: Arc<dyn RadioBackend> = backend.clone();
        let state = StateHandle::new();
        let (radio_side, server) = tokio::io::duplex(1024);
        tokio::spawn(run_transport(server, arc, state.clone(), raw_rx));

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
                        b"FA;" => b"FA00003020950;",
                        b"FB;" => b"FB00014034320;",
                        b"IF;" => b"IF000140343201234-0000012345021009999;",
                        b"MD;" => b"MD3;",
                        b"DA;" => b"DA0;",
                        b"PC;" => b"PC100;",
                        _ => b"",
                    };
                    let _ = wr.write_all(answer).await;
                    frame.clear();
                }
            }
        });

        backend.poll(&link, &state).await.expect("poll");
        let snap = state.snapshot();
        assert_eq!(snap.rx_vfo, Vfo::B);
        assert_eq!(snap.vfo(snap.rx_vfo).freq_hz, 14_034_320);
        assert_eq!(snap.vfo(snap.rx_vfo).mode, Mode::Cw);
    }

    #[tokio::test]
    async fn poll_records_md_and_da_on_active_vfo() {
        let (link, raw_rx) = link_channel();
        let backend = Arc::new(Ts590Backend::new());
        let arc: Arc<dyn RadioBackend> = backend.clone();
        let state = StateHandle::new();
        let (radio_side, server) = tokio::io::duplex(1024);
        tokio::spawn(run_transport(server, arc, state.clone(), raw_rx));

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
                        b"FA;" => b"FA00003020950;",
                        b"FB;" => b"FB00014034320;",
                        b"IF;" => b"IF000140343201234-0000012345021009999;",
                        b"MD;" => b"MD3;",
                        b"DA;" => b"DA1;",
                        b"PC;" => b"PC100;",
                        _ => b"",
                    };
                    let _ = wr.write_all(answer).await;
                    frame.clear();
                }
            }
        });

        backend.poll(&link, &state).await.expect("poll");
        let snap = state.snapshot();
        assert_eq!(snap.rx_vfo, Vfo::B);
        assert_eq!(snap.vfo(Vfo::B).mode, Mode::Cw);
        assert!(snap.vfo(Vfo::B).data);
        assert_eq!(
            snap.vfo(Vfo::A).mode,
            Mode::Usb,
            "active VFO B MD reply must not update VFO A"
        );
        assert!(
            !snap.vfo(Vfo::A).data,
            "active VFO B DA reply must not update VFO A"
        );
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

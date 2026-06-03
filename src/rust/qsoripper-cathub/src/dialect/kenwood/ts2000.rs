//! Foreign-dialect TS-2000 client dialect for OmniRig / HDSDR (and Log4OM via OmniRig).
//!
//! This is a universal-tier *translator*: it answers `IF;`, `FA;`, `FB;`, and `MD;` from
//! the universal state and **rejects Hamlib-style VFO-target writes** (`FR`/`FT`), which
//! were the source of the TS-590 VFO A/B oscillation. It carries no native passthrough.

use async_trait::async_trait;

use super::{ai_frame, freq_frame, mode_from_digit, mode_to_digit, parse_command, ERR};
use crate::dialect::{ApplyOutcome, ClientDialect, FaceContext};
use crate::model::{StateChange, StateMutation, Vfo};
use crate::permissions::CommandClass;
use crate::state::Snapshot;

/// The TS-2000 translator dialect.
#[derive(Clone, Default)]
pub(crate) struct Ts2000Dialect;

impl Ts2000Dialect {
    /// Create the dialect.
    pub(crate) fn new() -> Self {
        Ts2000Dialect
    }
}

/// Synthesize a TS-2000 `IF;` status response from the universal state.
///
/// The layout follows the Kenwood TS-2000 `IF` field order. The frequency, TX/RX, mode,
/// and split fields are driven by real state; auxiliary fields are reported as defaults.
/// (The exact byte layout is validated against OmniRig in the live bring-up runbook.)
fn synth_if(snapshot: &Snapshot) -> Vec<u8> {
    let freq = snapshot.vfo(snapshot.rx_vfo).freq_hz;
    let tx = u8::from(snapshot.ptt);
    let mode = mode_to_digit(snapshot.vfo(snapshot.rx_vfo).mode) - b'0';
    let split = u8::from(snapshot.split);
    let rx_vfo = match snapshot.rx_vfo {
        Vfo::A => 0u8,
        Vfo::B => 1u8,
    };
    // Canonical Kenwood `IF` answer (38 bytes incl. `IF` and `;`). Field widths:
    //   freq(11) step(4) rit/xit(±5=6) rit(1) xit(1) bank(1) mem(2) tx(1) mode(1)
    //   vfo(1) scan(1) split(1) tone(1) tone#(2) p15(1)
    // Frequency, TX/RX, mode, VFO, and split are state-driven; the rest are defaults.
    format!("IF{freq:011}0000+0000000000{tx}{mode}{rx_vfo}0{split}0000;").into_bytes()
}

#[async_trait]
impl ClientDialect for Ts2000Dialect {
    async fn handle(&self, request: &[u8], ctx: &FaceContext) -> Vec<u8> {
        let Some((verb, payload)) = parse_command(request) else {
            return ERR.to_vec();
        };
        let read = payload.is_empty();
        match verb.as_slice() {
            b"IF" if read => synth_if(&ctx.snapshot()),
            b"FA" => {
                if read {
                    return freq_frame(b"FA", ctx.snapshot().vfo(Vfo::A).freq_hz);
                }
                set_freq(ctx, Vfo::A, &payload).await
            }
            b"FB" => {
                if read {
                    return freq_frame(b"FB", ctx.snapshot().vfo(Vfo::B).freq_hz);
                }
                set_freq(ctx, Vfo::B, &payload).await
            }
            b"MD" => {
                if read {
                    let d = mode_to_digit(ctx.snapshot().vfo(Vfo::A).mode);
                    return vec![b'M', b'D', d, b';'];
                }
                let Some(&d) = payload.first() else {
                    return ERR.to_vec();
                };
                reply(
                    ctx.apply_modeled(
                        StateMutation::SetMode {
                            vfo: Vfo::A,
                            mode: mode_from_digit(d),
                        },
                        CommandClass::ModeledWrite,
                    )
                    .await,
                )
            }
            // VFO-target writes are rejected outright: this is the anti-oscillation
            // guarantee. A read of FR/FT is answered from state.
            b"FR" if read => vec![b'F', b'R', vfo_digit(ctx.snapshot().rx_vfo), b';'],
            b"FT" if read => {
                let snap = ctx.snapshot();
                let tx = if snap.split { snap.tx_vfo } else { snap.rx_vfo };
                vec![b'F', b'T', vfo_digit(tx), b';']
            }
            b"AI" => {
                // `AI;` read reports current state without changing it; `AI<n>;` writes toggle.
                if read {
                    ai_frame(ctx.ai_on())
                } else {
                    let on = payload.first().is_some_and(|&d| d != b'0');
                    ctx.set_ai(on);
                    Vec::new()
                }
            }
            b"ID" if read => b"ID019;".to_vec(),
            b"PS" if read => b"PS1;".to_vec(),
            // The translator does not carry native passthrough (foreign dialect).
            _ => ERR.to_vec(),
        }
    }

    fn format_notification(&self, change: &StateChange, ctx: &FaceContext) -> Option<Vec<u8>> {
        if !ctx.ai_on() {
            return None;
        }
        match *change {
            StateChange::Freq { vfo: Vfo::A, hz } => Some(freq_frame(b"FA", hz)),
            StateChange::Freq { vfo: Vfo::B, hz } => Some(freq_frame(b"FB", hz)),
            StateChange::Mode { vfo: Vfo::A, mode } => {
                Some(vec![b'M', b'D', mode_to_digit(mode), b';'])
            }
            StateChange::RxVfo { .. } => Some(synth_if(&ctx.snapshot())),
            _ => None,
        }
    }
}

fn vfo_digit(vfo: Vfo) -> u8 {
    match vfo {
        Vfo::A => b'0',
        Vfo::B => b'1',
    }
}

async fn set_freq(ctx: &FaceContext, vfo: Vfo, payload: &[u8]) -> Vec<u8> {
    let Ok(hz) = std::str::from_utf8(payload)
        .unwrap_or("")
        .trim()
        .parse::<u64>()
    else {
        return ERR.to_vec();
    };
    reply(
        ctx.apply_modeled(
            StateMutation::SetVfoFreq { vfo, hz },
            CommandClass::ModeledWrite,
        )
        .await,
    )
}

fn reply(outcome: ApplyOutcome) -> Vec<u8> {
    match outcome {
        ApplyOutcome::Ok => Vec::new(),
        _ => ERR.to_vec(),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::backend::loopback::LoopbackBackend;
    use crate::backend::RadioBackend;
    use crate::model::RadioEventSource;
    use crate::permissions::FacePermissions;
    use crate::ptt::PttManager;
    use crate::radio::{detached_link, spawn_scheduler};
    use crate::state::StateHandle;
    use std::sync::Arc;
    use std::time::Duration;

    fn ctx_with(perms: FacePermissions) -> (FaceContext, LoopbackBackend) {
        let backend = LoopbackBackend::new();
        let caps = backend.capabilities();
        let arc: Arc<dyn RadioBackend> = Arc::new(backend.clone());
        let state = StateHandle::new();
        let radio = spawn_scheduler(arc, detached_link(), state.clone());
        let ptt = PttManager::new(Duration::from_secs(300));
        let ctx = FaceContext::new(7, perms, state, radio, ptt, caps);
        (ctx, backend)
    }

    #[tokio::test]
    async fn if_response_contains_frequency() {
        let (ctx, _b) = ctx_with(FacePermissions::read_only());
        ctx.state.record(
            StateChange::Freq {
                vfo: Vfo::A,
                hz: 14_074_000,
            },
            RadioEventSource::PollDiff,
        );
        let reply = Ts2000Dialect::new().handle(b"IF;", &ctx).await;
        assert!(reply.starts_with(b"IF00014074000"));
        assert!(reply.ends_with(b";"));
        assert_eq!(reply.len(), 38, "Kenwood IF answer is 38 bytes");
    }

    #[tokio::test]
    async fn rejects_vfo_target_write() {
        let (ctx, backend) = ctx_with(FacePermissions::from_tokens(&["read", "write"]));
        let reply = Ts2000Dialect::new().handle(b"FR1;", &ctx).await;
        assert_eq!(reply, ERR.to_vec());
        tokio::time::sleep(Duration::from_millis(20)).await;
        // Crucially: no split mutation reached the radio.
        assert!(backend.mutations().is_empty());
    }

    #[tokio::test]
    async fn rx_vfo_change_notifies_with_current_if_status() {
        let (ctx, _backend) = ctx_with(FacePermissions::read_only());
        ctx.set_ai(true);
        ctx.state.record(
            StateChange::Freq {
                vfo: Vfo::B,
                hz: 14_034_320,
            },
            RadioEventSource::NativePush,
        );
        ctx.state.record(
            StateChange::Mode {
                vfo: Vfo::B,
                mode: crate::model::Mode::Usb,
            },
            RadioEventSource::NativePush,
        );
        ctx.state.record(
            StateChange::RxVfo { vfo: Vfo::B },
            RadioEventSource::NativePush,
        );

        let notification = Ts2000Dialect::new()
            .format_notification(&StateChange::RxVfo { vfo: Vfo::B }, &ctx)
            .expect("IF notification");

        assert!(notification.starts_with(b"IF00014034320"));
        assert_eq!(
            *notification.get(30).expect("VFO field"),
            b'1',
            "TS-2000 IF VFO field should report VFO B"
        );
    }

    #[tokio::test]
    async fn answers_id_as_ts2000() {
        let (ctx, _b) = ctx_with(FacePermissions::read_only());
        assert_eq!(
            Ts2000Dialect::new().handle(b"ID;", &ctx).await,
            b"ID019;".to_vec()
        );
    }

    #[tokio::test]
    async fn ai_read_reports_state_without_changing_it() {
        let (ctx, _b) = ctx_with(FacePermissions::read_only());
        assert_eq!(
            Ts2000Dialect::new().handle(b"AI;", &ctx).await,
            b"AI0;".to_vec()
        );
        assert!(!ctx.ai_on());
        assert!(Ts2000Dialect::new().handle(b"AI2;", &ctx).await.is_empty());
        assert_eq!(
            Ts2000Dialect::new().handle(b"AI;", &ctx).await,
            b"AI2;".to_vec()
        );
        assert!(ctx.ai_on(), "AI; read must not disable auto-info");
    }

    #[tokio::test]
    async fn reads_frequency_from_state() {
        let (ctx, _b) = ctx_with(FacePermissions::read_only());
        ctx.state.record(
            StateChange::Freq {
                vfo: Vfo::A,
                hz: 21_200_000,
            },
            RadioEventSource::PollDiff,
        );
        assert_eq!(
            Ts2000Dialect::new().handle(b"FA;", &ctx).await,
            b"FA00021200000;".to_vec()
        );
    }
}

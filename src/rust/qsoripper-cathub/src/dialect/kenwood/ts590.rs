//! The native Kenwood TS-590 client dialect (N1MM Logger+, ARCP-590, Log4OM-as-TS590).
//!
//! Modeled reads (`FA`/`FB`/`MD`/`IF`) are served from the universal state cache, so a
//! flurry of client polls never touches the radio. Modeled writes route through
//! [`FaceContext::apply_modeled`] for serialization, permission checks, and the PTT lease.
//! `AI` is virtualized per face. Any unmodeled native command falls through to a
//! permission-gated passthrough so genuine native features still work on a certified rig.

use async_trait::async_trait;

use super::{freq_frame, mode_from_digit, mode_to_digit, parse_command, ERR};
use crate::dialect::{ApplyOutcome, ClientDialect, FaceContext};
use crate::model::{StateChange, StateMutation, Vfo};
use crate::permissions::CommandClass;
use crate::state::Snapshot;

/// The native TS-590 dialect.
#[derive(Clone, Default)]
pub(crate) struct Ts590Dialect;

impl Ts590Dialect {
    /// Create the dialect.
    pub(crate) fn new() -> Self {
        Ts590Dialect
    }
}

/// Synthesize a Kenwood `IF;` status answer (38 bytes) from the universal state.
fn synth_if(snapshot: &Snapshot) -> Vec<u8> {
    let freq = snapshot.vfo(snapshot.rx_vfo).freq_hz;
    let tx = u8::from(snapshot.ptt);
    let mode = mode_to_digit(snapshot.vfo(snapshot.rx_vfo).mode) - b'0';
    let split = u8::from(snapshot.split);
    let rx_vfo = match snapshot.rx_vfo {
        Vfo::A => 0u8,
        Vfo::B => 1u8,
    };
    format!("IF{freq:011}0000+0000000000{tx}{mode}{rx_vfo}0{split}0000;").into_bytes()
}

#[async_trait]
impl ClientDialect for Ts590Dialect {
    async fn handle(&self, request: &[u8], ctx: &FaceContext) -> Vec<u8> {
        let Some((verb, payload)) = parse_command(request) else {
            return ERR.to_vec();
        };
        let read = payload.is_empty();
        match verb.as_slice() {
            b"FA" => {
                if read {
                    freq_frame(b"FA", ctx.snapshot().vfo(Vfo::A).freq_hz)
                } else {
                    set_freq(ctx, Vfo::A, &payload).await
                }
            }
            b"FB" => {
                if read {
                    freq_frame(b"FB", ctx.snapshot().vfo(Vfo::B).freq_hz)
                } else {
                    set_freq(ctx, Vfo::B, &payload).await
                }
            }
            b"MD" => {
                if read {
                    let d = mode_to_digit(ctx.snapshot().vfo(Vfo::A).mode);
                    vec![b'M', b'D', d, b';']
                } else {
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
            }
            b"TX" => reply(
                ctx.apply_modeled(
                    StateMutation::SetPtt { keyed: true },
                    CommandClass::PttWrite,
                )
                .await,
            ),
            b"RX" => reply(
                ctx.apply_modeled(
                    StateMutation::SetPtt { keyed: false },
                    CommandClass::PttWrite,
                )
                .await,
            ),
            b"AI" => {
                // Auto-information is virtualized per face: it never reaches the radio.
                let on = payload.first().is_some_and(|&d| d != b'0');
                ctx.set_ai(on);
                Vec::new()
            }
            b"ID" if read => b"ID021;".to_vec(),
            b"PS" if read => b"PS1;".to_vec(),
            b"IF" if read => synth_if(&ctx.snapshot()),
            // Any other native command is forwarded as a permission-gated passthrough.
            _ => {
                let class = if read {
                    CommandClass::PassthroughRead
                } else {
                    CommandClass::ConfigWrite
                };
                ctx.passthrough(request, class).await
            }
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
            _ => None,
        }
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
    use crate::model::{Mode, RadioEventSource};
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
        (FaceContext::new(5, perms, state, radio, ptt, caps), backend)
    }

    #[tokio::test]
    async fn reads_frequency_from_cache() {
        let (ctx, _b) = ctx_with(FacePermissions::read_only());
        ctx.state.record(
            StateChange::Freq {
                vfo: Vfo::A,
                hz: 7_074_000,
            },
            RadioEventSource::PollDiff,
        );
        assert_eq!(
            Ts590Dialect::new().handle(b"FA;", &ctx).await,
            b"FA00007074000;".to_vec()
        );
    }

    #[tokio::test]
    async fn write_is_denied_for_read_only_face() {
        let (ctx, backend) = ctx_with(FacePermissions::read_only());
        assert_eq!(
            Ts590Dialect::new().handle(b"FA00007050000;", &ctx).await,
            ERR.to_vec()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(backend.mutations().is_empty());
    }

    #[tokio::test]
    async fn mode_read_and_write() {
        let (ctx, backend) = ctx_with(FacePermissions::from_tokens(&["read", "write"]));
        assert_eq!(
            Ts590Dialect::new().handle(b"MD;", &ctx).await,
            b"MD2;".to_vec(),
            "default mode is USB digit 2"
        );
        assert_eq!(
            Ts590Dialect::new().handle(b"MD3;", &ctx).await,
            Vec::<u8>::new()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(
            backend.mutations(),
            vec![StateMutation::SetMode {
                vfo: Vfo::A,
                mode: Mode::Cw
            }]
        );
    }

    #[tokio::test]
    async fn ai_toggle_is_virtualized_and_drives_notifications() {
        let (ctx, _b) = ctx_with(FacePermissions::read_only());
        assert!(Ts590Dialect::new().handle(b"AI2;", &ctx).await.is_empty());
        assert!(ctx.ai_on());
        let note = Ts590Dialect::new().format_notification(
            &StateChange::Freq {
                vfo: Vfo::A,
                hz: 14_074_000,
            },
            &ctx,
        );
        assert_eq!(note, Some(b"FA00014074000;".to_vec()));
    }

    #[tokio::test]
    async fn id_and_ps_identify_as_ts590() {
        let (ctx, _b) = ctx_with(FacePermissions::read_only());
        assert_eq!(
            Ts590Dialect::new().handle(b"ID;", &ctx).await,
            b"ID021;".to_vec()
        );
        assert_eq!(
            Ts590Dialect::new().handle(b"PS;", &ctx).await,
            b"PS1;".to_vec()
        );
    }

    #[tokio::test]
    async fn if_answer_is_38_bytes() {
        let (ctx, _b) = ctx_with(FacePermissions::read_only());
        let reply = Ts590Dialect::new().handle(b"IF;", &ctx).await;
        assert_eq!(reply.len(), 38);
        assert!(reply.starts_with(b"IF"));
    }

    #[tokio::test]
    async fn unmodeled_set_requires_config_write() {
        let (ctx, backend) = ctx_with(FacePermissions::from_tokens(&["read", "write"]));
        // No config_write permission: an EX-menu set is refused, never forwarded.
        assert_eq!(
            Ts590Dialect::new().handle(b"EX0050000;", &ctx).await,
            ERR.to_vec()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(backend.passthroughs().is_empty());
    }

    #[tokio::test]
    async fn passthrough_read_is_forwarded_when_permitted() {
        let (ctx, backend) = ctx_with(FacePermissions::from_tokens(&["read"]));
        let reply = Ts590Dialect::new().handle(b"RM;", &ctx).await;
        // The loopback backend echoes the raw passthrough.
        assert_eq!(reply, b"RM;".to_vec());
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(backend.passthroughs(), vec![b"RM;".to_vec()]);
    }
}

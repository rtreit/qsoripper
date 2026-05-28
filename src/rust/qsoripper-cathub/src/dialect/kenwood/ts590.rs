//! TS-590 client dialect. Serves native Kenwood clients such as N1MM: status
//! reads are answered from cached universal state, writes become universal
//! mutations, PTT is permission-gated, and anything unmodeled passes through to
//! the radio.

use async_trait::async_trait;

use crate::backend::StateMutation;
use crate::dialect::{ClientDialect, FaceContext};
use crate::model::{Mode, Vfo};
use crate::state::{StateChange, StateHandle};

/// Native Kenwood TS-590 dialect.
pub(crate) struct Ts590Dialect;

impl Ts590Dialect {
    /// Construct the TS-590 dialect.
    pub(crate) fn new() -> Self {
        Self
    }

    async fn read_reply(verb: &[u8], state: &StateHandle) -> Option<Vec<u8>> {
        let snap = state.snapshot().await;
        match verb {
            b"FA" => Some(format_freq(b"FA", snap.freq(Vfo::A))),
            b"FB" => Some(format_freq(b"FB", snap.freq(Vfo::B))),
            b"MD" => Some(format_mode(snap.mode(snap.rx_vfo))),
            _ => None,
        }
    }

    async fn apply_write(verb: &[u8], payload: &[u8], ctx: &FaceContext) {
        if !ctx.permissions().allow_write {
            tracing::warn!(face = ctx.name(), "dropping write: face is read-only");
            return;
        }
        let mutation =
            match verb {
                b"FA" => parse_freq_payload(payload)
                    .map(|hz| StateMutation::Frequency { vfo: Vfo::A, hz }),
                b"FB" => parse_freq_payload(payload)
                    .map(|hz| StateMutation::Frequency { vfo: Vfo::B, hz }),
                b"MD" => parse_mode_payload(payload)
                    .map(|mode| StateMutation::Mode { vfo: Vfo::A, mode }),
                _ => None,
            };
        if let Some(mutation) = mutation {
            if let Err(error) = ctx.state().apply_mutation(mutation).await {
                tracing::warn!(face = ctx.name(), %error, "write failed");
            }
        }
    }

    async fn route_ptt(keyed: bool, ctx: &FaceContext) {
        if !ctx.permissions().allow_ptt {
            tracing::warn!(face = ctx.name(), keyed, "dropping PTT: face may not key");
            return;
        }
        if let Err(error) = ctx
            .state()
            .apply_mutation(StateMutation::Ptt { keyed })
            .await
        {
            tracing::warn!(face = ctx.name(), keyed, %error, "PTT routing failed");
        }
    }
}

#[async_trait]
impl ClientDialect for Ts590Dialect {
    async fn handle(&self, request: &[u8], ctx: &FaceContext) -> Vec<u8> {
        let Some((verb, payload)) = split_command(request) else {
            return Vec::new();
        };

        match verb.as_slice() {
            b"FA" | b"FB" | b"MD" if payload.is_empty() => Self::read_reply(&verb, ctx.state())
                .await
                .unwrap_or_default(),
            b"FA" | b"FB" | b"MD" => {
                Self::apply_write(&verb, &payload, ctx).await;
                Vec::new()
            }
            b"TX" => {
                Self::route_ptt(true, ctx).await;
                Vec::new()
            }
            b"RX" => {
                Self::route_ptt(false, ctx).await;
                Vec::new()
            }
            b"AI" if payload.is_empty() => {
                let level = u8::from(ctx.ai_enabled());
                format!("AI{level};").into_bytes()
            }
            b"AI" => {
                let enabled = payload.first().is_some_and(|&b| b != b'0');
                ctx.set_ai_enabled(enabled);
                Vec::new()
            }
            _ => {
                if ctx.permissions().allow_passthrough {
                    ctx.backend().passthrough(request).await.unwrap_or_default()
                } else {
                    tracing::warn!(face = ctx.name(), "dropping passthrough: not permitted");
                    Vec::new()
                }
            }
        }
    }

    fn format_notification(&self, change: &StateChange, ctx: &FaceContext) -> Option<Vec<u8>> {
        if !ctx.ai_enabled() {
            return None;
        }
        match change {
            StateChange::Frequency { vfo: Vfo::A, hz } => Some(format_freq(b"FA", *hz)),
            StateChange::Frequency { vfo: Vfo::B, hz } => Some(format_freq(b"FB", *hz)),
            StateChange::Mode { vfo: Vfo::A, mode } => Some(format_mode(*mode)),
            StateChange::Mode { vfo: Vfo::B, .. } => None,
        }
    }
}

fn format_freq(verb: &[u8], hz: u64) -> Vec<u8> {
    let mut out = verb.to_vec();
    out.extend_from_slice(format!("{hz:011};").as_bytes());
    out
}

fn format_mode(mode: Mode) -> Vec<u8> {
    format!("MD{};", mode.to_kenwood_digit()).into_bytes()
}

/// Split a terminated command into its alphabetic verb and remaining payload.
fn split_command(request: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    let body = request.strip_suffix(b";").unwrap_or(request);
    if body.is_empty() {
        return None;
    }
    let verb_len = body.iter().take_while(|b| b.is_ascii_alphabetic()).count();
    if verb_len == 0 {
        return None;
    }
    let (verb, payload) = body.split_at(verb_len);
    Some((verb.to_vec(), payload.to_vec()))
}

fn parse_freq_payload(payload: &[u8]) -> Option<u64> {
    if payload.len() != 11 || !payload.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(payload).ok()?.parse::<u64>().ok()
}

fn parse_mode_payload(payload: &[u8]) -> Option<Mode> {
    let &[digit] = payload else {
        return None;
    };
    Mode::from_kenwood_digit(digit as char)
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
    use crate::dialect::Permissions;
    use crate::state::run_mutation_dispatcher;
    use std::sync::Arc;

    fn wired(
        permissions: Permissions,
    ) -> (
        Ts590Dialect,
        FaceContext,
        Arc<LoopbackBackend>,
        tokio::task::JoinHandle<()>,
    ) {
        let (state, inbox) = StateHandle::new(16);
        let backend = Arc::new(LoopbackBackend::new());
        let dispatcher = tokio::spawn(run_mutation_dispatcher(
            inbox,
            backend.clone(),
            state.clone(),
        ));
        let ctx = FaceContext::new("test", permissions, state, backend.clone());
        (Ts590Dialect::new(), ctx, backend, dispatcher)
    }

    #[tokio::test]
    async fn read_serves_cached_frequency() {
        let (dialect, ctx, _backend, dispatcher) = wired(Permissions::default());
        ctx.state().set_frequency(Vfo::A, 14_074_000).await;
        let reply = dialect.handle(b"FA;", &ctx).await;
        assert_eq!(reply, b"FA00014074000;".to_vec());
        dispatcher.abort();
    }

    #[tokio::test]
    async fn write_applies_mutation_through_backend() {
        let (dialect, ctx, backend, dispatcher) = wired(Permissions::default());
        let reply = dialect.handle(b"FA00021074000;", &ctx).await;
        assert!(reply.is_empty());
        assert_eq!(ctx.state().snapshot().await.freq_a, 21_074_000);
        assert_eq!(backend.recorded_mutations().len(), 1);
        dispatcher.abort();
    }

    #[tokio::test]
    async fn ptt_dropped_without_permission() {
        let permissions = Permissions {
            allow_ptt: false,
            ..Permissions::default()
        };
        let (dialect, ctx, backend, dispatcher) = wired(permissions);
        let reply = dialect.handle(b"TX;", &ctx).await;
        assert!(reply.is_empty());
        assert!(backend.recorded_mutations().is_empty());
        dispatcher.abort();
    }

    #[tokio::test]
    async fn ptt_routed_with_permission() {
        let permissions = Permissions {
            allow_ptt: true,
            ..Permissions::default()
        };
        let (dialect, ctx, backend, dispatcher) = wired(permissions);
        dialect.handle(b"TX;", &ctx).await;
        assert_eq!(
            backend.recorded_mutations(),
            vec![StateMutation::Ptt { keyed: true }]
        );
        dispatcher.abort();
    }

    #[tokio::test]
    async fn unmodeled_command_passes_through() {
        let (dialect, ctx, backend, dispatcher) = wired(Permissions::default());
        let reply = dialect.handle(b"PS;", &ctx).await;
        assert!(reply.is_empty());
        assert_eq!(backend.recorded_passthroughs(), vec![b"PS;".to_vec()]);
        dispatcher.abort();
    }

    #[tokio::test]
    async fn ai_toggle_round_trips() {
        let (dialect, ctx, _backend, dispatcher) = wired(Permissions::default());
        assert_eq!(dialect.handle(b"AI;", &ctx).await, b"AI0;".to_vec());
        dialect.handle(b"AI1;", &ctx).await;
        assert!(ctx.ai_enabled());
        assert_eq!(dialect.handle(b"AI;", &ctx).await, b"AI1;".to_vec());
        dispatcher.abort();
    }

    #[tokio::test]
    async fn notification_only_when_ai_enabled() {
        let (dialect, ctx, _backend, dispatcher) = wired(Permissions::default());
        let change = StateChange::Frequency {
            vfo: Vfo::A,
            hz: 14_074_000,
        };
        assert!(dialect.format_notification(&change, &ctx).is_none());
        ctx.set_ai_enabled(true);
        assert_eq!(
            dialect.format_notification(&change, &ctx),
            Some(b"FA00014074000;".to_vec())
        );
        dispatcher.abort();
    }
}

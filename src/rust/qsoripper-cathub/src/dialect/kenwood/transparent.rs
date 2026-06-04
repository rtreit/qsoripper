//! The transparent mirror dialect for native TS-590 controllers (notably ARCP-590).
//!
//! Unlike [`Ts590Dialect`](super::ts590::Ts590Dialect), which virtualizes the radio behind a
//! modeled cache, a transparent face behaves as if it were wired directly to the rig: every
//! request except PTT and auto-information is forwarded to the radio verbatim, and the
//! radio's entire CAT stream is relayed back verbatim. The rig runs in AI2, so it echoes
//! every change any client makes through the hub — which is exactly what keeps a transparent
//! controller perfectly in sync. This removes the whole class of synthesis/drift bugs (stale
//! A/B, frozen frequency) for a client that already speaks the radio's exact protocol.
//!
//! The hub still owns the single physical port, so a few things stay hub-mediated:
//!   * PTT (`TX`/`RX`) routes through the shared lease so a mirror client can never key the
//!     transmitter while another face owns it (design §8.5).
//!   * Auto-information (`AI`) is virtualized per face; the radio itself stays in AI2, and the
//!     hub gates this face's fan-out on its own `AI` flag.
//!   * Identity/keepalive reads (`ID`/`PS`) are answered locally so a mirror client's steady
//!     heartbeat never generates radio traffic.

use async_trait::async_trait;

use super::ts590::snapshot_resync_frames;
use super::{ai_frame, parse_command, ERR};
use crate::dialect::{ApplyOutcome, ClientDialect, FaceContext};
use crate::model::{PttSource, StateChange, StateMutation};
use crate::permissions::CommandClass;
use crate::state::Snapshot;

/// The transparent TS-590 mirror dialect.
#[derive(Clone, Default)]
pub(crate) struct TransparentTs590Dialect;

impl TransparentTs590Dialect {
    /// Create the dialect.
    pub(crate) fn new() -> Self {
        TransparentTs590Dialect
    }
}

#[async_trait]
impl ClientDialect for TransparentTs590Dialect {
    async fn handle(&self, request: &[u8], ctx: &FaceContext) -> Vec<u8> {
        let Some((verb, payload)) = parse_command(request) else {
            return ERR.to_vec();
        };
        let read = payload.is_empty();
        match verb.as_slice() {
            // PTT stays hub-arbitrated even on a mirror face: a transparent client must not be
            // able to key the rig while another face owns the transmitter.
            b"TX" => reply(
                ctx.apply_modeled(
                    StateMutation::SetPtt {
                        keyed: true,
                        source: PttSource::Generic,
                    },
                    CommandClass::PttWrite,
                )
                .await,
            ),
            b"RX" => reply(
                ctx.apply_modeled(
                    StateMutation::SetPtt {
                        keyed: false,
                        source: PttSource::Generic,
                    },
                    CommandClass::PttWrite,
                )
                .await,
            ),
            // Auto-information is virtualized per face: the radio is already in AI2 (hub-owned
            // native push), so a mirror client toggling AI only flips its own fan-out flag.
            b"AI" => {
                if read {
                    ai_frame(ctx.ai_on())
                } else {
                    ctx.set_ai(payload.first().is_some_and(|&d| d != b'0'));
                    Vec::new()
                }
            }
            // Identity/keepalive reads are answered locally so a mirror client's heartbeat
            // never hits the radio. (ARCP-590 polls `PS;`/`AI;` continuously when idle.)
            b"ID" if read => b"ID021;".to_vec(),
            b"PS" if read => b"PS1;".to_vec(),
            // Everything else is forwarded to the radio verbatim: a transparent face behaves
            // as if wired directly to the rig, so reads and writes alike reach the real radio.
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

    fn format_notification(&self, _change: &StateChange, _ctx: &FaceContext) -> Option<Vec<u8>> {
        // A transparent face is driven entirely by the radio's verbatim CAT stream
        // (`format_native_passthrough` / `format_passthrough`); it never consumes a synthesized
        // modeled notification, which is precisely what kept drifting out of sync.
        None
    }

    fn format_passthrough(&self, raw: &[u8], ctx: &FaceContext) -> Option<Vec<u8>> {
        if ctx.ai_on() {
            Some(raw.to_vec())
        } else {
            None
        }
    }

    fn format_native_passthrough(&self, raw: &[u8], ctx: &FaceContext) -> Option<Vec<u8>> {
        if ctx.ai_on() {
            Some(raw.to_vec())
        } else {
            None
        }
    }

    fn resync(&self, snapshot: &Snapshot, ctx: &FaceContext) -> Vec<Vec<u8>> {
        // A lagged mirror face never received the raw frames it missed and ignores synthesized
        // notifications, so re-present the current radio state as a full set of raw frames.
        if ctx.ai_on() {
            snapshot_resync_frames(snapshot)
        } else {
            Vec::new()
        }
    }
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
    use crate::model::{RadioEventSource, StateChange, Vfo};
    use crate::permissions::FacePermissions;
    use crate::ptt::PttManager;
    use crate::radio::{detached_link, spawn_scheduler};
    use crate::state::StateHandle;
    use std::sync::Arc;
    use std::time::Duration;

    fn ctx_with(perms: FacePermissions) -> (FaceContext, LoopbackBackend, StateHandle, PttManager) {
        let backend = LoopbackBackend::new();
        let caps = backend.capabilities();
        let arc: Arc<dyn RadioBackend> = Arc::new(backend.clone());
        let state = StateHandle::new();
        let radio = spawn_scheduler(arc, detached_link(), state.clone());
        let ptt = PttManager::new(Duration::from_secs(300));
        (
            FaceContext::new(1, perms, state.clone(), radio, ptt.clone(), caps),
            backend,
            state,
            ptt,
        )
    }

    #[tokio::test]
    async fn forwards_a_vfo_select_to_the_radio_verbatim() {
        // The core bug fix: an A/B (`FR1;`) from a mirror client must reach the radio raw, not
        // be swallowed by virtualization.
        let (ctx, backend, _state, _ptt) = ctx_with(FacePermissions::from_tokens(&[
            "read",
            "write",
            "config_write",
        ]));
        let reply = TransparentTs590Dialect::new().handle(b"FR1;", &ctx).await;
        // Loopback echoes the passthrough; production fire-and-forget writes return empty.
        assert_eq!(reply, b"FR1;");
        assert_eq!(backend.passthroughs(), vec![b"FR1;".to_vec()]);
    }

    #[tokio::test]
    async fn forwards_a_frequency_read_to_the_radio() {
        let (ctx, backend, _state, _ptt) = ctx_with(FacePermissions::from_tokens(&[
            "read",
            "write",
            "config_write",
        ]));
        let _ = TransparentTs590Dialect::new().handle(b"FA;", &ctx).await;
        assert_eq!(backend.passthroughs(), vec![b"FA;".to_vec()]);
    }

    #[tokio::test]
    async fn keys_and_unkeys_through_the_ptt_lease() {
        let (ctx, _backend, _state, ptt) = ctx_with(FacePermissions::from_tokens(&["read", "ptt"]));
        let dialect = TransparentTs590Dialect::new();
        assert!(dialect.handle(b"TX;", &ctx).await.is_empty());
        assert_eq!(ptt.owner(), Some(1), "TX must take the shared PTT lease");
        assert!(dialect.handle(b"RX;", &ctx).await.is_empty());
        assert_eq!(ptt.owner(), None, "RX must release the shared PTT lease");
    }

    #[tokio::test]
    async fn ptt_command_never_reaches_the_radio_as_a_raw_write() {
        // TX/RX must be modeled (lease-arbitrated), not passed through as raw bytes.
        let (ctx, backend, _state, _ptt) = ctx_with(FacePermissions::from_tokens(&["read", "ptt"]));
        let _ = TransparentTs590Dialect::new().handle(b"TX;", &ctx).await;
        assert!(
            backend.passthroughs().is_empty(),
            "PTT must not be forwarded as a raw passthrough"
        );
    }

    #[tokio::test]
    async fn keepalive_reads_are_answered_locally() {
        let (ctx, backend, _state, _ptt) = ctx_with(FacePermissions::read_only());
        let dialect = TransparentTs590Dialect::new();
        assert_eq!(dialect.handle(b"ID;", &ctx).await, b"ID021;");
        assert_eq!(dialect.handle(b"PS;", &ctx).await, b"PS1;");
        assert!(
            backend.passthroughs().is_empty(),
            "ID/PS keepalives must not generate radio traffic"
        );
    }

    #[tokio::test]
    async fn auto_information_is_virtualized_per_face() {
        let (ctx, backend, _state, _ptt) = ctx_with(FacePermissions::read_only());
        let dialect = TransparentTs590Dialect::new();
        assert_eq!(dialect.handle(b"AI;", &ctx).await, b"AI0;");
        assert!(dialect.handle(b"AI2;", &ctx).await.is_empty());
        assert!(ctx.ai_on());
        assert_eq!(dialect.handle(b"AI;", &ctx).await, b"AI2;");
        assert!(
            backend.passthroughs().is_empty(),
            "AI must never reach the radio; the rig stays in hub-owned AI2"
        );
    }

    #[tokio::test]
    async fn relays_modeled_and_unmodeled_frames_verbatim_when_auto_info_on() {
        let (ctx, _backend, _state, _ptt) = ctx_with(FacePermissions::read_only());
        let dialect = TransparentTs590Dialect::new();
        // AI off: suppress everything.
        assert_eq!(dialect.format_native_passthrough(b"FR1;", &ctx), None);
        assert_eq!(dialect.format_passthrough(b"NB1;", &ctx), None);
        ctx.set_ai(true);
        // AI on: relay both the modeled CAT echo and the unmodeled frame byte-for-byte.
        assert_eq!(
            dialect.format_native_passthrough(b"FA00014035000;", &ctx),
            Some(b"FA00014035000;".to_vec())
        );
        assert_eq!(
            dialect.format_passthrough(b"NB1;", &ctx),
            Some(b"NB1;".to_vec())
        );
    }

    #[tokio::test]
    async fn never_synthesizes_a_modeled_notification() {
        let (ctx, _backend, _state, _ptt) = ctx_with(FacePermissions::read_only());
        ctx.set_ai(true);
        let dialect = TransparentTs590Dialect::new();
        assert_eq!(
            dialect.format_notification(
                &StateChange::Freq {
                    vfo: Vfo::A,
                    hz: 14_035_000,
                },
                &ctx
            ),
            None,
            "a transparent face must never emit a synthesized notification"
        );
    }

    #[tokio::test]
    async fn resync_re_presents_full_state_after_a_lag() {
        let (ctx, _backend, state, _ptt) = ctx_with(FacePermissions::read_only());
        ctx.set_ai(true);
        state.record(
            StateChange::Freq {
                vfo: Vfo::A,
                hz: 14_035_000,
            },
            RadioEventSource::PollDiff,
        );
        let frames = TransparentTs590Dialect::new().resync(&state.snapshot(), &ctx);
        assert!(
            frames.iter().any(|f| f == b"FA00014035000;"),
            "re-sync must include the current VFO A frequency, got {frames:?}"
        );
        assert!(
            frames.iter().any(|f| f.starts_with(b"IF")),
            "re-sync must include the operating-status IF frame"
        );
    }

    #[tokio::test]
    async fn resync_is_empty_when_auto_info_off() {
        let (ctx, _backend, state, _ptt) = ctx_with(FacePermissions::read_only());
        assert!(
            TransparentTs590Dialect::new()
                .resync(&state.snapshot(), &ctx)
                .is_empty(),
            "an AI-off mirror face must emit nothing on re-sync"
        );
    }
}

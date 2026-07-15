//! Client dialects and the per-session execution context.
//!
//! A [`ClientDialect`] translates one client's CAT vocabulary to and from the neutral
//! state, serving reads from the cache and routing writes through [`ClientSessionContext`]. The
//! context carries the session's identity, inherited endpoint permissions, the shared scheduler/PTT lease, and
//! its own virtualized auto-information flag, so every write participates in serialization,
//! permission checks, the single-owner PTT lease, and event fan-out (design §8).

pub(crate) mod kenwood;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use async_trait::async_trait;

use crate::backend::{BackendCapabilities, BackendError};
use crate::model::{StateChange, StateMutation};
use crate::permissions::{CommandClass, EndpointPermissions};
use crate::ptt::{PttDenied, PttManager};
use crate::radio::{OpKind, Priority, RadioHandle};
use crate::state::{Snapshot, StateHandle};

/// The Kenwood error reply, used when a modeled write or passthrough is refused.
const ERR_REPLY: &[u8] = b"?;";

/// The result of applying a modeled write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ApplyOutcome {
    /// The write was accepted and dispatched.
    Ok,
    /// The client session lacks permission for this command class.
    Denied,
    /// PTT is held by another client session.
    Busy,
    /// The backend failed to apply the write.
    Error,
    /// The backend does not support this operation.
    Unsupported,
}

/// One client session's execution context: identity, permissions, capabilities, and the shared
/// state/scheduler/PTT lease, plus its own auto-information toggle.
#[derive(Clone)]
pub(crate) struct ClientSessionContext {
    /// The unique session id (used for PTT ownership and scheduler fairness).
    pub(crate) session_id: u64,
    /// Permissions inherited from this session's configured endpoint.
    pub(crate) perms: EndpointPermissions,
    /// The backend's advertised capabilities.
    pub(crate) caps: BackendCapabilities,
    /// The shared universal state.
    pub(crate) state: StateHandle,
    /// The shared priority scheduler handle.
    pub(crate) radio: RadioHandle,
    /// The shared PTT lease.
    pub(crate) ptt: PttManager,
    /// This session's virtualized auto-information flag (never reaches the radio).
    ai: Arc<AtomicBool>,
    /// When true, present the *operating* VFO as VFO A to this endpoint (operating-VFO
    /// virtualization). Set from `[[serial_endpoint]] single_vfo` for single-VFO loggers such as
    /// N1MM SO1V; left false for true dual-VFO control endpoints such as ARCP-590.
    single_vfo: bool,
}

impl ClientSessionContext {
    /// Create a client session context.
    pub(crate) fn new(
        session_id: u64,
        perms: EndpointPermissions,
        state: StateHandle,
        radio: RadioHandle,
        ptt: PttManager,
        caps: BackendCapabilities,
    ) -> Self {
        ClientSessionContext {
            session_id,
            perms,
            caps,
            state,
            radio,
            ptt,
            ai: Arc::new(AtomicBool::new(false)),
            single_vfo: false,
        }
    }

    /// Enable operating-VFO virtualization for this session (builder style so existing
    /// call sites are unaffected). When enabled, the client always sees the operating VFO
    /// presented as VFO A. Returns `self` for chaining.
    pub(crate) fn with_single_vfo(mut self, single_vfo: bool) -> Self {
        self.single_vfo = single_vfo;
        self
    }

    /// A consistent point-in-time view of the radio.
    pub(crate) fn snapshot(&self) -> Snapshot {
        self.state.snapshot()
    }

    /// Apply a modeled write, enforcing permissions, the PTT lease, and serialization.
    pub(crate) async fn apply_modeled(
        &self,
        mutation: StateMutation,
        class: CommandClass,
    ) -> ApplyOutcome {
        if !self.perms.allows_mutation(class, &mutation) {
            return ApplyOutcome::Denied;
        }
        // Idempotent suppression: never re-send a value the radio already holds. This keeps
        // the hub as quiet on the wire as a native Hamlib driver and avoids the TS-590
        // PC-control beep that fires on every redundant set. PTT is never redundant.
        if self.state.is_redundant(&mutation) {
            return ApplyOutcome::Ok;
        }
        match (class, mutation) {
            (CommandClass::PttWrite, StateMutation::SetPtt { keyed: true, .. }) => {
                match self.ptt.try_key(self.session_id, self.perms.ptt) {
                    Err(PttDenied::Busy) => return ApplyOutcome::Busy,
                    Err(PttDenied::NotPermitted) => return ApplyOutcome::Denied,
                    Ok(()) => {}
                }
                if self
                    .radio
                    .submit(self.session_id, Priority::Ptt, OpKind::Apply(mutation))
                    .await
                    .is_ok()
                {
                    ApplyOutcome::Ok
                } else {
                    // The key request never reached the radio: release the lease.
                    self.ptt.unkey(self.session_id);
                    ApplyOutcome::Error
                }
            }
            (CommandClass::PttWrite, StateMutation::SetPtt { keyed: false, .. }) => {
                let result = self
                    .radio
                    .submit(self.session_id, Priority::Ptt, OpKind::Apply(mutation))
                    .await;
                self.ptt.unkey(self.session_id);
                map_outcome(&result)
            }
            _ => {
                let priority = match class {
                    CommandClass::PttWrite => Priority::Ptt,
                    _ => Priority::Write,
                };
                map_outcome(
                    &self
                        .radio
                        .submit(self.session_id, priority, OpKind::Apply(mutation))
                        .await,
                )
            }
        }
    }

    /// Forward a raw native command, returning the raw reply (or the error frame).
    pub(crate) async fn passthrough(&self, raw: &[u8], class: CommandClass) -> Vec<u8> {
        if !self.perms.allows(class) {
            return ERR_REPLY.to_vec();
        }
        match self
            .radio
            .submit(
                self.session_id,
                Priority::Read,
                OpKind::Passthrough(raw.to_vec()),
            )
            .await
        {
            Ok(bytes) => bytes,
            Err(_) => ERR_REPLY.to_vec(),
        }
    }

    /// Set this session's virtualized auto-information flag.
    pub(crate) fn set_ai(&self, on: bool) {
        self.ai.store(on, Ordering::SeqCst);
    }

    /// Whether this session has auto-information enabled.
    pub(crate) fn ai_on(&self) -> bool {
        self.ai.load(Ordering::SeqCst)
    }

    /// Whether this session uses operating-VFO virtualization (operating VFO presented as
    /// VFO A). True for single-VFO loggers (N1MM SO1V); false for dual-VFO endpoints.
    pub(crate) fn single_vfo(&self) -> bool {
        self.single_vfo
    }

    /// A clone of this context for a new connection: same shared state/radio/ptt and
    /// permissions, but a distinct session id and a fresh (off) auto-information flag.
    pub(crate) fn clone_for_session(&self, session_id: u64) -> ClientSessionContext {
        ClientSessionContext {
            session_id,
            perms: self.perms,
            caps: self.caps.clone(),
            state: self.state.clone(),
            radio: self.radio.clone(),
            ptt: self.ptt.clone(),
            ai: Arc::new(AtomicBool::new(false)),
            // A new connection to the same endpoint keeps that endpoint's VFO-presentation policy.
            single_vfo: self.single_vfo,
        }
    }

    /// Release the PTT lease if this session currently holds it, unkeying the radio first.
    ///
    /// Called when a client session's transport closes. A client that keys the transmitter and then
    /// disconnects (crash, cable pull, app kill) would otherwise leave the radio keyed until
    /// the `ptt_max_tx_ms` safety ceiling fires — minutes of unintended transmission. This
    /// drops TX immediately on disconnect (design §8.5), mirroring the orderly-shutdown path.
    pub(crate) async fn release_ptt_on_disconnect(&self) {
        if self.ptt.owner() != Some(self.session_id) {
            return;
        }
        let _ = self
            .radio
            .submit(
                self.session_id,
                Priority::Ptt,
                OpKind::Apply(StateMutation::SetPtt {
                    keyed: false,
                    source: crate::model::PttSource::Generic,
                }),
            )
            .await;
        self.ptt.unkey(self.session_id);
        tracing::warn!(
            session_id = self.session_id,
            "client disconnected while keyed; transmitter released"
        );
    }
}

fn map_outcome(result: &Result<Vec<u8>, BackendError>) -> ApplyOutcome {
    match result {
        Ok(_) => ApplyOutcome::Ok,
        Err(BackendError::Unsupported) => ApplyOutcome::Unsupported,
        Err(_) => ApplyOutcome::Error,
    }
}

/// A client CAT dialect: translates a client's vocabulary to/from the neutral state.
#[async_trait]
pub(crate) trait ClientDialect: Send + Sync {
    /// Handle one inbound request frame, returning the reply bytes (possibly empty).
    async fn handle(&self, request: &[u8], ctx: &ClientSessionContext) -> Vec<u8>;

    /// Render a state change as an unsolicited notification for this endpoint, if it applies.
    fn format_notification(
        &self,
        change: &StateChange,
        ctx: &ClientSessionContext,
    ) -> Option<Vec<u8>>;

    /// Render an unsolicited native frame the backend does not model as a notification for
    /// this endpoint, if it applies. Returns the bytes to push, or `None` to suppress it.
    ///
    /// The default suppresses the frame. A native pass-through dialect overrides this to
    /// relay the radio's CAT stream verbatim to clients that have enabled auto-information.
    fn format_passthrough(&self, _raw: &[u8], _ctx: &ClientSessionContext) -> Option<Vec<u8>> {
        None
    }

    /// Render a *modeled* native frame (one the backend parsed into a state change) as a
    /// verbatim relay for this endpoint, if it applies. Returns the bytes to push, or `None`.
    ///
    /// The default suppresses it: a virtualizing endpoint consumes the coalesced
    /// [`StateChange`](crate::model::StateChange) via [`Self::format_notification`] instead.
    /// A transparent mirror dialect overrides this to forward the radio's real CAT stream
    /// byte-for-byte, so the client never diverges from the rig.
    fn format_native_passthrough(
        &self,
        _raw: &[u8],
        _ctx: &ClientSessionContext,
    ) -> Option<Vec<u8>> {
        None
    }

    /// Re-present the full current state to a session that lagged the broadcast ring and lost
    /// one or more events. Returns the frames to write, in order.
    ///
    /// The default replays the snapshot through [`Self::format_notification`], which restores
    /// a virtualizing endpoint to live state. A transparent mirror dialect overrides this to emit
    /// the radio's state as raw frames (it never consumes synthesized notifications).
    fn resync(&self, snapshot: &Snapshot, ctx: &ClientSessionContext) -> Vec<Vec<u8>> {
        snapshot
            .as_changes()
            .iter()
            .filter_map(|change| self.format_notification(change, ctx))
            .collect()
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::backend::loopback::LoopbackBackend;
    use crate::backend::RadioBackend;
    use crate::model::{Mode, PttSource, Vfo};
    use crate::radio::{detached_link, spawn_scheduler};
    use std::time::Duration;

    fn ctx_with(perms: EndpointPermissions, id: u64) -> (ClientSessionContext, LoopbackBackend) {
        let backend = LoopbackBackend::new();
        let caps = backend.capabilities();
        let arc: Arc<dyn RadioBackend> = Arc::new(backend.clone());
        let state = StateHandle::new();
        let radio = spawn_scheduler(arc, detached_link(), state.clone());
        let ptt = PttManager::new(Duration::from_secs(300));
        (
            ClientSessionContext::new(id, perms, state, radio, ptt, caps),
            backend,
        )
    }

    #[tokio::test]
    async fn modeled_write_denied_without_permission() {
        let (ctx, backend) = ctx_with(EndpointPermissions::read_only(), 1);
        let outcome = ctx
            .apply_modeled(
                StateMutation::SetVfoFreq {
                    vfo: Vfo::A,
                    hz: 7_000_000,
                },
                CommandClass::ModeledWrite,
            )
            .await;
        assert_eq!(outcome, ApplyOutcome::Denied);
        assert!(backend.mutations().is_empty());
    }

    #[tokio::test]
    async fn redundant_modeled_write_is_suppressed() {
        let (ctx, backend) = ctx_with(EndpointPermissions::from_tokens(&["read", "write"]), 1);
        // A fresh snapshot already reports VFO A in USB, so re-setting USB must not reach the
        // radio: re-sending an unchanged value is exactly what triggers the TS-590 PC-control
        // beep when a client like WSJT-X re-asserts mode on every poll.
        let outcome = ctx
            .apply_modeled(
                StateMutation::SetMode {
                    vfo: Vfo::A,
                    mode: Mode::Usb,
                },
                CommandClass::ModeledWrite,
            )
            .await;
        assert_eq!(outcome, ApplyOutcome::Ok);
        assert!(backend.mutations().is_empty());

        // A genuine change still reaches the radio.
        let outcome = ctx
            .apply_modeled(
                StateMutation::SetMode {
                    vfo: Vfo::A,
                    mode: Mode::Cw,
                },
                CommandClass::ModeledWrite,
            )
            .await;
        assert_eq!(outcome, ApplyOutcome::Ok);
        assert_eq!(backend.mutations().len(), 1);
    }

    #[tokio::test]
    async fn ptt_write_is_never_suppressed_as_redundant() {
        // Two unkey requests in a row must both reach the radio; PTT is never deduplicated.
        let (ctx, backend) = ctx_with(EndpointPermissions::from_tokens(&["ptt"]), 1);
        for _ in 0..2 {
            let outcome = ctx
                .apply_modeled(
                    StateMutation::SetPtt {
                        keyed: false,
                        source: PttSource::Generic,
                    },
                    CommandClass::PttWrite,
                )
                .await;
            assert_eq!(outcome, ApplyOutcome::Ok);
        }
        assert_eq!(backend.mutations().len(), 2);
    }

    #[tokio::test]
    async fn modeled_write_dispatches_when_allowed() {
        let (ctx, backend) = ctx_with(EndpointPermissions::from_tokens(&["read", "write"]), 1);
        let outcome = ctx
            .apply_modeled(
                StateMutation::SetVfoFreq {
                    vfo: Vfo::A,
                    hz: 7_000_000,
                },
                CommandClass::ModeledWrite,
            )
            .await;
        assert_eq!(outcome, ApplyOutcome::Ok);
        assert_eq!(backend.mutations().len(), 1);
    }

    #[tokio::test]
    async fn frequency_write_does_not_grant_mode_control() {
        let (ctx, backend) = ctx_with(
            EndpointPermissions::from_tokens(&["read", "frequency_write"]),
            1,
        );

        let frequency = StateMutation::SetVfoFreq {
            vfo: Vfo::A,
            hz: 14_074_000,
        };
        assert_eq!(
            ctx.apply_modeled(frequency, CommandClass::ModeledWrite)
                .await,
            ApplyOutcome::Ok
        );

        let mode = StateMutation::SetMode {
            vfo: Vfo::A,
            mode: Mode::Cw,
        };
        assert_eq!(
            ctx.apply_modeled(mode, CommandClass::ModeledWrite).await,
            ApplyOutcome::Denied
        );
        assert_eq!(backend.mutations(), vec![frequency]);
    }

    #[tokio::test]
    async fn ptt_is_busy_for_a_second_session() {
        let (ctx1, _b) = ctx_with(EndpointPermissions::from_tokens(&["ptt"]), 1);
        // Share the same radio/ptt by cloning the context with a new session id.
        let ctx2 = ctx1.clone_for_session(2);
        assert_eq!(
            ctx1.apply_modeled(
                StateMutation::SetPtt {
                    keyed: true,
                    source: PttSource::Generic
                },
                CommandClass::PttWrite
            )
            .await,
            ApplyOutcome::Ok
        );
        assert_eq!(
            ctx2.apply_modeled(
                StateMutation::SetPtt {
                    keyed: true,
                    source: PttSource::Generic
                },
                CommandClass::PttWrite
            )
            .await,
            ApplyOutcome::Busy
        );
    }

    #[tokio::test]
    async fn passthrough_denied_returns_error_frame() {
        let (ctx, _b) = ctx_with(EndpointPermissions::read_only(), 1);
        // A read-only endpoint may not issue a passthrough write.
        assert_eq!(
            ctx.passthrough(b"EX0050000;", CommandClass::ConfigWrite)
                .await,
            b"?;".to_vec()
        );
    }

    #[tokio::test]
    async fn ai_flag_is_per_session_and_resets_on_clone() {
        let (ctx, _b) = ctx_with(EndpointPermissions::read_only(), 1);
        assert!(!ctx.ai_on());
        ctx.set_ai(true);
        assert!(ctx.ai_on());
        let fresh = ctx.clone_for_session(2);
        assert!(
            !fresh.ai_on(),
            "a cloned endpoint starts with auto-info off"
        );
    }
}

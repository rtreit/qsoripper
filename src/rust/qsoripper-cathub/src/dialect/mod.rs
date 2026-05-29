//! Client dialects and the per-face execution context.
//!
//! A [`ClientDialect`] translates one client's CAT vocabulary to and from the neutral
//! state, serving reads from the cache and routing writes through [`FaceContext`]. The
//! context carries the face's identity, permissions, the shared scheduler/PTT lease, and
//! its own virtualized auto-information flag, so every write participates in serialization,
//! permission checks, the single-owner PTT lease, and event fan-out (design §8).

pub(crate) mod kenwood;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use async_trait::async_trait;

use crate::backend::{BackendCapabilities, BackendError};
use crate::model::{StateChange, StateMutation};
use crate::permissions::{CommandClass, FacePermissions};
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
    /// The face lacks permission for this command class.
    Denied,
    /// PTT is held by another face.
    Busy,
    /// The backend failed to apply the write.
    Error,
    /// The backend does not support this operation.
    Unsupported,
}

/// One face's execution context: identity, permissions, capabilities, and the shared
/// state/scheduler/PTT lease, plus its own auto-information toggle.
#[derive(Clone)]
pub(crate) struct FaceContext {
    /// The unique face id (used for PTT ownership and scheduler fairness).
    pub(crate) face_id: u64,
    /// This face's permissions.
    pub(crate) perms: FacePermissions,
    /// The backend's advertised capabilities.
    pub(crate) caps: BackendCapabilities,
    /// The shared universal state.
    pub(crate) state: StateHandle,
    /// The shared priority scheduler handle.
    pub(crate) radio: RadioHandle,
    /// The shared PTT lease.
    pub(crate) ptt: PttManager,
    /// This face's virtualized auto-information flag (never reaches the radio).
    ai: Arc<AtomicBool>,
}

impl FaceContext {
    /// Create a face context.
    pub(crate) fn new(
        face_id: u64,
        perms: FacePermissions,
        state: StateHandle,
        radio: RadioHandle,
        ptt: PttManager,
        caps: BackendCapabilities,
    ) -> Self {
        FaceContext {
            face_id,
            perms,
            caps,
            state,
            radio,
            ptt,
            ai: Arc::new(AtomicBool::new(false)),
        }
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
        if !self.perms.allows(class) {
            return ApplyOutcome::Denied;
        }
        match (class, mutation) {
            (CommandClass::PttWrite, StateMutation::SetPtt { keyed: true }) => {
                match self.ptt.try_key(self.face_id, self.perms.ptt) {
                    Err(PttDenied::Busy) => return ApplyOutcome::Busy,
                    Err(PttDenied::NotPermitted) => return ApplyOutcome::Denied,
                    Ok(()) => {}
                }
                if self
                    .radio
                    .submit(self.face_id, Priority::Ptt, OpKind::Apply(mutation))
                    .await
                    .is_ok()
                {
                    ApplyOutcome::Ok
                } else {
                    // The key request never reached the radio: release the lease.
                    self.ptt.unkey(self.face_id);
                    ApplyOutcome::Error
                }
            }
            (CommandClass::PttWrite, StateMutation::SetPtt { keyed: false }) => {
                let result = self
                    .radio
                    .submit(self.face_id, Priority::Ptt, OpKind::Apply(mutation))
                    .await;
                self.ptt.unkey(self.face_id);
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
                        .submit(self.face_id, priority, OpKind::Apply(mutation))
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
                self.face_id,
                Priority::Read,
                OpKind::Passthrough(raw.to_vec()),
            )
            .await
        {
            Ok(bytes) => bytes,
            Err(_) => ERR_REPLY.to_vec(),
        }
    }

    /// Set this face's virtualized auto-information flag.
    pub(crate) fn set_ai(&self, on: bool) {
        self.ai.store(on, Ordering::SeqCst);
    }

    /// Whether this face has auto-information enabled.
    pub(crate) fn ai_on(&self) -> bool {
        self.ai.load(Ordering::SeqCst)
    }

    /// A clone of this context for a new connection: same shared state/radio/ptt and
    /// permissions, but a distinct face id and a fresh (off) auto-information flag.
    pub(crate) fn clone_with_face(&self, face_id: u64) -> FaceContext {
        FaceContext {
            face_id,
            perms: self.perms,
            caps: self.caps.clone(),
            state: self.state.clone(),
            radio: self.radio.clone(),
            ptt: self.ptt.clone(),
            ai: Arc::new(AtomicBool::new(false)),
        }
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
    async fn handle(&self, request: &[u8], ctx: &FaceContext) -> Vec<u8>;

    /// Render a state change as an unsolicited notification for this face, if it applies.
    fn format_notification(&self, change: &StateChange, ctx: &FaceContext) -> Option<Vec<u8>>;

    /// Render an unsolicited native frame the backend does not model as a notification for
    /// this face, if it applies. Returns the bytes to push, or `None` to suppress it.
    ///
    /// The default suppresses the frame. A native pass-through dialect overrides this to
    /// relay the radio's CAT stream verbatim to clients that have enabled auto-information.
    fn format_passthrough(&self, _raw: &[u8], _ctx: &FaceContext) -> Option<Vec<u8>> {
        None
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::backend::loopback::LoopbackBackend;
    use crate::backend::RadioBackend;
    use crate::model::Vfo;
    use crate::radio::{detached_link, spawn_scheduler};
    use std::time::Duration;

    fn ctx_with(perms: FacePermissions, id: u64) -> (FaceContext, LoopbackBackend) {
        let backend = LoopbackBackend::new();
        let caps = backend.capabilities();
        let arc: Arc<dyn RadioBackend> = Arc::new(backend.clone());
        let state = StateHandle::new();
        let radio = spawn_scheduler(arc, detached_link(), state.clone());
        let ptt = PttManager::new(Duration::from_secs(300));
        (
            FaceContext::new(id, perms, state, radio, ptt, caps),
            backend,
        )
    }

    #[tokio::test]
    async fn modeled_write_denied_without_permission() {
        let (ctx, backend) = ctx_with(FacePermissions::read_only(), 1);
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
    async fn modeled_write_dispatches_when_allowed() {
        let (ctx, backend) = ctx_with(FacePermissions::from_tokens(&["read", "write"]), 1);
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
    async fn ptt_is_busy_for_a_second_face() {
        let (ctx1, _b) = ctx_with(FacePermissions::from_tokens(&["ptt"]), 1);
        // Share the same radio/ptt by cloning the context with a new face id.
        let ctx2 = ctx1.clone_with_face(2);
        assert_eq!(
            ctx1.apply_modeled(
                StateMutation::SetPtt { keyed: true },
                CommandClass::PttWrite
            )
            .await,
            ApplyOutcome::Ok
        );
        assert_eq!(
            ctx2.apply_modeled(
                StateMutation::SetPtt { keyed: true },
                CommandClass::PttWrite
            )
            .await,
            ApplyOutcome::Busy
        );
    }

    #[tokio::test]
    async fn passthrough_denied_returns_error_frame() {
        let (ctx, _b) = ctx_with(FacePermissions::read_only(), 1);
        // A read-only face may not issue a passthrough write.
        assert_eq!(
            ctx.passthrough(b"EX0050000;", CommandClass::ConfigWrite)
                .await,
            b"?;".to_vec()
        );
    }

    #[tokio::test]
    async fn ai_flag_is_per_face_and_resets_on_clone() {
        let (ctx, _b) = ctx_with(FacePermissions::read_only(), 1);
        assert!(!ctx.ai_on());
        ctx.set_ai(true);
        assert!(ctx.ai_on());
        let fresh = ctx.clone_with_face(2);
        assert!(!fresh.ai_on(), "a cloned face starts with auto-info off");
    }
}

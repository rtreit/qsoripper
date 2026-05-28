//! Client dialect abstraction: the seam that lets one universal state serve
//! many client CAT vocabularies (native Kenwood for N1MM, TS-2000 emulation for
//! `OmniRig`, Hamlib net for the engine).

pub(crate) mod kenwood;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use async_trait::async_trait;

use crate::backend::RadioBackend;
use crate::state::{StateChange, StateHandle};

/// What a face is allowed to do, enforced before any radio write.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Permissions {
    /// Whether the face may change frequency/mode/split.
    pub(crate) allow_write: bool,
    /// Whether the face may key PTT.
    pub(crate) allow_ptt: bool,
    /// Whether the face may send raw passthrough commands.
    pub(crate) allow_passthrough: bool,
}

impl Default for Permissions {
    fn default() -> Self {
        Self {
            allow_write: true,
            allow_ptt: false,
            allow_passthrough: true,
        }
    }
}

/// Per-face runtime context handed to a dialect on every request.
#[derive(Clone)]
pub(crate) struct FaceContext {
    /// Face name, used in logs and as the PTT owner identity.
    name: String,
    /// What this face is permitted to do.
    permissions: Permissions,
    /// Shared universal state for reads and modeled writes.
    state: StateHandle,
    /// Active backend for raw passthrough commands.
    backend: Arc<dyn RadioBackend>,
    /// Whether this face has auto-information fan-out enabled.
    ai_enabled: Arc<AtomicBool>,
}

impl FaceContext {
    /// Build a face context.
    pub(crate) fn new(
        name: impl Into<String>,
        permissions: Permissions,
        state: StateHandle,
        backend: Arc<dyn RadioBackend>,
    ) -> Self {
        Self {
            name: name.into(),
            permissions,
            state,
            backend,
            ai_enabled: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Face name.
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// Face permissions.
    pub(crate) fn permissions(&self) -> Permissions {
        self.permissions
    }

    /// Shared universal state.
    pub(crate) fn state(&self) -> &StateHandle {
        &self.state
    }

    /// Active backend.
    pub(crate) fn backend(&self) -> &Arc<dyn RadioBackend> {
        &self.backend
    }

    /// Whether auto-information fan-out is enabled for this face.
    pub(crate) fn ai_enabled(&self) -> bool {
        self.ai_enabled.load(Ordering::Relaxed)
    }

    /// Enable or disable auto-information fan-out for this face.
    pub(crate) fn set_ai_enabled(&self, enabled: bool) {
        self.ai_enabled.store(enabled, Ordering::Relaxed);
    }
}

/// A client CAT dialect. Implementations translate between a client's command
/// vocabulary and the universal state plus backend.
#[async_trait]
pub(crate) trait ClientDialect: Send + Sync {
    /// Handle one client request frame, returning the bytes to write back to
    /// the client (empty when the command produces no reply).
    async fn handle(&self, request: &[u8], ctx: &FaceContext) -> Vec<u8>;

    /// Format a universal state change as an unsolicited frame for an
    /// AI-subscribed client, or `None` if the dialect does not surface it.
    fn format_notification(&self, change: &StateChange, ctx: &FaceContext) -> Option<Vec<u8>>;
}

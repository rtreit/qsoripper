//! Radio backend abstraction: the seam that lets the hub drive different radio
//! families (Kenwood, Icom CI-V, Yaesu) behind one trait.

pub(crate) mod kenwood;
pub(crate) mod loopback;

use async_trait::async_trait;

use crate::error::BackendError;
use crate::model::{Mode, Vfo};
use crate::state::StateHandle;

/// A single normalized change to apply to the radio.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StateMutation {
    /// Set a VFO frequency in Hz.
    Frequency {
        /// Target VFO.
        vfo: Vfo,
        /// Frequency in Hz.
        hz: u64,
    },
    /// Set a VFO mode.
    Mode {
        /// Target VFO.
        vfo: Vfo,
        /// Mode to set.
        mode: Mode,
    },
    /// Key or unkey the transmitter.
    Ptt {
        /// Whether the transmitter should be keyed.
        keyed: bool,
    },
}

/// A radio family backend. Implementations own the native command vocabulary
/// and the mapping to and from universal [`StateMutation`]s and [`StateHandle`].
#[async_trait]
pub(crate) trait RadioBackend: Send + Sync {
    /// Refresh the universal state by polling the radio.
    async fn poll(&self, state: &StateHandle) -> Result<(), BackendError>;

    /// Apply a mutation to the radio and reflect it into the universal state.
    async fn apply(&self, mutation: StateMutation, state: &StateHandle)
        -> Result<(), BackendError>;

    /// Pass a raw native command straight through to the radio and return the
    /// raw reply bytes (possibly empty for no-reply commands).
    async fn passthrough(&self, raw: &[u8]) -> Result<Vec<u8>, BackendError>;
}

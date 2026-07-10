//! The radio backend abstraction: the [`RadioBackend`] trait every concrete radio
//! implements, plus the [`BackendCapabilities`] it advertises.
//!
//! A backend is the only code that knows a specific radio's wire vocabulary. It maps the
//! neutral [`StateMutation`]/[`StateChange`] vocabulary to and from bytes and reports what
//! it can do (and how much it can be trusted) via [`BackendCapabilities`].

pub(crate) mod kenwood;
pub(crate) mod loopback;
pub(crate) mod rigctld;

use async_trait::async_trait;

pub(crate) use crate::error::BackendError;
use crate::model::{RadioEventSource, StateMutation};
use crate::radio::RadioLink;
use crate::state::StateHandle;

/// How the byte stream is split into frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Framing {
    /// Kenwood/Yaesu style: each frame ends with `;`.
    SemicolonTerminated,
    /// Line protocols (e.g. `rigctld` net): each frame ends with `\n`.
    LineTerminated,
    /// Icom CI-V style: each frame ends with `0xFD`. Reserved for a future CI-V backend.
    #[allow(dead_code)]
    CiV,
}

/// How a backend models split operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SplitStyle {
    /// Split is a TX/RX VFO pair (Kenwood, Yaesu, most rigs).
    VfoPair,
    /// The radio has no split concept. Reserved for single-VFO backends.
    #[allow(dead_code)]
    None,
}

/// How much the daemon trusts a backend's wire behavior (design §7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TrustTier {
    /// A first-party native driver that has been certified against the real radio.
    CertifiedNative,
    /// An out-of-process bridge (e.g. `rigctld`) that has not been soak-certified.
    UncertifiedBridge,
    /// The in-memory test backend.
    Loopback,
}

/// The native command family a backend can passthrough (for clients that need raw CAT).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NativeCommandFamily {
    /// Kenwood `;`-terminated ASCII CAT.
    Kenwood,
}

/// What a backend can do, advertised to faces and the poller.
#[derive(Debug, Clone)]
#[allow(clippy::struct_excessive_bools)] // Each flag is an independent capability bit.
pub(crate) struct BackendCapabilities {
    /// A human-readable model identifier.
    pub(crate) model: String,
    /// Number of VFOs.
    pub(crate) vfo_count: u8,
    /// Whether the radio models RIT.
    pub(crate) has_rit: bool,
    /// Whether the radio models XIT.
    pub(crate) has_xit: bool,
    /// Whether the radio reports an S-meter.
    pub(crate) has_smeter: bool,
    /// How split is modeled.
    pub(crate) split: SplitStyle,
    /// Whether the radio has a native push (auto-information) stream.
    pub(crate) native_push: bool,
    /// The native command family available for passthrough, if any.
    pub(crate) native_command_family: Option<NativeCommandFamily>,
    /// How frames are delimited.
    pub(crate) framing: Framing,
    /// Minimum tunable frequency in Hz.
    pub(crate) freq_min_hz: u64,
    /// Maximum tunable frequency in Hz.
    pub(crate) freq_max_hz: u64,
    /// Trust tier.
    pub(crate) trust: TrustTier,
}

impl BackendCapabilities {
    /// Whether raw native passthrough is available (a native command family is present).
    pub(crate) fn supports_passthrough(&self) -> bool {
        self.native_command_family.is_some()
    }

    /// A one-line human-readable summary (used in startup logging).
    pub(crate) fn summary(&self) -> String {
        format!(
            "model={} vfos={} rit={} xit={} smeter={} split={:?} native_push={} \
             family={:?} framing={:?} freq={}..{} trust={:?} passthrough={}",
            self.model,
            self.vfo_count,
            self.has_rit,
            self.has_xit,
            self.has_smeter,
            self.split,
            self.native_push,
            self.native_command_family,
            self.framing,
            self.freq_min_hz,
            self.freq_max_hz,
            self.trust,
            self.supports_passthrough(),
        )
    }
}

/// A concrete radio. Implementations own one radio's wire vocabulary; everything above
/// them speaks only the neutral [`StateMutation`]/[`StateChange`] vocabulary.
#[async_trait]
pub(crate) trait RadioBackend: Send + Sync {
    /// Run one baseline poll cycle, recording observed state via `state`.
    async fn poll(&self, link: &RadioLink, state: &StateHandle) -> Result<(), BackendError>;

    /// Apply a modeled mutation, recording the resulting state via `state`.
    async fn apply(
        &self,
        mutation: StateMutation,
        link: &RadioLink,
        state: &StateHandle,
    ) -> Result<(), BackendError>;

    /// Parse an unsolicited (native push) frame into a mutation, if recognized.
    fn parse_event(&self, frame: &[u8]) -> Option<StateMutation>;

    /// Record an unsolicited (native push) frame into the universal state, if recognized.
    fn record_event(&self, frame: &[u8], state: &StateHandle, source: RadioEventSource) -> bool {
        if let Some(mutation) = self.parse_event(frame) {
            state.record(mutation.into_change(), source);
            true
        } else {
            false
        }
    }

    /// Forward a raw native command, returning the raw reply.
    async fn passthrough(&self, raw: &[u8], link: &RadioLink) -> Result<Vec<u8>, BackendError>;

    /// The backend's advertised capabilities.
    fn capabilities(&self) -> BackendCapabilities;

    /// The command that enables the radio's native push stream, if it has one.
    /// Backends without a native push stream (e.g. an out-of-process rigctld bridge)
    /// keep the default of `None` and are polled instead.
    fn native_push_enable(&self) -> Option<Vec<u8>> {
        None
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn passthrough_support_tracks_command_family() {
        let mut caps = loopback::LoopbackBackend::new().capabilities();
        assert!(caps.supports_passthrough());
        caps.native_command_family = None;
        assert!(!caps.supports_passthrough());
    }

    #[test]
    fn summary_mentions_every_axis() {
        let caps = loopback::LoopbackBackend::new().capabilities();
        let s = caps.summary();
        assert!(s.contains("model="));
        assert!(s.contains("native_push="));
        assert!(s.contains("trust="));
        assert!(s.contains("passthrough="));
    }
}

//! The in-memory loopback backend used by tests and the `loopback` config option.
//!
//! It records every mutation and passthrough it receives and exposes a mutable "truth"
//! frequency so a test can simulate a front-panel knob turn that a poll then diffs into
//! state. It has no native push (so the poller always runs at baseline against it).

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;

use crate::backend::{
    BackendCapabilities, BackendError, Framing, NativeCommandFamily, RadioBackend, SplitStyle,
    TrustTier,
};
use crate::model::{RadioEventSource, StateChange, StateMutation, Vfo};
use crate::radio::RadioLink;
use crate::state::StateHandle;

/// The default VFO-A "truth" the loopback radio reports when polled.
const DEFAULT_TRUTH_FREQ_A: u64 = 14_074_000;

/// A deterministic in-memory backend.
#[derive(Clone)]
pub(crate) struct LoopbackBackend {
    mutations: Arc<Mutex<Vec<StateMutation>>>,
    passthroughs: Arc<Mutex<Vec<Vec<u8>>>>,
    polls: Arc<AtomicUsize>,
    truth_freq_a: Arc<AtomicU64>,
}

impl LoopbackBackend {
    /// Create a fresh loopback backend.
    pub(crate) fn new() -> Self {
        LoopbackBackend {
            mutations: Arc::new(Mutex::new(Vec::new())),
            passthroughs: Arc::new(Mutex::new(Vec::new())),
            polls: Arc::new(AtomicUsize::new(0)),
            truth_freq_a: Arc::new(AtomicU64::new(DEFAULT_TRUTH_FREQ_A)),
        }
    }

    /// The mutations applied so far (in order).
    #[cfg(test)]
    pub(crate) fn mutations(&self) -> Vec<StateMutation> {
        self.mutations
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The raw passthrough payloads forwarded so far (in order).
    #[cfg(test)]
    pub(crate) fn passthroughs(&self) -> Vec<Vec<u8>> {
        self.passthroughs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// How many poll cycles have run.
    #[cfg(test)]
    pub(crate) fn poll_count(&self) -> usize {
        self.polls.load(Ordering::SeqCst)
    }

    /// Simulate a front-panel change to VFO A's frequency; the next poll diffs it.
    #[cfg(test)]
    pub(crate) fn set_truth_freq_a(&self, hz: u64) {
        self.truth_freq_a.store(hz, Ordering::SeqCst);
    }
}

#[async_trait]
impl RadioBackend for LoopbackBackend {
    async fn poll(&self, _link: &RadioLink, state: &StateHandle) -> Result<(), BackendError> {
        self.polls.fetch_add(1, Ordering::SeqCst);
        // The loopback radio only surfaces VFO-A frequency truth; recording just one field
        // keeps the "one broadcast per real change" invariant easy to reason about.
        let hz = self.truth_freq_a.load(Ordering::SeqCst);
        state.record(
            StateChange::Freq { vfo: Vfo::A, hz },
            RadioEventSource::PollDiff,
        );
        Ok(())
    }

    async fn apply(
        &self,
        mutation: StateMutation,
        _link: &RadioLink,
        state: &StateHandle,
    ) -> Result<(), BackendError> {
        self.mutations
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(mutation);
        state.record(mutation.into_change(), RadioEventSource::OptimisticWrite);
        Ok(())
    }

    fn parse_event(&self, _frame: &[u8]) -> Option<StateMutation> {
        None
    }

    async fn passthrough(&self, raw: &[u8], _link: &RadioLink) -> Result<Vec<u8>, BackendError> {
        self.passthroughs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(raw.to_vec());
        Ok(raw.to_vec())
    }

    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            model: "loopback".to_string(),
            vfo_count: 2,
            has_rit: true,
            has_xit: true,
            has_smeter: true,
            split: SplitStyle::VfoPair,
            native_push: false,
            native_command_family: Some(NativeCommandFamily::Kenwood),
            framing: Framing::SemicolonTerminated,
            freq_min_hz: 30_000,
            freq_max_hz: 60_000_000,
            trust: TrustTier::Loopback,
        }
    }

    fn native_push_enable(&self) -> Option<Vec<u8>> {
        None
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::radio::detached_link;

    #[tokio::test]
    async fn poll_records_truth_and_counts() {
        let backend = LoopbackBackend::new();
        let state = StateHandle::new();
        backend.set_truth_freq_a(7_123_000);
        backend.poll(&detached_link(), &state).await.expect("poll");
        assert_eq!(backend.poll_count(), 1);
        assert_eq!(state.snapshot().vfo(Vfo::A).freq_hz, 7_123_000);
    }

    #[tokio::test]
    async fn apply_records_mutation_and_state() {
        let backend = LoopbackBackend::new();
        let state = StateHandle::new();
        backend
            .apply(
                StateMutation::SetVfoFreq {
                    vfo: Vfo::A,
                    hz: 14_250_000,
                },
                &detached_link(),
                &state,
            )
            .await
            .expect("apply");
        assert_eq!(
            backend.mutations(),
            vec![StateMutation::SetVfoFreq {
                vfo: Vfo::A,
                hz: 14_250_000
            }]
        );
        assert_eq!(state.snapshot().vfo(Vfo::A).freq_hz, 14_250_000);
    }

    #[tokio::test]
    async fn passthrough_echoes_and_records() {
        let backend = LoopbackBackend::new();
        let reply = backend
            .passthrough(b"EX0050000;", &detached_link())
            .await
            .expect("passthrough");
        assert_eq!(reply, b"EX0050000;");
        assert_eq!(backend.passthroughs(), vec![b"EX0050000;".to_vec()]);
    }

    #[test]
    fn loopback_has_no_native_push() {
        assert!(LoopbackBackend::new().native_push_enable().is_none());
        assert!(!LoopbackBackend::new().capabilities().native_push);
    }
}

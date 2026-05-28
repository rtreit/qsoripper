//! In-memory backend used by tests and `--dry-run`. Records every mutation and
//! passthrough, reflects mutations into the universal state, and serves canned
//! poll data without touching hardware.

#[cfg(test)]
use std::sync::Mutex;

use async_trait::async_trait;

use crate::backend::{RadioBackend, StateMutation};
use crate::error::BackendError;
use crate::model::{Mode, Vfo};
use crate::state::StateHandle;

/// A canned poll snapshot the loopback backend reports.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CannedPoll {
    /// VFO A frequency in Hz.
    pub(crate) freq_a: u64,
    /// VFO B frequency in Hz.
    pub(crate) freq_b: u64,
    /// VFO A mode.
    pub(crate) mode_a: Mode,
    /// VFO B mode.
    pub(crate) mode_b: Mode,
}

impl Default for CannedPoll {
    fn default() -> Self {
        Self {
            freq_a: 14_074_000,
            freq_b: 14_074_000,
            mode_a: Mode::Usb,
            mode_b: Mode::Usb,
        }
    }
}

/// Backend that records interactions instead of driving a radio.
pub(crate) struct LoopbackBackend {
    canned: CannedPoll,
    #[cfg(test)]
    mutations: Mutex<Vec<StateMutation>>,
    #[cfg(test)]
    passthroughs: Mutex<Vec<Vec<u8>>>,
}

impl LoopbackBackend {
    /// Create a loopback backend with default canned poll data.
    pub(crate) fn new() -> Self {
        Self {
            canned: CannedPoll::default(),
            #[cfg(test)]
            mutations: Mutex::new(Vec::new()),
            #[cfg(test)]
            passthroughs: Mutex::new(Vec::new()),
        }
    }

    /// Snapshot of mutations recorded so far (test introspection only).
    #[cfg(test)]
    pub(crate) fn recorded_mutations(&self) -> Vec<StateMutation> {
        self.mutations.lock().map(|g| g.clone()).unwrap_or_default()
    }

    /// Snapshot of passthrough payloads recorded so far (test introspection only).
    #[cfg(test)]
    pub(crate) fn recorded_passthroughs(&self) -> Vec<Vec<u8>> {
        self.passthroughs
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default()
    }

    #[cfg(test)]
    fn record_mutation(&self, mutation: StateMutation) {
        if let Ok(mut guard) = self.mutations.lock() {
            guard.push(mutation);
        }
    }

    #[cfg(not(test))]
    #[allow(clippy::unused_self)]
    fn record_mutation(&self, _mutation: StateMutation) {}
}

#[async_trait]
impl RadioBackend for LoopbackBackend {
    async fn poll(&self, state: &StateHandle) -> Result<(), BackendError> {
        state.set_frequency(Vfo::A, self.canned.freq_a).await;
        state.set_frequency(Vfo::B, self.canned.freq_b).await;
        state.set_mode(Vfo::A, self.canned.mode_a).await;
        state.set_mode(Vfo::B, self.canned.mode_b).await;
        Ok(())
    }

    async fn apply(
        &self,
        mutation: StateMutation,
        state: &StateHandle,
    ) -> Result<(), BackendError> {
        self.record_mutation(mutation);
        match mutation {
            StateMutation::Frequency { vfo, hz } => state.set_frequency(vfo, hz).await,
            StateMutation::Mode { vfo, mode } => state.set_mode(vfo, mode).await,
            StateMutation::Ptt { .. } => {}
        }
        Ok(())
    }

    async fn passthrough(&self, raw: &[u8]) -> Result<Vec<u8>, BackendError> {
        #[cfg(test)]
        {
            if let Ok(mut guard) = self.passthroughs.lock() {
                guard.push(raw.to_vec());
            }
        }
        #[cfg(not(test))]
        let _ = raw;
        Ok(Vec::new())
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn poll_populates_state_from_canned_data() {
        let (handle, _inbox) = StateHandle::new(16);
        let backend = LoopbackBackend::new();
        backend.poll(&handle).await.expect("poll");
        assert_eq!(handle.snapshot().await.freq_a, 14_074_000);
    }

    #[tokio::test]
    async fn apply_records_and_reflects_mutation() {
        let (handle, _inbox) = StateHandle::new(16);
        let backend = LoopbackBackend::new();
        backend
            .apply(
                StateMutation::Mode {
                    vfo: Vfo::A,
                    mode: Mode::Cw,
                },
                &handle,
            )
            .await
            .expect("apply");
        assert_eq!(handle.snapshot().await.mode_a, Mode::Cw);
        assert_eq!(backend.recorded_mutations().len(), 1);
    }

    #[tokio::test]
    async fn passthrough_records_raw_bytes() {
        let backend = LoopbackBackend::new();
        let reply = backend.passthrough(b"FA;").await.expect("passthrough");
        assert!(reply.is_empty());
        assert_eq!(backend.recorded_passthroughs(), vec![b"FA;".to_vec()]);
    }
}

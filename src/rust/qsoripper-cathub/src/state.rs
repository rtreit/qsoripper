//! Universal in-memory rig state, the mutation dispatch channel, and the
//! change-notification broadcast.
//!
//! Every path that observes or changes the radio goes through this layer:
//! backends populate it, dialects read and mutate it, and the poller refreshes
//! it. Faces subscribe to [`StateHandle::subscribe`] to push updates to
//! auto-information clients.

use std::sync::Arc;

use tokio::sync::{broadcast, mpsc, oneshot, RwLock};

use crate::backend::{RadioBackend, StateMutation};
use crate::error::BackendError;
use crate::model::{Mode, Vfo};

/// A change to one universal-state field, broadcast to faces for AI fan-out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StateChange {
    /// A VFO frequency changed.
    Frequency {
        /// Affected VFO.
        vfo: Vfo,
        /// New frequency in Hz.
        hz: u64,
    },
    /// A VFO mode changed.
    Mode {
        /// Affected VFO.
        vfo: Vfo,
        /// New mode.
        mode: Mode,
    },
}

/// The universal rig state snapshot.
#[derive(Debug, Clone, Copy)]
pub(crate) struct RigState {
    /// VFO A frequency in Hz.
    pub(crate) freq_a: u64,
    /// VFO B frequency in Hz.
    pub(crate) freq_b: u64,
    /// VFO A mode.
    pub(crate) mode_a: Mode,
    /// VFO B mode.
    pub(crate) mode_b: Mode,
    /// Active receive VFO.
    pub(crate) rx_vfo: Vfo,
}

impl Default for RigState {
    fn default() -> Self {
        Self {
            freq_a: 0,
            freq_b: 0,
            mode_a: Mode::Usb,
            mode_b: Mode::Usb,
            rx_vfo: Vfo::A,
        }
    }
}

impl RigState {
    /// Frequency for the given VFO.
    pub(crate) fn freq(&self, vfo: Vfo) -> u64 {
        match vfo {
            Vfo::A => self.freq_a,
            Vfo::B => self.freq_b,
        }
    }

    /// Mode for the given VFO.
    pub(crate) fn mode(&self, vfo: Vfo) -> Mode {
        match vfo {
            Vfo::A => self.mode_a,
            Vfo::B => self.mode_b,
        }
    }
}

/// A request to mutate the radio, carried over the dispatch channel.
struct MutationRequest {
    mutation: StateMutation,
    reply: oneshot::Sender<Result<(), BackendError>>,
}

/// Opaque wrapper around the mutation receiver so the channel payload stays
/// private while the receiver can be handed to the dispatcher.
pub(crate) struct MutationInbox {
    rx: mpsc::Receiver<MutationRequest>,
}

/// Shared, cloneable handle to the universal rig state.
#[derive(Clone)]
pub(crate) struct StateHandle {
    inner: Arc<RwLock<RigState>>,
    changes: broadcast::Sender<StateChange>,
    mutations: mpsc::Sender<MutationRequest>,
}

impl StateHandle {
    /// Create a state handle plus the receiver half of the mutation channel.
    ///
    /// The caller runs [`run_mutation_dispatcher`] with the returned inbox and
    /// the active backend to service [`StateHandle::apply_mutation`] calls.
    pub(crate) fn new(broadcast_capacity: usize) -> (Self, MutationInbox) {
        let (changes, _) = broadcast::channel(broadcast_capacity);
        let (mutations, rx) = mpsc::channel(64);
        let handle = Self {
            inner: Arc::new(RwLock::new(RigState::default())),
            changes,
            mutations,
        };
        (handle, MutationInbox { rx })
    }

    /// Subscribe to the change-notification broadcast.
    pub(crate) fn subscribe(&self) -> broadcast::Receiver<StateChange> {
        self.changes.subscribe()
    }

    /// Take a snapshot of the current state.
    pub(crate) async fn snapshot(&self) -> RigState {
        *self.inner.read().await
    }

    fn broadcast(&self, change: StateChange) {
        // A send error only means there are no subscribers, which is fine.
        let _ = self.changes.send(change);
    }

    /// Set a VFO frequency and broadcast the change.
    pub(crate) async fn set_frequency(&self, vfo: Vfo, hz: u64) {
        {
            let mut state = self.inner.write().await;
            match vfo {
                Vfo::A => state.freq_a = hz,
                Vfo::B => state.freq_b = hz,
            }
        }
        self.broadcast(StateChange::Frequency { vfo, hz });
    }

    /// Set a VFO mode and broadcast the change.
    pub(crate) async fn set_mode(&self, vfo: Vfo, mode: Mode) {
        {
            let mut state = self.inner.write().await;
            match vfo {
                Vfo::A => state.mode_a = mode,
                Vfo::B => state.mode_b = mode,
            }
        }
        self.broadcast(StateChange::Mode { vfo, mode });
    }

    /// Submit a mutation for the backend to apply, awaiting the result.
    pub(crate) async fn apply_mutation(&self, mutation: StateMutation) -> Result<(), BackendError> {
        let (reply, rx) = oneshot::channel();
        self.mutations
            .send(MutationRequest { mutation, reply })
            .await
            .map_err(|_| BackendError::Transport("mutation dispatcher stopped".to_string()))?;
        rx.await
            .map_err(|_| BackendError::Transport("mutation dispatcher dropped reply".to_string()))?
    }
}

/// Run the mutation dispatcher: pull mutation requests and apply them through
/// the backend, which updates the shared state on success.
pub(crate) async fn run_mutation_dispatcher(
    mut inbox: MutationInbox,
    backend: Arc<dyn RadioBackend>,
    state: StateHandle,
) {
    while let Some(request) = inbox.rx.recv().await {
        let result = backend.apply(request.mutation, &state).await;
        let _ = request.reply.send(result);
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::backend::loopback::LoopbackBackend;

    #[tokio::test]
    async fn set_frequency_updates_snapshot() {
        let (handle, _inbox) = StateHandle::new(16);
        handle.set_frequency(Vfo::A, 14_074_000).await;
        assert_eq!(handle.snapshot().await.freq_a, 14_074_000);
    }

    #[tokio::test]
    async fn set_mode_updates_snapshot() {
        let (handle, _inbox) = StateHandle::new(16);
        handle.set_mode(Vfo::B, Mode::Cw).await;
        assert_eq!(handle.snapshot().await.mode_b, Mode::Cw);
    }

    #[tokio::test]
    async fn subscribers_receive_changes() {
        let (handle, _inbox) = StateHandle::new(16);
        let mut rx = handle.subscribe();
        handle.set_mode(Vfo::A, Mode::Cw).await;
        let change = rx.recv().await.expect("a change");
        assert_eq!(
            change,
            StateChange::Mode {
                vfo: Vfo::A,
                mode: Mode::Cw
            }
        );
    }

    #[tokio::test]
    async fn apply_mutation_round_trips_through_backend() {
        let (handle, inbox) = StateHandle::new(16);
        let backend = Arc::new(LoopbackBackend::new());
        let dispatcher = tokio::spawn(run_mutation_dispatcher(
            inbox,
            backend.clone(),
            handle.clone(),
        ));
        handle
            .apply_mutation(StateMutation::Frequency {
                vfo: Vfo::A,
                hz: 21_074_000,
            })
            .await
            .expect("mutation applied");
        assert_eq!(handle.snapshot().await.freq_a, 21_074_000);
        assert_eq!(backend.recorded_mutations().len(), 1);
        dispatcher.abort();
    }
}

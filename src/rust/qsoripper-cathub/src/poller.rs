//! Baseline poll task. Drives the active backend to refresh universal state on
//! a fixed cadence so client reads can be served from cache between polls.

use std::sync::Arc;
use std::time::Duration;

use crate::backend::RadioBackend;
use crate::state::StateHandle;

/// Run the baseline poller until cancelled, refreshing state every `interval`.
pub(crate) async fn run_poller(
    backend: Arc<dyn RadioBackend>,
    state: StateHandle,
    interval: Duration,
) {
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        ticker.tick().await;
        if let Err(error) = backend.poll(&state).await {
            tracing::warn!(%error, "baseline poll failed");
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::backend::loopback::LoopbackBackend;

    #[tokio::test]
    async fn poller_refreshes_state() {
        let (state, _inbox) = StateHandle::new(16);
        let backend: Arc<dyn RadioBackend> = Arc::new(LoopbackBackend::new());
        let task = tokio::spawn(run_poller(
            backend,
            state.clone(),
            Duration::from_millis(10),
        ));
        tokio::time::sleep(Duration::from_millis(40)).await;
        assert_eq!(state.snapshot().await.freq_a, 14_074_000);
        task.abort();
    }
}

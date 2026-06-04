//! Central native-push ownership and the baseline poller (design §8.4).
//!
//! The daemon — not any client — owns the radio's spontaneous-update stream. At startup
//! (and on reconnect) it enables the rig's native push (`AI2;` on a TS-590) once and keeps
//! it on for the daemon's lifetime. Per-face auto-info is virtualized in [`crate::dialect`]
//! and never reaches the wire.
//!
//! The poller submits one baseline poll cycle at [`PollConfig`](crate::config::PollConfig)
//! cadence, backing off to the heartbeat rate only for fields the radio actually covers
//! with `NativePush` (poll-diff coverage never causes back-off, §8.4).

use std::sync::Arc;
use std::time::Duration;

use crate::backend::RadioBackend;
use crate::model::Field;
use crate::radio::{Expect, OpKind, Priority, RadioHandle, RadioLink};
use crate::state::StateHandle;

/// Reserved face id for the background poller (clients use ids `>= 1`).
pub(crate) const POLLER_FACE: u64 = 0;

/// Enable the radio's native push stream if the backend has one. Returns whether a push
/// command was issued (so the poller knows native push is in play).
pub(crate) async fn enable_native_push(backend: &Arc<dyn RadioBackend>, link: &RadioLink) -> bool {
    if let Some(cmd) = backend.native_push_enable() {
        // Auto-info enable is a no-reply set; the radio simply begins streaming.
        let _ = link.submit(cmd, Expect::NoReply).await;
        true
    } else {
        false
    }
}

/// Decide the next poll interval. While native push covers the primary frequency field,
/// the poller drops to the heartbeat (liveness) rate; otherwise it polls at baseline.
pub(crate) fn next_interval(
    state: &StateHandle,
    native_push_active: bool,
    baseline: Duration,
    heartbeat: Duration,
) -> Duration {
    // Back off only when native push covers the frequency of the *currently active* receive
    // VFO. Probing a fixed `Vfo::A` meant that while the radio sat on VFO B the poller saw
    // no coverage and never backed off (over-polling on B), and conversely could back off on
    // A's coverage while B's frequency was the live one. Track the active VFO instead.
    let active = state.snapshot().rx_vfo;
    if native_push_active && state.is_native_push_covered(Field::Freq(active)) {
        heartbeat
    } else {
        baseline
    }
}

/// Spawn the baseline poller. It submits poll cycles through the scheduler at
/// [`Priority::Poll`], so any interactive write or PTT always preempts it.
pub(crate) fn spawn_poller(
    radio: RadioHandle,
    state: StateHandle,
    native_push_active: bool,
    baseline: Duration,
    heartbeat: Duration,
) {
    tokio::spawn(async move {
        loop {
            let _ = radio
                .submit(POLLER_FACE, Priority::Poll, OpKind::Poll)
                .await;
            let interval = next_interval(&state, native_push_active, baseline, heartbeat);
            tokio::time::sleep(interval).await;
        }
    });
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::backend::kenwood::ts590::Ts590Backend;
    use crate::backend::loopback::LoopbackBackend;
    use crate::model::{RadioEventSource, StateChange, Vfo};
    use crate::radio::{detached_link, spawn_scheduler};

    #[tokio::test]
    async fn poller_drives_polls_through_scheduler() {
        let backend = LoopbackBackend::new();
        let arc: Arc<dyn RadioBackend> = Arc::new(backend.clone());
        let state = StateHandle::new();
        let radio = spawn_scheduler(arc, detached_link(), state.clone());
        spawn_poller(
            radio,
            state,
            false,
            Duration::from_millis(5),
            Duration::from_millis(50),
        );
        tokio::time::sleep(Duration::from_millis(40)).await;
        assert!(backend.poll_count() >= 2, "poller should run repeatedly");
    }

    #[test]
    fn back_off_only_when_native_push_covers_field() {
        let state = StateHandle::new();
        let baseline = Duration::from_millis(200);
        let heartbeat = Duration::from_millis(2_000);

        // No coverage yet: baseline.
        assert_eq!(next_interval(&state, true, baseline, heartbeat), baseline);

        // A poll-diff event must NOT trigger back-off.
        state.record(
            StateChange::Freq {
                vfo: Vfo::A,
                hz: 7_001_000,
            },
            RadioEventSource::PollDiff,
        );
        assert_eq!(next_interval(&state, true, baseline, heartbeat), baseline);

        // A native-push event covers the field: back off to heartbeat.
        state.record(
            StateChange::Freq {
                vfo: Vfo::A,
                hz: 7_002_000,
            },
            RadioEventSource::NativePush,
        );
        assert_eq!(next_interval(&state, true, baseline, heartbeat), heartbeat);

        // With native push disabled, always baseline regardless of coverage.
        assert_eq!(next_interval(&state, false, baseline, heartbeat), baseline);
    }

    #[test]
    fn back_off_follows_the_active_vfo_not_a_fixed_vfo_a() {
        // Native push covers VFO B's frequency while the radio sits on VFO B. The poller must
        // recognize coverage of the *active* VFO and back off, instead of over-polling
        // because it only ever probed VFO A.
        let state = StateHandle::new();
        let baseline = Duration::from_millis(200);
        let heartbeat = Duration::from_millis(2_000);

        // Make VFO B the active receive VFO.
        state.record(
            StateChange::RxVfo { vfo: Vfo::B },
            RadioEventSource::NativePush,
        );
        // Native push covers VFO B's frequency.
        state.record(
            StateChange::Freq {
                vfo: Vfo::B,
                hz: 14_074_000,
            },
            RadioEventSource::NativePush,
        );
        // Coverage of the active VFO (B) must drive the back-off.
        assert_eq!(next_interval(&state, true, baseline, heartbeat), heartbeat);
    }

    #[tokio::test]
    async fn enable_native_push_reports_capability() {
        let native: Arc<dyn RadioBackend> = Arc::new(Ts590Backend::new());
        // A detached link accepts the no-reply submit and drops it.
        assert!(enable_native_push(&native, &detached_link()).await);

        let bridge: Arc<dyn RadioBackend> = Arc::new(LoopbackBackend::new());
        // Loopback reports no native push.
        assert!(!enable_native_push(&bridge, &detached_link()).await);
    }
}

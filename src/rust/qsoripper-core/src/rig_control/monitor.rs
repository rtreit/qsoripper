//! Cached rig state snapshots with staleness tracking.

use std::{
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use prost_types::Timestamp;
use tokio::sync::{Mutex, RwLock};

use crate::proto::qsoripper::domain::{RigConnectionStatus, RigSnapshot};

use super::provider::RigControlProvider;

#[derive(Clone)]
struct CachedSnapshot {
    snapshot: RigSnapshot,
    fetched_monotonic: Instant,
}

/// Cached monitor around a rig control provider.
///
/// Caches the latest snapshot and refreshes it when the configured threshold
/// elapses. Follows the same pattern as [`SpaceWeatherMonitor`].
pub struct RigControlMonitor {
    provider: Arc<dyn RigControlProvider>,
    stale_threshold: Duration,
    refresh_gate: Mutex<()>,
    state: RwLock<Option<CachedSnapshot>>,
}

impl RigControlMonitor {
    /// Create a monitor with a staleness threshold.
    #[must_use]
    pub fn new(provider: Arc<dyn RigControlProvider>, stale_threshold: Duration) -> Self {
        Self {
            provider,
            stale_threshold,
            refresh_gate: Mutex::new(()),
            state: RwLock::new(None),
        }
    }

    /// Return the current snapshot, refreshing when stale or missing.
    pub async fn current_snapshot(&self) -> RigSnapshot {
        self.current_snapshot_with_max_age(self.stale_threshold)
            .await
    }

    /// Return the current snapshot, refreshing when it is older than `max_age`.
    pub async fn current_snapshot_with_max_age(&self, max_age: Duration) -> RigSnapshot {
        if let Some(cached) = self.state.read().await.clone() {
            if cached.fetched_monotonic.elapsed() < max_age {
                return cached.snapshot.clone();
            }
        }

        let _guard = self.refresh_gate.lock().await;
        if let Some(cached) = self.state.read().await.clone() {
            if cached.fetched_monotonic.elapsed() < max_age {
                return cached.snapshot.clone();
            }
        }

        self.refresh_snapshot_core().await
    }

    /// Force a refresh and return the latest available snapshot.
    pub async fn refresh_snapshot(&self) -> RigSnapshot {
        let _guard = self.refresh_gate.lock().await;
        self.refresh_snapshot_core().await
    }

    async fn refresh_snapshot_core(&self) -> RigSnapshot {
        let cached = self.state.read().await.clone();

        match self.provider.get_snapshot().await {
            Ok(mut snapshot) => {
                snapshot.sampled_at = Some(now_timestamp());
                snapshot.status = RigConnectionStatus::Connected as i32;
                snapshot.error_message = None;
                let cached_snapshot = CachedSnapshot {
                    snapshot: snapshot.clone(),
                    fetched_monotonic: Instant::now(),
                };
                *self.state.write().await = Some(cached_snapshot);
                snapshot
            }
            Err(error) => {
                if let Some(cached) = cached {
                    let mut snapshot = cached.snapshot.clone();
                    snapshot.status = RigConnectionStatus::Error as i32;
                    snapshot.error_message = Some(error.to_string());
                    snapshot
                } else {
                    let status =
                        if error.kind() == super::provider::RigControlProviderErrorKind::Disabled {
                            RigConnectionStatus::Disabled
                        } else {
                            RigConnectionStatus::Error
                        };

                    RigSnapshot {
                        sampled_at: Some(now_timestamp()),
                        status: status as i32,
                        error_message: Some(error.to_string()),
                        ..RigSnapshot::default()
                    }
                }
            }
        }
    }
}

fn now_timestamp() -> Timestamp {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    Timestamp {
        seconds: i64::try_from(now.as_secs()).unwrap_or(i64::MAX),
        nanos: i32::try_from(now.subsec_nanos()).unwrap_or(i32::MAX),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::rig_control::provider::{RigControlProvider, RigControlProviderError};

    #[derive(Clone)]
    struct FakeProvider {
        result: Result<RigSnapshot, RigControlProviderError>,
    }

    #[tonic::async_trait]
    impl RigControlProvider for FakeProvider {
        async fn get_snapshot(&self) -> Result<RigSnapshot, RigControlProviderError> {
            self.result.clone()
        }
    }

    #[tokio::test]
    async fn current_snapshot_returns_cached_value_within_threshold() {
        let provider = Arc::new(FakeProvider {
            result: Ok(RigSnapshot {
                frequency_hz: 14_074_000,
                ..RigSnapshot::default()
            }),
        });
        let monitor = RigControlMonitor::new(provider, Duration::from_secs(60));

        let initial = monitor.refresh_snapshot().await;
        let cached = monitor.current_snapshot().await;

        assert_eq!(initial.frequency_hz, cached.frequency_hz);
        assert_eq!(RigConnectionStatus::Connected as i32, cached.status);
    }

    #[tokio::test]
    async fn refresh_returns_error_snapshot_on_failure_without_cache() {
        let provider = Arc::new(FakeProvider {
            result: Err(RigControlProviderError::transport("connection refused")),
        });
        let monitor = RigControlMonitor::new(provider, Duration::from_secs(60));

        let snapshot = monitor.refresh_snapshot().await;

        assert_eq!(RigConnectionStatus::Error as i32, snapshot.status);
        assert!(snapshot.error_message.is_some());
    }

    #[tokio::test]
    async fn refresh_returns_stale_cached_on_failure_with_cache() {
        let good_provider = Arc::new(FakeProvider {
            result: Ok(RigSnapshot {
                frequency_hz: 7_074_000,
                ..RigSnapshot::default()
            }),
        });
        let monitor = RigControlMonitor::new(good_provider, Duration::from_secs(0));
        let _ = monitor.refresh_snapshot().await;

        // Swap to a failing provider
        let bad_provider = Arc::new(FakeProvider {
            result: Err(RigControlProviderError::transport("offline")),
        });
        let existing = monitor.state.write().await.clone();
        let monitor = RigControlMonitor {
            provider: bad_provider,
            stale_threshold: Duration::from_secs(0),
            refresh_gate: Mutex::new(()),
            state: RwLock::new(existing),
        };

        let snapshot = monitor.refresh_snapshot().await;

        assert_eq!(RigConnectionStatus::Error as i32, snapshot.status);
        assert_eq!(7_074_000, snapshot.frequency_hz);
        assert!(snapshot.error_message.is_some());
    }

    #[tokio::test]
    async fn disabled_provider_returns_disabled_status() {
        let provider = Arc::new(FakeProvider {
            result: Err(RigControlProviderError::disabled("not configured")),
        });
        let monitor = RigControlMonitor::new(provider, Duration::from_secs(60));

        let snapshot = monitor.refresh_snapshot().await;

        assert_eq!(RigConnectionStatus::Disabled as i32, snapshot.status);
    }

    /// Provider whose snapshot can change between reads, mimicking a radio that
    /// retunes underneath the monitor.
    struct MutableProvider {
        snapshot: std::sync::Mutex<RigSnapshot>,
    }

    impl MutableProvider {
        fn new(frequency_hz: u64) -> Self {
            Self {
                snapshot: std::sync::Mutex::new(RigSnapshot {
                    frequency_hz,
                    ..RigSnapshot::default()
                }),
            }
        }

        fn set_frequency(&self, frequency_hz: u64) {
            self.snapshot.lock().unwrap().frequency_hz = frequency_hz;
        }
    }

    #[tonic::async_trait]
    impl RigControlProvider for MutableProvider {
        async fn get_snapshot(&self) -> Result<RigSnapshot, RigControlProviderError> {
            Ok(self.snapshot.lock().unwrap().clone())
        }
    }

    /// Regression test for cathub rig-status lag: an interactive UI polls rig state
    /// at a fast cadence, so the default staleness threshold must surface a
    /// frequency change within that budget rather than masking it behind a
    /// multi-second cached snapshot.
    #[tokio::test]
    async fn default_threshold_reflects_rig_changes_within_interactive_budget() {
        use crate::rig_control::rigctld::DEFAULT_RIGCTLD_STALE_THRESHOLD_MS;

        const INTERACTIVE_BUDGET_MS: u64 = 150;

        let provider = Arc::new(MutableProvider::new(14_074_000));
        let monitor = RigControlMonitor::new(
            provider.clone(),
            Duration::from_millis(DEFAULT_RIGCTLD_STALE_THRESHOLD_MS),
        );

        // Prime the cache, then retune the radio.
        let _ = monitor.refresh_snapshot().await;
        provider.set_frequency(14_055_000);

        // After one interactive poll window a fresh read must see the new freq.
        tokio::time::sleep(Duration::from_millis(INTERACTIVE_BUDGET_MS)).await;
        let snapshot = monitor.current_snapshot().await;

        assert_eq!(
            14_055_000, snapshot.frequency_hz,
            "default stale threshold must surface rig changes within {INTERACTIVE_BUDGET_MS} ms",
        );
    }

    #[tokio::test]
    async fn max_age_override_refreshes_before_configured_threshold() {
        let provider = Arc::new(MutableProvider::new(14_074_000));
        let monitor = RigControlMonitor::new(provider.clone(), Duration::from_secs(60));

        let _ = monitor.refresh_snapshot().await;
        provider.set_frequency(14_055_000);
        tokio::time::sleep(Duration::from_millis(20)).await;

        let snapshot = monitor
            .current_snapshot_with_max_age(Duration::from_millis(10))
            .await;

        assert_eq!(14_055_000, snapshot.frequency_hz);
    }

    #[tokio::test]
    async fn max_age_override_reuses_cache_when_fresh_enough() {
        let provider = Arc::new(MutableProvider::new(14_074_000));
        let monitor = RigControlMonitor::new(provider.clone(), Duration::from_secs(60));

        let _ = monitor.refresh_snapshot().await;
        provider.set_frequency(14_055_000);

        let snapshot = monitor
            .current_snapshot_with_max_age(Duration::from_secs(60))
            .await;

        assert_eq!(14_074_000, snapshot.frequency_hz);
    }
}

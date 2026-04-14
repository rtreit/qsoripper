//! Cached current space weather snapshots.

use std::{
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use prost_types::Timestamp;
use tokio::sync::RwLock;

use crate::proto::qsoripper::domain::{SpaceWeatherSnapshot, SpaceWeatherStatus};

use super::provider::SpaceWeatherProvider;

#[derive(Clone)]
struct CachedSnapshot {
    snapshot: SpaceWeatherSnapshot,
    fetched_monotonic: Instant,
}

/// Small cache around the current space weather provider.
pub struct SpaceWeatherMonitor {
    provider: Arc<dyn SpaceWeatherProvider>,
    refresh_interval: Duration,
    stale_after: Duration,
    state: RwLock<Option<CachedSnapshot>>,
}

impl SpaceWeatherMonitor {
    /// Create a monitor around a provider with refresh and stale thresholds.
    #[must_use]
    pub fn new(
        provider: Arc<dyn SpaceWeatherProvider>,
        refresh_interval: Duration,
        stale_after: Duration,
    ) -> Self {
        Self {
            provider,
            refresh_interval,
            stale_after,
            state: RwLock::new(None),
        }
    }

    /// Return the current snapshot, refreshing when needed.
    pub async fn current_snapshot(&self) -> SpaceWeatherSnapshot {
        if let Some(cached) = self.state.read().await.clone() {
            if cached.fetched_monotonic.elapsed() < self.refresh_interval {
                return snapshot_with_age_status(&cached.snapshot, self.stale_after, &cached);
            }
        }

        self.refresh_snapshot().await
    }

    /// Force a refresh and return the latest available snapshot.
    pub async fn refresh_snapshot(&self) -> SpaceWeatherSnapshot {
        let cached = self.state.read().await.clone();

        match self.provider.fetch_current().await {
            Ok(mut snapshot) => {
                snapshot.fetched_at = Some(now_timestamp());
                snapshot.status = SpaceWeatherStatus::Current as i32;
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
                    let mut snapshot =
                        snapshot_with_age_status(&cached.snapshot, self.stale_after, &cached);
                    snapshot.status = SpaceWeatherStatus::Stale as i32;
                    snapshot.error_message = Some(error.to_string());
                    snapshot
                } else {
                    SpaceWeatherSnapshot {
                        fetched_at: Some(now_timestamp()),
                        status: SpaceWeatherStatus::Error as i32,
                        error_message: Some(error.to_string()),
                        source_name: Some("NOAA SWPC".to_string()),
                        ..SpaceWeatherSnapshot::default()
                    }
                }
            }
        }
    }
}

fn snapshot_with_age_status(
    snapshot: &SpaceWeatherSnapshot,
    stale_after: Duration,
    cached: &CachedSnapshot,
) -> SpaceWeatherSnapshot {
    let mut snapshot = snapshot.clone();
    snapshot.status = if cached.fetched_monotonic.elapsed() >= stale_after {
        SpaceWeatherStatus::Stale as i32
    } else {
        SpaceWeatherStatus::Current as i32
    };
    snapshot
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
    use crate::proto::qsoripper::domain::SpaceWeatherStatus;
    use crate::space_weather::provider::{SpaceWeatherProvider, SpaceWeatherProviderError};

    #[derive(Clone)]
    struct FakeProvider {
        result: Result<SpaceWeatherSnapshot, SpaceWeatherProviderError>,
    }

    #[tonic::async_trait]
    impl SpaceWeatherProvider for FakeProvider {
        async fn fetch_current(&self) -> Result<SpaceWeatherSnapshot, SpaceWeatherProviderError> {
            self.result.clone()
        }
    }

    #[tokio::test]
    async fn current_snapshot_returns_cached_value_without_refreshing() {
        let provider = Arc::new(FakeProvider {
            result: Ok(SpaceWeatherSnapshot {
                source_name: Some("NOAA SWPC".to_string()),
                planetary_k_index: Some(2.33),
                ..SpaceWeatherSnapshot::default()
            }),
        });
        let monitor =
            SpaceWeatherMonitor::new(provider, Duration::from_secs(60), Duration::from_secs(300));

        let initial = monitor.refresh_snapshot().await;
        let cached = monitor.current_snapshot().await;

        assert_eq!(initial.planetary_k_index, cached.planetary_k_index);
        assert_eq!(SpaceWeatherStatus::Current as i32, cached.status);
    }

    #[tokio::test]
    async fn refresh_snapshot_returns_stale_cached_snapshot_when_refresh_fails() {
        let good_provider = Arc::new(FakeProvider {
            result: Ok(SpaceWeatherSnapshot {
                source_name: Some("NOAA SWPC".to_string()),
                planetary_k_index: Some(3.0),
                ..SpaceWeatherSnapshot::default()
            }),
        });
        let monitor = SpaceWeatherMonitor::new(
            good_provider,
            Duration::from_secs(0),
            Duration::from_secs(0),
        );
        let _ = monitor.refresh_snapshot().await;

        {
            let bad_provider = Arc::new(FakeProvider {
                result: Err(SpaceWeatherProviderError::transport("offline")),
            });
            let state = monitor.state.write().await;
            let existing = state.clone().expect("cached snapshot");
            drop(state);
            let monitor = SpaceWeatherMonitor {
                provider: bad_provider,
                refresh_interval: Duration::from_secs(0),
                stale_after: Duration::from_secs(0),
                state: RwLock::new(Some(existing)),
            };

            let snapshot = monitor.refresh_snapshot().await;

            assert_eq!(SpaceWeatherStatus::Stale as i32, snapshot.status);
            assert_eq!(
                Some("Space weather provider transport error: offline".to_string()),
                snapshot.error_message
            );
            assert_eq!(Some(3.0), snapshot.planetary_k_index);
        }
    }
}

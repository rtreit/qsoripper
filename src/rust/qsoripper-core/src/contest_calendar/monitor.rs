//! Cached contest calendar snapshots.

use std::{
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use prost_types::Timestamp;
use tokio::sync::RwLock;

use crate::proto::qsoripper::domain::{ContestCalendarEntry, ContestCalendarStatus};

use super::provider::ContestCalendarProvider;

#[derive(Clone)]
struct CachedSnapshot {
    contests: Vec<ContestCalendarEntry>,
    fetched_at: Timestamp,
    fetched_monotonic: Instant,
}

/// Current contest calendar entries plus freshness metadata.
#[derive(Clone, Default)]
pub struct ContestCalendarSnapshot {
    /// Normalized contest entries.
    pub contests: Vec<ContestCalendarEntry>,
    /// Snapshot freshness/error status.
    pub status: ContestCalendarStatus,
    /// UTC time when this snapshot was fetched.
    pub fetched_at: Option<Timestamp>,
    /// UTC time until which the snapshot should be treated as fresh.
    pub valid_until: Option<Timestamp>,
    /// Human-readable provider error, when present.
    pub error_message: Option<String>,
}

/// Small cache around the current contest calendar provider.
pub struct ContestCalendarMonitor {
    provider: Arc<dyn ContestCalendarProvider>,
    refresh_interval: Duration,
    stale_after: Duration,
    state: RwLock<Option<CachedSnapshot>>,
}

impl ContestCalendarMonitor {
    /// Create a monitor around a provider with refresh and stale thresholds.
    #[must_use]
    pub fn new(
        provider: Arc<dyn ContestCalendarProvider>,
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

    /// Return current contest entries, refreshing when needed.
    pub async fn current_snapshot(&self) -> ContestCalendarSnapshot {
        if let Some(cached) = self.state.read().await.clone() {
            if cached.fetched_monotonic.elapsed() < self.refresh_interval {
                return snapshot_from_cached(&cached, self.refresh_interval, self.stale_after);
            }
        }

        self.refresh_snapshot().await
    }

    /// Force a refresh and return the latest available contest entries.
    pub async fn refresh_snapshot(&self) -> ContestCalendarSnapshot {
        let cached = self.state.read().await.clone();

        match self.provider.fetch_contests().await {
            Ok(contests) => {
                let fetched_at = now_timestamp();
                let cached_snapshot = CachedSnapshot {
                    contests: contests.clone(),
                    fetched_at,
                    fetched_monotonic: Instant::now(),
                };
                *self.state.write().await = Some(cached_snapshot);
                ContestCalendarSnapshot {
                    contests,
                    status: ContestCalendarStatus::Current,
                    valid_until: Some(add_duration(&fetched_at, self.refresh_interval)),
                    fetched_at: Some(fetched_at),
                    error_message: None,
                }
            }
            Err(error) => {
                if let Some(cached) = cached {
                    let mut snapshot =
                        snapshot_from_cached(&cached, self.refresh_interval, self.stale_after);
                    snapshot.status = ContestCalendarStatus::Stale;
                    snapshot.error_message = Some(error.to_string());
                    snapshot
                } else {
                    ContestCalendarSnapshot {
                        status: if error.to_string().contains("disabled error") {
                            ContestCalendarStatus::Disabled
                        } else {
                            ContestCalendarStatus::Error
                        },
                        fetched_at: Some(now_timestamp()),
                        error_message: Some(error.to_string()),
                        ..ContestCalendarSnapshot::default()
                    }
                }
            }
        }
    }
}

fn snapshot_from_cached(
    cached: &CachedSnapshot,
    refresh_interval: Duration,
    stale_after: Duration,
) -> ContestCalendarSnapshot {
    ContestCalendarSnapshot {
        contests: cached.contests.clone(),
        status: if cached.fetched_monotonic.elapsed() >= stale_after {
            ContestCalendarStatus::Stale
        } else {
            ContestCalendarStatus::Current
        },
        fetched_at: Some(cached.fetched_at),
        valid_until: Some(add_duration(&cached.fetched_at, refresh_interval)),
        error_message: None,
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

fn add_duration(timestamp: &Timestamp, duration: Duration) -> Timestamp {
    let seconds = i64::try_from(duration.as_secs()).unwrap_or(i64::MAX);
    Timestamp {
        seconds: timestamp.seconds.saturating_add(seconds),
        nanos: timestamp.nanos,
    }
}

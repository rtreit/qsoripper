//! PTT ownership and arbitration: a single-owner lease with a maximum-transmit safety
//! ceiling (§8.5). Contention is made safe rather than supporting simultaneous transmit.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// Why a PTT key request was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PttDenied {
    /// Another client session currently holds the lease.
    Busy,
    /// The client session lacks the `ptt` capability.
    NotPermitted,
}

struct Inner {
    owner: Option<u64>,
    keyed_at: Option<Instant>,
    max_tx: Duration,
}

/// Arbitrates the single physical transmitter across PTT-capable client sessions.
#[derive(Clone)]
pub(crate) struct PttManager {
    inner: Arc<Mutex<Inner>>,
}

impl PttManager {
    /// Create a manager with the given maximum-transmit safety ceiling.
    pub(crate) fn new(max_tx: Duration) -> Self {
        PttManager {
            inner: Arc::new(Mutex::new(Inner {
                owner: None,
                keyed_at: None,
                max_tx,
            })),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Attempt to acquire (or re-assert) the lease for `session_id`.
    ///
    /// The first capable session to key acquires the lease; while it is held, other sessions
    /// are refused. `has_ptt` reflects the configured endpoint's `ptt` capability.
    pub(crate) fn try_key(&self, session_id: u64, has_ptt: bool) -> Result<(), PttDenied> {
        if !has_ptt {
            return Err(PttDenied::NotPermitted);
        }
        let mut guard = self.lock();
        match guard.owner {
            Some(owner) if owner != session_id => Err(PttDenied::Busy),
            _ => {
                guard.owner = Some(session_id);
                guard.keyed_at = Some(Instant::now());
                Ok(())
            }
        }
    }

    /// Release the lease if `session_id` owns it.
    pub(crate) fn unkey(&self, session_id: u64) {
        let mut guard = self.lock();
        if guard.owner == Some(session_id) {
            guard.owner = None;
            guard.keyed_at = None;
        }
    }

    /// The current PTT owner, if any.
    pub(crate) fn owner(&self) -> Option<u64> {
        self.lock().owner
    }

    /// The current owner iff the maximum-transmit ceiling has elapsed, **without** releasing
    /// the lease.
    ///
    /// The lease is intentionally held until the caller has actually unkeyed the radio (send
    /// `RX;` first, then [`unkey`](Self::unkey)). Releasing here would open a window in which
    /// another session could acquire PTT and then be unkeyed by the caller's delayed `RX;`.
    /// This is a hard transmit-length ceiling, not a CAT-idle timer: a keyed-but-silent
    /// client (WSJT-X mid-over) is not reported until the ceiling.
    pub(crate) fn expired_owner(&self) -> Option<u64> {
        let guard = self.lock();
        if guard.keyed_at.is_some_and(|t| t.elapsed() >= guard.max_tx) {
            guard.owner
        } else {
            None
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn first_capable_session_acquires_lease() {
        let ptt = PttManager::new(Duration::from_secs(300));
        assert_eq!(ptt.try_key(1, true), Ok(()));
        assert_eq!(ptt.owner(), Some(1));
    }

    #[test]
    fn second_session_rejected_while_held() {
        let ptt = PttManager::new(Duration::from_secs(300));
        assert_eq!(ptt.try_key(1, true), Ok(()));
        assert_eq!(ptt.try_key(2, true), Err(PttDenied::Busy));
    }

    #[test]
    fn owner_can_reassert_without_error() {
        let ptt = PttManager::new(Duration::from_secs(300));
        assert_eq!(ptt.try_key(1, true), Ok(()));
        assert_eq!(ptt.try_key(1, true), Ok(()));
    }

    #[test]
    fn lease_releases_on_unkey() {
        let ptt = PttManager::new(Duration::from_secs(300));
        ptt.try_key(1, true).expect("key");
        ptt.unkey(1);
        assert_eq!(ptt.owner(), None);
        assert_eq!(ptt.try_key(2, true), Ok(()));
    }

    #[test]
    fn session_without_capability_is_not_permitted() {
        let ptt = PttManager::new(Duration::from_secs(300));
        assert_eq!(ptt.try_key(1, false), Err(PttDenied::NotPermitted));
        assert_eq!(ptt.owner(), None);
    }

    #[test]
    fn safety_ceiling_releases_a_stuck_transmitter() {
        let ptt = PttManager::new(Duration::from_millis(0));
        ptt.try_key(1, true).expect("key");
        // The ceiling reports the owner but holds the lease until the caller unkeys.
        assert_eq!(ptt.expired_owner(), Some(1));
        assert_eq!(ptt.owner(), Some(1));
        ptt.unkey(1);
        assert_eq!(ptt.owner(), None);
    }

    #[test]
    fn safety_ceiling_does_not_release_before_expiry() {
        let ptt = PttManager::new(Duration::from_secs(300));
        ptt.try_key(1, true).expect("key");
        assert_eq!(ptt.expired_owner(), None);
        assert_eq!(ptt.owner(), Some(1));
    }
}

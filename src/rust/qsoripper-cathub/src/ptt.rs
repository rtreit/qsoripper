//! PTT ownership and arbitration: a single-owner lease with a maximum-transmit safety
//! ceiling (§8.5). Contention is made safe rather than supporting simultaneous transmit.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// Why a PTT key request was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PttDenied {
    /// Another face currently holds the lease.
    Busy,
    /// The face lacks the `ptt` capability.
    NotPermitted,
}

struct Inner {
    owner: Option<u64>,
    keyed_at: Option<Instant>,
    max_tx: Duration,
}

/// Arbitrates the single physical transmitter across PTT-capable faces.
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

    /// Attempt to acquire (or re-assert) the lease for `face`.
    ///
    /// The first capable face to key acquires the lease; while it is held, other faces
    /// are refused. `has_ptt` reflects the face's `ptt` capability.
    pub(crate) fn try_key(&self, face: u64, has_ptt: bool) -> Result<(), PttDenied> {
        if !has_ptt {
            return Err(PttDenied::NotPermitted);
        }
        let mut guard = self.lock();
        match guard.owner {
            Some(owner) if owner != face => Err(PttDenied::Busy),
            _ => {
                guard.owner = Some(face);
                guard.keyed_at = Some(Instant::now());
                Ok(())
            }
        }
    }

    /// Release the lease if `face` owns it.
    pub(crate) fn unkey(&self, face: u64) {
        let mut guard = self.lock();
        if guard.owner == Some(face) {
            guard.owner = None;
            guard.keyed_at = None;
        }
    }

    /// The current PTT owner, if any.
    #[cfg(test)]
    pub(crate) fn owner(&self) -> Option<u64> {
        self.lock().owner
    }

    /// Release the lease if the maximum-transmit ceiling has elapsed.
    ///
    /// Returns the face whose transmission was force-released, if any. This is a hard
    /// transmit-length ceiling, not a CAT-idle timer: a keyed-but-silent client (WSJT-X
    /// mid-over) is not released until the ceiling.
    pub(crate) fn safety_release_if_expired(&self) -> Option<u64> {
        let mut guard = self.lock();
        let expired = guard.keyed_at.is_some_and(|t| t.elapsed() >= guard.max_tx);
        if expired {
            let owner = guard.owner.take();
            guard.keyed_at = None;
            owner
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
    fn first_capable_face_acquires_lease() {
        let ptt = PttManager::new(Duration::from_secs(300));
        assert_eq!(ptt.try_key(1, true), Ok(()));
        assert_eq!(ptt.owner(), Some(1));
    }

    #[test]
    fn second_face_rejected_while_held() {
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
    fn face_without_capability_is_not_permitted() {
        let ptt = PttManager::new(Duration::from_secs(300));
        assert_eq!(ptt.try_key(1, false), Err(PttDenied::NotPermitted));
        assert_eq!(ptt.owner(), None);
    }

    #[test]
    fn safety_ceiling_releases_a_stuck_transmitter() {
        let ptt = PttManager::new(Duration::from_millis(0));
        ptt.try_key(1, true).expect("key");
        assert_eq!(ptt.safety_release_if_expired(), Some(1));
        assert_eq!(ptt.owner(), None);
    }

    #[test]
    fn safety_ceiling_does_not_release_before_expiry() {
        let ptt = PttManager::new(Duration::from_secs(300));
        ptt.try_key(1, true).expect("key");
        assert_eq!(ptt.safety_release_if_expired(), None);
        assert_eq!(ptt.owner(), Some(1));
    }
}

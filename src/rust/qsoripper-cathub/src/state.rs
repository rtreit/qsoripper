//! The universal radio state: a single in-memory snapshot every face reads from, plus a
//! broadcast channel of [`StateChange`] notifications faces subscribe to for auto-info
//! fan-out (design §8.3, §8.4).
//!
//! [`StateHandle`] is synchronous (a plain `Mutex`/`RwLock`): reads and writes are short,
//! uncontended critical sections, so there is no reason to make callers `await`. A change
//! is broadcast **only when a field's value actually changes**, so an idempotent write
//! (re-setting the same frequency) produces no spurious notification.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, PoisonError, RwLock};

use tokio::sync::broadcast;

use crate::model::{Field, Mode, RadioEventSource, StateChange, Vfo};

/// Default IF passband reported for a VFO (Hz). Real backends may refine this; the default
/// matches the canonical Hamlib `get_mode` second line.
const DEFAULT_PASSBAND_HZ: u32 = 2_400;

/// A point-in-time view of one VFO.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct VfoSnapshot {
    /// Frequency in Hz.
    pub(crate) freq_hz: u64,
    /// Operating mode.
    pub(crate) mode: Mode,
    /// IF passband in Hz.
    pub(crate) passband_hz: u32,
}

impl Default for VfoSnapshot {
    fn default() -> Self {
        VfoSnapshot {
            freq_hz: 0,
            mode: Mode::Usb,
            passband_hz: DEFAULT_PASSBAND_HZ,
        }
    }
}

/// A consistent point-in-time view of the whole radio.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)] // Each flag mirrors an independent radio state.
pub(crate) struct Snapshot {
    a: VfoSnapshot,
    b: VfoSnapshot,
    /// The receive VFO.
    pub(crate) rx_vfo: Vfo,
    /// The transmit VFO (relevant when split is enabled).
    pub(crate) tx_vfo: Vfo,
    /// Whether split is enabled.
    pub(crate) split: bool,
    /// Whether the transmitter is keyed.
    pub(crate) ptt: bool,
    /// Whether the radio reports power on.
    pub(crate) power_on: bool,
    /// Whether RIT is enabled.
    pub(crate) rit_enabled: bool,
    /// RIT offset in Hz.
    pub(crate) rit_offset_hz: i32,
    /// Whether XIT is enabled.
    pub(crate) xit_enabled: bool,
    /// XIT offset in Hz.
    pub(crate) xit_offset_hz: i32,
}

impl Default for Snapshot {
    fn default() -> Self {
        Snapshot {
            a: VfoSnapshot::default(),
            b: VfoSnapshot::default(),
            rx_vfo: Vfo::A,
            tx_vfo: Vfo::A,
            split: false,
            ptt: false,
            // A TS-590 answers `\get_powerstat` with "1" at rest.
            power_on: true,
            rit_enabled: false,
            rit_offset_hz: 0,
            xit_enabled: false,
            xit_offset_hz: 0,
        }
    }
}

impl Snapshot {
    /// The view of one VFO.
    pub(crate) fn vfo(&self, vfo: Vfo) -> VfoSnapshot {
        match vfo {
            Vfo::A => self.a,
            Vfo::B => self.b,
        }
    }
}

struct Inner {
    snapshot: RwLock<Snapshot>,
    covered: Mutex<HashSet<Field>>,
    tx: broadcast::Sender<StateChange>,
}

/// A clonable handle to the universal state.
#[derive(Clone)]
pub(crate) struct StateHandle {
    inner: Arc<Inner>,
}

impl StateHandle {
    /// Create an empty state at its defaults.
    pub(crate) fn new() -> Self {
        let (tx, _rx) = broadcast::channel(256);
        StateHandle {
            inner: Arc::new(Inner {
                snapshot: RwLock::new(Snapshot::default()),
                covered: Mutex::new(HashSet::new()),
                tx,
            }),
        }
    }

    /// Record an observed change. The change is applied to the snapshot and broadcast to
    /// subscribers **only if it actually changes a value**. A [`RadioEventSource::NativePush`]
    /// change additionally marks the field as natively covered so the poller can back off.
    pub(crate) fn record(&self, change: StateChange, source: RadioEventSource) {
        if source == RadioEventSource::NativePush {
            self.inner
                .covered
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(change.field());
        }
        let changed = {
            let mut snap = self
                .inner
                .snapshot
                .write()
                .unwrap_or_else(PoisonError::into_inner);
            apply_change(&mut snap, change)
        };
        if changed {
            // A send error only means there are no subscribers; that is fine.
            let _ = self.inner.tx.send(change);
        }
    }

    /// A consistent point-in-time view of the radio.
    pub(crate) fn snapshot(&self) -> Snapshot {
        *self
            .inner
            .snapshot
            .read()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Subscribe to the change stream (for a face's auto-info fan-out).
    pub(crate) fn subscribe(&self) -> broadcast::Receiver<StateChange> {
        self.inner.tx.subscribe()
    }

    /// Whether the radio's native push stream has been observed to cover this field.
    pub(crate) fn is_native_push_covered(&self, field: Field) -> bool {
        self.inner
            .covered
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(&field)
    }
}

/// Apply a change to the snapshot, returning whether any value actually changed.
fn apply_change(snap: &mut Snapshot, change: StateChange) -> bool {
    match change {
        StateChange::Freq { vfo, hz } => {
            let target = vfo_mut(snap, vfo);
            if target.freq_hz == hz {
                false
            } else {
                target.freq_hz = hz;
                true
            }
        }
        StateChange::Mode { vfo, mode } => {
            let target = vfo_mut(snap, vfo);
            if target.mode == mode {
                false
            } else {
                target.mode = mode;
                true
            }
        }
        StateChange::Split { enabled, tx_vfo } => {
            let new_tx = tx_vfo.unwrap_or(snap.tx_vfo);
            if snap.split == enabled && snap.tx_vfo == new_tx {
                false
            } else {
                snap.split = enabled;
                snap.tx_vfo = new_tx;
                true
            }
        }
        StateChange::Ptt { keyed } => {
            if snap.ptt == keyed {
                false
            } else {
                snap.ptt = keyed;
                true
            }
        }
        StateChange::Rit { enabled, offset_hz } => {
            if snap.rit_enabled == enabled && snap.rit_offset_hz == offset_hz {
                false
            } else {
                snap.rit_enabled = enabled;
                snap.rit_offset_hz = offset_hz;
                true
            }
        }
        StateChange::Xit { enabled, offset_hz } => {
            if snap.xit_enabled == enabled && snap.xit_offset_hz == offset_hz {
                false
            } else {
                snap.xit_enabled = enabled;
                snap.xit_offset_hz = offset_hz;
                true
            }
        }
    }
}

fn vfo_mut(snap: &mut Snapshot, vfo: Vfo) -> &mut VfoSnapshot {
    match vfo {
        Vfo::A => &mut snap.a,
        Vfo::B => &mut snap.b,
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sane() {
        let snap = StateHandle::new().snapshot();
        assert!(snap.power_on);
        assert_eq!(snap.vfo(Vfo::A).freq_hz, 0);
        assert_eq!(snap.vfo(Vfo::A).mode, Mode::Usb);
        assert_eq!(snap.vfo(Vfo::A).passband_hz, 2_400);
        assert!(!snap.split && !snap.ptt);
    }

    #[test]
    fn record_updates_snapshot() {
        let state = StateHandle::new();
        state.record(
            StateChange::Freq {
                vfo: Vfo::A,
                hz: 7_030_000,
            },
            RadioEventSource::PollDiff,
        );
        assert_eq!(state.snapshot().vfo(Vfo::A).freq_hz, 7_030_000);
    }

    #[tokio::test]
    async fn change_broadcasts_only_on_actual_change() {
        let state = StateHandle::new();
        let mut rx = state.subscribe();
        // First set: USB == default USB, so NO broadcast.
        state.record(
            StateChange::Mode {
                vfo: Vfo::A,
                mode: Mode::Usb,
            },
            RadioEventSource::PollDiff,
        );
        // A real change: CW differs from USB, so exactly one frame.
        state.record(
            StateChange::Mode {
                vfo: Vfo::A,
                mode: Mode::Cw,
            },
            RadioEventSource::PollDiff,
        );
        let got = rx.try_recv().expect("one change");
        assert_eq!(
            got,
            StateChange::Mode {
                vfo: Vfo::A,
                mode: Mode::Cw
            }
        );
        assert!(rx.try_recv().is_err(), "no second frame for the no-op set");
    }

    #[test]
    fn native_push_marks_coverage_but_poll_diff_does_not() {
        let state = StateHandle::new();
        assert!(!state.is_native_push_covered(Field::Freq(Vfo::A)));
        state.record(
            StateChange::Freq { vfo: Vfo::A, hz: 1 },
            RadioEventSource::PollDiff,
        );
        assert!(!state.is_native_push_covered(Field::Freq(Vfo::A)));
        state.record(
            StateChange::Freq { vfo: Vfo::A, hz: 2 },
            RadioEventSource::NativePush,
        );
        assert!(state.is_native_push_covered(Field::Freq(Vfo::A)));
    }

    #[test]
    fn split_and_rit_xit_round_trip() {
        let state = StateHandle::new();
        state.record(
            StateChange::Split {
                enabled: true,
                tx_vfo: Some(Vfo::B),
            },
            RadioEventSource::OptimisticWrite,
        );
        state.record(
            StateChange::Rit {
                enabled: true,
                offset_hz: 100,
            },
            RadioEventSource::OptimisticWrite,
        );
        state.record(
            StateChange::Xit {
                enabled: true,
                offset_hz: -50,
            },
            RadioEventSource::OptimisticWrite,
        );
        let snap = state.snapshot();
        assert!(snap.split && snap.tx_vfo == Vfo::B);
        assert!(snap.rit_enabled && snap.rit_offset_hz == 100);
        assert!(snap.xit_enabled && snap.xit_offset_hz == -50);
    }
}

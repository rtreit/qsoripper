//! The universal radio state: a single in-memory snapshot every endpoint reads from, plus a
//! broadcast channel of [`StateChange`] notifications endpoints subscribe to for auto-info
//! fan-out (design §8.3, §8.4).
//!
//! [`StateHandle`] is synchronous (a plain `Mutex`/`RwLock`): reads and writes are short,
//! uncontended critical sections, so there is no reason to make callers `await`. A change
//! is broadcast **only when a field's value actually changes**, so an idempotent write
//! (re-setting the same frequency) produces no spurious notification.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, PoisonError, RwLock};

use tokio::sync::broadcast;

use crate::model::{Field, Mode, RadioEventSource, StateChange, StateMutation, TxPower, Vfo};

/// A cached mode fact that may have been invalidated by a frequency change. The TS-590
/// recalls mode and DATA independently from its per-band memory, so both facts must be
/// re-asserted after tuning even when the pre-tune snapshot already held the requested mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum ModeFact {
    Base(Vfo),
    Data(Vfo),
}

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
    /// Whether the backend has observed this VFO's base mode.
    pub(crate) mode_known: bool,
    /// Whether the DATA sub-mode flag (TS-590 `DA`) is on for this VFO.
    pub(crate) data: bool,
    /// IF passband in Hz.
    pub(crate) passband_hz: u32,
}

impl Default for VfoSnapshot {
    fn default() -> Self {
        VfoSnapshot {
            freq_hz: 0,
            mode: Mode::Usb,
            mode_known: false,
            data: false,
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
    /// Configured transmitter output power, when the backend can expose it.
    pub(crate) tx_power: Option<TxPower>,
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
            tx_power: None,
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

    /// Whether applying `mutation` would leave the radio unchanged from this snapshot.
    ///
    /// The hub forwards a modeled write to the wire only when it would actually change the
    /// radio. Re-sending a value the radio already holds is not just wasted I/O: on the
    /// TS-590 every redundant `MD`/`FA` set makes the radio emit its PC-control beep (a
    /// Morse "U"), which is why clients like WSJT-X — that re-assert mode/frequency on every
    /// poll — chirp the radio through the hub but not through a native Hamlib driver, which
    /// caches state and never re-sends an unchanged value. Suppressing the no-op keeps the
    /// hub as quiet on the wire as the native driver. PTT is never redundant: keying and
    /// unkeying must always reach the radio and participate in the single-owner lease.
    pub(crate) fn is_redundant(&self, mutation: &StateMutation) -> bool {
        match *mutation {
            StateMutation::SetRxVfo { vfo } => self.rx_vfo == vfo,
            StateMutation::SetVfoFreq { vfo, hz } => self.vfo(vfo).freq_hz == hz,
            // Compare by the digit actually written to the radio, not the enum identity.
            // WSJT-X asserts "PKTUSB", which decomposes into a base mode of USB (sent as MD2)
            // plus a separate DATA flag handled by `SetDataMode` below. The TS-590 beeps on
            // every mode set it receives (frequency sets are silent), so suppressing the no-op
            // MD frame is what keeps it quiet, exactly like a native driver that never re-sends
            // an unchanged mode.
            StateMutation::SetMode { mode, .. } => {
                self.vfo(self.rx_vfo).mode.to_kenwood_digit() == mode.to_kenwood_digit()
            }
            // The DATA flag (`DA`) is a separate wire fact from the base mode (`MD`): suppress
            // a re-assert of the value the radio already holds so toggling, e.g., FT8↔WSPR
            // (both PKTUSB) writes nothing after the first, and selecting a plain mode only
            // emits `DA0` when DATA was actually on.
            StateMutation::SetDataMode { vfo, on } => self.vfo(vfo).data == on,
            StateMutation::SetSplit { enabled, tx_vfo } => {
                self.split == enabled && self.tx_vfo == tx_vfo.unwrap_or(self.tx_vfo)
            }
            StateMutation::SetRit { enabled, offset_hz } => {
                self.rit_enabled == enabled && self.rit_offset_hz == offset_hz
            }
            StateMutation::SetXit { enabled, offset_hz } => {
                self.xit_enabled == enabled && self.xit_offset_hz == offset_hz
            }
            StateMutation::SetPtt { .. } => false,
        }
    }

    /// Decompose this snapshot into the full ordered list of [`StateChange`]s that
    /// reconstruct it.
    ///
    /// Used to re-synchronize a client session that fell behind the broadcast ring
    /// ([`RecvError::Lagged`](tokio::sync::broadcast::error::RecvError::Lagged)): replaying
    /// these through the dialect's notification formatter restores the client to the current
    /// radio state even when a one-shot event (a mode or VFO change) was evicted from the
    /// ring before the endpoint read it. `RxVfo` is emitted last so a foreign dialect's
    /// VFO-switch frame (which leads with the active `FA`/`MD`) reflects the final state.
    pub(crate) fn as_changes(&self) -> Vec<StateChange> {
        vec![
            StateChange::Freq {
                vfo: Vfo::A,
                hz: self.a.freq_hz,
            },
            StateChange::Freq {
                vfo: Vfo::B,
                hz: self.b.freq_hz,
            },
            StateChange::Mode {
                vfo: Vfo::A,
                mode: self.a.mode,
            },
            StateChange::Mode {
                vfo: Vfo::B,
                mode: self.b.mode,
            },
            StateChange::DataMode {
                vfo: Vfo::A,
                on: self.a.data,
            },
            StateChange::DataMode {
                vfo: Vfo::B,
                on: self.b.data,
            },
            StateChange::Split {
                enabled: self.split,
                tx_vfo: Some(self.tx_vfo),
            },
            StateChange::Rit {
                enabled: self.rit_enabled,
                offset_hz: self.rit_offset_hz,
            },
            StateChange::Xit {
                enabled: self.xit_enabled,
                offset_hz: self.xit_offset_hz,
            },
            StateChange::Ptt { keyed: self.ptt },
            StateChange::TxPower {
                power: self.tx_power,
            },
            StateChange::RxVfo { vfo: self.rx_vfo },
        ]
    }
}

/// An ordered radio-output event delivered to endpoints for auto-information fan-out.
///
/// Both modeled changes and unmodeled native frames travel on the **same** broadcast so
/// endpoints observe them in the order the radio produced them — important for native
/// pass-through clients that consume the CAT stream directly.
#[derive(Debug, Clone)]
pub(crate) enum RadioEvent {
    /// A coalesced modeled state change (poll diff or native push).
    Change(StateChange),
    /// An unsolicited native frame the backend does not model, forwarded verbatim to
    /// native pass-through endpoints so client-side feature state machines (for example
    /// ARCP-590's NB on/NB1/NB2/off cycle) and front-panel changes stay in sync.
    Raw(Arc<[u8]>),
    /// A native frame the backend *did* model, forwarded verbatim for transparent mirror
    /// endpoints (ARCP-590). Virtualizing endpoints ignore it — they consume the coalesced
    /// [`RadioEvent::Change`] instead — but a transparent endpoint relays it so it tracks the
    /// radio's real CAT stream rather than a synthesis, eliminating push/snapshot drift.
    RawNative(Arc<[u8]>),
}

struct Inner {
    snapshot: RwLock<Snapshot>,
    covered: Mutex<HashSet<Field>>,
    uncertain_mode: Mutex<HashSet<ModeFact>>,
    tx: broadcast::Sender<RadioEvent>,
}

/// A clonable handle to the universal state.
#[derive(Clone)]
pub(crate) struct StateHandle {
    inner: Arc<Inner>,
}

impl StateHandle {
    /// Create an empty state at its defaults.
    pub(crate) fn new() -> Self {
        // Capacity headroom: a modeled native frame now broadcasts both a coalesced `Change`
        // and a verbatim `RawNative`, so the bus carries up to two events per radio frame.
        // A larger ring keeps a momentarily busy endpoint (notably a transparent mirror during a
        // contest-rate tuning sweep) from lagging and having to re-sync.
        let (tx, _rx) = broadcast::channel(1024);
        StateHandle {
            inner: Arc::new(Inner {
                snapshot: RwLock::new(Snapshot::default()),
                covered: Mutex::new(HashSet::new()),
                uncertain_mode: Mutex::new(HashSet::new()),
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

        // A TS-590 frequency set can cross a band boundary and synchronously recall that
        // band's stored MD/DA settings. Until fresh MD and DA facts arrive, the old cached
        // mode must not suppress a client's immediately following mode re-assertion (the
        // normal WSJT-X band-change sequence is F then M). Each fact becomes certain again
        // as soon as it is observed or successfully written, even when its value compares
        // equal and therefore produced no broadcast change.
        let mut uncertain = self
            .inner
            .uncertain_mode
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        match change {
            StateChange::Freq { vfo, .. }
                if changed && source == RadioEventSource::OptimisticWrite =>
            {
                uncertain.insert(ModeFact::Base(vfo));
                uncertain.insert(ModeFact::Data(vfo));
            }
            StateChange::Mode { vfo, .. } => {
                uncertain.remove(&ModeFact::Base(vfo));
            }
            StateChange::DataMode { vfo, .. } => {
                uncertain.remove(&ModeFact::Data(vfo));
            }
            _ => {}
        }
        drop(uncertain);
        if changed {
            // A send error only means there are no subscribers; that is fine.
            let _ = self.inner.tx.send(RadioEvent::Change(change));
        }
    }

    /// Broadcast an unsolicited native frame the backend does not model, so native
    /// pass-through endpoints can forward it verbatim. This does not touch the snapshot or the
    /// native-push coverage set; it is a transparent relay of the radio's CAT stream.
    pub(crate) fn record_raw(&self, frame: &[u8]) {
        let _ = self.inner.tx.send(RadioEvent::Raw(Arc::from(frame)));
    }

    /// Broadcast a *modeled* native frame verbatim for transparent mirror endpoints. The caller
    /// has already recorded the modeled change (updating the snapshot and broadcasting a
    /// coalesced [`RadioEvent::Change`]); this additionally relays the original bytes so a
    /// transparent endpoint mirrors the radio's exact CAT stream. Non-transparent endpoints ignore
    /// this event. It does not touch the snapshot or the native-push coverage set.
    pub(crate) fn record_raw_native(&self, frame: &[u8]) {
        let _ = self.inner.tx.send(RadioEvent::RawNative(Arc::from(frame)));
    }

    /// A consistent point-in-time view of the radio.
    pub(crate) fn snapshot(&self) -> Snapshot {
        *self
            .inner
            .snapshot
            .read()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether a modeled write is safely redundant against the current cache.
    ///
    /// Mode facts invalidated by an optimistic frequency change are deliberately treated as
    /// non-redundant until the radio reports them or a client re-asserts them. Other fields
    /// retain the snapshot's ordinary idempotent suppression.
    pub(crate) fn is_redundant(&self, mutation: &StateMutation) -> bool {
        let uncertain = self
            .inner
            .uncertain_mode
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let invalidated = match *mutation {
            StateMutation::SetMode { vfo, .. } => uncertain.contains(&ModeFact::Base(vfo)),
            StateMutation::SetDataMode { vfo, .. } => uncertain.contains(&ModeFact::Data(vfo)),
            _ => false,
        };
        !invalidated && self.snapshot().is_redundant(mutation)
    }

    /// Subscribe to the radio-output event stream for a client session's auto-info fan-out.
    pub(crate) fn subscribe(&self) -> broadcast::Receiver<RadioEvent> {
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
        StateChange::RxVfo { vfo } => {
            if snap.rx_vfo == vfo {
                false
            } else {
                snap.rx_vfo = vfo;
                true
            }
        }
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
            let changed = target.mode != mode;
            target.mode = mode;
            target.mode_known = true;
            changed
        }
        StateChange::DataMode { vfo, on } => {
            let target = vfo_mut(snap, vfo);
            if target.data == on {
                false
            } else {
                target.data = on;
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
        StateChange::TxPower { power } => {
            if snap.tx_power == power {
                false
            } else {
                snap.tx_power = power;
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
    use crate::model::PttSource;

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

    #[test]
    fn is_redundant_detects_unchanged_values() {
        let state = StateHandle::new();
        state.record(
            StateChange::Freq {
                vfo: Vfo::A,
                hz: 7_030_000,
            },
            RadioEventSource::PollDiff,
        );
        let snap = state.snapshot();

        // Re-setting the value the radio already holds is redundant.
        assert!(snap.is_redundant(&StateMutation::SetVfoFreq {
            vfo: Vfo::A,
            hz: 7_030_000,
        }));
        assert!(!snap.is_redundant(&StateMutation::SetVfoFreq {
            vfo: Vfo::A,
            hz: 7_040_000,
        }));

        // Mode is tracked against the receive VFO (default A, USB).
        assert!(snap.is_redundant(&StateMutation::SetMode {
            vfo: Vfo::A,
            mode: Mode::Usb,
        }));
        // An Unknown mode still writes the USB digit (MD2), so a SetMode(Unknown) on a
        // USB VFO is the same frame the radio already holds and must be redundant.
        assert!(snap.is_redundant(&StateMutation::SetMode {
            vfo: Vfo::A,
            mode: Mode::Unknown,
        }));
        assert!(!snap.is_redundant(&StateMutation::SetMode {
            vfo: Vfo::A,
            mode: Mode::Cw,
        }));
    }

    #[test]
    fn is_redundant_and_round_trip_for_data_mode() {
        let state = StateHandle::new();
        let snap = state.snapshot();
        // DATA defaults off, so SetDataMode { on: false } is a no-op and on: true is a change.
        assert!(snap.is_redundant(&StateMutation::SetDataMode {
            vfo: Vfo::A,
            on: false,
        }));
        assert!(!snap.is_redundant(&StateMutation::SetDataMode {
            vfo: Vfo::A,
            on: true,
        }));

        // Recording the change flips the snapshot and inverts which write is redundant.
        state.record(
            StateChange::DataMode {
                vfo: Vfo::A,
                on: true,
            },
            RadioEventSource::OptimisticWrite,
        );
        let snap = state.snapshot();
        assert!(snap.vfo(Vfo::A).data);
        assert!(snap.is_redundant(&StateMutation::SetDataMode {
            vfo: Vfo::A,
            on: true,
        }));
        assert!(!snap.is_redundant(&StateMutation::SetDataMode {
            vfo: Vfo::A,
            on: false,
        }));
    }

    #[test]
    fn is_redundant_covers_split_rit_xit() {
        let snap = StateHandle::new().snapshot();
        // Defaults: split off, RIT/XIT off at zero offset.
        assert!(snap.is_redundant(&StateMutation::SetSplit {
            enabled: false,
            tx_vfo: None,
        }));
        assert!(!snap.is_redundant(&StateMutation::SetSplit {
            enabled: true,
            tx_vfo: Some(Vfo::B),
        }));
        assert!(snap.is_redundant(&StateMutation::SetRit {
            enabled: false,
            offset_hz: 0,
        }));
        assert!(!snap.is_redundant(&StateMutation::SetRit {
            enabled: true,
            offset_hz: 100,
        }));
        assert!(snap.is_redundant(&StateMutation::SetXit {
            enabled: false,
            offset_hz: 0,
        }));
        assert!(!snap.is_redundant(&StateMutation::SetXit {
            enabled: true,
            offset_hz: -50,
        }));
    }

    #[test]
    fn is_redundant_never_suppresses_ptt() {
        let snap = StateHandle::new().snapshot();
        // PTT is off by default, but an unkey must still always reach the radio.
        assert!(!snap.is_redundant(&StateMutation::SetPtt {
            keyed: false,
            source: PttSource::Generic,
        }));
        assert!(!snap.is_redundant(&StateMutation::SetPtt {
            keyed: true,
            source: PttSource::Generic,
        }));
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
        assert!(
            matches!(
                got,
                RadioEvent::Change(StateChange::Mode {
                    vfo: Vfo::A,
                    mode: Mode::Cw
                })
            ),
            "expected a single CW mode change, got {got:?}"
        );
        assert!(rx.try_recv().is_err(), "no second frame for the no-op set");
    }

    #[tokio::test]
    async fn record_raw_broadcasts_verbatim_without_touching_snapshot() {
        let state = StateHandle::new();
        let mut rx = state.subscribe();
        state.record_raw(b"NB1;");
        let evt = rx.try_recv().expect("one raw event");
        assert!(
            matches!(&evt, RadioEvent::Raw(bytes) if &**bytes == b"NB1;"),
            "expected RadioEvent::Raw(NB1;), got {evt:?}"
        );
        // A raw relay must not mark native-push coverage.
        assert!(!state.is_native_push_covered(Field::Freq(Vfo::A)));
    }

    #[tokio::test]
    async fn record_raw_native_broadcasts_verbatim_without_touching_snapshot() {
        let state = StateHandle::new();
        let mut rx = state.subscribe();
        state.record_raw_native(b"FA00014035000;");
        let evt = rx.try_recv().expect("one raw-native event");
        assert!(
            matches!(&evt, RadioEvent::RawNative(bytes) if &**bytes == b"FA00014035000;"),
            "expected RadioEvent::RawNative(FA...), got {evt:?}"
        );
        // Relaying the verbatim frame must not itself mutate the snapshot or coverage; the
        // caller has already recorded the modeled change.
        assert_eq!(state.snapshot().vfo(Vfo::A).freq_hz, 0);
        assert!(!state.is_native_push_covered(Field::Freq(Vfo::A)));
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

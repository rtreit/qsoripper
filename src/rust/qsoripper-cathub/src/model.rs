//! Core domain types shared across the hub: VFO selection, operating mode, the
//! universal [`StateMutation`]/[`StateChange`] pair, the [`Field`] coverage key,
//! and the [`RadioEventSource`] provenance tag.
//!
//! These are backend- and dialect-independent. Backends map their native wire
//! vocabulary to and from these types; dialects render them into client
//! vocabularies. Keeping them here (not in `backend`) lets every layer depend on
//! one neutral vocabulary.

use std::fmt;

/// Identifies one of the radio's two VFOs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Vfo {
    /// VFO A.
    A,
    /// VFO B.
    B,
}

impl fmt::Display for Vfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Vfo::A => f.write_str("A"),
            Vfo::B => f.write_str("B"),
        }
    }
}

/// Operating mode, normalized across radio families.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Mode {
    /// Lower sideband.
    Lsb,
    /// Upper sideband.
    Usb,
    /// CW (normal).
    Cw,
    /// CW reverse.
    CwR,
    /// FSK / RTTY.
    Fsk,
    /// FSK / RTTY reverse.
    FskR,
    /// AM.
    Am,
    /// FM.
    Fm,
    /// An unrecognized mode (treated as USB on the wire).
    Unknown,
}

impl Mode {
    /// Render the mode as its Kenwood `MD` digit byte (ASCII).
    pub(crate) fn to_kenwood_digit(self) -> u8 {
        match self {
            Mode::Lsb => b'1',
            // An unknown mode falls back to USB so a write still produces a valid frame.
            Mode::Usb | Mode::Unknown => b'2',
            Mode::Cw => b'3',
            Mode::Fm => b'4',
            Mode::Am => b'5',
            Mode::Fsk => b'6',
            Mode::CwR => b'7',
            Mode::FskR => b'9',
        }
    }

    /// Parse a Kenwood `MD` digit byte into a mode.
    pub(crate) fn from_kenwood_digit(digit: u8) -> Mode {
        match digit {
            b'1' => Mode::Lsb,
            b'2' => Mode::Usb,
            b'3' => Mode::Cw,
            b'4' => Mode::Fm,
            b'5' => Mode::Am,
            b'6' => Mode::Fsk,
            b'7' => Mode::CwR,
            b'9' => Mode::FskR,
            _ => Mode::Unknown,
        }
    }

    /// Render the mode as a Hamlib `rigctld` mode token.
    pub(crate) fn hamlib_token(self) -> &'static str {
        match self {
            Mode::Lsb => "LSB",
            Mode::Usb | Mode::Unknown => "USB",
            Mode::Cw => "CW",
            Mode::CwR => "CWR",
            Mode::Fsk => "RTTY",
            Mode::FskR => "RTTYR",
            Mode::Am => "AM",
            Mode::Fm => "FM",
        }
    }

    /// Parse a Hamlib `rigctld` mode token into a mode.
    pub(crate) fn from_hamlib_token(token: &str) -> Mode {
        match token.trim() {
            "LSB" => Mode::Lsb,
            "USB" => Mode::Usb,
            "CW" => Mode::Cw,
            "CWR" => Mode::CwR,
            "RTTY" => Mode::Fsk,
            "RTTYR" => Mode::FskR,
            "AM" => Mode::Am,
            "FM" => Mode::Fm,
            _ => Mode::Unknown,
        }
    }
}

/// Which transmit audio path a PTT key request selects.
///
/// This mirrors Hamlib's `RIG_PTT_ON*` family and the Kenwood `TX`/`TX0`/`TX1`
/// commands. Digital-mode clients such as WSJT-X request [`PttSource::Data`]
/// (`T 3` / `RIG_PTT_ON_DATA`) so the radio modulates from the DATA/USB audio
/// input rather than the microphone, and so the TS-590 does not emit the
/// data-confirmation beep produced by a bare `TX;`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum PttSource {
    /// Generic PTT (Hamlib `RIG_PTT_ON`, Kenwood `TX;`).
    #[default]
    Generic,
    /// Microphone audio path (Hamlib `RIG_PTT_ON_MIC`, Kenwood `TX0;`).
    Mic,
    /// Data/USB audio path (Hamlib `RIG_PTT_ON_DATA`, Kenwood `TX1;`).
    Data,
}

/// A single normalized change to apply to the radio (a write intent).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(clippy::enum_variant_names)] // The `Set` prefix names the write intent uniformly.
pub(crate) enum StateMutation {
    /// Set a VFO's frequency in Hz.
    SetVfoFreq {
        /// Target VFO.
        vfo: Vfo,
        /// Frequency in Hz.
        hz: u64,
    },
    /// Set a VFO's mode.
    SetMode {
        /// Target VFO.
        vfo: Vfo,
        /// Mode to set.
        mode: Mode,
    },
    /// Enable or disable split, optionally choosing the TX VFO.
    SetSplit {
        /// Whether split is enabled.
        enabled: bool,
        /// The transmit VFO when split is enabled.
        tx_vfo: Option<Vfo>,
    },
    /// Key or unkey the transmitter.
    SetPtt {
        /// Whether the transmitter should be keyed.
        keyed: bool,
        /// The transmit audio path to select when keying (ignored when unkeying).
        source: PttSource,
    },
    /// Set the RIT offset in Hz (a zero offset disables RIT).
    SetRit {
        /// Offset in Hz.
        offset_hz: i32,
        /// Whether RIT is enabled.
        enabled: bool,
    },
    /// Set the XIT offset in Hz (a zero offset disables XIT).
    SetXit {
        /// Offset in Hz.
        offset_hz: i32,
        /// Whether XIT is enabled.
        enabled: bool,
    },
}

impl StateMutation {
    /// The observable [`StateChange`] this mutation produces once applied.
    pub(crate) fn into_change(self) -> StateChange {
        match self {
            StateMutation::SetVfoFreq { vfo, hz } => StateChange::Freq { vfo, hz },
            StateMutation::SetMode { vfo, mode } => StateChange::Mode { vfo, mode },
            StateMutation::SetSplit { enabled, tx_vfo } => StateChange::Split { enabled, tx_vfo },
            StateMutation::SetPtt { keyed, .. } => StateChange::Ptt { keyed },
            StateMutation::SetRit { offset_hz, enabled } => StateChange::Rit { enabled, offset_hz },
            StateMutation::SetXit { offset_hz, enabled } => StateChange::Xit { enabled, offset_hz },
        }
    }
}

/// A single observed change to the universal state, broadcast to faces for AI
/// fan-out and recorded into the [`Snapshot`](crate::state::Snapshot).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StateChange {
    /// A VFO frequency changed.
    Freq {
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
    /// Split state changed.
    Split {
        /// Whether split is enabled.
        enabled: bool,
        /// The transmit VFO when split is enabled.
        tx_vfo: Option<Vfo>,
    },
    /// PTT (transmit) state changed.
    Ptt {
        /// Whether the transmitter is keyed.
        keyed: bool,
    },
    /// RIT state changed.
    Rit {
        /// Whether RIT is enabled.
        enabled: bool,
        /// Offset in Hz.
        offset_hz: i32,
    },
    /// XIT state changed.
    Xit {
        /// Whether XIT is enabled.
        enabled: bool,
        /// Offset in Hz.
        offset_hz: i32,
    },
}

impl StateChange {
    /// The coverage [`Field`] this change updates.
    pub(crate) fn field(&self) -> Field {
        match *self {
            StateChange::Freq { vfo, .. } => Field::Freq(vfo),
            StateChange::Mode { vfo, .. } => Field::Mode(vfo),
            StateChange::Split { .. } => Field::Split,
            StateChange::Ptt { .. } => Field::Ptt,
            StateChange::Rit { .. } => Field::Rit,
            StateChange::Xit { .. } => Field::Xit,
        }
    }
}

/// A coverage key identifying one observable field, used to decide whether the
/// radio's native push stream covers a field (so the baseline poller can back off).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Field {
    /// A VFO frequency.
    Freq(Vfo),
    /// A VFO mode.
    Mode(Vfo),
    /// Split state.
    Split,
    /// PTT state.
    Ptt,
    /// RIT state.
    Rit,
    /// XIT state.
    Xit,
    /// S-meter reading. Reserved for backends that surface signal strength.
    #[allow(dead_code)]
    SMeter,
    /// Output power. Reserved for backends that surface power.
    #[allow(dead_code)]
    Power,
}

/// Where a state change originated, so the poller can distinguish radio-driven
/// native push (which warrants backing off) from poll-derived diffs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RadioEventSource {
    /// An unsolicited frame the radio pushed on its own (auto-information).
    NativePush,
    /// A difference observed during a baseline poll cycle.
    PollDiff,
    /// An optimistic write reflected immediately after the backend acknowledged it.
    OptimisticWrite,
    /// A value confirmed by a verifying read-back. Reserved for verify-after-write.
    #[allow(dead_code)]
    VerifyRead,
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn kenwood_mode_digits_round_trip() {
        for mode in [
            Mode::Lsb,
            Mode::Usb,
            Mode::Cw,
            Mode::Fm,
            Mode::Am,
            Mode::Fsk,
            Mode::CwR,
            Mode::FskR,
        ] {
            assert_eq!(Mode::from_kenwood_digit(mode.to_kenwood_digit()), mode);
        }
    }

    #[test]
    fn unknown_kenwood_digit_maps_to_unknown_and_back_to_usb() {
        assert_eq!(Mode::from_kenwood_digit(b'0'), Mode::Unknown);
        assert_eq!(Mode::Unknown.to_kenwood_digit(), b'2');
    }

    #[test]
    fn hamlib_tokens_round_trip() {
        for mode in [
            Mode::Lsb,
            Mode::Usb,
            Mode::Cw,
            Mode::CwR,
            Mode::Fsk,
            Mode::FskR,
            Mode::Am,
            Mode::Fm,
        ] {
            assert_eq!(Mode::from_hamlib_token(mode.hamlib_token()), mode);
        }
        assert_eq!(Mode::from_hamlib_token("WAT"), Mode::Unknown);
    }

    #[test]
    fn vfo_display_renders_letter() {
        assert_eq!(Vfo::A.to_string(), "A");
        assert_eq!(Vfo::B.to_string(), "B");
    }

    #[test]
    fn mutation_into_change_maps_each_variant() {
        assert_eq!(
            StateMutation::SetVfoFreq {
                vfo: Vfo::A,
                hz: 7_000_000
            }
            .into_change(),
            StateChange::Freq {
                vfo: Vfo::A,
                hz: 7_000_000
            }
        );
        assert_eq!(
            StateMutation::SetPtt {
                keyed: true,
                source: PttSource::Generic,
            }
            .into_change(),
            StateChange::Ptt { keyed: true }
        );
        assert_eq!(
            StateMutation::SetRit {
                offset_hz: 50,
                enabled: true
            }
            .into_change(),
            StateChange::Rit {
                enabled: true,
                offset_hz: 50
            }
        );
    }

    #[test]
    fn change_field_keys_are_stable() {
        assert_eq!(
            StateChange::Freq { vfo: Vfo::B, hz: 1 }.field(),
            Field::Freq(Vfo::B)
        );
        assert_eq!(
            StateChange::Xit {
                enabled: false,
                offset_hz: 0
            }
            .field(),
            Field::Xit
        );
    }
}

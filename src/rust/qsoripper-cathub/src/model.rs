//! Core domain enums shared across the hub: VFO selection and operating mode.

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
///
/// The numeric encoding mirrors the Kenwood `MD` command digits so the TS-590
/// backend maps modes with a simple table while remaining a backend-independent
/// universal value for dialects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Mode {
    /// Lower sideband.
    Lsb,
    /// Upper sideband.
    Usb,
    /// CW (normal).
    Cw,
    /// FM.
    Fm,
    /// AM.
    Am,
    /// FSK / RTTY.
    Fsk,
    /// CW reverse.
    CwR,
    /// FSK reverse.
    FskR,
}

impl Mode {
    /// Render the mode as its Kenwood `MD` digit.
    pub(crate) fn to_kenwood_digit(self) -> char {
        match self {
            Mode::Lsb => '1',
            Mode::Usb => '2',
            Mode::Cw => '3',
            Mode::Fm => '4',
            Mode::Am => '5',
            Mode::Fsk => '6',
            Mode::CwR => '7',
            Mode::FskR => '9',
        }
    }

    /// Parse a Kenwood `MD` digit into a mode.
    pub(crate) fn from_kenwood_digit(digit: char) -> Option<Self> {
        match digit {
            '1' => Some(Mode::Lsb),
            '2' => Some(Mode::Usb),
            '3' => Some(Mode::Cw),
            '4' => Some(Mode::Fm),
            '5' => Some(Mode::Am),
            '6' => Some(Mode::Fsk),
            '7' => Some(Mode::CwR),
            '9' => Some(Mode::FskR),
            _ => None,
        }
    }
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
            let digit = mode.to_kenwood_digit();
            assert_eq!(Mode::from_kenwood_digit(digit), Some(mode));
        }
    }

    #[test]
    fn unknown_kenwood_digit_is_none() {
        assert_eq!(Mode::from_kenwood_digit('0'), None);
        assert_eq!(Mode::from_kenwood_digit('8'), None);
    }

    #[test]
    fn vfo_display_renders_letter() {
        assert_eq!(Vfo::A.to_string(), "A");
        assert_eq!(Vfo::B.to_string(), "B");
    }
}

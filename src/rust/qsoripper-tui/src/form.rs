//! QSO entry form state and field navigation.

use chrono::Utc;

/// Band names in display order, used as ADIF band strings.
pub(crate) const BANDS: &[&str] = &[
    "160M", "80M", "60M", "40M", "30M", "20M", "17M", "15M", "12M", "10M", "6M", "2M", "70CM",
];

/// Mode names in display order, used as ADIF mode strings.
pub(crate) const MODES: &[&str] = &["SSB", "CW", "FT8", "FT4", "RTTY", "PSK31", "AM", "FM"];

/// Default center frequency in MHz for each entry in [`BANDS`], in the same order.
pub(crate) const BAND_DEFAULT_FREQS: &[f64] = &[
    1.900, 3.750, 5.330, 7.150, 10.125, 14.225, 18.100, 21.200, 24.940, 28.400, 50.125, 146.520,
    446.000,
];

/// Index of 20M in [`BANDS`] — used as the startup default.
const DEFAULT_BAND_IDX: usize = 5;

/// Focusable fields in the QSO entry form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Field {
    /// Worked callsign.
    Callsign,
    /// Band selector (cycles through [`BANDS`]).
    Band,
    /// Mode selector (cycles through [`MODES`]).
    Mode,
    /// RST sent report.
    RstSent,
    /// RST received report.
    RstRcvd,
    /// Short comment.
    Comment,
    /// Operator notes.
    Notes,
    /// Frequency in MHz (free-text).
    FrequencyMhz,
    /// UTC date (`YYYY-MM-DD`).
    Date,
    /// UTC time on / start (`HH:MM`).
    Time,
    /// UTC time off / end (`HH:MM`).
    TimeOff,
    /// Worked station QTH (city/location).
    Qth,
    // Advanced fields shown in the advanced view.
    /// Transmitter power.
    TxPower,
    /// Submode override (e.g., USB, LSB, FT4).
    Submode,
    /// Contest identifier.
    ContestId,
    /// Serial number sent.
    SerialSent,
    /// Serial number received.
    SerialRcvd,
    /// Exchange sent.
    ExchangeSent,
    /// Exchange received.
    ExchangeRcvd,
    // Advanced page 2
    /// Propagation mode (ADIF `PROP_MODE`).
    PropMode,
    /// Satellite name.
    SatName,
    /// Satellite mode.
    SatMode,
    /// IOTA designator.
    Iota,
    /// ARRL section.
    ArrlSection,
    /// Worked state (US state abbreviation).
    WorkedState,
    /// Worked county.
    WorkedCounty,
}

/// Primary navigation order for Tab/Shift-Tab in the log entry view.
const FIELD_ORDER: &[Field] = &[
    Field::Callsign,
    Field::Band,
    Field::Mode,
    Field::RstSent,
    Field::RstRcvd,
    Field::Comment,
    Field::Notes,
    Field::FrequencyMhz,
    Field::Date,
    Field::Time,
    Field::TimeOff,
    Field::Qth,
];

/// Navigation order for Tab/Shift-Tab in the advanced view (both pages combined).
const ADVANCED_FIELD_ORDER: &[Field] = &[
    // Page 1 — power, contest
    Field::TxPower,
    Field::Submode,
    Field::ContestId,
    Field::SerialSent,
    Field::SerialRcvd,
    Field::ExchangeSent,
    Field::ExchangeRcvd,
    // Page 2 — propagation, awards
    Field::PropMode,
    Field::SatName,
    Field::SatMode,
    Field::Iota,
    Field::ArrlSection,
    Field::WorkedState,
    Field::WorkedCounty,
];

/// State of the QSO entry form (basic + advanced fields).
#[derive(Clone)]
pub(crate) struct LogForm {
    /// Currently focused field.
    pub(crate) focused: Field,
    /// Worked callsign text.
    pub(crate) callsign: String,
    /// Index into [`BANDS`].
    pub(crate) band_idx: usize,
    /// Index into [`MODES`].
    pub(crate) mode_idx: usize,
    /// Frequency in MHz as a display string.
    pub(crate) frequency_mhz: String,
    /// Date in `YYYY-MM-DD` format.
    pub(crate) date: String,
    /// Time on (start) in `HH:MM` format.
    pub(crate) time: String,
    /// Time off (end) in `HH:MM` format; empty means same as time on.
    pub(crate) time_off: String,
    /// Worked station QTH (city/location).
    pub(crate) qth: String,
    /// RST sent report string.
    pub(crate) rst_sent: String,
    /// RST received report string.
    pub(crate) rst_rcvd: String,
    /// Short comment.
    pub(crate) comment: String,
    /// Operator notes.
    pub(crate) notes: String,
    // Advanced fields — page 1
    /// Transmitter power (e.g., "100W", "5W").
    pub(crate) tx_power: String,
    /// Submode override supplied by operator (overrides mode-derived submode).
    pub(crate) submode_override: String,
    /// Contest identifier (e.g., "CQWW", "ARRL-DX").
    pub(crate) contest_id: String,
    /// Contest serial number sent.
    pub(crate) serial_sent: String,
    /// Contest serial number received.
    pub(crate) serial_rcvd: String,
    /// Full exchange sent string.
    pub(crate) exchange_sent: String,
    /// Full exchange received string.
    pub(crate) exchange_rcvd: String,
    // Advanced fields — page 2
    /// Propagation mode (ADIF `PROP_MODE` value, e.g., "ES", "TEP", "SAT").
    pub(crate) prop_mode: String,
    /// Satellite name (e.g., "AO-7").
    pub(crate) sat_name: String,
    /// Satellite mode (e.g., "V/U").
    pub(crate) sat_mode: String,
    /// IOTA designator (e.g., "EU-005").
    pub(crate) iota: String,
    /// ARRL section abbreviation (e.g., "WWA", "ENY").
    pub(crate) arrl_section: String,
    /// Worked US state abbreviation.
    pub(crate) worked_state: String,
    /// Worked county name.
    pub(crate) worked_county: String,
}

impl Default for LogForm {
    fn default() -> Self {
        Self::new()
    }
}

impl LogForm {
    /// Create a new form initialised with current UTC date/time and 20M/SSB defaults.
    pub(crate) fn new() -> Self {
        let now = Utc::now();
        let mut form = Self {
            focused: Field::Callsign,
            callsign: String::new(),
            band_idx: DEFAULT_BAND_IDX,
            mode_idx: 0,
            frequency_mhz: String::new(),
            date: now.format("%Y-%m-%d").to_string(),
            time: now.format("%H:%M").to_string(),
            time_off: String::new(),
            qth: String::new(),
            rst_sent: String::new(),
            rst_rcvd: String::new(),
            comment: String::new(),
            notes: String::new(),
            tx_power: String::new(),
            submode_override: String::new(),
            contest_id: String::new(),
            serial_sent: String::new(),
            serial_rcvd: String::new(),
            exchange_sent: String::new(),
            exchange_rcvd: String::new(),
            prop_mode: String::new(),
            sat_name: String::new(),
            sat_mode: String::new(),
            iota: String::new(),
            arrl_section: String::new(),
            worked_state: String::new(),
            worked_county: String::new(),
        };
        form.on_band_change();
        form
    }

    /// Move focus to the next basic field, wrapping around.
    pub(crate) fn next_field(&mut self) {
        let idx = FIELD_ORDER
            .iter()
            .position(|f| f == &self.focused)
            .unwrap_or(0);
        self.focused = FIELD_ORDER
            .get((idx + 1) % FIELD_ORDER.len())
            .cloned()
            .unwrap_or(Field::Callsign);
    }

    /// Move focus to the previous basic field, wrapping around.
    pub(crate) fn prev_field(&mut self) {
        let idx = FIELD_ORDER
            .iter()
            .position(|f| f == &self.focused)
            .unwrap_or(0);
        let new_idx = if idx == 0 {
            FIELD_ORDER.len().saturating_sub(1)
        } else {
            idx - 1
        };
        self.focused = FIELD_ORDER.get(new_idx).cloned().unwrap_or(Field::Callsign);
    }

    /// Move focus to the next advanced field, wrapping around.
    pub(crate) fn next_advanced_field(&mut self) {
        let idx = ADVANCED_FIELD_ORDER
            .iter()
            .position(|f| f == &self.focused)
            .unwrap_or(0);
        self.focused = ADVANCED_FIELD_ORDER
            .get((idx + 1) % ADVANCED_FIELD_ORDER.len())
            .cloned()
            .unwrap_or(Field::TxPower);
    }

    /// Move focus to the previous advanced field, wrapping around.
    pub(crate) fn prev_advanced_field(&mut self) {
        let idx = ADVANCED_FIELD_ORDER
            .iter()
            .position(|f| f == &self.focused)
            .unwrap_or(0);
        let new_idx = if idx == 0 {
            ADVANCED_FIELD_ORDER.len().saturating_sub(1)
        } else {
            idx - 1
        };
        self.focused = ADVANCED_FIELD_ORDER
            .get(new_idx)
            .cloned()
            .unwrap_or(Field::TxPower);
    }

    /// Update frequency and RST defaults after the band changes.
    pub(crate) fn on_band_change(&mut self) {
        let freq = BAND_DEFAULT_FREQS
            .get(self.band_idx)
            .copied()
            .unwrap_or(14.225);
        self.frequency_mhz = format!("{freq:.3}");
        self.on_mode_change();
    }

    /// Update RST defaults to match the currently selected mode.
    pub(crate) fn on_mode_change(&mut self) {
        let rst = default_rst_for_mode(self.mode_idx);
        self.rst_sent = rst.to_string();
        self.rst_rcvd = rst.to_string();
    }

    /// Return a mutable reference to the focused field's text buffer.
    ///
    /// Returns `None` for cycle-only fields (`Band`, `Mode`).
    pub(crate) fn current_field_text_mut(&mut self) -> Option<&mut String> {
        match self.focused {
            Field::Callsign => Some(&mut self.callsign),
            Field::FrequencyMhz => Some(&mut self.frequency_mhz),
            Field::Date => Some(&mut self.date),
            Field::Time => Some(&mut self.time),
            Field::TimeOff => Some(&mut self.time_off),
            Field::Qth => Some(&mut self.qth),
            Field::RstSent => Some(&mut self.rst_sent),
            Field::RstRcvd => Some(&mut self.rst_rcvd),
            Field::Comment => Some(&mut self.comment),
            Field::Notes => Some(&mut self.notes),
            Field::TxPower => Some(&mut self.tx_power),
            Field::Submode => Some(&mut self.submode_override),
            Field::ContestId => Some(&mut self.contest_id),
            Field::SerialSent => Some(&mut self.serial_sent),
            Field::SerialRcvd => Some(&mut self.serial_rcvd),
            Field::ExchangeSent => Some(&mut self.exchange_sent),
            Field::ExchangeRcvd => Some(&mut self.exchange_rcvd),
            Field::PropMode => Some(&mut self.prop_mode),
            Field::SatName => Some(&mut self.sat_name),
            Field::SatMode => Some(&mut self.sat_mode),
            Field::Iota => Some(&mut self.iota),
            Field::ArrlSection => Some(&mut self.arrl_section),
            Field::WorkedState => Some(&mut self.worked_state),
            Field::WorkedCounty => Some(&mut self.worked_county),
            Field::Band | Field::Mode => None,
        }
    }

    /// Current band name from the [`BANDS`] slice.
    pub(crate) fn band_str(&self) -> &str {
        BANDS.get(self.band_idx).copied().unwrap_or("20M")
    }

    /// Current mode name from the [`MODES`] slice.
    pub(crate) fn mode_str(&self) -> &str {
        MODES.get(self.mode_idx).copied().unwrap_or("SSB")
    }

    /// Returns `true` if the focused field is a Left/Right cycle selector.
    pub(crate) fn is_cycle_field(&self) -> bool {
        matches!(self.focused, Field::Band | Field::Mode)
    }

    /// Cycle to the next band whose name starts with `ch` (case-insensitive).
    ///
    /// Repeated calls with the same char advance through all matching bands, wrapping around.
    pub(crate) fn type_select_band(&mut self, ch: char) {
        let ch_lo = ch.to_ascii_lowercase();
        let matches: Vec<usize> = BANDS
            .iter()
            .enumerate()
            .filter(|(_, b)| b.chars().next().map(|c| c.to_ascii_lowercase()) == Some(ch_lo))
            .map(|(i, _)| i)
            .collect();
        if matches.is_empty() {
            return;
        }
        let pos = matches.iter().position(|&i| i == self.band_idx);
        let next = pos.map_or(0, |p| (p + 1) % matches.len());
        if let Some(&idx) = matches.get(next) {
            self.band_idx = idx;
            self.on_band_change();
        }
    }

    /// Cycle to the next mode whose name starts with `ch` (case-insensitive).
    ///
    /// Repeated calls with the same char advance through all matching modes, wrapping around.
    pub(crate) fn type_select_mode(&mut self, ch: char) {
        let ch_lo = ch.to_ascii_lowercase();
        let matches: Vec<usize> = MODES
            .iter()
            .enumerate()
            .filter(|(_, m)| m.chars().next().map(|c| c.to_ascii_lowercase()) == Some(ch_lo))
            .map(|(i, _)| i)
            .collect();
        if matches.is_empty() {
            return;
        }
        let pos = matches.iter().position(|&i| i == self.mode_idx);
        let next = pos.map_or(0, |p| (p + 1) % matches.len());
        if let Some(&idx) = matches.get(next) {
            self.mode_idx = idx;
            self.on_mode_change();
        }
    }
}

/// Return the default RST string for the given mode index.
fn default_rst_for_mode(mode_idx: usize) -> &'static str {
    match MODES.get(mode_idx).copied().unwrap_or("SSB") {
        "SSB" | "AM" | "FM" => "59",
        _ => "599",
    }
}

/// Returns `true` if `field` belongs to the advanced form page 2 (propagation / awards).
pub(crate) fn is_advanced_page2(field: &Field) -> bool {
    matches!(
        field,
        Field::PropMode
            | Field::SatName
            | Field::SatMode
            | Field::Iota
            | Field::ArrlSection
            | Field::WorkedState
            | Field::WorkedCounty
    )
}

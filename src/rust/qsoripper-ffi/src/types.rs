//! C-compatible types for the QsoRipper FFI boundary.
//!
//! These structs mirror only the fields the win32 app actually uses,
//! keeping the ABI surface small.

#![allow(clippy::doc_markdown, clippy::indexing_slicing)]

/// RST signal report (readability, strength, optional tone).
#[repr(C)]
pub struct QsrRstReport {
    /// Readability (1-5).
    pub readability: i32,
    /// Signal strength (1-9).
    pub strength: i32,
    /// Tone (0 = not applicable, 1-9 for CW).
    pub tone: i32,
}

/// Request to log a new QSO.
#[repr(C)]
pub struct QsrLogQsoRequest {
    /// Worked station callsign (null-terminated UTF-8).
    pub callsign: [u8; 32],
    /// Operator's own (logging) station callsign (null-terminated UTF-8). Optional:
    /// if empty, the server materializes it from the active station profile.
    pub station_callsign: [u8; 32],
    /// Band string, e.g. "20M" (null-terminated).
    pub band: [u8; 8],
    /// Mode string, e.g. "SSB" (null-terminated).
    pub mode: [u8; 8],
    /// UTC date+time string "YYYY-MM-DD HH:MM" (null-terminated).
    pub datetime: [u8; 32],
    /// RST sent.
    pub rst_sent: QsrRstReport,
    /// RST received.
    pub rst_rcvd: QsrRstReport,
    /// Frequency in kHz (0 = not set).
    pub freq_khz: u64,
    /// Comment (null-terminated).
    pub comment: [u8; 256],
    /// Notes (null-terminated).
    pub notes: [u8; 256],
    /// Worked operator callsign (null-terminated).
    pub worked_operator_callsign: [u8; 32],
    /// Operator name (null-terminated).
    pub worked_name: [u8; 64],
    /// Worked grid square (null-terminated).
    pub worked_grid: [u8; 16],
    /// Worked country (null-terminated).
    pub worked_country: [u8; 64],
    /// Worked DXCC entity code as decimal text (null-terminated).
    pub worked_dxcc: [u8; 16],
    /// Worked CQ zone as decimal text (null-terminated).
    pub worked_cq_zone: [u8; 16],
    /// Worked ITU zone as decimal text (null-terminated).
    pub worked_itu_zone: [u8; 16],
    /// Worked continent code (null-terminated).
    pub worked_continent: [u8; 8],
    /// TX power (null-terminated).
    pub tx_power: [u8; 16],
    /// Submode (null-terminated).
    pub submode: [u8; 16],
    /// Contest ID (null-terminated).
    pub contest_id: [u8; 32],
    /// Serial sent (null-terminated).
    pub serial_sent: [u8; 16],
    /// Serial received (null-terminated).
    pub serial_rcvd: [u8; 16],
    /// Exchange sent (null-terminated).
    pub exchange_sent: [u8; 64],
    /// Exchange received (null-terminated).
    pub exchange_rcvd: [u8; 64],
    /// Propagation mode (null-terminated).
    pub prop_mode: [u8; 16],
    /// Satellite name (null-terminated).
    pub sat_name: [u8; 32],
    /// Satellite mode (null-terminated).
    pub sat_mode: [u8; 16],
    /// IOTA reference (null-terminated).
    pub iota: [u8; 16],
    /// ARRL section (null-terminated).
    pub arrl_section: [u8; 16],
    /// Worked state (null-terminated).
    pub worked_state: [u8; 16],
    /// Worked county (null-terminated).
    pub worked_county: [u8; 32],
    /// SKCC membership number (null-terminated).
    pub skcc: [u8; 16],
    /// QSL sent status text (null-terminated).
    pub qsl_sent_status: [u8; 16],
    /// QSL sent date "YYYY-MM-DD" (null-terminated).
    pub qsl_sent_date: [u8; 16],
    /// QSL received status text (null-terminated).
    pub qsl_rcvd_status: [u8; 16],
    /// QSL received date "YYYY-MM-DD" (null-terminated).
    pub qsl_rcvd_date: [u8; 16],
    /// LoTW sent tri-state text (Y/N/blank, null-terminated).
    pub lotw_sent: [u8; 8],
    /// LoTW received tri-state text (Y/N/blank, null-terminated).
    pub lotw_rcvd: [u8; 8],
    /// eQSL sent tri-state text (Y/N/blank, null-terminated).
    pub eqsl_sent: [u8; 8],
    /// eQSL received tri-state text (Y/N/blank, null-terminated).
    pub eqsl_rcvd: [u8; 8],
    /// QRZ log identifier (null-terminated).
    pub qrz_log_id: [u8; 32],
    /// QRZ book identifier (null-terminated).
    pub qrz_book_id: [u8; 32],
    /// Station snapshot station callsign (null-terminated).
    pub snapshot_station_callsign: [u8; 32],
    /// Station snapshot operator callsign (null-terminated).
    pub snapshot_operator_callsign: [u8; 32],
    /// Station snapshot profile name (null-terminated).
    pub snapshot_profile: [u8; 64],
    /// Station snapshot operator name (null-terminated).
    pub snapshot_operator_name: [u8; 64],
    /// Station snapshot grid square (null-terminated).
    pub snapshot_grid: [u8; 16],
    /// Station snapshot country (null-terminated).
    pub snapshot_country: [u8; 64],
    /// Station snapshot state (null-terminated).
    pub snapshot_state: [u8; 16],
    /// Station snapshot county (null-terminated).
    pub snapshot_county: [u8; 32],
    /// Station snapshot ARRL section (null-terminated).
    pub snapshot_arrl_section: [u8; 16],
    /// Station snapshot DXCC as decimal text (null-terminated).
    pub snapshot_dxcc: [u8; 16],
    /// Station snapshot CQ zone as decimal text (null-terminated).
    pub snapshot_cq_zone: [u8; 16],
    /// Station snapshot ITU zone as decimal text (null-terminated).
    pub snapshot_itu_zone: [u8; 16],
    /// Station snapshot latitude as decimal text (null-terminated).
    pub snapshot_latitude: [u8; 24],
    /// Station snapshot longitude as decimal text (null-terminated).
    pub snapshot_longitude: [u8; 24],
    /// CW receive WPM as decimal text (null-terminated).
    pub cw_rx_wpm: [u8; 8],
    /// CW transcript text (null-terminated).
    pub cw_transcript: [u8; 256],
    /// Extra ADIF fields as newline-delimited KEY=value text (null-terminated).
    pub extra_fields: [u8; 256],
    /// Time off date+time string "YYYY-MM-DD HH:MM" (null-terminated, empty = not set).
    pub time_off: [u8; 32],
}

/// Result from logging a new QSO.
#[repr(C)]
pub struct QsrLogQsoResult {
    /// Assigned local UUID (null-terminated).
    pub local_id: [u8; 64],
}

/// Request to update an existing QSO (same fields as log, plus local_id).
#[repr(C)]
pub struct QsrUpdateQsoRequest {
    /// Local UUID of the QSO to update (null-terminated).
    pub local_id: [u8; 64],
    /// Same payload as log request.
    pub qso: QsrLogQsoRequest,
}

impl Default for QsrQsoDetail {
    fn default() -> Self {
        // SAFETY: QsrQsoDetail only contains integer byte arrays, so all-zero is valid.
        unsafe { std::mem::zeroed() }
    }
}

/// Summary of a QSO for list display.
#[repr(C)]
pub struct QsrQsoSummary {
    /// UTC time string (null-terminated).
    pub utc: [u8; 24],
    /// Worked callsign (null-terminated).
    pub callsign: [u8; 16],
    /// Band display string (null-terminated).
    pub band: [u8; 8],
    /// Mode display string (null-terminated).
    pub mode: [u8; 8],
    /// RST sent display string (null-terminated).
    pub rst_sent: [u8; 8],
    /// RST received display string (null-terminated).
    pub rst_rcvd: [u8; 8],
    /// Country (null-terminated).
    pub country: [u8; 32],
    /// Grid square (null-terminated).
    pub grid: [u8; 8],
    /// Local UUID (null-terminated).
    pub local_id: [u8; 64],
}

/// Heap-allocated list of QSO summaries.
#[repr(C)]
pub struct QsrQsoList {
    /// Pointer to array of summaries (owned by Rust, freed via `qsr_free_qso_list`).
    pub items: *mut QsrQsoSummary,
    /// Number of items.
    pub count: i32,
}

/// Full QSO detail for editing.
#[repr(C)]
pub struct QsrQsoDetail {
    /// Worked callsign (null-terminated).
    pub callsign: [u8; 32],
    /// Band display string (null-terminated).
    pub band: [u8; 8],
    /// Mode display string (null-terminated).
    pub mode: [u8; 8],
    /// UTC date string "YYYY-MM-DD" (null-terminated).
    pub date: [u8; 16],
    /// UTC time string "HH:MM" (null-terminated).
    pub time: [u8; 16],
    /// Frequency in MHz string (null-terminated).
    pub freq_mhz: [u8; 16],
    /// RST sent display string (null-terminated).
    pub rst_sent: [u8; 8],
    /// RST received display string (null-terminated).
    pub rst_rcvd: [u8; 8],
    /// Comment (null-terminated).
    pub comment: [u8; 256],
    /// Notes (null-terminated).
    pub notes: [u8; 256],
    /// Local UUID (null-terminated).
    pub local_id: [u8; 64],
    /// Time off string "HH:MM" (null-terminated, empty = not set).
    pub time_off: [u8; 16],
    /// Worked operator callsign (null-terminated).
    pub worked_operator_callsign: [u8; 32],
    /// Worked name (null-terminated).
    pub worked_name: [u8; 64],
    /// Worked grid square (null-terminated).
    pub worked_grid: [u8; 16],
    /// Worked country (null-terminated).
    pub worked_country: [u8; 64],
    /// Worked DXCC entity code as decimal text (null-terminated).
    pub worked_dxcc: [u8; 16],
    /// Worked CQ zone as decimal text (null-terminated).
    pub worked_cq_zone: [u8; 16],
    /// Worked ITU zone as decimal text (null-terminated).
    pub worked_itu_zone: [u8; 16],
    /// Worked continent code (null-terminated).
    pub worked_continent: [u8; 8],
    /// TX power (null-terminated).
    pub tx_power: [u8; 16],
    /// Submode (null-terminated).
    pub submode: [u8; 16],
    /// Contest ID (null-terminated).
    pub contest_id: [u8; 32],
    /// Serial sent (null-terminated).
    pub serial_sent: [u8; 16],
    /// Serial received (null-terminated).
    pub serial_rcvd: [u8; 16],
    /// Exchange sent (null-terminated).
    pub exchange_sent: [u8; 64],
    /// Exchange received (null-terminated).
    pub exchange_rcvd: [u8; 64],
    /// Propagation mode (null-terminated).
    pub prop_mode: [u8; 16],
    /// Satellite name (null-terminated).
    pub sat_name: [u8; 32],
    /// Satellite mode (null-terminated).
    pub sat_mode: [u8; 16],
    /// IOTA reference (null-terminated).
    pub iota: [u8; 16],
    /// ARRL section (null-terminated).
    pub arrl_section: [u8; 16],
    /// Worked state (null-terminated).
    pub worked_state: [u8; 16],
    /// Worked county (null-terminated).
    pub worked_county: [u8; 32],
    /// SKCC membership number (null-terminated).
    pub skcc: [u8; 16],
    /// Station callsign (null-terminated).
    pub station_callsign: [u8; 32],
    /// QSL sent status text (null-terminated).
    pub qsl_sent_status: [u8; 16],
    /// QSL sent date "YYYY-MM-DD" (null-terminated).
    pub qsl_sent_date: [u8; 16],
    /// QSL received status text (null-terminated).
    pub qsl_rcvd_status: [u8; 16],
    /// QSL received date "YYYY-MM-DD" (null-terminated).
    pub qsl_rcvd_date: [u8; 16],
    /// LoTW sent tri-state text (Y/N/blank, null-terminated).
    pub lotw_sent: [u8; 8],
    /// LoTW received tri-state text (Y/N/blank, null-terminated).
    pub lotw_rcvd: [u8; 8],
    /// eQSL sent tri-state text (Y/N/blank, null-terminated).
    pub eqsl_sent: [u8; 8],
    /// eQSL received tri-state text (Y/N/blank, null-terminated).
    pub eqsl_rcvd: [u8; 8],
    /// QRZ log identifier (null-terminated).
    pub qrz_log_id: [u8; 32],
    /// QRZ book identifier (null-terminated).
    pub qrz_book_id: [u8; 32],
    /// Station snapshot station callsign (null-terminated).
    pub snapshot_station_callsign: [u8; 32],
    /// Station snapshot operator callsign (null-terminated).
    pub snapshot_operator_callsign: [u8; 32],
    /// Station snapshot profile name (null-terminated).
    pub snapshot_profile: [u8; 64],
    /// Station snapshot operator name (null-terminated).
    pub snapshot_operator_name: [u8; 64],
    /// Station snapshot grid square (null-terminated).
    pub snapshot_grid: [u8; 16],
    /// Station snapshot country (null-terminated).
    pub snapshot_country: [u8; 64],
    /// Station snapshot state (null-terminated).
    pub snapshot_state: [u8; 16],
    /// Station snapshot county (null-terminated).
    pub snapshot_county: [u8; 32],
    /// Station snapshot ARRL section (null-terminated).
    pub snapshot_arrl_section: [u8; 16],
    /// Station snapshot DXCC as decimal text (null-terminated).
    pub snapshot_dxcc: [u8; 16],
    /// Station snapshot CQ zone as decimal text (null-terminated).
    pub snapshot_cq_zone: [u8; 16],
    /// Station snapshot ITU zone as decimal text (null-terminated).
    pub snapshot_itu_zone: [u8; 16],
    /// Station snapshot latitude as decimal text (null-terminated).
    pub snapshot_latitude: [u8; 24],
    /// Station snapshot longitude as decimal text (null-terminated).
    pub snapshot_longitude: [u8; 24],
    /// CW receive WPM as decimal text (null-terminated).
    pub cw_rx_wpm: [u8; 8],
    /// CW transcript text (null-terminated).
    pub cw_transcript: [u8; 256],
    /// Sync status text (null-terminated).
    pub sync_status: [u8; 24],
    /// Created timestamp text (null-terminated).
    pub created_at: [u8; 32],
    /// Updated timestamp text (null-terminated).
    pub updated_at: [u8; 32],
    /// Extra ADIF fields as newline-delimited KEY=value text (null-terminated).
    pub extra_fields: [u8; 256],
}

/// Result from a callsign lookup.
#[repr(C)]
pub struct QsrLookupResult {
    /// 1 = data found, 0 = not found or error.
    pub has_data: i32,
    /// 1 = callsign not found, 0 = found or error.
    pub not_found: i32,
    /// Error message (null-terminated, empty = no error).
    pub error_msg: [u8; 128],
    /// Formatted name (null-terminated).
    pub name: [u8; 64],
    /// QTH / city (null-terminated).
    pub qth: [u8; 64],
    /// Grid square (null-terminated).
    pub grid: [u8; 16],
    /// Country (null-terminated).
    pub country: [u8; 64],
    /// CQ zone (0 = unknown).
    pub cq_zone: i32,
}

/// Rig status snapshot.
#[repr(C)]
pub struct QsrRigStatus {
    /// 1 = connected, 0 = disconnected.
    pub connected: i32,
    /// Frequency display string, e.g. "14.225.000" (null-terminated).
    pub freq_display: [u8; 32],
    /// Frequency in MHz string (null-terminated).
    pub freq_mhz: [u8; 16],
    /// Band string (null-terminated).
    pub band: [u8; 8],
    /// Mode string (null-terminated).
    pub mode: [u8; 16],
}

/// Space weather data.
#[repr(C)]
pub struct QsrSpaceWeather {
    /// 1 = data available, 0 = no data.
    pub has_data: i32,
    /// Planetary K-index.
    pub k_index: f64,
    /// Solar flux index.
    pub solar_flux: f64,
    /// Sunspot number.
    pub sunspot_number: i32,
}

// ── Helper functions ──────────────────────────────────────────────────

/// Copy a Rust string into a fixed-size C buffer, null-terminating.
pub(crate) fn str_to_buf(s: &str, buf: &mut [u8]) {
    let bytes = s.as_bytes();
    let len = bytes.len().min(buf.len().saturating_sub(1));
    buf[..len].copy_from_slice(&bytes[..len]);
    buf[len] = 0;
    // Zero remaining bytes for clean C reads
    for b in &mut buf[len + 1..] {
        *b = 0;
    }
}

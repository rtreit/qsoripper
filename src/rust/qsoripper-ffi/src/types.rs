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
    /// Operator name (null-terminated).
    pub worked_name: [u8; 64],
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
    /// Worked name (null-terminated).
    pub worked_name: [u8; 64],
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

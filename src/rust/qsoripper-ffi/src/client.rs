//! Internal gRPC client wrapper.
//!
//! Manages a tokio runtime and tonic channels, providing synchronous
//! blocking calls suitable for the C FFI boundary.

// The date-parsing and struct-population code uses index-based access
// with validated lengths. Suppress pedantic indexing/slicing warnings.
#![allow(
    clippy::indexing_slicing,
    clippy::cast_precision_loss,
    clippy::similar_names
)]

use std::ffi::CStr;
use std::fmt::Write as _;
use std::os::raw::c_char;
use std::sync::Mutex;

use tonic::transport::Channel;

use qsoripper_core::domain::band::{band_from_adif, band_to_adif};
use qsoripper_core::domain::mode::{mode_from_adif, mode_to_adif};
use qsoripper_core::proto::qsoripper::domain::{
    Band, Mode, QslStatus, QsoRecord, RigConnectionStatus, RstReport, StationSnapshot, SyncStatus,
};
use qsoripper_core::proto::qsoripper::services::{
    logbook_service_client::LogbookServiceClient, lookup_service_client::LookupServiceClient,
    rig_control_service_client::RigControlServiceClient,
    space_weather_service_client::SpaceWeatherServiceClient, DeleteQsoRequest,
    GetCurrentSpaceWeatherRequest, GetQsoRequest, GetRigSnapshotRequest, ListQsosRequest,
    LogQsoRequest, LookupRequest, UpdateQsoRequest,
};

use crate::register_qso_list_allocation;
use crate::types::{
    str_to_buf, QsrLogQsoRequest, QsrLogQsoResult, QsrLookupResult, QsrQsoDetail, QsrQsoList,
    QsrQsoSummary, QsrRigStatus, QsrRstReport, QsrSpaceWeather, QsrUpdateQsoRequest,
};

/// Thread-local last error message.
static LAST_ERROR: Mutex<String> = Mutex::new(String::new());

/// Set the last error message.
pub(crate) fn set_error(msg: impl Into<String>) {
    if let Ok(mut guard) = LAST_ERROR.lock() {
        *guard = msg.into();
    }
}

/// Get the last error message as a C string pointer.
/// The pointer is valid until the next FFI call.
/// Returns an empty string if no error.
pub(crate) fn last_error_cstr() -> *const c_char {
    // Use a thread-local to hold the null-terminated copy
    thread_local! {
        static BUF: std::cell::RefCell<Vec<u8>> = const { std::cell::RefCell::new(Vec::new()) };
    }

    let msg = LAST_ERROR
        .lock()
        .map_or_else(|_| String::new(), |g| g.clone());
    BUF.with(|buf| {
        let mut b = buf.borrow_mut();
        b.clear();
        b.extend_from_slice(msg.as_bytes());
        b.push(0);
        b.as_ptr().cast::<c_char>()
    })
}

/// Read a null-terminated UTF-8 C string, returning an empty string on null/invalid.
pub(crate) unsafe fn cstr_to_str<'a>(ptr: *const c_char) -> &'a str {
    if ptr.is_null() {
        return "";
    }
    unsafe { CStr::from_ptr(ptr) }.to_str().unwrap_or("")
}

/// Read a null-terminated string from a fixed-size byte buffer.
pub(crate) fn buf_to_str(buf: &[u8]) -> &str {
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    std::str::from_utf8(&buf[..end]).unwrap_or("")
}

/// Format a frequency in Hz to radio-style display: "14.225.000" or "3.536.500".
/// Pattern: `<MHz>.<kHz>.<fractional kHz in Hz>`
fn format_freq_radio_style(freq_hz: u64) -> String {
    let mhz = freq_hz / 1_000_000;
    let khz = (freq_hz % 1_000_000) / 1_000;
    let hz = freq_hz % 1_000;
    format!("{mhz}.{khz:03}.{hz:03}")
}

fn format_freq_mhz(freq_hz: u64) -> String {
    let mhz = freq_hz / 1_000_000;
    let frac = freq_hz % 1_000_000;
    format!("{mhz}.{frac:06}")
}

/// Opaque client handle holding the runtime and gRPC channels.
pub struct QsrClient {
    runtime: tokio::runtime::Runtime,
    logbook: LogbookServiceClient<Channel>,
    lookup: LookupServiceClient<Channel>,
    rig: RigControlServiceClient<Channel>,
    weather: SpaceWeatherServiceClient<Channel>,
}

impl QsrClient {
    /// Connect to the engine at the given endpoint (e.g. `"http://127.0.0.1:50051"`).
    pub(crate) fn connect(endpoint: &str) -> Result<Box<Self>, String> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("Failed to create tokio runtime: {e}"))?;

        let channel = runtime
            .block_on(
                Channel::from_shared(endpoint.to_string())
                    .map_err(|e| e.to_string())?
                    .connect(),
            )
            .map_err(|e| format!("Failed to connect to {endpoint}: {e}"))?;

        Ok(Box::new(Self {
            runtime,
            logbook: LogbookServiceClient::new(channel.clone()),
            lookup: LookupServiceClient::new(channel.clone()),
            rig: RigControlServiceClient::new(channel.clone()),
            weather: SpaceWeatherServiceClient::new(channel),
        }))
    }

    /// Log a new QSO. Returns 0 on success.
    pub(crate) fn log_qso(&mut self, req: &QsrLogQsoRequest, out: &mut QsrLogQsoResult) -> i32 {
        let qso = match build_qso_record(req) {
            Ok(q) => q,
            Err(e) => {
                set_error(e);
                return -1;
            }
        };

        match self.runtime.block_on(self.logbook.log_qso(LogQsoRequest {
            qso: Some(qso),
            sync_to_qrz: false,
        })) {
            Ok(resp) => {
                str_to_buf(&resp.into_inner().local_id, &mut out.local_id);
                0
            }
            Err(e) => {
                set_error(format!("LogQso failed: {}", e.message()));
                -1
            }
        }
    }

    /// Update an existing QSO. Returns 0 on success.
    pub(crate) fn update_qso(&mut self, req: &QsrUpdateQsoRequest) -> i32 {
        let edited = match build_qso_record(&req.qso) {
            Ok(q) => q,
            Err(e) => {
                set_error(e);
                return -1;
            }
        };
        let local_id = buf_to_str(&req.local_id).to_string();
        let existing = match self.runtime.block_on(self.logbook.get_qso(GetQsoRequest {
            local_id: local_id.clone(),
        })) {
            Ok(response) => {
                let Some(qso) = response.into_inner().qso else {
                    set_error(format!("QSO not found: {local_id}"));
                    return -1;
                };
                qso
            }
            Err(e) => {
                set_error(format!("GetQso before update failed: {}", e.message()));
                return -1;
            }
        };
        let mut qso = apply_ffi_edit(existing, edited);
        qso.local_id = local_id;

        match self
            .runtime
            .block_on(self.logbook.update_qso(UpdateQsoRequest {
                qso: Some(qso),
                sync_to_qrz: false,
            })) {
            Ok(_) => 0,
            Err(e) => {
                set_error(format!("UpdateQso failed: {}", e.message()));
                -1
            }
        }
    }

    /// Get a single QSO by local_id. Returns 0 on success.
    pub(crate) fn get_qso(&mut self, local_id: &str, out: &mut QsrQsoDetail) -> i32 {
        match self.runtime.block_on(self.logbook.get_qso(GetQsoRequest {
            local_id: local_id.to_string(),
        })) {
            Ok(resp) => {
                if let Some(qso) = resp.into_inner().qso {
                    populate_qso_detail(&qso, out);
                    0
                } else {
                    set_error(format!("QSO not found: {local_id}"));
                    -1
                }
            }
            Err(e) => {
                set_error(format!("GetQso failed: {}", e.message()));
                -1
            }
        }
    }

    /// Delete a QSO by local_id. Returns 0 on success.
    pub(crate) fn delete_qso(&mut self, local_id: &str) -> i32 {
        match self
            .runtime
            .block_on(self.logbook.delete_qso(DeleteQsoRequest {
                local_id: local_id.to_string(),
                delete_from_qrz: false,
            })) {
            Ok(_) => 0,
            Err(e) => {
                set_error(format!("DeleteQso failed: {}", e.message()));
                -1
            }
        }
    }

    /// List all QSOs. Returns a heap-allocated list.
    pub(crate) fn list_qsos(&mut self, out: &mut QsrQsoList) -> i32 {
        let stream_result = self
            .runtime
            .block_on(self.logbook.list_qsos(ListQsosRequest {
                limit: 0,
                ..Default::default()
            }));

        let mut stream = match stream_result {
            Ok(resp) => resp.into_inner(),
            Err(e) => {
                set_error(format!("ListQsos failed: {}", e.message()));
                return -1;
            }
        };

        let mut items: Vec<QsrQsoSummary> = Vec::new();

        loop {
            match self.runtime.block_on(stream.message()) {
                Ok(Some(resp)) => {
                    if let Some(qso) = resp.qso {
                        items.push(qso_to_summary(&qso));
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    set_error(format!("ListQsos stream error: {}", e.message()));
                    return -1;
                }
            }
        }

        let allocation_len = items.len();
        #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
        let count = allocation_len as i32;

        if items.is_empty() {
            out.items = std::ptr::null_mut();
            out.count = 0;
        } else {
            let boxed = items.into_boxed_slice();
            out.count = count;
            out.items = Box::into_raw(boxed).cast::<QsrQsoSummary>();
            register_qso_list_allocation(out.items, allocation_len);
        }

        0
    }

    /// Lookup a callsign. Returns 0 on success.
    pub(crate) fn lookup(&mut self, callsign: &str, out: &mut QsrLookupResult) -> i32 {
        match self.runtime.block_on(self.lookup.lookup(LookupRequest {
            callsign: callsign.to_string(),
            skip_cache: false,
        })) {
            Ok(resp) => {
                let inner = resp.into_inner();
                if let Some(result) = inner.result {
                    populate_lookup_result(&result, out);
                }
                0
            }
            Err(e) => {
                set_error(format!("Lookup failed: {}", e.message()));
                -1
            }
        }
    }

    /// Get rig snapshot. Returns 0 on success.
    pub(crate) fn get_rig_snapshot(&mut self, out: &mut QsrRigStatus) -> i32 {
        match self
            .runtime
            .block_on(self.rig.get_rig_snapshot(GetRigSnapshotRequest {}))
        {
            Ok(resp) => {
                if let Some(snapshot) = resp.into_inner().snapshot {
                    populate_rig_status(&snapshot, out);
                }
                0
            }
            Err(e) => {
                set_error(format!("GetRigSnapshot failed: {}", e.message()));
                -1
            }
        }
    }

    /// Get current space weather. Returns 0 on success.
    pub(crate) fn get_space_weather(&mut self, out: &mut QsrSpaceWeather) -> i32 {
        match self.runtime.block_on(
            self.weather
                .get_current_space_weather(GetCurrentSpaceWeatherRequest {}),
        ) {
            Ok(resp) => {
                if let Some(snapshot) = resp.into_inner().snapshot {
                    out.has_data = 1;
                    out.k_index = snapshot.planetary_k_index.unwrap_or(0.0);
                    out.solar_flux = snapshot.solar_flux_index.unwrap_or(0.0);
                    #[allow(clippy::cast_possible_wrap)]
                    {
                        out.sunspot_number = snapshot.sunspot_number.unwrap_or(0) as i32;
                    }
                }
                0
            }
            Err(e) => {
                set_error(format!("GetSpaceWeather failed: {}", e.message()));
                -1
            }
        }
    }
}

// ── Conversion helpers ──────────────────────────────────────────────────

#[allow(deprecated)]
fn apply_ffi_edit(mut existing: QsoRecord, edited: QsoRecord) -> QsoRecord {
    existing.qrz_logid = edited.qrz_logid;
    existing.qrz_bookid = edited.qrz_bookid;
    existing.station_callsign = edited.station_callsign;
    existing.worked_callsign = edited.worked_callsign;
    existing.utc_timestamp = edited.utc_timestamp;
    existing.band = edited.band;
    existing.mode = edited.mode;
    existing.frequency_khz = edited.frequency_khz;
    existing.frequency_hz = edited.frequency_hz;
    existing.submode = edited.submode;
    existing.station_snapshot = edited.station_snapshot;
    existing.utc_end_timestamp = edited.utc_end_timestamp;
    existing.rst_sent = edited.rst_sent;
    existing.rst_received = edited.rst_received;
    existing.tx_power = edited.tx_power;
    existing.qsl_sent_status = edited.qsl_sent_status;
    existing.qsl_received_status = edited.qsl_received_status;
    existing.lotw_sent = edited.lotw_sent;
    existing.lotw_received = edited.lotw_received;
    existing.eqsl_sent = edited.eqsl_sent;
    existing.eqsl_received = edited.eqsl_received;
    existing.qsl_sent_date = edited.qsl_sent_date;
    existing.qsl_received_date = edited.qsl_received_date;
    existing.worked_operator_callsign = edited.worked_operator_callsign;
    existing.worked_operator_name = edited.worked_operator_name;
    existing.worked_grid = edited.worked_grid;
    existing.worked_country = edited.worked_country;
    existing.worked_dxcc = edited.worked_dxcc;
    existing.worked_state = edited.worked_state;
    existing.worked_cq_zone = edited.worked_cq_zone;
    existing.worked_itu_zone = edited.worked_itu_zone;
    existing.worked_county = edited.worked_county;
    existing.worked_iota = edited.worked_iota;
    existing.worked_continent = edited.worked_continent;
    existing.worked_arrl_section = edited.worked_arrl_section;
    existing.skcc = edited.skcc;
    existing.contest_id = edited.contest_id;
    existing.serial_sent = edited.serial_sent;
    existing.serial_received = edited.serial_received;
    existing.exchange_sent = edited.exchange_sent;
    existing.exchange_received = edited.exchange_received;
    existing.prop_mode = edited.prop_mode;
    existing.sat_name = edited.sat_name;
    existing.sat_mode = edited.sat_mode;
    existing.notes = edited.notes;
    existing.comment = edited.comment;
    existing.extra_fields = edited.extra_fields;
    existing.cw_decode_rx_wpm = edited.cw_decode_rx_wpm;
    existing.cw_decode_transcript = edited.cw_decode_transcript;
    existing
}

/// Build a `QsoRecord` proto from the FFI request struct.
fn build_qso_record(req: &QsrLogQsoRequest) -> Result<QsoRecord, String> {
    let callsign = buf_to_str(&req.callsign);
    let station_callsign = buf_to_str(&req.station_callsign);
    let band_str = buf_to_str(&req.band);
    let mode_str = buf_to_str(&req.mode);
    let datetime_str = buf_to_str(&req.datetime);

    if station_callsign.trim().is_empty() {
        // Optional at the FFI layer: leave empty so the server can materialize
        // it from the active station profile (legacy Win32 app behavior).
    }

    let band = band_from_adif(&band_str.to_uppercase())
        .ok_or_else(|| format!("Unknown band: {band_str}"))?;
    let mode = mode_from_adif(&mode_str.to_uppercase())
        .ok_or_else(|| format!("Unknown mode: {mode_str}"))?;

    let timestamp = parse_datetime(datetime_str)?;

    let mut qso = QsoRecord {
        worked_callsign: callsign.to_uppercase(),
        station_callsign: station_callsign.to_uppercase(),
        band: band.into(),
        mode: mode.into(),
        utc_timestamp: Some(timestamp),
        ..Default::default()
    };

    // RST
    if req.rst_sent.readability > 0 {
        qso.rst_sent = Some(build_rst_report(&req.rst_sent));
    }

    if req.rst_rcvd.readability > 0 {
        qso.rst_received = Some(build_rst_report(&req.rst_rcvd));
    }

    if req.freq_khz > 0 {
        qso.frequency_hz = Some(req.freq_khz * 1000);
        #[allow(deprecated)]
        {
            qso.frequency_khz = Some(req.freq_khz);
        }
    }

    set_optional_str(&req.comment, |s| qso.comment = Some(s.to_string()));
    set_optional_str(&req.notes, |s| qso.notes = Some(s.to_string()));
    set_optional_str(&req.worked_name, |s| {
        qso.worked_operator_name = Some(s.to_string());
    });
    set_optional_str(&req.worked_grid, |s| qso.worked_grid = Some(s.to_string()));
    set_optional_str(&req.worked_country, |s| {
        qso.worked_country = Some(s.to_string());
    });
    set_optional_u32(&req.worked_dxcc, "worked DXCC", |v| {
        qso.worked_dxcc = Some(v);
    })?;
    set_optional_u32(&req.worked_cq_zone, "worked CQ zone", |v| {
        qso.worked_cq_zone = Some(v);
    })?;
    set_optional_u32(&req.worked_itu_zone, "worked ITU zone", |v| {
        qso.worked_itu_zone = Some(v);
    })?;
    set_optional_str(&req.worked_continent, |s| {
        qso.worked_continent = Some(s.to_string());
    });
    set_optional_str(&req.tx_power, |s| qso.tx_power = Some(s.to_string()));
    set_optional_str(&req.submode, |s| qso.submode = Some(s.to_string()));
    set_optional_str(&req.contest_id, |s| qso.contest_id = Some(s.to_string()));
    set_optional_str(&req.serial_sent, |s| qso.serial_sent = Some(s.to_string()));
    set_optional_str(&req.serial_rcvd, |s| {
        qso.serial_received = Some(s.to_string());
    });
    set_optional_str(&req.exchange_sent, |s| {
        qso.exchange_sent = Some(s.to_string());
    });
    set_optional_str(&req.exchange_rcvd, |s| {
        qso.exchange_received = Some(s.to_string());
    });
    set_optional_str(&req.prop_mode, |s| qso.prop_mode = Some(s.to_string()));
    set_optional_str(&req.sat_name, |s| qso.sat_name = Some(s.to_string()));
    set_optional_str(&req.sat_mode, |s| qso.sat_mode = Some(s.to_string()));
    set_optional_str(&req.iota, |s| qso.worked_iota = Some(s.to_string()));
    set_optional_str(&req.arrl_section, |s| {
        qso.worked_arrl_section = Some(s.to_string());
    });
    set_optional_str(&req.worked_state, |s| {
        qso.worked_state = Some(s.to_string());
    });
    set_optional_str(&req.worked_county, |s| {
        qso.worked_county = Some(s.to_string());
    });
    set_optional_str(&req.skcc, |s| qso.skcc = Some(s.to_string()));
    set_optional_str(&req.worked_operator_callsign, |s| {
        qso.worked_operator_callsign = Some(s.to_uppercase());
    });
    populate_qsl_fields(req, &mut qso)?;
    populate_station_snapshot(req, &mut qso)?;
    populate_transcript_and_extra_fields(req, &mut qso)?;

    let time_off = buf_to_str(&req.time_off);
    if !time_off.is_empty() {
        if let Ok(ts) = parse_datetime(time_off) {
            qso.utc_end_timestamp = Some(ts);
        }
    }

    Ok(qso)
}

/// If the buffer is non-empty, call the setter with the string value.
fn set_optional_str(buf: &[u8], setter: impl FnOnce(&str)) {
    let s = buf_to_str(buf);
    if !s.is_empty() {
        setter(s);
    }
}

fn set_optional_u32(buf: &[u8], field_name: &str, setter: impl FnOnce(u32)) -> Result<(), String> {
    let s = buf_to_str(buf);
    if !s.is_empty() {
        setter(
            s.parse::<u32>()
                .map_err(|_| format!("Invalid {field_name}: {s}"))?,
        );
    }
    Ok(())
}

fn set_optional_f64(buf: &[u8], field_name: &str, setter: impl FnOnce(f64)) -> Result<(), String> {
    let s = buf_to_str(buf);
    if !s.is_empty() {
        setter(
            s.parse::<f64>()
                .map_err(|_| format!("Invalid {field_name}: {s}"))?,
        );
    }
    Ok(())
}

fn parse_qsl_status(buf: &[u8], field_name: &str) -> Result<Option<QslStatus>, String> {
    let s = buf_to_str(buf).trim().to_ascii_uppercase();
    if s.is_empty() {
        return Ok(None);
    }
    let status = match s.as_str() {
        "N" | "NO" => QslStatus::No,
        "Y" | "YES" => QslStatus::Yes,
        "R" | "REQUESTED" => QslStatus::Requested,
        "Q" | "QUEUED" => QslStatus::Queued,
        "I" | "IGNORE" | "IGNORED" => QslStatus::Ignore,
        "UNSPECIFIED" => QslStatus::Unspecified,
        _ => return Err(format!("Invalid {field_name}: {s}")),
    };
    Ok(Some(status))
}

fn parse_optional_bool(buf: &[u8], field_name: &str) -> Result<Option<bool>, String> {
    let s = buf_to_str(buf).trim().to_ascii_uppercase();
    if s.is_empty() {
        return Ok(None);
    }
    match s.as_str() {
        "Y" | "YES" | "TRUE" | "1" => Ok(Some(true)),
        "N" | "NO" | "FALSE" | "0" => Ok(Some(false)),
        _ => Err(format!("Invalid {field_name}: {s}")),
    }
}

fn parse_date(buf: &[u8], field_name: &str) -> Result<Option<prost_types::Timestamp>, String> {
    let s = buf_to_str(buf);
    if s.is_empty() {
        return Ok(None);
    }
    parse_datetime(&format!("{s} 00:00"))
        .map(Some)
        .map_err(|e| format!("Invalid {field_name}: {e}"))
}

fn populate_qsl_fields(req: &QsrLogQsoRequest, qso: &mut QsoRecord) -> Result<(), String> {
    if let Some(status) = parse_qsl_status(&req.qsl_sent_status, "QSL sent status")? {
        qso.qsl_sent_status = status.into();
    }
    if let Some(status) = parse_qsl_status(&req.qsl_rcvd_status, "QSL received status")? {
        qso.qsl_received_status = status.into();
    }
    qso.lotw_sent = parse_optional_bool(&req.lotw_sent, "LoTW sent")?;
    qso.lotw_received = parse_optional_bool(&req.lotw_rcvd, "LoTW received")?;
    qso.eqsl_sent = parse_optional_bool(&req.eqsl_sent, "eQSL sent")?;
    qso.eqsl_received = parse_optional_bool(&req.eqsl_rcvd, "eQSL received")?;
    qso.qsl_sent_date = parse_date(&req.qsl_sent_date, "QSL sent date")?;
    qso.qsl_received_date = parse_date(&req.qsl_rcvd_date, "QSL received date")?;
    set_optional_str(&req.qrz_log_id, |s| qso.qrz_logid = Some(s.to_string()));
    set_optional_str(&req.qrz_book_id, |s| qso.qrz_bookid = Some(s.to_string()));
    Ok(())
}

fn populate_station_snapshot(req: &QsrLogQsoRequest, qso: &mut QsoRecord) -> Result<(), String> {
    let has_snapshot = [
        &req.snapshot_station_callsign[..],
        &req.snapshot_operator_callsign[..],
        &req.snapshot_profile[..],
        &req.snapshot_operator_name[..],
        &req.snapshot_grid[..],
        &req.snapshot_country[..],
        &req.snapshot_state[..],
        &req.snapshot_county[..],
        &req.snapshot_arrl_section[..],
        &req.snapshot_dxcc[..],
        &req.snapshot_cq_zone[..],
        &req.snapshot_itu_zone[..],
        &req.snapshot_latitude[..],
        &req.snapshot_longitude[..],
    ]
    .iter()
    .any(|buf| !buf_to_str(buf).is_empty());
    if !has_snapshot {
        return Ok(());
    }

    let mut snapshot = StationSnapshot {
        station_callsign: buf_to_str(&req.snapshot_station_callsign).to_uppercase(),
        ..Default::default()
    };
    if snapshot.station_callsign.is_empty() {
        snapshot.station_callsign = buf_to_str(&req.station_callsign).to_uppercase();
    }
    set_optional_str(&req.snapshot_operator_callsign, |s| {
        snapshot.operator_callsign = Some(s.to_uppercase());
    });
    set_optional_str(&req.snapshot_profile, |s| {
        snapshot.profile_name = Some(s.to_string());
    });
    set_optional_str(&req.snapshot_operator_name, |s| {
        snapshot.operator_name = Some(s.to_string());
    });
    set_optional_str(&req.snapshot_grid, |s| {
        snapshot.grid = Some(s.to_uppercase());
    });
    set_optional_str(&req.snapshot_country, |s| {
        snapshot.country = Some(s.to_string());
    });
    set_optional_str(&req.snapshot_state, |s| {
        snapshot.state = Some(s.to_string());
    });
    set_optional_str(&req.snapshot_county, |s| {
        snapshot.county = Some(s.to_string());
    });
    set_optional_str(&req.snapshot_arrl_section, |s| {
        snapshot.arrl_section = Some(s.to_uppercase());
    });
    set_optional_u32(&req.snapshot_dxcc, "station DXCC", |v| {
        snapshot.dxcc = Some(v);
    })?;
    set_optional_u32(&req.snapshot_cq_zone, "station CQ zone", |v| {
        snapshot.cq_zone = Some(v);
    })?;
    set_optional_u32(&req.snapshot_itu_zone, "station ITU zone", |v| {
        snapshot.itu_zone = Some(v);
    })?;
    set_optional_f64(&req.snapshot_latitude, "station latitude", |v| {
        snapshot.latitude = Some(v);
    })?;
    set_optional_f64(&req.snapshot_longitude, "station longitude", |v| {
        snapshot.longitude = Some(v);
    })?;
    qso.station_snapshot = Some(snapshot);
    Ok(())
}

fn populate_transcript_and_extra_fields(
    req: &QsrLogQsoRequest,
    qso: &mut QsoRecord,
) -> Result<(), String> {
    set_optional_u32(&req.cw_rx_wpm, "CW RX WPM", |v| {
        qso.cw_decode_rx_wpm = Some(v);
    })?;
    set_optional_str(&req.cw_transcript, |s| {
        qso.cw_decode_transcript = Some(s.to_string());
    });
    let extra = buf_to_str(&req.extra_fields);
    for line in extra.lines().map(str::trim).filter(|line| !line.is_empty()) {
        let Some((key, value)) = line.split_once('=') else {
            return Err(format!("Invalid extra ADIF field: {line}"));
        };
        qso.extra_fields
            .insert(key.trim().to_ascii_uppercase(), value.trim().to_string());
    }
    Ok(())
}

/// Build a proto `RstReport` from the FFI `QsrRstReport`.
#[allow(clippy::cast_sign_loss)]
fn build_rst_report(rst: &QsrRstReport) -> RstReport {
    let r = rst.readability as u32;
    let s = rst.strength as u32;
    let t = rst.tone as u32;
    let raw = if t > 0 {
        format!("{r}{s}{t}")
    } else {
        format!("{r}{s}")
    };
    RstReport {
        readability: Some(r),
        strength: Some(s),
        tone: if t > 0 { Some(t) } else { None },
        raw,
    }
}

/// Parse "YYYY-MM-DD HH:MM" or "YYYY-MM-DD HH:MM:SS" into a protobuf Timestamp.
fn parse_datetime(s: &str) -> Result<prost_types::Timestamp, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("Empty datetime".to_string());
    }

    let mut datetime_parts = s.split_whitespace();
    let date_part = datetime_parts
        .next()
        .ok_or_else(|| format!("Invalid datetime format: {s}"))?;
    let time_part = datetime_parts
        .next()
        .ok_or_else(|| format!("Invalid datetime format: {s}"))?;
    if datetime_parts.next().is_some() {
        return Err(format!("Invalid datetime format: {s}"));
    }

    let date_parts: Vec<&str> = date_part.split('-').collect();
    if date_parts.len() != 3 {
        return Err(format!("Invalid date: {date_part}"));
    }

    let year: i32 = date_parts[0]
        .parse()
        .map_err(|_| format!("Invalid year: {}", date_parts[0]))?;
    let month: u32 = date_parts[1]
        .parse()
        .map_err(|_| format!("Invalid month: {}", date_parts[1]))?;
    let day: u32 = date_parts[2]
        .parse()
        .map_err(|_| format!("Invalid day: {}", date_parts[2]))?;

    if !(1..=12).contains(&month) {
        return Err(format!("Month out of range: {month}"));
    }
    let max_day = days_in_month(year, month);
    if !(1..=max_day).contains(&day) {
        return Err(format!("Day out of range: {day}"));
    }

    let time_parts: Vec<&str> = time_part.split(':').collect();
    if !(time_parts.len() == 2 || time_parts.len() == 3) {
        return Err(format!("Invalid time: {time_part}"));
    }

    let hour: u32 = time_parts[0]
        .parse()
        .map_err(|_| format!("Invalid hour: {}", time_parts[0]))?;
    let minute: u32 = time_parts[1]
        .parse()
        .map_err(|_| format!("Invalid minute: {}", time_parts[1]))?;
    let second: u32 = if time_parts.len() > 2 {
        time_parts[2]
            .parse()
            .map_err(|_| format!("Invalid second: {}", time_parts[2]))?
    } else {
        0
    };
    if hour > 23 {
        return Err(format!("Hour out of range: {hour}"));
    }
    if minute > 59 {
        return Err(format!("Minute out of range: {minute}"));
    }
    if second > 59 {
        return Err(format!("Second out of range: {second}"));
    }

    // Calculate Unix timestamp from components (UTC)
    #[allow(clippy::cast_possible_wrap)]
    let days = days_from_civil(year, month, day);
    #[allow(clippy::cast_possible_wrap)]
    let secs = i64::from(days) * 86400
        + i64::from(hour) * 3600
        + i64::from(minute) * 60
        + i64::from(second);

    Ok(prost_types::Timestamp {
        seconds: secs,
        nanos: 0,
    })
}

/// Days from civil date (Chrono-free algorithm from Howard Hinnant).
#[allow(clippy::cast_possible_wrap, clippy::cast_sign_loss)]
fn days_from_civil(year: i32, month: u32, day: u32) -> i32 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u32;
    let m = month;
    let doy = if m > 2 {
        (153 * (m - 3) + 2) / 5 + day - 1
    } else {
        (153 * (m + 9) + 2) / 5 + day - 1
    };
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe as i32 - 719_468
}

fn is_leap_year(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}

fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap_year(year) {
                29
            } else {
                28
            }
        }
        _ => 0,
    }
}

/// Convert a proto `QsoRecord` to a `QsrQsoSummary`.
fn qso_to_summary(qso: &QsoRecord) -> QsrQsoSummary {
    let mut s = QsrQsoSummary {
        utc: [0; 24],
        callsign: [0; 16],
        band: [0; 8],
        mode: [0; 8],
        rst_sent: [0; 8],
        rst_rcvd: [0; 8],
        country: [0; 32],
        grid: [0; 8],
        local_id: [0; 64],
    };

    // Format UTC time as HH:MM
    if let Some(ts) = &qso.utc_timestamp {
        let total_secs = ts.seconds;
        let day_secs = total_secs.rem_euclid(86400);
        let h = day_secs / 3600;
        let m = (day_secs % 3600) / 60;
        let utc_str = format!("{h:02}:{m:02}");
        str_to_buf(&utc_str, &mut s.utc);
    }

    str_to_buf(&qso.worked_callsign, &mut s.callsign);

    let band = Band::try_from(qso.band).unwrap_or(Band::Unspecified);
    if let Some(band_str) = band_to_adif(band) {
        str_to_buf(band_str, &mut s.band);
    }

    let mode = Mode::try_from(qso.mode).unwrap_or(Mode::Unspecified);
    if let Some(mode_str) = mode_to_adif(mode) {
        str_to_buf(mode_str, &mut s.mode);
    }

    // RST sent
    if let Some(rst) = &qso.rst_sent {
        let rst_str = format_rst(rst);
        str_to_buf(&rst_str, &mut s.rst_sent);
    }

    // RST received
    if let Some(rst) = &qso.rst_received {
        let rst_str = format_rst(rst);
        str_to_buf(&rst_str, &mut s.rst_rcvd);
    }

    if let Some(country) = &qso.worked_country {
        str_to_buf(country, &mut s.country);
    }

    if let Some(grid) = &qso.worked_grid {
        str_to_buf(grid, &mut s.grid);
    }

    str_to_buf(&qso.local_id, &mut s.local_id);

    s
}

/// Format an RST report as a string like "59" or "599".
fn format_rst(rst: &RstReport) -> String {
    let r = rst.readability.unwrap_or(0);
    let s = rst.strength.unwrap_or(0);
    match rst.tone {
        Some(t) if t > 0 => format!("{r}{s}{t}"),
        _ => format!("{r}{s}"),
    }
}

fn timestamp_date(ts: &prost_types::Timestamp) -> String {
    let (y, m, d, _, _) = timestamp_parts(ts);
    format!("{y:04}-{m:02}-{d:02}")
}

fn timestamp_datetime(ts: &prost_types::Timestamp) -> String {
    let (y, m, d, h, min) = timestamp_parts(ts);
    format!("{y:04}-{m:02}-{d:02} {h:02}:{min:02}")
}

fn timestamp_parts(ts: &prost_types::Timestamp) -> (i64, i64, i64, i64, i64) {
    let total_secs = ts.seconds;
    let days = (total_secs / 86400) + 719_468;
    let era = if days >= 0 { days } else { days - 146_096 } / 146_097;
    let doe = days - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    let day_secs = total_secs.rem_euclid(86400);
    let h = day_secs / 3600;
    let min = (day_secs % 3600) / 60;
    (y, m, d, h, min)
}

fn qsl_status_text(status: i32) -> &'static str {
    match QslStatus::try_from(status).unwrap_or(QslStatus::Unspecified) {
        QslStatus::No => "N",
        QslStatus::Yes => "Y",
        QslStatus::Requested => "R",
        QslStatus::Queued => "Q",
        QslStatus::Ignore => "I",
        QslStatus::Unspecified => "",
    }
}

fn optional_bool_text(value: Option<bool>) -> &'static str {
    match value {
        Some(true) => "Y",
        Some(false) => "N",
        None => "",
    }
}

fn sync_status_text(status: i32) -> &'static str {
    match SyncStatus::try_from(status).unwrap_or(SyncStatus::LocalOnly) {
        SyncStatus::LocalOnly => "LocalOnly",
        SyncStatus::Synced => "Synced",
        SyncStatus::Modified => "Modified",
        SyncStatus::Conflict => "Conflict",
    }
}

fn populate_confirmation_detail(qso: &QsoRecord, out: &mut QsrQsoDetail) {
    str_to_buf(
        qsl_status_text(qso.qsl_sent_status),
        &mut out.qsl_sent_status,
    );
    str_to_buf(
        qsl_status_text(qso.qsl_received_status),
        &mut out.qsl_rcvd_status,
    );
    str_to_buf(optional_bool_text(qso.lotw_sent), &mut out.lotw_sent);
    str_to_buf(optional_bool_text(qso.lotw_received), &mut out.lotw_rcvd);
    str_to_buf(optional_bool_text(qso.eqsl_sent), &mut out.eqsl_sent);
    str_to_buf(optional_bool_text(qso.eqsl_received), &mut out.eqsl_rcvd);
    if let Some(ts) = &qso.qsl_sent_date {
        str_to_buf(&timestamp_date(ts), &mut out.qsl_sent_date);
    }
    if let Some(ts) = &qso.qsl_received_date {
        str_to_buf(&timestamp_date(ts), &mut out.qsl_rcvd_date);
    }
    if let Some(v) = &qso.qrz_logid {
        str_to_buf(v, &mut out.qrz_log_id);
    }
    if let Some(v) = &qso.qrz_bookid {
        str_to_buf(v, &mut out.qrz_book_id);
    }
}

fn populate_station_snapshot_detail(qso: &QsoRecord, out: &mut QsrQsoDetail) {
    let Some(snapshot) = &qso.station_snapshot else {
        return;
    };
    str_to_buf(
        &snapshot.station_callsign,
        &mut out.snapshot_station_callsign,
    );
    if let Some(v) = &snapshot.operator_callsign {
        str_to_buf(v, &mut out.snapshot_operator_callsign);
    }
    if let Some(v) = &snapshot.profile_name {
        str_to_buf(v, &mut out.snapshot_profile);
    }
    if let Some(v) = &snapshot.operator_name {
        str_to_buf(v, &mut out.snapshot_operator_name);
    }
    if let Some(v) = &snapshot.grid {
        str_to_buf(v, &mut out.snapshot_grid);
    }
    if let Some(v) = &snapshot.country {
        str_to_buf(v, &mut out.snapshot_country);
    }
    if let Some(v) = &snapshot.state {
        str_to_buf(v, &mut out.snapshot_state);
    }
    if let Some(v) = &snapshot.county {
        str_to_buf(v, &mut out.snapshot_county);
    }
    if let Some(v) = &snapshot.arrl_section {
        str_to_buf(v, &mut out.snapshot_arrl_section);
    }
    if let Some(v) = snapshot.dxcc {
        str_to_buf(&v.to_string(), &mut out.snapshot_dxcc);
    }
    if let Some(v) = snapshot.cq_zone {
        str_to_buf(&v.to_string(), &mut out.snapshot_cq_zone);
    }
    if let Some(v) = snapshot.itu_zone {
        str_to_buf(&v.to_string(), &mut out.snapshot_itu_zone);
    }
    if let Some(v) = snapshot.latitude {
        str_to_buf(&v.to_string(), &mut out.snapshot_latitude);
    }
    if let Some(v) = snapshot.longitude {
        str_to_buf(&v.to_string(), &mut out.snapshot_longitude);
    }
}

fn populate_transcript_metadata_detail(qso: &QsoRecord, out: &mut QsrQsoDetail) {
    if let Some(v) = qso.cw_decode_rx_wpm {
        str_to_buf(&v.to_string(), &mut out.cw_rx_wpm);
    }
    if let Some(v) = &qso.cw_decode_transcript {
        str_to_buf(v, &mut out.cw_transcript);
    }
    str_to_buf(sync_status_text(qso.sync_status), &mut out.sync_status);
    if let Some(ts) = &qso.created_at {
        str_to_buf(&timestamp_datetime(ts), &mut out.created_at);
    }
    if let Some(ts) = &qso.updated_at {
        str_to_buf(&timestamp_datetime(ts), &mut out.updated_at);
    }
    if !qso.extra_fields.is_empty() {
        let mut text = String::new();
        let mut entries: Vec<_> = qso.extra_fields.iter().collect();
        entries.sort_by(|(left, _), (right, _)| left.cmp(right));
        for (key, value) in entries {
            let _ = writeln!(&mut text, "{key}={value}");
        }
        str_to_buf(&text, &mut out.extra_fields);
    }
}

fn populate_qso_optional_fields(qso: &QsoRecord, out: &mut QsrQsoDetail) {
    if let Some(v) = &qso.comment {
        str_to_buf(v, &mut out.comment);
    }
    if let Some(v) = &qso.notes {
        str_to_buf(v, &mut out.notes);
    }
    if let Some(v) = &qso.worked_operator_name {
        str_to_buf(v, &mut out.worked_name);
    }
    if let Some(v) = &qso.worked_operator_callsign {
        str_to_buf(v, &mut out.worked_operator_callsign);
    }
    if let Some(v) = &qso.worked_grid {
        str_to_buf(v, &mut out.worked_grid);
    }
    if let Some(v) = &qso.worked_country {
        str_to_buf(v, &mut out.worked_country);
    }
    if let Some(v) = qso.worked_dxcc {
        str_to_buf(&v.to_string(), &mut out.worked_dxcc);
    }
    if let Some(v) = qso.worked_cq_zone {
        str_to_buf(&v.to_string(), &mut out.worked_cq_zone);
    }
    if let Some(v) = qso.worked_itu_zone {
        str_to_buf(&v.to_string(), &mut out.worked_itu_zone);
    }
    if let Some(v) = &qso.worked_continent {
        str_to_buf(v, &mut out.worked_continent);
    }
    if let Some(v) = &qso.tx_power {
        str_to_buf(v, &mut out.tx_power);
    }
    if let Some(v) = &qso.submode {
        str_to_buf(v, &mut out.submode);
    }
    if let Some(v) = &qso.contest_id {
        str_to_buf(v, &mut out.contest_id);
    }
    if let Some(v) = &qso.serial_sent {
        str_to_buf(v, &mut out.serial_sent);
    }
    if let Some(v) = &qso.serial_received {
        str_to_buf(v, &mut out.serial_rcvd);
    }
    if let Some(v) = &qso.exchange_sent {
        str_to_buf(v, &mut out.exchange_sent);
    }
    if let Some(v) = &qso.exchange_received {
        str_to_buf(v, &mut out.exchange_rcvd);
    }
    if let Some(v) = &qso.prop_mode {
        str_to_buf(v, &mut out.prop_mode);
    }
    if let Some(v) = &qso.sat_name {
        str_to_buf(v, &mut out.sat_name);
    }
    if let Some(v) = &qso.sat_mode {
        str_to_buf(v, &mut out.sat_mode);
    }
    if let Some(v) = &qso.worked_iota {
        str_to_buf(v, &mut out.iota);
    }
    if let Some(v) = &qso.worked_arrl_section {
        str_to_buf(v, &mut out.arrl_section);
    }
    if let Some(v) = &qso.worked_state {
        str_to_buf(v, &mut out.worked_state);
    }
    if let Some(v) = &qso.worked_county {
        str_to_buf(v, &mut out.worked_county);
    }
    if let Some(v) = &qso.skcc {
        str_to_buf(v, &mut out.skcc);
    }
    populate_confirmation_detail(qso, out);
    populate_station_snapshot_detail(qso, out);
    populate_transcript_metadata_detail(qso, out);
}

/// Populate a `QsrQsoDetail` from a proto `QsoRecord`.
fn populate_qso_detail(qso: &QsoRecord, out: &mut QsrQsoDetail) {
    *out = QsrQsoDetail::default();

    str_to_buf(&qso.worked_callsign, &mut out.callsign);
    str_to_buf(&qso.station_callsign, &mut out.station_callsign);
    str_to_buf(&qso.local_id, &mut out.local_id);

    let band = Band::try_from(qso.band).unwrap_or(Band::Unspecified);
    if let Some(band_str) = band_to_adif(band) {
        str_to_buf(band_str, &mut out.band);
    }

    let mode = Mode::try_from(qso.mode).unwrap_or(Mode::Unspecified);
    if let Some(mode_str) = mode_to_adif(mode) {
        str_to_buf(mode_str, &mut out.mode);
    }

    // Date and time from timestamp
    if let Some(ts) = &qso.utc_timestamp {
        let total_secs = ts.seconds;
        let days = (total_secs / 86400) + 719_468;
        let era = if days >= 0 { days } else { days - 146_096 } / 146_097;
        let doe = days - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
        let y = yoe + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        let y = if m <= 2 { y + 1 } else { y };

        let date_str = format!("{y:04}-{m:02}-{d:02}");
        str_to_buf(&date_str, &mut out.date);

        let day_secs = total_secs.rem_euclid(86400);
        let h = day_secs / 3600;
        let min = (day_secs % 3600) / 60;
        let time_str = format!("{h:02}:{min:02}");
        str_to_buf(&time_str, &mut out.time);
    }

    // Frequency — prefer Hz field, fall back to deprecated kHz
    #[allow(deprecated)]
    let freq_hz = qso.frequency_hz.or(qso.frequency_khz.map(|k| k * 1000));
    if let Some(hz) = freq_hz {
        if hz > 0 {
            let freq_str = format_freq_mhz(hz);
            str_to_buf(&freq_str, &mut out.freq_mhz);
        }
    }

    // RST
    if let Some(rst) = &qso.rst_sent {
        str_to_buf(&format_rst(rst), &mut out.rst_sent);
    }
    if let Some(rst) = &qso.rst_received {
        str_to_buf(&format_rst(rst), &mut out.rst_rcvd);
    }

    // Optional string fields
    populate_qso_optional_fields(qso, out);

    // Time off
    if let Some(ts) = &qso.utc_end_timestamp {
        let day_secs = ts.seconds.rem_euclid(86400);
        let h = day_secs / 3600;
        let min = (day_secs % 3600) / 60;
        let time_off_str = format!("{h:02}:{min:02}");
        str_to_buf(&time_off_str, &mut out.time_off);
    }
}

/// Populate a `QsrLookupResult` from a proto `LookupResult`.
fn populate_lookup_result(
    result: &qsoripper_core::proto::qsoripper::domain::LookupResult,
    out: &mut QsrLookupResult,
) {
    use qsoripper_core::proto::qsoripper::domain::LookupState;

    *out = QsrLookupResult {
        has_data: 0,
        not_found: 0,
        error_msg: [0; 128],
        name: [0; 64],
        qth: [0; 64],
        grid: [0; 16],
        country: [0; 64],
        cq_zone: 0,
    };

    match result.state() {
        LookupState::NotFound => {
            out.not_found = 1;
            return;
        }
        LookupState::Error => {
            if let Some(msg) = &result.error_message {
                str_to_buf(msg, &mut out.error_msg);
            } else {
                str_to_buf("Lookup error", &mut out.error_msg);
            }
            return;
        }
        _ => {}
    }

    if result.state() != LookupState::Found {
        return;
    }

    if let Some(record) = &result.record {
        out.has_data = 1;

        // Prefer formatted_name, fallback to first_name
        if let Some(name) = &record.formatted_name {
            str_to_buf(name, &mut out.name);
        } else if !record.first_name.is_empty() {
            str_to_buf(&record.first_name, &mut out.name);
        }

        // QTH: prefer addr2, fallback to state
        if let Some(addr2) = &record.addr2 {
            str_to_buf(addr2, &mut out.qth);
        }

        if let Some(grid) = &record.grid_square {
            str_to_buf(grid, &mut out.grid);
        }

        if let Some(country) = &record.country {
            str_to_buf(country, &mut out.country);
        }

        #[allow(clippy::cast_possible_wrap)]
        if let Some(cq) = record.cq_zone {
            out.cq_zone = cq as i32;
        }
    }
}

/// Populate a `QsrRigStatus` from a proto `RigSnapshot`.
fn populate_rig_status(
    snapshot: &qsoripper_core::proto::qsoripper::domain::RigSnapshot,
    out: &mut QsrRigStatus,
) {
    *out = QsrRigStatus {
        connected: 0,
        freq_display: [0; 32],
        freq_mhz: [0; 16],
        band: [0; 8],
        mode: [0; 16],
    };

    if snapshot.status() != RigConnectionStatus::Connected {
        return;
    }

    out.connected = 1;

    if snapshot.frequency_hz > 0 {
        let display = format_freq_radio_style(snapshot.frequency_hz);
        let mhz_str = format_freq_mhz(snapshot.frequency_hz);
        str_to_buf(&display, &mut out.freq_display);
        str_to_buf(&mhz_str, &mut out.freq_mhz);
    }

    let band_enum = Band::try_from(snapshot.band).unwrap_or(Band::Unspecified);
    if let Some(band_str) = band_to_adif(band_enum) {
        str_to_buf(band_str, &mut out.band);
    }

    let mode_enum = Mode::try_from(snapshot.mode).unwrap_or(Mode::Unspecified);
    if let Some(mode_str) = mode_to_adif(mode_enum) {
        str_to_buf(mode_str, &mut out.mode);
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::{
        apply_ffi_edit, buf_to_str, build_qso_record, format_freq_mhz, format_freq_radio_style,
        parse_datetime, qso_to_summary,
    };
    use crate::types::{str_to_buf, QsrLogQsoRequest, QsrRstReport};
    use qsoripper_core::proto::qsoripper::domain::{
        QslStatus, QsoCompletion, QsoRecord, StationSnapshot,
    };

    #[test]
    fn ffi_edit_preserves_fields_not_exposed_by_the_abi() {
        let mut existing = QsoRecord {
            local_id: "ffi-edit".into(),
            notes: Some("clear me".into()),
            worked_latitude: Some(47.61),
            worked_longitude: Some(-122.33),
            worked_altitude_meters: Some(120.0),
            worked_gridsquare_ext: Some("AB".into()),
            frequency_rx_hz: Some(14_076_000),
            owner_callsign: Some("K7OWNER".into()),
            qso_complete: QsoCompletion::Yes as i32,
            ..Default::default()
        };
        existing.band_rx = qsoripper_core::proto::qsoripper::domain::Band::Band20m as i32;
        let edited = QsoRecord {
            comment: Some("updated through FFI".into()),
            notes: None,
            ..Default::default()
        };

        let merged = apply_ffi_edit(existing, edited);

        assert_eq!(merged.comment.as_deref(), Some("updated through FFI"));
        assert!(merged.notes.is_none());
        assert_eq!(merged.worked_latitude, Some(47.61));
        assert_eq!(merged.worked_longitude, Some(-122.33));
        assert_eq!(merged.worked_altitude_meters, Some(120.0));
        assert_eq!(merged.worked_gridsquare_ext.as_deref(), Some("AB"));
        assert_eq!(merged.frequency_rx_hz, Some(14_076_000));
        assert_eq!(merged.owner_callsign.as_deref(), Some("K7OWNER"));
        assert_eq!(merged.qso_complete, QsoCompletion::Yes as i32);
    }

    fn baseline_request() -> QsrLogQsoRequest {
        let mut req: QsrLogQsoRequest = unsafe { std::mem::zeroed() };
        str_to_buf("W1AW", &mut req.callsign);
        str_to_buf("K7TST", &mut req.station_callsign);
        str_to_buf("20M", &mut req.band);
        str_to_buf("SSB", &mut req.mode);
        str_to_buf("2025-01-15 14:30", &mut req.datetime);
        req.rst_sent = QsrRstReport {
            readability: 5,
            strength: 9,
            tone: 0,
        };
        req.rst_rcvd = QsrRstReport {
            readability: 5,
            strength: 9,
            tone: 0,
        };
        req
    }

    #[test]
    fn build_qso_record_populates_station_callsign() {
        let req = baseline_request();
        let qso = build_qso_record(&req).expect("build_qso_record should succeed");
        assert_eq!(qso.station_callsign, "K7TST");
        assert_eq!(qso.worked_callsign, "W1AW");
        assert!(
            !qso.station_callsign.trim().is_empty(),
            "station_callsign must be populated so the server's persistence validator accepts the QSO"
        );
    }

    #[test]
    fn frequency_formatters_preserve_hz_fractional_digits() {
        assert_eq!(format_freq_radio_style(14_074_123), "14.074.123");
        assert_eq!(format_freq_radio_style(14_074_000), "14.074.000");
        assert_eq!(format_freq_mhz(14_074_123), "14.074123");
    }

    #[test]
    fn build_qso_record_leaves_station_callsign_empty_for_server_materialization() {
        let mut req = baseline_request();
        req.station_callsign = [0u8; 32];
        let qso = build_qso_record(&req)
            .expect("empty station_callsign is optional so the server can materialize it");
        assert!(
            qso.station_callsign.trim().is_empty(),
            "empty FFI station_callsign must round-trip as empty so the server fills it"
        );
    }

    #[test]
    fn build_qso_record_populates_advanced_card_fields() {
        let mut req = baseline_request();
        str_to_buf("W1AW/OP", &mut req.worked_operator_callsign);
        str_to_buf("Y", &mut req.qsl_sent_status);
        str_to_buf("N", &mut req.lotw_sent);
        str_to_buf("2025-01-16", &mut req.qsl_sent_date);
        str_to_buf("123", &mut req.qrz_log_id);
        str_to_buf("Home", &mut req.snapshot_profile);
        str_to_buf("K7TST", &mut req.snapshot_station_callsign);
        str_to_buf("CN87", &mut req.snapshot_grid);
        str_to_buf("291", &mut req.snapshot_dxcc);
        str_to_buf("34", &mut req.cw_rx_wpm);
        str_to_buf("CQ TEST", &mut req.cw_transcript);
        str_to_buf("APP_TEST=value", &mut req.extra_fields);

        let qso = build_qso_record(&req).expect("advanced card fields should map");

        assert_eq!(qso.worked_operator_callsign.as_deref(), Some("W1AW/OP"));
        assert_eq!(qso.qsl_sent_status, QslStatus::Yes as i32);
        assert_eq!(qso.lotw_sent, Some(false));
        assert!(qso.qsl_sent_date.is_some());
        assert_eq!(qso.qrz_logid.as_deref(), Some("123"));
        assert_eq!(qso.cw_decode_rx_wpm, Some(34));
        assert_eq!(qso.cw_decode_transcript.as_deref(), Some("CQ TEST"));
        assert_eq!(
            qso.extra_fields.get("APP_TEST").map(String::as_str),
            Some("value")
        );
        let snapshot = qso
            .station_snapshot
            .expect("station snapshot should be present");
        assert_eq!(snapshot.profile_name.as_deref(), Some("Home"));
        assert_eq!(snapshot.station_callsign, "K7TST");
        assert_eq!(snapshot.grid.as_deref(), Some("CN87"));
        assert_eq!(snapshot.dxcc, Some(291));
    }

    #[test]
    fn qso_to_summary_does_not_use_station_snapshot_country_when_worked_country_missing() {
        let qso = QsoRecord {
            worked_callsign: "JA1ABC".to_string(),
            station_snapshot: Some(StationSnapshot {
                country: Some("United States".to_string()),
                ..StationSnapshot::default()
            }),
            ..QsoRecord::default()
        };

        let summary = qso_to_summary(&qso);

        assert_eq!("", buf_to_str(&summary.country));
    }

    #[test]
    fn qso_to_summary_uses_explicit_worked_country_when_present() {
        let qso = QsoRecord {
            worked_callsign: "JA1ABC".to_string(),
            worked_country: Some("Japan".to_string()),
            station_snapshot: Some(StationSnapshot {
                country: Some("United States".to_string()),
                ..StationSnapshot::default()
            }),
            ..QsoRecord::default()
        };

        let summary = qso_to_summary(&qso);

        assert_eq!("Japan", buf_to_str(&summary.country));
    }

    #[test]
    fn rust_bug_1_parse_datetime_rejects_out_of_range_components() {
        let invalid_inputs = [
            "2025-13-01 12:00",
            "2025-00-01 12:00",
            "2025-01-32 12:00",
            "2025-02-29 12:00",
            "2024-02-30 12:00",
            "2025-01-01 24:00",
            "2025-01-01 12:60",
            "2025-01-01 12:00:60",
            "2025-01-01 12:00:AA",
        ];

        for input in invalid_inputs {
            assert!(
                parse_datetime(input).is_err(),
                "Expected parse_datetime to reject invalid input: {input}"
            );
        }
    }
}

//! Side-by-side settings sync dialog backed by the engines' `SetupService`.
//!
//! Press `Y` in the launcher to open a popup that fetches the current
//! `SetupStatus` from both engines, displays them side-by-side, and lets
//! the operator pick which side becomes the source of truth. The chosen
//! side is then pushed to the other engine via `SaveSetup`.

use std::time::Duration;

use qsoripper_core::proto::qsoripper::domain::{ConflictPolicy, StationProfile, SyncConfig};
use qsoripper_core::proto::qsoripper::services::{
    setup_service_client::SetupServiceClient, GetSetupStatusRequest, RigControlSettings,
    SaveSetupRequest, SetupFieldValue, SetupStatus,
};
use tokio::runtime::Runtime;
use tonic::transport::{Channel, Endpoint};

/// Which side of the dialog is selected as the source of truth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Side {
    Left,
    Right,
}

impl Side {
    pub(crate) fn toggle(self) -> Self {
        match self {
            Self::Left => Self::Right,
            Self::Right => Self::Left,
        }
    }
}

/// One column in the sync popup. Either holds a `SetupStatus` or an error.
#[derive(Debug, Clone)]
pub(crate) struct EngineSnapshot {
    pub label: &'static str,
    pub endpoint: String,
    pub status: Option<SetupStatus>,
    pub error: Option<String>,
}

impl EngineSnapshot {
    fn new(label: &'static str, endpoint: impl Into<String>) -> Self {
        Self {
            label,
            endpoint: endpoint.into(),
            status: None,
            error: None,
        }
    }
}

/// Lifecycle of the dialog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DialogState {
    Ready,
    Applying,
    Applied(String),
    Failed(String),
}

/// Modal sync dialog state.
#[derive(Debug, Clone)]
pub(crate) struct SyncDialog {
    pub left: EngineSnapshot,
    pub right: EngineSnapshot,
    pub source: Side,
    pub state: DialogState,
}

impl SyncDialog {
    /// Fetch `SetupStatus` from both engines (Rust @ 50051, .NET @ 50052) and
    /// build a dialog. Connection/RPC failures are recorded per-side; the
    /// dialog still opens so the operator sees the error.
    pub(crate) fn fetch(rt: &Runtime) -> Self {
        let mut left = EngineSnapshot::new("Rust (50051)", "http://127.0.0.1:50051");
        let mut right = EngineSnapshot::new(".NET (50052)", "http://127.0.0.1:50052");

        match rt.block_on(fetch_status(&left.endpoint)) {
            Ok(status) => left.status = Some(status),
            Err(error) => left.error = Some(error),
        }
        match rt.block_on(fetch_status(&right.endpoint)) {
            Ok(status) => right.status = Some(status),
            Err(error) => right.error = Some(error),
        }

        Self {
            left,
            right,
            source: Side::Left,
            state: DialogState::Ready,
        }
    }

    pub(crate) fn source_snapshot(&self) -> &EngineSnapshot {
        match self.source {
            Side::Left => &self.left,
            Side::Right => &self.right,
        }
    }

    pub(crate) fn target_snapshot(&self) -> &EngineSnapshot {
        match self.source {
            Side::Left => &self.right,
            Side::Right => &self.left,
        }
    }

    /// Push the source side's settings to the target engine.
    pub(crate) fn apply(&mut self, rt: &Runtime) {
        let (Some(source), target) = (
            self.source_snapshot().status.clone(),
            self.target_snapshot(),
        ) else {
            self.state = DialogState::Failed(
                "source engine has no settings to copy (check connection)".to_owned(),
            );
            return;
        };
        if target.error.is_some() {
            self.state = DialogState::Failed(format!(
                "target engine '{}' is unreachable: {}",
                target.label,
                target.error.as_deref().unwrap_or("?")
            ));
            return;
        }
        let request = save_request_from_status(&source);
        let endpoint = target.endpoint.clone();
        let target_label = target.label;
        self.state = DialogState::Applying;
        match rt.block_on(apply_save(&endpoint, request)) {
            Ok(()) => {
                self.state = DialogState::Applied(format!("Pushed settings to {target_label}."));
            }
            Err(error) => {
                self.state = DialogState::Failed(format!("{target_label}: {error}"));
            }
        }
    }
}

/// Convert a `SetupStatus` snapshot into a `SaveSetupRequest`.
///
/// QRZ XML password and QRZ Logbook API key are NOT carried in `SetupStatus`
/// (only `has_qrz_xml_password` / `has_qrz_logbook_api_key` flags). The
/// receiving engine preserves the existing password when the field is `None`
/// in the request, so omitting them is safe.
#[allow(deprecated)]
pub(crate) fn save_request_from_status(status: &SetupStatus) -> SaveSetupRequest {
    let persistence_values = status
        .persistence_values
        .iter()
        .filter(|value| value.has_value && !value.secret && !value.redacted)
        .map(|value| SetupFieldValue {
            key: value.key.clone(),
            value: Some(value.display_value.clone()),
        })
        .collect();
    SaveSetupRequest {
        storage_backend: status.storage_backend,
        sqlite_path: status.sqlite_path.clone(),
        station_profile: status.station_profile.clone(),
        qrz_xml_username: status.qrz_xml_username.clone(),
        qrz_xml_password: None,
        log_file_path: status.log_file_path.clone(),
        qrz_logbook_api_key: None,
        sync_config: status.sync_config,
        rig_control: status.rig_control.clone(),
        persistence_values,
    }
}

async fn fetch_status(endpoint: &str) -> Result<SetupStatus, String> {
    let channel = connect(endpoint).await?;
    let mut client = SetupServiceClient::new(channel);
    let response = tokio::time::timeout(
        Duration::from_secs(3),
        client.get_setup_status(GetSetupStatusRequest {}),
    )
    .await
    .map_err(|_| "timed out waiting for GetSetupStatus".to_owned())?
    .map_err(|status| format!("RPC error: {}", status.message()))?;
    response
        .into_inner()
        .status
        .ok_or_else(|| "engine returned no SetupStatus payload".to_owned())
}

async fn apply_save(endpoint: &str, request: SaveSetupRequest) -> Result<(), String> {
    let channel = connect(endpoint).await?;
    let mut client = SetupServiceClient::new(channel);
    tokio::time::timeout(Duration::from_secs(5), client.save_setup(request))
        .await
        .map_err(|_| "timed out waiting for SaveSetup".to_owned())?
        .map_err(|status| format!("RPC error: {}", status.message()))?;
    Ok(())
}

async fn connect(endpoint: &str) -> Result<Channel, String> {
    let endpoint = Endpoint::from_shared(endpoint.to_owned())
        .map_err(|e| format!("invalid endpoint: {e}"))?
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(5));
    endpoint
        .connect()
        .await
        .map_err(|e| format!("connect failed: {e}"))
}

/// Field rows rendered side-by-side. Each row is `(label, left, right)`.
pub(crate) fn diff_rows(dialog: &SyncDialog) -> Vec<(String, String, String, bool)> {
    let left = snapshot_fields(&dialog.left);
    let right = snapshot_fields(&dialog.right);
    let mut rows = Vec::with_capacity(left.len());
    for ((label, lhs), (_, rhs)) in left.into_iter().zip(right.into_iter()) {
        let differs = lhs != rhs;
        rows.push((label.to_owned(), lhs, rhs, differs));
    }
    rows
}

fn snapshot_fields(snapshot: &EngineSnapshot) -> Vec<(&'static str, String)> {
    snapshot.status.as_ref().map_or_else(
        || unreachable_snapshot_fields(snapshot.error.as_deref().unwrap_or("(no data)")),
        reachable_snapshot_fields,
    )
}

fn unreachable_snapshot_fields(error: &str) -> Vec<(&'static str, String)> {
    vec![
        ("Status", format!("unreachable: {error}")),
        ("Station callsign", String::new()),
        ("Operator callsign", String::new()),
        ("Operator name", String::new()),
        ("Grid", String::new()),
        ("QRZ XML user", String::new()),
        ("QRZ XML password", String::new()),
        ("QRZ logbook key", String::new()),
        ("Log file path", String::new()),
        ("Auto sync", String::new()),
        ("Sync interval (s)", String::new()),
        ("Conflict policy", String::new()),
        ("Rig control", String::new()),
    ]
}

fn reachable_snapshot_fields(status: &SetupStatus) -> Vec<(&'static str, String)> {
    let default_profile = StationProfile {
        profile_name: None,
        station_callsign: String::new(),
        operator_callsign: None,
        operator_name: None,
        grid: None,
        county: None,
        state: None,
        country: None,
        dxcc: None,
        cq_zone: None,
        itu_zone: None,
        latitude: None,
        longitude: None,
        arrl_section: None,
    };
    let default_sync = SyncConfig {
        auto_sync_enabled: false,
        sync_interval_seconds: 0,
        conflict_policy: ConflictPolicy::Unspecified as i32,
    };
    let profile = status.station_profile.as_ref().unwrap_or(&default_profile);
    let sync = status.sync_config.as_ref().unwrap_or(&default_sync);

    vec![
        ("Status", "reachable".to_owned()),
        ("Station callsign", profile.station_callsign.clone()),
        (
            "Operator callsign",
            profile.operator_callsign.clone().unwrap_or_default(),
        ),
        (
            "Operator name",
            profile.operator_name.clone().unwrap_or_default(),
        ),
        ("Grid", profile.grid.clone().unwrap_or_default()),
        (
            "QRZ XML user",
            status.qrz_xml_username.clone().unwrap_or_default(),
        ),
        (
            "QRZ XML password",
            secret_status_marker(status.has_qrz_xml_password),
        ),
        (
            "QRZ logbook key",
            secret_status_marker(status.has_qrz_logbook_api_key),
        ),
        (
            "Log file path",
            status.log_file_path.clone().unwrap_or_default(),
        ),
        ("Auto sync", sync.auto_sync_enabled.to_string()),
        ("Sync interval (s)", sync.sync_interval_seconds.to_string()),
        (
            "Conflict policy",
            conflict_policy_label(sync.conflict_policy),
        ),
        (
            "Rig control",
            rig_control_label(status.rig_control.as_ref()),
        ),
    ]
}

fn secret_status_marker(is_set: bool) -> String {
    if is_set { "<set>" } else { "<unset>" }.to_owned()
}

fn conflict_policy_label(value: i32) -> String {
    format!(
        "{:?}",
        ConflictPolicy::try_from(value).unwrap_or(ConflictPolicy::Unspecified)
    )
}

fn rig_control_label(rig: Option<&RigControlSettings>) -> String {
    let enabled = rig
        .and_then(|settings| settings.enabled)
        .map_or_else(|| "?".to_owned(), enabled_label);
    let host = rig
        .and_then(|settings| settings.host.as_deref())
        .unwrap_or("?");
    let port = rig
        .and_then(|settings| settings.port)
        .map_or_else(|| "?".to_owned(), |port| port.to_string());
    format!("{enabled}@{host}:{port}")
}

fn enabled_label(enabled: bool) -> String {
    if enabled { "on" } else { "off" }.to_owned()
}

#[cfg(test)]
#[allow(deprecated, clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use qsoripper_core::proto::qsoripper::domain::StationProfile;

    fn mk_status(callsign: &str, log_path: Option<&str>) -> SetupStatus {
        SetupStatus {
            config_file_exists: true,
            setup_complete: true,
            config_path: "config.toml".to_owned(),
            storage_backend: 0,
            sqlite_path: None,
            has_station_profile: true,
            station_profile: Some(StationProfile {
                profile_name: None,
                station_callsign: callsign.to_owned(),
                operator_callsign: None,
                operator_name: None,
                grid: None,
                county: None,
                state: None,
                country: None,
                dxcc: None,
                cq_zone: None,
                itu_zone: None,
                latitude: None,
                longitude: None,
                arrl_section: None,
            }),
            qrz_xml_username: Some("user".to_owned()),
            has_qrz_xml_password: true,
            suggested_sqlite_path: String::new(),
            warnings: vec![],
            active_station_profile_id: None,
            station_profile_count: 1,
            log_file_path: log_path.map(str::to_owned),
            suggested_log_file_path: String::new(),
            is_first_run: false,
            has_qrz_logbook_api_key: false,
            sync_config: None,
            rig_control: None,
            persistence_step_enabled: false,
            persistence_label: String::new(),
            persistence_description: String::new(),
            persistence_definitions: vec![],
            persistence_values: vec![],
            persistence_contract_explicit: false,
        }
    }

    #[test]
    fn save_request_preserves_secret_fields_as_none() {
        let request = save_request_from_status(&mk_status("K7XYZ", Some("/tmp/log.db")));
        assert!(request.qrz_xml_password.is_none());
        assert!(request.qrz_logbook_api_key.is_none());
        assert_eq!(request.log_file_path.as_deref(), Some("/tmp/log.db"));
        assert_eq!(
            request
                .station_profile
                .as_ref()
                .map(|p| p.station_callsign.as_str()),
            Some("K7XYZ")
        );
    }

    #[test]
    fn diff_rows_flag_differences() {
        let dialog = SyncDialog {
            left: EngineSnapshot {
                label: "L",
                endpoint: String::new(),
                status: Some(mk_status("K7XYZ", Some("/a.db"))),
                error: None,
            },
            right: EngineSnapshot {
                label: "R",
                endpoint: String::new(),
                status: Some(mk_status("K7XYZ", Some("/b.db"))),
                error: None,
            },
            source: Side::Left,
            state: DialogState::Ready,
        };
        let rows = diff_rows(&dialog);
        let callsign_row = rows
            .iter()
            .find(|(l, ..)| l == "Station callsign")
            .expect("station callsign row present");
        assert!(!callsign_row.3, "matching callsigns should not flag a diff");
        let path_row = rows
            .iter()
            .find(|(l, ..)| l == "Log file path")
            .expect("log file path row present");
        assert!(path_row.3, "differing log paths should flag a diff");
    }

    #[test]
    fn target_is_opposite_of_source() {
        let dialog = SyncDialog {
            left: EngineSnapshot::new("L", ""),
            right: EngineSnapshot::new("R", ""),
            source: Side::Left,
            state: DialogState::Ready,
        };
        assert_eq!(dialog.source_snapshot().label, "L");
        assert_eq!(dialog.target_snapshot().label, "R");
        let mut dialog = dialog;
        dialog.source = Side::Right;
        assert_eq!(dialog.target_snapshot().label, "L");
    }

    #[test]
    fn apply_without_source_status_fails_with_message() {
        let rt = Runtime::new().expect("tokio runtime");
        let mut dialog = SyncDialog {
            left: EngineSnapshot {
                label: "L",
                endpoint: String::new(),
                status: None,
                error: Some("nope".to_owned()),
            },
            right: EngineSnapshot::new("R", ""),
            source: Side::Left,
            state: DialogState::Ready,
        };
        dialog.apply(&rt);
        assert!(matches!(dialog.state, DialogState::Failed(_)));
    }
}

//! Side-by-side settings sync dialog backed by the engines' `SetupService`.
//!
//! Press `Y` in the launcher to open a popup that fetches the current
//! `SetupStatus` from both engines, displays them side-by-side, and lets
//! the operator pick which side becomes the source of truth. The chosen
//! side is then pushed to the other engine via `SaveSetup`.

use std::time::Duration;

use qsoripper_core::proto::qsoripper::domain::ConflictPolicy;
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
        cat_hub: None,
        wsjtx_ingest: status.wsjtx_ingest.clone(),
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

/// A side-by-side comparison row for the sync dialog.
#[derive(Debug, Clone)]
pub(crate) struct DiffRow {
    pub field: &'static str,
    pub left: String,
    pub right: String,
    pub differs: bool,
}

/// Field rows rendered side-by-side.
///
/// Merges the left and right field lists by label so the dialog is robust to
/// either side gaining, losing, or reordering rows. Labels appearing only on
/// one side are still displayed; the missing column shows `<absent>` and the
/// row is flagged as differing.
pub(crate) fn diff_rows(dialog: &SyncDialog) -> Vec<DiffRow> {
    merge_field_lists(
        &snapshot_fields(&dialog.left),
        &snapshot_fields(&dialog.right),
    )
}

const ABSENT_MARKER: &str = "<absent>";

fn merge_field_lists(
    left: &[(&'static str, String)],
    right: &[(&'static str, String)],
) -> Vec<DiffRow> {
    let right_lookup: std::collections::HashMap<&'static str, &String> =
        right.iter().map(|(label, value)| (*label, value)).collect();
    let left_labels: std::collections::HashSet<&'static str> =
        left.iter().map(|(label, _)| *label).collect();

    let mut rows = Vec::with_capacity(left.len() + right.len());
    for (label, lhs) in left {
        let (rhs, present_right) = match right_lookup.get(label) {
            Some(value) => ((*value).clone(), true),
            None => (ABSENT_MARKER.to_owned(), false),
        };
        let differs = !present_right || *lhs != rhs;
        rows.push(DiffRow {
            field: label,
            left: lhs.clone(),
            right: rhs,
            differs,
        });
    }
    for (label, rhs) in right {
        if left_labels.contains(label) {
            continue;
        }
        rows.push(DiffRow {
            field: label,
            left: ABSENT_MARKER.to_owned(),
            right: rhs.clone(),
            differs: true,
        });
    }
    rows
}

/// Labels and order for the 13 rows shown in the dialog. Both the "reachable"
/// and "unreachable" snapshots use the same labels so `diff_rows` can zip them.
const FIELD_LABELS: [&str; 13] = [
    "Status",
    "Station callsign",
    "Operator callsign",
    "Operator name",
    "Grid",
    "QRZ XML user",
    "QRZ XML password",
    "QRZ logbook key",
    "Log file path",
    "Auto sync",
    "Sync interval (s)",
    "Conflict policy",
    "Rig control",
];

fn snapshot_fields(snapshot: &EngineSnapshot) -> Vec<(&'static str, String)> {
    if let Some(status) = snapshot.status.as_ref() {
        return reachable_snapshot_fields(status);
    }
    let error = snapshot.error.as_deref().unwrap_or("(no data)");
    let mut rows: Vec<(&'static str, String)> = FIELD_LABELS
        .iter()
        .map(|&label| (label, String::new()))
        .collect();
    if let Some(first) = rows.first_mut() {
        first.1 = format!("unreachable: {error}");
    }
    rows
}

fn reachable_snapshot_fields(status: &SetupStatus) -> Vec<(&'static str, String)> {
    let profile = status.station_profile.clone().unwrap_or_default();
    let sync = status.sync_config.unwrap_or_default();
    vec![
        (FIELD_LABELS[0], "reachable".to_owned()),
        (FIELD_LABELS[1], profile.station_callsign),
        (
            FIELD_LABELS[2],
            profile.operator_callsign.unwrap_or_default(),
        ),
        (FIELD_LABELS[3], profile.operator_name.unwrap_or_default()),
        (FIELD_LABELS[4], profile.grid.unwrap_or_default()),
        (
            FIELD_LABELS[5],
            status.qrz_xml_username.clone().unwrap_or_default(),
        ),
        (FIELD_LABELS[6], secret_marker(status.has_qrz_xml_password)),
        (
            FIELD_LABELS[7],
            secret_marker(status.has_qrz_logbook_api_key),
        ),
        (
            FIELD_LABELS[8],
            status.log_file_path.clone().unwrap_or_default(),
        ),
        (FIELD_LABELS[9], sync.auto_sync_enabled.to_string()),
        (FIELD_LABELS[10], sync.sync_interval_seconds.to_string()),
        (
            FIELD_LABELS[11],
            conflict_policy_label(sync.conflict_policy),
        ),
        (
            FIELD_LABELS[12],
            rig_control_label(status.rig_control.as_ref()),
        ),
    ]
}

fn secret_marker(is_set: bool) -> String {
    if is_set { "<set>" } else { "<unset>" }.to_owned()
}

fn conflict_policy_label(value: i32) -> String {
    format!(
        "{:?}",
        ConflictPolicy::try_from(value).unwrap_or(ConflictPolicy::Unspecified)
    )
}

fn rig_control_label(rig: Option<&RigControlSettings>) -> String {
    let enabled = rig.and_then(|settings| settings.enabled).map_or_else(
        || "?".to_owned(),
        |on| if on { "on" } else { "off" }.to_owned(),
    );
    let host = rig
        .and_then(|settings| settings.host.as_deref())
        .unwrap_or("?");
    let port = rig
        .and_then(|settings| settings.port)
        .map_or_else(|| "?".to_owned(), |port| port.to_string());
    format!("{enabled}@{host}:{port}")
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use qsoripper_core::proto::qsoripper::domain::StationProfile;

    fn mk_status(callsign: &str, log_path: Option<&str>) -> SetupStatus {
        SetupStatus {
            station_profile: Some(StationProfile {
                station_callsign: callsign.to_owned(),
                ..Default::default()
            }),
            qrz_xml_username: Some("user".to_owned()),
            has_qrz_xml_password: true,
            log_file_path: log_path.map(str::to_owned),
            ..Default::default()
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
            .find(|row| row.field == "Station callsign")
            .expect("station callsign row present");
        assert!(
            !callsign_row.differs,
            "matching callsigns should not flag a diff"
        );
        let path_row = rows
            .iter()
            .find(|row| row.field == "Log file path")
            .expect("log file path row present");
        assert!(path_row.differs, "differing log paths should flag a diff");
    }

    #[test]
    fn diff_rows_handles_asymmetric_field_sets() {
        let left_fields: Vec<(&'static str, String)> = vec![
            ("shared-a", "1".to_owned()),
            ("only-left", "lonely".to_owned()),
            ("shared-b", "same".to_owned()),
        ];
        let right_fields: Vec<(&'static str, String)> = vec![
            ("shared-a", "2".to_owned()),
            ("shared-b", "same".to_owned()),
            ("only-right", "different".to_owned()),
        ];

        let rows = merge_field_lists(&left_fields, &right_fields);
        let fields: Vec<&str> = rows.iter().map(|r| r.field).collect();
        assert_eq!(
            fields,
            vec!["shared-a", "only-left", "shared-b", "only-right"]
        );

        let shared_a = rows.iter().find(|r| r.field == "shared-a").unwrap();
        assert!(shared_a.differs);
        assert_eq!(shared_a.left, "1");
        assert_eq!(shared_a.right, "2");

        let only_left = rows.iter().find(|r| r.field == "only-left").unwrap();
        assert!(only_left.differs);
        assert_eq!(only_left.left, "lonely");
        assert_eq!(only_left.right, "<absent>");

        let shared_b = rows.iter().find(|r| r.field == "shared-b").unwrap();
        assert!(!shared_b.differs);

        let only_right = rows.iter().find(|r| r.field == "only-right").unwrap();
        assert!(only_right.differs);
        assert_eq!(only_right.left, "<absent>");
        assert_eq!(only_right.right, "different");
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

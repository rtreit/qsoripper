//! `LoTW` sync tests for the in-memory storage adapter.

#![allow(clippy::expect_used)]

use prost_types::Timestamp;
use qsoripper_core::adif::parse_adi_qsos;
use qsoripper_core::lotw::{
    execute_sync, upload_single_qso, LotwApi, LotwError, LotwReport, LotwUploadResult,
};
use qsoripper_core::proto::qsoripper::domain::{Band, LotwSyncStatus, Mode, QslStatus, QsoRecord};
use qsoripper_core::storage::{LogbookStore, SyncMetadata};
use qsoripper_storage_memory::MemoryStorage;
use std::sync::Mutex;

struct MockLotwApi {
    report: LotwReport,
    upload_error: Option<&'static str>,
    fetch_error: Option<&'static str>,
    fetched_since: Mutex<Vec<Option<String>>>,
}

impl MockLotwApi {
    fn successful(report: LotwReport) -> Self {
        Self {
            report,
            upload_error: None,
            fetch_error: None,
            fetched_since: Mutex::new(Vec::new()),
        }
    }

    fn failing(upload_error: Option<&'static str>, fetch_error: Option<&'static str>) -> Self {
        Self {
            report: LotwReport::default(),
            upload_error,
            fetch_error,
            fetched_since: Mutex::new(Vec::new()),
        }
    }
}

#[tonic::async_trait]
impl LotwApi for MockLotwApi {
    async fn upload_qsos(&self, qsos: &[QsoRecord]) -> Result<LotwUploadResult, LotwError> {
        if let Some(message) = self.upload_error {
            return Err(LotwError::Tqsl(message.to_string()));
        }
        Ok(LotwUploadResult {
            submitted: u32::try_from(qsos.len()).unwrap_or(u32::MAX),
        })
    }

    async fn fetch_confirmations(&self, since: Option<&str>) -> Result<LotwReport, LotwError> {
        self.fetched_since
            .lock()
            .expect("capture confirmation high-water")
            .push(since.map(str::to_string));
        if let Some(message) = self.fetch_error {
            return Err(LotwError::Network(message.to_string()));
        }
        Ok(self.report.clone())
    }
}

#[tokio::test]
async fn sync_uploads_and_confirms_without_replacing_notes() {
    let store = MemoryStorage::new();
    let mut local = qso("local-1", 1_786_157_400);
    local.notes = Some("Keep this operator note.".to_string());
    store.insert_qso(&local).await.expect("insert local QSO");

    let confirmations = parse_adi_qsos(include_bytes!(
        "../../../../tests/fixtures/lotw_confirmations.adi"
    ))
    .await
    .expect("parse LoTW fixture");
    let api = MockLotwApi::successful(LotwReport {
        confirmations,
        high_water: Some("2026-08-09 12:34:56".to_string()),
    });

    let result = execute_sync(&api, &store, false, true, true)
        .await
        .expect("LoTW sync");

    assert_eq!(result.uploaded_records, 1);
    assert_eq!(result.confirmed_records, 1);
    let saved = store
        .get_qso("local-1")
        .await
        .expect("read QSO")
        .expect("saved QSO");
    assert_eq!(
        LotwSyncStatus::try_from(saved.lotw_sync_status),
        Ok(LotwSyncStatus::Confirmed)
    );
    assert_eq!(
        QslStatus::try_from(saved.lotw_sent_status),
        Ok(QslStatus::Yes)
    );
    assert_eq!(saved.worked_grid.as_deref(), Some("DM34"));
    assert_eq!(saved.notes.as_deref(), Some("Keep this operator note."));
    assert_eq!(
        store
            .get_sync_metadata()
            .await
            .expect("read metadata")
            .lotw_last_qsl
            .as_deref(),
        Some("2026-08-09 12:34:56")
    );
}

#[tokio::test]
async fn sync_marks_ambiguous_matches_as_conflicts() {
    let store = MemoryStorage::new();
    store
        .insert_qso(&qso("local-1", 1_754_621_400))
        .await
        .expect("insert first QSO");
    store
        .insert_qso(&qso("local-2", 1_754_621_520))
        .await
        .expect("insert second QSO");
    let api = MockLotwApi::successful(LotwReport {
        confirmations: vec![qso("report-1", 1_754_621_460)],
        high_water: None,
    });

    let result = execute_sync(&api, &store, true, false, true)
        .await
        .expect("LoTW sync");

    assert_eq!(result.conflict_records, 1);
    for local_id in ["local-1", "local-2"] {
        let saved = store
            .get_qso(local_id)
            .await
            .expect("read QSO")
            .expect("saved QSO");
        assert_eq!(
            LotwSyncStatus::try_from(saved.lotw_sync_status),
            Ok(LotwSyncStatus::Conflict)
        );
    }
}

#[tokio::test]
async fn sync_persists_upload_and_download_failures_for_retry() {
    let store = MemoryStorage::new();
    store
        .insert_qso(&qso("local-1", 1_754_621_400))
        .await
        .expect("insert QSO");
    let api = MockLotwApi::failing(Some("signing failed"), Some("report unavailable"));

    let result = execute_sync(&api, &store, false, true, true)
        .await
        .expect("LoTW sync result");

    assert_eq!(result.error_records, 2);
    let error = result.error_summary.expect("error summary");
    assert!(error.contains("signing failed"));
    assert!(error.contains("report unavailable"));
    let saved = store
        .get_qso("local-1")
        .await
        .expect("read QSO")
        .expect("saved QSO");
    assert_eq!(
        LotwSyncStatus::try_from(saved.lotw_sync_status),
        Ok(LotwSyncStatus::Failed)
    );
}

#[tokio::test]
async fn incremental_sync_preserves_high_water_and_reports_unmatched_qsos() {
    let store = MemoryStorage::new();
    let mut uploaded = qso("local-1", 1_754_621_400);
    uploaded.lotw_sync_status = LotwSyncStatus::Uploaded.into();
    store.insert_qso(&uploaded).await.expect("insert QSO");
    store
        .upsert_sync_metadata(&SyncMetadata {
            lotw_last_qsl: Some("2026-08-01 01:02:03".to_string()),
            ..SyncMetadata::default()
        })
        .await
        .expect("save metadata");
    let mut unmatched = qso("remote-1", 1_754_621_400);
    unmatched.worked_callsign = "W1AW".to_string();
    let api = MockLotwApi::successful(LotwReport {
        confirmations: vec![unmatched],
        high_water: None,
    });

    let result = execute_sync(&api, &store, false, true, true)
        .await
        .expect("incremental sync");

    assert_eq!(result.uploaded_records, 0);
    assert_eq!(result.unmatched_records, 1);
    assert_eq!(
        result.confirmation_high_water.as_deref(),
        Some("2026-08-01 01:02:03")
    );
    assert_eq!(
        api.fetched_since
            .lock()
            .expect("read captured high-water")
            .as_slice(),
        &[Some("2026-08-01 01:02:03".to_string())]
    );
}

#[tokio::test]
async fn full_sync_does_not_send_saved_high_water() {
    let store = MemoryStorage::new();
    store
        .upsert_sync_metadata(&SyncMetadata {
            lotw_last_qsl: Some("2026-08-01 01:02:03".to_string()),
            ..SyncMetadata::default()
        })
        .await
        .expect("save metadata");
    let api = MockLotwApi::successful(LotwReport::default());

    let result = execute_sync(&api, &store, true, false, true)
        .await
        .expect("full sync");

    assert_eq!(result.total_records, 0);
    assert_eq!(
        api.fetched_since
            .lock()
            .expect("read captured high-water")
            .as_slice(),
        &[None]
    );
}

#[tokio::test]
async fn single_upload_marks_unchanged_qso_as_uploaded() {
    let store = MemoryStorage::new();
    let snapshot = qso("local-1", 1_754_621_400);
    store.insert_qso(&snapshot).await.expect("insert QSO");
    let api = MockLotwApi::successful(LotwReport::default());

    let saved = upload_single_qso(&api, &store, &snapshot)
        .await
        .expect("single upload");

    assert_eq!(
        LotwSyncStatus::try_from(saved.lotw_sync_status),
        Ok(LotwSyncStatus::Uploaded)
    );
    assert_eq!(saved.lotw_sent, Some(true));
    assert!(saved.lotw_sent_date.is_some());
}

#[tokio::test]
async fn single_upload_keeps_concurrent_edit_pending() {
    let store = MemoryStorage::new();
    let snapshot = qso("local-1", 1_754_621_400);
    store.insert_qso(&snapshot).await.expect("insert QSO");
    let mut current = snapshot.clone();
    current.updated_at = Some(timestamp(1_754_621_401));
    current.notes = Some("Changed during upload".to_string());
    store.update_qso(&current).await.expect("update QSO");
    let api = MockLotwApi::successful(LotwReport::default());

    let saved = upload_single_qso(&api, &store, &snapshot)
        .await
        .expect("single upload");

    assert_eq!(
        LotwSyncStatus::try_from(saved.lotw_sync_status),
        Ok(LotwSyncStatus::Modified)
    );
    assert_eq!(saved.notes.as_deref(), Some("Changed during upload"));
}

#[tokio::test]
async fn single_upload_failure_marks_qso_as_failed() {
    let store = MemoryStorage::new();
    let snapshot = qso("local-1", 1_754_621_400);
    store.insert_qso(&snapshot).await.expect("insert QSO");
    let api = MockLotwApi::failing(Some("certificate rejected"), None);

    let error = upload_single_qso(&api, &store, &snapshot)
        .await
        .expect_err("single upload failure");

    assert!(error.to_string().contains("certificate rejected"));
    let saved = store
        .get_qso("local-1")
        .await
        .expect("read QSO")
        .expect("saved QSO");
    assert_eq!(
        LotwSyncStatus::try_from(saved.lotw_sync_status),
        Ok(LotwSyncStatus::Failed)
    );
}

#[tokio::test]
async fn single_upload_handles_deleted_and_missing_qsos() {
    let store = MemoryStorage::new();
    let snapshot = qso("deleted", 1_754_621_400);
    store.insert_qso(&snapshot).await.expect("insert QSO");
    store
        .soft_delete_qso("deleted", 1_754_621_500_000, false)
        .await
        .expect("delete QSO");
    let api = MockLotwApi::successful(LotwReport::default());

    let deleted = upload_single_qso(&api, &store, &snapshot)
        .await
        .expect("deleted QSO remains available");
    assert!(deleted.deleted_at.is_some());
    assert_eq!(
        LotwSyncStatus::try_from(deleted.lotw_sync_status),
        Ok(LotwSyncStatus::LocalOnly)
    );

    let missing = qso("missing", 1_754_621_400);
    let error = upload_single_qso(&api, &store, &missing)
        .await
        .expect_err("missing QSO");
    assert!(error.to_string().contains("disappeared"));
}

#[tokio::test]
async fn invalid_lotw_status_is_treated_as_pending() {
    let store = MemoryStorage::new();
    let mut local = qso("local-1", 1_754_621_400);
    local.lotw_sync_status = i32::MAX;
    store.insert_qso(&local).await.expect("insert QSO");
    let api = MockLotwApi::successful(LotwReport::default());

    let result = execute_sync(&api, &store, false, true, false)
        .await
        .expect("upload sync");

    assert_eq!(result.uploaded_records, 1);
}

fn qso(local_id: &str, seconds: i64) -> QsoRecord {
    QsoRecord {
        local_id: local_id.to_string(),
        station_callsign: "KC7AVA".to_string(),
        worked_callsign: "N7XAK".to_string(),
        utc_timestamp: Some(timestamp(seconds)),
        created_at: Some(timestamp(seconds)),
        updated_at: Some(timestamp(seconds)),
        band: Band::Band20m as i32,
        mode: Mode::Ft8 as i32,
        lotw_sync_status: LotwSyncStatus::LocalOnly as i32,
        ..QsoRecord::default()
    }
}

const fn timestamp(seconds: i64) -> Timestamp {
    Timestamp { seconds, nanos: 0 }
}

#[test]
fn sync_metadata_defaults_do_not_require_qrz_values() {
    let metadata = SyncMetadata {
        lotw_last_qsl: Some("2026-08-09 12:34:56".to_string()),
        ..SyncMetadata::default()
    };
    assert_eq!(metadata.qrz_qso_count, 0);
}

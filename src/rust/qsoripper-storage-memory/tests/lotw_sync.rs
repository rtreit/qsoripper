//! `LoTW` sync tests for the in-memory storage adapter.

#![allow(clippy::expect_used)]

use prost_types::Timestamp;
use qsoripper_core::adif::parse_adi_qsos;
use qsoripper_core::lotw::{execute_sync, LotwApi, LotwError, LotwReport, LotwUploadResult};
use qsoripper_core::proto::qsoripper::domain::{Band, LotwSyncStatus, Mode, QslStatus, QsoRecord};
use qsoripper_core::storage::{LogbookStore, SyncMetadata};
use qsoripper_storage_memory::MemoryStorage;

struct MockLotwApi {
    report: LotwReport,
}

#[tonic::async_trait]
impl LotwApi for MockLotwApi {
    async fn upload_qsos(&self, qsos: &[QsoRecord]) -> Result<LotwUploadResult, LotwError> {
        Ok(LotwUploadResult {
            submitted: u32::try_from(qsos.len()).unwrap_or(u32::MAX),
        })
    }

    async fn fetch_confirmations(&self, _since: Option<&str>) -> Result<LotwReport, LotwError> {
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
    let api = MockLotwApi {
        report: LotwReport {
            confirmations,
            high_water: Some("2026-08-09 12:34:56".to_string()),
        },
    };

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
    let api = MockLotwApi {
        report: LotwReport {
            confirmations: vec![qso("report-1", 1_754_621_460)],
            high_water: None,
        },
    };

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

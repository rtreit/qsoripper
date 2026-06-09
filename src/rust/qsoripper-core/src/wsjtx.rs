//! WSJT-X ingestion helpers that feed the existing ADIF import pipeline.

use crate::adif::parse_adi_qsos_without_header_detection;
use crate::application::logbook::{AdifImportSummary, LogbookEngine, LogbookError};
use crate::proto::qsoripper::domain::StationProfile;
use serde_json::Value;
use std::fs;
use std::path::Path;

/// Import a WSJT-X UDP datagram by extracting any embedded ADIF payload and
/// feeding it through the normal logbook import path.
///
/// The parser accepts both raw ADIF text and lightweight JSON envelopes such as
/// `{"type":"logged_qso","adif":"<CALL:...>"}` that WSJT-X integrations often
/// emit when forwarding QSO events to downstream tooling.
///
/// # Errors
///
/// Returns an error when the datagram has no usable ADIF payload or when the
/// import pipeline rejects the parsed QSO records.
pub async fn ingest_wsjtx_udp_datagram(
    logbook_engine: &LogbookEngine,
    datagram: &[u8],
    active_station_profile: Option<&StationProfile>,
    refresh: bool,
) -> Result<AdifImportSummary, String> {
    let adif_payload = extract_adif_payload(datagram)?;
    ingest_adif_payload(
        logbook_engine,
        &adif_payload,
        active_station_profile,
        refresh,
    )
    .await
}

/// Import any newly appended ADIF data from a WSJT-X log file.
///
/// The current implementation keeps a simple byte offset cursor so the caller can
/// replay appended records without re-importing the full file every time.
///
/// # Errors
///
/// Returns an error when the file cannot be read or when the ADIF import path
/// rejects the newly appended records.
pub async fn ingest_wsjtx_adif_tail(
    logbook_engine: &LogbookEngine,
    path: &Path,
    active_station_profile: Option<&StationProfile>,
    refresh: bool,
    cursor: &mut usize,
) -> Result<AdifImportSummary, String> {
    let bytes =
        fs::read(path).map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    if *cursor >= bytes.len() {
        return Ok(AdifImportSummary::default());
    }

    let Some(appended) = bytes.get(*cursor..) else {
        return Ok(AdifImportSummary::default());
    };
    let appended = appended.to_vec();
    *cursor = bytes.len();
    ingest_adif_payload(logbook_engine, &appended, active_station_profile, refresh).await
}

fn extract_adif_payload(datagram: &[u8]) -> Result<Vec<u8>, String> {
    if let Ok(value) = serde_json::from_slice::<Value>(datagram) {
        if let Some(adif) = value.get("adif").and_then(Value::as_str).map(str::to_owned) {
            return Ok(adif.into_bytes());
        }

        if let Some(adif) = value.get("qsos").and_then(Value::as_str).map(str::to_owned) {
            return Ok(adif.into_bytes());
        }

        if let Some(adif) = value
            .get("payload")
            .and_then(Value::as_str)
            .map(str::to_owned)
        {
            return Ok(adif.into_bytes());
        }
    }

    if datagram
        .iter()
        .any(|byte| *byte == b'<' || *byte == b'>' || *byte == b'\n')
    {
        return Ok(datagram.to_vec());
    }

    Err("WSJT-X datagram does not contain a usable ADIF payload".to_string())
}

async fn ingest_adif_payload(
    logbook_engine: &LogbookEngine,
    adif_payload: &[u8],
    active_station_profile: Option<&StationProfile>,
    refresh: bool,
) -> Result<AdifImportSummary, String> {
    let qsos = parse_adi_qsos_without_header_detection(adif_payload)
        .await
        .map_err(|error| format!("failed to parse ADIF payload: {error}"))?;

    if qsos.is_empty() {
        return Ok(AdifImportSummary::default());
    }

    logbook_engine
        .import_adif_qsos(qsos, active_station_profile, refresh)
        .await
        .map_err(format_wsjtx_error)
}

fn format_wsjtx_error(error: LogbookError) -> String {
    match error {
        LogbookError::Validation(message) => format!("WSJT-X import validation failed: {message}"),
        LogbookError::NotFound(message) => format!("WSJT-X import failed to find QSO: {message}"),
        LogbookError::AlreadyDeleted(message) => {
            format!("WSJT-X import failed because QSO is deleted: {message}")
        }
        LogbookError::Storage(error) => format!("WSJT-X import storage failed: {error}"),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::{extract_adif_payload, ingest_wsjtx_adif_tail, ingest_wsjtx_udp_datagram};
    use crate::application::logbook::LogbookEngine;
    use crate::proto::qsoripper::domain::QsoRecord;
    use crate::storage::{
        EngineStorage, LogbookCounts, LogbookStore, LookupSnapshot, LookupSnapshotStore,
        QsoHistoryPage, QsoListQuery, StorageError, SyncMetadata,
    };
    use std::collections::BTreeMap;
    use std::fs;
    use std::sync::Arc;
    use tempfile::NamedTempFile;
    use tokio::sync::RwLock;

    #[derive(Default)]
    struct TestStorage {
        qsos: RwLock<BTreeMap<String, QsoRecord>>,
        sync_metadata: RwLock<SyncMetadata>,
        lookup_snapshots: RwLock<BTreeMap<String, LookupSnapshot>>,
    }

    impl TestStorage {
        fn new() -> Self {
            Self::default()
        }
    }

    impl EngineStorage for TestStorage {
        fn logbook(&self) -> &dyn LogbookStore {
            self
        }

        fn lookup_snapshots(&self) -> &dyn LookupSnapshotStore {
            self
        }

        fn backend_name(&self) -> &'static str {
            "test"
        }
    }

    #[tonic::async_trait]
    impl LogbookStore for TestStorage {
        async fn insert_qso(&self, qso: &QsoRecord) -> Result<(), StorageError> {
            let mut qsos = self.qsos.write().await;
            if qsos.contains_key(&qso.local_id) {
                return Err(StorageError::duplicate("qso", &qso.local_id));
            }
            qsos.insert(qso.local_id.clone(), qso.clone());
            Ok(())
        }

        async fn update_qso(&self, qso: &QsoRecord) -> Result<bool, StorageError> {
            let mut qsos = self.qsos.write().await;
            Ok(qsos.insert(qso.local_id.clone(), qso.clone()).is_some())
        }

        async fn delete_qso(&self, local_id: &str) -> Result<bool, StorageError> {
            Ok(self.qsos.write().await.remove(local_id).is_some())
        }

        async fn soft_delete_qso(
            &self,
            local_id: &str,
            deleted_at_ms: i64,
            pending_remote_delete: bool,
        ) -> Result<bool, StorageError> {
            let mut qsos = self.qsos.write().await;
            let Some(record) = qsos.get_mut(local_id) else {
                return Ok(false);
            };
            record.deleted_at = Some(millis_to_timestamp(deleted_at_ms));
            record.pending_remote_delete = pending_remote_delete;
            Ok(true)
        }

        async fn restore_qso(&self, local_id: &str) -> Result<bool, StorageError> {
            let mut qsos = self.qsos.write().await;
            let Some(record) = qsos.get_mut(local_id) else {
                return Ok(false);
            };
            record.deleted_at = None;
            record.pending_remote_delete = false;
            Ok(true)
        }

        async fn get_qso(&self, local_id: &str) -> Result<Option<QsoRecord>, StorageError> {
            Ok(self.qsos.read().await.get(local_id).cloned())
        }

        async fn list_qsos(&self, _query: &QsoListQuery) -> Result<Vec<QsoRecord>, StorageError> {
            Ok(self.qsos.read().await.values().cloned().collect())
        }

        async fn list_qso_history(
            &self,
            _worked_callsign: &str,
            _limit: u32,
        ) -> Result<QsoHistoryPage, StorageError> {
            Ok(QsoHistoryPage::default())
        }

        async fn qso_counts(&self) -> Result<LogbookCounts, StorageError> {
            Ok(LogbookCounts::default())
        }

        async fn get_sync_metadata(&self) -> Result<SyncMetadata, StorageError> {
            Ok(self.sync_metadata.read().await.clone())
        }

        async fn upsert_sync_metadata(&self, metadata: &SyncMetadata) -> Result<(), StorageError> {
            *self.sync_metadata.write().await = metadata.clone();
            Ok(())
        }

        async fn purge_deleted_qsos(
            &self,
            _local_ids: &[String],
            _older_than_ms: Option<i64>,
        ) -> Result<u32, StorageError> {
            Ok(0)
        }
    }

    #[tonic::async_trait]
    impl LookupSnapshotStore for TestStorage {
        async fn get_lookup_snapshot(
            &self,
            callsign: &str,
        ) -> Result<Option<LookupSnapshot>, StorageError> {
            Ok(self.lookup_snapshots.read().await.get(callsign).cloned())
        }

        async fn upsert_lookup_snapshot(
            &self,
            snapshot: &LookupSnapshot,
        ) -> Result<(), StorageError> {
            self.lookup_snapshots
                .write()
                .await
                .insert(snapshot.callsign.clone(), snapshot.clone());
            Ok(())
        }

        async fn delete_lookup_snapshot(&self, callsign: &str) -> Result<bool, StorageError> {
            Ok(self
                .lookup_snapshots
                .write()
                .await
                .remove(callsign)
                .is_some())
        }
    }

    fn millis_to_timestamp(millis: i64) -> prost_types::Timestamp {
        let seconds = millis.div_euclid(1_000);
        let nanos = i32::try_from(millis.rem_euclid(1_000))
            .unwrap_or(0)
            .saturating_mul(1_000_000);
        prost_types::Timestamp { seconds, nanos }
    }

    fn sample_adif() -> &'static [u8] {
        b"<STATION_CALLSIGN:4>W1AW <CALL:5>K7ABC <QSO_DATE:8>20250101 <TIME_ON:4>1200 <BAND:3>20M <MODE:3>FT8 <RST_SENT:3>-10 <RST_RCVD:3>-12 <EOR>"
    }

    #[test]
    fn extract_adif_payload_accepts_json_wrapped_adif() {
        let payload = br#"{"type":"logged_qso","adif":"<CALL:5>K7ABC <EOR>"}"#;

        let extracted = extract_adif_payload(payload).expect("payload");

        assert_eq!(String::from_utf8(extracted).unwrap(), "<CALL:5>K7ABC <EOR>");
    }

    #[tokio::test]
    async fn ingest_wsjtx_udp_datagram_imports_qso_from_json_payload() {
        let engine = LogbookEngine::new(Arc::new(TestStorage::new()));
        let payload = br#"{"type":"logged_qso","adif":"<STATION_CALLSIGN:4>W1AW <CALL:5>K7ABC <QSO_DATE:8>20250101 <TIME_ON:4>1200 <BAND:3>20M <MODE:3>FT8 <RST_SENT:3>-10 <RST_RCVD:3>-12 <EOR>"}"#;

        let summary = ingest_wsjtx_udp_datagram(&engine, payload, None, false)
            .await
            .expect("import succeeded");

        assert_eq!(summary.records_imported, 1);
        assert_eq!(
            engine
                .list_qsos(&QsoListQuery::default())
                .await
                .expect("qsos")
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn ingest_wsjtx_adif_tail_imports_appended_records() {
        let engine = LogbookEngine::new(Arc::new(TestStorage::new()));
        let file = NamedTempFile::new().expect("temp file");
        let path = file.path().to_path_buf();

        fs::write(&path, sample_adif()).expect("write initial record");

        let mut cursor = 0;
        let summary = ingest_wsjtx_adif_tail(&engine, &path, None, false, &mut cursor)
            .await
            .expect("tail import");

        assert_eq!(summary.records_imported, 1);
        let metadata_len =
            usize::try_from(fs::metadata(&path).unwrap().len()).expect("metadata len");
        assert_eq!(cursor, metadata_len);
    }
}

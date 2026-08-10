//! Local reviewed contest details catalog enrichment.

use std::{fs::File, path::Path, sync::Arc};

use serde::Deserialize;

use crate::proto::qsoripper::domain::{Band, ContestCalendarEntry, ContestDetailsStatus, Mode};

use super::provider::{ContestCalendarProvider, ContestCalendarProviderError};

/// Environment variable pointing at a reviewed local contest details catalog.
pub const CONTEST_CALENDAR_DETAILS_PATH_ENV_VAR: &str = "QSORIPPER_CONTEST_CALENDAR_DETAILS_PATH";
/// Default reviewed local contest details catalog path.
pub const DEFAULT_CONTEST_CALENDAR_DETAILS_PATH: &str =
    "data/contest-calendar/contest-details.json";

/// Reviewed local contest details used to enrich calendar metadata.
pub struct ContestDetailsCatalog {
    entries: Vec<ContestDetailsCatalogEntry>,
}

impl ContestDetailsCatalog {
    /// Load a reviewed local details catalog from JSON.
    ///
    /// # Errors
    ///
    /// Returns a parse error when the file cannot be read or decoded.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ContestCalendarProviderError> {
        let path = path.as_ref();
        let file = File::open(path).map_err(|error| {
            ContestCalendarProviderError::parse(format!(
                "failed to read contest details catalog '{}': {error}",
                path.display()
            ))
        })?;
        let document: ContestDetailsCatalogFile =
            serde_json::from_reader(file).map_err(|error| {
                ContestCalendarProviderError::parse(format!(
                    "failed to parse contest details catalog '{}': {error}",
                    path.display()
                ))
            })?;
        Ok(Self {
            entries: document.entries.into_iter().map(Into::into).collect(),
        })
    }

    /// Enrich a contest entry with matching local catalog details.
    #[must_use]
    pub fn enrich(&self, contest: &ContestCalendarEntry) -> ContestCalendarEntry {
        let Some(entry) = self.entries.iter().find(|entry| entry.is_match(contest)) else {
            return contest.clone();
        };

        let mut enriched = contest.clone();
        if !entry.bands.is_empty() {
            enriched.bands.clone_from(&entry.bands);
        }
        if !entry.modes.is_empty() {
            enriched.modes.clone_from(&entry.modes);
        }
        if let Some(exchange) = entry.exchange.as_ref().filter(|value| !value.is_empty()) {
            enriched.exchange = Some(exchange.clone());
        }
        if let Some(rules_url) = entry.rules_url.as_ref().filter(|value| !value.is_empty()) {
            enriched.rules_url = Some(rules_url.clone());
        }
        enriched.details_status = entry.details_status;
        enriched
    }
}

/// Decorates contest calendar metadata with reviewed local contest details.
pub struct CatalogEnrichingContestCalendarProvider {
    inner: Arc<dyn ContestCalendarProvider>,
    catalog: ContestDetailsCatalog,
}

impl CatalogEnrichingContestCalendarProvider {
    /// Create a catalog-enriching provider.
    #[must_use]
    pub fn new(inner: Arc<dyn ContestCalendarProvider>, catalog: ContestDetailsCatalog) -> Self {
        Self { inner, catalog }
    }
}

#[tonic::async_trait]
impl ContestCalendarProvider for CatalogEnrichingContestCalendarProvider {
    async fn fetch_contests(
        &self,
    ) -> Result<Vec<ContestCalendarEntry>, ContestCalendarProviderError> {
        Ok(self
            .inner
            .fetch_contests()
            .await?
            .iter()
            .map(|contest| self.catalog.enrich(contest))
            .collect())
    }
}

#[derive(Debug, Deserialize, Default)]
struct ContestDetailsCatalogFile {
    #[serde(default)]
    entries: Vec<ContestDetailsCatalogItem>,
}

#[derive(Debug, Deserialize, Default)]
struct ContestDetailsCatalogItem {
    #[serde(default)]
    #[serde(alias = "contestId")]
    contest_id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    #[serde(alias = "sourceUrl")]
    source_url: Option<String>,
    #[serde(default)]
    bands: Vec<String>,
    #[serde(default)]
    modes: Vec<String>,
    #[serde(default)]
    exchange: Option<String>,
    #[serde(default)]
    #[serde(alias = "rulesUrl")]
    rules_url: Option<String>,
    #[serde(default)]
    #[serde(alias = "detailsStatus")]
    details_status: Option<String>,
}

struct ContestDetailsCatalogEntry {
    contest_id: String,
    name: String,
    source_url: String,
    bands: Vec<i32>,
    modes: Vec<i32>,
    exchange: Option<String>,
    rules_url: Option<String>,
    details_status: i32,
}

impl ContestDetailsCatalogEntry {
    fn is_match(&self, contest: &ContestCalendarEntry) -> bool {
        if !self.contest_id.is_empty()
            && self.contest_id == normalize(Some(contest.contest_id.as_str()))
        {
            return true;
        }

        if !self.source_url.is_empty()
            && self.source_url == normalize(contest.source_url.as_deref())
        {
            return true;
        }

        !self.name.is_empty() && self.name == normalize(Some(contest.name.as_str()))
    }
}

impl From<ContestDetailsCatalogItem> for ContestDetailsCatalogEntry {
    fn from(value: ContestDetailsCatalogItem) -> Self {
        Self {
            contest_id: normalize(value.contest_id.as_deref()),
            name: normalize(value.name.as_deref()),
            source_url: normalize(value.source_url.as_deref()),
            bands: value
                .bands
                .iter()
                .filter_map(|band| parse_band(band))
                .map(|band| band as i32)
                .collect(),
            modes: value
                .modes
                .iter()
                .filter_map(|mode| parse_mode(mode))
                .map(|mode| mode as i32)
                .collect(),
            exchange: value.exchange.map(|value| value.trim().to_string()),
            rules_url: value.rules_url.map(|value| value.trim().to_string()),
            details_status: parse_details_status(value.details_status.as_deref()) as i32,
        }
    }
}

fn normalize(value: Option<&str>) -> String {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_uppercase)
        .unwrap_or_default()
}

fn parse_details_status(value: Option<&str>) -> ContestDetailsStatus {
    match normalize(value).as_str() {
        "FULL" => ContestDetailsStatus::Full,
        "METADATAONLY" | "METADATA_ONLY" => ContestDetailsStatus::MetadataOnly,
        _ => ContestDetailsStatus::Partial,
    }
}

fn parse_band(value: &str) -> Option<Band> {
    Some(match normalize(Some(value)).as_str() {
        "160M" => Band::Band160m,
        "80M" => Band::Band80m,
        "40M" => Band::Band40m,
        "30M" => Band::Band30m,
        "20M" => Band::Band20m,
        "17M" => Band::Band17m,
        "15M" => Band::Band15m,
        "12M" => Band::Band12m,
        "10M" => Band::Band10m,
        "6M" => Band::Band6m,
        "2M" => Band::Band2m,
        "70CM" => Band::Band70cm,
        _ => return None,
    })
}

fn parse_mode(value: &str) -> Option<Mode> {
    Some(match normalize(Some(value)).as_str() {
        "CW" => Mode::Cw,
        "SSB" => Mode::Ssb,
        "RTTY" => Mode::Rtty,
        "FT8" => Mode::Ft8,
        "FM" => Mode::Fm,
        "AM" => Mode::Am,
        _ => return None,
    })
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn catalog_enriches_by_name() {
        let path = temp_catalog_path();
        fs::write(
            &path,
            r#"{
                "entries": [{
                    "name": "Real Time Contest",
                    "bands": ["20m", "40m"],
                    "modes": ["cw", "ssb"],
                    "exchange": "RST + serial",
                    "rulesUrl": "https://example.test/rules",
                    "detailsStatus": "full"
                }]
            }"#,
        )
        .expect("write catalog");

        let catalog = ContestDetailsCatalog::load(&path).expect("catalog");
        let contest = ContestCalendarEntry {
            contest_id: "contest".to_string(),
            name: "Real Time Contest".to_string(),
            source_name: "WA7BNM Contest Calendar".to_string(),
            details_status: ContestDetailsStatus::MetadataOnly as i32,
            ..ContestCalendarEntry::default()
        };

        let enriched = catalog.enrich(&contest);

        assert_eq!(
            enriched.bands,
            vec![Band::Band20m as i32, Band::Band40m as i32]
        );
        assert_eq!(enriched.modes, vec![Mode::Cw as i32, Mode::Ssb as i32]);
        assert_eq!(enriched.exchange.as_deref(), Some("RST + serial"));
        assert_eq!(
            enriched.rules_url.as_deref(),
            Some("https://example.test/rules")
        );
        assert_eq!(enriched.details_status, ContestDetailsStatus::Full as i32);
    }

    #[test]
    fn catalog_preserves_metadata_only_status() {
        let path = temp_catalog_path();
        fs::write(
            &path,
            r#"{
                "entries": [{
                    "contestId": "contest",
                    "detailsStatus": "metadataOnly"
                }]
            }"#,
        )
        .expect("write catalog");

        let catalog = ContestDetailsCatalog::load(&path).expect("catalog");
        let contest = ContestCalendarEntry {
            contest_id: "contest".to_string(),
            details_status: ContestDetailsStatus::Partial as i32,
            ..ContestCalendarEntry::default()
        };

        let enriched = catalog.enrich(&contest);

        assert_eq!(
            enriched.details_status,
            ContestDetailsStatus::MetadataOnly as i32
        );
    }

    fn temp_catalog_path() -> tempfile::TempPath {
        tempfile::Builder::new()
            .prefix("qsoripper-contest-catalog-")
            .suffix(".json")
            .tempfile()
            .expect("create temp catalog")
            .into_temp_path()
    }
}

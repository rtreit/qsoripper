//! WA7BNM RSS contest calendar provider.

use std::{env, fmt, path::PathBuf, time::Duration};

use chrono::{DateTime, Datelike, NaiveDate, NaiveTime};
use prost_types::Timestamp;
use serde::Deserialize;

use crate::proto::qsoripper::domain::{ContestCalendarEntry, ContestDetailsStatus};

use super::{
    catalog::{CONTEST_CALENDAR_DETAILS_PATH_ENV_VAR, DEFAULT_CONTEST_CALENDAR_DETAILS_PATH},
    provider::{ContestCalendarProvider, ContestCalendarProviderError},
};

/// Environment variable that enables or disables contest calendar fetching.
pub const CONTEST_CALENDAR_ENABLED_ENV_VAR: &str = "QSORIPPER_CONTEST_CALENDAR_ENABLED";
/// Environment variable that overrides the WA7BNM RSS URL.
pub const CONTEST_CALENDAR_RSS_URL_ENV_VAR: &str = "QSORIPPER_CONTEST_CALENDAR_RSS_URL";
/// Environment variable that overrides the HTTP timeout in seconds.
pub const CONTEST_CALENDAR_HTTP_TIMEOUT_SECONDS_ENV_VAR: &str =
    "QSORIPPER_CONTEST_CALENDAR_HTTP_TIMEOUT_SECONDS";
/// Environment variable that overrides the refresh interval in seconds.
pub const CONTEST_CALENDAR_REFRESH_INTERVAL_SECONDS_ENV_VAR: &str =
    "QSORIPPER_CONTEST_CALENDAR_REFRESH_INTERVAL_SECONDS";
/// Environment variable that overrides the stale-after threshold in seconds.
pub const CONTEST_CALENDAR_STALE_AFTER_SECONDS_ENV_VAR: &str =
    "QSORIPPER_CONTEST_CALENDAR_STALE_AFTER_SECONDS";

/// Default WA7BNM RSS endpoint.
pub const DEFAULT_CONTEST_CALENDAR_RSS_URL: &str = "https://www.contestcalendar.com/calendar.rss";
/// Default contest calendar HTTP timeout.
pub const DEFAULT_CONTEST_CALENDAR_HTTP_TIMEOUT_SECONDS: u64 = 8;
/// Default contest calendar refresh interval.
pub const DEFAULT_CONTEST_CALENDAR_REFRESH_INTERVAL_SECONDS: u64 = 3600;
/// Default stale-after threshold.
pub const DEFAULT_CONTEST_CALENDAR_STALE_AFTER_SECONDS: u64 = 86_400;

/// WA7BNM contest calendar provider configuration.
#[derive(Clone)]
pub struct Wa7bnmContestCalendarConfig {
    enabled: bool,
    rss_url: String,
    http_timeout: Duration,
    refresh_interval: Duration,
    stale_after: Duration,
    details_path: Option<PathBuf>,
    details_path_is_explicit: bool,
}

impl fmt::Debug for Wa7bnmContestCalendarConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Wa7bnmContestCalendarConfig")
            .field("enabled", &self.enabled)
            .field("rss_url", &self.rss_url)
            .field("http_timeout", &self.http_timeout)
            .field("refresh_interval", &self.refresh_interval)
            .field("stale_after", &self.stale_after)
            .field("details_path", &self.details_path)
            .field("details_path_is_explicit", &self.details_path_is_explicit)
            .finish()
    }
}

impl Wa7bnmContestCalendarConfig {
    /// Load provider configuration from environment variables.
    ///
    /// # Errors
    ///
    /// Returns a message when integer or boolean settings cannot be parsed.
    pub fn from_env() -> Result<Self, String> {
        Self::from_value_provider(|name| env::var(name).ok())
    }

    /// Load provider configuration from an arbitrary key/value source.
    ///
    /// # Errors
    ///
    /// Returns a message when integer or boolean settings cannot be parsed.
    pub fn from_value_provider<F>(mut get_value: F) -> Result<Self, String>
    where
        F: FnMut(&'static str) -> Option<String>,
    {
        let enabled = optional_value_bool(CONTEST_CALENDAR_ENABLED_ENV_VAR, true, &mut get_value)?;
        let rss_url = optional_value(CONTEST_CALENDAR_RSS_URL_ENV_VAR, &mut get_value)
            .unwrap_or_else(|| DEFAULT_CONTEST_CALENDAR_RSS_URL.to_string());
        let http_timeout_seconds = optional_value_u64(
            CONTEST_CALENDAR_HTTP_TIMEOUT_SECONDS_ENV_VAR,
            DEFAULT_CONTEST_CALENDAR_HTTP_TIMEOUT_SECONDS,
            &mut get_value,
        )?;
        let refresh_interval_seconds = optional_value_u64(
            CONTEST_CALENDAR_REFRESH_INTERVAL_SECONDS_ENV_VAR,
            DEFAULT_CONTEST_CALENDAR_REFRESH_INTERVAL_SECONDS,
            &mut get_value,
        )?;
        let stale_after_seconds = optional_value_u64(
            CONTEST_CALENDAR_STALE_AFTER_SECONDS_ENV_VAR,
            DEFAULT_CONTEST_CALENDAR_STALE_AFTER_SECONDS,
            &mut get_value,
        )?;
        let details_path_value =
            optional_value(CONTEST_CALENDAR_DETAILS_PATH_ENV_VAR, &mut get_value);
        let details_path_is_explicit = details_path_value.is_some();
        let details_path =
            Some(PathBuf::from(details_path_value.unwrap_or_else(|| {
                DEFAULT_CONTEST_CALENDAR_DETAILS_PATH.to_string()
            })));

        Ok(Self {
            enabled,
            rss_url,
            http_timeout: Duration::from_secs(http_timeout_seconds),
            refresh_interval: Duration::from_secs(refresh_interval_seconds),
            stale_after: Duration::from_secs(stale_after_seconds),
            details_path,
            details_path_is_explicit,
        })
    }

    /// Return whether contest calendar fetching is enabled.
    #[must_use]
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Return the refresh interval for cached contest snapshots.
    #[must_use]
    pub fn refresh_interval(&self) -> Duration {
        self.refresh_interval
    }

    /// Return the stale-after threshold.
    #[must_use]
    pub fn stale_after(&self) -> Duration {
        self.stale_after
    }

    /// Return the optional reviewed local details catalog path.
    #[must_use]
    pub fn details_path(&self) -> Option<&PathBuf> {
        self.details_path.as_ref()
    }

    /// Return whether the details catalog path was explicitly configured.
    #[must_use]
    pub fn details_path_is_explicit(&self) -> bool {
        self.details_path_is_explicit
    }
}

/// WA7BNM RSS contest calendar provider.
pub struct Wa7bnmContestCalendarProvider {
    config: Wa7bnmContestCalendarConfig,
    client: reqwest::Client,
}

impl Wa7bnmContestCalendarProvider {
    /// Create a WA7BNM RSS provider.
    ///
    /// # Errors
    ///
    /// Returns a provider error if the HTTP client cannot be built.
    pub fn new(config: Wa7bnmContestCalendarConfig) -> Result<Self, ContestCalendarProviderError> {
        let client = reqwest::Client::builder()
            .timeout(config.http_timeout)
            .build()
            .map_err(|error| {
                ContestCalendarProviderError::transport(format!(
                    "failed to create HTTP client: {error}"
                ))
            })?;
        Ok(Self { config, client })
    }

    async fn fetch_rss(&self) -> Result<String, ContestCalendarProviderError> {
        self.client
            .get(&self.config.rss_url)
            .send()
            .await
            .map_err(|error| ContestCalendarProviderError::transport(error.to_string()))?
            .error_for_status()
            .map_err(|error| ContestCalendarProviderError::transport(error.to_string()))?
            .text()
            .await
            .map_err(|error| ContestCalendarProviderError::transport(error.to_string()))
    }
}

#[tonic::async_trait]
impl ContestCalendarProvider for Wa7bnmContestCalendarProvider {
    async fn fetch_contests(
        &self,
    ) -> Result<Vec<ContestCalendarEntry>, ContestCalendarProviderError> {
        let rss = self.fetch_rss().await?;
        parse_wa7bnm_rss(&rss, &self.config.rss_url)
    }
}

#[derive(Debug, Deserialize)]
struct Rss {
    channel: Channel,
}

#[derive(Debug, Deserialize)]
struct Channel {
    #[serde(rename = "lastBuildDate")]
    last_build_date: Option<String>,
    #[serde(default)]
    item: Vec<Item>,
}

#[derive(Debug, Deserialize)]
struct Item {
    title: String,
    link: Option<String>,
    description: String,
}

fn parse_wa7bnm_rss(
    xml: &str,
    source_url: &str,
) -> Result<Vec<ContestCalendarEntry>, ContestCalendarProviderError> {
    let rss: Rss = quick_xml::de::from_str(xml)
        .map_err(|error| ContestCalendarProviderError::parse(error.to_string()))?;
    let source_year = rss
        .channel
        .last_build_date
        .as_deref()
        .and_then(|value| DateTime::parse_from_rfc2822(value).ok())
        .map_or_else(|| chrono::Utc::now().year(), |value| value.year());

    rss.channel
        .item
        .into_iter()
        .map(|item| entry_from_item(item, source_year, source_url))
        .collect()
}

fn entry_from_item(
    item: Item,
    source_year: i32,
    fallback_source_url: &str,
) -> Result<ContestCalendarEntry, ContestCalendarProviderError> {
    let (start, end) = parse_schedule(&item.description, source_year)?;
    let source_url = item.link.unwrap_or_else(|| fallback_source_url.to_string());
    let contest_id = stable_contest_id(&item.title, &start);
    Ok(ContestCalendarEntry {
        contest_id,
        name: item.title,
        start_time_utc: Some(start),
        end_time_utc: Some(end),
        bands: Vec::new(),
        modes: Vec::new(),
        exchange: None,
        rules_url: None,
        source_url: Some(source_url),
        source_name: "WA7BNM Contest Calendar".to_string(),
        details_status: ContestDetailsStatus::MetadataOnly as i32,
    })
}

fn parse_schedule(
    description: &str,
    source_year: i32,
) -> Result<(Timestamp, Timestamp), ContestCalendarProviderError> {
    if let Some((start_part, end_part)) = split_multi_day(description) {
        let start_date_time = parse_time_and_date(start_part, source_year)?;
        let end_date_time = parse_time_and_date(end_part, source_year)?;
        if end_date_time < start_date_time {
            return Err(ContestCalendarProviderError::parse(format!(
                "contest end precedes start in schedule '{description}'"
            )));
        }
        return Ok((
            timestamp_from_naive(start_date_time),
            timestamp_from_naive(end_date_time),
        ));
    }

    let (times, date) = description.split_once(',').ok_or_else(|| {
        ContestCalendarProviderError::parse(format!("missing comma in schedule '{description}'"))
    })?;
    let (start_time, end_time) = times.trim().split_once('-').ok_or_else(|| {
        ContestCalendarProviderError::parse(format!(
            "missing time range in schedule '{description}'"
        ))
    })?;
    let date = parse_month_day(date.trim(), source_year)?;
    let start = parse_utc_time(start_time.trim())?;
    let end = parse_utc_time(end_time.trim())?;
    let start_date_time = date.and_time(start);
    let mut end_date_time = date.and_time(end);
    if end_date_time <= start_date_time {
        end_date_time = end_date_time
            .checked_add_signed(chrono::Duration::days(1))
            .ok_or_else(|| ContestCalendarProviderError::parse("contest end overflow"))?;
    }
    Ok((
        timestamp_from_naive(start_date_time),
        timestamp_from_naive(end_date_time),
    ))
}

fn split_multi_day(description: &str) -> Option<(&str, &str)> {
    for separator in [" to ", " - ", " thru ", " through "] {
        if let Some((start_part, end_part)) = description.split_once(separator) {
            if start_part.contains(',') && end_part.contains(',') {
                return Some((start_part.trim(), end_part.trim()));
            }
        }
    }
    None
}

fn parse_time_and_date(
    part: &str,
    source_year: i32,
) -> Result<chrono::NaiveDateTime, ContestCalendarProviderError> {
    let (time, date) = part.split_once(',').ok_or_else(|| {
        ContestCalendarProviderError::parse(format!("missing comma in schedule part '{part}'"))
    })?;
    let date = parse_month_day(date.trim(), source_year)?;
    let time = parse_utc_time(time.trim())?;
    Ok(date.and_time(time))
}

fn parse_month_day(value: &str, year: i32) -> Result<NaiveDate, ContestCalendarProviderError> {
    let mut parts = value.split_whitespace();
    let month = parts.next().ok_or_else(|| {
        ContestCalendarProviderError::parse(format!("missing month in '{value}'"))
    })?;
    let day = parts
        .next()
        .ok_or_else(|| ContestCalendarProviderError::parse(format!("missing day in '{value}'")))?
        .parse::<u32>()
        .map_err(|_| ContestCalendarProviderError::parse(format!("invalid day in '{value}'")))?;
    let month = match month {
        "Jan" | "January" => 1,
        "Feb" | "February" => 2,
        "Mar" | "March" => 3,
        "Apr" | "April" => 4,
        "May" => 5,
        "Jun" | "June" => 6,
        "Jul" | "July" => 7,
        "Aug" | "August" => 8,
        "Sep" | "Sept" | "September" => 9,
        "Oct" | "October" => 10,
        "Nov" | "November" => 11,
        "Dec" | "December" => 12,
        _ => {
            return Err(ContestCalendarProviderError::parse(format!(
                "invalid month '{month}'"
            )))
        }
    };
    NaiveDate::from_ymd_opt(year, month, day)
        .ok_or_else(|| ContestCalendarProviderError::parse(format!("invalid date '{value}'")))
}

fn parse_utc_time(value: &str) -> Result<NaiveTime, ContestCalendarProviderError> {
    let value = value.trim_end_matches('Z');
    if value.len() != 4 {
        return Err(ContestCalendarProviderError::parse(format!(
            "invalid UTC time '{value}'"
        )));
    }
    let hour = value[0..2]
        .parse::<u32>()
        .map_err(|_| ContestCalendarProviderError::parse(format!("invalid UTC hour '{value}'")))?;
    let minute = value[2..4].parse::<u32>().map_err(|_| {
        ContestCalendarProviderError::parse(format!("invalid UTC minute '{value}'"))
    })?;
    NaiveTime::from_hms_opt(hour, minute, 0)
        .ok_or_else(|| ContestCalendarProviderError::parse(format!("invalid UTC time '{value}'")))
}

fn timestamp_from_naive(value: chrono::NaiveDateTime) -> Timestamp {
    Timestamp {
        seconds: value.and_utc().timestamp(),
        nanos: 0,
    }
}

fn stable_contest_id(name: &str, start: &Timestamp) -> String {
    let input = format!("{}\n{}", name.trim().to_ascii_uppercase(), start.seconds);
    let mut hash = 14_695_981_039_346_656_037_u64;
    for byte in input.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(1_099_511_628_211);
    }
    format!("wa7bnm-{hash:016x}")
}

fn optional_value<F>(name: &'static str, get_value: &mut F) -> Option<String>
where
    F: FnMut(&'static str) -> Option<String>,
{
    get_value(name).and_then(|value| {
        let value = value.trim().to_string();
        if value.is_empty() {
            None
        } else {
            Some(value)
        }
    })
}

fn optional_value_u64<F>(
    name: &'static str,
    default_value: u64,
    get_value: &mut F,
) -> Result<u64, String>
where
    F: FnMut(&'static str) -> Option<String>,
{
    optional_value(name, get_value).map_or(Ok(default_value), |value| {
        value
            .parse::<u64>()
            .map_err(|_| format!("{name} must be an unsigned integer, got '{value}'"))
    })
}

fn optional_value_bool<F>(
    name: &'static str,
    default_value: bool,
    get_value: &mut F,
) -> Result<bool, String>
where
    F: FnMut(&'static str) -> Option<String>,
{
    optional_value(name, get_value).map_or(Ok(default_value), |value| {
        match value.to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "y" | "on" => Ok(true),
            "0" | "false" | "no" | "n" | "off" => Ok(false),
            _ => Err(format!("{name} must be boolean, got '{value}'")),
        }
    })
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn parse_wa7bnm_rss_returns_metadata_only_entries() {
        let xml = r#"<?xml version="1.0" encoding="utf-8" ?>
<rss version="2.0"><channel>
<lastBuildDate>Sat, 23 May 2026 00:00:00 +0000</lastBuildDate>
<item><title>Real Time Contest</title><link>https://www.contestcalendar.com/weeklycontdetails.php?ref=x</link><description>1600Z-2000Z, May 24</description></item>
</channel></rss>"#;

        let contests = parse_wa7bnm_rss(xml, DEFAULT_CONTEST_CALENDAR_RSS_URL).expect("contests");

        assert_eq!(contests.len(), 1);
        let contest = contests.first().expect("contest");
        assert_eq!(contest.name, "Real Time Contest");
        assert_eq!(
            contest.details_status,
            ContestDetailsStatus::MetadataOnly as i32
        );
        assert!(contest.bands.is_empty());
        assert_eq!(
            contest.start_time_utc.as_ref().expect("start").seconds,
            1_779_638_400
        );
        assert_eq!(
            contest.end_time_utc.as_ref().expect("end").seconds,
            1_779_652_800
        );
    }

    #[test]
    fn parse_schedule_rolls_end_time_to_next_day() {
        let (start, end) = parse_schedule("2300Z-0100Z, May 24", 2026).expect("schedule");

        assert_eq!(end.seconds - start.seconds, 7_200);
    }

    #[test]
    fn parse_schedule_handles_multi_day_range() {
        let (start, end) =
            parse_schedule("0000Z, May 30 to 2359Z, May 31", 2026).expect("schedule");

        let expected_start = NaiveDate::from_ymd_opt(2026, 5, 30)
            .expect("start date")
            .and_hms_opt(0, 0, 0)
            .expect("start time")
            .and_utc()
            .timestamp();
        let expected_end = NaiveDate::from_ymd_opt(2026, 5, 31)
            .expect("end date")
            .and_hms_opt(23, 59, 0)
            .expect("end time")
            .and_utc()
            .timestamp();

        assert_eq!(start.seconds, expected_start);
        assert_eq!(end.seconds, expected_end);
    }
}

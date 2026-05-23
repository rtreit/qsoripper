//! Contest calendar providers, caching, and filtering.

mod catalog;
mod monitor;
mod provider;
mod wa7bnm;

pub use catalog::{
    CatalogEnrichingContestCalendarProvider, ContestDetailsCatalog,
    CONTEST_CALENDAR_DETAILS_PATH_ENV_VAR, DEFAULT_CONTEST_CALENDAR_DETAILS_PATH,
};
pub use monitor::{ContestCalendarMonitor, ContestCalendarSnapshot};
pub use provider::{
    ContestCalendarProvider, ContestCalendarProviderError, ContestCalendarProviderErrorKind,
    DisabledContestCalendarProvider,
};
pub use wa7bnm::{
    Wa7bnmContestCalendarConfig, Wa7bnmContestCalendarProvider, CONTEST_CALENDAR_ENABLED_ENV_VAR,
    CONTEST_CALENDAR_HTTP_TIMEOUT_SECONDS_ENV_VAR,
    CONTEST_CALENDAR_REFRESH_INTERVAL_SECONDS_ENV_VAR, CONTEST_CALENDAR_RSS_URL_ENV_VAR,
    CONTEST_CALENDAR_STALE_AFTER_SECONDS_ENV_VAR, DEFAULT_CONTEST_CALENDAR_HTTP_TIMEOUT_SECONDS,
    DEFAULT_CONTEST_CALENDAR_REFRESH_INTERVAL_SECONDS, DEFAULT_CONTEST_CALENDAR_RSS_URL,
    DEFAULT_CONTEST_CALENDAR_STALE_AFTER_SECONDS,
};

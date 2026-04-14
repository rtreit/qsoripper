//! Space weather providers, normalization, and cached current snapshots.

mod monitor;
mod noaa;
mod provider;

pub use monitor::SpaceWeatherMonitor;
pub use noaa::{
    NoaaSpaceWeatherConfig, NoaaSpaceWeatherConfigError, NoaaSpaceWeatherProvider,
    DEFAULT_NOAA_HTTP_TIMEOUT_SECONDS, DEFAULT_NOAA_KP_INDEX_URL,
    DEFAULT_NOAA_REFRESH_INTERVAL_SECONDS, DEFAULT_NOAA_SOLAR_INDICES_URL,
    DEFAULT_NOAA_STALE_AFTER_SECONDS, NOAA_HTTP_TIMEOUT_SECONDS_ENV_VAR, NOAA_KP_INDEX_URL_ENV_VAR,
    NOAA_REFRESH_INTERVAL_SECONDS_ENV_VAR, NOAA_SOLAR_INDICES_URL_ENV_VAR,
    NOAA_SPACE_WEATHER_ENABLED_ENV_VAR, NOAA_STALE_AFTER_SECONDS_ENV_VAR,
};
pub use provider::{
    DisabledSpaceWeatherProvider, SpaceWeatherProvider, SpaceWeatherProviderError,
    SpaceWeatherProviderErrorKind,
};

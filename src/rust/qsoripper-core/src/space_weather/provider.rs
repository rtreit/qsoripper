//! Provider seam for current space weather data.

use crate::proto::qsoripper::domain::SpaceWeatherSnapshot;

/// Abstraction over an external current space weather provider.
#[tonic::async_trait]
pub trait SpaceWeatherProvider: Send + Sync {
    /// Fetch a fresh normalized current space weather snapshot.
    async fn fetch_current(&self) -> Result<SpaceWeatherSnapshot, SpaceWeatherProviderError>;
}

/// Stable provider error categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpaceWeatherProviderErrorKind {
    /// Provider is disabled by configuration.
    Disabled,
    /// Transport failed before a valid response was received.
    Transport,
    /// Provider returned a payload that could not be parsed or understood.
    Parse,
}

/// Errors surfaced by the space weather provider layer.
#[derive(Debug, Clone)]
pub struct SpaceWeatherProviderError {
    kind: SpaceWeatherProviderErrorKind,
    message: String,
}

impl SpaceWeatherProviderError {
    /// Provider is disabled.
    #[must_use]
    pub fn disabled(message: impl Into<String>) -> Self {
        Self::new(SpaceWeatherProviderErrorKind::Disabled, message)
    }

    /// Provider transport failed.
    #[must_use]
    pub fn transport(message: impl Into<String>) -> Self {
        Self::new(SpaceWeatherProviderErrorKind::Transport, message)
    }

    /// Provider returned an unexpected payload.
    #[must_use]
    pub fn parse(message: impl Into<String>) -> Self {
        Self::new(SpaceWeatherProviderErrorKind::Parse, message)
    }

    fn new(kind: SpaceWeatherProviderErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    /// Returns whether the error class is suitable for retry handling.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        matches!(self.kind, SpaceWeatherProviderErrorKind::Transport)
    }
}

impl std::fmt::Display for SpaceWeatherProviderError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "Space weather provider {} error: {}",
            match self.kind {
                SpaceWeatherProviderErrorKind::Disabled => "disabled",
                SpaceWeatherProviderErrorKind::Transport => "transport",
                SpaceWeatherProviderErrorKind::Parse => "parse",
            },
            self.message
        )
    }
}

impl std::error::Error for SpaceWeatherProviderError {}

/// Provider used when space weather fetching is disabled.
#[derive(Debug, Clone)]
pub struct DisabledSpaceWeatherProvider {
    reason: String,
}

impl DisabledSpaceWeatherProvider {
    /// Create a disabled provider with a stable reason message.
    #[must_use]
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }
}

#[tonic::async_trait]
impl SpaceWeatherProvider for DisabledSpaceWeatherProvider {
    async fn fetch_current(&self) -> Result<SpaceWeatherSnapshot, SpaceWeatherProviderError> {
        Err(SpaceWeatherProviderError::disabled(self.reason.clone()))
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn retryable_errors_are_transport_only() {
        assert!(SpaceWeatherProviderError::transport("offline").is_retryable());
        assert!(!SpaceWeatherProviderError::disabled("off").is_retryable());
        assert!(!SpaceWeatherProviderError::parse("bad").is_retryable());
    }

    #[tokio::test]
    async fn disabled_provider_returns_disabled_error() {
        let provider = DisabledSpaceWeatherProvider::new("disabled");

        let error = provider.fetch_current().await.expect_err("error");

        assert_eq!(
            "Space weather provider disabled error: disabled",
            error.to_string()
        );
    }
}

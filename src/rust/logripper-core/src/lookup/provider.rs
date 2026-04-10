//! Provider seam for callsign lookup data sources.

use crate::proto::logripper::domain::CallsignRecord;

/// Abstraction over an external callsign data provider.
#[tonic::async_trait]
pub trait CallsignProvider: Send + Sync {
    /// Look up a normalized callsign and return a provider-normalized result.
    async fn lookup_callsign(&self, callsign: &str) -> Result<ProviderLookup, ProviderLookupError>;
}

/// Provider lookup outcomes.
#[derive(Debug, Clone)]
pub enum ProviderLookup {
    /// The callsign exists and has a normalized record.
    Found(Box<CallsignRecord>),
    /// The provider confirms the callsign does not exist.
    NotFound,
}

/// Provider errors surfaced to lookup orchestration logic.
#[derive(Debug, Clone, thiserror::Error)]
pub enum ProviderLookupError {
    /// Provider is unavailable due to configuration.
    #[error("Provider configuration error: {0}")]
    Configuration(String),
    /// Provider authentication failed.
    #[error("Provider authentication error: {0}")]
    Authentication(String),
    /// Provider session state is invalid.
    #[error("Provider session error: {0}")]
    Session(String),
    /// Provider transport failed.
    #[error("Provider transport error: {0}")]
    Transport(String),
    /// Provider returned an unexpected payload.
    #[error("Provider parse error: {0}")]
    Parse(String),
    /// Provider indicated request throttling.
    #[error("Provider rate-limit error: {0}")]
    RateLimited(String),
}

impl ProviderLookupError {
    /// Returns whether the error class is suitable for retry handling.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::Transport(_) | Self::RateLimited(_))
    }
}

/// Provider used when configuration is missing or invalid.
#[derive(Debug, Clone)]
pub struct DisabledCallsignProvider {
    reason: String,
}

impl DisabledCallsignProvider {
    /// Create a disabled provider with a stable reason message.
    #[must_use]
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }
}

#[tonic::async_trait]
impl CallsignProvider for DisabledCallsignProvider {
    async fn lookup_callsign(
        &self,
        _callsign: &str,
    ) -> Result<ProviderLookup, ProviderLookupError> {
        Err(ProviderLookupError::Configuration(self.reason.clone()))
    }
}

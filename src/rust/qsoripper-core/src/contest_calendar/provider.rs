//! Provider seam for contest calendar data.

use crate::proto::qsoripper::domain::ContestCalendarEntry;

/// Abstraction over an external contest calendar provider.
#[tonic::async_trait]
pub trait ContestCalendarProvider: Send + Sync {
    /// Fetch fresh normalized contest calendar entries.
    async fn fetch_contests(
        &self,
    ) -> Result<Vec<ContestCalendarEntry>, ContestCalendarProviderError>;
}

/// Stable provider error categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContestCalendarProviderErrorKind {
    /// Provider is disabled by configuration.
    Disabled,
    /// Transport failed before a valid response was received.
    Transport,
    /// Provider returned a payload that could not be parsed or understood.
    Parse,
}

/// Errors surfaced by the contest calendar provider layer.
#[derive(Debug, Clone)]
pub struct ContestCalendarProviderError {
    kind: ContestCalendarProviderErrorKind,
    message: String,
}

impl ContestCalendarProviderError {
    /// Provider is disabled.
    #[must_use]
    pub fn disabled(message: impl Into<String>) -> Self {
        Self::new(ContestCalendarProviderErrorKind::Disabled, message)
    }

    /// Provider transport failed.
    #[must_use]
    pub fn transport(message: impl Into<String>) -> Self {
        Self::new(ContestCalendarProviderErrorKind::Transport, message)
    }

    /// Provider returned an unexpected payload.
    #[must_use]
    pub fn parse(message: impl Into<String>) -> Self {
        Self::new(ContestCalendarProviderErrorKind::Parse, message)
    }

    fn new(kind: ContestCalendarProviderErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ContestCalendarProviderError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "Contest calendar provider {} error: {}",
            match self.kind {
                ContestCalendarProviderErrorKind::Disabled => "disabled",
                ContestCalendarProviderErrorKind::Transport => "transport",
                ContestCalendarProviderErrorKind::Parse => "parse",
            },
            self.message
        )
    }
}

impl std::error::Error for ContestCalendarProviderError {}

/// Provider used when contest calendar fetching is disabled.
#[derive(Debug, Clone)]
pub struct DisabledContestCalendarProvider {
    reason: String,
}

impl DisabledContestCalendarProvider {
    /// Create a disabled provider with a stable reason message.
    #[must_use]
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }
}

#[tonic::async_trait]
impl ContestCalendarProvider for DisabledContestCalendarProvider {
    async fn fetch_contests(
        &self,
    ) -> Result<Vec<ContestCalendarEntry>, ContestCalendarProviderError> {
        Err(ContestCalendarProviderError::disabled(self.reason.clone()))
    }
}

//! Crate error types.
//!
//! [`BackendError`] is the failure surface of the radio link and backends (re-exported
//! from [`crate::backend`]). [`ConfigError`] covers configuration loading/validation, and
//! [`CatHubError`] is the daemon's top-level error returned from [`crate::run`].

use thiserror::Error;

/// A failure from the radio transport or a backend operation.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub(crate) enum BackendError {
    /// The transport failed (closed, write error, scheduler gone).
    #[error("transport: {0}")]
    Transport(String),
    /// A solicited command timed out waiting for its reply.
    #[error("timeout")]
    Timeout,
    /// The radio (or bridge) rejected the command.
    #[error("rejected: {0}")]
    Rejected(String),
    /// The operation is not supported by this backend.
    #[error("unsupported")]
    Unsupported,
}

/// A configuration load or validation failure.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// The config file could not be read.
    #[error("reading config: {0}")]
    Io(#[from] std::io::Error),
    /// The config file is not valid TOML or has the wrong shape.
    #[error("parsing config: {0}")]
    Parse(#[from] toml::de::Error),
    /// The config parsed but is semantically invalid.
    #[error("invalid config: {0}")]
    Invalid(String),
}

/// The daemon's top-level error.
#[derive(Debug, Error)]
pub enum CatHubError {
    /// A configuration problem.
    #[error(transparent)]
    Config(#[from] ConfigError),
    /// An I/O problem binding an endpoint, opening a port, or similar.
    #[error("i/o: {0}")]
    Io(#[from] std::io::Error),
    /// The configured backend could not be built or initialized.
    #[error("backend: {0}")]
    Backend(String),
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn backend_error_displays() {
        assert_eq!(BackendError::Timeout.to_string(), "timeout");
        assert_eq!(BackendError::Unsupported.to_string(), "unsupported");
        assert_eq!(
            BackendError::Rejected("rigctld RPRT -1".into()).to_string(),
            "rejected: rigctld RPRT -1"
        );
        assert_eq!(
            BackendError::Transport("closed".into()).to_string(),
            "transport: closed"
        );
    }

    #[test]
    fn config_error_wraps_invalid() {
        let err = ConfigError::Invalid("no endpoints".into());
        assert_eq!(err.to_string(), "invalid config: no endpoints");
    }

    #[test]
    fn cathub_error_from_config() {
        let err: CatHubError = ConfigError::Invalid("bad".into()).into();
        assert!(matches!(err, CatHubError::Config(_)));
        assert!(err.to_string().contains("invalid config: bad"));
    }

    #[test]
    fn cathub_error_backend_displays() {
        assert_eq!(
            CatHubError::Backend("unknown".into()).to_string(),
            "backend: unknown"
        );
    }
}

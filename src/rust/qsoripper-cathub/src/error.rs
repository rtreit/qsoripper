//! Typed error surfaces for the CAT hub.

use std::io;

use thiserror::Error;

/// Errors raised while loading or validating configuration.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// The configuration file could not be read.
    #[error("failed to read config file '{path}': {source}")]
    Read {
        /// Path that failed to load.
        path: String,
        /// Underlying I/O error.
        source: io::Error,
    },
    /// The configuration file was not valid TOML or failed schema parsing.
    #[error("failed to parse config: {0}")]
    Parse(String),
    /// The configuration parsed but was semantically invalid.
    #[error("invalid configuration: {0}")]
    Invalid(String),
}

/// Errors raised by a radio backend.
#[derive(Debug, Error)]
pub enum BackendError {
    /// The radio transport failed (I/O, disconnect).
    #[error("radio transport error: {0}")]
    Transport(String),
    /// A command timed out waiting for a reply.
    #[error("radio command timed out: {0}")]
    Timeout(String),
    /// A reply could not be parsed.
    #[error("failed to parse radio reply: {0}")]
    Parse(String),
    /// The backend does not support the requested operation.
    #[error("operation not supported by backend: {0}")]
    Unsupported(String),
}

/// Errors raised while running a client face.
#[derive(Debug, Error)]
pub enum FaceError {
    /// The face's serial or network endpoint could not be bound.
    #[error("failed to bind face '{name}' to '{endpoint}': {message}")]
    Bind {
        /// Face name from configuration.
        name: String,
        /// Endpoint that failed to bind.
        endpoint: String,
        /// Description of the bind failure.
        message: String,
    },
    /// An I/O error occurred while serving the face.
    #[error("face '{name}' I/O error: {source}")]
    Io {
        /// Face name from configuration.
        name: String,
        /// Underlying I/O error.
        source: io::Error,
    },
}

/// Top-level error type for the daemon.
#[derive(Debug, Error)]
pub enum CatHubError {
    /// Configuration failed to load or validate.
    #[error(transparent)]
    Config(#[from] ConfigError),
    /// A backend failed fatally during startup.
    #[error(transparent)]
    Backend(#[from] BackendError),
    /// A face failed fatally during startup.
    #[error(transparent)]
    Face(#[from] FaceError),
    /// A generic I/O failure during startup or shutdown.
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
}

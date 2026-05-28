//! `qsoripper-cathub`: a multi-client CAT hub that owns the radio's serial port
//! and fans it out to multiple logging, panadapter, and engine clients.
//!
//! This crate is built as a library plus a thin binary so integration tests and
//! the engine can drive the hub in-process. The public surface is intentionally
//! minimal; the daemon internals are crate-private.

#![forbid(unsafe_code)]

mod backend;
mod config;
mod dialect;
mod error;
mod model;
mod poller;
mod radio;
mod serial_face;
mod state;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;

use crate::backend::kenwood::Ts590Backend;
use crate::backend::loopback::LoopbackBackend;
use crate::backend::{RadioBackend, StateMutation};
use crate::config::{BackendKind, Config, DialectKind};
use crate::dialect::kenwood::Ts590Dialect;
use crate::dialect::{ClientDialect, FaceContext};
use crate::error::{BackendError, ConfigError};
use crate::poller::run_poller;
use crate::radio::run_radio_task;
use crate::state::{run_mutation_dispatcher, StateHandle};

pub use error::CatHubError;

/// Command-line arguments for the CAT hub daemon.
#[derive(Debug, Parser)]
#[command(
    name = "qsoripper-cathub",
    about = "Multi-client CAT hub for shared radio control"
)]
pub struct Cli {
    /// Path to the TOML configuration file. Defaults to the per-user config path.
    #[arg(long)]
    pub config: Option<PathBuf>,
    /// Path to a log file. When omitted, logs go to stderr.
    #[arg(long)]
    pub log: Option<PathBuf>,
    /// Load and validate the configuration, print it, and exit.
    #[arg(long)]
    pub dry_run: bool,
}

/// Run the daemon to completion using the given command-line arguments.
///
/// # Errors
///
/// Returns a [`CatHubError`] if configuration fails to load or validate, if the
/// radio transport cannot be opened, or if a face fails to bind.
pub async fn run(cli: Cli) -> Result<(), CatHubError> {
    let config_path = cli
        .config
        .or_else(config::default_config_path)
        .ok_or_else(|| {
            ConfigError::Invalid("no configuration path resolved; pass --config".to_string())
        })?;
    let config = Config::load(&config_path)?;
    config.validate()?;

    if cli.dry_run {
        print!("{}", config.describe());
        return Ok(());
    }

    run_daemon(config).await
}

async fn run_daemon(config: Config) -> Result<(), CatHubError> {
    let (state, inbox) = StateHandle::new(256);
    let reply_timeout = Duration::from_millis(config.radio.reply_timeout_ms);

    let backend: Arc<dyn RadioBackend> = match config.radio.backend {
        BackendKind::Ts590 => {
            let (radio, inbox) = radio::channel(64);
            let port = serial2_tokio::SerialPort::open(&config.radio.port, config.radio.baud)
                .map_err(|e| {
                    BackendError::Transport(format!(
                        "opening radio port {}: {e}",
                        config.radio.port
                    ))
                })?;
            tokio::spawn(run_radio_task(port, inbox, reply_timeout));
            // Disable the radio's auto-information so unsolicited frames can't
            // desynchronize the polled request/reply stream.
            if let Err(error) = radio.send(b"AI0;".to_vec(), false).await {
                tracing::warn!(%error, "failed to disable radio auto-information");
            }
            Arc::new(Ts590Backend::new(radio))
        }
        BackendKind::Loopback => Arc::new(LoopbackBackend::new()),
    };

    tokio::spawn(run_mutation_dispatcher(
        inbox,
        backend.clone(),
        state.clone(),
    ));
    tokio::spawn(run_poller(
        backend.clone(),
        state.clone(),
        Duration::from_millis(config.poll.baseline_ms),
    ));

    // Prime the cache with one poll so early client reads see real radio state
    // instead of defaults. Best-effort: the baseline poller retries on failure.
    if let Err(error) = backend.poll(&state).await {
        tracing::warn!(%error, "initial poll failed; serving defaults until next poll");
    }

    for face in &config.faces {
        let ctx = FaceContext::new(
            face.name.clone(),
            face.permissions(),
            state.clone(),
            backend.clone(),
        );
        let dialect: Arc<dyn ClientDialect> = match face.dialect {
            DialectKind::Ts590 => Arc::new(Ts590Dialect::new()),
        };
        let port = serial_face::open_serial(&face.name, &face.port, face.baud)?;
        let name = face.name.clone();
        let changes = state.subscribe();
        tokio::spawn(async move {
            if let Err(error) = serial_face::run_face(port, dialect, ctx, changes).await {
                tracing::error!(%error, face = %name, "face stopped");
            }
        });
    }

    tracing::info!("cathub running; press Ctrl+C to stop");
    tokio::signal::ctrl_c().await?;
    tracing::info!("shutdown requested; releasing PTT");
    let release = state.apply_mutation(StateMutation::Ptt { keyed: false });
    match tokio::time::timeout(Duration::from_millis(500), release).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => tracing::warn!(%error, "failed to release PTT on shutdown"),
        Err(_) => tracing::warn!("timed out releasing PTT on shutdown"),
    }
    Ok(())
}

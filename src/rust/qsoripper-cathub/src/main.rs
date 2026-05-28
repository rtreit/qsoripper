//! Thin binary entry point for the CAT hub daemon: initialize logging, parse
//! arguments, and run the daemon until shutdown.

use clap::Parser as _;
use qsoripper_cathub::{run, Cli};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> std::process::ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    match run(cli).await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!(%error, "cathub failed");
            std::process::ExitCode::FAILURE
        }
    }
}

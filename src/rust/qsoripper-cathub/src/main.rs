//! Binary entry point for the qsoripper-cathub daemon.

use std::process::ExitCode;

use clap::Parser;

use qsoripper_cathub::{run, Cli};

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let _guard = qsoripper_cathub::init_logging();
    match run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            tracing::error!(error = %err, "cathub exited with error");
            eprintln!("cathub: {err}");
            ExitCode::FAILURE
        }
    }
}

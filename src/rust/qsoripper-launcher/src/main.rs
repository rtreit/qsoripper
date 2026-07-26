//! Binary entry point for the `QsoRipper` launcher TUI.

mod catalog;
mod cathub_runtime;
mod config;
mod discovery;
mod model;
mod plan;
mod ports;
mod process;
mod sync;
mod ui;

use std::path::PathBuf;

use anyhow::Result;

use crate::discovery::{detect_repo_root, ArtifactRoot, Configuration};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut configuration = Configuration::Release;
    let mut repo_root: Option<PathBuf> = None;
    let mut config_path: Option<PathBuf> = None;

    let mut it = args.into_iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--debug" => configuration = Configuration::Debug,
            "--release" => configuration = Configuration::Release,
            "--repo-root" => {
                repo_root = it.next().map(PathBuf::from);
            }
            "--config" => {
                config_path = it.next().map(PathBuf::from);
            }
            "-h" | "--help" => {
                print_help();
                return Ok(());
            }
            other => {
                eprintln!("qsoripper-launcher: unknown argument '{other}'\n");
                print_help();
                std::process::exit(2);
            }
        }
    }

    let repo_root = match repo_root {
        Some(r) => r,
        None => detect_repo_root()?,
    };
    let artifact_root = ArtifactRoot::from_repo_root(&repo_root, configuration);
    let config_path = match config_path {
        Some(p) => p,
        None => config::default_shared_config_path()?,
    };
    let selection = config::load(&config_path)?;
    let mut app = ui::AppState::new(selection, config_path, artifact_root);
    ui::run(&mut app)
}

fn print_help() {
    println!(
        "qsoripper-launcher [options]\n\
         \n\
         Options:\n\
           --release          Use artifacts/publish/*/Release (default)\n\
           --debug            Use artifacts/publish/*/Debug\n\
           --repo-root <DIR>  Override repo root detection\n\
           --config <FILE>    Override shared config.toml path\n\
           -h, --help         Show this help\n"
    );
}

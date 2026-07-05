//! Binary entry point for the `WinKeyer` EEPROM TUI editor.

mod model;
mod ui;

use std::path::PathBuf;

use anyhow::{bail, Result};

use crate::model::WinKeyerImage;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "-h" || arg == "--help") {
        print_help();
        return Ok(());
    }

    let mut path: Option<PathBuf> = None;
    let mut it = args.into_iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--file" => path = it.next().map(PathBuf::from),
            other if other.starts_with('-') => bail!("unknown argument '{other}'"),
            other => {
                if path.is_some() {
                    bail!("unexpected extra path '{other}'");
                }
                path = Some(PathBuf::from(other));
            }
        }
    }

    let Some(path) = path else {
        print_help();
        bail!("missing WinKeyer .eep file path");
    };

    let image = WinKeyerImage::load(&path)?;
    let mut app = ui::AppState::new(path, image);
    ui::run(&mut app)
}

fn print_help() {
    println!(
        "qsoripper-winkeyer-editor <FILE.eep>\n\
         \n\
         Keyboard-first terminal editor for 300-byte WK3tools WinKeyer .eep files.\n\
         \n\
         Options:\n\
           --file <FILE>  File to open\n\
           -h, --help     Show this help\n"
    );
}

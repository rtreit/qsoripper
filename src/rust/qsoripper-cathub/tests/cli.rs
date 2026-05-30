//! Public-surface integration tests for the cathub binary's library entry points.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing)]

use std::path::PathBuf;

use qsoripper_cathub::{run, Cli};

fn temp_config(contents: &str, tag: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("cathub-cli-{}-{}.toml", std::process::id(), tag));
    std::fs::write(&path, contents).expect("write temp config");
    path
}

#[tokio::test]
async fn dry_run_accepts_a_valid_loopback_config() {
    let path = temp_config(
        "[radio]\nbackend = \"loopback\"\n\
         [[face]]\nname = \"n1mm\"\ntransport = \"COM11\"\ndialect = \"ts590\"\nperms = [\"read\", \"write\"]\n\
         [[hamlib_net]]\nname = \"engine\"\nbind = \"127.0.0.1:4532\"\nperms = [\"read\"]\n",
        "valid",
    );
    let cli = Cli {
        config: Some(path.clone()),
        log: None,
        dry_run: true,
    };
    assert!(run(cli).await.is_ok());
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn dry_run_rejects_an_invalid_backend() {
    let path = temp_config(
        "[radio]\nbackend = \"icom\"\n\
         [[face]]\nname = \"x\"\ntransport = \"COM5\"\ndialect = \"ts590\"\n",
        "badbackend",
    );
    let cli = Cli {
        config: Some(path.clone()),
        log: None,
        dry_run: true,
    };
    assert!(run(cli).await.is_err());
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn missing_config_is_an_error() {
    let cli = Cli {
        config: Some(PathBuf::from("definitely-missing-cathub-config.toml")),
        log: None,
        dry_run: true,
    };
    assert!(run(cli).await.is_err());
}

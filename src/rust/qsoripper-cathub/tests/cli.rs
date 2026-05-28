//! Integration coverage for the public CLI entry point: configuration
//! resolution, the dry-run path, and load/parse error surfaces.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::PathBuf;

use qsoripper_cathub::{run, Cli};

fn temp_config(contents: &str) -> PathBuf {
    let unique = format!(
        "cathub-test-{}-{}.toml",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time after epoch")
            .as_nanos()
    );
    let path = std::env::temp_dir().join(unique);
    std::fs::write(&path, contents).expect("write temp config");
    path
}

#[tokio::test]
async fn dry_run_validates_and_returns_ok() {
    let path = temp_config(
        r#"
[radio]
port = "COM3"
backend = "loopback"

[[face]]
name = "n1mm"
port = "COM11"
dialect = "ts590"
"#,
    );
    let cli = Cli {
        config: Some(path.clone()),
        log: None,
        dry_run: true,
    };
    let result = run(cli).await;
    std::fs::remove_file(&path).ok();
    assert!(result.is_ok(), "dry-run should succeed: {result:?}");
}

#[tokio::test]
async fn missing_config_file_is_an_error() {
    let path = std::env::temp_dir().join("cathub-missing-config-should-not-exist.toml");
    std::fs::remove_file(&path).ok();
    let cli = Cli {
        config: Some(path),
        log: None,
        dry_run: true,
    };
    assert!(run(cli).await.is_err());
}

#[tokio::test]
async fn invalid_config_is_an_error() {
    let path = temp_config("this is not valid [[ toml");
    let cli = Cli {
        config: Some(path.clone()),
        log: None,
        dry_run: true,
    };
    let result = run(cli).await;
    std::fs::remove_file(&path).ok();
    assert!(result.is_err());
}

#[tokio::test]
async fn invalid_semantics_are_rejected() {
    let path = temp_config(
        r#"
[radio]
port = "  "
backend = "loopback"
"#,
    );
    let cli = Cli {
        config: Some(path.clone()),
        log: None,
        dry_run: true,
    };
    let result = run(cli).await;
    std::fs::remove_file(&path).ok();
    assert!(result.is_err());
}

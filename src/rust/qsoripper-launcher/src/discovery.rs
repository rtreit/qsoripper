//! Resolve published artifact roots like `artifacts/publish/<sub>/<Release|Debug>/`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Build configuration the launcher targets when resolving artifacts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Configuration {
    Release,
    Debug,
}

impl Configuration {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Release => "Release",
            Self::Debug => "Debug",
        }
    }
}

/// The published-artifact root and the configuration name to use beneath it.
#[derive(Debug, Clone)]
pub(crate) struct ArtifactRoot {
    publish_root: PathBuf,
    configuration: &'static str,
    repo_root: Option<PathBuf>,
}

impl ArtifactRoot {
    /// Build an artifact root from an explicit publish dir and configuration.
    pub(crate) fn new(publish_root: PathBuf, configuration: Configuration) -> Self {
        Self {
            publish_root,
            configuration: configuration.as_str(),
            repo_root: None,
        }
    }

    /// Resolve `artifacts/publish/` relative to a repo root.
    pub(crate) fn from_repo_root(repo_root: &Path, configuration: Configuration) -> Self {
        let mut root = Self::new(repo_root.join("artifacts").join("publish"), configuration);
        root.repo_root = Some(repo_root.to_path_buf());
        root
    }

    pub(crate) fn path(&self) -> &Path {
        &self.publish_root
    }

    pub(crate) fn configuration(&self) -> &str {
        self.configuration
    }

    pub(crate) fn repo_root(&self) -> Option<&Path> {
        self.repo_root.as_deref()
    }
}

/// Walk up from the launcher binary's location looking for a directory that
/// contains both `artifacts` and `src`. This lets `qsoripper-launcher.exe`
/// work whether it's invoked from the repo root or from
/// `artifacts/publish/qsoripper-launcher/<cfg>/`.
pub(crate) fn detect_repo_root() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("locating current executable")?;
    let mut cursor = exe.parent().map(Path::to_path_buf);
    while let Some(dir) = cursor {
        if dir.join("artifacts").is_dir() && dir.join("src").is_dir() {
            return Ok(dir);
        }
        cursor = dir.parent().map(Path::to_path_buf);
    }
    // Fall back to the current working directory; the caller will surface
    // a clearer error when an artifact lookup fails.
    std::env::current_dir().context("resolving fallback repo root via cwd")
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::similar_names,
    clippy::too_many_lines,
    clippy::needless_pass_by_value
)]
mod tests {
    use super::*;

    use crate::catalog::{catalog, ComponentKind};

    #[test]
    fn artifact_path_uses_configuration_subdirectory() {
        let root = ArtifactRoot::new(
            PathBuf::from("/repo/artifacts/publish"),
            Configuration::Release,
        );
        let spec = catalog()
            .into_iter()
            .find(|c| c.id == crate::catalog::ENGINE_RUST)
            .expect("rust engine in catalog");
        let path = spec.artifact.executable_path(&root);
        let expected_stem = if cfg!(windows) {
            "qsoripper-server.exe"
        } else {
            "qsoripper-server"
        };
        assert!(path.ends_with(format!("qsoripper-server/Release/{expected_stem}")));
    }

    #[test]
    fn debug_configuration_string_is_debug() {
        assert_eq!(Configuration::Debug.as_str(), "Debug");
    }

    #[test]
    fn every_engine_has_a_port() {
        for spec in catalog() {
            if spec.kind == ComponentKind::Engine {
                assert!(
                    spec.engine_port.is_some(),
                    "engine {} missing port",
                    spec.id
                );
            }
        }
    }
}

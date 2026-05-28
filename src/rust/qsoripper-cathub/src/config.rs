//! Configuration schema and loading. The hub is configured from a TOML file
//! describing the radio, the baseline poll cadence, and one or more client
//! faces. Phase 1 supports serial faces; the Hamlib net section lands later.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::dialect::Permissions;
use crate::error::ConfigError;

/// Top-level daemon configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Config {
    /// Radio transport settings.
    pub(crate) radio: RadioConfig,
    /// Baseline polling settings.
    #[serde(default)]
    pub(crate) poll: PollConfig,
    /// Client faces.
    #[serde(default, rename = "face")]
    pub(crate) faces: Vec<FaceConfig>,
}

/// Which radio backend family to drive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum BackendKind {
    /// Kenwood TS-590.
    Ts590,
    /// In-memory loopback backend (testing and `--dry-run`).
    Loopback,
}

/// Which client dialect a face speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum DialectKind {
    /// Native Kenwood TS-590.
    Ts590,
}

/// Radio transport configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RadioConfig {
    /// Serial port path (for example `COM3` or `/dev/ttyUSB0`).
    pub(crate) port: String,
    /// Serial baud rate.
    #[serde(default = "default_baud")]
    pub(crate) baud: u32,
    /// Backend family.
    pub(crate) backend: BackendKind,
    /// Per-command reply timeout in milliseconds.
    #[serde(default = "default_reply_timeout_ms")]
    pub(crate) reply_timeout_ms: u64,
}

/// Baseline polling configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PollConfig {
    /// Baseline poll interval in milliseconds.
    #[serde(default = "default_baseline_ms")]
    pub(crate) baseline_ms: u64,
}

impl Default for PollConfig {
    fn default() -> Self {
        Self {
            baseline_ms: default_baseline_ms(),
        }
    }
}

/// One client face.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FaceConfig {
    /// Face name (used in logs and as PTT owner identity).
    pub(crate) name: String,
    /// Serial port path the client connects to.
    pub(crate) port: String,
    /// Serial baud rate.
    #[serde(default = "default_baud")]
    pub(crate) baud: u32,
    /// Dialect this face speaks.
    pub(crate) dialect: DialectKind,
    /// Whether the face may change frequency/mode/split.
    #[serde(default = "default_true")]
    pub(crate) allow_write: bool,
    /// Whether the face may key PTT.
    #[serde(default)]
    pub(crate) allow_ptt: bool,
    /// Whether the face may send raw passthrough commands.
    #[serde(default = "default_true")]
    pub(crate) allow_passthrough: bool,
}

impl FaceConfig {
    /// Permissions derived from this face configuration.
    pub(crate) fn permissions(&self) -> Permissions {
        Permissions {
            allow_write: self.allow_write,
            allow_ptt: self.allow_ptt,
            allow_passthrough: self.allow_passthrough,
        }
    }
}

impl Config {
    /// Load and parse configuration from a TOML file.
    pub(crate) fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.display().to_string(),
            source,
        })?;
        Self::parse(&text)
    }

    /// Parse configuration from a TOML string.
    pub(crate) fn parse(text: &str) -> Result<Self, ConfigError> {
        toml::from_str(text).map_err(|e| ConfigError::Parse(e.to_string()))
    }

    /// Validate semantic constraints not expressible in the schema.
    pub(crate) fn validate(&self) -> Result<(), ConfigError> {
        if self.radio.port.trim().is_empty() {
            return Err(ConfigError::Invalid(
                "radio.port must not be empty".to_string(),
            ));
        }
        if self.radio.reply_timeout_ms == 0 {
            return Err(ConfigError::Invalid(
                "radio.reply_timeout_ms must be greater than zero".to_string(),
            ));
        }
        if self.poll.baseline_ms == 0 {
            return Err(ConfigError::Invalid(
                "poll.baseline_ms must be greater than zero".to_string(),
            ));
        }
        for face in &self.faces {
            if face.name.trim().is_empty() {
                return Err(ConfigError::Invalid(
                    "face.name must not be empty".to_string(),
                ));
            }
            if face.port.trim().is_empty() {
                return Err(ConfigError::Invalid(format!(
                    "face '{}' port must not be empty",
                    face.name
                )));
            }
        }
        Ok(())
    }

    /// Render a human-readable summary of the resolved configuration.
    pub(crate) fn describe(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(
            out,
            "radio: port={} baud={} backend={:?} reply_timeout_ms={}",
            self.radio.port, self.radio.baud, self.radio.backend, self.radio.reply_timeout_ms
        );
        let _ = writeln!(out, "poll: baseline_ms={}", self.poll.baseline_ms);
        if self.faces.is_empty() {
            let _ = writeln!(out, "faces: (none)");
        }
        for face in &self.faces {
            let _ = writeln!(
                out,
                "face: name={} port={} baud={} dialect={:?} write={} ptt={} passthrough={}",
                face.name,
                face.port,
                face.baud,
                face.dialect,
                face.allow_write,
                face.allow_ptt,
                face.allow_passthrough
            );
        }
        out
    }
}

/// Resolve the default configuration file path from the environment.
pub(crate) fn default_config_path() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("APPDATA") {
        return Some(Path::new(&dir).join("qsoripper").join("cathub.toml"));
    }
    if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME") {
        return Some(Path::new(&dir).join("qsoripper").join("cathub.toml"));
    }
    if let Some(dir) = std::env::var_os("HOME") {
        return Some(
            Path::new(&dir)
                .join(".config")
                .join("qsoripper")
                .join("cathub.toml"),
        );
    }
    None
}

fn default_baud() -> u32 {
    115_200
}

fn default_reply_timeout_ms() -> u64 {
    250
}

fn default_baseline_ms() -> u64 {
    500
}

fn default_true() -> bool {
    true
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
[radio]
port = "COM3"
baud = 115200
backend = "ts590"
reply_timeout_ms = 200

[poll]
baseline_ms = 400

[[face]]
name = "n1mm"
port = "COM11"
dialect = "ts590"
allow_ptt = true
"#;

    #[test]
    fn parses_full_config() {
        let config = Config::parse(SAMPLE).expect("parse");
        assert_eq!(config.radio.port, "COM3");
        assert_eq!(config.radio.backend, BackendKind::Ts590);
        assert_eq!(config.radio.reply_timeout_ms, 200);
        assert_eq!(config.poll.baseline_ms, 400);
        assert_eq!(config.faces.len(), 1);
        let face = &config.faces[0];
        assert_eq!(face.name, "n1mm");
        assert_eq!(face.baud, 115_200);
        assert!(face.allow_ptt);
        assert!(face.allow_write);
        assert!(face.allow_passthrough);
        config.validate().expect("valid");
    }

    #[test]
    fn defaults_apply_when_omitted() {
        let config = Config::parse(
            r#"
[radio]
port = "/dev/ttyUSB0"
backend = "loopback"
"#,
        )
        .expect("parse");
        assert_eq!(config.radio.baud, 115_200);
        assert_eq!(config.radio.reply_timeout_ms, 250);
        assert_eq!(config.poll.baseline_ms, 500);
        assert!(config.faces.is_empty());
    }

    #[test]
    fn unknown_field_is_rejected() {
        let err = Config::parse(
            r#"
[radio]
port = "COM3"
backend = "ts590"
bogus = true
"#,
        )
        .expect_err("should reject unknown field");
        assert!(matches!(err, ConfigError::Parse(_)));
    }

    #[test]
    fn empty_port_is_invalid() {
        let config = Config::parse(
            r#"
[radio]
port = "  "
backend = "ts590"
"#,
        )
        .expect("parse");
        assert!(matches!(config.validate(), Err(ConfigError::Invalid(_))));
    }

    #[test]
    fn zero_timeout_is_invalid() {
        let config = Config::parse(
            r#"
[radio]
port = "COM3"
backend = "ts590"
reply_timeout_ms = 0
"#,
        )
        .expect("parse");
        assert!(matches!(config.validate(), Err(ConfigError::Invalid(_))));
    }

    #[test]
    fn permissions_round_trip_from_face() {
        let config = Config::parse(SAMPLE).expect("parse");
        let perms = config.faces[0].permissions();
        assert!(perms.allow_ptt);
        assert!(perms.allow_write);
        assert!(perms.allow_passthrough);
    }

    #[test]
    fn describe_lists_radio_and_faces() {
        let config = Config::parse(SAMPLE).expect("parse");
        let described = config.describe();
        assert!(described.contains("port=COM3"));
        assert!(described.contains("name=n1mm"));
    }

    #[test]
    fn describe_handles_no_faces() {
        let config = Config::parse(
            r#"
[radio]
port = "COM3"
backend = "ts590"
"#,
        )
        .expect("parse");
        assert!(config.describe().contains("faces: (none)"));
    }
}

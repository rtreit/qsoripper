//! Daemon configuration (TOML).
//!
//! One `[radio]` section selects and parameterizes the backend; `[poll]`, `[ptt]`, and
//! `[events]` tune cadence and safety; `[[face]]` and `[[hamlib_net]]` declare the client
//! endpoints. Everything but `[radio].backend` has a sane default so a minimal config is
//! short.

use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;

use crate::error::ConfigError;
use crate::permissions::FacePermissions;

fn default_transport() -> String {
    "serial".to_string()
}
fn default_baud() -> u32 {
    4_800
}
fn default_host() -> String {
    "127.0.0.1".to_string()
}
fn default_tcp_port() -> u16 {
    4_532
}
fn default_reply_timeout_ms() -> u64 {
    1_000
}
fn default_baseline_ms() -> u64 {
    250
}
fn default_heartbeat_ms() -> u64 {
    3_000
}
fn default_max_tx_ms() -> u64 {
    300_000
}
fn default_native_push() -> bool {
    true
}

/// The `[radio]` section.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct RadioConfig {
    /// Backend selector: `ts590`, `rigctld`, or `loopback`.
    pub(crate) backend: String,
    /// Human-readable / Hamlib model id (e.g. `TS-590SG`, `2014`).
    #[serde(default)]
    pub(crate) model: String,
    /// `serial` or `tcp`.
    #[serde(default = "default_transport")]
    pub(crate) transport: String,
    /// Serial port path (e.g. `COM3`, `/dev/ttyUSB0`).
    #[serde(default)]
    pub(crate) port: String,
    /// Serial baud rate.
    #[serde(default = "default_baud")]
    pub(crate) baud: u32,
    /// TCP host (for `tcp` transport or the rigctld bridge).
    #[serde(default = "default_host")]
    pub(crate) host: String,
    /// TCP port.
    #[serde(default = "default_tcp_port")]
    pub(crate) tcp_port: u16,
    /// Whether a bridge backend has been operator-certified.
    #[serde(default)]
    pub(crate) certified: bool,
    /// Per-command reply timeout in milliseconds.
    #[serde(default = "default_reply_timeout_ms")]
    pub(crate) reply_timeout_ms: u64,
}

/// The `[poll]` section.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct PollConfig {
    /// Baseline poll interval in milliseconds.
    #[serde(default = "default_baseline_ms")]
    pub(crate) baseline_ms: u64,
    /// Heartbeat (backed-off) interval in milliseconds.
    #[serde(default = "default_heartbeat_ms")]
    pub(crate) heartbeat_ms: u64,
}

impl Default for PollConfig {
    fn default() -> Self {
        PollConfig {
            baseline_ms: default_baseline_ms(),
            heartbeat_ms: default_heartbeat_ms(),
        }
    }
}

/// The `[ptt]` section.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct PttConfig {
    /// Maximum continuous transmit time in milliseconds (safety ceiling).
    #[serde(default = "default_max_tx_ms")]
    pub(crate) max_tx_ms: u64,
}

impl Default for PttConfig {
    fn default() -> Self {
        PttConfig {
            max_tx_ms: default_max_tx_ms(),
        }
    }
}

/// The `[events]` section.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct EventsConfig {
    /// Whether to enable the radio's native push stream.
    #[serde(default = "default_native_push")]
    pub(crate) native_push: bool,
}

impl Default for EventsConfig {
    fn default() -> Self {
        EventsConfig {
            native_push: default_native_push(),
        }
    }
}

/// A `[[face]]` (serial client) endpoint.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct FaceConfig {
    /// A label for logging.
    pub(crate) name: String,
    /// The serial port this face listens on (a com0com / tty path).
    pub(crate) transport: String,
    /// Baud rate for the face port.
    #[serde(default = "default_baud")]
    pub(crate) baud: u32,
    /// Dialect: `ts590` or `ts2000`.
    pub(crate) dialect: String,
    /// Permission tokens (`read`, `write`, `ptt`, `config_write`).
    #[serde(default)]
    pub(crate) perms: Vec<String>,
}

impl FaceConfig {
    /// The parsed permission set.
    pub(crate) fn permissions(&self) -> FacePermissions {
        FacePermissions::from_tokens(&self.perms)
    }
}

/// A `[[hamlib_net]]` (rigctld-compatible TCP) endpoint.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct HamlibNetConfig {
    /// A label for logging.
    pub(crate) name: String,
    /// The bind address (e.g. `127.0.0.1:4532`).
    pub(crate) bind: String,
    /// Permission tokens (`read`, `write`, `ptt`, `config_write`).
    #[serde(default)]
    pub(crate) perms: Vec<String>,
}

impl HamlibNetConfig {
    /// The parsed permission set.
    pub(crate) fn permissions(&self) -> FacePermissions {
        FacePermissions::from_tokens(&self.perms)
    }
}

/// The full daemon configuration.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Config {
    /// The radio backend section.
    pub(crate) radio: RadioConfig,
    /// Poll cadence.
    #[serde(default)]
    pub(crate) poll: PollConfig,
    /// PTT safety.
    #[serde(default)]
    pub(crate) ptt: PttConfig,
    /// Event/native-push policy.
    #[serde(default)]
    pub(crate) events: EventsConfig,
    /// Serial client endpoints.
    #[serde(default)]
    pub(crate) face: Vec<FaceConfig>,
    /// Hamlib net endpoints.
    #[serde(default)]
    pub(crate) hamlib_net: Vec<HamlibNetConfig>,
}

/// The table key the daemon's settings live under inside the shared unified `config.toml`.
const UNIFIED_SECTION: &str = "cat_hub";
/// Environment override for the shared config path (shared with the engine and launcher).
const CONFIG_PATH_ENV: &str = "QSORIPPER_CONFIG_PATH";
/// Per-user config directory name shared across QsoRipper components.
const SHARED_DIR: &str = "qsoripper";
/// Shared unified config file name.
const SHARED_FILE: &str = "config.toml";

impl Config {
    /// Parse a configuration from a bare TOML string (top-level `[radio]` … layout).
    pub(crate) fn parse(text: &str) -> Result<Config, ConfigError> {
        let config: Config = toml::from_str(text)?;
        config.validate()?;
        Ok(config)
    }

    /// Parse a configuration from a TOML document that may be either the unified
    /// `config.toml` (daemon settings nested under `[cat_hub]`, alongside the engine's and
    /// launcher's own sections) or a standalone cathub config (top-level `[radio]` …).
    ///
    /// Detection is by presence of a top-level `cat_hub` table: when present, only that
    /// subtree is used and every other section is ignored; otherwise the whole document is
    /// parsed as a standalone config for backward compatibility.
    pub(crate) fn parse_document(text: &str) -> Result<Config, ConfigError> {
        let document: toml::Value = toml::from_str(text)?;
        if let Some(section) = document.get(UNIFIED_SECTION) {
            let config: Config = section.clone().try_into()?;
            config.validate()?;
            Ok(config)
        } else {
            Config::parse(text)
        }
    }

    /// Load and parse a configuration from a file, accepting either the unified or the
    /// standalone layout (see [`Config::parse_document`]).
    pub(crate) fn load(path: &std::path::Path) -> Result<Config, ConfigError> {
        let text = std::fs::read_to_string(path)?;
        Config::parse_document(&text)
    }

    /// Validate semantic constraints not captured by the type system.
    pub(crate) fn validate(&self) -> Result<(), ConfigError> {
        match self.radio.backend.as_str() {
            "ts590" | "rigctld" | "loopback" => {}
            other => {
                return Err(ConfigError::Invalid(format!(
                    "unknown radio.backend '{other}' (expected ts590, rigctld, or loopback)"
                )))
            }
        }
        if self.radio.backend != "loopback" {
            match self.radio.transport.as_str() {
                "serial" => {
                    if self.radio.port.is_empty() {
                        return Err(ConfigError::Invalid(
                            "radio.transport = \"serial\" requires radio.port".to_string(),
                        ));
                    }
                }
                "tcp" => {}
                other => {
                    return Err(ConfigError::Invalid(format!(
                        "unknown radio.transport '{other}' (expected serial or tcp)"
                    )))
                }
            }
        }
        for face in &self.face {
            if !matches!(face.dialect.as_str(), "ts590" | "ts2000") {
                return Err(ConfigError::Invalid(format!(
                    "face '{}' has unknown dialect '{}' (expected ts590 or ts2000)",
                    face.name, face.dialect
                )));
            }
        }
        if self.face.is_empty() && self.hamlib_net.is_empty() {
            return Err(ConfigError::Invalid(
                "at least one [[face]] or [[hamlib_net]] endpoint is required".to_string(),
            ));
        }
        Ok(())
    }

    /// The PTT maximum-transmit safety ceiling.
    pub(crate) fn ptt_max_tx(&self) -> Duration {
        Duration::from_millis(self.ptt.max_tx_ms)
    }

    /// The baseline poll interval.
    pub(crate) fn baseline_interval(&self) -> Duration {
        Duration::from_millis(self.poll.baseline_ms)
    }

    /// The heartbeat poll interval.
    pub(crate) fn heartbeat_interval(&self) -> Duration {
        Duration::from_millis(self.poll.heartbeat_ms)
    }

    /// A human-readable multi-line description (used for `--dry-run`).
    pub(crate) fn describe(&self) -> String {
        use std::fmt::Write as _;
        let mut out = String::new();
        let _ = writeln!(
            out,
            "radio: backend={} model={} transport={} port={} baud={} host={} tcp_port={} \
             certified={} reply_timeout_ms={}",
            self.radio.backend,
            self.radio.model,
            self.radio.transport,
            self.radio.port,
            self.radio.baud,
            self.radio.host,
            self.radio.tcp_port,
            self.radio.certified,
            self.radio.reply_timeout_ms,
        );
        let _ = writeln!(
            out,
            "poll: baseline_ms={} heartbeat_ms={}",
            self.poll.baseline_ms, self.poll.heartbeat_ms
        );
        let _ = writeln!(out, "ptt: max_tx_ms={}", self.ptt.max_tx_ms);
        let _ = writeln!(out, "events: native_push={}", self.events.native_push);
        for face in &self.face {
            let _ = writeln!(
                out,
                "face: name={} transport={} baud={} dialect={} perms={:?}",
                face.name, face.transport, face.baud, face.dialect, face.perms
            );
        }
        for ep in &self.hamlib_net {
            let _ = writeln!(
                out,
                "hamlib_net: name={} bind={} perms={:?}",
                ep.name, ep.bind, ep.perms
            );
        }
        out
    }

    /// The default config path: the per-user unified `config.toml` shared with the engine and
    /// launcher. Resolution mirrors the engine and launcher:
    ///
    /// 1. `QSORIPPER_CONFIG_PATH` if set,
    /// 2. `%APPDATA%\qsoripper\config.toml` (Windows) or
    ///    `$XDG_CONFIG_HOME/qsoripper/config.toml` → `$HOME/.config/qsoripper/config.toml` (Unix),
    /// 3. a bare `config.toml` in the working directory as a last resort.
    ///
    /// Daemon settings live under the `[cat_hub]` table of that file (see
    /// [`Config::parse_document`]); a standalone `--config cathub.toml` is still accepted.
    pub(crate) fn default_config_path() -> PathBuf {
        if let Some(path) = std::env::var_os(CONFIG_PATH_ENV) {
            return PathBuf::from(path);
        }
        #[cfg(target_os = "windows")]
        {
            if let Some(app_data) = std::env::var_os("APPDATA") {
                return PathBuf::from(app_data).join(SHARED_DIR).join(SHARED_FILE);
            }
        }
        #[cfg(not(target_os = "windows"))]
        {
            if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
                return PathBuf::from(xdg).join(SHARED_DIR).join(SHARED_FILE);
            }
            if let Some(home) = std::env::var_os("HOME") {
                return PathBuf::from(home)
                    .join(".config")
                    .join(SHARED_DIR)
                    .join(SHARED_FILE);
            }
        }
        PathBuf::from(SHARED_FILE)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
[radio]
backend = "ts590"
model = "TS-590SG"
transport = "serial"
port = "COM3"
baud = 4800

[poll]
baseline_ms = 200
heartbeat_ms = 2500

[ptt]
max_tx_ms = 120000

[events]
native_push = true

[[face]]
name = "n1mm"
transport = "COM11"
baud = 4800
dialect = "ts590"
perms = ["read", "write", "ptt"]

[[hamlib_net]]
name = "engine"
bind = "127.0.0.1:4532"
perms = ["read"]
"#;

    #[test]
    fn parses_full_config() {
        let config = Config::parse(SAMPLE).expect("parse");
        assert_eq!(config.radio.backend, "ts590");
        assert_eq!(config.radio.port, "COM3");
        assert_eq!(config.face.len(), 1);
        assert_eq!(config.hamlib_net.len(), 1);
        assert!(config.face[0].permissions().ptt);
        assert!(config.hamlib_net[0].permissions().read);
        assert!(!config.hamlib_net[0].permissions().write);
        assert_eq!(config.baseline_interval(), Duration::from_millis(200));
        assert_eq!(config.heartbeat_interval(), Duration::from_millis(2_500));
        assert_eq!(config.ptt_max_tx(), Duration::from_millis(120_000));
    }

    #[test]
    fn applies_defaults() {
        let text = r#"
[radio]
backend = "loopback"

[[face]]
name = "x"
transport = "COM5"
dialect = "ts590"
"#;
        let config = Config::parse(text).expect("parse");
        assert_eq!(config.radio.baud, 4_800);
        assert_eq!(config.poll.baseline_ms, 250);
        assert_eq!(config.ptt.max_tx_ms, 300_000);
        assert!(config.events.native_push);
        assert_eq!(config.face[0].baud, 4_800);
    }

    #[test]
    fn rejects_unknown_backend() {
        let text = r#"
[radio]
backend = "icom"
[[face]]
name = "x"
transport = "COM5"
dialect = "ts590"
"#;
        assert!(Config::parse(text).is_err());
    }

    #[test]
    fn serial_backend_requires_port() {
        let text = r#"
[radio]
backend = "ts590"
transport = "serial"
[[face]]
name = "x"
transport = "COM5"
dialect = "ts590"
"#;
        let err = Config::parse(text).expect_err("missing port");
        assert!(err.to_string().contains("requires radio.port"));
    }

    #[test]
    fn rejects_unknown_dialect() {
        let text = r#"
[radio]
backend = "loopback"
[[face]]
name = "x"
transport = "COM5"
dialect = "yaesu"
"#;
        assert!(Config::parse(text).is_err());
    }

    #[test]
    fn requires_at_least_one_endpoint() {
        let text = r#"
[radio]
backend = "loopback"
"#;
        assert!(Config::parse(text).is_err());
    }

    #[test]
    fn describe_mentions_all_sections() {
        let config = Config::parse(SAMPLE).expect("parse");
        let text = config.describe();
        assert!(text.contains("radio: backend=ts590"));
        assert!(text.contains("reply_timeout_ms=1000"));
        assert!(text.contains("poll: baseline_ms=200"));
        assert!(text.contains("ptt: max_tx_ms=120000"));
        assert!(text.contains("events: native_push=true"));
        assert!(text.contains("face: name=n1mm"));
        assert!(text.contains("hamlib_net: name=engine"));
    }

    #[test]
    fn default_path_is_unified_config_toml() {
        // With no env override, the default resolves to the shared unified file name and lives
        // under the shared per-user directory rather than a standalone cathub.toml.
        let path = Config::default_config_path();
        let text = path.to_string_lossy();
        assert!(
            text.ends_with("config.toml"),
            "expected config.toml, got {text}"
        );
        // The bare last-resort fallback is the only case without the shared dir; on dev/CI
        // machines APPDATA/HOME are set, so the shared dir should be present.
        if std::env::var_os(CONFIG_PATH_ENV).is_none() {
            assert!(
                text.contains(SHARED_DIR) || text == SHARED_FILE,
                "expected shared dir or bare fallback, got {text}"
            );
        }
    }

    #[test]
    fn parses_unified_cat_hub_section() {
        // A unified config.toml carrying unrelated engine/launcher tables plus a [cat_hub]
        // subtree must load only the cat_hub subtree and ignore everything else.
        let text = r#"
[station_profile]
callsign = "K7ABC"

[launcher]
selected = ["engine-rust"]

[cat_hub.radio]
backend = "ts590"
transport = "serial"
port = "COM4"
baud = 4800

[[cat_hub.face]]
name = "n1mm"
transport = "COM11"
dialect = "ts590"
perms = ["read", "write", "ptt"]

[[cat_hub.hamlib_net]]
name = "engine"
bind = "127.0.0.1:4532"
perms = ["read"]
"#;
        let config = Config::parse_document(text).expect("parse unified");
        assert_eq!(config.radio.backend, "ts590");
        assert_eq!(config.radio.port, "COM4");
        assert_eq!(config.face.len(), 1);
        assert_eq!(config.hamlib_net.len(), 1);
        assert!(config.face[0].permissions().ptt);
    }

    #[test]
    fn parse_document_falls_back_to_standalone() {
        // A standalone config without a [cat_hub] table still parses for back-compat.
        let config = Config::parse_document(SAMPLE).expect("parse standalone");
        assert_eq!(config.radio.backend, "ts590");
        assert_eq!(config.radio.port, "COM3");
        assert_eq!(config.face.len(), 1);
    }

    #[test]
    fn parse_document_validates_cat_hub_section() {
        // Validation still applies to the nested subtree (no endpoints is invalid).
        let text = r#"
[cat_hub.radio]
backend = "loopback"
"#;
        assert!(Config::parse_document(text).is_err());
    }
}

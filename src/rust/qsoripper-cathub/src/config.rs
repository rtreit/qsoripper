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

impl Config {
    /// Parse a configuration from a TOML string.
    pub(crate) fn parse(text: &str) -> Result<Config, ConfigError> {
        let config: Config = toml::from_str(text)?;
        config.validate()?;
        Ok(config)
    }

    /// Load and parse a configuration from a file.
    pub(crate) fn load(path: &std::path::Path) -> Result<Config, ConfigError> {
        let text = std::fs::read_to_string(path)?;
        Config::parse(&text)
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
        let mut out = String::new();
        out.push_str(&format!(
            "radio: backend={} model={} transport={} port={} baud={} host={} tcp_port={} \
             certified={} reply_timeout_ms={}\n",
            self.radio.backend,
            self.radio.model,
            self.radio.transport,
            self.radio.port,
            self.radio.baud,
            self.radio.host,
            self.radio.tcp_port,
            self.radio.certified,
            self.radio.reply_timeout_ms,
        ));
        out.push_str(&format!(
            "poll: baseline_ms={} heartbeat_ms={}\n",
            self.poll.baseline_ms, self.poll.heartbeat_ms
        ));
        out.push_str(&format!("ptt: max_tx_ms={}\n", self.ptt.max_tx_ms));
        out.push_str(&format!("events: native_push={}\n", self.events.native_push));
        for face in &self.face {
            out.push_str(&format!(
                "face: name={} transport={} baud={} dialect={} perms={:?}\n",
                face.name, face.transport, face.baud, face.dialect, face.perms
            ));
        }
        for ep in &self.hamlib_net {
            out.push_str(&format!(
                "hamlib_net: name={} bind={} perms={:?}\n",
                ep.name, ep.bind, ep.perms
            ));
        }
        out
    }

    /// The default config path for this platform.
    pub(crate) fn default_config_path() -> PathBuf {
        #[cfg(target_os = "windows")]
        {
            if let Ok(profile) = std::env::var("USERPROFILE") {
                return PathBuf::from(profile).join("cathub.toml");
            }
        }
        #[cfg(not(target_os = "windows"))]
        {
            if let Ok(home) = std::env::var("HOME") {
                return PathBuf::from(home).join(".config").join("cathub.toml");
            }
        }
        PathBuf::from("cathub.toml")
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
    fn default_path_is_named_cathub_toml() {
        assert!(Config::default_config_path()
            .to_string_lossy()
            .ends_with("cathub.toml"));
    }
}

//! Daemon configuration (TOML).
//!
//! One `[radio]` section selects and parameterizes the backend; `[poll]`, `[ptt]`, and
//! `[events]` tune cadence and safety; `[[serial_endpoint]]` and `[[hamlib_net]]` declare the client
//! endpoints. Everything but `[radio].backend` has a sane default so a minimal config is
//! short.

use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;

use crate::error::ConfigError;
use crate::permissions::EndpointPermissions;

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
fn default_winkeyer_baud() -> u32 {
    1_200
}
fn default_winkeyer_max_tx_ms() -> u64 {
    30_000
}
fn default_winkeyer_api_bind() -> String {
    "127.0.0.1:50071".to_string()
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

/// A `[[serial_endpoint]]` (serial client) endpoint.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct SerialEndpointConfig {
    /// A label for logging.
    pub(crate) name: String,
    /// The serial port this endpoint listens on (a com0com / tty path).
    pub(crate) transport: String,
    /// The paired endpoint opened by the client application. The hub never opens it.
    #[serde(default)]
    pub(crate) application_transport: Option<String>,
    /// Baud rate for the endpoint port.
    #[serde(default = "default_baud")]
    pub(crate) baud: u32,
    /// Dialect: `ts590` or `ts2000`.
    pub(crate) dialect: String,
    /// Permission tokens (`read`, `frequency_write`, `write`, `ptt`, `config_write`).
    #[serde(default)]
    pub(crate) perms: Vec<String>,
    /// Present the *operating* VFO as VFO A to this endpoint (operating-VFO virtualization).
    ///
    /// Single-VFO loggers (notably N1MM Logger+ in SO1V) read the active VFO from the
    /// Kenwood `IF;` answer and refuse to track VFO B ("You should not use VFO B when
    /// configured for SO1V"). With `single_vfo = true` the hub always presents whichever
    /// VFO the operator is actually using as VFO A, so the logger follows A/B switches
    /// seamlessly with no warning. Leave this `false` for true dual-VFO control endpoints such
    /// as ARCP-590, which must see and address real VFO A and B independently.
    #[serde(default)]
    pub(crate) single_vfo: bool,
}

impl SerialEndpointConfig {
    /// The parsed permission set.
    pub(crate) fn permissions(&self) -> EndpointPermissions {
        EndpointPermissions::from_tokens(&self.perms)
    }
}

/// A `[[hamlib_net]]` (rigctld-compatible TCP) endpoint.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct HamlibNetConfig {
    /// A label for logging.
    pub(crate) name: String,
    /// The bind address (e.g. `127.0.0.1:4532`).
    pub(crate) bind: String,
    /// Permission tokens (`read`, `frequency_write`, `write`, `ptt`, `config_write`).
    #[serde(default)]
    pub(crate) perms: Vec<String>,
    /// Present the *operating* VFO as VFO A to this endpoint (operating-VFO virtualization).
    ///
    /// Single-VFO rigctld clients (notably WSJT-X, which expects to receive on VFO A, and
    /// Log4OM, which polls `\get_vfo_info VFOA`) misbehave when the hub reports the true
    /// VFO B as the active receive VFO: WSJT-X stops decoding and Log4OM logs the inactive
    /// VFO A's stale frequency. With `single_vfo = true` the endpoint always presents
    /// whichever VFO the operator is actually using as VFO A (`get_vfo` -> `VFOA`,
    /// `get_vfo_info` answers from the operating VFO with `Split: 0`, `get_split_vfo` is
    /// `0/VFOA`, and `set_split_vfo 1` is rejected so a client never believes a real A/B
    /// split was armed). Leave this `false` for true dual-VFO control endpoints that must
    /// see and address real VFO A and B independently.
    #[serde(default)]
    pub(crate) single_vfo: bool,
}

impl HamlibNetConfig {
    /// The parsed permission set.
    pub(crate) fn permissions(&self) -> EndpointPermissions {
        EndpointPermissions::from_tokens(&self.perms)
    }
}

/// The optional `[winkeyer]` physical-keyer broker section.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct WinkeyerConfig {
    /// Physical WinKeyer serial port exclusively owned by CatHub.
    pub(crate) port: String,
    /// Physical baud rate. WinKeyer always starts at 1200 baud.
    #[serde(default = "default_winkeyer_baud")]
    pub(crate) baud: u32,
    /// Broker-wide transmit safety ceiling.
    #[serde(default = "default_winkeyer_max_tx_ms")]
    pub(crate) max_tx_ms: u64,
    /// Loopback gRPC endpoint used by QsoRipper engines.
    #[serde(default = "default_winkeyer_api_bind")]
    pub(crate) api_bind: String,
}

/// One virtual WinKeyer serial endpoint backed by a com0com/PTY pair.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct WinkeyerEndpointConfig {
    /// Stable endpoint name used in logs and ownership status.
    pub(crate) name: String,
    /// Hub side of the virtual serial pair.
    pub(crate) transport: String,
    /// Paired endpoint opened by the client application. The hub never opens it.
    #[serde(default)]
    pub(crate) application_transport: Option<String>,
    /// Virtual endpoint baud rate.
    #[serde(default = "default_winkeyer_baud")]
    pub(crate) baud: u32,
    /// Whether this client controls the idle paddle/foreground settings.
    #[serde(default)]
    pub(crate) primary: bool,
    /// Permission tokens: `status`, `send`, `control`, `ptt`, `config_write`.
    #[serde(default)]
    pub(crate) perms: Vec<String>,
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
    pub(crate) serial_endpoint: Vec<SerialEndpointConfig>,
    /// Hamlib net endpoints.
    #[serde(default)]
    pub(crate) hamlib_net: Vec<HamlibNetConfig>,
    /// Optional physical WinKeyer broker.
    #[serde(default)]
    pub(crate) winkeyer: Option<WinkeyerConfig>,
    /// Virtual WinKeyer client endpoints.
    #[serde(default)]
    pub(crate) winkeyer_endpoint: Vec<WinkeyerEndpointConfig>,
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
        for endpoint in &self.serial_endpoint {
            if endpoint
                .application_transport
                .as_deref()
                .is_some_and(|application| {
                    application
                        .trim()
                        .eq_ignore_ascii_case(endpoint.transport.trim())
                })
            {
                return Err(ConfigError::Invalid(format!(
                    "endpoint '{}' application_transport must differ from transport",
                    endpoint.name
                )));
            }
            if !matches!(
                endpoint.dialect.as_str(),
                "ts590" | "ts590-transparent" | "ts2000"
            ) {
                return Err(ConfigError::Invalid(format!(
                    "endpoint '{}' has unknown dialect '{}' (expected ts590, ts590-transparent, or ts2000)",
                    endpoint.name, endpoint.dialect
                )));
            }
            if endpoint.dialect == "ts590-transparent" && endpoint.single_vfo {
                return Err(ConfigError::Invalid(format!(
                    "endpoint '{}' combines dialect 'ts590-transparent' with single_vfo = true; a \
                     transparent mirror endpoint relays the radio's real dual-VFO stream verbatim and \
                     cannot virtualize the operating VFO",
                    endpoint.name
                )));
            }
        }
        if self.serial_endpoint.is_empty() && self.hamlib_net.is_empty() {
            return Err(ConfigError::Invalid(
                "at least one [[serial_endpoint]] or [[hamlib_net]] endpoint is required"
                    .to_string(),
            ));
        }
        self.validate_winkeyer()?;
        Ok(())
    }

    fn validate_winkeyer(&self) -> Result<(), ConfigError> {
        let Some(winkeyer) = &self.winkeyer else {
            if !self.winkeyer_endpoint.is_empty() {
                return Err(ConfigError::Invalid(
                    "[[winkeyer_endpoint]] requires a [winkeyer] physical device section"
                        .to_string(),
                ));
            }
            return Ok(());
        };
        if winkeyer.port.trim().is_empty() {
            return Err(ConfigError::Invalid(
                "winkeyer.port must not be empty".to_string(),
            ));
        }
        if winkeyer.baud != 1_200 {
            return Err(ConfigError::Invalid(
                "winkeyer.baud must be 1200; high-baud session switching is not broker-safe"
                    .to_string(),
            ));
        }
        if !(1_000..=300_000).contains(&winkeyer.max_tx_ms) {
            return Err(ConfigError::Invalid(
                "winkeyer.max_tx_ms must be between 1000 and 300000".to_string(),
            ));
        }
        let bind: std::net::SocketAddr = winkeyer.api_bind.parse().map_err(|_| {
            ConfigError::Invalid("winkeyer.api_bind must be a host:port socket address".to_string())
        })?;
        if !bind.ip().is_loopback() {
            return Err(ConfigError::Invalid(
                "winkeyer.api_bind must use a loopback address".to_string(),
            ));
        }
        if winkeyer.port.eq_ignore_ascii_case(&self.radio.port) {
            return Err(ConfigError::Invalid(
                "winkeyer.port must be distinct from radio.port".to_string(),
            ));
        }
        let primary_count = self
            .winkeyer_endpoint
            .iter()
            .filter(|endpoint| endpoint.primary)
            .count();
        if primary_count > 1 {
            return Err(ConfigError::Invalid(
                "at most one [[winkeyer_endpoint]] may set primary = true".to_string(),
            ));
        }
        let mut transports = std::collections::BTreeSet::new();
        for endpoint in &self.winkeyer_endpoint {
            validate_winkeyer_endpoint(endpoint)?;
            let normalized = endpoint.transport.to_ascii_uppercase();
            if !transports.insert(normalized) {
                return Err(ConfigError::Invalid(
                    "winkeyer endpoint transports must be distinct".to_string(),
                ));
            }
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
        for endpoint in &self.serial_endpoint {
            let _ = writeln!(
                out,
                "serial_endpoint: name={} transport={} baud={} dialect={} perms={:?} single_vfo={}",
                endpoint.name,
                endpoint.transport,
                endpoint.baud,
                endpoint.dialect,
                endpoint.perms,
                endpoint.single_vfo
            );
        }
        for ep in &self.hamlib_net {
            let _ = writeln!(
                out,
                "hamlib_net: name={} bind={} perms={:?} single_vfo={}",
                ep.name, ep.bind, ep.perms, ep.single_vfo
            );
        }
        self.append_winkeyer_description(&mut out);
        if !self.serial_endpoint.is_empty() || !self.hamlib_net.is_empty() {
            out.push('\n');
            let _ = writeln!(
                out,
                "Client connection guide -- point each application HERE, never at the radio's own \
                 port ({}):",
                self.radio.port
            );
            for endpoint in &self.serial_endpoint {
                if let Some(application_transport) = endpoint.application_transport.as_deref() {
                    let _ = writeln!(
                        out,
                        "  - {name}: hub={hub}, application={application}, {dialect} dialect, {baud} baud.",
                        name = endpoint.name,
                        hub = endpoint.transport,
                        application = application_transport,
                        dialect = endpoint.dialect,
                        baud = endpoint.baud,
                    );
                } else {
                    let _ = writeln!(
                        out,
                        "  - {name}: the hub owns {hub}; application port is not recorded, {dialect} dialect, {baud} baud.",
                        name = endpoint.name,
                        hub = endpoint.transport,
                        dialect = endpoint.dialect,
                        baud = endpoint.baud,
                    );
                }
            }
            for ep in &self.hamlib_net {
                let _ = writeln!(
                    out,
                    "  - {name}: point this application at {bind} as a Hamlib NET (rigctld) device.",
                    name = ep.name,
                    bind = ep.bind,
                );
            }
        }
        out
    }

    fn append_winkeyer_description(&self, out: &mut String) {
        use std::fmt::Write as _;
        let Some(winkeyer) = &self.winkeyer else {
            return;
        };
        let _ = writeln!(
            out,
            "winkeyer: port={} baud={} max_tx_ms={} api_bind={}",
            winkeyer.port, winkeyer.baud, winkeyer.max_tx_ms, winkeyer.api_bind
        );
        for endpoint in &self.winkeyer_endpoint {
            let _ = writeln!(
                out,
                "winkeyer_endpoint: name={} hub_transport={} application_transport={} baud={} primary={} perms={:?}",
                endpoint.name,
                endpoint.transport,
                endpoint.application_transport.as_deref().unwrap_or("(not recorded)"),
                endpoint.baud,
                endpoint.primary,
                endpoint.perms
            );
        }
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

fn validate_winkeyer_endpoint(endpoint: &WinkeyerEndpointConfig) -> Result<(), ConfigError> {
    if endpoint.transport.trim().is_empty() {
        return Err(ConfigError::Invalid(format!(
            "winkeyer endpoint '{}' requires transport",
            endpoint.name
        )));
    }
    if endpoint.baud != 1_200 {
        return Err(ConfigError::Invalid(format!(
            "winkeyer endpoint '{}' baud must be 1200",
            endpoint.name
        )));
    }
    if endpoint
        .application_transport
        .as_deref()
        .is_some_and(|application| {
            application
                .trim()
                .eq_ignore_ascii_case(endpoint.transport.trim())
        })
    {
        return Err(ConfigError::Invalid(format!(
            "winkeyer endpoint '{}' application_transport must differ from transport",
            endpoint.name
        )));
    }
    for permission in &endpoint.perms {
        if !matches!(
            permission.as_str(),
            "status" | "send" | "control" | "ptt" | "config_write"
        ) {
            return Err(ConfigError::Invalid(format!(
                "winkeyer endpoint '{}' has unknown permission '{}'",
                endpoint.name, permission
            )));
        }
    }
    let has = |permission: &str| endpoint.perms.iter().any(|value| value == permission);
    if has("send") && !has("status") {
        return Err(ConfigError::Invalid(format!(
            "winkeyer endpoint '{}' permission 'send' requires 'status'",
            endpoint.name
        )));
    }
    if has("ptt") && (!has("send") || !has("control")) {
        return Err(ConfigError::Invalid(format!(
            "winkeyer endpoint '{}' permission 'ptt' requires 'send' and 'control'",
            endpoint.name
        )));
    }
    if has("config_write") && (!has("status") || !has("control")) {
        return Err(ConfigError::Invalid(format!(
            "winkeyer endpoint '{}' permission 'config_write' requires 'status' and 'control'",
            endpoint.name
        )));
    }
    Ok(())
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

[[serial_endpoint]]
name = "n1mm"
transport = "COM20"
application_transport = "COM21"
baud = 4800
dialect = "ts590"
perms = ["read", "write", "ptt"]
single_vfo = true

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
        assert_eq!(config.serial_endpoint.len(), 1);
        assert_eq!(config.hamlib_net.len(), 1);
        assert!(config.serial_endpoint[0].permissions().ptt);
        assert!(
            config.serial_endpoint[0].single_vfo,
            "single_vfo parses as true"
        );
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

[[serial_endpoint]]
name = "x"
transport = "COM5"
dialect = "ts590"
"#;
        let config = Config::parse(text).expect("parse");
        assert_eq!(config.radio.baud, 4_800);
        assert_eq!(config.poll.baseline_ms, 250);
        assert_eq!(config.ptt.max_tx_ms, 300_000);
        assert!(config.events.native_push);
        assert_eq!(config.serial_endpoint[0].baud, 4_800);
        assert!(
            !config.serial_endpoint[0].single_vfo,
            "single_vfo defaults to false (native dual-VFO presentation)"
        );
    }

    #[test]
    fn rejects_unknown_backend() {
        let text = r#"
[radio]
backend = "icom"
[[serial_endpoint]]
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
[[serial_endpoint]]
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
[[serial_endpoint]]
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
        assert!(text.contains("endpoint: name=n1mm"));
        assert!(text.contains("hamlib_net: name=engine"));
        assert!(text.contains("Client connection guide"));
        assert!(text.contains("hub=COM20, application=COM21"));
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

[[cat_hub.serial_endpoint]]
name = "n1mm"
transport = "COM20"
application_transport = "COM21"
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
        assert_eq!(config.serial_endpoint.len(), 1);
        assert_eq!(
            config.serial_endpoint[0].application_transport.as_deref(),
            Some("COM21")
        );
        assert_eq!(config.hamlib_net.len(), 1);
        assert!(config.serial_endpoint[0].permissions().ptt);
    }

    #[test]
    fn parse_document_falls_back_to_standalone() {
        // A standalone config without a [cat_hub] table still parses for back-compat.
        let config = Config::parse_document(SAMPLE).expect("parse standalone");
        assert_eq!(config.radio.backend, "ts590");
        assert_eq!(config.radio.port, "COM3");
        assert_eq!(config.serial_endpoint.len(), 1);
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

    #[test]
    fn parses_and_describes_winkeyer_broker() {
        let text = r#"
[radio]
backend = "loopback"

[[hamlib_net]]
name = "engine"
bind = "127.0.0.1:4532"

[winkeyer]
port = "COM3"
max_tx_ms = 30000
api_bind = "127.0.0.1:50071"

[[winkeyer_endpoint]]
name = "n1mm"
transport = "COM40"
application_transport = "COM41"
primary = true
perms = ["status", "send", "control", "ptt"]
"#;
        let config = Config::parse(text).expect("parse keyer");
        let winkeyer = config.winkeyer.as_ref().expect("winkeyer");
        assert_eq!(winkeyer.port, "COM3");
        assert_eq!(winkeyer.baud, 1_200);
        assert_eq!(config.winkeyer_endpoint.len(), 1);
        assert!(config.winkeyer_endpoint[0].primary);
        assert_eq!(
            config.winkeyer_endpoint[0].application_transport.as_deref(),
            Some("COM41")
        );
        let description = config.describe();
        assert!(description.contains("winkeyer: port=COM3"));
        assert!(description.contains("winkeyer_endpoint: name=n1mm"));
        assert!(description.contains("application_transport=COM41"));
    }

    #[test]
    fn rejects_matching_hub_and_application_transports() {
        let serial = SAMPLE.replace(
            "application_transport = \"COM21\"",
            "application_transport = \"com20\"",
        );
        assert!(Config::parse(&serial)
            .expect_err("matching serial pair")
            .to_string()
            .contains("application_transport must differ"));

        let winkeyer = r#"
[radio]
backend = "loopback"
[[hamlib_net]]
name = "engine"
bind = "127.0.0.1:4532"
[winkeyer]
port = "COM3"
[[winkeyer_endpoint]]
name = "wktools"
transport = "COM42"
application_transport = "com42"
"#;
        assert!(Config::parse(winkeyer)
            .expect_err("matching WinKeyer pair")
            .to_string()
            .contains("application_transport must differ"));
    }

    #[test]
    fn rejects_non_loopback_winkeyer_api_and_duplicate_primary_endpoints() {
        let non_loopback = r#"
[radio]
backend = "loopback"
[[hamlib_net]]
name = "engine"
bind = "127.0.0.1:4532"
[winkeyer]
port = "COM3"
api_bind = "0.0.0.0:50071"
"#;
        assert!(Config::parse(non_loopback)
            .expect_err("non-loopback")
            .to_string()
            .contains("loopback"));

        let duplicate_primary = r#"
[radio]
backend = "loopback"
[[hamlib_net]]
name = "engine"
bind = "127.0.0.1:4532"
[winkeyer]
port = "COM3"
[[winkeyer_endpoint]]
name = "one"
transport = "COM40"
primary = true
[[winkeyer_endpoint]]
name = "two"
transport = "COM42"
primary = true
"#;
        assert!(Config::parse(duplicate_primary)
            .expect_err("primary")
            .to_string()
            .contains("at most one"));
    }

    #[test]
    fn rejects_keyer_port_collision_and_endpoints_without_device() {
        let collision = r#"
[radio]
backend = "ts590"
port = "COM3"
[[hamlib_net]]
name = "engine"
bind = "127.0.0.1:4532"
[winkeyer]
port = "com3"
"#;
        assert!(Config::parse(collision)
            .expect_err("collision")
            .to_string()
            .contains("distinct"));

        let orphan = r#"
[radio]
backend = "loopback"
[[hamlib_net]]
name = "engine"
bind = "127.0.0.1:4532"
[[winkeyer_endpoint]]
name = "n1mm"
transport = "COM40"
"#;
        assert!(Config::parse(orphan)
            .expect_err("orphan")
            .to_string()
            .contains("requires a [winkeyer]"));
    }
}

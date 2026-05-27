//! CW keying support for contest macro expansion and keyer backends.

use std::fmt::Write as _;
use std::time::Duration;

use thiserror::Error;

use crate::proto::qsoripper::domain::StationProfile;
use crate::proto::qsoripper::services::{CwKeyerBackend, CwKeyerStatus, CwMacro, CwSendContext};

/// Environment variable that selects the CW keyer backend.
pub const CW_KEYER_BACKEND_ENV_VAR: &str = "QSORIPPER_CW_KEYER_BACKEND";
/// Environment variable that identifies the `WinKeyer` serial port.
pub const CW_WINKEYER_PORT_ENV_VAR: &str = "QSORIPPER_CW_WINKEYER_PORT";
/// Environment variable that overrides the `WinKeyer` serial baud rate.
pub const CW_WINKEYER_BAUD_ENV_VAR: &str = "QSORIPPER_CW_WINKEYER_BAUD";
/// Environment variable that sets the default CW speed in words per minute.
pub const CW_SPEED_WPM_ENV_VAR: &str = "QSORIPPER_CW_SPEED_WPM";
/// Default CW speed used when no override is configured.
pub const DEFAULT_CW_SPEED_WPM: u32 = 25;
/// Default `WinKeyer` serial baud rate.
pub const DEFAULT_WINKEYER_BAUD: u32 = 1200;

/// Errors produced while expanding or sending CW.
#[derive(Debug, Error)]
pub enum CwError {
    /// The requested named macro is not defined.
    #[error("unknown CW macro '{0}'")]
    UnknownMacro(String),
    /// A macro template referenced a token the engine does not understand.
    #[error("unknown CW macro token '{0}'")]
    UnknownToken(String),
    /// A macro template opened a token without closing it.
    #[error("unmatched '{{' in CW macro template")]
    UnmatchedOpenBrace,
    /// A macro template contained an unmatched close brace.
    #[error("unmatched '}}' in CW macro template")]
    UnmatchedCloseBrace,
    /// A known token could not be resolved from station or send context.
    #[error("CW macro token '{0}' requires {1}")]
    MissingTokenValue(&'static str, &'static str),
    /// The selected backend cannot accept CW because it is not configured.
    #[error("CW keyer backend is not configured: {0}")]
    BackendUnavailable(String),
    /// The selected backend failed while talking to keying hardware.
    #[error("CW keyer I/O failed: {0}")]
    Io(String),
}

/// Engine-side CW keying backend selection.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum CwBackendKind {
    /// Dry-run backend used for development, tests, and CI.
    Null,
    /// K1EL WinKeyer-compatible serial hardware.
    Winkeyer,
    /// Reserved future UDP cwdaemon backend.
    Cwdaemon,
}

/// Runtime configuration for CW macro expansion and keyer hardware.
#[derive(Debug, Clone)]
pub struct CwKeyerConfig {
    /// Configured backend kind.
    pub backend: CwBackendKind,
    /// Optional `WinKeyer` serial port name.
    pub winkeyer_port: Option<String>,
    /// `WinKeyer` serial baud rate.
    pub winkeyer_baud: u32,
    /// Default keying speed in words per minute.
    pub default_speed_wpm: u32,
}

impl CwKeyerConfig {
    /// Builds configuration from raw string values, applying defaults and validation.
    ///
    /// # Errors
    ///
    /// Returns an error when the backend name is unsupported or numeric configuration cannot be parsed.
    pub fn from_values(
        backend: Option<&str>,
        winkeyer_port: Option<String>,
        winkeyer_baud: Option<&str>,
        default_speed_wpm: Option<&str>,
    ) -> Result<Self, CwError> {
        let backend = match backend
            .unwrap_or("null")
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "" | "null" => CwBackendKind::Null,
            "winkeyer" => CwBackendKind::Winkeyer,
            "cwdaemon" => CwBackendKind::Cwdaemon,
            value => {
                return Err(CwError::BackendUnavailable(format!(
                    "unsupported backend '{value}'"
                )));
            }
        };
        let winkeyer_baud = parse_u32_or_default(winkeyer_baud, DEFAULT_WINKEYER_BAUD)?;
        let default_speed_wpm = parse_u32_or_default(default_speed_wpm, DEFAULT_CW_SPEED_WPM)?;

        Ok(Self {
            backend,
            winkeyer_port: winkeyer_port.and_then(|value| non_empty(value.as_str())),
            winkeyer_baud,
            default_speed_wpm: clamp_speed(default_speed_wpm),
        })
    }

    /// Builds configuration from process environment variables.
    ///
    /// # Errors
    ///
    /// Returns an error when configured values fail validation.
    pub fn from_env() -> Result<Self, CwError> {
        Self::from_values(
            std::env::var(CW_KEYER_BACKEND_ENV_VAR).ok().as_deref(),
            std::env::var(CW_WINKEYER_PORT_ENV_VAR).ok(),
            std::env::var(CW_WINKEYER_BAUD_ENV_VAR).ok().as_deref(),
            std::env::var(CW_SPEED_WPM_ENV_VAR).ok().as_deref(),
        )
    }
}

/// Coordinates deterministic CW macro expansion with the configured keyer backend.
#[derive(Clone)]
pub struct CwController {
    config: CwKeyerConfig,
}

impl CwController {
    /// Creates a controller from runtime keyer configuration.
    #[must_use]
    pub fn new(config: CwKeyerConfig) -> Self {
        Self { config }
    }

    /// Returns the built-in contest CW macros exposed by the engine.
    #[must_use]
    pub fn built_in_macros(&self) -> Vec<CwMacro> {
        built_in_macros()
    }

    /// Expands a named built-in macro using station and send context values.
    ///
    /// # Errors
    ///
    /// Returns an error when the macro is unknown or the template cannot be expanded.
    pub fn expand_macro(
        &self,
        name: &str,
        context: Option<&CwSendContext>,
        station_profile: Option<&StationProfile>,
    ) -> Result<String, CwError> {
        let macro_definition = built_in_macros()
            .into_iter()
            .find(|candidate| candidate.name.eq_ignore_ascii_case(name))
            .ok_or_else(|| CwError::UnknownMacro(name.to_string()))?;
        expand_template(&macro_definition.template, context, station_profile)
    }

    /// Expands arbitrary CW text using station and send context values.
    ///
    /// # Errors
    ///
    /// Returns an error when the template references invalid or unavailable tokens.
    pub fn expand_text(
        &self,
        text: &str,
        context: Option<&CwSendContext>,
        station_profile: Option<&StationProfile>,
    ) -> Result<String, CwError> {
        expand_template(text, context, station_profile)
    }

    /// Sends already-expanded text through the configured backend.
    ///
    /// # Errors
    ///
    /// Returns an error when the backend is unavailable or keyer I/O fails.
    pub fn send_text(&self, text: &str, speed_wpm: Option<u32>) -> Result<(), CwError> {
        match self.config.backend {
            CwBackendKind::Null => Ok(()),
            CwBackendKind::Winkeyer => {
                let port_name = self.config.winkeyer_port.as_deref().ok_or_else(|| {
                    CwError::BackendUnavailable(format!("{CW_WINKEYER_PORT_ENV_VAR} is required"))
                })?;
                let mut keyer = WinkeyerPort::open(port_name, self.config.winkeyer_baud)?;
                keyer.initialize()?;
                keyer.set_speed(speed_wpm.unwrap_or(self.config.default_speed_wpm))?;
                keyer.send_text(text)
            }
            CwBackendKind::Cwdaemon => Err(CwError::BackendUnavailable(
                "cwdaemon backend is reserved but not implemented".to_string(),
            )),
        }
    }

    /// Requests that the configured backend clear or abort queued CW.
    ///
    /// # Errors
    ///
    /// Returns an error when the backend is unavailable or keyer I/O fails.
    pub fn abort(&self) -> Result<(), CwError> {
        match self.config.backend {
            CwBackendKind::Null => Ok(()),
            CwBackendKind::Winkeyer => {
                let port_name = self.config.winkeyer_port.as_deref().ok_or_else(|| {
                    CwError::BackendUnavailable(format!("{CW_WINKEYER_PORT_ENV_VAR} is required"))
                })?;
                let mut keyer = WinkeyerPort::open(port_name, self.config.winkeyer_baud)?;
                keyer.initialize()?;
                keyer.clear_buffer()
            }
            CwBackendKind::Cwdaemon => Err(CwError::BackendUnavailable(
                "cwdaemon backend is reserved but not implemented".to_string(),
            )),
        }
    }

    /// Sets the active CW speed on the configured backend and returns the clamped value.
    ///
    /// # Errors
    ///
    /// Returns an error when the backend is unavailable or keyer I/O fails.
    pub fn set_speed(&self, speed_wpm: u32) -> Result<u32, CwError> {
        let speed_wpm = clamp_speed(speed_wpm);
        match self.config.backend {
            CwBackendKind::Null => Ok(speed_wpm),
            CwBackendKind::Winkeyer => {
                let port_name = self.config.winkeyer_port.as_deref().ok_or_else(|| {
                    CwError::BackendUnavailable(format!("{CW_WINKEYER_PORT_ENV_VAR} is required"))
                })?;
                let mut keyer = WinkeyerPort::open(port_name, self.config.winkeyer_baud)?;
                keyer.initialize()?;
                keyer.set_speed(speed_wpm)?;
                Ok(speed_wpm)
            }
            CwBackendKind::Cwdaemon => Err(CwError::BackendUnavailable(
                "cwdaemon backend is reserved but not implemented".to_string(),
            )),
        }
    }

    /// Reports current configuration and backend availability.
    #[must_use]
    pub fn status(&self) -> CwKeyerStatus {
        CwKeyerStatus {
            backend: backend_to_proto(self.config.backend) as i32,
            available: match self.config.backend {
                CwBackendKind::Null => true,
                CwBackendKind::Winkeyer => self.config.winkeyer_port.is_some(),
                CwBackendKind::Cwdaemon => false,
            },
            speed_wpm: self.config.default_speed_wpm,
            port_name: self.config.winkeyer_port.clone(),
            error_message: match self.config.backend {
                CwBackendKind::Winkeyer if self.config.winkeyer_port.is_none() => {
                    Some(format!("{CW_WINKEYER_PORT_ENV_VAR} is required"))
                }
                CwBackendKind::Cwdaemon => Some("cwdaemon backend is not implemented".to_string()),
                _ => None,
            },
        }
    }
}

/// Expands a CW macro template using the shared token grammar.
///
/// # Errors
///
/// Returns an error when braces are unbalanced, a token is unknown, or a required token value is missing.
pub fn expand_template(
    template: &str,
    context: Option<&CwSendContext>,
    station_profile: Option<&StationProfile>,
) -> Result<String, CwError> {
    let mut output = String::with_capacity(template.len());
    let mut chars = template.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
                output.push('{');
            }
            '}' if chars.peek() == Some(&'}') => {
                chars.next();
                output.push('}');
            }
            '{' => {
                let mut token = String::new();
                loop {
                    match chars.next() {
                        Some('}') => break,
                        Some('{') | None => return Err(CwError::UnmatchedOpenBrace),
                        Some(token_ch) => token.push(token_ch),
                    }
                }
                output.push_str(resolve_token(&token, context, station_profile)?.as_str());
            }
            '}' => return Err(CwError::UnmatchedCloseBrace),
            _ => output.push(ch),
        }
    }

    Ok(output)
}

fn built_in_macros() -> Vec<CwMacro> {
    vec![
        CwMacro {
            name: "cq".to_string(),
            label: "CQ".to_string(),
            template: "CQ TEST {MYCALL} {MYCALL}".to_string(),
        },
        CwMacro {
            name: "exchange".to_string(),
            label: "Exchange".to_string(),
            template: "{HISCALL} {RST} {EXCH}".to_string(),
        },
        CwMacro {
            name: "tu".to_string(),
            label: "TU".to_string(),
            template: "TU {MYCALL}".to_string(),
        },
        CwMacro {
            name: "repeat".to_string(),
            label: "Repeat".to_string(),
            template: "{HISCALL} {RST} {EXCH}".to_string(),
        },
    ]
}

fn resolve_token(
    token: &str,
    context: Option<&CwSendContext>,
    station_profile: Option<&StationProfile>,
) -> Result<String, CwError> {
    match token.trim().to_ascii_uppercase().as_str() {
        "MYCALL" => {
            let value =
                station_profile.and_then(|profile| non_empty(profile.station_callsign.as_str()));
            value.ok_or(CwError::MissingTokenValue(
                "MYCALL",
                "an active station callsign",
            ))
        }
        "HISCALL" => context
            .and_then(|context| context.worked_callsign.as_deref())
            .and_then(non_empty)
            .ok_or(CwError::MissingTokenValue("HISCALL", "a worked callsign")),
        "RST" => Ok(context
            .and_then(|context| context.rst.as_deref())
            .and_then(non_empty)
            .unwrap_or_else(|| "599".to_string())),
        "EXCH" => context
            .and_then(|context| context.exchange.as_deref())
            .and_then(non_empty)
            .ok_or(CwError::MissingTokenValue("EXCH", "an exchange")),
        "NR" => context
            .and_then(|context| context.serial)
            .map(|value| value.to_string())
            .ok_or(CwError::MissingTokenValue("NR", "a serial number")),
        value => Err(CwError::UnknownToken(value.to_string())),
    }
}

fn backend_to_proto(backend: CwBackendKind) -> CwKeyerBackend {
    match backend {
        CwBackendKind::Null => CwKeyerBackend::Null,
        CwBackendKind::Winkeyer => CwKeyerBackend::Winkeyer,
        CwBackendKind::Cwdaemon => CwKeyerBackend::Cwdaemon,
    }
}

fn parse_u32_or_default(value: Option<&str>, default_value: u32) -> Result<u32, CwError> {
    match value.and_then(non_empty) {
        Some(value) => value.parse::<u32>().map_err(|err| {
            CwError::BackendUnavailable(format!(
                "invalid numeric CW configuration '{value}': {err}"
            ))
        }),
        None => Ok(default_value),
    }
}

fn non_empty(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_ascii_uppercase())
    }
}

fn clamp_speed(speed_wpm: u32) -> u32 {
    speed_wpm.clamp(5, 99)
}

struct WinkeyerPort {
    port: serial2::SerialPort,
}

impl WinkeyerPort {
    fn open(port_name: &str, baud_rate: u32) -> Result<Self, CwError> {
        let mut port = serial2::SerialPort::open(port_name, baud_rate)
            .map_err(|err| CwError::Io(format!("open {port_name}: {err}")))?;
        let timeout = Duration::from_millis(500);
        port.set_read_timeout(timeout)
            .map_err(|err| CwError::Io(format!("set read timeout for {port_name}: {err}")))?;
        port.set_write_timeout(timeout)
            .map_err(|err| CwError::Io(format!("set write timeout for {port_name}: {err}")))?;
        Ok(Self { port })
    }

    fn initialize(&mut self) -> Result<(), CwError> {
        self.write_all(&[0x00, 0x02])?;
        let mut version = [0_u8; 1];
        self.port
            .read_exact(&mut version)
            .map_err(|err| CwError::Io(format!("read WinKeyer host-open response: {err}")))?;
        if version[0] == 0xFF {
            return Err(CwError::Io(
                "WinKeyer returned 0xFF; check serial baud rate".to_string(),
            ));
        }

        self.write_all(&[0x00, 0x0B])?;
        self.clear_buffer()
    }

    fn set_speed(&mut self, speed_wpm: u32) -> Result<(), CwError> {
        let speed =
            u8::try_from(clamp_speed(speed_wpm)).map_err(|err| CwError::Io(err.to_string()))?;
        self.write_all(&[0x02, speed])
    }

    fn send_text(&mut self, text: &str) -> Result<(), CwError> {
        let mut bytes = Vec::with_capacity(text.len());
        for ch in text.chars() {
            if !ch.is_ascii() {
                return Err(CwError::Io(format!("non-ASCII CW character '{ch}'")));
            }
            let mut encoded = [0_u8; 4];
            let encoded = ch.to_ascii_uppercase().encode_utf8(&mut encoded);
            bytes.extend_from_slice(encoded.as_bytes());
        }
        self.write_all(bytes.as_slice())
    }

    fn clear_buffer(&mut self) -> Result<(), CwError> {
        self.write_all(&[0x0A])
    }

    fn write_all(&mut self, bytes: &[u8]) -> Result<(), CwError> {
        self.port
            .write_all(bytes)
            .map_err(|err| CwError::Io(format_bytes_error(bytes, &err)))?;
        self.port
            .flush()
            .map_err(|err| CwError::Io(format!("flush serial port: {err}")))
    }
}

fn format_bytes_error(bytes: &[u8], err: &std::io::Error) -> String {
    let mut hex = String::new();
    for (index, byte) in bytes.iter().enumerate() {
        if index > 0 {
            hex.push(' ');
        }
        let _ = write!(&mut hex, "{byte:02X}");
    }
    format!("write [{hex}]: {err}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn station_profile() -> StationProfile {
        StationProfile {
            station_callsign: "K7ABC".to_string(),
            ..StationProfile::default()
        }
    }

    #[test]
    fn expands_built_in_exchange_macro() -> Result<(), CwError> {
        let controller = CwController::new(CwKeyerConfig {
            backend: CwBackendKind::Null,
            winkeyer_port: None,
            winkeyer_baud: DEFAULT_WINKEYER_BAUD,
            default_speed_wpm: DEFAULT_CW_SPEED_WPM,
        });
        let context = CwSendContext {
            worked_callsign: Some("W1AW".to_string()),
            exchange: Some("WA".to_string()),
            ..CwSendContext::default()
        };

        let expanded =
            controller.expand_macro("exchange", Some(&context), Some(&station_profile()))?;

        assert_eq!(expanded, "W1AW 599 WA");
        Ok(())
    }

    #[test]
    fn expands_literal_braces_and_case_insensitive_tokens() -> Result<(), CwError> {
        let expanded = expand_template("{{{mycall}}}", None, Some(&station_profile()))?;

        assert_eq!(expanded, "{K7ABC}");
        Ok(())
    }

    #[test]
    fn rejects_unknown_tokens() {
        let result = expand_template("{NOPE}", None, Some(&station_profile()));

        assert!(matches!(result, Err(CwError::UnknownToken(token)) if token == "NOPE"));
    }
}

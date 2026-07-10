//! CW keying support for contest macro expansion and keyer backends.

use std::fmt::Write as _;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

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
/// Environment variable that explicitly permits hardware CW transmission.
pub const CW_TRANSMIT_ENABLED_ENV_VAR: &str = "QSORIPPER_CW_TRANSMIT_ENABLED";
/// Environment variable that caps one queued hardware transmission.
pub const CW_MAX_TX_MS_ENV_VAR: &str = "QSORIPPER_CW_MAX_TX_MS";
/// Default CW speed used when no override is configured.
pub const DEFAULT_CW_SPEED_WPM: u32 = 25;
/// Default `WinKeyer` serial baud rate.
pub const DEFAULT_WINKEYER_BAUD: u32 = 1200;
/// Default hardware keying safety ceiling.
pub const DEFAULT_CW_MAX_TX_MS: u64 = 120_000;
const MIN_CW_SPEED_WPM: u32 = 5;
const MAX_CW_SPEED_WPM: u32 = 99;
const MIN_CW_MAX_TX_MS: u64 = 1_000;
const MAX_CW_MAX_TX_MS: u64 = 300_000;
const COMMAND_QUEUE_CAPACITY: usize = 32;

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
    /// A speed was outside the WinKeyer-supported range.
    #[error("CW speed must be between 5 and 99 WPM, got {0}")]
    InvalidSpeed(u32),
    /// Text was empty or contained a character the hardware cannot send.
    #[error("invalid CW text: {0}")]
    InvalidText(String),
    /// Hardware transmission was requested without the explicit safety gate.
    #[error(
        "CW hardware transmission is disabled; set {CW_TRANSMIT_ENABLED_ENV_VAR}=true to enable it"
    )]
    TransmitDisabled,
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
    /// Whether real hardware is permitted to key the transmitter.
    pub transmit_enabled: bool,
    /// Maximum duration before the engine clears the `WinKeyer` input buffer.
    pub max_tx_ms: u64,
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
        transmit_enabled: Option<&str>,
        max_tx_ms: Option<&str>,
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
        validate_speed(default_speed_wpm)?;
        let transmit_enabled = parse_bool_or_default(transmit_enabled, false)?;
        let max_tx_ms = parse_u64_or_default(max_tx_ms, DEFAULT_CW_MAX_TX_MS)?;
        if !(MIN_CW_MAX_TX_MS..=MAX_CW_MAX_TX_MS).contains(&max_tx_ms) {
            return Err(CwError::BackendUnavailable(format!(
                "{CW_MAX_TX_MS_ENV_VAR} must be between {MIN_CW_MAX_TX_MS} and {MAX_CW_MAX_TX_MS}, got {max_tx_ms}"
            )));
        }

        Ok(Self {
            backend,
            winkeyer_port: winkeyer_port.and_then(|value| non_empty(value.as_str())),
            winkeyer_baud,
            default_speed_wpm,
            transmit_enabled,
            max_tx_ms,
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
            std::env::var(CW_TRANSMIT_ENABLED_ENV_VAR).ok().as_deref(),
            std::env::var(CW_MAX_TX_MS_ENV_VAR).ok().as_deref(),
        )
    }
}

/// Coordinates deterministic macro expansion with one serialized, engine-owned backend.
#[derive(Clone)]
pub struct CwController {
    commands: SyncSender<CwCommand>,
}

impl CwController {
    /// Creates a controller and its dedicated backend worker.
    #[must_use]
    pub fn new(config: CwKeyerConfig) -> Self {
        Self::with_factory(config, Arc::new(open_winkeyer))
    }

    fn with_factory(config: CwKeyerConfig, factory: WinkeyerFactory) -> Self {
        let (commands, receiver) = mpsc::sync_channel(COMMAND_QUEUE_CAPACITY);
        thread::spawn(move || CwWorker::new(config, factory).run(&receiver));
        Self { commands }
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
        let (reply, response) = mpsc::channel();
        self.send_command(CwCommand::SendText {
            text: text.to_string(),
            speed_wpm,
            reply,
        })?;
        recv_response(&response)
    }

    /// Requests that the configured backend clear or abort queued CW.
    ///
    /// # Errors
    ///
    /// Returns an error when the backend is unavailable or keyer I/O fails.
    pub fn abort(&self) -> Result<(), CwError> {
        let (reply, response) = mpsc::channel();
        self.send_command(CwCommand::Abort { reply })?;
        recv_response(&response)
    }

    /// Sets and retains the active CW speed on the configured backend.
    ///
    /// # Errors
    ///
    /// Returns an error when the backend is unavailable or keyer I/O fails.
    pub fn set_speed(&self, speed_wpm: u32) -> Result<u32, CwError> {
        let (reply, response) = mpsc::channel();
        self.send_command(CwCommand::SetSpeed { speed_wpm, reply })?;
        recv_response(&response)
    }

    /// Reports current configuration and backend availability.
    #[must_use]
    pub fn status(&self) -> CwKeyerStatus {
        let (reply, response) = mpsc::channel();
        if self.send_command(CwCommand::Status { reply }).is_err() {
            return unavailable_status("CW backend worker is not running");
        }
        response
            .recv()
            .unwrap_or_else(|_| unavailable_status("CW backend worker stopped"))
    }

    fn send_command(&self, command: CwCommand) -> Result<(), CwError> {
        self.commands
            .send(command)
            .map_err(|_| CwError::BackendUnavailable("backend worker stopped".to_string()))
    }
}

fn recv_response<T>(response: &Receiver<Result<T, CwError>>) -> Result<T, CwError> {
    response
        .recv()
        .map_err(|_| CwError::BackendUnavailable("backend worker stopped".to_string()))?
}

fn unavailable_status(message: &str) -> CwKeyerStatus {
    CwKeyerStatus {
        available: false,
        error_message: Some(message.to_string()),
        ..CwKeyerStatus::default()
    }
}

enum CwCommand {
    SendText {
        text: String,
        speed_wpm: Option<u32>,
        reply: mpsc::Sender<Result<(), CwError>>,
    },
    Abort {
        reply: mpsc::Sender<Result<(), CwError>>,
    },
    SetSpeed {
        speed_wpm: u32,
        reply: mpsc::Sender<Result<u32, CwError>>,
    },
    Status {
        reply: mpsc::Sender<CwKeyerStatus>,
    },
}

trait WinkeyerDevice: Send {
    fn initialize(&mut self) -> Result<u8, CwError>;
    fn set_speed(&mut self, speed_wpm: u32) -> Result<(), CwError>;
    fn send_text(&mut self, text: &str) -> Result<(), CwError>;
    fn clear_buffer(&mut self) -> Result<(), CwError>;
    fn is_busy(&mut self) -> Result<bool, CwError>;
    fn close(&mut self) -> Result<(), CwError>;
}

type WinkeyerFactory =
    Arc<dyn Fn(&str, u32) -> Result<Box<dyn WinkeyerDevice>, CwError> + Send + Sync>;

struct CwWorker {
    config: CwKeyerConfig,
    active_speed_wpm: u32,
    keyer: Option<Box<dyn WinkeyerDevice>>,
    firmware_revision: Option<u8>,
    last_error: Option<String>,
    watchdog_deadline: Option<Instant>,
    factory: WinkeyerFactory,
}

impl CwWorker {
    fn new(config: CwKeyerConfig, factory: WinkeyerFactory) -> Self {
        Self {
            active_speed_wpm: config.default_speed_wpm,
            config,
            keyer: None,
            firmware_revision: None,
            last_error: None,
            watchdog_deadline: None,
            factory,
        }
    }

    fn run(mut self, receiver: &Receiver<CwCommand>) {
        loop {
            let command_result = match self.watchdog_deadline {
                Some(deadline) => {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    receiver.recv_timeout(remaining)
                }
                None => receiver.recv().map_err(|_| RecvTimeoutError::Disconnected),
            };
            match command_result {
                Ok(command) => self.handle(command),
                Err(RecvTimeoutError::Timeout) => self.expire_watchdog(),
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        self.disconnect();
    }

    fn handle(&mut self, command: CwCommand) {
        match command {
            CwCommand::SendText {
                text,
                speed_wpm,
                reply,
            } => {
                let _ = reply.send(self.send_text(&text, speed_wpm));
            }
            CwCommand::Abort { reply } => {
                let _ = reply.send(self.abort());
            }
            CwCommand::SetSpeed { speed_wpm, reply } => {
                let _ = reply.send(self.set_speed(speed_wpm));
            }
            CwCommand::Status { reply } => {
                let _ = reply.send(self.status());
            }
        }
    }

    fn send_text(&mut self, text: &str, speed_wpm: Option<u32>) -> Result<(), CwError> {
        validate_text(text)?;
        let speed = speed_wpm.unwrap_or(self.active_speed_wpm);
        validate_speed(speed)?;
        match self.config.backend {
            CwBackendKind::Null => {
                self.active_speed_wpm = speed;
                self.last_error = None;
                Ok(())
            }
            CwBackendKind::Winkeyer => {
                if !self.config.transmit_enabled {
                    return Err(CwError::TransmitDisabled);
                }
                self.with_keyer(|keyer| {
                    keyer.set_speed(speed)?;
                    keyer.send_text(text)
                })?;
                self.active_speed_wpm = speed;
                self.watchdog_deadline =
                    Some(Instant::now() + Duration::from_millis(self.config.max_tx_ms));
                Ok(())
            }
            CwBackendKind::Cwdaemon => Err(CwError::BackendUnavailable(
                "cwdaemon backend is reserved but not implemented".to_string(),
            )),
        }
    }

    fn abort(&mut self) -> Result<(), CwError> {
        self.watchdog_deadline = None;
        match self.config.backend {
            CwBackendKind::Null => Ok(()),
            CwBackendKind::Winkeyer => self.with_keyer(|keyer| keyer.clear_buffer()),
            CwBackendKind::Cwdaemon => Err(CwError::BackendUnavailable(
                "cwdaemon backend is reserved but not implemented".to_string(),
            )),
        }
    }

    fn set_speed(&mut self, speed_wpm: u32) -> Result<u32, CwError> {
        validate_speed(speed_wpm)?;
        match self.config.backend {
            CwBackendKind::Null => {}
            CwBackendKind::Winkeyer => {
                self.with_keyer(|keyer| keyer.set_speed(speed_wpm))?;
            }
            CwBackendKind::Cwdaemon => {
                return Err(CwError::BackendUnavailable(
                    "cwdaemon backend is reserved but not implemented".to_string(),
                ));
            }
        }
        self.active_speed_wpm = speed_wpm;
        self.last_error = None;
        Ok(speed_wpm)
    }

    fn status(&mut self) -> CwKeyerStatus {
        let available = match self.config.backend {
            CwBackendKind::Null => true,
            CwBackendKind::Winkeyer if self.config.winkeyer_port.is_none() => {
                self.last_error = Some(format!("{CW_WINKEYER_PORT_ENV_VAR} is required"));
                false
            }
            CwBackendKind::Winkeyer => self.ensure_keyer().is_ok(),
            CwBackendKind::Cwdaemon => {
                self.last_error = Some("cwdaemon backend is not implemented".to_string());
                false
            }
        };
        CwKeyerStatus {
            backend: backend_to_proto(self.config.backend) as i32,
            available,
            speed_wpm: self.active_speed_wpm,
            port_name: self.config.winkeyer_port.clone(),
            error_message: self.last_error.clone(),
            transmit_enabled: self.config.transmit_enabled,
            max_tx_ms: self.config.max_tx_ms,
            firmware_revision: self.firmware_revision.map(u32::from),
        }
    }

    fn ensure_keyer(&mut self) -> Result<(), CwError> {
        if self.keyer.is_some() {
            return Ok(());
        }
        let port_name = self.config.winkeyer_port.clone().ok_or_else(|| {
            CwError::BackendUnavailable(format!("{CW_WINKEYER_PORT_ENV_VAR} is required"))
        })?;
        let mut keyer = match (self.factory)(&port_name, self.config.winkeyer_baud) {
            Ok(keyer) => keyer,
            Err(error) => {
                self.last_error = Some(error.to_string());
                return Err(error);
            }
        };
        match keyer.initialize() {
            Ok(revision) => {
                self.firmware_revision = Some(revision);
                self.last_error = None;
                self.keyer = Some(keyer);
                Ok(())
            }
            Err(error) => {
                self.last_error = Some(error.to_string());
                let _ = keyer.close();
                Err(error)
            }
        }
    }

    fn with_keyer<T>(
        &mut self,
        operation: impl FnOnce(&mut dyn WinkeyerDevice) -> Result<T, CwError>,
    ) -> Result<T, CwError> {
        self.ensure_keyer()?;
        let mut keyer = self
            .keyer
            .take()
            .ok_or_else(|| CwError::BackendUnavailable("WinKeyer is not connected".to_string()))?;
        let result = operation(keyer.as_mut());
        match result {
            Ok(value) => {
                self.last_error = None;
                self.keyer = Some(keyer);
                Ok(value)
            }
            Err(error) => {
                self.last_error = Some(error.to_string());
                let _ = keyer.close();
                self.firmware_revision = None;
                Err(error)
            }
        }
    }

    fn expire_watchdog(&mut self) {
        self.watchdog_deadline = None;
        if let Some(keyer) = self.keyer.as_deref_mut() {
            match keyer.is_busy() {
                Ok(false) => self.last_error = None,
                Ok(true) => {
                    if let Err(error) = keyer.clear_buffer() {
                        self.last_error = Some(error.to_string());
                        self.disconnect();
                    } else {
                        self.last_error = Some(format!(
                            "CW transmit safety ceiling reached after {} ms; WinKeyer buffer cleared",
                            self.config.max_tx_ms
                        ));
                    }
                }
                Err(error) => {
                    self.last_error = Some(error.to_string());
                    self.disconnect();
                }
            }
        }
    }

    fn disconnect(&mut self) {
        if let Some(mut keyer) = self.keyer.take() {
            let _ = keyer.close();
        }
        self.firmware_revision = None;
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

fn parse_u64_or_default(value: Option<&str>, default_value: u64) -> Result<u64, CwError> {
    match value.and_then(non_empty) {
        Some(value) => value.parse::<u64>().map_err(|error| {
            CwError::BackendUnavailable(format!(
                "invalid numeric CW configuration '{value}': {error}"
            ))
        }),
        None => Ok(default_value),
    }
}

fn parse_bool_or_default(value: Option<&str>, default_value: bool) -> Result<bool, CwError> {
    match value.map(str::trim).filter(|value| !value.is_empty()) {
        None => Ok(default_value),
        Some(value)
            if matches!(
                value.to_ascii_lowercase().as_str(),
                "true" | "1" | "yes" | "on"
            ) =>
        {
            Ok(true)
        }
        Some(value)
            if matches!(
                value.to_ascii_lowercase().as_str(),
                "false" | "0" | "no" | "off"
            ) =>
        {
            Ok(false)
        }
        Some(value) => Err(CwError::BackendUnavailable(format!(
            "invalid boolean CW configuration '{value}'"
        ))),
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

fn validate_speed(speed_wpm: u32) -> Result<(), CwError> {
    if (MIN_CW_SPEED_WPM..=MAX_CW_SPEED_WPM).contains(&speed_wpm) {
        Ok(())
    } else {
        Err(CwError::InvalidSpeed(speed_wpm))
    }
}

fn validate_text(text: &str) -> Result<(), CwError> {
    if text.is_empty() {
        return Err(CwError::InvalidText("text must not be empty".to_string()));
    }
    text.chars()
        .find(|character| !character.is_ascii())
        .map_or(Ok(()), |character| {
            Err(CwError::InvalidText(format!(
                "non-ASCII character '{character}'"
            )))
        })
}

struct WinkeyerPort {
    port: serial2::SerialPort,
    host_open: bool,
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
        Ok(Self {
            port,
            host_open: false,
        })
    }

    fn initialize_port(&mut self) -> Result<u8, CwError> {
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

        self.host_open = true;
        self.clear_buffer_port()?;
        Ok(version[0])
    }

    fn set_speed(&mut self, speed_wpm: u32) -> Result<(), CwError> {
        validate_speed(speed_wpm)?;
        let speed = u8::try_from(speed_wpm).map_err(|error| CwError::Io(error.to_string()))?;
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

    fn clear_buffer_port(&mut self) -> Result<(), CwError> {
        self.write_all(&[0x0A])
    }

    fn is_busy_port(&mut self) -> Result<bool, CwError> {
        self.port
            .discard_input_buffer()
            .map_err(|err| CwError::Io(format!("discard stale WinKeyer status: {err}")))?;
        self.write_all(&[0x15])?;
        let mut status = [0_u8; 1];
        loop {
            self.port
                .read_exact(&mut status)
                .map_err(|err| CwError::Io(format!("read WinKeyer status: {err}")))?;
            if status[0] & 0xE8 == 0xC0 {
                return Ok(status[0] & 0x04 != 0);
            }
        }
    }

    fn close_port(&mut self) -> Result<(), CwError> {
        if !self.host_open {
            return Ok(());
        }
        self.write_all(&[0x00, 0x03])?;
        self.host_open = false;
        Ok(())
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

impl Drop for WinkeyerPort {
    fn drop(&mut self) {
        let _ = self.close_port();
    }
}

impl WinkeyerDevice for WinkeyerPort {
    fn initialize(&mut self) -> Result<u8, CwError> {
        self.initialize_port()
    }

    fn set_speed(&mut self, speed_wpm: u32) -> Result<(), CwError> {
        Self::set_speed(self, speed_wpm)
    }

    fn send_text(&mut self, text: &str) -> Result<(), CwError> {
        Self::send_text(self, text)
    }

    fn clear_buffer(&mut self) -> Result<(), CwError> {
        self.clear_buffer_port()
    }

    fn is_busy(&mut self) -> Result<bool, CwError> {
        self.is_busy_port()
    }

    fn close(&mut self) -> Result<(), CwError> {
        self.close_port()
    }
}

fn open_winkeyer(port_name: &str, baud_rate: u32) -> Result<Box<dyn WinkeyerDevice>, CwError> {
    WinkeyerPort::open(port_name, baud_rate).map(|keyer| Box::new(keyer) as Box<dyn WinkeyerDevice>)
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
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct FakeState {
        opens: usize,
        initializes: usize,
        closes: usize,
        clears: usize,
        speeds: Vec<u32>,
        texts: Vec<String>,
        busy: bool,
    }

    struct FakeWinkeyer {
        state: Arc<Mutex<FakeState>>,
    }

    struct FailingInitWinkeyer {
        state: Arc<Mutex<FakeState>>,
    }

    impl WinkeyerDevice for FakeWinkeyer {
        fn initialize(&mut self) -> Result<u8, CwError> {
            self.state.lock().unwrap().initializes += 1;
            Ok(31)
        }

        fn set_speed(&mut self, speed_wpm: u32) -> Result<(), CwError> {
            self.state.lock().unwrap().speeds.push(speed_wpm);
            Ok(())
        }

        fn send_text(&mut self, text: &str) -> Result<(), CwError> {
            self.state.lock().unwrap().texts.push(text.to_string());
            Ok(())
        }

        fn clear_buffer(&mut self) -> Result<(), CwError> {
            self.state.lock().unwrap().clears += 1;
            Ok(())
        }

        fn is_busy(&mut self) -> Result<bool, CwError> {
            Ok(self.state.lock().unwrap().busy)
        }

        fn close(&mut self) -> Result<(), CwError> {
            self.state.lock().unwrap().closes += 1;
            Ok(())
        }
    }

    impl WinkeyerDevice for FailingInitWinkeyer {
        fn initialize(&mut self) -> Result<u8, CwError> {
            self.state.lock().unwrap().initializes += 1;
            Err(CwError::Io("initialization failed".to_string()))
        }

        fn set_speed(&mut self, _speed_wpm: u32) -> Result<(), CwError> {
            Ok(())
        }

        fn send_text(&mut self, _text: &str) -> Result<(), CwError> {
            Ok(())
        }

        fn clear_buffer(&mut self) -> Result<(), CwError> {
            Ok(())
        }

        fn is_busy(&mut self) -> Result<bool, CwError> {
            Ok(false)
        }

        fn close(&mut self) -> Result<(), CwError> {
            self.state.lock().unwrap().closes += 1;
            Ok(())
        }
    }

    fn null_config() -> CwKeyerConfig {
        CwKeyerConfig {
            backend: CwBackendKind::Null,
            winkeyer_port: None,
            winkeyer_baud: DEFAULT_WINKEYER_BAUD,
            default_speed_wpm: DEFAULT_CW_SPEED_WPM,
            transmit_enabled: false,
            max_tx_ms: DEFAULT_CW_MAX_TX_MS,
        }
    }

    fn fake_controller(
        transmit_enabled: bool,
        max_tx_ms: u64,
    ) -> (CwController, Arc<Mutex<FakeState>>) {
        let state = Arc::new(Mutex::new(FakeState::default()));
        state.lock().unwrap().busy = true;
        let factory_state = state.clone();
        let factory: WinkeyerFactory = Arc::new(move |_port, _baud| {
            factory_state.lock().unwrap().opens += 1;
            Ok(Box::new(FakeWinkeyer {
                state: factory_state.clone(),
            }))
        });
        let controller = CwController::with_factory(
            CwKeyerConfig {
                backend: CwBackendKind::Winkeyer,
                winkeyer_port: Some("FAKE".to_string()),
                winkeyer_baud: DEFAULT_WINKEYER_BAUD,
                default_speed_wpm: DEFAULT_CW_SPEED_WPM,
                transmit_enabled,
                max_tx_ms,
            },
            factory,
        );
        (controller, state)
    }

    fn station_profile() -> StationProfile {
        StationProfile {
            station_callsign: "K7ABC".to_string(),
            ..StationProfile::default()
        }
    }

    #[test]
    fn expands_built_in_exchange_macro() -> Result<(), CwError> {
        let controller = CwController::new(null_config());
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

    #[test]
    fn active_speed_persists_and_invalid_speeds_are_rejected() -> Result<(), CwError> {
        let controller = CwController::new(null_config());
        assert_eq!(controller.set_speed(32)?, 32);
        assert_eq!(controller.status().speed_wpm, 32);
        assert!(matches!(
            controller.set_speed(4),
            Err(CwError::InvalidSpeed(4))
        ));
        assert!(matches!(
            controller.set_speed(100),
            Err(CwError::InvalidSpeed(100))
        ));
        Ok(())
    }

    #[test]
    fn transmit_gate_prevents_hardware_open() {
        let (controller, state) = fake_controller(false, DEFAULT_CW_MAX_TX_MS);
        assert!(matches!(
            controller.send_text("TEST", None),
            Err(CwError::TransmitDisabled)
        ));
        assert_eq!(state.lock().unwrap().opens, 0);
    }

    #[test]
    fn persistent_session_reuses_connection_and_closes_on_shutdown() -> Result<(), CwError> {
        let (controller, state) = fake_controller(true, DEFAULT_CW_MAX_TX_MS);
        let status = controller.status();
        assert!(status.available);
        assert_eq!(status.firmware_revision, Some(31));
        controller.send_text("CQ", Some(28))?;
        controller.send_text("TU", None)?;
        controller.abort()?;
        {
            let state = state.lock().unwrap();
            assert_eq!(state.opens, 1);
            assert_eq!(state.initializes, 1);
            assert_eq!(state.texts, ["CQ", "TU"]);
            assert_eq!(state.speeds, [28, 28]);
            assert_eq!(state.clears, 1);
        }
        drop(controller);
        for _ in 0..20 {
            if state.lock().unwrap().closes == 1 {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(state.lock().unwrap().closes, 1);
        Ok(())
    }

    #[test]
    fn watchdog_clears_the_buffer_once() -> Result<(), CwError> {
        let (controller, state) = fake_controller(true, 20);
        controller.send_text("TEST", None)?;
        thread::sleep(Duration::from_millis(60));
        assert_eq!(state.lock().unwrap().clears, 1);
        assert!(controller
            .status()
            .error_message
            .is_some_and(|message| message.contains("safety ceiling")));
        Ok(())
    }

    #[test]
    fn watchdog_does_not_report_an_idle_keyer_as_timed_out() -> Result<(), CwError> {
        let (controller, state) = fake_controller(true, 20);
        controller.send_text("TEST", None)?;
        state.lock().unwrap().busy = false;
        thread::sleep(Duration::from_millis(60));

        assert_eq!(state.lock().unwrap().clears, 0);
        assert!(controller.status().error_message.is_none());
        Ok(())
    }

    #[test]
    fn status_reports_factory_failures() {
        let factory: WinkeyerFactory =
            Arc::new(|_port, _baud| Err(CwError::Io("serial port is busy".to_string())));
        let controller = CwController::with_factory(
            CwKeyerConfig {
                backend: CwBackendKind::Winkeyer,
                winkeyer_port: Some("FAKE".to_string()),
                winkeyer_baud: DEFAULT_WINKEYER_BAUD,
                default_speed_wpm: DEFAULT_CW_SPEED_WPM,
                transmit_enabled: true,
                max_tx_ms: DEFAULT_CW_MAX_TX_MS,
            },
            factory,
        );

        let status = controller.status();

        assert!(!status.available);
        assert!(status
            .error_message
            .is_some_and(|message| message.contains("serial port is busy")));
    }

    #[test]
    fn failed_initialization_closes_partial_session() {
        let state = Arc::new(Mutex::new(FakeState::default()));
        let factory_state = state.clone();
        let factory: WinkeyerFactory = Arc::new(move |_port, _baud| {
            factory_state.lock().unwrap().opens += 1;
            Ok(Box::new(FailingInitWinkeyer {
                state: factory_state.clone(),
            }))
        });
        let controller = CwController::with_factory(
            CwKeyerConfig {
                backend: CwBackendKind::Winkeyer,
                winkeyer_port: Some("FAKE".to_string()),
                winkeyer_baud: DEFAULT_WINKEYER_BAUD,
                default_speed_wpm: DEFAULT_CW_SPEED_WPM,
                transmit_enabled: true,
                max_tx_ms: DEFAULT_CW_MAX_TX_MS,
            },
            factory,
        );

        let status = controller.status();

        assert!(!status.available);
        assert_eq!(state.lock().unwrap().closes, 1);
    }

    #[test]
    fn configuration_requires_valid_safety_values() {
        assert!(matches!(
            CwKeyerConfig::from_values(None, None, None, Some("4"), None, None),
            Err(CwError::InvalidSpeed(4))
        ));
        assert!(CwKeyerConfig::from_values(
            Some("winkeyer"),
            Some("COM3".to_string()),
            None,
            None,
            Some("true"),
            Some("120000")
        )
        .is_ok());
    }
}

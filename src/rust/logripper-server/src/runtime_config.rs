use std::collections::BTreeMap;
use std::sync::Arc;

use logripper_core::application::logbook::LogbookEngine;
use logripper_core::lookup::{
    CallsignProvider, DisabledCallsignProvider, LookupCoordinator, LookupCoordinatorConfig,
    QrzXmlConfig, QrzXmlProvider, DEFAULT_QRZ_XML_BASE_URL, QRZ_HTTP_TIMEOUT_SECONDS_ENV_VAR,
    QRZ_MAX_RETRIES_ENV_VAR, QRZ_USER_AGENT_ENV_VAR, QRZ_XML_BASE_URL_ENV_VAR,
    QRZ_XML_CAPTURE_ONLY_ENV_VAR, QRZ_XML_PASSWORD_ENV_VAR, QRZ_XML_USERNAME_ENV_VAR,
};
use logripper_core::proto::logripper::services::{
    ApplyRuntimeConfigRequest, ResetRuntimeConfigRequest, RuntimeConfigDefinition,
    RuntimeConfigMutation, RuntimeConfigMutationKind, RuntimeConfigSnapshot, RuntimeConfigValue,
    RuntimeConfigValueKind,
};
use tokio::sync::RwLock;

use crate::{build_storage, parse_storage_backend, StorageBackendKind, StorageOptions};

pub(crate) const STORAGE_BACKEND_ENV_VAR: &str = "LOGRIPPER_STORAGE_BACKEND";
pub(crate) const SQLITE_PATH_ENV_VAR: &str = "LOGRIPPER_SQLITE_PATH";
const DEFAULT_STORAGE_BACKEND: &str = "memory";
const DEFAULT_SQLITE_PATH: &str = "logripper.db";
const REDACTED_VALUE: &str = "<redacted>";

#[derive(Clone)]
struct RuntimeBindings {
    logbook_engine: LogbookEngine,
    lookup_coordinator: Arc<LookupCoordinator>,
    active_storage_backend: String,
    lookup_provider_summary: String,
}

pub(crate) struct RuntimeConfigManager {
    base_values: BTreeMap<String, String>,
    overrides: RwLock<BTreeMap<String, String>>,
    bindings: RwLock<RuntimeBindings>,
}

impl RuntimeConfigManager {
    pub(crate) fn new_from_storage_options(storage: &StorageOptions) -> Result<Self, String> {
        let base_values = capture_supported_env();
        Self::new(seed_storage_values(base_values, storage))
    }

    pub(crate) fn new(base_values: BTreeMap<String, String>) -> Result<Self, String> {
        let bindings = build_runtime_bindings(&base_values)?;
        Ok(Self {
            base_values,
            overrides: RwLock::new(BTreeMap::new()),
            bindings: RwLock::new(bindings),
        })
    }

    pub(crate) async fn snapshot(&self) -> RuntimeConfigSnapshot {
        let overrides = self.overrides.read().await.clone();
        let bindings = self.bindings.read().await.clone();
        build_snapshot(&self.base_values, &overrides, &bindings)
    }

    pub(crate) async fn apply_request(
        &self,
        request: ApplyRuntimeConfigRequest,
    ) -> Result<RuntimeConfigSnapshot, String> {
        let mut next_overrides = self.overrides.read().await.clone();

        for mutation in request.mutations {
            apply_mutation(&mut next_overrides, mutation)?;
        }

        self.swap_runtime(next_overrides).await
    }

    pub(crate) async fn reset_request(
        &self,
        request: ResetRuntimeConfigRequest,
    ) -> Result<RuntimeConfigSnapshot, String> {
        let mut next_overrides = self.overrides.read().await.clone();

        if request.keys.is_empty() {
            next_overrides.clear();
        } else {
            for raw_key in request.keys {
                let key = canonical_key(&raw_key)?;
                next_overrides.remove(key);
            }
        }

        self.swap_runtime(next_overrides).await
    }

    pub(crate) async fn logbook_engine(&self) -> LogbookEngine {
        self.bindings.read().await.logbook_engine.clone()
    }

    pub(crate) async fn lookup_coordinator(&self) -> Arc<LookupCoordinator> {
        self.bindings.read().await.lookup_coordinator.clone()
    }

    pub(crate) async fn active_storage_backend(&self) -> String {
        self.bindings.read().await.active_storage_backend.clone()
    }

    async fn swap_runtime(
        &self,
        next_overrides: BTreeMap<String, String>,
    ) -> Result<RuntimeConfigSnapshot, String> {
        let merged = merge_values(&self.base_values, &next_overrides);
        let next_bindings = build_runtime_bindings(&merged)?;

        {
            let mut bindings = self.bindings.write().await;
            *bindings = next_bindings;
        }

        {
            let mut overrides = self.overrides.write().await;
            *overrides = next_overrides;
        }

        Ok(self.snapshot().await)
    }
}

struct ConfigFieldSpec {
    key: &'static str,
    label: &'static str,
    description: &'static str,
    kind: RuntimeConfigValueKind,
    secret: bool,
    allowed_values: &'static [&'static str],
    default_value: Option<&'static str>,
}

const STORAGE_ALLOWED_VALUES: &[&str] = &["memory", "sqlite"];

const SUPPORTED_FIELDS: &[ConfigFieldSpec] = &[
    ConfigFieldSpec {
        key: STORAGE_BACKEND_ENV_VAR,
        label: "Storage backend",
        description: "Hot-swap the active logbook storage implementation for new requests.",
        kind: RuntimeConfigValueKind::String,
        secret: false,
        allowed_values: STORAGE_ALLOWED_VALUES,
        default_value: Some(DEFAULT_STORAGE_BACKEND),
    },
    ConfigFieldSpec {
        key: SQLITE_PATH_ENV_VAR,
        label: "SQLite path",
        description: "SQLite database path used when the active storage backend is sqlite.",
        kind: RuntimeConfigValueKind::Path,
        secret: false,
        allowed_values: &[],
        default_value: Some(DEFAULT_SQLITE_PATH),
    },
    ConfigFieldSpec {
        key: QRZ_XML_USERNAME_ENV_VAR,
        label: "QRZ XML username",
        description: "QRZ XML login username for live callsign lookups.",
        kind: RuntimeConfigValueKind::String,
        secret: false,
        allowed_values: &[],
        default_value: None,
    },
    ConfigFieldSpec {
        key: QRZ_XML_PASSWORD_ENV_VAR,
        label: "QRZ XML password",
        description: "QRZ XML login password. The live snapshot always redacts this value.",
        kind: RuntimeConfigValueKind::String,
        secret: true,
        allowed_values: &[],
        default_value: None,
    },
    ConfigFieldSpec {
        key: QRZ_USER_AGENT_ENV_VAR,
        label: "QRZ user agent",
        description: "User agent string supplied to QRZ XML requests.",
        kind: RuntimeConfigValueKind::String,
        secret: false,
        allowed_values: &[],
        default_value: None,
    },
    ConfigFieldSpec {
        key: QRZ_XML_BASE_URL_ENV_VAR,
        label: "QRZ XML base URL",
        description: "QRZ XML endpoint used by the live lookup provider.",
        kind: RuntimeConfigValueKind::String,
        secret: false,
        allowed_values: &[],
        default_value: Some(DEFAULT_QRZ_XML_BASE_URL),
    },
    ConfigFieldSpec {
        key: QRZ_HTTP_TIMEOUT_SECONDS_ENV_VAR,
        label: "QRZ HTTP timeout seconds",
        description: "HTTP timeout used by live QRZ XML requests.",
        kind: RuntimeConfigValueKind::Integer,
        secret: false,
        allowed_values: &[],
        default_value: Some("8"),
    },
    ConfigFieldSpec {
        key: QRZ_MAX_RETRIES_ENV_VAR,
        label: "QRZ max retries",
        description: "Retry count for retryable QRZ XML transport failures.",
        kind: RuntimeConfigValueKind::Integer,
        secret: false,
        allowed_values: &[],
        default_value: Some("2"),
    },
    ConfigFieldSpec {
        key: QRZ_XML_CAPTURE_ONLY_ENV_VAR,
        label: "QRZ capture-only mode",
        description:
            "When true, capture and redact the outbound QRZ request instead of sending it.",
        kind: RuntimeConfigValueKind::Boolean,
        secret: false,
        allowed_values: &["true", "false"],
        default_value: Some("false"),
    },
];

fn capture_supported_env() -> BTreeMap<String, String> {
    let mut values = BTreeMap::new();

    for field in SUPPORTED_FIELDS {
        if let Ok(value) = std::env::var(field.key) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                values.insert(field.key.to_string(), trimmed.to_string());
            }
        }
    }

    values
}

fn seed_storage_values(
    mut base_values: BTreeMap<String, String>,
    storage: &StorageOptions,
) -> BTreeMap<String, String> {
    base_values.insert(
        STORAGE_BACKEND_ENV_VAR.to_string(),
        match storage.backend {
            StorageBackendKind::Memory => "memory".to_string(),
            StorageBackendKind::Sqlite => "sqlite".to_string(),
        },
    );
    base_values.insert(
        SQLITE_PATH_ENV_VAR.to_string(),
        storage.sqlite_path.display().to_string(),
    );
    base_values
}

fn apply_mutation(
    overrides: &mut BTreeMap<String, String>,
    mutation: RuntimeConfigMutation,
) -> Result<(), String> {
    let key = canonical_key(&mutation.key)?;
    let action = RuntimeConfigMutationKind::try_from(mutation.kind)
        .map_err(|_| format!("Unsupported mutation kind for '{key}'."))?;

    match action {
        RuntimeConfigMutationKind::Set => {
            let value = mutation
                .value
                .ok_or_else(|| format!("A value is required when setting '{key}'."))?;
            let normalized = normalize_value(key, &value)?;
            overrides.insert(key.to_string(), normalized);
        }
        RuntimeConfigMutationKind::Clear => {
            overrides.remove(key);
        }
        RuntimeConfigMutationKind::Unspecified => {
            return Err(format!("A mutation kind is required for '{key}'."));
        }
    }

    Ok(())
}

fn canonical_key(raw_key: &str) -> Result<&'static str, String> {
    let trimmed = raw_key.trim();
    SUPPORTED_FIELDS
        .iter()
        .find(|field| field.key.eq_ignore_ascii_case(trimmed))
        .map(|field| field.key)
        .ok_or_else(|| format!("Unsupported runtime config key '{trimmed}'."))
}

fn normalize_value(key: &'static str, raw_value: &str) -> Result<String, String> {
    let trimmed = raw_value.trim();
    if trimmed.is_empty() {
        return Err(format!("A non-empty value is required for '{key}'."));
    }

    if key == STORAGE_BACKEND_ENV_VAR {
        parse_storage_backend(trimmed)
            .map_err(|error| error.to_string())
            .map(|_| trimmed.to_ascii_lowercase())
    } else if key == QRZ_XML_CAPTURE_ONLY_ENV_VAR {
        parse_bool(trimmed).map(|value| {
            if value {
                "true".to_string()
            } else {
                "false".to_string()
            }
        })
    } else if key == QRZ_HTTP_TIMEOUT_SECONDS_ENV_VAR {
        trimmed
            .parse::<u64>()
            .map(|value| value.to_string())
            .map_err(|_| format!("'{key}' expects an integer value."))
    } else if key == QRZ_MAX_RETRIES_ENV_VAR {
        trimmed
            .parse::<u32>()
            .map(|value| value.to_string())
            .map_err(|_| format!("'{key}' expects an integer value."))
    } else {
        Ok(trimmed.to_string())
    }
}

fn parse_bool(raw_value: &str) -> Result<bool, String> {
    match raw_value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "y" | "on" => Ok(true),
        "0" | "false" | "no" | "n" | "off" => Ok(false),
        _ => Err(format!(
            "'{raw_value}' is not a valid boolean. Use true/false, yes/no, 1/0, y/n, or on/off."
        )),
    }
}

fn merge_values(
    base_values: &BTreeMap<String, String>,
    overrides: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut merged = base_values.clone();
    merged.extend(overrides.clone());
    merged
}

fn build_runtime_bindings(values: &BTreeMap<String, String>) -> Result<RuntimeBindings, String> {
    let storage = build_storage(
        &parse_storage_options_from_values(values).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    let logbook_engine = LogbookEngine::new(storage);
    let active_storage_backend = logbook_engine.storage_backend_name().to_string();
    let (provider, lookup_provider_summary) = build_lookup_provider(values);
    let lookup_coordinator = Arc::new(LookupCoordinator::new(
        provider,
        LookupCoordinatorConfig::default(),
    ));

    Ok(RuntimeBindings {
        logbook_engine,
        lookup_coordinator,
        active_storage_backend,
        lookup_provider_summary,
    })
}

fn parse_storage_options_from_values(
    values: &BTreeMap<String, String>,
) -> Result<StorageOptions, Box<dyn std::error::Error>> {
    let backend = parse_storage_backend(
        values
            .get(STORAGE_BACKEND_ENV_VAR)
            .map_or(DEFAULT_STORAGE_BACKEND, String::as_str),
    )?;
    let sqlite_path = values
        .get(SQLITE_PATH_ENV_VAR)
        .cloned()
        .unwrap_or_else(|| DEFAULT_SQLITE_PATH.to_string())
        .into();

    Ok(StorageOptions {
        backend,
        sqlite_path,
    })
}

fn build_lookup_provider(values: &BTreeMap<String, String>) -> (Arc<dyn CallsignProvider>, String) {
    match QrzXmlConfig::from_value_provider(|name| values.get(name).cloned()) {
        Ok(config) => match QrzXmlProvider::new(config.clone()) {
            Ok(provider) => {
                let summary = if config.capture_only() {
                    format!("QRZ XML capture-only via {}", config.base_url())
                } else {
                    format!("QRZ XML live via {}", config.base_url())
                };
                (Arc::new(provider), summary)
            }
            Err(error) => {
                let reason = error.to_string();
                (
                    Arc::new(DisabledCallsignProvider::new(reason.clone())),
                    format!("Disabled: {reason}"),
                )
            }
        },
        Err(error) => {
            let reason = error.to_string();
            (
                Arc::new(DisabledCallsignProvider::new(reason.clone())),
                format!("Disabled: {reason}"),
            )
        }
    }
}

fn build_snapshot(
    base_values: &BTreeMap<String, String>,
    overrides: &BTreeMap<String, String>,
    bindings: &RuntimeBindings,
) -> RuntimeConfigSnapshot {
    let merged = merge_values(base_values, overrides);
    let definitions = SUPPORTED_FIELDS
        .iter()
        .map(|field| RuntimeConfigDefinition {
            key: field.key.to_string(),
            label: field.label.to_string(),
            description: field.description.to_string(),
            kind: field.kind as i32,
            secret: field.secret,
            allowed_values: field
                .allowed_values
                .iter()
                .map(|value| (*value).to_string())
                .collect(),
        })
        .collect();
    let values = SUPPORTED_FIELDS
        .iter()
        .map(|field| build_value(field, &merged, overrides))
        .collect();
    let mut warnings = Vec::new();

    if overrides.contains_key(STORAGE_BACKEND_ENV_VAR)
        || overrides.contains_key(SQLITE_PATH_ENV_VAR)
    {
        warnings.push(
            "Switching storage backends swaps the active engine state; records are not migrated automatically."
                .to_string(),
        );
    }

    RuntimeConfigSnapshot {
        definitions,
        values,
        active_storage_backend: bindings.active_storage_backend.clone(),
        lookup_provider_summary: bindings.lookup_provider_summary.clone(),
        warnings,
    }
}

fn build_value(
    field: &ConfigFieldSpec,
    merged: &BTreeMap<String, String>,
    overrides: &BTreeMap<String, String>,
) -> RuntimeConfigValue {
    let effective_value = merged
        .get(field.key)
        .cloned()
        .or_else(|| field.default_value.map(str::to_string));
    let has_value = effective_value.is_some();
    let display_value = if field.secret && has_value {
        REDACTED_VALUE.to_string()
    } else {
        effective_value.unwrap_or_default()
    };

    RuntimeConfigValue {
        key: field.key.to_string(),
        has_value,
        display_value,
        overridden: overrides.contains_key(field.key),
        secret: field.secret,
        redacted: field.secret && has_value,
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::{SystemTime, UNIX_EPOCH};

    use logripper_core::proto::logripper::domain::{Band, Mode, QsoRecord};

    use super::{
        ApplyRuntimeConfigRequest, ResetRuntimeConfigRequest, RuntimeConfigManager,
        RuntimeConfigMutation, RuntimeConfigMutationKind, QRZ_USER_AGENT_ENV_VAR,
        QRZ_XML_CAPTURE_ONLY_ENV_VAR, QRZ_XML_PASSWORD_ENV_VAR, QRZ_XML_USERNAME_ENV_VAR,
        SQLITE_PATH_ENV_VAR, STORAGE_BACKEND_ENV_VAR,
    };

    fn sample_qso(local_id: &str) -> QsoRecord {
        QsoRecord {
            local_id: local_id.to_string(),
            station_callsign: "K7DBG".to_string(),
            worked_callsign: "W1AW".to_string(),
            band: Band::Band20m as i32,
            mode: Mode::Ssb as i32,
            ..QsoRecord::default()
        }
    }

    fn unique_sqlite_path() -> String {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        std::env::temp_dir()
            .join(format!(
                "logripper-runtime-config-{}-{suffix}.db",
                std::process::id()
            ))
            .display()
            .to_string()
    }

    #[tokio::test]
    async fn runtime_manager_hot_swaps_storage_backends() {
        let manager = RuntimeConfigManager::new(BTreeMap::new()).expect("manager");
        let initial_snapshot = manager.snapshot().await;
        assert_eq!("memory", initial_snapshot.active_storage_backend);

        let sqlite_path = unique_sqlite_path();
        let sqlite_snapshot = manager
            .apply_request(ApplyRuntimeConfigRequest {
                mutations: vec![
                    RuntimeConfigMutation {
                        key: STORAGE_BACKEND_ENV_VAR.to_string(),
                        kind: RuntimeConfigMutationKind::Set as i32,
                        value: Some("sqlite".to_string()),
                    },
                    RuntimeConfigMutation {
                        key: SQLITE_PATH_ENV_VAR.to_string(),
                        kind: RuntimeConfigMutationKind::Set as i32,
                        value: Some(sqlite_path.clone()),
                    },
                ],
            })
            .await
            .expect("sqlite apply");
        assert_eq!("sqlite", sqlite_snapshot.active_storage_backend);

        let sqlite_engine = manager.logbook_engine().await;
        sqlite_engine
            .log_qso(sample_qso("sqlite-record"))
            .await
            .expect("sqlite write");
        drop(sqlite_engine);

        let memory_snapshot = manager
            .apply_request(ApplyRuntimeConfigRequest {
                mutations: vec![RuntimeConfigMutation {
                    key: STORAGE_BACKEND_ENV_VAR.to_string(),
                    kind: RuntimeConfigMutationKind::Set as i32,
                    value: Some("memory".to_string()),
                }],
            })
            .await
            .expect("memory apply");
        assert_eq!("memory", memory_snapshot.active_storage_backend);

        let memory_status = manager
            .logbook_engine()
            .await
            .get_sync_status()
            .await
            .expect("memory status");
        assert_eq!(0, memory_status.local_qso_count);

        let reset_snapshot = manager
            .reset_request(ResetRuntimeConfigRequest {
                keys: vec![
                    STORAGE_BACKEND_ENV_VAR.to_string(),
                    SQLITE_PATH_ENV_VAR.to_string(),
                ],
            })
            .await
            .expect("reset");
        assert_eq!("memory", reset_snapshot.active_storage_backend);
        drop(manager);

        let sqlite_file = std::path::PathBuf::from(sqlite_path);
        if sqlite_file.exists() {
            std::fs::remove_file(sqlite_file).expect("remove sqlite file");
        }
    }

    #[tokio::test]
    async fn runtime_manager_updates_lookup_provider_summary_when_capture_only_changes() {
        let mut base_values = BTreeMap::new();
        base_values.insert(QRZ_XML_USERNAME_ENV_VAR.to_string(), "KC7AVA".to_string());
        base_values.insert(
            QRZ_XML_PASSWORD_ENV_VAR.to_string(),
            "super-secret-password".to_string(),
        );
        base_values.insert(
            QRZ_USER_AGENT_ENV_VAR.to_string(),
            "LogRipper/0.1.0 (KC7AVA)".to_string(),
        );

        let manager = RuntimeConfigManager::new(base_values).expect("manager");
        let initial_summary = manager.snapshot().await.lookup_provider_summary;
        assert_contains(&initial_summary, "QRZ XML live");

        let capture_only_summary = manager
            .apply_request(ApplyRuntimeConfigRequest {
                mutations: vec![RuntimeConfigMutation {
                    key: QRZ_XML_CAPTURE_ONLY_ENV_VAR.to_string(),
                    kind: RuntimeConfigMutationKind::Set as i32,
                    value: Some("true".to_string()),
                }],
            })
            .await
            .expect("capture-only apply")
            .lookup_provider_summary;
        assert_contains(&capture_only_summary, "capture-only");
    }

    fn assert_contains(actual: &str, expected: &str) {
        assert!(
            actual.contains(expected),
            "expected '{actual}' to contain '{expected}'"
        );
    }
}

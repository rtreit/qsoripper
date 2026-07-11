use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use qsoripper_core::cw::{
    CW_CATHUB_CLIENT_NAME_ENV_VAR, CW_CATHUB_ENDPOINT_ENV_VAR, CW_KEYER_BACKEND_ENV_VAR,
    CW_MAX_TX_MS_ENV_VAR, CW_SPEED_WPM_ENV_VAR, CW_TRANSMIT_ENABLED_ENV_VAR,
    CW_WINKEYER_BAUD_ENV_VAR, CW_WINKEYER_PORT_ENV_VAR,
};
use qsoripper_core::domain::lookup::normalize_callsign;
use qsoripper_core::domain::station::station_profile_has_values;
use qsoripper_core::lookup::{
    QrzXmlConfig, QrzXmlProvider, QRZ_USER_AGENT_ENV_VAR, QRZ_XML_PASSWORD_ENV_VAR,
    QRZ_XML_USERNAME_ENV_VAR,
};
use qsoripper_core::proto::qsoripper::domain::{ConflictPolicy, StationProfile, SyncConfig};
use qsoripper_core::proto::qsoripper::services::{
    setup_service_server::SetupService, station_profile_service_server::StationProfileService,
    ActiveStationContext, CatHubEventSettings, CatHubHamlibNetEndpoint, CatHubPermission,
    CatHubPollSettings, CatHubPttSettings, CatHubRadioSettings, CatHubSerialFace, CatHubSettings,
    CatHubWinkeyerFace, CatHubWinkeyerSettings, ClearSessionStationProfileOverrideRequest,
    ClearSessionStationProfileOverrideResponse, DeleteStationProfileRequest,
    DeleteStationProfileResponse, GetActiveStationContextRequest, GetActiveStationContextResponse,
    GetSetupStatusRequest, GetSetupStatusResponse, GetSetupWizardStateRequest,
    GetSetupWizardStateResponse, GetStationProfileRequest, GetStationProfileResponse,
    ListStationProfilesRequest, ListStationProfilesResponse, RigControlSettings,
    RuntimeConfigDefinition, RuntimeConfigValue, SaveSetupRequest, SaveSetupResponse,
    SaveStationProfileRequest, SaveStationProfileResponse, SetActiveStationProfileRequest,
    SetActiveStationProfileResponse, SetSessionStationProfileOverrideRequest,
    SetSessionStationProfileOverrideResponse, SetupFieldValidation, SetupStatus, SetupWizardStep,
    SetupWizardStepStatus, StationProfileRecord, StorageBackend, TestQrzCredentialsRequest,
    TestQrzCredentialsResponse, TestQrzLogbookCredentialsRequest,
    TestQrzLogbookCredentialsResponse, ValidateSetupStepRequest, ValidateSetupStepResponse,
    WinkeyerFacePermission, WsjtxIngestSettings, WsjtxIngestStatus,
};
use qsoripper_core::qrz_logbook::{QrzLogbookClient, QrzLogbookConfig};
use qsoripper_core::rig_control::{
    RIGCTLD_ENABLED_ENV_VAR, RIGCTLD_HOST_ENV_VAR, RIGCTLD_PORT_ENV_VAR,
    RIGCTLD_READ_TIMEOUT_MS_ENV_VAR, RIGCTLD_STALE_THRESHOLD_MS_ENV_VAR,
};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, RwLock};
use tonic::{Request, Response, Status};

use crate::runtime_config::{
    RuntimeConfigManager, DEFAULT_QRZ_LOGBOOK_BASE_URL, QRZ_LOGBOOK_API_KEY_ENV_VAR,
    QRZ_LOGBOOK_BASE_URL_ENV_VAR, SQLITE_PATH_ENV_VAR, STORAGE_BACKEND_ENV_VAR,
    SYNC_AUTO_ENABLED_ENV_VAR, SYNC_CONFLICT_POLICY_ENV_VAR, SYNC_INTERVAL_SECONDS_ENV_VAR,
    WSJTX_INGEST_ADIF_TAIL_ENABLED_ENV_VAR, WSJTX_INGEST_ADIF_TAIL_PATH_ENV_VAR,
    WSJTX_INGEST_ENABLED_ENV_VAR, WSJTX_INGEST_POLL_INTERVAL_MS_ENV_VAR,
    WSJTX_INGEST_SYNC_TO_QRZ_ENV_VAR, WSJTX_INGEST_UDP_BIND_ENV_VAR,
    WSJTX_INGEST_UDP_ENABLED_ENV_VAR,
};
use crate::station_profile_support::{
    insert_station_profile_runtime_values, normalize_station_profile as normalize_profile_payload,
    DEFAULT_PROFILE_NAME,
};

pub(crate) const CONFIG_PATH_ENV_VAR: &str = "QSORIPPER_CONFIG_PATH";
const DEFAULT_CONFIG_FILE_NAME: &str = "config.toml";
const DEFAULT_LOG_FILE_NAME: &str = "qsoripper.db";
const PERSISTENCE_PATH_KEY: &str = "persistence.path";
const PERSISTENCE_STEP_DESCRIPTION: &str =
    "Choose where QsoRipper stores the local logbook used by this engine.";
const PERSISTENCE_STEP_LABEL: &str = "Log storage";

#[derive(Clone)]
pub(crate) struct SetupControlSurface {
    state: Arc<SetupState>,
    runtime_config: Arc<RuntimeConfigManager>,
    wsjtx_ingest_status: Option<Arc<Mutex<WsjtxIngestStatus>>>,
}

impl SetupControlSurface {
    pub(crate) fn new(state: Arc<SetupState>, runtime_config: Arc<RuntimeConfigManager>) -> Self {
        Self {
            state,
            runtime_config,
            wsjtx_ingest_status: None,
        }
    }

    pub(crate) fn with_wsjtx_ingest_status(
        mut self,
        status: Arc<Mutex<WsjtxIngestStatus>>,
    ) -> Self {
        self.wsjtx_ingest_status = Some(status);
        self
    }

    async fn attach_wsjtx_status(&self, mut status: SetupStatus) -> SetupStatus {
        if let Some(wsjtx_status) = &self.wsjtx_ingest_status {
            status.wsjtx_ingest_status = Some(wsjtx_status.lock().await.clone());
        }
        status
    }
}

#[derive(Clone)]
pub(crate) struct StationProfileControlSurface {
    state: Arc<SetupState>,
    runtime_config: Arc<RuntimeConfigManager>,
}

impl StationProfileControlSurface {
    pub(crate) fn new(state: Arc<SetupState>, runtime_config: Arc<RuntimeConfigManager>) -> Self {
        Self {
            state,
            runtime_config,
        }
    }
}

#[tonic::async_trait]
impl SetupService for SetupControlSurface {
    async fn get_setup_status(
        &self,
        _request: Request<GetSetupStatusRequest>,
    ) -> Result<Response<GetSetupStatusResponse>, Status> {
        let status = self.attach_wsjtx_status(self.state.status().await).await;
        Ok(Response::new(GetSetupStatusResponse {
            status: Some(status),
        }))
    }

    async fn save_setup(
        &self,
        request: Request<SaveSetupRequest>,
    ) -> Result<Response<SaveSetupResponse>, Status> {
        let status = self
            .state
            .save_setup(request.into_inner(), &self.runtime_config)
            .await
            .map_err(Status::invalid_argument)?;
        let status = self.attach_wsjtx_status(status).await;
        Ok(Response::new(SaveSetupResponse {
            status: Some(status),
        }))
    }

    async fn get_setup_wizard_state(
        &self,
        _request: Request<GetSetupWizardStateRequest>,
    ) -> Result<Response<GetSetupWizardStateResponse>, Status> {
        Ok(Response::new(self.state.wizard_state().await))
    }

    async fn validate_setup_step(
        &self,
        request: Request<ValidateSetupStepRequest>,
    ) -> Result<Response<ValidateSetupStepResponse>, Status> {
        let inner = request.into_inner();
        let step = SetupWizardStep::try_from(inner.step).unwrap_or(SetupWizardStep::Unspecified);
        Ok(Response::new(validate_step(step, &inner)))
    }

    async fn test_qrz_credentials(
        &self,
        request: Request<TestQrzCredentialsRequest>,
    ) -> Result<Response<TestQrzCredentialsResponse>, Status> {
        let inner = request.into_inner();
        let result = test_qrz_login(
            &inner.qrz_xml_username,
            &inner.qrz_xml_password,
            &self.runtime_config,
        )
        .await;
        Ok(Response::new(result))
    }

    async fn test_qrz_logbook_credentials(
        &self,
        request: Request<TestQrzLogbookCredentialsRequest>,
    ) -> Result<Response<TestQrzLogbookCredentialsResponse>, Status> {
        let inner = request.into_inner();
        let result = test_qrz_logbook_api_key(&inner.api_key, &self.runtime_config).await;
        Ok(Response::new(result))
    }
}

#[tonic::async_trait]
impl StationProfileService for StationProfileControlSurface {
    async fn list_station_profiles(
        &self,
        _request: Request<ListStationProfilesRequest>,
    ) -> Result<Response<ListStationProfilesResponse>, Status> {
        Ok(Response::new(self.state.list_station_profiles().await))
    }

    async fn get_station_profile(
        &self,
        request: Request<GetStationProfileRequest>,
    ) -> Result<Response<GetStationProfileResponse>, Status> {
        let response = self
            .state
            .get_station_profile(request.into_inner())
            .await
            .map_err(Status::not_found)?;
        Ok(Response::new(response))
    }

    async fn save_station_profile(
        &self,
        request: Request<SaveStationProfileRequest>,
    ) -> Result<Response<SaveStationProfileResponse>, Status> {
        let response = self
            .state
            .save_station_profile(request.into_inner(), &self.runtime_config)
            .await
            .map_err(Status::invalid_argument)?;
        Ok(Response::new(response))
    }

    async fn delete_station_profile(
        &self,
        request: Request<DeleteStationProfileRequest>,
    ) -> Result<Response<DeleteStationProfileResponse>, Status> {
        let response = self
            .state
            .delete_station_profile(request.into_inner(), &self.runtime_config)
            .await
            .map_err(Status::invalid_argument)?;
        Ok(Response::new(response))
    }

    async fn set_active_station_profile(
        &self,
        request: Request<SetActiveStationProfileRequest>,
    ) -> Result<Response<SetActiveStationProfileResponse>, Status> {
        let response = self
            .state
            .set_active_station_profile(request.into_inner(), &self.runtime_config)
            .await
            .map_err(Status::invalid_argument)?;
        Ok(Response::new(response))
    }

    async fn get_active_station_context(
        &self,
        _request: Request<GetActiveStationContextRequest>,
    ) -> Result<Response<GetActiveStationContextResponse>, Status> {
        let context = self
            .state
            .active_station_context(&self.runtime_config)
            .await;
        Ok(Response::new(GetActiveStationContextResponse {
            context: Some(context),
        }))
    }

    async fn set_session_station_profile_override(
        &self,
        request: Request<SetSessionStationProfileOverrideRequest>,
    ) -> Result<Response<SetSessionStationProfileOverrideResponse>, Status> {
        let context = self
            .state
            .set_session_station_profile_override(request.into_inner(), &self.runtime_config)
            .await
            .map_err(Status::invalid_argument)?;
        Ok(Response::new(SetSessionStationProfileOverrideResponse {
            context: Some(context),
        }))
    }

    async fn clear_session_station_profile_override(
        &self,
        _request: Request<ClearSessionStationProfileOverrideRequest>,
    ) -> Result<Response<ClearSessionStationProfileOverrideResponse>, Status> {
        let context = self
            .state
            .clear_session_station_profile_override(&self.runtime_config)
            .await
            .map_err(Status::invalid_argument)?;
        Ok(Response::new(ClearSessionStationProfileOverrideResponse {
            context: Some(context),
        }))
    }
}

pub(crate) struct SetupState {
    config_path: PathBuf,
    suggested_log_file_path: PathBuf,
    persisted_config: RwLock<Option<PersistedSetupConfig>>,
}

impl SetupState {
    pub(crate) fn load(config_path: PathBuf) -> Result<Self, String> {
        let persisted_config = load_persisted_config(&config_path)?;
        Ok(Self {
            suggested_log_file_path: suggested_log_file_path(&config_path),
            config_path,
            persisted_config: RwLock::new(persisted_config),
        })
    }

    pub(crate) async fn runtime_config_values(&self) -> BTreeMap<String, String> {
        self.persisted_config
            .read()
            .await
            .as_ref()
            .map_or_else(BTreeMap::new, PersistedSetupConfig::to_runtime_values)
    }

    pub(crate) async fn status(&self) -> SetupStatus {
        let persisted_config = self.persisted_config.read().await.clone();
        let cat_hub = load_cat_hub_config(self.config_path.as_path());
        build_status(
            self.config_path.as_path(),
            self.suggested_log_file_path.as_path(),
            persisted_config.as_ref(),
            cat_hub.as_ref(),
        )
    }

    async fn wizard_state(&self) -> GetSetupWizardStateResponse {
        let persisted_config = self.persisted_config.read().await.clone();
        let cat_hub = load_cat_hub_config(self.config_path.as_path());
        let status = build_status(
            self.config_path.as_path(),
            self.suggested_log_file_path.as_path(),
            persisted_config.as_ref(),
            cat_hub.as_ref(),
        );
        let steps = build_wizard_steps(persisted_config.as_ref(), cat_hub.as_ref());
        let station_profiles = persisted_config
            .as_ref()
            .map_or_else(Vec::new, PersistedSetupConfig::list_station_profile_records);
        GetSetupWizardStateResponse {
            status: Some(status),
            steps,
            station_profiles,
        }
    }

    async fn save_setup(
        &self,
        request: SaveSetupRequest,
        runtime_config: &RuntimeConfigManager,
    ) -> Result<SetupStatus, String> {
        let existing_config = self.persisted_config.read().await.clone();
        let config = PersistedSetupConfig::from_request(
            existing_config.as_ref(),
            &request,
            self.suggested_log_file_path.as_path(),
        )?;
        let runtime_values = config.to_runtime_values();
        runtime_config
            .preview_config_file_values(runtime_values.clone())
            .await?;

        let cat_hub_update = request
            .cat_hub
            .as_ref()
            .map(PersistedCatHubConfig::from_proto)
            .transpose()?;
        let wsjtx_ingest_update = request.wsjtx_ingest.as_ref().map(|_| &config.wsjtx_ingest);
        write_persisted_config(
            self.config_path.as_path(),
            &config,
            cat_hub_update.as_ref(),
            wsjtx_ingest_update,
        )?;

        {
            let mut persisted_config = self.persisted_config.write().await;
            *persisted_config = Some(config);
        }

        runtime_config
            .replace_config_file_values(runtime_values)
            .await?;
        Ok(self.status().await)
    }

    async fn list_station_profiles(&self) -> ListStationProfilesResponse {
        let persisted_config = self.persisted_config.read().await.clone();
        ListStationProfilesResponse {
            profiles: persisted_config
                .as_ref()
                .map_or_else(Vec::new, PersistedSetupConfig::list_station_profile_records),
            active_profile_id: persisted_config
                .as_ref()
                .and_then(PersistedSetupConfig::active_station_profile_id),
        }
    }

    async fn get_station_profile(
        &self,
        request: GetStationProfileRequest,
    ) -> Result<GetStationProfileResponse, String> {
        let profile_id = normalize_profile_id(&request.profile_id);
        let persisted_config = self.persisted_config.read().await.clone();
        let profile = persisted_config
            .as_ref()
            .and_then(|config| config.get_station_profile_record(&profile_id))
            .ok_or_else(|| format!("Station profile '{profile_id}' was not found."))?;
        Ok(GetStationProfileResponse {
            profile: Some(profile),
        })
    }

    async fn save_station_profile(
        &self,
        request: SaveStationProfileRequest,
        runtime_config: &RuntimeConfigManager,
    ) -> Result<SaveStationProfileResponse, String> {
        let profile = normalize_profile_payload(
            request
                .profile
                .ok_or_else(|| "SaveStationProfile requires a profile payload.".to_string())?,
            normalize_optional_callsign,
            normalize_optional_string,
        )?;
        let profile_id = self
            .mutate_persisted_config(runtime_config, |config| {
                Ok(config.save_station_profile(
                    request.profile_id.as_deref(),
                    &profile,
                    request.make_active,
                ))
            })
            .await?;
        let persisted_config = self.persisted_config.read().await.clone();
        let saved = persisted_config
            .as_ref()
            .and_then(|config| config.get_station_profile_record(&profile_id))
            .ok_or_else(|| format!("Station profile '{profile_id}' was not found after save."))?;
        Ok(SaveStationProfileResponse {
            profile: Some(saved),
            active_profile_id: persisted_config
                .as_ref()
                .and_then(PersistedSetupConfig::active_station_profile_id),
        })
    }

    async fn delete_station_profile(
        &self,
        request: DeleteStationProfileRequest,
        runtime_config: &RuntimeConfigManager,
    ) -> Result<DeleteStationProfileResponse, String> {
        self.mutate_persisted_config(runtime_config, |config| {
            config.delete_station_profile(&request.profile_id)
        })
        .await?;
        let persisted_config = self.persisted_config.read().await.clone();
        Ok(DeleteStationProfileResponse {
            active_profile_id: persisted_config
                .as_ref()
                .and_then(PersistedSetupConfig::active_station_profile_id),
        })
    }

    async fn set_active_station_profile(
        &self,
        request: SetActiveStationProfileRequest,
        runtime_config: &RuntimeConfigManager,
    ) -> Result<SetActiveStationProfileResponse, String> {
        let profile_id = normalize_profile_id(&request.profile_id);
        self.mutate_persisted_config(runtime_config, |config| {
            config.set_active_station_profile(&profile_id)
        })
        .await?;
        let persisted_config = self.persisted_config.read().await.clone();
        let active = persisted_config
            .as_ref()
            .and_then(|config| config.get_station_profile_record(&profile_id))
            .ok_or_else(|| {
                format!("Station profile '{profile_id}' was not found after activation.")
            })?;
        Ok(SetActiveStationProfileResponse {
            profile: Some(active),
        })
    }

    async fn active_station_context(
        &self,
        runtime_config: &RuntimeConfigManager,
    ) -> ActiveStationContext {
        let persisted_config = self.persisted_config.read().await.clone();
        let session_override_profile = runtime_config.session_station_profile_override().await;
        let effective_active_profile = runtime_config.effective_station_profile().await;
        let mut warnings = Vec::new();
        if persisted_config.is_none() {
            warnings.push("Persisted setup does not exist yet.".to_string());
        }
        if session_override_profile.is_some() {
            warnings.push(
                "A process-session station override is active for new QSO saves.".to_string(),
            );
        }

        ActiveStationContext {
            persisted_active_profile_id: persisted_config
                .as_ref()
                .and_then(PersistedSetupConfig::active_station_profile_id),
            persisted_active_profile: persisted_config
                .as_ref()
                .and_then(PersistedSetupConfig::station_profile),
            effective_active_profile,
            has_session_override: session_override_profile.is_some(),
            session_override_profile,
            warnings,
        }
    }

    async fn set_session_station_profile_override(
        &self,
        request: SetSessionStationProfileOverrideRequest,
        runtime_config: &RuntimeConfigManager,
    ) -> Result<ActiveStationContext, String> {
        let profile = normalize_profile_payload(
            request.profile.ok_or_else(|| {
                "SetSessionStationProfileOverride requires a profile payload.".to_string()
            })?,
            normalize_optional_callsign,
            normalize_optional_string,
        )?;
        runtime_config
            .set_session_station_profile_override(Some(profile))
            .await?;
        Ok(self.active_station_context(runtime_config).await)
    }

    async fn clear_session_station_profile_override(
        &self,
        runtime_config: &RuntimeConfigManager,
    ) -> Result<ActiveStationContext, String> {
        runtime_config
            .set_session_station_profile_override(None)
            .await?;
        Ok(self.active_station_context(runtime_config).await)
    }

    async fn mutate_persisted_config<T>(
        &self,
        runtime_config: &RuntimeConfigManager,
        mutate: impl FnOnce(&mut PersistedSetupConfig) -> Result<T, String>,
    ) -> Result<T, String> {
        let current = self.persisted_config.read().await.clone().ok_or_else(|| {
            "Persisted setup does not exist yet. Run SaveSetup first.".to_string()
        })?;
        let mut next = current;
        let result = mutate(&mut next)?;
        let runtime_values = next.to_runtime_values();
        runtime_config
            .preview_config_file_values(runtime_values.clone())
            .await?;
        write_persisted_config(self.config_path.as_path(), &next, None, None)?;
        {
            let mut persisted_config = self.persisted_config.write().await;
            *persisted_config = Some(next);
        }
        runtime_config
            .replace_config_file_values(runtime_values)
            .await?;
        Ok(result)
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct PersistedSetupConfig {
    #[serde(default, skip_serializing_if = "PersistedLogbookConfig::is_empty")]
    logbook: PersistedLogbookConfig,
    #[serde(default, skip_serializing_if = "PersistedStorageConfig::is_empty")]
    storage: PersistedStorageConfig,
    #[serde(default)]
    station_profile: PersistedStationProfile,
    #[serde(default)]
    station_profiles: PersistedStationProfileCatalog,
    #[serde(default)]
    qrz_xml: PersistedQrzXmlConfig,
    #[serde(default, skip_serializing_if = "PersistedQrzLogbookConfig::is_empty")]
    qrz_logbook: PersistedQrzLogbookConfig,
    #[serde(default, skip_serializing_if = "PersistedSyncConfig::is_empty")]
    sync: PersistedSyncConfig,
    #[serde(default, skip_serializing_if = "PersistedRigControlConfig::is_empty")]
    rig_control: PersistedRigControlConfig,
    #[serde(default, skip_serializing_if = "PersistedWsjtxIngestConfig::is_empty")]
    wsjtx_ingest: PersistedWsjtxIngestConfig,
    #[serde(default, skip_serializing_if = "PersistedCwKeyingConfig::is_empty")]
    cw_keying: PersistedCwKeyingConfig,
}

impl PersistedSetupConfig {
    fn from_request(
        existing: Option<&Self>,
        request: &SaveSetupRequest,
        suggested_log_file_path: &Path,
    ) -> Result<Self, String> {
        let station_profile = normalize_profile_payload(
            request
                .station_profile
                .clone()
                .ok_or_else(|| "SaveSetup requires a station_profile payload.".to_string())?,
            normalize_optional_callsign,
            normalize_optional_string,
        )?;
        let qrz_xml_username = normalize_optional_string(request.qrz_xml_username.as_deref());
        let qrz_xml_password = normalize_optional_string(request.qrz_xml_password.as_deref());

        let requested_log_file_path = setup_request_persistence_path(request)
            .or_else(|| normalize_optional_string(request.log_file_path.as_deref()));
        #[allow(deprecated)]
        let legacy_sqlite_path = normalize_optional_string(request.sqlite_path.as_deref());
        #[allow(deprecated)]
        let legacy_storage_backend = StorageBackend::try_from(request.storage_backend)
            .unwrap_or(StorageBackend::Unspecified);
        let (logbook, storage) = if let Some(log_file_path) = requested_log_file_path {
            (
                PersistedLogbookConfig {
                    file_path: Some(log_file_path),
                },
                PersistedStorageConfig::default(),
            )
        } else if matches!(legacy_storage_backend, StorageBackend::Memory) {
            (
                PersistedLogbookConfig::default(),
                PersistedStorageConfig {
                    backend: Some("memory".to_string()),
                    sqlite_path: None,
                },
            )
        } else if matches!(legacy_storage_backend, StorageBackend::Sqlite)
            || legacy_sqlite_path.is_some()
        {
            (
                PersistedLogbookConfig {
                    file_path: Some(
                        legacy_sqlite_path
                            .unwrap_or_else(|| suggested_log_file_path.display().to_string()),
                    ),
                },
                PersistedStorageConfig::default(),
            )
        } else {
            return Err("A log_file_path is required.".to_string());
        };

        let mut station_profiles = existing
            .map(|config| config.station_profiles.clone())
            .unwrap_or_default();
        let legacy_profile = existing
            .map(|config| config.station_profile.clone())
            .unwrap_or_default();
        let active_profile_id = existing.and_then(PersistedSetupConfig::active_station_profile_id);
        station_profiles.save_profile(
            active_profile_id.as_deref(),
            &station_profile,
            true,
            &legacy_profile,
        );

        let mut config = existing.cloned().unwrap_or_default();
        config.logbook = logbook;
        config.storage = storage;
        config.station_profile = PersistedStationProfile::from_proto(&station_profile);
        config.station_profiles = station_profiles;
        config.qrz_xml = PersistedQrzXmlConfig {
            username: qrz_xml_username,
            password: if qrz_xml_password.is_some() {
                qrz_xml_password
            } else {
                existing.and_then(|c| c.qrz_xml.password.clone())
            },
            // Preserve any existing user_agent; the setup wizard does not set it
            // directly, and runtime derives a default from the username when absent.
            user_agent: existing.and_then(|c| c.qrz_xml.user_agent.clone()),
        };

        // QRZ logbook API key: update when explicitly provided, otherwise keep existing.
        let qrz_logbook_api_key = normalize_optional_string(request.qrz_logbook_api_key.as_deref());
        if qrz_logbook_api_key.is_some() {
            config.qrz_logbook.api_key = qrz_logbook_api_key;
        }

        // Sync config: update when explicitly provided, otherwise keep existing.
        if let Some(ref sync_config) = request.sync_config {
            config.sync = PersistedSyncConfig::from_proto(sync_config);
        }

        // Rig control config: update when explicitly provided, otherwise keep existing.
        if let Some(ref rig_control) = request.rig_control {
            config.rig_control = PersistedRigControlConfig::from_proto(rig_control)?;
        }
        if let Some(ref wsjtx_ingest) = request.wsjtx_ingest {
            config.wsjtx_ingest = PersistedWsjtxIngestConfig::from_proto(wsjtx_ingest)?;
        }

        config.sync_active_station_profile();

        Ok(config)
    }

    fn to_runtime_values(&self) -> BTreeMap<String, String> {
        let mut values = BTreeMap::new();
        let log_file_path = self.log_file_path();

        match self.runtime_storage_backend() {
            StorageBackend::Memory => {
                values.insert(STORAGE_BACKEND_ENV_VAR.to_string(), "memory".to_string());
            }
            StorageBackend::Sqlite => {
                values.insert(STORAGE_BACKEND_ENV_VAR.to_string(), "sqlite".to_string());
                if let Some(log_file_path) = log_file_path {
                    values.insert(SQLITE_PATH_ENV_VAR.to_string(), log_file_path);
                }
            }
            StorageBackend::Unspecified => {}
        }

        if let Some(profile) = self.station_profile() {
            insert_station_profile_runtime_values(&mut values, &profile);
        }

        if let Some(username) = self.qrz_xml.username.as_deref() {
            values.insert(QRZ_XML_USERNAME_ENV_VAR.to_string(), username.to_string());
        }
        if let Some(password) = self.qrz_xml.password.as_deref() {
            values.insert(QRZ_XML_PASSWORD_ENV_VAR.to_string(), password.to_string());
        }
        if let Some(user_agent) = self.qrz_xml.user_agent.as_deref() {
            values.insert(QRZ_USER_AGENT_ENV_VAR.to_string(), user_agent.to_string());
        }

        // QRZ logbook config
        if let Some(api_key) = self.qrz_logbook.api_key.as_deref() {
            values.insert(QRZ_LOGBOOK_API_KEY_ENV_VAR.to_string(), api_key.to_string());
        }
        if let Some(base_url) = self.qrz_logbook.base_url.as_deref() {
            values.insert(
                QRZ_LOGBOOK_BASE_URL_ENV_VAR.to_string(),
                base_url.to_string(),
            );
        }

        // Sync config
        values.insert(
            SYNC_AUTO_ENABLED_ENV_VAR.to_string(),
            self.sync.auto_sync_enabled.to_string(),
        );
        values.insert(
            SYNC_INTERVAL_SECONDS_ENV_VAR.to_string(),
            self.sync.sync_interval_seconds.to_string(),
        );
        if !self.sync.conflict_policy.is_empty() {
            values.insert(
                SYNC_CONFLICT_POLICY_ENV_VAR.to_string(),
                self.sync.conflict_policy.clone(),
            );
        }

        // Rig control config
        if let Some(enabled) = self.rig_control.enabled {
            values.insert(RIGCTLD_ENABLED_ENV_VAR.to_string(), enabled.to_string());
        }
        if let Some(ref host) = self.rig_control.host {
            values.insert(RIGCTLD_HOST_ENV_VAR.to_string(), host.clone());
        }
        if let Some(port) = self.rig_control.port {
            values.insert(RIGCTLD_PORT_ENV_VAR.to_string(), port.to_string());
        }
        if let Some(read_timeout_ms) = self.rig_control.read_timeout_ms {
            values.insert(
                RIGCTLD_READ_TIMEOUT_MS_ENV_VAR.to_string(),
                read_timeout_ms.to_string(),
            );
        }
        if let Some(stale_threshold_ms) = self.rig_control.stale_threshold_ms {
            values.insert(
                RIGCTLD_STALE_THRESHOLD_MS_ENV_VAR.to_string(),
                stale_threshold_ms.to_string(),
            );
        }

        if !PersistedWsjtxIngestConfig::is_empty(&self.wsjtx_ingest) {
            self.wsjtx_ingest.insert_runtime_values(&mut values);
        }
        self.cw_keying.insert_runtime_values(&mut values);

        values
    }

    fn log_file_path(&self) -> Option<String> {
        normalize_optional_string(self.logbook.file_path.as_deref())
            .or_else(|| normalize_optional_string(self.storage.sqlite_path.as_deref()))
    }

    fn runtime_storage_backend(&self) -> StorageBackend {
        if self.log_file_path().is_some() {
            StorageBackend::Sqlite
        } else {
            self.legacy_storage_backend()
        }
    }

    fn legacy_storage_backend(&self) -> StorageBackend {
        match self.storage.backend.as_deref() {
            Some("memory") => StorageBackend::Memory,
            Some("sqlite") => StorageBackend::Sqlite,
            _ => StorageBackend::Unspecified,
        }
    }

    fn station_profile(&self) -> Option<StationProfile> {
        let profile = self.station_profiles.active_profile().map_or_else(
            || self.station_profile.to_proto(),
            PersistedStationProfileEntry::to_proto,
        );
        station_profile_has_values(&profile).then_some(profile)
    }

    fn active_station_profile_id(&self) -> Option<String> {
        self.station_profiles.active_profile_id()
    }

    fn station_profile_count(&self) -> usize {
        self.station_profiles
            .count_with_legacy_fallback(&self.station_profile)
    }

    fn list_station_profile_records(&self) -> Vec<StationProfileRecord> {
        self.station_profiles
            .list_records(&self.station_profile)
            .into_iter()
            .map(|entry| entry.to_record(self.active_station_profile_id().as_deref()))
            .collect()
    }

    fn get_station_profile_record(&self, profile_id: &str) -> Option<StationProfileRecord> {
        self.station_profiles
            .list_records(&self.station_profile)
            .into_iter()
            .find(|entry| entry.profile_id == normalize_profile_id(profile_id))
            .map(|entry| entry.to_record(self.active_station_profile_id().as_deref()))
    }

    fn save_station_profile(
        &mut self,
        requested_profile_id: Option<&str>,
        profile: &StationProfile,
        make_active: bool,
    ) -> String {
        let profile_id = self.station_profiles.save_profile(
            requested_profile_id,
            profile,
            make_active,
            &self.station_profile,
        );
        self.sync_active_station_profile();
        profile_id
    }

    fn delete_station_profile(&mut self, profile_id: &str) -> Result<(), String> {
        self.station_profiles
            .delete_profile(profile_id, &self.station_profile)?;
        self.sync_active_station_profile();
        Ok(())
    }

    fn set_active_station_profile(&mut self, profile_id: &str) -> Result<(), String> {
        self.station_profiles
            .set_active_profile(profile_id, &self.station_profile)?;
        self.sync_active_station_profile();
        Ok(())
    }

    fn sync_active_station_profile(&mut self) {
        self.station_profile = self.station_profiles.active_profile().map_or_else(
            || self.station_profile.clone(),
            |entry| PersistedStationProfile::from_proto(&entry.to_proto()),
        );
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct PersistedCwKeyingConfig {
    backend: Option<String>,
    winkeyer_port: Option<String>,
    winkeyer_baud: Option<u32>,
    cathub_endpoint: Option<String>,
    cathub_client_name: Option<String>,
    speed_wpm: Option<u32>,
    transmit_enabled: Option<bool>,
    max_tx_ms: Option<u64>,
}

impl PersistedCwKeyingConfig {
    fn is_empty(&self) -> bool {
        self.backend.is_none()
            && self.winkeyer_port.is_none()
            && self.winkeyer_baud.is_none()
            && self.cathub_endpoint.is_none()
            && self.cathub_client_name.is_none()
            && self.speed_wpm.is_none()
            && self.transmit_enabled.is_none()
            && self.max_tx_ms.is_none()
    }

    fn insert_runtime_values(&self, values: &mut BTreeMap<String, String>) {
        if let Some(value) = self.backend.as_deref() {
            values.insert(CW_KEYER_BACKEND_ENV_VAR.to_string(), value.to_string());
        }
        if let Some(value) = self.winkeyer_port.as_deref() {
            values.insert(CW_WINKEYER_PORT_ENV_VAR.to_string(), value.to_string());
        }
        if let Some(value) = self.winkeyer_baud {
            values.insert(CW_WINKEYER_BAUD_ENV_VAR.to_string(), value.to_string());
        }
        if let Some(value) = self.cathub_endpoint.as_deref() {
            values.insert(CW_CATHUB_ENDPOINT_ENV_VAR.to_string(), value.to_string());
        }
        if let Some(value) = self.cathub_client_name.as_deref() {
            values.insert(CW_CATHUB_CLIENT_NAME_ENV_VAR.to_string(), value.to_string());
        }
        if let Some(value) = self.speed_wpm {
            values.insert(CW_SPEED_WPM_ENV_VAR.to_string(), value.to_string());
        }
        if let Some(value) = self.transmit_enabled {
            values.insert(CW_TRANSMIT_ENABLED_ENV_VAR.to_string(), value.to_string());
        }
        if let Some(value) = self.max_tx_ms {
            values.insert(CW_MAX_TX_MS_ENV_VAR.to_string(), value.to_string());
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct PersistedLogbookConfig {
    file_path: Option<String>,
}

impl PersistedLogbookConfig {
    fn is_empty(config: &Self) -> bool {
        normalize_optional_string(config.file_path.as_deref()).is_none()
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct PersistedStorageConfig {
    backend: Option<String>,
    sqlite_path: Option<String>,
}

impl PersistedStorageConfig {
    fn is_empty(config: &Self) -> bool {
        config.backend.is_none()
            && normalize_optional_string(config.sqlite_path.as_deref()).is_none()
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct PersistedStationProfile {
    profile_name: Option<String>,
    station_callsign: Option<String>,
    operator_callsign: Option<String>,
    operator_name: Option<String>,
    grid: Option<String>,
    county: Option<String>,
    state: Option<String>,
    country: Option<String>,
    arrl_section: Option<String>,
    dxcc: Option<u32>,
    cq_zone: Option<u32>,
    itu_zone: Option<u32>,
    latitude: Option<f64>,
    longitude: Option<f64>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct PersistedStationProfileCatalog {
    active_profile_id: Option<String>,
    #[serde(default)]
    entries: Vec<PersistedStationProfileEntry>,
}

impl PersistedStationProfileCatalog {
    fn active_profile_id(&self) -> Option<String> {
        self.active_profile()
            .map(|entry| entry.profile_id.clone())
            .or_else(|| self.active_profile_id.clone())
    }

    fn active_profile(&self) -> Option<&PersistedStationProfileEntry> {
        self.active_profile_id
            .as_deref()
            .and_then(|profile_id| self.find_entry(profile_id))
            .or_else(|| self.entries.first())
    }

    fn find_entry(&self, profile_id: &str) -> Option<&PersistedStationProfileEntry> {
        let normalized_id = normalize_profile_id(profile_id);
        self.entries
            .iter()
            .find(|entry| entry.profile_id == normalized_id)
    }

    fn count_with_legacy_fallback(&self, legacy_profile: &PersistedStationProfile) -> usize {
        if self.entries.is_empty() && legacy_profile.has_values() {
            1
        } else {
            self.entries.len()
        }
    }

    fn list_records(
        &self,
        legacy_profile: &PersistedStationProfile,
    ) -> Vec<PersistedStationProfileEntry> {
        if self.entries.is_empty() && legacy_profile.has_values() {
            vec![PersistedStationProfileEntry {
                profile_id: generate_profile_id(None, &legacy_profile.to_proto(), &[]),
                profile: legacy_profile.clone(),
            }]
        } else {
            self.entries.clone()
        }
    }

    fn save_profile(
        &mut self,
        requested_profile_id: Option<&str>,
        profile: &StationProfile,
        make_active: bool,
        legacy_profile: &PersistedStationProfile,
    ) -> String {
        self.bootstrap_from_legacy(legacy_profile);
        let existing_ids: Vec<String> = self
            .entries
            .iter()
            .map(|entry| entry.profile_id.clone())
            .collect();
        let profile_id = requested_profile_id
            .map(normalize_profile_id)
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| generate_profile_id(None, profile, &existing_ids));

        if let Some(entry) = self
            .entries
            .iter_mut()
            .find(|entry| entry.profile_id == profile_id)
        {
            entry.profile = PersistedStationProfile::from_proto(profile);
        } else {
            self.entries.push(PersistedStationProfileEntry {
                profile_id: profile_id.clone(),
                profile: PersistedStationProfile::from_proto(profile),
            });
        }

        if make_active || self.active_profile_id.as_deref().is_none() {
            self.active_profile_id = Some(profile_id.clone());
        }

        profile_id
    }

    fn delete_profile(
        &mut self,
        profile_id: &str,
        legacy_profile: &PersistedStationProfile,
    ) -> Result<(), String> {
        self.bootstrap_from_legacy(legacy_profile);
        let normalized_id = normalize_profile_id(profile_id);
        if self.active_profile_id.as_deref() == Some(normalized_id.as_str()) {
            return Err(
                "The active station profile cannot be deleted. Activate another profile first."
                    .to_string(),
            );
        }
        let initial_len = self.entries.len();
        self.entries
            .retain(|entry| entry.profile_id != normalized_id);
        if self.entries.len() == initial_len {
            return Err(format!("Station profile '{normalized_id}' was not found."));
        }
        Ok(())
    }

    fn set_active_profile(
        &mut self,
        profile_id: &str,
        legacy_profile: &PersistedStationProfile,
    ) -> Result<(), String> {
        self.bootstrap_from_legacy(legacy_profile);
        let normalized_id = normalize_profile_id(profile_id);
        if self
            .entries
            .iter()
            .any(|entry| entry.profile_id == normalized_id)
        {
            self.active_profile_id = Some(normalized_id);
            Ok(())
        } else {
            Err(format!("Station profile '{normalized_id}' was not found."))
        }
    }

    fn bootstrap_from_legacy(&mut self, legacy_profile: &PersistedStationProfile) {
        if self.entries.is_empty() && legacy_profile.has_values() {
            let profile = legacy_profile.to_proto();
            self.entries.push(PersistedStationProfileEntry {
                profile_id: generate_profile_id(None, &profile, &[]),
                profile: legacy_profile.clone(),
            });
        }
        if self.active_profile_id.as_deref().is_none() {
            self.active_profile_id = self.entries.first().map(|entry| entry.profile_id.clone());
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct PersistedStationProfileEntry {
    profile_id: String,
    #[serde(flatten)]
    profile: PersistedStationProfile,
}

impl PersistedStationProfileEntry {
    fn to_proto(&self) -> StationProfile {
        self.profile.to_proto()
    }

    fn to_record(&self, active_profile_id: Option<&str>) -> StationProfileRecord {
        StationProfileRecord {
            profile_id: self.profile_id.clone(),
            profile: Some(self.to_proto()),
            is_active: active_profile_id == Some(self.profile_id.as_str()),
        }
    }
}

impl PersistedStationProfile {
    fn from_proto(profile: &StationProfile) -> Self {
        Self {
            profile_name: normalize_optional_string(profile.profile_name.as_deref()),
            station_callsign: normalize_optional_callsign(Some(profile.station_callsign.as_str())),
            operator_callsign: normalize_optional_callsign(profile.operator_callsign.as_deref()),
            operator_name: normalize_optional_string(profile.operator_name.as_deref()),
            grid: normalize_optional_string(profile.grid.as_deref()),
            county: normalize_optional_string(profile.county.as_deref()),
            state: normalize_optional_string(profile.state.as_deref()),
            country: normalize_optional_string(profile.country.as_deref()),
            arrl_section: normalize_optional_string(profile.arrl_section.as_deref()),
            dxcc: profile.dxcc,
            cq_zone: profile.cq_zone,
            itu_zone: profile.itu_zone,
            latitude: profile.latitude,
            longitude: profile.longitude,
        }
    }

    fn to_proto(&self) -> StationProfile {
        StationProfile {
            profile_name: self.profile_name.clone(),
            station_callsign: self.station_callsign.clone().unwrap_or_default(),
            operator_callsign: self.operator_callsign.clone(),
            operator_name: self.operator_name.clone(),
            grid: self.grid.clone(),
            county: self.county.clone(),
            state: self.state.clone(),
            country: self.country.clone(),
            arrl_section: self.arrl_section.clone(),
            dxcc: self.dxcc,
            cq_zone: self.cq_zone,
            itu_zone: self.itu_zone,
            latitude: self.latitude,
            longitude: self.longitude,
        }
    }

    fn has_values(&self) -> bool {
        station_profile_has_values(&self.to_proto())
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct PersistedQrzXmlConfig {
    username: Option<String>,
    /// QRZ XML password is persisted with the saved setup config so engine
    /// restarts can continue serving live lookups without requiring a
    /// separate process-level environment variable injection step.
    password: Option<String>,
    user_agent: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct PersistedQrzLogbookConfig {
    api_key: Option<String>,
    base_url: Option<String>,
}

impl PersistedQrzLogbookConfig {
    fn is_empty(config: &Self) -> bool {
        normalize_optional_string(config.api_key.as_deref()).is_none()
            && normalize_optional_string(config.base_url.as_deref()).is_none()
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct PersistedSyncConfig {
    #[serde(default)]
    auto_sync_enabled: bool,
    #[serde(default = "default_sync_interval_seconds")]
    sync_interval_seconds: u32,
    #[serde(default)]
    conflict_policy: String,
}

fn default_sync_interval_seconds() -> u32 {
    300
}

impl PersistedSyncConfig {
    fn is_empty(config: &Self) -> bool {
        !config.auto_sync_enabled
            && config.sync_interval_seconds == default_sync_interval_seconds()
            && (config.conflict_policy.is_empty() || config.conflict_policy == "last_write_wins")
    }

    fn from_proto(sync_config: &SyncConfig) -> Self {
        let conflict_policy = match ConflictPolicy::try_from(sync_config.conflict_policy) {
            Ok(ConflictPolicy::LastWriteWins) => "last_write_wins".to_string(),
            _ => "flag_for_review".to_string(),
        };
        Self {
            auto_sync_enabled: sync_config.auto_sync_enabled,
            sync_interval_seconds: if sync_config.sync_interval_seconds == 0 {
                default_sync_interval_seconds()
            } else {
                sync_config.sync_interval_seconds
            },
            conflict_policy,
        }
    }

    fn to_proto(&self) -> SyncConfig {
        let conflict_policy = match self.conflict_policy.as_str() {
            "flag_for_review" => ConflictPolicy::FlagForReview,
            _ => ConflictPolicy::LastWriteWins,
        };
        SyncConfig {
            auto_sync_enabled: self.auto_sync_enabled,
            sync_interval_seconds: if self.sync_interval_seconds == 0 {
                default_sync_interval_seconds()
            } else {
                self.sync_interval_seconds
            },
            conflict_policy: conflict_policy as i32,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct PersistedRigControlConfig {
    enabled: Option<bool>,
    host: Option<String>,
    port: Option<u16>,
    read_timeout_ms: Option<u64>,
    stale_threshold_ms: Option<u64>,
}

impl PersistedRigControlConfig {
    fn is_empty(config: &Self) -> bool {
        config.enabled.is_none()
            && config.host.is_none()
            && config.port.is_none()
            && config.read_timeout_ms.is_none()
            && config.stale_threshold_ms.is_none()
    }

    fn from_proto(rig_control: &RigControlSettings) -> Result<Self, String> {
        let port = match rig_control.port {
            Some(0) => {
                return Err("Rig control port must be between 1 and 65535.".to_string());
            }
            Some(port) => Some(
                u16::try_from(port)
                    .map_err(|_| "Rig control port must be between 1 and 65535.".to_string())?,
            ),
            None => None,
        };

        let read_timeout_ms = match rig_control.read_timeout_ms {
            Some(0) => {
                return Err(
                    "Rig control read timeout must be greater than 0 milliseconds.".to_string(),
                );
            }
            Some(value) => Some(value),
            None => None,
        };

        let stale_threshold_ms = match rig_control.stale_threshold_ms {
            Some(0) => {
                return Err(
                    "Rig control stale threshold must be greater than 0 milliseconds.".to_string(),
                );
            }
            Some(value) => Some(value),
            None => None,
        };

        Ok(Self {
            enabled: rig_control.enabled,
            host: normalize_optional_string(rig_control.host.as_deref()),
            port,
            read_timeout_ms,
            stale_threshold_ms,
        })
    }

    fn to_proto(&self) -> Option<RigControlSettings> {
        if Self::is_empty(self) {
            return None;
        }

        Some(RigControlSettings {
            enabled: self.enabled,
            host: self.host.clone(),
            port: self.port.map(u32::from),
            read_timeout_ms: self.read_timeout_ms,
            stale_threshold_ms: self.stale_threshold_ms,
        })
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct PersistedWsjtxIngestConfig {
    enabled: Option<bool>,
    udp_enabled: Option<bool>,
    udp_bind: Option<String>,
    adif_tail_enabled: Option<bool>,
    adif_tail_path: Option<String>,
    poll_interval_ms: Option<u32>,
    sync_to_qrz: Option<bool>,
}

impl PersistedWsjtxIngestConfig {
    fn is_empty(config: &Self) -> bool {
        config.enabled.is_none()
            && config.udp_enabled.is_none()
            && normalize_optional_string(config.udp_bind.as_deref()).is_none()
            && config.adif_tail_enabled.is_none()
            && normalize_optional_string(config.adif_tail_path.as_deref()).is_none()
            && config.poll_interval_ms.is_none()
            && config.sync_to_qrz.is_none()
    }

    fn from_proto(settings: &WsjtxIngestSettings) -> Result<Self, String> {
        let udp_bind = normalize_optional_string(Some(&settings.udp_bind))
            .unwrap_or_else(|| crate::runtime_config::DEFAULT_WSJTX_INGEST_UDP_BIND.to_string());
        validate_host_port(&udp_bind, "WSJT-X UDP bind")?;
        let poll_interval_ms = if settings.poll_interval_ms == 0 {
            crate::runtime_config::DEFAULT_WSJTX_INGEST_POLL_INTERVAL_MS
                .parse()
                .unwrap_or(1_000)
        } else {
            settings.poll_interval_ms
        };
        let adif_tail_path = normalize_optional_string(settings.adif_tail_path.as_deref());
        if settings.adif_tail_enabled && adif_tail_path.is_none() {
            return Err(
                "WSJT-X ADIF tail path is required when ADIF tailing is enabled.".to_string(),
            );
        }

        Ok(Self {
            enabled: Some(settings.enabled),
            udp_enabled: Some(settings.udp_enabled.unwrap_or(true)),
            udp_bind: Some(udp_bind),
            adif_tail_enabled: Some(settings.adif_tail_enabled),
            adif_tail_path,
            poll_interval_ms: Some(poll_interval_ms),
            sync_to_qrz: Some(settings.sync_to_qrz),
        })
    }

    fn to_proto(&self) -> Option<WsjtxIngestSettings> {
        if Self::is_empty(self) {
            return None;
        }

        Some(WsjtxIngestSettings {
            enabled: self.enabled.unwrap_or(false),
            udp_enabled: Some(self.udp_enabled.unwrap_or(true)),
            udp_bind: normalize_optional_string(self.udp_bind.as_deref()).unwrap_or_else(|| {
                crate::runtime_config::DEFAULT_WSJTX_INGEST_UDP_BIND.to_string()
            }),
            adif_tail_enabled: self.adif_tail_enabled.unwrap_or(false),
            adif_tail_path: normalize_optional_string(self.adif_tail_path.as_deref()),
            poll_interval_ms: self.poll_interval_ms.unwrap_or_else(|| {
                crate::runtime_config::DEFAULT_WSJTX_INGEST_POLL_INTERVAL_MS
                    .parse()
                    .unwrap_or(1_000)
            }),
            sync_to_qrz: self.sync_to_qrz.unwrap_or(false),
        })
    }

    fn insert_runtime_values(&self, values: &mut BTreeMap<String, String>) {
        if let Some(enabled) = self.enabled {
            values.insert(
                WSJTX_INGEST_ENABLED_ENV_VAR.to_string(),
                enabled.to_string(),
            );
        }
        if let Some(udp_enabled) = self.udp_enabled {
            values.insert(
                WSJTX_INGEST_UDP_ENABLED_ENV_VAR.to_string(),
                udp_enabled.to_string(),
            );
        }
        if let Some(udp_bind) = normalize_optional_string(self.udp_bind.as_deref()) {
            values.insert(WSJTX_INGEST_UDP_BIND_ENV_VAR.to_string(), udp_bind);
        }
        if let Some(adif_tail_enabled) = self.adif_tail_enabled {
            values.insert(
                WSJTX_INGEST_ADIF_TAIL_ENABLED_ENV_VAR.to_string(),
                adif_tail_enabled.to_string(),
            );
        }
        if let Some(adif_tail_path) = normalize_optional_string(self.adif_tail_path.as_deref()) {
            values.insert(
                WSJTX_INGEST_ADIF_TAIL_PATH_ENV_VAR.to_string(),
                adif_tail_path,
            );
        }
        if let Some(poll_interval_ms) = self.poll_interval_ms {
            values.insert(
                WSJTX_INGEST_POLL_INTERVAL_MS_ENV_VAR.to_string(),
                poll_interval_ms.to_string(),
            );
        }
        if let Some(sync_to_qrz) = self.sync_to_qrz {
            values.insert(
                WSJTX_INGEST_SYNC_TO_QRZ_ENV_VAR.to_string(),
                sync_to_qrz.to_string(),
            );
        }
    }
}

/// Mirror of the cathub daemon's `[cat_hub.radio]` table. Every field is optional
/// so the engine only writes what the wizard supplied and the daemon fills in the
/// rest from its own defaults.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct PersistedCatHubRadio {
    #[serde(skip_serializing_if = "Option::is_none")]
    backend: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    transport: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    port: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    baud: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    host: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tcp_port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    certified: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reply_timeout_ms: Option<u64>,
}

/// Mirror of the cathub daemon's `[cat_hub.poll]` table.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct PersistedCatHubPoll {
    #[serde(skip_serializing_if = "Option::is_none")]
    baseline_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    heartbeat_ms: Option<u64>,
}

/// Mirror of the cathub daemon's `[cat_hub.ptt]` table.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct PersistedCatHubPtt {
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tx_ms: Option<u64>,
}

/// Mirror of the cathub daemon's `[cat_hub.events]` table.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct PersistedCatHubEvents {
    #[serde(skip_serializing_if = "Option::is_none")]
    native_push: Option<bool>,
}

/// Mirror of a cathub daemon `[[cat_hub.face]]` serial endpoint.
#[derive(Clone, Debug, Deserialize, Serialize)]
struct PersistedCatHubFace {
    name: String,
    transport: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    baud: Option<u32>,
    dialect: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    perms: Vec<String>,
}

/// Mirror of a cathub daemon `[[cat_hub.hamlib_net]]` TCP endpoint.
#[derive(Clone, Debug, Deserialize, Serialize)]
struct PersistedCatHubHamlibNet {
    name: String,
    bind: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    perms: Vec<String>,
}

/// Mirror of the cathub daemon's `[cat_hub.winkeyer]` table.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct PersistedCatHubWinkeyer {
    port: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    baud: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tx_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    api_bind: Option<String>,
}

/// Mirror of a cathub daemon `[[cat_hub.winkeyer_face]]` endpoint.
#[derive(Clone, Debug, Deserialize, Serialize)]
struct PersistedCatHubWinkeyerFace {
    name: String,
    transport: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    baud: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    primary: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    perms: Vec<String>,
}

/// Mirror of the cathub daemon's `[cat_hub]` section. This is deliberately NOT a
/// field of `PersistedSetupConfig`: the engine never parses `[cat_hub]` as part of
/// loading its own config, so a malformed `[cat_hub]` written by the daemon (or a
/// future schema the engine does not know about) can never break engine startup.
/// It is parsed leniently for display and written only when the wizard supplies a
/// complete replacement.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct PersistedCatHubConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    radio: Option<PersistedCatHubRadio>,
    #[serde(skip_serializing_if = "Option::is_none")]
    poll: Option<PersistedCatHubPoll>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ptt: Option<PersistedCatHubPtt>,
    #[serde(skip_serializing_if = "Option::is_none")]
    events: Option<PersistedCatHubEvents>,
    // The cathub daemon keys the serial-face array as singular `[[cat_hub.face]]`.
    #[serde(rename = "face", default, skip_serializing_if = "Vec::is_empty")]
    faces: Vec<PersistedCatHubFace>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    hamlib_net: Vec<PersistedCatHubHamlibNet>,
    #[serde(skip_serializing_if = "Option::is_none")]
    winkeyer: Option<PersistedCatHubWinkeyer>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    winkeyer_face: Vec<PersistedCatHubWinkeyerFace>,
}

const CAT_HUB_RADIO_BACKENDS: [&str; 3] = ["ts590", "rigctld", "loopback"];
const CAT_HUB_RADIO_TRANSPORTS: [&str; 2] = ["serial", "tcp"];
const CAT_HUB_FACE_DIALECTS: [&str; 2] = ["ts590", "ts2000"];

/// Map a `CatHubPermission` enum value to the lowercase TOML token the cathub
/// daemon expects. Unknown/unspecified values are dropped.
fn cat_hub_perm_token(value: i32) -> Option<&'static str> {
    match CatHubPermission::try_from(value) {
        Ok(CatHubPermission::Read) => Some("read"),
        Ok(CatHubPermission::Write) => Some("write"),
        Ok(CatHubPermission::Ptt) => Some("ptt"),
        Ok(CatHubPermission::ConfigWrite) => Some("config_write"),
        _ => None,
    }
}

/// Map a TOML permission token back to its `CatHubPermission` enum value.
fn cat_hub_perm_from_token(token: &str) -> Option<i32> {
    let perm = match token {
        "read" => CatHubPermission::Read,
        "write" => CatHubPermission::Write,
        "ptt" => CatHubPermission::Ptt,
        "config_write" => CatHubPermission::ConfigWrite,
        _ => return None,
    };
    Some(perm as i32)
}

fn cat_hub_perms_to_tokens(perms: &[i32]) -> Vec<String> {
    perms
        .iter()
        .filter_map(|value| cat_hub_perm_token(*value).map(str::to_string))
        .collect()
}

fn cat_hub_perms_to_proto(perms: &[String]) -> Vec<i32> {
    perms
        .iter()
        .filter_map(|token| cat_hub_perm_from_token(token))
        .collect()
}

fn winkeyer_perm_token(value: i32) -> Option<&'static str> {
    match WinkeyerFacePermission::try_from(value) {
        Ok(WinkeyerFacePermission::Status) => Some("status"),
        Ok(WinkeyerFacePermission::Send) => Some("send"),
        Ok(WinkeyerFacePermission::Control) => Some("control"),
        Ok(WinkeyerFacePermission::Ptt) => Some("ptt"),
        Ok(WinkeyerFacePermission::ConfigWrite) => Some("config_write"),
        _ => None,
    }
}

fn winkeyer_perm_from_token(token: &str) -> Option<i32> {
    let permission = match token {
        "status" => WinkeyerFacePermission::Status,
        "send" => WinkeyerFacePermission::Send,
        "control" => WinkeyerFacePermission::Control,
        "ptt" => WinkeyerFacePermission::Ptt,
        "config_write" => WinkeyerFacePermission::ConfigWrite,
        _ => return None,
    };
    Some(permission as i32)
}

impl PersistedCatHubConfig {
    fn is_empty(&self) -> bool {
        self.radio.is_none()
            && self.poll.is_none()
            && self.ptt.is_none()
            && self.events.is_none()
            && self.faces.is_empty()
            && self.hamlib_net.is_empty()
            && self.winkeyer.is_none()
            && self.winkeyer_face.is_empty()
    }

    /// Project the persisted `[cat_hub]` section onto the proto envelope for status
    /// and wizard display. Returns `None` when nothing is configured.
    fn to_proto(&self) -> Option<CatHubSettings> {
        if self.is_empty() {
            return None;
        }
        Some(CatHubSettings {
            radio: self.radio.as_ref().map(|radio| CatHubRadioSettings {
                backend: radio.backend.clone(),
                model: radio.model.clone(),
                transport: radio.transport.clone(),
                port: radio.port.clone(),
                baud: radio.baud,
                host: radio.host.clone(),
                tcp_port: radio.tcp_port.map(u32::from),
                certified: radio.certified,
                reply_timeout_ms: radio.reply_timeout_ms,
            }),
            poll: self.poll.as_ref().map(|poll| CatHubPollSettings {
                baseline_ms: poll.baseline_ms,
                heartbeat_ms: poll.heartbeat_ms,
            }),
            ptt: self.ptt.as_ref().map(|ptt| CatHubPttSettings {
                max_tx_ms: ptt.max_tx_ms,
            }),
            events: self.events.as_ref().map(|events| CatHubEventSettings {
                native_push: events.native_push,
            }),
            faces: self
                .faces
                .iter()
                .map(|face| CatHubSerialFace {
                    name: face.name.clone(),
                    transport: face.transport.clone(),
                    baud: face.baud.unwrap_or_default(),
                    dialect: face.dialect.clone(),
                    perms: cat_hub_perms_to_proto(&face.perms),
                })
                .collect(),
            hamlib_net: self
                .hamlib_net
                .iter()
                .map(|endpoint| CatHubHamlibNetEndpoint {
                    name: endpoint.name.clone(),
                    bind: endpoint.bind.clone(),
                    perms: cat_hub_perms_to_proto(&endpoint.perms),
                })
                .collect(),
            winkeyer: self
                .winkeyer
                .as_ref()
                .map(|winkeyer| CatHubWinkeyerSettings {
                    port: winkeyer.port.clone(),
                    baud: winkeyer.baud,
                    max_tx_ms: winkeyer.max_tx_ms,
                    api_bind: winkeyer.api_bind.clone(),
                }),
            winkeyer_faces: self
                .winkeyer_face
                .iter()
                .map(|face| CatHubWinkeyerFace {
                    name: face.name.clone(),
                    transport: face.transport.clone(),
                    baud: face.baud,
                    primary: face.primary,
                    perms: face
                        .perms
                        .iter()
                        .filter_map(|token| winkeyer_perm_from_token(token))
                        .collect(),
                })
                .collect(),
        })
    }

    /// Build a complete, daemon-valid `[cat_hub]` section from a wizard request.
    /// The proto contract is full-replacement: `radio` (with a backend) is
    /// required and at least one endpoint must be present. Additional wizard
    /// safety checks (unique names/transports/binds) keep the written set a strict
    /// subset of what the daemon accepts.
    fn from_proto(settings: &CatHubSettings) -> Result<Self, String> {
        let radio = cat_hub_radio_from_proto(
            settings
                .radio
                .as_ref()
                .ok_or_else(|| "CAT hub radio settings are required.".to_string())?,
        )?;
        let faces = cat_hub_faces_from_proto(&settings.faces, radio.port.as_deref())?;
        let hamlib_net = cat_hub_endpoints_from_proto(&settings.hamlib_net)?;

        if faces.is_empty() && hamlib_net.is_empty() {
            return Err(
                "CAT hub configuration requires at least one serial face or hamlib_net endpoint."
                    .to_string(),
            );
        }

        let mut names: Vec<&str> = Vec::new();
        for face in &faces {
            names.push(face.name.as_str());
        }
        for endpoint in &hamlib_net {
            names.push(endpoint.name.as_str());
        }
        if let Some(duplicate) = first_duplicate(&names) {
            return Err(format!(
                "CAT hub endpoint names must be unique: '{duplicate}'."
            ));
        }

        let poll = settings
            .poll
            .as_ref()
            .map(cat_hub_poll_from_proto)
            .transpose()?;
        let ptt = settings
            .ptt
            .as_ref()
            .map(cat_hub_ptt_from_proto)
            .transpose()?;
        let events = settings
            .events
            .as_ref()
            .map(|events| PersistedCatHubEvents {
                native_push: events.native_push,
            });
        let winkeyer = settings
            .winkeyer
            .as_ref()
            .map(cat_hub_winkeyer_from_proto)
            .transpose()?;
        if winkeyer.as_ref().is_some_and(|keyer| {
            radio
                .port
                .as_deref()
                .is_some_and(|port| keyer.port.eq_ignore_ascii_case(port))
        }) {
            return Err("CAT hub WinKeyer port must be distinct from the radio port.".to_string());
        }
        let winkeyer_face = cat_hub_winkeyer_faces_from_proto(
            &settings.winkeyer_faces,
            winkeyer.as_ref(),
            radio.port.as_deref(),
        )?;

        Ok(Self {
            radio: Some(radio),
            poll,
            ptt,
            events,
            faces,
            hamlib_net,
            winkeyer,
            winkeyer_face,
        })
    }
}

fn cat_hub_winkeyer_from_proto(
    winkeyer: &CatHubWinkeyerSettings,
) -> Result<PersistedCatHubWinkeyer, String> {
    let port = normalize_optional_string(Some(&winkeyer.port))
        .ok_or_else(|| "CAT hub WinKeyer port is required.".to_string())?;
    if let Some(baud) = winkeyer.baud {
        if baud != 1_200 {
            return Err("CAT hub WinKeyer baud must be 1200.".to_string());
        }
    }
    if let Some(max_tx_ms) = winkeyer.max_tx_ms {
        if !(1_000..=300_000).contains(&max_tx_ms) {
            return Err("CAT hub WinKeyer max_tx_ms must be between 1000 and 300000.".to_string());
        }
    }
    let api_bind = normalize_optional_string(winkeyer.api_bind.as_deref());
    if let Some(bind) = api_bind.as_deref() {
        let address: std::net::SocketAddr = bind.parse().map_err(|_| {
            "CAT hub WinKeyer api_bind must be a host:port socket address.".to_string()
        })?;
        if !address.ip().is_loopback() {
            return Err("CAT hub WinKeyer api_bind must use a loopback address.".to_string());
        }
    }
    Ok(PersistedCatHubWinkeyer {
        port,
        baud: winkeyer.baud,
        max_tx_ms: winkeyer.max_tx_ms,
        api_bind,
    })
}

fn cat_hub_winkeyer_faces_from_proto(
    faces: &[CatHubWinkeyerFace],
    winkeyer: Option<&PersistedCatHubWinkeyer>,
    radio_port: Option<&str>,
) -> Result<Vec<PersistedCatHubWinkeyerFace>, String> {
    if winkeyer.is_none() && !faces.is_empty() {
        return Err("CAT hub WinKeyer faces require WinKeyer settings.".to_string());
    }
    let mut result = Vec::with_capacity(faces.len());
    let mut transports = Vec::with_capacity(faces.len());
    let mut primary_count = 0;
    for face in faces {
        let name = normalize_optional_string(Some(&face.name))
            .ok_or_else(|| "CAT hub WinKeyer face name is required.".to_string())?;
        let transport = normalize_optional_string(Some(&face.transport))
            .ok_or_else(|| format!("CAT hub WinKeyer face '{name}' requires a transport."))?;
        if radio_port.is_some_and(|port| transport.eq_ignore_ascii_case(port)) {
            return Err(format!(
                "CAT hub WinKeyer face '{name}' cannot reuse the radio port."
            ));
        }
        if winkeyer.is_some_and(|settings| transport.eq_ignore_ascii_case(&settings.port)) {
            return Err(format!(
                "CAT hub WinKeyer face '{name}' cannot reuse the physical WinKeyer port."
            ));
        }
        if let Some(baud) = face.baud {
            if baud != 1_200 {
                return Err(format!("CAT hub WinKeyer face '{name}' baud must be 1200."));
            }
        }
        if face.primary == Some(true) {
            primary_count += 1;
        }
        let has = |permission| face.perms.contains(&(permission as i32));
        if has(WinkeyerFacePermission::Send) && !has(WinkeyerFacePermission::Status) {
            return Err(format!(
                "CAT hub WinKeyer face '{name}' permission 'send' requires 'status'."
            ));
        }
        if has(WinkeyerFacePermission::Ptt)
            && (!has(WinkeyerFacePermission::Send) || !has(WinkeyerFacePermission::Control))
        {
            return Err(format!(
                "CAT hub WinKeyer face '{name}' permission 'ptt' requires 'send' and 'control'."
            ));
        }
        if has(WinkeyerFacePermission::ConfigWrite)
            && (!has(WinkeyerFacePermission::Status) || !has(WinkeyerFacePermission::Control))
        {
            return Err(format!(
                "CAT hub WinKeyer face '{name}' permission 'config_write' requires 'status' and 'control'."
            ));
        }
        transports.push(transport.clone());
        result.push(PersistedCatHubWinkeyerFace {
            name,
            transport,
            baud: face.baud,
            primary: face.primary,
            perms: face
                .perms
                .iter()
                .filter_map(|value| winkeyer_perm_token(*value).map(str::to_string))
                .collect(),
        });
    }
    if primary_count > 1 {
        return Err("At most one CAT hub WinKeyer face may be primary.".to_string());
    }
    let transport_refs: Vec<&str> = transports.iter().map(String::as_str).collect();
    if let Some(duplicate) = first_duplicate(&transport_refs) {
        return Err(format!(
            "CAT hub WinKeyer faces must use distinct transports: '{duplicate}'."
        ));
    }
    Ok(result)
}

fn cat_hub_radio_from_proto(radio: &CatHubRadioSettings) -> Result<PersistedCatHubRadio, String> {
    let backend = normalize_optional_string(radio.backend.as_deref())
        .ok_or_else(|| "CAT hub radio backend is required.".to_string())?
        .to_ascii_lowercase();
    if !CAT_HUB_RADIO_BACKENDS.contains(&backend.as_str()) {
        return Err(format!(
            "CAT hub radio backend '{backend}' is not supported (expected one of: {}).",
            CAT_HUB_RADIO_BACKENDS.join(", ")
        ));
    }

    let transport = match normalize_optional_string(radio.transport.as_deref()) {
        Some(value) => {
            let value = value.to_ascii_lowercase();
            if !CAT_HUB_RADIO_TRANSPORTS.contains(&value.as_str()) {
                return Err(format!(
                    "CAT hub radio transport '{value}' is not supported (expected one of: {}).",
                    CAT_HUB_RADIO_TRANSPORTS.join(", ")
                ));
            }
            Some(value)
        }
        None => None,
    };

    let port = normalize_optional_string(radio.port.as_deref());
    let effective_transport = transport.as_deref().unwrap_or("serial");
    if backend != "loopback" && effective_transport == "serial" && port.is_none() {
        return Err("CAT hub radio requires a serial port for the selected backend.".to_string());
    }

    let tcp_port = match radio.tcp_port {
        Some(0) => return Err("CAT hub radio tcp_port must be between 1 and 65535.".to_string()),
        Some(value) => Some(
            u16::try_from(value)
                .map_err(|_| "CAT hub radio tcp_port must be between 1 and 65535.".to_string())?,
        ),
        None => None,
    };
    if let Some(0) = radio.baud {
        return Err("CAT hub radio baud must be greater than 0.".to_string());
    }
    if let Some(0) = radio.reply_timeout_ms {
        return Err("CAT hub radio reply_timeout_ms must be greater than 0.".to_string());
    }

    Ok(PersistedCatHubRadio {
        backend: Some(backend),
        model: normalize_optional_string(radio.model.as_deref()),
        transport,
        port,
        baud: radio.baud,
        host: normalize_optional_string(radio.host.as_deref()),
        tcp_port,
        certified: radio.certified,
        reply_timeout_ms: radio.reply_timeout_ms,
    })
}

fn cat_hub_poll_from_proto(poll: &CatHubPollSettings) -> Result<PersistedCatHubPoll, String> {
    if let Some(0) = poll.baseline_ms {
        return Err("CAT hub poll baseline_ms must be greater than 0.".to_string());
    }
    if let Some(0) = poll.heartbeat_ms {
        return Err("CAT hub poll heartbeat_ms must be greater than 0.".to_string());
    }
    Ok(PersistedCatHubPoll {
        baseline_ms: poll.baseline_ms,
        heartbeat_ms: poll.heartbeat_ms,
    })
}

fn cat_hub_ptt_from_proto(ptt: &CatHubPttSettings) -> Result<PersistedCatHubPtt, String> {
    if let Some(0) = ptt.max_tx_ms {
        return Err("CAT hub ptt max_tx_ms must be greater than 0.".to_string());
    }
    Ok(PersistedCatHubPtt {
        max_tx_ms: ptt.max_tx_ms,
    })
}

fn cat_hub_faces_from_proto(
    faces: &[CatHubSerialFace],
    radio_port: Option<&str>,
) -> Result<Vec<PersistedCatHubFace>, String> {
    let mut result = Vec::with_capacity(faces.len());
    let mut transports: Vec<String> = Vec::new();
    for face in faces {
        let name = normalize_optional_string(Some(&face.name))
            .ok_or_else(|| "CAT hub serial face name is required.".to_string())?;
        let transport = normalize_optional_string(Some(&face.transport))
            .ok_or_else(|| format!("CAT hub serial face '{name}' requires a transport."))?;
        if let Some(port) = radio_port {
            if transport.eq_ignore_ascii_case(port) {
                return Err(format!(
                    "CAT hub serial face '{name}' cannot reuse the radio port '{port}'."
                ));
            }
        }
        let dialect = normalize_optional_string(Some(&face.dialect))
            .ok_or_else(|| format!("CAT hub serial face '{name}' requires a dialect."))?
            .to_ascii_lowercase();
        if !CAT_HUB_FACE_DIALECTS.contains(&dialect.as_str()) {
            return Err(format!(
                "CAT hub serial face '{name}' dialect '{dialect}' is not supported (expected one of: {}).",
                CAT_HUB_FACE_DIALECTS.join(", ")
            ));
        }
        let baud = match face.baud {
            0 => None,
            value => Some(value),
        };
        transports.push(transport.clone());
        result.push(PersistedCatHubFace {
            name,
            transport,
            baud,
            dialect,
            perms: cat_hub_perms_to_tokens(&face.perms),
        });
    }
    let transport_refs: Vec<&str> = transports.iter().map(String::as_str).collect();
    if let Some(duplicate) = first_duplicate(&transport_refs) {
        return Err(format!(
            "CAT hub serial faces must use distinct transports: '{duplicate}'."
        ));
    }
    Ok(result)
}

fn cat_hub_endpoints_from_proto(
    endpoints: &[CatHubHamlibNetEndpoint],
) -> Result<Vec<PersistedCatHubHamlibNet>, String> {
    let mut result = Vec::with_capacity(endpoints.len());
    let mut binds: Vec<String> = Vec::new();
    for endpoint in endpoints {
        let name = normalize_optional_string(Some(&endpoint.name))
            .ok_or_else(|| "CAT hub hamlib_net endpoint name is required.".to_string())?;
        let bind = normalize_optional_string(Some(&endpoint.bind)).ok_or_else(|| {
            format!("CAT hub hamlib_net endpoint '{name}' requires a bind address.")
        })?;
        validate_cat_hub_bind(&bind, &name)?;
        binds.push(bind.clone());
        result.push(PersistedCatHubHamlibNet {
            name,
            bind,
            perms: cat_hub_perms_to_tokens(&endpoint.perms),
        });
    }
    let bind_refs: Vec<&str> = binds.iter().map(String::as_str).collect();
    if let Some(duplicate) = first_duplicate(&bind_refs) {
        return Err(format!(
            "CAT hub hamlib_net endpoints must use distinct bind addresses: '{duplicate}'."
        ));
    }
    Ok(result)
}

/// Light validation of a `host:port` bind string. Kept intentionally permissive so
/// hostnames the daemon accepts are not rejected here.
fn validate_cat_hub_bind(bind: &str, name: &str) -> Result<(), String> {
    let (host, port) = bind.rsplit_once(':').ok_or_else(|| {
        format!("CAT hub hamlib_net endpoint '{name}' bind must be in host:port form.")
    })?;
    if host.is_empty() {
        return Err(format!(
            "CAT hub hamlib_net endpoint '{name}' bind must include a host."
        ));
    }
    let port: u32 = port
        .parse()
        .map_err(|_| format!("CAT hub hamlib_net endpoint '{name}' bind port is not a number."))?;
    if port == 0 || port > u32::from(u16::MAX) {
        return Err(format!(
            "CAT hub hamlib_net endpoint '{name}' bind port must be between 1 and 65535."
        ));
    }
    Ok(())
}

fn validate_host_port(bind: &str, label: &str) -> Result<(), String> {
    let (host, port) = bind
        .rsplit_once(':')
        .ok_or_else(|| format!("{label} must be in host:port form."))?;
    if host.trim().is_empty() {
        return Err(format!("{label} must include a host."));
    }
    let port: u32 = port
        .parse()
        .map_err(|_| format!("{label} port is not a number."))?;
    if port == 0 || port > u32::from(u16::MAX) {
        return Err(format!("{label} port must be between 1 and 65535."));
    }
    Ok(())
}

fn first_duplicate<'a>(values: &[&'a str]) -> Option<&'a str> {
    let mut seen: Vec<&str> = Vec::with_capacity(values.len());
    for value in values {
        if seen
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(value))
        {
            return Some(value);
        }
        seen.push(value);
    }
    None
}

/// Leniently load the `[cat_hub]` section from the unified config for display.
/// Any parse failure yields `None` (and a logged warning) instead of failing the
/// whole engine, mirroring how the engine never hard-depends on `[cat_hub]`.
fn load_cat_hub_config(config_path: &Path) -> Option<PersistedCatHubConfig> {
    let content = fs::read_to_string(config_path).ok()?;
    let document = match content.parse::<toml::Table>() {
        Ok(document) => document,
        Err(error) => {
            eprintln!("Warning: failed to parse config while reading [cat_hub] section: {error}");
            return None;
        }
    };
    let section = document.get("cat_hub")?;
    match section.clone().try_into::<PersistedCatHubConfig>() {
        Ok(config) => Some(config),
        Err(error) => {
            eprintln!("Warning: ignoring malformed [cat_hub] section for setup status: {error}");
            None
        }
    }
}

pub(crate) fn default_config_path() -> Result<PathBuf, String> {
    #[cfg(target_os = "windows")]
    {
        let app_data = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .ok_or_else(|| {
                "APPDATA is not set; cannot resolve the default config path.".to_string()
            })?;
        Ok(app_data.join("qsoripper").join(DEFAULT_CONFIG_FILE_NAME))
    }

    #[cfg(not(target_os = "windows"))]
    {
        if let Some(xdg_config_home) = std::env::var_os("XDG_CONFIG_HOME") {
            return Ok(PathBuf::from(xdg_config_home)
                .join("qsoripper")
                .join(DEFAULT_CONFIG_FILE_NAME));
        }

        let home = std::env::var_os("HOME").map(PathBuf::from).ok_or_else(|| {
            "HOME is not set; cannot resolve the default config path.".to_string()
        })?;
        Ok(home
            .join(".config")
            .join("qsoripper")
            .join(DEFAULT_CONFIG_FILE_NAME))
    }
}

fn suggested_log_file_path(config_path: &Path) -> PathBuf {
    config_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(DEFAULT_LOG_FILE_NAME)
}

fn load_persisted_config(config_path: &Path) -> Result<Option<PersistedSetupConfig>, String> {
    if !config_path.exists() {
        return Ok(None);
    }

    let content = fs::read_to_string(config_path)
        .map_err(|error| format!("Failed to read config '{}': {error}", config_path.display()))?;
    let mut config = toml::from_str::<PersistedSetupConfig>(&content).map_err(|error| {
        format!(
            "Failed to parse config '{}': {error}",
            config_path.display()
        )
    })?;
    let legacy_station_profile = config.station_profile.clone();
    config
        .station_profiles
        .bootstrap_from_legacy(&legacy_station_profile);
    config.sync_active_station_profile();
    Ok(Some(config))
}

/// Top-level TOML keys owned by the engine setup config. On save these are replaced
/// wholesale. The `[cat_hub]` and `[wsjtx_ingest]` sections are CONDITIONALLY
/// engine-managed: they are preserved verbatim when a save does not touch them, and
/// only removed-and-rewritten when the caller supplies a complete replacement (see
/// `write_persisted_config`). Every other
/// unknown top-level table (for example `[launcher]` written by the launcher) is always
/// preserved untouched so the unified `config.toml` can be shared across all components.
const ENGINE_OWNED_CONFIG_KEYS: [&str; 8] = [
    "logbook",
    "storage",
    "station_profile",
    "station_profiles",
    "qrz_xml",
    "qrz_logbook",
    "sync",
    "rig_control",
];

fn write_persisted_config(
    config_path: &Path,
    config: &PersistedSetupConfig,
    cat_hub_update: Option<&PersistedCatHubConfig>,
    wsjtx_ingest_update: Option<&PersistedWsjtxIngestConfig>,
) -> Result<(), String> {
    if let Some(parent) = config_path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            format!(
                "Failed to create config directory '{}': {error}",
                parent.display()
            )
        })?;
    }

    // Serialize only the engine-owned config, then splice it into the existing document so
    // unknown top-level tables survive. Engine-owned tables are canonicalized (their inline
    // comments/order are not preserved); unknown tables are preserved verbatim.
    let owned_text = toml::to_string_pretty(config).map_err(|error| {
        format!(
            "Failed to serialize persisted setup config '{}': {error}",
            config_path.display()
        )
    })?;
    let owned_doc: toml_edit::DocumentMut = owned_text.parse().map_err(|error| {
        format!(
            "Failed to re-parse serialized setup config '{}': {error}",
            config_path.display()
        )
    })?;

    let mut doc: toml_edit::DocumentMut = match fs::read_to_string(config_path) {
        Ok(existing) => existing.parse().map_err(|error| {
            format!(
                "Failed to parse existing config '{}': {error}",
                config_path.display()
            )
        })?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => toml_edit::DocumentMut::new(),
        Err(error) => {
            return Err(format!(
                "Failed to read existing config '{}': {error}",
                config_path.display()
            ));
        }
    };

    // Drop every engine-owned key (clearing tables the wizard intentionally emptied), then
    // re-insert exactly the keys the serializer produced.
    for key in ENGINE_OWNED_CONFIG_KEYS {
        doc.remove(key);
    }
    for (key, item) in owned_doc.iter() {
        if key == "wsjtx_ingest" || key == "cw_keying" {
            continue;
        }
        doc.insert(key, item.clone());
    }

    // Only touch `[cat_hub]` when the caller supplied a complete replacement; otherwise
    // leave whatever the cathub daemon wrote (comments, ordering, unknown keys) untouched.
    if let Some(cat_hub) = cat_hub_update {
        splice_cat_hub_section(&mut doc, cat_hub, config_path)?;
    }
    if let Some(wsjtx_ingest) = wsjtx_ingest_update {
        splice_wsjtx_ingest_section(&mut doc, wsjtx_ingest, config_path)?;
    }

    fs::write(config_path, doc.to_string()).map_err(|error| {
        format!(
            "Failed to write config '{}': {error}",
            config_path.display()
        )
    })
}

fn splice_wsjtx_ingest_section(
    doc: &mut toml_edit::DocumentMut,
    wsjtx_ingest: &PersistedWsjtxIngestConfig,
    config_path: &Path,
) -> Result<(), String> {
    let owned_text = toml::to_string_pretty(&PersistedSetupConfig {
        wsjtx_ingest: wsjtx_ingest.clone(),
        ..PersistedSetupConfig::default()
    })
    .map_err(|error| {
        format!(
            "Failed to serialize WSJT-X ingest config '{}': {error}",
            config_path.display()
        )
    })?;
    let owned_doc: toml_edit::DocumentMut = owned_text.parse().map_err(|error| {
        format!(
            "Failed to re-parse WSJT-X ingest config '{}': {error}",
            config_path.display()
        )
    })?;
    doc.remove("wsjtx_ingest");
    if let Some(item) = owned_doc.get("wsjtx_ingest") {
        doc.insert("wsjtx_ingest", item.clone());
    }
    Ok(())
}

#[derive(Serialize)]
struct CatHubDocument<'a> {
    cat_hub: &'a PersistedCatHubConfig,
}

fn splice_cat_hub_section(
    doc: &mut toml_edit::DocumentMut,
    cat_hub: &PersistedCatHubConfig,
    config_path: &Path,
) -> Result<(), String> {
    doc.remove("cat_hub");
    let wrapped = toml::to_string_pretty(&CatHubDocument { cat_hub }).map_err(|error| {
        format!(
            "Failed to serialize CAT hub config for '{}': {error}",
            config_path.display()
        )
    })?;
    let wrapped_doc: toml_edit::DocumentMut = wrapped.parse().map_err(|error| {
        format!(
            "Failed to re-parse serialized CAT hub config for '{}': {error}",
            config_path.display()
        )
    })?;
    if let Some(item) = wrapped_doc.get("cat_hub") {
        doc.insert("cat_hub", item.clone());
    }
    Ok(())
}

fn build_status(
    config_path: &Path,
    suggested_log_file_path: &Path,
    persisted_config: Option<&PersistedSetupConfig>,
    cat_hub: Option<&PersistedCatHubConfig>,
) -> SetupStatus {
    let warnings = build_warnings(persisted_config);
    let station_profile = persisted_config.and_then(PersistedSetupConfig::station_profile);
    let log_file_path = persisted_config.and_then(PersistedSetupConfig::log_file_path);
    let persistence_has_value = log_file_path.is_some();
    let persistence_display_value = log_file_path
        .clone()
        .unwrap_or_else(|| suggested_log_file_path.display().to_string());
    let storage_backend = persisted_config.map_or(
        StorageBackend::Unspecified,
        PersistedSetupConfig::runtime_storage_backend,
    );

    #[allow(deprecated)]
    SetupStatus {
        config_file_exists: persisted_config.is_some(),
        setup_complete: persisted_config.is_some() && warnings.is_empty(),
        config_path: config_path.display().to_string(),
        storage_backend: storage_backend as i32,
        sqlite_path: log_file_path.clone(),
        has_station_profile: station_profile.is_some(),
        station_profile,
        qrz_xml_username: persisted_config.and_then(|config| config.qrz_xml.username.clone()),
        has_qrz_xml_password: persisted_config
            .and_then(|config| config.qrz_xml.password.as_ref())
            .is_some(),
        suggested_sqlite_path: suggested_log_file_path.display().to_string(),
        warnings,
        active_station_profile_id: persisted_config
            .and_then(PersistedSetupConfig::active_station_profile_id),
        station_profile_count: persisted_config.map_or(0, |config| {
            u32::try_from(config.station_profile_count()).unwrap_or(u32::MAX)
        }),
        log_file_path,
        suggested_log_file_path: suggested_log_file_path.display().to_string(),
        is_first_run: persisted_config.is_none(),
        has_qrz_logbook_api_key: persisted_config
            .and_then(|config| config.qrz_logbook.api_key.as_ref())
            .is_some(),
        sync_config: persisted_config.map(|config| config.sync.to_proto()),
        rig_control: persisted_config.and_then(|config| config.rig_control.to_proto()),
        cat_hub: cat_hub.and_then(PersistedCatHubConfig::to_proto),
        wsjtx_ingest: persisted_config.and_then(|config| config.wsjtx_ingest.to_proto()),
        wsjtx_ingest_status: None,
        persistence_step_enabled: true,
        persistence_label: PERSISTENCE_STEP_LABEL.to_string(),
        persistence_description: PERSISTENCE_STEP_DESCRIPTION.to_string(),
        persistence_definitions: vec![RuntimeConfigDefinition {
            key: PERSISTENCE_PATH_KEY.to_string(),
            label: "Log file path".to_string(),
            description: "Path to the SQLite logbook file used by the Rust engine.".to_string(),
            kind: qsoripper_core::proto::qsoripper::services::RuntimeConfigValueKind::Path.into(),
            secret: false,
            allowed_values: Vec::new(),
            required: true,
        }],
        persistence_values: vec![RuntimeConfigValue {
            key: PERSISTENCE_PATH_KEY.to_string(),
            has_value: persistence_has_value,
            display_value: persistence_display_value,
            overridden: false,
            secret: false,
            redacted: false,
        }],
        persistence_contract_explicit: true,
    }
}

fn build_warnings(persisted_config: Option<&PersistedSetupConfig>) -> Vec<String> {
    let Some(config) = persisted_config else {
        return vec!["No persisted QsoRipper setup exists yet.".to_string()];
    };

    let mut warnings = Vec::new();
    let log_file_path = config.log_file_path();

    if log_file_path.is_none() {
        if matches!(config.legacy_storage_backend(), StorageBackend::Memory) {
            warnings.push(
                "Persisted setup still uses legacy in-memory storage; save a log file path to migrate to the backend-agnostic setup model."
                    .to_string(),
            );
        } else {
            warnings.push("Persisted setup is missing a log_file_path.".to_string());
        }
    }

    if config.station_profile().is_none() {
        warnings.push("Persisted setup is missing a valid station profile.".to_string());
    }
    if config.station_profile_count() > 0 && config.active_station_profile_id().is_none() {
        warnings
            .push("Persisted setup is missing an active station profile selection.".to_string());
    }

    warnings
}

fn build_wizard_steps(
    persisted_config: Option<&PersistedSetupConfig>,
    cat_hub: Option<&PersistedCatHubConfig>,
) -> Vec<SetupWizardStepStatus> {
    vec![
        build_log_file_step(persisted_config),
        build_station_profiles_step(persisted_config),
        build_qrz_integration_step(persisted_config),
        build_cat_hub_step(cat_hub),
        build_review_step(persisted_config),
    ]
}

fn build_cat_hub_step(cat_hub: Option<&PersistedCatHubConfig>) -> SetupWizardStepStatus {
    // CAT hub configuration is entirely optional. The step is always complete:
    // operators who do not run the multi-client hub simply leave it unconfigured,
    // and any supplied configuration is fully validated on save.
    let _ = cat_hub;
    SetupWizardStepStatus {
        step: SetupWizardStep::CatHub.into(),
        complete: true,
        issues: Vec::new(),
    }
}

fn build_log_file_step(config: Option<&PersistedSetupConfig>) -> SetupWizardStepStatus {
    let log_file = config.and_then(PersistedSetupConfig::log_file_path);
    let complete = log_file.is_some();
    let issues = if complete {
        Vec::new()
    } else {
        vec!["A log file path is required.".to_string()]
    };
    SetupWizardStepStatus {
        step: SetupWizardStep::LogFile.into(),
        complete,
        issues,
    }
}

fn build_station_profiles_step(config: Option<&PersistedSetupConfig>) -> SetupWizardStepStatus {
    let has_profiles = config.is_some_and(|c| c.station_profile_count() > 0);
    let has_active = config
        .and_then(PersistedSetupConfig::active_station_profile_id)
        .is_some();
    let complete = has_profiles && has_active;
    let mut issues = Vec::new();
    if !has_profiles {
        issues.push("At least one station profile is required.".to_string());
    }
    if has_profiles && !has_active {
        issues.push("An active station profile must be selected.".to_string());
    }
    SetupWizardStepStatus {
        step: SetupWizardStep::StationProfiles.into(),
        complete,
        issues,
    }
}

fn build_qrz_integration_step(_config: Option<&PersistedSetupConfig>) -> SetupWizardStepStatus {
    // QRZ integration is entirely optional.  The step is always complete:
    //   - no config → user hasn't configured QRZ yet, which is fine.
    //   - config exists, no username → user intentionally skipped QRZ.
    //   - config exists, username set → password is supplied via env var at runtime.
    SetupWizardStepStatus {
        step: SetupWizardStep::QrzIntegration.into(),
        complete: true,
        issues: Vec::new(),
    }
}

fn build_review_step(config: Option<&PersistedSetupConfig>) -> SetupWizardStepStatus {
    let all_prior_complete = config.is_some()
        && config
            .and_then(PersistedSetupConfig::log_file_path)
            .is_some()
        && config.is_some_and(|c| c.station_profile_count() > 0)
        && config
            .and_then(PersistedSetupConfig::active_station_profile_id)
            .is_some();
    let issues = if all_prior_complete {
        Vec::new()
    } else {
        vec!["Complete the previous steps before reviewing.".to_string()]
    };
    SetupWizardStepStatus {
        step: SetupWizardStep::Review.into(),
        complete: all_prior_complete,
        issues,
    }
}

fn validate_step(
    step: SetupWizardStep,
    request: &ValidateSetupStepRequest,
) -> ValidateSetupStepResponse {
    match step {
        SetupWizardStep::LogFile => validate_log_file_step(request),
        SetupWizardStep::StationProfiles => validate_station_profiles_step(request),
        SetupWizardStep::QrzIntegration => validate_qrz_step(request),
        SetupWizardStep::CatHub | SetupWizardStep::Review | SetupWizardStep::Unspecified => {
            ValidateSetupStepResponse {
                valid: true,
                fields: Vec::new(),
            }
        }
    }
}

fn validate_log_file_step(request: &ValidateSetupStepRequest) -> ValidateSetupStepResponse {
    let path = setup_validation_persistence_path(request)
        .or_else(|| normalize_optional_string(request.log_file_path.as_deref()));
    let (valid, message) = match &path {
        Some(p) => {
            let parent = Path::new(p.as_str()).parent();
            match parent {
                Some(d) if !d.as_os_str().is_empty() && !d.exists() => (
                    false,
                    format!("Parent directory '{}' does not exist.", d.display()),
                ),
                _ => (true, String::new()),
            }
        }
        None => (false, "A log file path is required.".to_string()),
    };
    ValidateSetupStepResponse {
        valid,
        fields: vec![SetupFieldValidation {
            field: PERSISTENCE_PATH_KEY.to_string(),
            valid,
            message,
        }],
    }
}

fn setup_request_persistence_path(request: &SaveSetupRequest) -> Option<String> {
    request
        .persistence_values
        .iter()
        .find(|field| field.key.eq_ignore_ascii_case(PERSISTENCE_PATH_KEY))
        .and_then(|field| normalize_optional_string(field.value.as_deref()))
}

fn setup_validation_persistence_path(request: &ValidateSetupStepRequest) -> Option<String> {
    request
        .persistence_values
        .iter()
        .find(|field| field.key.eq_ignore_ascii_case(PERSISTENCE_PATH_KEY))
        .and_then(|field| normalize_optional_string(field.value.as_deref()))
}

fn validate_station_profiles_step(request: &ValidateSetupStepRequest) -> ValidateSetupStepResponse {
    let profile = request.station_profile.as_ref();
    let mut fields = Vec::new();

    let callsign_valid = profile
        .and_then(|p| normalize_optional_string(Some(p.station_callsign.as_str())))
        .is_some();
    fields.push(SetupFieldValidation {
        field: "station_callsign".to_string(),
        valid: callsign_valid,
        message: if callsign_valid {
            String::new()
        } else {
            "Station callsign is required.".to_string()
        },
    });

    let name_valid = profile
        .and_then(|p| normalize_optional_string(p.profile_name.as_deref()))
        .is_some();
    fields.push(SetupFieldValidation {
        field: "profile_name".to_string(),
        valid: name_valid,
        message: if name_valid {
            String::new()
        } else {
            "Profile name is required.".to_string()
        },
    });

    let operator_valid = profile
        .and_then(|p| normalize_optional_string(p.operator_callsign.as_deref()))
        .is_some();
    fields.push(SetupFieldValidation {
        field: "operator_callsign".to_string(),
        valid: operator_valid,
        message: if operator_valid {
            String::new()
        } else {
            "Operator callsign is required.".to_string()
        },
    });

    let grid_valid = profile
        .and_then(|p| normalize_optional_string(p.grid.as_deref()))
        .is_some();
    fields.push(SetupFieldValidation {
        field: "grid".to_string(),
        valid: grid_valid,
        message: if grid_valid {
            String::new()
        } else {
            "Grid square is required.".to_string()
        },
    });

    let all_valid = callsign_valid && name_valid && operator_valid && grid_valid;
    ValidateSetupStepResponse {
        valid: all_valid,
        fields,
    }
}

fn validate_qrz_step(request: &ValidateSetupStepRequest) -> ValidateSetupStepResponse {
    let username = normalize_optional_string(request.qrz_xml_username.as_deref());
    let mut fields = Vec::new();

    // Username-only is valid; the password is supplied via env var at runtime.
    fields.push(SetupFieldValidation {
        field: "qrz_xml_username".to_string(),
        valid: true,
        message: String::new(),
    });

    fields.push(SetupFieldValidation {
        field: "qrz_xml_password".to_string(),
        valid: true,
        message: if username.is_some() {
            "Password must be set via QSORIPPER_QRZ_XML_PASSWORD environment variable.".to_string()
        } else {
            String::new()
        },
    });

    ValidateSetupStepResponse {
        valid: true,
        fields,
    }
}

async fn test_qrz_login(
    username: &str,
    password: &str,
    runtime_config: &RuntimeConfigManager,
) -> TestQrzCredentialsResponse {
    let username = username.trim();
    let password = password.trim();

    if username.is_empty() || password.is_empty() {
        return TestQrzCredentialsResponse {
            success: false,
            error_message: "Username and password are both required.".to_string(),
        };
    }

    // Build a temporary QRZ config using the test credentials plus the current
    // runtime user-agent setting (required by QRZ to identify clients).
    let effective = runtime_config.effective_values().await;
    let user_agent = effective
        .get(QRZ_USER_AGENT_ENV_VAR)
        .cloned()
        .unwrap_or_else(|| format!("QsoRipper/0.1.0 ({username})"));

    let config = QrzXmlConfig::from_value_provider(|name| match name {
        n if n == QRZ_XML_USERNAME_ENV_VAR => Some(username.to_string()),
        n if n == QRZ_XML_PASSWORD_ENV_VAR => Some(password.to_string()),
        n if n == QRZ_USER_AGENT_ENV_VAR => Some(user_agent.clone()),
        _ => effective.get(name).cloned(),
    });

    let config = match config {
        Ok(c) => c,
        Err(error) => {
            return TestQrzCredentialsResponse {
                success: false,
                error_message: format!("Invalid QRZ configuration: {error}"),
            };
        }
    };

    let provider = match QrzXmlProvider::new(config) {
        Ok(p) => p,
        Err(error) => {
            return TestQrzCredentialsResponse {
                success: false,
                error_message: format!("Failed to create QRZ provider: {error}"),
            };
        }
    };

    match provider.test_login().await {
        Ok(()) => TestQrzCredentialsResponse {
            success: true,
            error_message: String::new(),
        },
        Err(error) => TestQrzCredentialsResponse {
            success: false,
            error_message: format!("{error}"),
        },
    }
}

async fn test_qrz_logbook_api_key(
    api_key: &str,
    runtime_config: &RuntimeConfigManager,
) -> TestQrzLogbookCredentialsResponse {
    let api_key = api_key.trim();
    if api_key.is_empty() {
        return TestQrzLogbookCredentialsResponse {
            success: false,
            error_message: "API key is required.".to_string(),
            qso_count: None,
            logbook_owner: None,
        };
    }

    let effective = runtime_config.effective_values().await;
    let base_url = effective
        .get(QRZ_LOGBOOK_BASE_URL_ENV_VAR)
        .cloned()
        .unwrap_or_else(|| DEFAULT_QRZ_LOGBOOK_BASE_URL.to_string());

    let config = QrzLogbookConfig::new(api_key.to_string(), base_url, "QsoRipper/1.0".to_string());

    let client = match QrzLogbookClient::new(config) {
        Ok(c) => c,
        Err(error) => {
            return TestQrzLogbookCredentialsResponse {
                success: false,
                error_message: format!("Failed to create logbook client: {error}"),
                qso_count: None,
                logbook_owner: None,
            };
        }
    };

    match client.test_connection().await {
        Ok(status) => TestQrzLogbookCredentialsResponse {
            success: true,
            error_message: String::new(),
            qso_count: Some(status.qso_count),
            logbook_owner: Some(status.owner),
        },
        Err(error) => TestQrzLogbookCredentialsResponse {
            success: false,
            error_message: format!("{error}"),
            qso_count: None,
            logbook_owner: None,
        },
    }
}

fn normalize_optional_string(value: Option<&str>) -> Option<String> {
    let trimmed = value?.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn normalize_optional_callsign(value: Option<&str>) -> Option<String> {
    let trimmed = value?.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(normalize_callsign(trimmed))
    }
}

fn normalize_profile_id(raw_value: &str) -> String {
    let mut normalized = String::new();
    let mut previous_was_separator = false;
    for character in raw_value.chars().flat_map(char::to_lowercase) {
        if character.is_ascii_alphanumeric() {
            normalized.push(character);
            previous_was_separator = false;
        } else if !previous_was_separator {
            normalized.push('-');
            previous_was_separator = true;
        }
    }

    normalized.trim_matches('-').to_string()
}

fn generate_profile_id(
    requested_profile_id: Option<&str>,
    profile: &StationProfile,
    existing_ids: &[String],
) -> String {
    let mut base = requested_profile_id
        .map(normalize_profile_id)
        .filter(|value| !value.is_empty())
        .or_else(|| {
            profile
                .profile_name
                .as_deref()
                .map(normalize_profile_id)
                .filter(|value| !value.is_empty())
        })
        .or_else(|| {
            Some(normalize_profile_id(&profile.station_callsign)).filter(|value| !value.is_empty())
        })
        .unwrap_or_else(|| DEFAULT_PROFILE_NAME.to_ascii_lowercase());

    if !existing_ids.iter().any(|existing_id| existing_id == &base) {
        return base;
    }

    let original = base.clone();
    let mut suffix = 2_u32;
    while existing_ids.iter().any(|existing_id| existing_id == &base) {
        base = format!("{original}-{suffix}");
        suffix += 1;
    }

    base
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    deprecated
)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::sync::Arc;
    use std::time::{SystemTime, UNIX_EPOCH};

    use tonic::Request;

    use super::{
        build_cat_hub_step, build_log_file_step, build_qrz_integration_step, build_review_step,
        build_station_profiles_step, build_wizard_steps, default_config_path, load_cat_hub_config,
        load_persisted_config, suggested_log_file_path, validate_log_file_step, validate_qrz_step,
        validate_station_profiles_step, write_persisted_config, PersistedCatHubConfig,
        PersistedSetupConfig, SetupControlSurface, SetupState, StationProfileControlSurface,
        CW_CATHUB_CLIENT_NAME_ENV_VAR, CW_CATHUB_ENDPOINT_ENV_VAR, CW_KEYER_BACKEND_ENV_VAR,
        CW_MAX_TX_MS_ENV_VAR, CW_SPEED_WPM_ENV_VAR, CW_TRANSMIT_ENABLED_ENV_VAR,
        CW_WINKEYER_BAUD_ENV_VAR, CW_WINKEYER_PORT_ENV_VAR, DEFAULT_CONFIG_FILE_NAME,
        RIGCTLD_ENABLED_ENV_VAR, RIGCTLD_HOST_ENV_VAR, RIGCTLD_PORT_ENV_VAR,
        RIGCTLD_READ_TIMEOUT_MS_ENV_VAR, RIGCTLD_STALE_THRESHOLD_MS_ENV_VAR,
    };
    use crate::runtime_config::RuntimeConfigManager;
    use qsoripper_core::proto::qsoripper::domain::{ConflictPolicy, StationProfile, SyncConfig};
    use qsoripper_core::proto::qsoripper::services::{
        setup_service_server::SetupService, station_profile_service_server::StationProfileService,
        CatHubEventSettings, CatHubHamlibNetEndpoint, CatHubPermission, CatHubPollSettings,
        CatHubPttSettings, CatHubRadioSettings, CatHubSerialFace, CatHubSettings,
        CatHubWinkeyerFace, CatHubWinkeyerSettings, GetActiveStationContextRequest,
        GetSetupStatusRequest, GetSetupWizardStateRequest, ListStationProfilesRequest,
        RigControlSettings, SaveSetupRequest, SaveStationProfileRequest,
        SetActiveStationProfileRequest, SetSessionStationProfileOverrideRequest, SetupWizardStep,
        StorageBackend, ValidateSetupStepRequest, WinkeyerFacePermission, WsjtxIngestSettings,
    };

    fn unique_config_path() -> std::path::PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        std::env::temp_dir()
            .join(format!(
                "qsoripper-setup-test-{}-{suffix}",
                std::process::id()
            ))
            .join(DEFAULT_CONFIG_FILE_NAME)
    }

    fn absolute_log_file_path(config_path: &std::path::Path, file_name: &str) -> String {
        config_path
            .parent()
            .expect("config directory")
            .join(file_name)
            .display()
            .to_string()
    }

    #[tokio::test]
    async fn get_setup_status_reports_missing_config() {
        let config_path = unique_config_path();
        let setup_state = Arc::new(SetupState::load(config_path.clone()).expect("setup state"));
        let runtime_config = Arc::new(RuntimeConfigManager::new(BTreeMap::new()).expect("runtime"));
        let service = SetupControlSurface::new(setup_state, runtime_config);

        let status =
            SetupService::get_setup_status(&service, Request::new(GetSetupStatusRequest {}))
                .await
                .expect("status")
                .into_inner()
                .status
                .expect("status payload");

        assert!(!status.config_file_exists);
        assert!(!status.setup_complete);
        assert_eq!(config_path.display().to_string(), status.config_path);
        assert_eq!(
            suggested_log_file_path(&config_path).display().to_string(),
            status.suggested_log_file_path
        );
        assert_eq!(status.suggested_log_file_path, status.suggested_sqlite_path);
        assert!(status
            .warnings
            .contains(&"No persisted QsoRipper setup exists yet.".to_string()));
    }

    #[tokio::test]
    async fn get_setup_status_reads_legacy_sqlite_storage_as_log_file_path() {
        let config_path = unique_config_path();
        let config_directory = config_path.parent().expect("config directory");
        fs::create_dir_all(config_directory).expect("create config directory");
        fs::write(
            &config_path,
            r#"[storage]
backend = "sqlite"
sqlite_path = 'legacy\portable.db'

[station_profile]
station_callsign = "K7RND"
"#,
        )
        .expect("write legacy config");

        let setup_state = Arc::new(SetupState::load(config_path.clone()).expect("setup state"));
        let runtime_config = Arc::new(RuntimeConfigManager::new(BTreeMap::new()).expect("runtime"));
        let service = SetupControlSurface::new(setup_state, runtime_config);

        let status =
            SetupService::get_setup_status(&service, Request::new(GetSetupStatusRequest {}))
                .await
                .expect("status")
                .into_inner()
                .status
                .expect("status payload");

        assert!(status.config_file_exists);
        assert!(status.setup_complete);
        assert_eq!(StorageBackend::Sqlite as i32, status.storage_backend);
        assert_eq!(Some("legacy\\portable.db"), status.log_file_path.as_deref());
        assert_eq!(status.log_file_path, status.sqlite_path);
        assert!(status.warnings.is_empty());

        fs::remove_dir_all(config_directory).expect("remove temp config directory");
    }

    #[tokio::test]
    async fn get_setup_status_flags_legacy_memory_setup_for_migration() {
        let config_path = unique_config_path();
        let config_directory = config_path.parent().expect("config directory");
        fs::create_dir_all(config_directory).expect("create config directory");
        fs::write(
            &config_path,
            r#"[storage]
backend = "memory"

[station_profile]
station_callsign = "K7RND"
"#,
        )
        .expect("write legacy config");

        let setup_state = Arc::new(SetupState::load(config_path.clone()).expect("setup state"));
        let runtime_config = Arc::new(RuntimeConfigManager::new(BTreeMap::new()).expect("runtime"));
        let service = SetupControlSurface::new(setup_state, runtime_config);

        let status =
            SetupService::get_setup_status(&service, Request::new(GetSetupStatusRequest {}))
                .await
                .expect("status")
                .into_inner()
                .status
                .expect("status payload");

        assert!(status.config_file_exists);
        assert!(!status.setup_complete);
        assert_eq!(StorageBackend::Memory as i32, status.storage_backend);
        assert!(status.log_file_path.is_none());
        assert!(status
            .warnings
            .iter()
            .any(|warning| warning.contains("legacy in-memory storage")));

        fs::remove_dir_all(config_directory).expect("remove temp config directory");
    }

    #[tokio::test]
    async fn save_setup_persists_config_and_hot_applies_runtime_values() {
        let config_path = unique_config_path();
        let log_file_path = absolute_log_file_path(&config_path, "portable.db");
        let setup_state = Arc::new(SetupState::load(config_path.clone()).expect("setup state"));
        let runtime_config = Arc::new(RuntimeConfigManager::new(BTreeMap::new()).expect("runtime"));
        let service = SetupControlSurface::new(setup_state.clone(), runtime_config.clone());

        let response = SetupService::save_setup(
            &service,
            Request::new(SaveSetupRequest {
                storage_backend: StorageBackend::Unspecified as i32,
                sqlite_path: None,
                log_file_path: Some(log_file_path.clone()),
                station_profile: Some(StationProfile {
                    station_callsign: "k7rnd".to_string(),
                    operator_name: Some("Randy".to_string()),
                    arrl_section: Some("WWA".to_string()),
                    ..StationProfile::default()
                }),
                qrz_xml_username: Some("k7rnd".to_string()),
                qrz_xml_password: Some("secret".to_string()),
                ..Default::default()
            }),
        )
        .await
        .expect("save setup")
        .into_inner();

        let status = response.status.expect("status payload");
        let station_profile = status.station_profile.as_ref().expect("station profile");
        assert!(status.config_file_exists);
        assert!(status.setup_complete);
        assert_eq!(StorageBackend::Sqlite as i32, status.storage_backend);
        assert_eq!(
            Some(log_file_path.as_str()),
            status.log_file_path.as_deref()
        );
        assert_eq!(status.log_file_path, status.sqlite_path);
        assert_eq!(Some("Home"), station_profile.profile_name.as_deref());
        assert_eq!("K7RND", station_profile.station_callsign);
        assert_eq!(Some("WWA"), station_profile.arrl_section.as_deref());
        assert!(config_path.exists());
        let saved_config = fs::read_to_string(&config_path).expect("saved config");
        let parsed_config =
            toml::from_str::<PersistedSetupConfig>(&saved_config).expect("parse saved config");
        assert_eq!(
            Some(log_file_path.as_str()),
            parsed_config.logbook.file_path.as_deref()
        );
        assert_eq!(
            Some("WWA"),
            parsed_config.station_profile.arrl_section.as_deref()
        );
        assert!(parsed_config.storage.backend.is_none());
        assert!(parsed_config.storage.sqlite_path.is_none());

        let runtime_snapshot = runtime_config.snapshot().await;
        assert_eq!("sqlite", runtime_snapshot.active_storage_backend);
        assert_eq!(
            Some("K7RND"),
            runtime_snapshot
                .active_station_profile
                .as_ref()
                .map(|profile| profile.station_callsign.as_str())
        );

        drop(service);
        drop(runtime_config);
        drop(setup_state);

        let config_directory = config_path.parent().expect("config directory");
        fs::remove_dir_all(config_directory).expect("remove temp config directory");
    }

    #[allow(clippy::too_many_lines)]
    #[tokio::test]
    async fn save_setup_preserves_existing_station_profiles() {
        let config_path = unique_config_path();
        let initial_log_file_path = absolute_log_file_path(&config_path, "home.db");
        let updated_log_file_path = absolute_log_file_path(&config_path, "updated.db");
        let setup_state = Arc::new(SetupState::load(config_path.clone()).expect("setup state"));
        let runtime_config = Arc::new(RuntimeConfigManager::new(BTreeMap::new()).expect("runtime"));
        let setup_service = SetupControlSurface::new(setup_state.clone(), runtime_config.clone());
        let station_profile_service =
            StationProfileControlSurface::new(setup_state.clone(), runtime_config.clone());

        SetupService::save_setup(
            &setup_service,
            Request::new(SaveSetupRequest {
                storage_backend: StorageBackend::Unspecified as i32,
                sqlite_path: None,
                log_file_path: Some(initial_log_file_path),
                station_profile: Some(StationProfile {
                    profile_name: Some("Home".to_string()),
                    station_callsign: "k7rnd".to_string(),
                    grid: Some("CN87".to_string()),
                    ..StationProfile::default()
                }),
                qrz_xml_username: None,
                qrz_xml_password: None,
                ..Default::default()
            }),
        )
        .await
        .expect("initial setup");

        StationProfileService::save_station_profile(
            &station_profile_service,
            Request::new(SaveStationProfileRequest {
                profile_id: None,
                profile: Some(StationProfile {
                    profile_name: Some("POTA".to_string()),
                    station_callsign: "k7rnd/p".to_string(),
                    grid: Some("CN88".to_string()),
                    ..StationProfile::default()
                }),
                make_active: false,
            }),
        )
        .await
        .expect("save second profile");

        let updated = SetupService::save_setup(
            &setup_service,
            Request::new(SaveSetupRequest {
                storage_backend: StorageBackend::Unspecified as i32,
                sqlite_path: None,
                log_file_path: Some(updated_log_file_path.clone()),
                station_profile: Some(StationProfile {
                    profile_name: Some("Home Debug".to_string()),
                    station_callsign: "k7rnd".to_string(),
                    grid: Some("CN86".to_string()),
                    ..StationProfile::default()
                }),
                qrz_xml_username: Some("k7rnd".to_string()),
                qrz_xml_password: Some("secret".to_string()),
                ..Default::default()
            }),
        )
        .await
        .expect("updated setup")
        .into_inner()
        .status
        .expect("status");

        assert_eq!(StorageBackend::Sqlite as i32, updated.storage_backend);
        assert_eq!(
            Some(updated_log_file_path.as_str()),
            updated.log_file_path.as_deref()
        );
        assert_eq!(Some("home"), updated.active_station_profile_id.as_deref());
        assert_eq!(2, updated.station_profile_count);
        assert_eq!(
            Some("Home Debug"),
            updated
                .station_profile
                .as_ref()
                .and_then(|profile| profile.profile_name.as_deref())
        );
        assert_eq!(
            Some("CN86"),
            updated
                .station_profile
                .as_ref()
                .and_then(|profile| profile.grid.as_deref())
        );

        let listed = StationProfileService::list_station_profiles(
            &station_profile_service,
            Request::new(ListStationProfilesRequest {}),
        )
        .await
        .expect("list profiles")
        .into_inner();
        assert_eq!(2, listed.profiles.len());
        assert!(listed
            .profiles
            .iter()
            .any(|profile| profile.profile_id == "pota"));
        assert!(listed.profiles.iter().any(|profile| {
            profile.profile_id == "home"
                && profile
                    .profile
                    .as_ref()
                    .and_then(|value| value.profile_name.as_deref())
                    == Some("Home Debug")
        }));

        let runtime_snapshot = runtime_config.snapshot().await;
        assert_eq!("sqlite", runtime_snapshot.active_storage_backend);

        drop(station_profile_service);
        drop(setup_service);
        drop(runtime_config);
        drop(setup_state);

        let config_directory = config_path.parent().expect("config directory");
        fs::remove_dir_all(config_directory).expect("remove temp config directory");
    }

    #[tokio::test]
    async fn save_setup_accepts_username_only_qrz_credentials() {
        let config_path = unique_config_path();
        let log_file_path = absolute_log_file_path(&config_path, "partial.db");
        let setup_state = Arc::new(SetupState::load(config_path.clone()).expect("setup state"));
        let runtime_config = Arc::new(RuntimeConfigManager::new(BTreeMap::new()).expect("runtime"));
        let service = SetupControlSurface::new(setup_state.clone(), runtime_config.clone());

        let response = SetupService::save_setup(
            &service,
            Request::new(SaveSetupRequest {
                storage_backend: StorageBackend::Unspecified as i32,
                sqlite_path: None,
                log_file_path: Some(log_file_path),
                station_profile: Some(StationProfile {
                    station_callsign: "k7rnd".to_string(),
                    ..StationProfile::default()
                }),
                qrz_xml_username: Some("k7rnd".to_string()),
                qrz_xml_password: None,
                ..Default::default()
            }),
        )
        .await
        .expect("username-only save should succeed; password comes from env");

        let status = response.into_inner().status.expect("status payload");
        assert_eq!(Some("k7rnd"), status.qrz_xml_username.as_deref());
        assert!(!status.has_qrz_xml_password);

        drop(service);
        drop(runtime_config);
        drop(setup_state);
        let config_directory = config_path.parent().expect("config directory");
        let _ = fs::remove_dir_all(config_directory);
    }

    #[allow(clippy::too_many_lines)]
    #[tokio::test]
    async fn station_profile_service_lists_legacy_profile_and_supports_activation_and_session_override(
    ) {
        let config_path = unique_config_path();
        let log_file_path = absolute_log_file_path(&config_path, "station-profiles.db");
        let setup_state = Arc::new(SetupState::load(config_path.clone()).expect("setup state"));
        let runtime_config = Arc::new(RuntimeConfigManager::new(BTreeMap::new()).expect("runtime"));
        let setup_service = SetupControlSurface::new(setup_state.clone(), runtime_config.clone());
        let station_profile_service =
            StationProfileControlSurface::new(setup_state.clone(), runtime_config.clone());

        SetupService::save_setup(
            &setup_service,
            Request::new(SaveSetupRequest {
                storage_backend: StorageBackend::Unspecified as i32,
                sqlite_path: None,
                log_file_path: Some(log_file_path),
                station_profile: Some(StationProfile {
                    profile_name: Some("Home".to_string()),
                    station_callsign: "k7rnd".to_string(),
                    grid: Some("CN87".to_string()),
                    ..StationProfile::default()
                }),
                qrz_xml_username: None,
                qrz_xml_password: None,
                ..Default::default()
            }),
        )
        .await
        .expect("save setup");

        let saved = StationProfileService::save_station_profile(
            &station_profile_service,
            Request::new(SaveStationProfileRequest {
                profile_id: None,
                profile: Some(StationProfile {
                    profile_name: Some("POTA".to_string()),
                    station_callsign: "k7rnd/p".to_string(),
                    grid: Some("CN88".to_string()),
                    ..StationProfile::default()
                }),
                make_active: false,
            }),
        )
        .await
        .expect("save station profile")
        .into_inner();

        assert_eq!(Some("home"), saved.active_profile_id.as_deref());

        let listed = StationProfileService::list_station_profiles(
            &station_profile_service,
            Request::new(ListStationProfilesRequest {}),
        )
        .await
        .expect("list profiles")
        .into_inner();
        assert_eq!(2, listed.profiles.len());
        assert_eq!(Some("home"), listed.active_profile_id.as_deref());
        let portable = listed
            .profiles
            .iter()
            .find(|profile| profile.profile_id == "pota")
            .expect("portable profile");
        assert!(!portable.is_active);

        let activated = StationProfileService::set_active_station_profile(
            &station_profile_service,
            Request::new(SetActiveStationProfileRequest {
                profile_id: "pota".to_string(),
            }),
        )
        .await
        .expect("activate profile")
        .into_inner();
        assert_eq!("pota", activated.profile.expect("profile").profile_id);

        let context = StationProfileService::get_active_station_context(
            &station_profile_service,
            Request::new(GetActiveStationContextRequest {}),
        )
        .await
        .expect("active context")
        .into_inner()
        .context
        .expect("context payload");
        assert_eq!(Some("pota"), context.persisted_active_profile_id.as_deref());
        assert_eq!(
            Some("K7RND/P"),
            context
                .effective_active_profile
                .as_ref()
                .map(|profile| profile.station_callsign.as_str())
        );

        let override_context = StationProfileService::set_session_station_profile_override(
            &station_profile_service,
            Request::new(SetSessionStationProfileOverrideRequest {
                profile: Some(StationProfile {
                    profile_name: Some("Field Day".to_string()),
                    station_callsign: "k7rnd/7".to_string(),
                    grid: Some("CN85".to_string()),
                    ..StationProfile::default()
                }),
            }),
        )
        .await
        .expect("session override")
        .into_inner()
        .context
        .expect("context");
        assert!(override_context.has_session_override);
        assert_eq!(
            Some("K7RND/7"),
            override_context
                .effective_active_profile
                .as_ref()
                .map(|profile| profile.station_callsign.as_str())
        );

        drop(station_profile_service);
        drop(setup_service);
        drop(runtime_config);
        drop(setup_state);

        let config_directory = config_path.parent().expect("config directory");
        fs::remove_dir_all(config_directory).expect("remove temp config directory");
    }

    #[test]
    fn default_config_path_ends_with_standard_filename() {
        let path = default_config_path().expect("default config path");

        assert_eq!(
            Some(DEFAULT_CONFIG_FILE_NAME),
            path.file_name().and_then(|name| name.to_str())
        );
    }

    // ── Wizard step builder tests ───────────────────────────────────────────

    #[test]
    fn build_wizard_steps_none_config_yields_five_steps() {
        let steps = build_wizard_steps(None, None);
        assert_eq!(5, steps.len());
        assert_eq!(i32::from(SetupWizardStep::LogFile), steps[0].step);
        assert_eq!(i32::from(SetupWizardStep::StationProfiles), steps[1].step);
        assert_eq!(i32::from(SetupWizardStep::QrzIntegration), steps[2].step);
        assert_eq!(i32::from(SetupWizardStep::CatHub), steps[3].step);
        assert!(steps[3].complete);
        assert_eq!(i32::from(SetupWizardStep::Review), steps[4].step);
    }

    #[test]
    fn log_file_step_incomplete_when_no_config() {
        let step = build_log_file_step(None);
        assert!(!step.complete);
        assert!(!step.issues.is_empty());
    }

    #[test]
    fn log_file_step_complete_when_log_file_set() {
        let mut config = PersistedSetupConfig::default();
        config.logbook.file_path = Some("/tmp/test.db".to_string());
        let step = build_log_file_step(Some(&config));
        assert!(step.complete);
        assert!(step.issues.is_empty());
    }

    #[test]
    fn station_profiles_step_incomplete_when_no_profiles() {
        let step = build_station_profiles_step(None);
        assert!(!step.complete);
        assert!(step.issues.iter().any(|i| i.contains("At least one")));
    }

    #[test]
    fn station_profiles_step_incomplete_without_active_profile() {
        // The catalog falls back to the first entry when no explicit active ID is set,
        // so we must also verify that having no entries at all is incomplete.
        let config = PersistedSetupConfig::default();
        let step = build_station_profiles_step(Some(&config));
        assert!(!step.complete);
        assert!(step.issues.iter().any(|i| i.contains("At least one")));
    }

    #[test]
    fn station_profiles_step_complete_with_active_profile() {
        let mut config = PersistedSetupConfig::default();
        config
            .station_profiles
            .entries
            .push(super::PersistedStationProfileEntry {
                profile_id: "home".to_string(),
                profile: super::PersistedStationProfile {
                    profile_name: Some("Home".to_string()),
                    station_callsign: Some("K7RND".to_string()),
                    ..Default::default()
                },
            });
        config.station_profiles.active_profile_id = Some("home".to_string());
        let step = build_station_profiles_step(Some(&config));
        assert!(step.complete);
        assert!(step.issues.is_empty());
    }

    #[test]
    fn qrz_step_complete_when_both_absent() {
        let step = build_qrz_integration_step(None);
        assert!(step.complete);
        assert!(step.issues.is_empty());
    }

    #[test]
    fn qrz_step_complete_when_both_present() {
        let mut config = PersistedSetupConfig::default();
        config.qrz_xml.username = Some("user".to_string());
        config.qrz_xml.password = Some("pass".to_string());
        let step = build_qrz_integration_step(Some(&config));
        assert!(step.complete);
        assert!(step.issues.is_empty());
    }

    #[test]
    fn qrz_step_complete_when_only_username() {
        let mut config = PersistedSetupConfig::default();
        config.qrz_xml.username = Some("user".to_string());
        let step = build_qrz_integration_step(Some(&config));
        assert!(
            step.complete,
            "username-only is valid; password comes from env"
        );
        assert!(step.issues.is_empty());
    }

    #[test]
    fn review_step_incomplete_when_prior_steps_incomplete() {
        let step = build_review_step(None);
        assert!(!step.complete);
    }

    #[test]
    fn review_step_complete_when_all_prior_complete() {
        let mut config = PersistedSetupConfig::default();
        config.logbook.file_path = Some("/tmp/test.db".to_string());
        config
            .station_profiles
            .entries
            .push(super::PersistedStationProfileEntry {
                profile_id: "home".to_string(),
                profile: super::PersistedStationProfile {
                    profile_name: Some("Home".to_string()),
                    station_callsign: Some("K7RND".to_string()),
                    ..Default::default()
                },
            });
        config.station_profiles.active_profile_id = Some("home".to_string());
        let step = build_review_step(Some(&config));
        assert!(step.complete);
        assert!(step.issues.is_empty());
    }

    // ── Validation tests ────────────────────────────────────────────────────

    #[test]
    fn validate_log_file_step_rejects_empty_path() {
        let request = ValidateSetupStepRequest {
            step: SetupWizardStep::LogFile.into(),
            log_file_path: None,
            ..Default::default()
        };
        let result = validate_log_file_step(&request);
        assert!(!result.valid);
        assert_eq!(1, result.fields.len());
        assert!(!result.fields[0].valid);
    }

    #[test]
    fn validate_log_file_step_accepts_valid_path() {
        let dir = std::env::temp_dir();
        let path = dir.join("test-validate.db");
        let request = ValidateSetupStepRequest {
            step: SetupWizardStep::LogFile.into(),
            log_file_path: Some(path.display().to_string()),
            ..Default::default()
        };
        let result = validate_log_file_step(&request);
        assert!(result.valid);
        assert!(result.fields[0].valid);
    }

    #[test]
    fn validate_station_profiles_rejects_empty_profile() {
        let request = ValidateSetupStepRequest {
            step: SetupWizardStep::StationProfiles.into(),
            station_profile: Some(StationProfile::default()),
            ..Default::default()
        };
        let result = validate_station_profiles_step(&request);
        assert!(!result.valid);
        assert_eq!(4, result.fields.len());
    }

    #[test]
    fn validate_station_profiles_accepts_complete_profile() {
        let request = ValidateSetupStepRequest {
            step: SetupWizardStep::StationProfiles.into(),
            station_profile: Some(StationProfile {
                profile_name: Some("Home".to_string()),
                station_callsign: "K7RND".to_string(),
                operator_callsign: Some("K7RND".to_string()),
                grid: Some("CN87".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let result = validate_station_profiles_step(&request);
        assert!(result.valid);
        assert!(result.fields.iter().all(|f| f.valid));
    }

    #[test]
    fn validate_qrz_step_accepts_both_absent() {
        let request = ValidateSetupStepRequest {
            step: SetupWizardStep::QrzIntegration.into(),
            ..Default::default()
        };
        let result = validate_qrz_step(&request);
        assert!(result.valid);
    }

    #[test]
    fn validate_qrz_step_accepts_both_present() {
        let request = ValidateSetupStepRequest {
            step: SetupWizardStep::QrzIntegration.into(),
            qrz_xml_username: Some("user".to_string()),
            qrz_xml_password: Some("pass".to_string()),
            ..Default::default()
        };
        let result = validate_qrz_step(&request);
        assert!(result.valid);
    }

    #[test]
    fn validate_qrz_step_accepts_username_only() {
        let request = ValidateSetupStepRequest {
            step: SetupWizardStep::QrzIntegration.into(),
            qrz_xml_username: Some("user".to_string()),
            qrz_xml_password: None,
            ..Default::default()
        };
        let result = validate_qrz_step(&request);
        assert!(
            result.valid,
            "username-only is valid; password comes from env"
        );
    }

    // ── is_first_run tests ──────────────────────────────────────────────────

    #[tokio::test]
    async fn is_first_run_true_when_no_config() {
        let config_path = unique_config_path();
        let setup_state = Arc::new(SetupState::load(config_path.clone()).expect("setup state"));
        let runtime_config = Arc::new(RuntimeConfigManager::new(BTreeMap::new()).expect("runtime"));
        let service = SetupControlSurface::new(setup_state, runtime_config);

        let status =
            SetupService::get_setup_status(&service, Request::new(GetSetupStatusRequest {}))
                .await
                .expect("status")
                .into_inner()
                .status
                .expect("status payload");

        assert!(status.is_first_run);
    }

    #[tokio::test]
    async fn is_first_run_false_after_save() {
        let config_path = unique_config_path();
        let config_directory = config_path.parent().expect("config directory");
        fs::create_dir_all(config_directory).expect("create config directory");

        let setup_state = Arc::new(SetupState::load(config_path.clone()).expect("setup state"));
        let runtime_config = Arc::new(RuntimeConfigManager::new(BTreeMap::new()).expect("runtime"));
        let service = SetupControlSurface::new(setup_state, runtime_config);

        let log_file = absolute_log_file_path(&config_path, "test.db");
        SetupService::save_setup(
            &service,
            Request::new(SaveSetupRequest {
                log_file_path: Some(log_file),
                station_profile: Some(StationProfile {
                    profile_name: Some("Home".to_string()),
                    station_callsign: "K7RND".to_string(),
                    ..Default::default()
                }),
                ..Default::default()
            }),
        )
        .await
        .expect("save setup");

        let status =
            SetupService::get_setup_status(&service, Request::new(GetSetupStatusRequest {}))
                .await
                .expect("status")
                .into_inner()
                .status
                .expect("status payload");

        assert!(!status.is_first_run);

        drop(service);
        let _ = fs::remove_dir_all(config_directory);
    }

    // ── GetSetupWizardState RPC test ────────────────────────────────────────

    #[tokio::test]
    async fn get_wizard_state_returns_steps_and_status() {
        let config_path = unique_config_path();
        let setup_state = Arc::new(SetupState::load(config_path.clone()).expect("setup state"));
        let runtime_config = Arc::new(RuntimeConfigManager::new(BTreeMap::new()).expect("runtime"));
        let service = SetupControlSurface::new(setup_state, runtime_config);

        let response = SetupService::get_setup_wizard_state(
            &service,
            Request::new(GetSetupWizardStateRequest {}),
        )
        .await
        .expect("wizard state")
        .into_inner();

        assert!(response.status.is_some());
        assert_eq!(5, response.steps.len());
        assert!(response.station_profiles.is_empty());

        // For a fresh config, LogFile should be incomplete
        let log_step = &response.steps[0];
        assert_eq!(i32::from(SetupWizardStep::LogFile), log_step.step);
        assert!(!log_step.complete);
    }

    // ── ValidateSetupStep RPC test ──────────────────────────────────────────

    #[tokio::test]
    async fn validate_step_rpc_validates_log_file() {
        let config_path = unique_config_path();
        let setup_state = Arc::new(SetupState::load(config_path.clone()).expect("setup state"));
        let runtime_config = Arc::new(RuntimeConfigManager::new(BTreeMap::new()).expect("runtime"));
        let service = SetupControlSurface::new(setup_state, runtime_config);

        let dir = std::env::temp_dir();
        let path = dir.join("validate-rpc-test.db");
        let response = SetupService::validate_setup_step(
            &service,
            Request::new(ValidateSetupStepRequest {
                step: SetupWizardStep::LogFile.into(),
                log_file_path: Some(path.display().to_string()),
                ..Default::default()
            }),
        )
        .await
        .expect("validate")
        .into_inner();

        assert!(response.valid);
    }

    // ── TestQrzCredentials RPC test (empty creds) ───────────────────────────

    #[tokio::test]
    async fn test_qrz_credentials_rejects_empty() {
        let config_path = unique_config_path();
        let setup_state = Arc::new(SetupState::load(config_path.clone()).expect("setup state"));
        let runtime_config = Arc::new(RuntimeConfigManager::new(BTreeMap::new()).expect("runtime"));
        let service = SetupControlSurface::new(setup_state, runtime_config);

        let response = SetupService::test_qrz_credentials(
            &service,
            Request::new(
                qsoripper_core::proto::qsoripper::services::TestQrzCredentialsRequest {
                    qrz_xml_username: String::new(),
                    qrz_xml_password: String::new(),
                },
            ),
        )
        .await
        .expect("test qrz")
        .into_inner();

        assert!(!response.success);
        assert!(!response.error_message.is_empty());
    }

    // ── QRZ logbook API key and sync config tests ───────────────────────────

    #[tokio::test]
    async fn save_setup_persists_logbook_api_key_and_reports_in_status() {
        let config_path = unique_config_path();
        let log_file_path = absolute_log_file_path(&config_path, "logbook-key.db");
        let setup_state = Arc::new(SetupState::load(config_path.clone()).expect("setup state"));
        let runtime_config = Arc::new(RuntimeConfigManager::new(BTreeMap::new()).expect("runtime"));
        let service = SetupControlSurface::new(setup_state.clone(), runtime_config.clone());

        // Save with logbook API key
        let response = SetupService::save_setup(
            &service,
            Request::new(SaveSetupRequest {
                log_file_path: Some(log_file_path),
                station_profile: Some(StationProfile {
                    station_callsign: "k7rnd".to_string(),
                    ..StationProfile::default()
                }),
                qrz_logbook_api_key: Some("abc-123-logbook-key".to_string()),
                ..Default::default()
            }),
        )
        .await
        .expect("save setup")
        .into_inner();

        let status = response.status.expect("status payload");
        assert!(status.has_qrz_logbook_api_key);

        // Verify it round-trips through persisted config on disk
        let saved_toml = fs::read_to_string(&config_path).expect("read config");
        let parsed =
            toml::from_str::<PersistedSetupConfig>(&saved_toml).expect("parse saved config");
        assert_eq!(
            Some("abc-123-logbook-key"),
            parsed.qrz_logbook.api_key.as_deref()
        );

        // Verify runtime values include the key
        let runtime_values = setup_state.runtime_config_values().await;
        assert_eq!(
            Some("abc-123-logbook-key"),
            runtime_values
                .get(crate::runtime_config::QRZ_LOGBOOK_API_KEY_ENV_VAR)
                .map(String::as_str)
        );

        drop(service);
        drop(runtime_config);
        drop(setup_state);

        let config_directory = config_path.parent().expect("config directory");
        let _ = fs::remove_dir_all(config_directory);
    }

    #[tokio::test]
    async fn save_setup_without_logbook_key_reports_false() {
        let config_path = unique_config_path();
        let log_file_path = absolute_log_file_path(&config_path, "no-logbook-key.db");
        let setup_state = Arc::new(SetupState::load(config_path.clone()).expect("setup state"));
        let runtime_config = Arc::new(RuntimeConfigManager::new(BTreeMap::new()).expect("runtime"));
        let service = SetupControlSurface::new(setup_state, runtime_config);

        let response = SetupService::save_setup(
            &service,
            Request::new(SaveSetupRequest {
                log_file_path: Some(log_file_path),
                station_profile: Some(StationProfile {
                    station_callsign: "k7rnd".to_string(),
                    ..StationProfile::default()
                }),
                ..Default::default()
            }),
        )
        .await
        .expect("save setup")
        .into_inner();

        let status = response.status.expect("status payload");
        assert!(!status.has_qrz_logbook_api_key);
        // Default sync config should be present
        let sync = status.sync_config.expect("sync_config should be present");
        assert!(!sync.auto_sync_enabled);
        assert_eq!(300, sync.sync_interval_seconds);
        assert_eq!(ConflictPolicy::LastWriteWins as i32, sync.conflict_policy);

        drop(service);
        let config_directory = config_path.parent().expect("config directory");
        let _ = fs::remove_dir_all(config_directory);
    }

    #[tokio::test]
    async fn save_setup_persists_sync_config_and_round_trips() {
        let config_path = unique_config_path();
        let log_file_path = absolute_log_file_path(&config_path, "sync-config.db");
        let setup_state = Arc::new(SetupState::load(config_path.clone()).expect("setup state"));
        let runtime_config = Arc::new(RuntimeConfigManager::new(BTreeMap::new()).expect("runtime"));
        let service = SetupControlSurface::new(setup_state.clone(), runtime_config.clone());

        let response = SetupService::save_setup(
            &service,
            Request::new(SaveSetupRequest {
                log_file_path: Some(log_file_path),
                station_profile: Some(StationProfile {
                    station_callsign: "k7rnd".to_string(),
                    ..StationProfile::default()
                }),
                sync_config: Some(SyncConfig {
                    auto_sync_enabled: true,
                    sync_interval_seconds: 600,
                    conflict_policy: ConflictPolicy::FlagForReview as i32,
                }),
                ..Default::default()
            }),
        )
        .await
        .expect("save setup")
        .into_inner();

        let status = response.status.expect("status payload");
        let sync = status.sync_config.expect("sync_config");
        assert!(sync.auto_sync_enabled);
        assert_eq!(600, sync.sync_interval_seconds);
        assert_eq!(ConflictPolicy::FlagForReview as i32, sync.conflict_policy);

        // Verify persisted TOML
        let saved_toml = fs::read_to_string(&config_path).expect("read config");
        let parsed =
            toml::from_str::<PersistedSetupConfig>(&saved_toml).expect("parse saved config");
        assert!(parsed.sync.auto_sync_enabled);
        assert_eq!(600, parsed.sync.sync_interval_seconds);
        assert_eq!("flag_for_review", parsed.sync.conflict_policy);

        // Verify runtime values
        let runtime_values = setup_state.runtime_config_values().await;
        assert_eq!(
            Some("true"),
            runtime_values
                .get(crate::runtime_config::SYNC_AUTO_ENABLED_ENV_VAR)
                .map(String::as_str)
        );
        assert_eq!(
            Some("600"),
            runtime_values
                .get(crate::runtime_config::SYNC_INTERVAL_SECONDS_ENV_VAR)
                .map(String::as_str)
        );
        assert_eq!(
            Some("flag_for_review"),
            runtime_values
                .get(crate::runtime_config::SYNC_CONFLICT_POLICY_ENV_VAR)
                .map(String::as_str)
        );

        drop(service);
        drop(runtime_config);
        drop(setup_state);

        let config_directory = config_path.parent().expect("config directory");
        let _ = fs::remove_dir_all(config_directory);
    }

    #[tokio::test]
    async fn save_setup_persists_rig_control_and_round_trips() {
        let config_path = unique_config_path();
        let log_file_path = absolute_log_file_path(&config_path, "rig-control.db");
        let setup_state = Arc::new(SetupState::load(config_path.clone()).expect("setup state"));
        let runtime_config = Arc::new(RuntimeConfigManager::new(BTreeMap::new()).expect("runtime"));
        let service = SetupControlSurface::new(setup_state.clone(), runtime_config.clone());

        let response = SetupService::save_setup(
            &service,
            Request::new(SaveSetupRequest {
                log_file_path: Some(log_file_path),
                station_profile: Some(StationProfile {
                    station_callsign: "k7rnd".to_string(),
                    ..StationProfile::default()
                }),
                rig_control: Some(RigControlSettings {
                    enabled: Some(true),
                    host: Some("127.0.0.1".to_string()),
                    port: Some(4532),
                    read_timeout_ms: Some(2500),
                    stale_threshold_ms: Some(6000),
                }),
                ..Default::default()
            }),
        )
        .await
        .expect("save setup")
        .into_inner();

        let status = response.status.expect("status payload");
        let rig_control = status.rig_control.expect("rig_control");
        assert_eq!(Some(true), rig_control.enabled);
        assert_eq!(Some("127.0.0.1"), rig_control.host.as_deref());
        assert_eq!(Some(4532), rig_control.port);
        assert_eq!(Some(2500), rig_control.read_timeout_ms);
        assert_eq!(Some(6000), rig_control.stale_threshold_ms);

        let saved_toml = fs::read_to_string(&config_path).expect("read config");
        let parsed =
            toml::from_str::<PersistedSetupConfig>(&saved_toml).expect("parse saved config");
        assert_eq!(Some(true), parsed.rig_control.enabled);
        assert_eq!(Some("127.0.0.1"), parsed.rig_control.host.as_deref());
        assert_eq!(Some(4532), parsed.rig_control.port);
        assert_eq!(Some(2500), parsed.rig_control.read_timeout_ms);
        assert_eq!(Some(6000), parsed.rig_control.stale_threshold_ms);

        let runtime_values = setup_state.runtime_config_values().await;
        assert_eq!(
            Some("true"),
            runtime_values
                .get(RIGCTLD_ENABLED_ENV_VAR)
                .map(String::as_str)
        );
        assert_eq!(
            Some("127.0.0.1"),
            runtime_values.get(RIGCTLD_HOST_ENV_VAR).map(String::as_str)
        );
        assert_eq!(
            Some("4532"),
            runtime_values.get(RIGCTLD_PORT_ENV_VAR).map(String::as_str)
        );
        assert_eq!(
            Some("2500"),
            runtime_values
                .get(RIGCTLD_READ_TIMEOUT_MS_ENV_VAR)
                .map(String::as_str)
        );
        assert_eq!(
            Some("6000"),
            runtime_values
                .get(RIGCTLD_STALE_THRESHOLD_MS_ENV_VAR)
                .map(String::as_str)
        );

        drop(service);
        drop(runtime_config);
        drop(setup_state);

        let config_directory = config_path.parent().expect("config directory");
        let _ = fs::remove_dir_all(config_directory);
    }

    #[tokio::test]
    async fn save_setup_persists_wsjtx_ingest_and_round_trips() {
        let config_path = unique_config_path();
        let log_file_path = absolute_log_file_path(&config_path, "wsjtx-ingest.db");
        let adif_tail_path = absolute_log_file_path(&config_path, "wsjtx_log.adi");
        let setup_state = Arc::new(SetupState::load(config_path.clone()).expect("setup state"));
        let runtime_config = Arc::new(RuntimeConfigManager::new(BTreeMap::new()).expect("runtime"));
        let service = SetupControlSurface::new(setup_state.clone(), runtime_config.clone());

        let response = SetupService::save_setup(
            &service,
            Request::new(SaveSetupRequest {
                log_file_path: Some(log_file_path),
                station_profile: Some(StationProfile {
                    station_callsign: "k7rnd".to_string(),
                    ..StationProfile::default()
                }),
                wsjtx_ingest: Some(WsjtxIngestSettings {
                    enabled: true,
                    udp_enabled: Some(true),
                    udp_bind: "127.0.0.1:2237".to_string(),
                    adif_tail_enabled: true,
                    adif_tail_path: Some(adif_tail_path.clone()),
                    poll_interval_ms: 1500,
                    sync_to_qrz: true,
                }),
                ..Default::default()
            }),
        )
        .await
        .expect("save setup")
        .into_inner();

        let status = response.status.expect("status payload");
        let wsjtx = status.wsjtx_ingest.expect("wsjtx settings");
        assert!(wsjtx.enabled);
        assert_eq!(Some(true), wsjtx.udp_enabled);
        assert_eq!("127.0.0.1:2237", wsjtx.udp_bind);
        assert!(wsjtx.adif_tail_enabled);
        assert_eq!(
            Some(adif_tail_path.as_str()),
            wsjtx.adif_tail_path.as_deref()
        );
        assert_eq!(1500, wsjtx.poll_interval_ms);
        assert!(wsjtx.sync_to_qrz);

        let saved_toml = fs::read_to_string(&config_path).expect("read config");
        assert!(
            saved_toml.contains("[wsjtx_ingest]"),
            "wsjtx_ingest table: {saved_toml}"
        );
        let parsed =
            toml::from_str::<PersistedSetupConfig>(&saved_toml).expect("parse saved config");
        assert_eq!(Some(true), parsed.wsjtx_ingest.enabled);
        assert_eq!(
            Some("127.0.0.1:2237"),
            parsed.wsjtx_ingest.udp_bind.as_deref()
        );

        let runtime_values = setup_state.runtime_config_values().await;
        assert_eq!(
            Some("true"),
            runtime_values
                .get(crate::runtime_config::WSJTX_INGEST_ENABLED_ENV_VAR)
                .map(String::as_str)
        );
        assert_eq!(
            Some(adif_tail_path.as_str()),
            runtime_values
                .get(crate::runtime_config::WSJTX_INGEST_ADIF_TAIL_PATH_ENV_VAR)
                .map(String::as_str)
        );

        drop(service);
        drop(runtime_config);
        drop(setup_state);

        let config_directory = config_path.parent().expect("config directory");
        let _ = fs::remove_dir_all(config_directory);
    }

    #[tokio::test]
    async fn save_setup_without_wsjtx_ingest_preserves_existing_wsjtx_section_verbatim() {
        let config_path = unique_config_path();
        let log_file_path = absolute_log_file_path(&config_path, "wsjtx-preserve.db");
        fs::create_dir_all(config_path.parent().expect("config directory"))
            .expect("create config directory");
        fs::write(
            &config_path,
            format!(
                r#"
[wsjtx_ingest]
# keep operator comment
enabled = true
future_key = "preserve-me"

[logbook]
file_path = "{}"
"#,
                log_file_path.replace('\\', "\\\\")
            ),
        )
        .expect("seed config");
        let setup_state = Arc::new(SetupState::load(config_path.clone()).expect("setup state"));
        let runtime_config = Arc::new(RuntimeConfigManager::new(BTreeMap::new()).expect("runtime"));
        let service = SetupControlSurface::new(setup_state.clone(), runtime_config.clone());
        let replacement_log = absolute_log_file_path(&config_path, "replacement.db");

        let _ = SetupService::save_setup(
            &service,
            Request::new(SaveSetupRequest {
                log_file_path: Some(replacement_log),
                station_profile: Some(StationProfile {
                    station_callsign: "k7rnd".to_string(),
                    ..StationProfile::default()
                }),
                ..Default::default()
            }),
        )
        .await
        .expect("save setup");

        let saved_toml = fs::read_to_string(&config_path).expect("read config");
        assert!(saved_toml.contains("# keep operator comment"));
        assert!(saved_toml.contains("future_key = \"preserve-me\""));

        drop(service);
        drop(runtime_config);
        drop(setup_state);

        let config_directory = config_path.parent().expect("config directory");
        let _ = fs::remove_dir_all(config_directory);
    }

    #[tokio::test]
    async fn save_setup_rejects_wsjtx_adif_tail_without_path() {
        let config_path = unique_config_path();
        let log_file_path = absolute_log_file_path(&config_path, "wsjtx-invalid.db");
        let setup_state = Arc::new(SetupState::load(config_path.clone()).expect("setup state"));
        let runtime_config = Arc::new(RuntimeConfigManager::new(BTreeMap::new()).expect("runtime"));
        let service = SetupControlSurface::new(setup_state.clone(), runtime_config.clone());

        let error = SetupService::save_setup(
            &service,
            Request::new(SaveSetupRequest {
                log_file_path: Some(log_file_path),
                station_profile: Some(StationProfile {
                    station_callsign: "k7rnd".to_string(),
                    ..StationProfile::default()
                }),
                wsjtx_ingest: Some(WsjtxIngestSettings {
                    enabled: true,
                    udp_enabled: Some(false),
                    adif_tail_enabled: true,
                    ..Default::default()
                }),
                ..Default::default()
            }),
        )
        .await
        .expect_err("ADIF tail without path should fail");

        assert!(
            error
                .message()
                .contains("WSJT-X ADIF tail path is required"),
            "unexpected error: {error}"
        );

        drop(service);
        drop(runtime_config);
        drop(setup_state);

        let config_directory = config_path.parent().expect("config directory");
        let _ = fs::remove_dir_all(config_directory);
    }

    #[tokio::test]
    async fn save_setup_preserves_rig_control_when_omitted_in_subsequent_save() {
        let config_path = unique_config_path();
        let log_file_path = absolute_log_file_path(&config_path, "preserve-rig-control.db");
        let setup_state = Arc::new(SetupState::load(config_path.clone()).expect("setup state"));
        let runtime_config = Arc::new(RuntimeConfigManager::new(BTreeMap::new()).expect("runtime"));
        let service = SetupControlSurface::new(setup_state.clone(), runtime_config.clone());

        SetupService::save_setup(
            &service,
            Request::new(SaveSetupRequest {
                log_file_path: Some(log_file_path.clone()),
                station_profile: Some(StationProfile {
                    station_callsign: "k7rnd".to_string(),
                    ..StationProfile::default()
                }),
                rig_control: Some(RigControlSettings {
                    enabled: Some(true),
                    host: Some("127.0.0.1".to_string()),
                    port: Some(4532),
                    ..Default::default()
                }),
                ..Default::default()
            }),
        )
        .await
        .expect("first save");

        let response = SetupService::save_setup(
            &service,
            Request::new(SaveSetupRequest {
                log_file_path: Some(log_file_path),
                station_profile: Some(StationProfile {
                    station_callsign: "k7rnd".to_string(),
                    ..StationProfile::default()
                }),
                ..Default::default()
            }),
        )
        .await
        .expect("second save")
        .into_inner();

        let rig_control = response
            .status
            .expect("status payload")
            .rig_control
            .expect("rig_control");
        assert_eq!(Some(true), rig_control.enabled);
        assert_eq!(Some("127.0.0.1"), rig_control.host.as_deref());
        assert_eq!(Some(4532), rig_control.port);

        drop(service);
        drop(runtime_config);
        drop(setup_state);

        let config_directory = config_path.parent().expect("config directory");
        let _ = fs::remove_dir_all(config_directory);
    }

    #[tokio::test]
    async fn save_setup_preserves_logbook_key_when_omitted_in_subsequent_save() {
        let config_path = unique_config_path();
        let log_file_path = absolute_log_file_path(&config_path, "preserve-key.db");
        let setup_state = Arc::new(SetupState::load(config_path.clone()).expect("setup state"));
        let runtime_config = Arc::new(RuntimeConfigManager::new(BTreeMap::new()).expect("runtime"));
        let service = SetupControlSurface::new(setup_state.clone(), runtime_config.clone());

        // First save: set the key
        SetupService::save_setup(
            &service,
            Request::new(SaveSetupRequest {
                log_file_path: Some(log_file_path.clone()),
                station_profile: Some(StationProfile {
                    station_callsign: "k7rnd".to_string(),
                    ..StationProfile::default()
                }),
                qrz_logbook_api_key: Some("original-key".to_string()),
                ..Default::default()
            }),
        )
        .await
        .expect("first save");

        // Second save: omit the key
        let response = SetupService::save_setup(
            &service,
            Request::new(SaveSetupRequest {
                log_file_path: Some(log_file_path),
                station_profile: Some(StationProfile {
                    station_callsign: "k7rnd".to_string(),
                    ..StationProfile::default()
                }),
                // qrz_logbook_api_key omitted
                ..Default::default()
            }),
        )
        .await
        .expect("second save")
        .into_inner();

        let status = response.status.expect("status payload");
        assert!(
            status.has_qrz_logbook_api_key,
            "logbook key should be preserved across saves when omitted"
        );

        drop(service);
        drop(runtime_config);
        drop(setup_state);

        let config_directory = config_path.parent().expect("config directory");
        let _ = fs::remove_dir_all(config_directory);
    }

    #[tokio::test]
    async fn save_setup_preserves_xml_password_when_omitted_in_subsequent_save() {
        let config_path = unique_config_path();
        let log_file_path = absolute_log_file_path(&config_path, "preserve-password.db");
        let setup_state = Arc::new(SetupState::load(config_path.clone()).expect("setup state"));
        let runtime_config = Arc::new(RuntimeConfigManager::new(BTreeMap::new()).expect("runtime"));
        let service = SetupControlSurface::new(setup_state.clone(), runtime_config.clone());

        SetupService::save_setup(
            &service,
            Request::new(SaveSetupRequest {
                log_file_path: Some(log_file_path.clone()),
                station_profile: Some(StationProfile {
                    station_callsign: "k7rnd".to_string(),
                    ..StationProfile::default()
                }),
                qrz_xml_username: Some("k7rnd".to_string()),
                qrz_xml_password: Some("original-secret".to_string()),
                ..Default::default()
            }),
        )
        .await
        .expect("first save");

        let response = SetupService::save_setup(
            &service,
            Request::new(SaveSetupRequest {
                log_file_path: Some(log_file_path),
                station_profile: Some(StationProfile {
                    station_callsign: "k7rnd".to_string(),
                    ..StationProfile::default()
                }),
                qrz_xml_username: Some("k7rnd".to_string()),
                ..Default::default()
            }),
        )
        .await
        .expect("second save")
        .into_inner();

        let status = response.status.expect("status payload");
        assert!(
            status.has_qrz_xml_password,
            "xml password should be preserved across saves when omitted"
        );

        let effective_values = runtime_config.effective_values().await;
        assert_eq!(
            Some("original-secret"),
            effective_values
                .get(qsoripper_core::lookup::QRZ_XML_PASSWORD_ENV_VAR)
                .map(String::as_str)
        );

        drop(service);
        drop(runtime_config);
        drop(setup_state);

        let config_directory = config_path.parent().expect("config directory");
        let _ = fs::remove_dir_all(config_directory);
    }

    #[test]
    fn persisted_sync_config_round_trips_through_proto() {
        let proto = SyncConfig {
            auto_sync_enabled: true,
            sync_interval_seconds: 120,
            conflict_policy: ConflictPolicy::FlagForReview as i32,
        };
        let persisted = super::PersistedSyncConfig::from_proto(&proto);
        assert!(persisted.auto_sync_enabled);
        assert_eq!(120, persisted.sync_interval_seconds);
        assert_eq!("flag_for_review", persisted.conflict_policy);

        let back = persisted.to_proto();
        assert!(back.auto_sync_enabled);
        assert_eq!(120, back.sync_interval_seconds);
        assert_eq!(ConflictPolicy::FlagForReview as i32, back.conflict_policy);
    }

    #[test]
    fn persisted_sync_config_defaults_interval_when_zero() {
        let proto = SyncConfig {
            auto_sync_enabled: false,
            sync_interval_seconds: 0,
            conflict_policy: ConflictPolicy::LastWriteWins as i32,
        };
        let persisted = super::PersistedSyncConfig::from_proto(&proto);
        assert_eq!(300, persisted.sync_interval_seconds);

        let back = persisted.to_proto();
        assert_eq!(300, back.sync_interval_seconds);
    }

    #[test]
    fn persisted_sync_config_treats_unspecified_policy_as_flag_for_review() {
        let proto = SyncConfig {
            conflict_policy: ConflictPolicy::Unspecified as i32,
            ..Default::default()
        };

        let persisted = super::PersistedSyncConfig::from_proto(&proto);
        assert_eq!("flag_for_review", persisted.conflict_policy);
    }

    #[test]
    fn persisted_rig_control_config_round_trips_through_proto() {
        let proto = RigControlSettings {
            enabled: Some(true),
            host: Some("127.0.0.1".to_string()),
            port: Some(4532),
            read_timeout_ms: Some(2000),
            stale_threshold_ms: Some(5000),
        };
        let persisted =
            super::PersistedRigControlConfig::from_proto(&proto).expect("rig control config");
        assert_eq!(Some(true), persisted.enabled);
        assert_eq!(Some("127.0.0.1"), persisted.host.as_deref());
        assert_eq!(Some(4532), persisted.port);
        assert_eq!(Some(2000), persisted.read_timeout_ms);
        assert_eq!(Some(5000), persisted.stale_threshold_ms);

        let back = persisted.to_proto().expect("rig control proto");
        assert_eq!(proto.enabled, back.enabled);
        assert_eq!(proto.host, back.host);
        assert_eq!(proto.port, back.port);
        assert_eq!(proto.read_timeout_ms, back.read_timeout_ms);
        assert_eq!(proto.stale_threshold_ms, back.stale_threshold_ms);
    }

    #[test]
    fn persisted_rig_control_config_rejects_invalid_port() {
        let error = super::PersistedRigControlConfig::from_proto(&RigControlSettings {
            port: Some(u32::from(u16::MAX) + 1),
            ..Default::default()
        })
        .expect_err("invalid port should be rejected");
        assert_eq!("Rig control port must be between 1 and 65535.", error);
    }

    #[test]
    fn password_round_trips_through_serialized_config() {
        let mut config = PersistedSetupConfig::default();
        config.qrz_xml.username = Some("K7RND".to_string());
        config.qrz_xml.password = Some("super_secret_password".to_string());

        let toml_output = toml::to_string_pretty(&config).expect("serialize config");

        assert!(
            toml_output.contains("K7RND"),
            "username should be present in TOML"
        );
        assert!(
            toml_output.contains("super_secret_password"),
            "password should be serialized so restarts preserve lookup auth"
        );
        assert!(
            toml_output.contains("password"),
            "password key should be present in serialized TOML"
        );
    }

    #[test]
    fn config_with_password_deserializes_and_reserializes_password() {
        let toml_input = r#"
[qrz_xml]
username = "K7RND"
password = "legacy_secret"
"#;
        let config: PersistedSetupConfig =
            toml::from_str(toml_input).expect("deserialize legacy config");
        assert_eq!(Some("K7RND"), config.qrz_xml.username.as_deref());
        assert_eq!(
            Some("legacy_secret"),
            config.qrz_xml.password.as_deref(),
            "legacy password should still be deserialized for runtime use"
        );

        let reserialized = toml::to_string_pretty(&config).expect("reserialize");
        assert!(
            reserialized.contains("legacy_secret"),
            "password should survive reserialization: {reserialized}"
        );
    }

    #[tokio::test]
    async fn save_setup_persists_password_to_disk() {
        let config_path = unique_config_path();
        let log_file_path = absolute_log_file_path(&config_path, "no_pw.db");
        let setup_state = Arc::new(SetupState::load(config_path.clone()).expect("setup state"));
        let runtime_config = Arc::new(RuntimeConfigManager::new(BTreeMap::new()).expect("runtime"));
        let service = SetupControlSurface::new(setup_state, runtime_config);

        SetupService::save_setup(
            &service,
            Request::new(SaveSetupRequest {
                storage_backend: StorageBackend::Unspecified as i32,
                sqlite_path: None,
                log_file_path: Some(log_file_path),
                station_profile: Some(StationProfile {
                    station_callsign: "k7rnd".to_string(),
                    ..StationProfile::default()
                }),
                qrz_xml_username: Some("k7rnd".to_string()),
                qrz_xml_password: Some("should_persist".to_string()),
                ..Default::default()
            }),
        )
        .await
        .expect("save setup");

        let saved = fs::read_to_string(&config_path).expect("read saved config");
        assert!(
            saved.contains("should_persist"),
            "password should be written to disk so restarts preserve lookup auth: {saved}"
        );
        assert!(
            saved.contains("K7RND") || saved.contains("k7rnd"),
            "username should be in saved config"
        );

        drop(service);
        let config_directory = config_path.parent().expect("config directory");
        fs::remove_dir_all(config_directory).expect("remove temp config directory");
    }

    #[test]
    fn write_persisted_config_preserves_unknown_tables() {
        // The unified config.toml is shared with the CAT hub daemon ([cat_hub]) and the
        // launcher ([launcher]); an engine setup save must not clobber those sections.
        let config_path = unique_config_path();
        let config_directory = config_path.parent().expect("config directory");
        fs::create_dir_all(config_directory).expect("create config directory");
        fs::write(
            &config_path,
            r#"[cat_hub.radio]
backend = "ts590"
port = "COM4"

[[cat_hub.face]]
name = "n1mm"
transport = "COM11"
dialect = "ts590"

[[cat_hub.hamlib_net]]
name = "engine"
bind = "127.0.0.1:4532"

[launcher]
engines = [1]
"#,
        )
        .expect("seed config");

        let config = PersistedSetupConfig::default();
        write_persisted_config(&config_path, &config, None, None).expect("write");

        let saved = fs::read_to_string(&config_path).expect("read");
        assert!(saved.contains("[cat_hub.radio]"), "cat_hub.radio: {saved}");
        assert!(saved.contains("port = \"COM4\""), "radio port: {saved}");
        assert!(saved.contains("[[cat_hub.face]]"), "cat_hub.face: {saved}");
        assert!(
            saved.contains("[[cat_hub.hamlib_net]]"),
            "cat_hub.hamlib_net: {saved}"
        );
        assert!(saved.contains("[launcher]"), "launcher: {saved}");

        // The engine can still load its own config from the merged document.
        load_persisted_config(&config_path)
            .expect("load")
            .expect("config present");

        fs::remove_dir_all(config_directory).expect("remove temp config directory");
    }

    #[test]
    fn write_persisted_config_removes_cleared_engine_tables() {
        // When the wizard clears an engine-owned table, the stale section must be removed,
        // while unknown sections such as [cat_hub] are preserved.
        let config_path = unique_config_path();
        let config_directory = config_path.parent().expect("config directory");
        fs::create_dir_all(config_directory).expect("create config directory");
        fs::write(
            &config_path,
            r#"[rig_control]
enabled = true
host = "127.0.0.1"
port = 4532

[cat_hub.radio]
backend = "ts590"
"#,
        )
        .expect("seed config");

        let config = PersistedSetupConfig::default();
        write_persisted_config(&config_path, &config, None, None).expect("write");

        let saved = fs::read_to_string(&config_path).expect("read");
        assert!(
            !saved.contains("[rig_control]"),
            "stale rig_control should be removed: {saved}"
        );
        assert!(
            saved.contains("[cat_hub.radio]"),
            "cat_hub must survive: {saved}"
        );

        fs::remove_dir_all(config_directory).expect("remove temp config directory");
    }

    // ── CAT hub setup tests ─────────────────────────────────────────────────

    fn valid_cat_hub_settings() -> CatHubSettings {
        CatHubSettings {
            radio: Some(CatHubRadioSettings {
                backend: Some("ts590".to_string()),
                model: None,
                transport: None,
                port: Some("COM3".to_string()),
                baud: Some(4800),
                host: None,
                tcp_port: Some(4532),
                certified: Some(false),
                reply_timeout_ms: Some(1000),
            }),
            poll: Some(CatHubPollSettings {
                baseline_ms: Some(250),
                heartbeat_ms: Some(3000),
            }),
            ptt: Some(CatHubPttSettings {
                max_tx_ms: Some(300_000),
            }),
            events: Some(CatHubEventSettings {
                native_push: Some(true),
            }),
            faces: vec![CatHubSerialFace {
                name: "n1mm".to_string(),
                transport: "COM11".to_string(),
                baud: 4800,
                dialect: "ts590".to_string(),
                perms: vec![
                    CatHubPermission::Read as i32,
                    CatHubPermission::Write as i32,
                ],
            }],
            hamlib_net: vec![CatHubHamlibNetEndpoint {
                name: "engine".to_string(),
                bind: "127.0.0.1:4532".to_string(),
                perms: vec![CatHubPermission::Read as i32],
            }],
            winkeyer: Some(CatHubWinkeyerSettings {
                port: "COM3-WK".to_string(),
                baud: Some(1200),
                max_tx_ms: Some(30_000),
                api_bind: Some("127.0.0.1:50071".to_string()),
            }),
            winkeyer_faces: vec![CatHubWinkeyerFace {
                name: "n1mm-cw".to_string(),
                transport: "COM41".to_string(),
                baud: Some(1200),
                primary: Some(true),
                perms: vec![
                    WinkeyerFacePermission::Status as i32,
                    WinkeyerFacePermission::Send as i32,
                    WinkeyerFacePermission::Control as i32,
                ],
            }],
        }
    }

    #[test]
    fn cat_hub_from_proto_round_trips_through_to_proto() {
        let settings = valid_cat_hub_settings();
        let persisted = PersistedCatHubConfig::from_proto(&settings).expect("valid settings");
        let projected = persisted.to_proto().expect("non-empty");

        let radio = projected.radio.expect("radio");
        assert_eq!(radio.backend.as_deref(), Some("ts590"));
        assert_eq!(radio.port.as_deref(), Some("COM3"));
        assert_eq!(radio.tcp_port, Some(4532));
        assert_eq!(projected.faces.len(), 1);
        assert_eq!(projected.faces[0].name, "n1mm");
        assert_eq!(projected.faces[0].dialect, "ts590");
        assert_eq!(
            projected.faces[0].perms,
            vec![
                CatHubPermission::Read as i32,
                CatHubPermission::Write as i32
            ]
        );
        assert_eq!(projected.hamlib_net.len(), 1);
        assert_eq!(projected.hamlib_net[0].bind, "127.0.0.1:4532");
        let winkeyer = projected.winkeyer.expect("winkeyer");
        assert_eq!(winkeyer.port, "COM3-WK");
        assert_eq!(winkeyer.api_bind.as_deref(), Some("127.0.0.1:50071"));
        assert_eq!(projected.winkeyer_faces.len(), 1);
        assert!(projected.winkeyer_faces[0].primary.unwrap_or_default());
    }

    #[test]
    fn cat_hub_from_proto_requires_radio() {
        let mut settings = valid_cat_hub_settings();
        settings.radio = None;
        let error = PersistedCatHubConfig::from_proto(&settings).expect_err("radio required");
        assert!(error.contains("radio settings are required"), "{error}");
    }

    #[test]
    fn cat_hub_from_proto_requires_at_least_one_endpoint() {
        let mut settings = valid_cat_hub_settings();
        settings.faces.clear();
        settings.hamlib_net.clear();
        let error = PersistedCatHubConfig::from_proto(&settings).expect_err("endpoint required");
        assert!(error.contains("at least one serial face"), "{error}");
    }

    #[test]
    fn cat_hub_from_proto_rejects_unknown_backend() {
        let mut settings = valid_cat_hub_settings();
        settings.radio.as_mut().expect("radio").backend = Some("icom".to_string());
        let error = PersistedCatHubConfig::from_proto(&settings).expect_err("bad backend");
        assert!(error.contains("backend 'icom'"), "{error}");
    }

    #[test]
    fn cat_hub_from_proto_rejects_face_reusing_radio_port() {
        let mut settings = valid_cat_hub_settings();
        settings.faces[0].transport = "COM3".to_string();
        let error = PersistedCatHubConfig::from_proto(&settings).expect_err("port reuse");
        assert!(error.contains("cannot reuse the radio port"), "{error}");
    }

    #[test]
    fn cat_hub_from_proto_rejects_duplicate_endpoint_names() {
        let mut settings = valid_cat_hub_settings();
        settings.hamlib_net[0].name = "n1mm".to_string();
        let error = PersistedCatHubConfig::from_proto(&settings).expect_err("duplicate name");
        assert!(error.contains("names must be unique"), "{error}");
    }

    #[test]
    fn cat_hub_from_proto_rejects_unknown_dialect() {
        let mut settings = valid_cat_hub_settings();
        settings.faces[0].dialect = "icom".to_string();
        let error = PersistedCatHubConfig::from_proto(&settings).expect_err("bad dialect");
        assert!(error.contains("dialect 'icom'"), "{error}");
    }

    #[test]
    fn cat_hub_from_proto_rejects_bad_bind() {
        let mut settings = valid_cat_hub_settings();
        settings.hamlib_net[0].bind = "127.0.0.1".to_string();
        let error = PersistedCatHubConfig::from_proto(&settings).expect_err("bad bind");
        assert!(error.contains("host:port"), "{error}");
    }

    #[test]
    fn cat_hub_from_proto_serial_backend_requires_port() {
        let mut settings = valid_cat_hub_settings();
        settings.radio.as_mut().expect("radio").port = None;
        let error = PersistedCatHubConfig::from_proto(&settings).expect_err("port required");
        assert!(error.contains("requires a serial port"), "{error}");
    }

    #[test]
    fn build_cat_hub_step_is_always_complete() {
        let step = build_cat_hub_step(None);
        assert!(step.complete);
        assert!(step.issues.is_empty());
        assert_eq!(i32::from(SetupWizardStep::CatHub), step.step);
    }

    #[test]
    fn load_cat_hub_config_reads_unified_section() {
        let config_path = unique_config_path();
        let config_directory = config_path.parent().expect("config directory");
        fs::create_dir_all(config_directory).expect("create config directory");
        fs::write(
            &config_path,
            r#"[cat_hub.radio]
backend = "ts590"
port = "COM3"

[[cat_hub.face]]
name = "n1mm"
transport = "COM11"
dialect = "ts590"
perms = ["read", "write"]

[[cat_hub.hamlib_net]]
name = "engine"
bind = "127.0.0.1:4532"
"#,
        )
        .expect("seed config");

        let loaded = load_cat_hub_config(&config_path).expect("cat_hub present");
        let projected = loaded.to_proto().expect("non-empty");
        assert_eq!(
            projected.radio.expect("radio").port.as_deref(),
            Some("COM3")
        );
        assert_eq!(projected.faces.len(), 1);
        assert_eq!(projected.hamlib_net.len(), 1);

        fs::remove_dir_all(config_directory).expect("remove temp config directory");
    }

    #[tokio::test]
    async fn cw_keying_toml_loads_and_survives_setup_write_verbatim() {
        let config_path = unique_config_path();
        fs::create_dir_all(config_path.parent().expect("config directory"))
            .expect("create config directory");
        fs::write(
            &config_path,
            r#"
[cw_keying]
backend = "winkeyer"
winkeyer_port = "COM3"
winkeyer_baud = 1200
cathub_endpoint = "http://127.0.0.1:50071"
cathub_client_name = "rust-engine"
speed_wpm = 20
transmit_enabled = true
max_tx_ms = 30000
future_key = "preserve-me"
"#,
        )
        .expect("write config");

        let config = load_persisted_config(&config_path)
            .expect("load config")
            .expect("config present");
        let values = config.to_runtime_values();
        write_persisted_config(&config_path, &config, None, None).expect("rewrite config");
        let saved = fs::read_to_string(&config_path).expect("read config");

        assert_eq!(
            Some("winkeyer"),
            values.get(CW_KEYER_BACKEND_ENV_VAR).map(String::as_str)
        );
        assert_eq!(
            Some("COM3"),
            values.get(CW_WINKEYER_PORT_ENV_VAR).map(String::as_str)
        );
        assert_eq!(
            Some("1200"),
            values.get(CW_WINKEYER_BAUD_ENV_VAR).map(String::as_str)
        );
        assert_eq!(
            Some("http://127.0.0.1:50071"),
            values.get(CW_CATHUB_ENDPOINT_ENV_VAR).map(String::as_str)
        );
        assert_eq!(
            Some("rust-engine"),
            values
                .get(CW_CATHUB_CLIENT_NAME_ENV_VAR)
                .map(String::as_str)
        );
        assert_eq!(
            Some("20"),
            values.get(CW_SPEED_WPM_ENV_VAR).map(String::as_str)
        );
        assert_eq!(
            Some("true"),
            values.get(CW_TRANSMIT_ENABLED_ENV_VAR).map(String::as_str)
        );
        assert_eq!(
            Some("30000"),
            values.get(CW_MAX_TX_MS_ENV_VAR).map(String::as_str)
        );
        assert!(saved.contains("future_key = \"preserve-me\""));

        let _ = fs::remove_dir_all(config_path.parent().expect("config directory"));
    }

    #[test]
    fn load_cat_hub_config_ignores_malformed_section() {
        let config_path = unique_config_path();
        let config_directory = config_path.parent().expect("config directory");
        fs::create_dir_all(config_directory).expect("create config directory");
        // tcp_port out of range for u16 makes the [cat_hub] section undeserializable.
        fs::write(
            &config_path,
            r#"[cat_hub.radio]
backend = "ts590"
tcp_port = 99999
"#,
        )
        .expect("seed config");

        assert!(load_cat_hub_config(&config_path).is_none());

        fs::remove_dir_all(config_directory).expect("remove temp config directory");
    }

    #[test]
    fn write_persisted_config_writes_and_preserves_cat_hub() {
        let config_path = unique_config_path();
        let config_directory = config_path.parent().expect("config directory");
        fs::create_dir_all(config_directory).expect("create config directory");

        // First write: supply a complete CAT hub section.
        let cat_hub =
            PersistedCatHubConfig::from_proto(&valid_cat_hub_settings()).expect("valid settings");
        let engine_config = PersistedSetupConfig::default();
        write_persisted_config(&config_path, &engine_config, Some(&cat_hub), None).expect("write");

        let saved = fs::read_to_string(&config_path).expect("read");
        assert!(saved.contains("[cat_hub.radio]"), "radio: {saved}");
        assert!(saved.contains("[[cat_hub.face]]"), "face: {saved}");
        // The written section must satisfy the real cathub daemon validator.
        qsoripper_cathub::validate_cat_hub_toml(&saved).expect("daemon-valid cat_hub");

        // Second write WITHOUT a CAT hub update must leave the section untouched.
        write_persisted_config(&config_path, &engine_config, None, None).expect("second write");
        let preserved = fs::read_to_string(&config_path).expect("read again");
        assert!(
            preserved.contains("[cat_hub.radio]"),
            "preserved: {preserved}"
        );
        assert!(
            preserved.contains("name = \"n1mm\""),
            "face preserved: {preserved}"
        );
        let reloaded = load_cat_hub_config(&config_path).expect("cat_hub still present");
        assert!(reloaded.to_proto().is_some());

        fs::remove_dir_all(config_directory).expect("remove temp config directory");
    }
}

//! Map a [`Selection`] onto concrete spawn arguments + env for each component.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::catalog::{
    engine_endpoint, find, ComponentSpec, DAEMON_CATHUB, ENGINE_DOTNET, ENGINE_RUST, UI_DEBUGHOST,
};
use crate::model::Selection;

/// Per-process spawn plan: the resolved component spec, command-line args,
/// and environment overrides to apply on top of the parent environment.
pub(crate) struct LaunchPlan {
    pub spec: ComponentSpec,
    pub args: Vec<OsString>,
    pub env: Vec<(String, String)>,
    pub readiness_port: Option<u16>,
}

enum CatHubReadiness {
    ConfigUnreadable,
    Configured(Option<u16>),
}

const QSORIPPER_ENGINE_ENV: &str = "QSORIPPER_ENGINE";
const QSORIPPER_ENDPOINT_ENV: &str = "QSORIPPER_ENDPOINT";

fn engine_profile_name(engine_id: &str) -> Option<&'static str> {
    match engine_id {
        id if id == ENGINE_RUST => Some("local-rust"),
        id if id == ENGINE_DOTNET => Some("local-dotnet"),
        _ => None,
    }
}

/// Build the launch plan for a UI component given the current selection.
pub(crate) fn ui_plan(ui_id: &str, selection: &Selection) -> Option<LaunchPlan> {
    let spec = find(ui_id)?;
    let mut env = Vec::new();
    let mut args: Vec<OsString> = Vec::new();

    if spec.engine_bindable {
        if let Some(engine_id) = selection.bindings.get(&spec.id).copied() {
            if let Some(profile) = engine_profile_name(engine_id) {
                env.push((QSORIPPER_ENGINE_ENV.to_owned(), profile.to_owned()));
            }
            if let Some(ep) = engine_endpoint(engine_id) {
                env.push((QSORIPPER_ENDPOINT_ENV.to_owned(), ep));
            }
        }
    }

    if spec.id == UI_DEBUGHOST {
        // Match the URL runall.ps1 forces for the DebugHost so the launcher
        // and runall hand off the same endpoint to the browser.
        args.push("--urls".into());
        args.push("http://localhost:5082".into());
    }

    Some(LaunchPlan {
        spec,
        args,
        env,
        readiness_port: None,
    })
}

/// Build the launch plan for an engine component. Engines take no per-process
/// configuration from the launcher in the first pass.
pub(crate) fn engine_plan(engine_id: &str) -> Option<LaunchPlan> {
    let spec = find(engine_id)?;
    let readiness_port = spec.engine_port;
    Some(LaunchPlan {
        spec,
        args: Vec::new(),
        env: Vec::new(),
        readiness_port,
    })
}

/// Build the launch plan for a background daemon. The CAT hub is passed an
/// explicit `--config` so it reads the same file `Start-CatHub.ps1` would: the
/// unified `config.toml` when it carries a top-level `[cat_hub]` table,
/// otherwise `CatHub`'s standalone configuration path.
pub(crate) fn daemon_plan(
    daemon_id: &str,
    unified_config_path: &Path,
    standalone_config_override: Option<&Path>,
) -> Option<LaunchPlan> {
    let spec = find(daemon_id)?;
    let mut args: Vec<OsString> = Vec::new();
    let mut readiness_port = spec.engine_port;
    if daemon_id == DAEMON_CATHUB {
        let (config, managed) =
            resolve_cathub_config(unified_config_path, standalone_config_override);
        if let CatHubReadiness::Configured(configured_port) = cathub_readiness_port(&config) {
            readiness_port = configured_port;
        }
        if managed {
            args.push("--section".into());
            args.push("cat_hub".into());
        }
        args.push("--config".into());
        args.push(config.into_os_string());
    }
    Some(LaunchPlan {
        spec,
        args,
        env: Vec::new(),
        readiness_port,
    })
}

/// Choose the `CatHub` config path. Uses the unified config only when it parses
/// and exposes a top-level `cat_hub` table; any read/parse failure or a missing
/// table falls back to `CatHub`'s standalone configuration path.
fn resolve_cathub_config(
    unified_config_path: &Path,
    standalone_config_override: Option<&Path>,
) -> (PathBuf, bool) {
    if unified_has_cat_hub(unified_config_path) {
        return (unified_config_path.to_path_buf(), true);
    }
    if let Some(path) = standalone_config_override {
        return (path.to_path_buf(), false);
    }
    (default_cathub_config_path(), false)
}

fn default_cathub_config_path() -> PathBuf {
    if let Some(path) = std::env::var_os("CATHUB_CONFIG_PATH") {
        return PathBuf::from(path);
    }
    #[cfg(windows)]
    if let Some(app_data) = std::env::var_os("APPDATA") {
        return PathBuf::from(app_data).join("cathub").join("cathub.toml");
    }
    #[cfg(not(windows))]
    {
        if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
            return PathBuf::from(xdg).join("cathub").join("cathub.toml");
        }
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home)
                .join(".config")
                .join("cathub")
                .join("cathub.toml");
        }
    }
    PathBuf::from("cathub.toml")
}

fn cathub_readiness_port(config_path: &Path) -> CatHubReadiness {
    let Ok(text) = std::fs::read_to_string(config_path) else {
        return CatHubReadiness::ConfigUnreadable;
    };
    let Ok(document) = text.parse::<toml_edit::DocumentMut>() else {
        return CatHubReadiness::ConfigUnreadable;
    };
    let root = document.get("cat_hub").unwrap_or(document.as_item());
    let hamlib_port = root
        .get("hamlib_net")
        .and_then(toml_edit::Item::as_array_of_tables)
        .and_then(|endpoints| {
            endpoints.iter().find_map(|endpoint| {
                endpoint
                    .get("bind")
                    .and_then(toml_edit::Item::as_str)
                    .and_then(socket_port)
            })
        });
    let winkeyer_port = root
        .get("winkeyer")
        .and_then(|winkeyer| winkeyer.get("api_bind"))
        .and_then(toml_edit::Item::as_str)
        .and_then(socket_port);
    CatHubReadiness::Configured(hamlib_port.or(winkeyer_port))
}

fn socket_port(bind: &str) -> Option<u16> {
    bind.rsplit_once(':')
        .and_then(|(_, port)| port.parse::<u16>().ok())
}

fn unified_has_cat_hub(unified_config_path: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(unified_config_path) else {
        return false;
    };
    let Ok(doc) = text.parse::<toml_edit::DocumentMut>() else {
        return false;
    };
    doc.get("cat_hub").is_some()
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
    use crate::catalog::{UI_CWSCOPE, UI_GUI};

    #[test]
    fn gui_plan_injects_engine_env_for_dotnet_binding() {
        let mut sel = Selection::default_preset();
        sel.engines = vec![ENGINE_DOTNET];
        sel.bindings.clear();
        sel.bindings.insert(UI_GUI, ENGINE_DOTNET);
        let plan = ui_plan(UI_GUI, &sel).expect("plan");
        let map: std::collections::HashMap<_, _> = plan.env.into_iter().collect();
        assert_eq!(
            map.get(QSORIPPER_ENGINE_ENV).map(String::as_str),
            Some("local-dotnet")
        );
        assert_eq!(
            map.get(QSORIPPER_ENDPOINT_ENV).map(String::as_str),
            Some("http://127.0.0.1:50052"),
        );
    }

    #[test]
    fn cwscope_plan_has_no_engine_env() {
        let sel = Selection::default_preset();
        let plan = ui_plan(UI_CWSCOPE, &sel).expect("plan");
        assert!(plan.env.is_empty());
    }

    #[test]
    fn win32_plan_has_no_engine_env() {
        use crate::catalog::UI_WIN32;
        let sel = Selection::default_preset();
        let plan = ui_plan(UI_WIN32, &sel).expect("plan");
        assert!(
            plan.env.is_empty(),
            "win32 client does not yet honor QSORIPPER_ENGINE/QSORIPPER_ENDPOINT",
        );
        assert!(plan.args.is_empty());
    }

    #[test]
    fn debughost_plan_forces_the_runall_url() {
        let sel = Selection::default_preset();
        let plan = ui_plan(UI_DEBUGHOST, &sel).expect("plan");
        let strs: Vec<String> = plan
            .args
            .into_iter()
            .map(|a| a.into_string().unwrap())
            .collect();
        assert_eq!(
            strs,
            vec!["--urls".to_string(), "http://localhost:5082".to_string()]
        );
    }

    #[test]
    fn daemon_plan_uses_external_config_without_managed_cat_hub() {
        let dir = tempfile::tempdir().unwrap();
        let unified = dir.path().join("config.toml");
        std::fs::write(&unified, "[launcher]\nengines = [\"rust-engine\"]\n").unwrap();
        let external = dir.path().join("cathub.toml");
        let plan = daemon_plan(DAEMON_CATHUB, &unified, Some(&external)).expect("plan");
        let strs: Vec<String> = plan
            .args
            .into_iter()
            .map(|a| a.into_string().unwrap())
            .collect();
        let expected = external.to_string_lossy().into_owned();
        assert_eq!(strs, vec!["--config".to_string(), expected]);
    }

    #[test]
    fn daemon_plan_uses_unified_config_when_it_has_cat_hub() {
        let dir = tempfile::tempdir().unwrap();
        let unified = dir.path().join("config.toml");
        std::fs::write(&unified, "[cat_hub]\n[radio]\nport = \"COM3\"\n").unwrap();
        let plan = daemon_plan(DAEMON_CATHUB, &unified, None).expect("plan");
        let strs: Vec<String> = plan
            .args
            .into_iter()
            .map(|a| a.into_string().unwrap())
            .collect();
        let expected = unified.to_string_lossy().into_owned();
        assert_eq!(
            strs,
            vec![
                "--section".to_string(),
                "cat_hub".to_string(),
                "--config".to_string(),
                expected
            ]
        );
    }

    #[test]
    fn daemon_plan_falls_back_when_unified_config_is_invalid_toml() {
        let dir = tempfile::tempdir().unwrap();
        let unified = dir.path().join("config.toml");
        std::fs::write(&unified, "this is = not valid = toml [[[").unwrap();
        let external = dir.path().join("cathub.toml");
        let plan = daemon_plan(DAEMON_CATHUB, &unified, Some(&external)).expect("plan");
        let strs: Vec<String> = plan
            .args
            .into_iter()
            .map(|a| a.into_string().unwrap())
            .collect();
        let expected = external.to_string_lossy().into_owned();
        assert_eq!(strs, vec!["--config".to_string(), expected]);
    }

    #[test]
    fn daemon_plan_reads_configured_hamlib_readiness_port() {
        let dir = tempfile::tempdir().unwrap();
        let unified = dir.path().join("config.toml");
        std::fs::write(
            &unified,
            "[cat_hub.radio]\nbackend = \"loopback\"\n[[cat_hub.hamlib_net]]\nname = \"engine\"\nbind = \"127.0.0.1:4632\"\n",
        )
        .unwrap();
        let plan = daemon_plan(DAEMON_CATHUB, &unified, None).expect("plan");
        assert_eq!(plan.readiness_port, Some(4632));
    }

    #[test]
    fn daemon_plan_uses_winkeyer_readiness_without_hamlib() {
        let dir = tempfile::tempdir().unwrap();
        let unified = dir.path().join("config.toml");
        std::fs::write(
            &unified,
            "[cat_hub.radio]\nbackend = \"loopback\"\n[cat_hub.winkeyer]\napi_bind = \"127.0.0.1:51071\"\n",
        )
        .unwrap();
        let plan = daemon_plan(DAEMON_CATHUB, &unified, None).expect("plan");
        assert_eq!(plan.readiness_port, Some(51071));
    }

    #[test]
    fn daemon_plan_has_no_readiness_port_for_serial_only_config() {
        let dir = tempfile::tempdir().unwrap();
        let unified = dir.path().join("config.toml");
        std::fs::write(
            &unified,
            "[cat_hub.radio]\nbackend = \"loopback\"\n[[cat_hub.serial_endpoint]]\nname = \"logger\"\ntransport = \"COM10\"\n",
        )
        .unwrap();
        let plan = daemon_plan(DAEMON_CATHUB, &unified, None).expect("plan");
        assert_eq!(plan.readiness_port, None);
    }
}

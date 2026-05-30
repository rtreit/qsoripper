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

    Some(LaunchPlan { spec, args, env })
}

/// Build the launch plan for an engine component. Engines take no per-process
/// configuration from the launcher in the first pass.
pub(crate) fn engine_plan(engine_id: &str) -> Option<LaunchPlan> {
    let spec = find(engine_id)?;
    Some(LaunchPlan {
        spec,
        args: Vec::new(),
        env: Vec::new(),
    })
}

/// Build the launch plan for a background daemon. The CAT hub is passed an
/// explicit `--config` so it reads the same file `Start-CatHub.ps1` would: the
/// unified `config.toml` when it carries a top-level `[cat_hub]` table,
/// otherwise the repo sample at `<repo_root>/config/cathub.toml`.
pub(crate) fn daemon_plan(
    daemon_id: &str,
    unified_config_path: &Path,
    repo_root: &Path,
) -> Option<LaunchPlan> {
    let spec = find(daemon_id)?;
    let mut args: Vec<OsString> = Vec::new();
    if daemon_id == DAEMON_CATHUB {
        let config = resolve_cathub_config(unified_config_path, repo_root);
        args.push("--config".into());
        args.push(config.into_os_string());
    }
    Some(LaunchPlan {
        spec,
        args,
        env: Vec::new(),
    })
}

/// Choose the cathub config path. Uses the unified config only when it parses
/// and exposes a top-level `cat_hub` table; any read/parse failure or a missing
/// table falls back to the repo sample so a malformed unified file never makes
/// the hub silently mis-parse it as a standalone cathub config.
fn resolve_cathub_config(unified_config_path: &Path, repo_root: &Path) -> PathBuf {
    if unified_has_cat_hub(unified_config_path) {
        return unified_config_path.to_path_buf();
    }
    repo_root.join("config").join("cathub.toml")
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
    fn daemon_plan_falls_back_to_repo_sample_without_cat_hub() {
        let dir = tempfile::tempdir().unwrap();
        let unified = dir.path().join("config.toml");
        std::fs::write(&unified, "[launcher]\nengines = [\"rust-engine\"]\n").unwrap();
        let repo_root = dir.path().join("repo");
        let plan = daemon_plan(DAEMON_CATHUB, &unified, &repo_root).expect("plan");
        let strs: Vec<String> = plan
            .args
            .into_iter()
            .map(|a| a.into_string().unwrap())
            .collect();
        let expected = repo_root
            .join("config")
            .join("cathub.toml")
            .to_string_lossy()
            .into_owned();
        assert_eq!(strs, vec!["--config".to_string(), expected]);
    }

    #[test]
    fn daemon_plan_uses_unified_config_when_it_has_cat_hub() {
        let dir = tempfile::tempdir().unwrap();
        let unified = dir.path().join("config.toml");
        std::fs::write(&unified, "[cat_hub]\n[radio]\nport = \"COM3\"\n").unwrap();
        let repo_root = dir.path().join("repo");
        let plan = daemon_plan(DAEMON_CATHUB, &unified, &repo_root).expect("plan");
        let strs: Vec<String> = plan
            .args
            .into_iter()
            .map(|a| a.into_string().unwrap())
            .collect();
        let expected = unified.to_string_lossy().into_owned();
        assert_eq!(strs, vec!["--config".to_string(), expected]);
    }

    #[test]
    fn daemon_plan_falls_back_when_unified_config_is_invalid_toml() {
        let dir = tempfile::tempdir().unwrap();
        let unified = dir.path().join("config.toml");
        std::fs::write(&unified, "this is = not valid = toml [[[").unwrap();
        let repo_root = dir.path().join("repo");
        let plan = daemon_plan(DAEMON_CATHUB, &unified, &repo_root).expect("plan");
        let strs: Vec<String> = plan
            .args
            .into_iter()
            .map(|a| a.into_string().unwrap())
            .collect();
        let expected = repo_root
            .join("config")
            .join("cathub.toml")
            .to_string_lossy()
            .into_owned();
        assert_eq!(strs, vec!["--config".to_string(), expected]);
    }
}

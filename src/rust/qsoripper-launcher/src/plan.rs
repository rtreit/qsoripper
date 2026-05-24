//! Map a [`Selection`] onto concrete spawn arguments + env for each component.

use std::ffi::OsString;

use crate::catalog::{
    engine_endpoint, find, ComponentSpec, ENGINE_DOTNET, ENGINE_RUST, UI_DEBUGHOST,
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
}

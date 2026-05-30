//! Selection state: which engines/UIs are checked and the per-UI bindings.

use std::collections::BTreeMap;

#[cfg(test)]
use crate::catalog::DAEMON_CATHUB;
#[cfg(test)]
use crate::catalog::ENGINE_DOTNET;
use crate::catalog::{catalog, ComponentId, ComponentKind, ENGINE_RUST, UI_DEBUGHOST, UI_GUI};

/// User-editable launcher selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Selection {
    /// Background daemons (e.g. the CAT hub) the user wants started first.
    pub daemons: Vec<ComponentId>,
    /// Engines the user wants started.
    pub engines: Vec<ComponentId>,
    /// UIs the user wants started.
    pub uis: Vec<ComponentId>,
    /// For each bindable UI, the engine component id it should target.
    pub bindings: BTreeMap<ComponentId, ComponentId>,
}

impl Selection {
    /// Default selection: Rust engine + Avalonia GUI + `DebugHost`, both bound to Rust.
    /// Daemons stay off by default so the launcher never grabs the radio serial
    /// port on machines without one attached.
    pub(crate) fn default_preset() -> Self {
        let mut bindings = BTreeMap::new();
        bindings.insert(UI_GUI, ENGINE_RUST);
        bindings.insert(UI_DEBUGHOST, ENGINE_RUST);
        Self {
            daemons: Vec::new(),
            engines: vec![ENGINE_RUST],
            uis: vec![UI_GUI, UI_DEBUGHOST],
            bindings,
        }
    }

    pub(crate) fn daemon_selected(&self, id: ComponentId) -> bool {
        self.daemons.contains(&id)
    }

    pub(crate) fn engine_selected(&self, id: ComponentId) -> bool {
        self.engines.contains(&id)
    }

    pub(crate) fn ui_selected(&self, id: ComponentId) -> bool {
        self.uis.contains(&id)
    }

    /// Toggle membership of `id` in the daemons, engines, or UIs list,
    /// depending on the component kind. UIs that get unchecked keep their
    /// binding so it is restored if the user toggles them back on.
    pub(crate) fn toggle(&mut self, id: ComponentId) {
        let Some(spec) = catalog().into_iter().find(|c| c.id == id) else {
            return;
        };
        let list = match spec.kind {
            ComponentKind::Daemon => &mut self.daemons,
            ComponentKind::Engine => &mut self.engines,
            ComponentKind::Ui => &mut self.uis,
        };
        if let Some(pos) = list.iter().position(|e| *e == id) {
            list.remove(pos);
        } else {
            list.push(id);
        }
    }

    /// Set the engine a bindable UI targets. No-op if the UI is not bindable
    /// or the engine is unknown.
    pub(crate) fn set_binding(&mut self, ui_id: ComponentId, engine_id: ComponentId) {
        let ui_is_bindable = catalog()
            .into_iter()
            .any(|c| c.id == ui_id && c.kind == ComponentKind::Ui && c.engine_bindable);
        let engine_known = catalog()
            .into_iter()
            .any(|c| c.id == engine_id && c.kind == ComponentKind::Engine);
        if ui_is_bindable && engine_known {
            self.bindings.insert(ui_id, engine_id);
        }
    }

    /// Repair bindings so every bindable selected UI targets one of the
    /// selected engines (or [`ENGINE_RUST`] if no engines are selected).
    pub(crate) fn repair_bindings(&mut self) {
        let fallback = self.engines.first().copied().unwrap_or(ENGINE_RUST);
        let bindable_uis: Vec<ComponentId> = catalog()
            .into_iter()
            .filter(|c| c.kind == ComponentKind::Ui && c.engine_bindable)
            .map(|c| c.id)
            .collect();
        for ui in bindable_uis {
            if !self.ui_selected(ui) {
                continue;
            }
            let current = self.bindings.get(&ui).copied();
            let needs_fix = match current {
                None => true,
                Some(engine) => !self.engine_selected(engine),
            };
            if needs_fix {
                self.bindings.insert(ui, fallback);
            }
        }
    }
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

    #[test]
    fn default_preset_is_rust_plus_gui_and_debughost() {
        let sel = Selection::default_preset();
        assert!(sel.engine_selected(ENGINE_RUST));
        assert!(sel.ui_selected(UI_GUI));
        assert!(sel.ui_selected(UI_DEBUGHOST));
        assert_eq!(sel.bindings.get(&UI_GUI), Some(&ENGINE_RUST));
        assert_eq!(sel.bindings.get(&UI_DEBUGHOST), Some(&ENGINE_RUST));
    }

    #[test]
    fn toggle_adds_then_removes_engine() {
        let mut sel = Selection::default_preset();
        assert!(!sel.engine_selected(ENGINE_DOTNET));
        sel.toggle(ENGINE_DOTNET);
        assert!(sel.engine_selected(ENGINE_DOTNET));
        sel.toggle(ENGINE_DOTNET);
        assert!(!sel.engine_selected(ENGINE_DOTNET));
    }

    #[test]
    fn toggle_routes_daemon_into_daemons_list() {
        let mut sel = Selection::default_preset();
        assert!(sel.daemons.is_empty());
        sel.toggle(DAEMON_CATHUB);
        assert!(sel.daemon_selected(DAEMON_CATHUB));
        assert!(!sel.engine_selected(DAEMON_CATHUB));
        sel.toggle(DAEMON_CATHUB);
        assert!(!sel.daemon_selected(DAEMON_CATHUB));
    }

    #[test]
    fn set_binding_rejects_daemon_as_engine_target() {
        let mut sel = Selection::default_preset();
        // The CAT hub daemon must never be selectable as a UI's engine.
        sel.set_binding(UI_GUI, DAEMON_CATHUB);
        assert_eq!(sel.bindings.get(&UI_GUI), Some(&ENGINE_RUST));
    }

    #[test]
    fn repair_bindings_falls_back_to_first_selected_engine() {
        let mut sel = Selection::default_preset();
        // Remove rust engine, add dotnet; the GUI binding should swing over.
        sel.toggle(ENGINE_RUST);
        sel.toggle(ENGINE_DOTNET);
        sel.repair_bindings();
        assert_eq!(sel.bindings.get(&UI_GUI), Some(&ENGINE_DOTNET));
    }

    #[test]
    fn set_binding_ignores_non_bindable_ui() {
        let mut sel = Selection::default_preset();
        sel.set_binding(crate::catalog::UI_CWSCOPE, ENGINE_DOTNET);
        assert!(!sel.bindings.contains_key(&crate::catalog::UI_CWSCOPE));
    }
}

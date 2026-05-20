//! Read/write the `[launcher]` table inside the shared `config.toml`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use toml_edit::{value, Array, DocumentMut, Item, Table};

use crate::catalog::{catalog, ComponentId, ComponentKind};
use crate::model::Selection;

/// Resolve the shared config path the same way `start-qsoripper.ps1` does.
///
/// * Windows: `%APPDATA%/qsoripper/config.toml`
/// * Linux/macOS: `$XDG_CONFIG_HOME/qsoripper/config.toml`, falling back to
///   `$HOME/.config/qsoripper/config.toml`.
pub(crate) fn default_shared_config_path() -> Result<PathBuf> {
    if cfg!(windows) {
        let appdata = std::env::var_os("APPDATA")
            .context("APPDATA is not set; cannot resolve the default shared config path")?;
        return Ok(PathBuf::from(appdata).join("qsoripper").join("config.toml"));
    }
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        return Ok(PathBuf::from(xdg).join("qsoripper").join("config.toml"));
    }
    let home = std::env::var_os("HOME")
        .context("HOME is not set; cannot resolve the default shared config path")?;
    Ok(PathBuf::from(home)
        .join(".config")
        .join("qsoripper")
        .join("config.toml"))
}

/// Load the `[launcher]` table from `path` (if it exists) into a [`Selection`].
/// Missing entries fall back to [`Selection::default_preset`].
pub(crate) fn load(path: &Path) -> Result<Selection> {
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Selection::default_preset())
        }
        Err(e) => return Err(anyhow::Error::from(e).context(format!("reading {}", path.display()))),
    };
    let doc: DocumentMut = text
        .parse()
        .with_context(|| format!("parsing TOML at {}", path.display()))?;
    Ok(extract_selection(&doc))
}

fn extract_selection(doc: &DocumentMut) -> Selection {
    let mut sel = Selection::default_preset();
    let Some(launcher) = doc.get("launcher").and_then(Item::as_table) else {
        return sel;
    };

    if let Some(engines) = launcher.get("engines").and_then(Item::as_array) {
        let mut out = Vec::new();
        for v in engines {
            if let Some(s) = v.as_str() {
                if let Some(id) = resolve_known_id(s, ComponentKind::Engine) {
                    out.push(id);
                }
            }
        }
        sel.engines = out;
    }
    if let Some(uis) = launcher.get("uis").and_then(Item::as_array) {
        let mut out = Vec::new();
        for v in uis {
            if let Some(s) = v.as_str() {
                if let Some(id) = resolve_known_id(s, ComponentKind::Ui) {
                    out.push(id);
                }
            }
        }
        sel.uis = out;
    }
    if let Some(bindings) = launcher.get("bindings").and_then(Item::as_table) {
        let mut out = BTreeMap::new();
        for (k, v) in bindings {
            let Some(s) = v.as_str() else { continue };
            let Some(ui_id) = resolve_known_id(k, ComponentKind::Ui) else {
                continue;
            };
            let Some(engine_id) = resolve_known_id(s, ComponentKind::Engine) else {
                continue;
            };
            out.insert(ui_id, engine_id);
        }
        sel.bindings = out;
    }
    sel.repair_bindings();
    sel
}

fn resolve_known_id(name: &str, kind: ComponentKind) -> Option<ComponentId> {
    catalog()
        .into_iter()
        .find(|c| c.id == name && c.kind == kind)
        .map(|c| c.id)
}

/// Merge-write the `[launcher]` table back into `path`, preserving every
/// other top-level table in the document.
pub(crate) fn save(path: &Path, selection: &Selection) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }

    let mut doc: DocumentMut = match fs::read_to_string(path) {
        Ok(t) => t
            .parse()
            .with_context(|| format!("parsing TOML at {}", path.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => DocumentMut::new(),
        Err(e) => return Err(anyhow::Error::from(e).context(format!("reading {}", path.display()))),
    };

    let launcher_item = doc.entry("launcher").or_insert(Item::Table(Table::new()));
    let launcher = launcher_item
        .as_table_mut()
        .context("[launcher] is not a table")?;

    let mut engines = Array::new();
    for id in &selection.engines {
        engines.push(*id);
    }
    launcher.insert("engines", value(engines));

    let mut uis = Array::new();
    for id in &selection.uis {
        uis.push(*id);
    }
    launcher.insert("uis", value(uis));

    let bindings_item = launcher
        .entry("bindings")
        .or_insert(Item::Table(Table::new()));
    let bindings = bindings_item
        .as_table_mut()
        .context("[launcher.bindings] is not a table")?;
    let known_keys: Vec<String> = bindings.iter().map(|(k, _)| k.to_owned()).collect();
    for k in known_keys {
        bindings.remove(&k);
    }
    for (ui_id, engine_id) in &selection.bindings {
        bindings.insert(ui_id, value(*engine_id));
    }

    fs::write(path, doc.to_string()).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
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
    use crate::catalog::{ENGINE_DOTNET, ENGINE_RUST, UI_DEBUGHOST, UI_GUI};
    use tempfile::tempdir;

    #[test]
    fn load_returns_defaults_when_file_missing() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("config.toml");
        let sel = load(&p).unwrap();
        assert_eq!(sel, Selection::default_preset());
    }

    #[test]
    fn round_trip_preserves_other_tables() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("config.toml");
        fs::write(
            &p,
            "[station_profile]\ncall = \"K7XYZ\"\n\n[launcher]\nengines = [\"rust-engine\"]\n",
        )
        .unwrap();

        let mut sel = Selection::default_preset();
        sel.engines = vec![ENGINE_DOTNET];
        sel.uis = vec![UI_GUI];
        sel.bindings.clear();
        sel.bindings.insert(UI_GUI, ENGINE_DOTNET);
        save(&p, &sel).unwrap();

        let text = fs::read_to_string(&p).unwrap();
        assert!(
            text.contains("[station_profile]"),
            "station_profile preserved"
        );
        assert!(text.contains("K7XYZ"), "station_profile call preserved");

        let reloaded = load(&p).unwrap();
        assert_eq!(reloaded.engines, vec![ENGINE_DOTNET]);
        assert_eq!(reloaded.uis, vec![UI_GUI]);
        assert_eq!(reloaded.bindings.get(&UI_GUI), Some(&ENGINE_DOTNET));
    }

    #[test]
    fn load_repairs_bindings_pointing_at_unselected_engine() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("config.toml");
        fs::write(
            &p,
            "[launcher]\nengines = [\"dotnet-engine\"]\nuis = [\"gui\", \"debughost\"]\n[launcher.bindings]\ngui = \"rust-engine\"\n",
        )
        .unwrap();
        let sel = load(&p).unwrap();
        assert_eq!(sel.bindings.get(&UI_GUI), Some(&ENGINE_DOTNET));
        assert_eq!(sel.bindings.get(&UI_DEBUGHOST), Some(&ENGINE_DOTNET));
    }

    #[test]
    fn unknown_component_names_are_dropped() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("config.toml");
        fs::write(
            &p,
            "[launcher]\nengines = [\"unknown-engine\", \"rust-engine\"]\nuis = [\"phantom-ui\"]\n",
        )
        .unwrap();
        let sel = load(&p).unwrap();
        assert_eq!(sel.engines, vec![ENGINE_RUST]);
        assert!(sel.uis.is_empty());
    }
}

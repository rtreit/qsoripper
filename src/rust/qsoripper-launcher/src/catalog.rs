//! Component catalog: every engine and UI the launcher knows how to start.

use std::path::PathBuf;

use crate::discovery::ArtifactRoot;

/// Stable identifier used for persistence and the binding map.
pub(crate) type ComponentId = &'static str;

/// Whether a component is an engine (acts as a gRPC server) or a UI client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ComponentKind {
    /// Engine that exposes a gRPC endpoint on a known TCP port.
    Engine,
    /// UI client that connects to one of the running engines.
    Ui,
}

/// Static metadata for a launchable component.
#[derive(Debug, Clone)]
pub(crate) struct ComponentSpec {
    pub id: ComponentId,
    pub display_name: &'static str,
    pub kind: ComponentKind,
    /// `Some(port)` for engines that listen on a canonical 127.0.0.1 port.
    pub engine_port: Option<u16>,
    /// `true` if the UI honors `QSORIPPER_ENGINE` / `QSORIPPER_ENDPOINT` envs
    /// to pick an engine. Used to decide whether to show a binding picker.
    pub engine_bindable: bool,
    /// `true` for terminal apps that need their own console window to render
    /// (e.g. ratatui-based TUIs). On Windows we spawn with `CREATE_NEW_CONSOLE`
    /// and inherit stdio; on Unix we route through a terminal emulator.
    pub wants_console: bool,
    /// Resolves the executable path under the published artifact tree.
    pub artifact: ArtifactSpec,
}

/// How to locate a component's executable inside `artifacts/publish/`.
#[derive(Debug, Clone)]
pub(crate) struct ArtifactSpec {
    /// Subdirectory name under `artifacts/publish/<sub>/<Configuration>/`.
    pub publish_subdir: &'static str,
    /// File name of the executable on the current platform (without `.exe`;
    /// platform suffix is appended in [`ArtifactSpec::executable_path`]).
    pub executable_stem: &'static str,
}

impl ArtifactSpec {
    /// Resolve the full executable path under the published artifact root.
    pub(crate) fn executable_path(&self, root: &ArtifactRoot) -> PathBuf {
        let mut p = root
            .path()
            .join(self.publish_subdir)
            .join(root.configuration());
        let mut name = self.executable_stem.to_owned();
        if cfg!(windows) {
            name.push_str(".exe");
        }
        p.push(name);
        p
    }
}

pub(crate) const ENGINE_RUST: ComponentId = "rust-engine";
pub(crate) const ENGINE_DOTNET: ComponentId = "dotnet-engine";
pub(crate) const UI_GUI: ComponentId = "gui";
pub(crate) const UI_DEBUGHOST: ComponentId = "debughost";
pub(crate) const UI_TUI: ComponentId = "tui";
pub(crate) const UI_CWSCOPE: ComponentId = "cwscope";

/// Static list of every component the launcher manages.
pub(crate) fn catalog() -> Vec<ComponentSpec> {
    vec![
        ComponentSpec {
            id: ENGINE_RUST,
            display_name: "Rust engine (qsoripper-server)",
            kind: ComponentKind::Engine,
            engine_port: Some(50051),
            engine_bindable: false,
            wants_console: false,
            artifact: ArtifactSpec {
                publish_subdir: "qsoripper-server",
                executable_stem: "qsoripper-server",
            },
        },
        ComponentSpec {
            id: ENGINE_DOTNET,
            display_name: ".NET engine (QsoRipper.Engine.DotNet)",
            kind: ComponentKind::Engine,
            engine_port: Some(50052),
            engine_bindable: false,
            wants_console: false,
            artifact: ArtifactSpec {
                publish_subdir: "qsoripper-engine-dotnet",
                executable_stem: "QsoRipper.Engine.DotNet",
            },
        },
        ComponentSpec {
            id: UI_GUI,
            display_name: "Avalonia GUI (QsoRipper.Gui)",
            kind: ComponentKind::Ui,
            engine_port: None,
            engine_bindable: true,
            wants_console: false,
            artifact: ArtifactSpec {
                publish_subdir: "qsoripper-gui",
                executable_stem: "QsoRipper.Gui",
            },
        },
        ComponentSpec {
            id: UI_DEBUGHOST,
            display_name: "DebugHost (http://localhost:5082)",
            kind: ComponentKind::Ui,
            engine_port: None,
            engine_bindable: true,
            wants_console: false,
            artifact: ArtifactSpec {
                publish_subdir: "qsoripper-debughost",
                executable_stem: "QsoRipper.DebugHost",
            },
        },
        ComponentSpec {
            id: UI_TUI,
            display_name: "Terminal UI (qsoripper-tui)",
            kind: ComponentKind::Ui,
            engine_port: None,
            engine_bindable: true,
            wants_console: true,
            artifact: ArtifactSpec {
                publish_subdir: "qsoripper-tui",
                executable_stem: "qsoripper-tui",
            },
        },
        ComponentSpec {
            id: UI_CWSCOPE,
            display_name: "CW Scope (CwDecoderGui)",
            kind: ComponentKind::Ui,
            engine_port: None,
            engine_bindable: false,
            wants_console: false,
            artifact: ArtifactSpec {
                publish_subdir: "cw-decoder-gui",
                executable_stem: "CwDecoderGui",
            },
        },
    ]
}

/// Look up a component by id.
pub(crate) fn find(id: &str) -> Option<ComponentSpec> {
    catalog().into_iter().find(|c| c.id == id)
}

/// Look up the canonical engine endpoint for the given engine component id.
pub(crate) fn engine_endpoint(id: &str) -> Option<String> {
    find(id)
        .filter(|c| c.kind == ComponentKind::Engine)
        .and_then(|c| c.engine_port)
        .map(|p| format!("http://127.0.0.1:{p}"))
}

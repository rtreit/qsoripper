//! ratatui rendering and the keyboard-driven launcher state machine.

use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Wrap};
use ratatui::Terminal;

use crate::catalog::{catalog, ComponentId, ComponentKind, ENGINE_DOTNET, ENGINE_RUST};
use crate::config;
use crate::discovery::ArtifactRoot;
use crate::model::Selection;
use crate::plan::{engine_plan, ui_plan};
use crate::ports::{is_port_listening, wait_for_port};
use crate::process::{spawn, stop_pid, ProcessRegistry};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Column {
    Engines,
    Uis,
    Bindings,
}

impl Column {
    fn cycle(self) -> Self {
        match self {
            Self::Engines => Self::Uis,
            Self::Uis => Self::Bindings,
            Self::Bindings => Self::Engines,
        }
    }
    fn cycle_back(self) -> Self {
        match self {
            Self::Engines => Self::Bindings,
            Self::Uis => Self::Engines,
            Self::Bindings => Self::Uis,
        }
    }
}

/// One status line per component. Updated after launch/stop and at each tick.
#[derive(Debug, Clone)]
struct Status {
    message: String,
    style: Style,
}

impl Status {
    fn idle() -> Self {
        Self {
            message: "idle".to_owned(),
            style: Style::default().fg(Color::DarkGray),
        }
    }
    fn running(pid: u32) -> Self {
        Self {
            message: format!("running (PID {pid})"),
            style: Style::default().fg(Color::Green),
        }
    }
    fn already_running() -> Self {
        Self {
            message: "already listening (external)".to_owned(),
            style: Style::default().fg(Color::Yellow),
        }
    }
    fn failed(reason: &str) -> Self {
        Self {
            message: format!("failed: {reason}"),
            style: Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        }
    }
    fn starting() -> Self {
        Self {
            message: "starting...".to_owned(),
            style: Style::default().fg(Color::Cyan),
        }
    }
    fn stopped() -> Self {
        Self {
            message: "stopped".to_owned(),
            style: Style::default().fg(Color::Gray),
        }
    }
}

/// Top-level TUI state.
pub(crate) struct AppState {
    pub selection: Selection,
    pub config_path: PathBuf,
    pub artifact_root: ArtifactRoot,
    column: Column,
    cursor: BTreeMap<Column, usize>,
    statuses: BTreeMap<ComponentId, Status>,
    registry: ProcessRegistry,
    last_message: String,
    should_quit: bool,
}

impl AppState {
    pub(crate) fn new(
        selection: Selection,
        config_path: PathBuf,
        artifact_root: ArtifactRoot,
    ) -> Self {
        let mut cursor = BTreeMap::new();
        cursor.insert(Column::Engines, 0);
        cursor.insert(Column::Uis, 0);
        cursor.insert(Column::Bindings, 0);
        Self {
            selection,
            config_path,
            artifact_root,
            column: Column::Engines,
            cursor,
            statuses: BTreeMap::new(),
            registry: ProcessRegistry::new(),
            last_message: "Press ? for help, Enter to launch selected, Q to quit.".to_owned(),
            should_quit: false,
        }
    }

    fn engine_components() -> Vec<ComponentId> {
        catalog()
            .into_iter()
            .filter(|c| c.kind == ComponentKind::Engine)
            .map(|c| c.id)
            .collect()
    }

    fn ui_components() -> Vec<ComponentId> {
        catalog()
            .into_iter()
            .filter(|c| c.kind == ComponentKind::Ui)
            .map(|c| c.id)
            .collect()
    }

    fn bindable_uis_selected(&self) -> Vec<ComponentId> {
        Self::ui_components()
            .into_iter()
            .filter(|id| {
                catalog()
                    .into_iter()
                    .any(|c| c.id == *id && c.engine_bindable)
                    && self.selection.ui_selected(id)
            })
            .collect()
    }

    fn current_list(&self) -> Vec<ComponentId> {
        match self.column {
            Column::Engines => Self::engine_components(),
            Column::Uis => Self::ui_components(),
            Column::Bindings => self.bindable_uis_selected(),
        }
    }

    fn move_cursor(&mut self, delta: i32) {
        let len = i32::try_from(self.current_list().len()).unwrap_or(i32::MAX);
        if len == 0 {
            return;
        }
        let cur = self.cursor.entry(self.column).or_insert(0);
        let cur_i = i32::try_from(*cur).unwrap_or(0);
        let new = (cur_i + delta).rem_euclid(len);
        *cur = usize::try_from(new).unwrap_or(0);
    }

    fn current_id(&self) -> Option<ComponentId> {
        let list = self.current_list();
        let cur = *self.cursor.get(&self.column).unwrap_or(&0);
        list.get(cur).copied()
    }

    fn toggle_current(&mut self) {
        match self.column {
            Column::Engines | Column::Uis => {
                if let Some(id) = self.current_id() {
                    self.selection.toggle(id);
                    self.selection.repair_bindings();
                }
            }
            Column::Bindings => {
                if let Some(ui_id) = self.current_id() {
                    let engines = self.selection.engines.clone();
                    if engines.is_empty() {
                        "No engines selected; check an engine first."
                            .clone_into(&mut self.last_message);
                        return;
                    }
                    let current = self.selection.bindings.get(&ui_id).copied();
                    let idx = current
                        .and_then(|c| engines.iter().position(|e| *e == c))
                        .unwrap_or(0);
                    let next_idx = (idx + 1) % engines.len();
                    if let Some(&next) = engines.get(next_idx) {
                        self.selection.set_binding(ui_id, next);
                    }
                }
            }
        }
    }

    /// Spawn every selected engine, wait for readiness, then spawn each
    /// selected UI with the per-UI engine binding env vars.
    fn launch_selected(&mut self) {
        // Engines first.
        for engine_id in self.selection.engines.clone() {
            let Some(plan) = engine_plan(engine_id) else {
                continue;
            };
            let exe = plan.spec.artifact.executable_path(&self.artifact_root);
            let port = plan.spec.engine_port.unwrap_or(0);
            if port != 0 && is_port_listening(port, Duration::from_millis(200)) {
                self.statuses.insert(engine_id, Status::already_running());
                continue;
            }
            self.statuses.insert(engine_id, Status::starting());
            let arg_refs: Vec<&std::ffi::OsStr> = plan.args.iter().map(AsRef::as_ref).collect();
            match spawn(&plan.spec, &exe, &arg_refs, &plan.env, &mut self.registry) {
                Ok(p) => {
                    if port != 0
                        && !wait_for_port(port, Duration::from_secs(15), Duration::from_millis(300))
                    {
                        self.statuses.insert(
                            engine_id,
                            Status::failed(&format!("never came up on 127.0.0.1:{port}")),
                        );
                    } else {
                        self.statuses.insert(engine_id, Status::running(p.pid));
                    }
                }
                Err(e) => {
                    self.statuses
                        .insert(engine_id, Status::failed(&e.to_string()));
                }
            }
        }

        // UIs next.
        for ui_id in self.selection.uis.clone() {
            let Some(plan) = ui_plan(ui_id, &self.selection) else {
                continue;
            };
            let exe = plan.spec.artifact.executable_path(&self.artifact_root);
            self.statuses.insert(ui_id, Status::starting());
            let arg_refs: Vec<&std::ffi::OsStr> = plan.args.iter().map(AsRef::as_ref).collect();
            match spawn(&plan.spec, &exe, &arg_refs, &plan.env, &mut self.registry) {
                Ok(p) => {
                    self.statuses.insert(ui_id, Status::running(p.pid));
                }
                Err(e) => {
                    self.statuses.insert(ui_id, Status::failed(&e.to_string()));
                }
            }
        }

        // Persist selections on a successful launch attempt (best-effort).
        if let Err(e) = config::save(&self.config_path, &self.selection) {
            self.last_message = format!(
                "Launched (warning: failed to persist selections to {}: {e})",
                self.config_path.display()
            );
        } else {
            self.last_message = format!(
                "Launched. Selections saved to {}.",
                self.config_path.display()
            );
        }
    }

    /// Stop every PID the launcher started.
    fn stop_managed(&mut self) {
        let ids: Vec<ComponentId> = self.registry.iter().map(|p| p.component).collect();
        for id in ids {
            if let Some(p) = self.registry.remove(id) {
                if stop_pid(p.pid) {
                    self.statuses.insert(id, Status::stopped());
                } else {
                    self.statuses
                        .insert(id, Status::failed("could not signal PID"));
                }
            }
        }
        "Stopped all launcher-managed processes.".clone_into(&mut self.last_message);
    }

    fn status_for(&self, id: ComponentId) -> Status {
        self.statuses.get(&id).cloned().unwrap_or_else(Status::idle)
    }
}

fn engine_label(id: ComponentId) -> &'static str {
    match id {
        i if i == ENGINE_RUST => "Rust",
        i if i == ENGINE_DOTNET => ".NET",
        _ => id,
    }
}

fn render(frame: &mut ratatui::Frame, app: &AppState) {
    let area = frame.area();
    let [header_area, body_area, status_area, footer_area] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(8),
            Constraint::Length(7),
            Constraint::Length(3),
        ])
        .areas(area);

    let header = Paragraph::new(Line::from(vec![
        Span::styled(
            " QsoRipper Launcher ",
            Style::default().bg(Color::Blue).fg(Color::White).bold(),
        ),
        Span::raw("  artifacts: "),
        Span::styled(
            app.artifact_root.path().to_string_lossy().to_string(),
            Style::default().fg(Color::Cyan),
        ),
        Span::raw("  ("),
        Span::styled(
            app.artifact_root.configuration().to_owned(),
            Style::default().fg(Color::Yellow),
        ),
        Span::raw(")"),
    ]))
    .block(Block::default().borders(Borders::ALL));
    frame.render_widget(header, header_area);

    let [engines_area, uis_area, bindings_area] = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(33),
            Constraint::Percentage(33),
            Constraint::Percentage(34),
        ])
        .areas(body_area);

    render_column(frame, engines_area, app, Column::Engines, "Engines");
    render_column(frame, uis_area, app, Column::Uis, "UIs");
    render_column(frame, bindings_area, app, Column::Bindings, "Bindings");

    let status_items: Vec<ListItem> = catalog()
        .into_iter()
        .map(|spec| {
            let st = app.status_for(spec.id);
            let line = Line::from(vec![
                Span::styled(format!("  {:30} ", spec.display_name), Style::default()),
                Span::styled(st.message, st.style),
            ]);
            ListItem::new(line)
        })
        .collect();
    let status =
        List::new(status_items).block(Block::default().title(" Status ").borders(Borders::ALL));
    frame.render_widget(status, status_area);

    let footer = Paragraph::new(Line::from(vec![
        Span::styled("Space", Style::default().fg(Color::Yellow).bold()),
        Span::raw(" toggle  "),
        Span::styled("Tab", Style::default().fg(Color::Yellow).bold()),
        Span::raw(" cycle column  "),
        Span::styled("Enter", Style::default().fg(Color::Yellow).bold()),
        Span::raw(" launch  "),
        Span::styled("S", Style::default().fg(Color::Yellow).bold()),
        Span::raw(" stop  "),
        Span::styled("R", Style::default().fg(Color::Yellow).bold()),
        Span::raw(" restart  "),
        Span::styled("Q", Style::default().fg(Color::Yellow).bold()),
        Span::raw(" quit   "),
        Span::styled(&app.last_message, Style::default().fg(Color::DarkGray)),
    ]))
    .block(Block::default().borders(Borders::ALL))
    .wrap(Wrap { trim: true });
    frame.render_widget(footer, footer_area);
}

fn render_column(
    frame: &mut ratatui::Frame,
    area: Rect,
    app: &AppState,
    column: Column,
    title: &str,
) {
    let active = app.column == column;
    let list_ids: Vec<ComponentId> = match column {
        Column::Engines => AppState::engine_components(),
        Column::Uis => AppState::ui_components(),
        Column::Bindings => app.bindable_uis_selected(),
    };
    let cursor = *app.cursor.get(&column).unwrap_or(&0);

    let items: Vec<ListItem> = list_ids
        .iter()
        .enumerate()
        .map(|(i, id)| {
            let display_name = catalog()
                .into_iter()
                .find(|c| c.id == *id)
                .map_or(*id, |c| c.display_name);
            let checked = match column {
                Column::Engines => app.selection.engine_selected(id),
                Column::Uis => app.selection.ui_selected(id),
                Column::Bindings => true,
            };
            let check = match column {
                Column::Bindings => "  ",
                Column::Engines | Column::Uis => {
                    if checked {
                        "[x] "
                    } else {
                        "[ ] "
                    }
                }
            };
            let binding_suffix = if column == Column::Bindings {
                let engine = app
                    .selection
                    .bindings
                    .get(id)
                    .copied()
                    .map_or("(unset)", engine_label);
                format!("  -> {engine}")
            } else {
                String::new()
            };
            let style = if active && i == cursor {
                Style::default().bg(Color::DarkGray).fg(Color::White).bold()
            } else {
                Style::default()
            };
            let text = format!("{check}{display_name}{binding_suffix}");
            ListItem::new(text).style(style)
        })
        .collect();

    let title_style = if active {
        Style::default().fg(Color::Yellow).bold()
    } else {
        Style::default()
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(format!(" {title} "), title_style));
    let list = List::new(items).block(block);
    frame.render_widget(list, area);
}

fn handle_key(app: &mut AppState, key: KeyEvent) {
    if key.kind != KeyEventKind::Press {
        return;
    }
    match (key.code, key.modifiers) {
        (KeyCode::Char('q') | KeyCode::Esc, _) => app.should_quit = true,
        (KeyCode::Tab, _) => app.column = app.column.cycle(),
        (KeyCode::BackTab, _) => app.column = app.column.cycle_back(),
        (KeyCode::Up | KeyCode::Char('k'), _) => app.move_cursor(-1),
        (KeyCode::Down | KeyCode::Char('j'), _) => app.move_cursor(1),
        (KeyCode::Char(' '), _) => app.toggle_current(),
        (KeyCode::Enter, _) => app.launch_selected(),
        (KeyCode::Char('s'), KeyModifiers::NONE) => app.stop_managed(),
        (KeyCode::Char('r'), KeyModifiers::NONE) => {
            app.stop_managed();
            app.launch_selected();
        }
        _ => {}
    }
}

/// Run the TUI event loop.
pub(crate) fn run(app: &mut AppState) -> Result<()> {
    use crossterm::execute;
    use crossterm::terminal::{
        disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
    };

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = (|| -> Result<()> {
        let tick = Duration::from_millis(250);
        let mut last_tick = Instant::now();
        while !app.should_quit {
            terminal.draw(|f| render(f, app))?;
            let timeout = tick.saturating_sub(last_tick.elapsed());
            if event::poll(timeout)? {
                if let Event::Key(key) = event::read()? {
                    handle_key(app, key);
                }
            }
            if last_tick.elapsed() >= tick {
                last_tick = Instant::now();
            }
        }
        Ok(())
    })();

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

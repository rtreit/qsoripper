//! ratatui rendering and keyboard-driven editor state.

use std::io;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Result};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};
use ratatui::Terminal;

use crate::model::{BoolField, Profile, ProfileIndex, WinKeyerImage};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    User1,
    User2,
    Global,
    Raw,
}

impl Focus {
    fn label(self) -> &'static str {
        match self {
            Self::User1 => "User 1",
            Self::User2 => "User 2",
            Self::Global => "Global",
            Self::Raw => "Raw",
        }
    }

    fn next(self) -> Self {
        match self {
            Self::User1 => Self::User2,
            Self::User2 => Self::Global,
            Self::Global => Self::Raw,
            Self::Raw => Self::User1,
        }
    }

    fn previous(self) -> Self {
        match self {
            Self::User1 => Self::Raw,
            Self::User2 => Self::User1,
            Self::Global => Self::User2,
            Self::Raw => Self::Global,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProfileField {
    Speed,
    CommandSpeed,
    KeyerMode,
    SidetoneHz,
    Weight,
    PttLead,
    PttTail,
    MinWpm,
    MaxWpm,
    KeyComp,
    Farnsworth,
    PaddleSample,
    DitDahRatio,
    OutputPort,
    PttEnabled,
    SidetoneEnabled,
    Autospace,
    ContestSpacing,
    PaddleSwap,
    PaddleOnlySidetone,
    PaddleMute,
    So2r,
    FastCommand,
    CutZero,
    CutNine,
    PaddleHang,
    Letterspace,
    Tune50,
}

impl ProfileField {
    const ALL: [Self; 28] = [
        Self::Speed,
        Self::CommandSpeed,
        Self::KeyerMode,
        Self::SidetoneHz,
        Self::Weight,
        Self::PttLead,
        Self::PttTail,
        Self::MinWpm,
        Self::MaxWpm,
        Self::KeyComp,
        Self::Farnsworth,
        Self::PaddleSample,
        Self::DitDahRatio,
        Self::OutputPort,
        Self::PttEnabled,
        Self::SidetoneEnabled,
        Self::Autospace,
        Self::ContestSpacing,
        Self::PaddleSwap,
        Self::PaddleOnlySidetone,
        Self::PaddleMute,
        Self::So2r,
        Self::FastCommand,
        Self::CutZero,
        Self::CutNine,
        Self::PaddleHang,
        Self::Letterspace,
        Self::Tune50,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Speed => "Operating WPM",
            Self::CommandSpeed => "Command WPM",
            Self::KeyerMode => "Keyer mode",
            Self::SidetoneHz => "Sidetone Hz",
            Self::Weight => "Weight %",
            Self::PttLead => "PTT lead",
            Self::PttTail => "PTT tail",
            Self::MinWpm => "Speed pot min",
            Self::MaxWpm => "Speed pot max",
            Self::KeyComp => "Key comp ms",
            Self::Farnsworth => "Farnsworth WPM",
            Self::PaddleSample => "Paddle sample %",
            Self::DitDahRatio => "Dit/dah setting",
            Self::OutputPort => "Output port",
            Self::PttEnabled => "PTT enabled",
            Self::SidetoneEnabled => "Sidetone enabled",
            Self::Autospace => "Autospace",
            Self::ContestSpacing => "Contest spacing",
            Self::PaddleSwap => "Paddle swap",
            Self::PaddleOnlySidetone => "Paddle-only tone",
            Self::PaddleMute => "Paddle mute",
            Self::So2r => "SO2R",
            Self::FastCommand => "Fast command",
            Self::CutZero => "Cut zero",
            Self::CutNine => "Cut nine",
            Self::PaddleHang => "Paddle hang",
            Self::Letterspace => "Letterspace %",
            Self::Tune50 => "50% tune",
        }
    }

    fn editable_hint(self) -> &'static str {
        match self {
            Self::KeyerMode | Self::OutputPort => "Enter/Space cycles",
            Self::PttEnabled
            | Self::SidetoneEnabled
            | Self::Autospace
            | Self::ContestSpacing
            | Self::PaddleSwap
            | Self::PaddleOnlySidetone
            | Self::PaddleMute
            | Self::So2r
            | Self::FastCommand
            | Self::CutZero
            | Self::CutNine
            | Self::Tune50 => "Enter/Space toggles",
            _ => "Enter edits",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GlobalField {
    FirstExtension,
    RttyP1,
    RttyP2,
}

impl GlobalField {
    const ALL: [Self; 3] = [Self::FirstExtension, Self::RttyP1, Self::RttyP2];
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum EditTarget {
    Profile(ProfileIndex, ProfileField),
    Global(GlobalField),
    Raw(usize),
}

pub(crate) struct AppState {
    path: PathBuf,
    image: WinKeyerImage,
    original: WinKeyerImage,
    focus: Focus,
    profile_cursor: usize,
    global_cursor: usize,
    raw_offset: usize,
    raw_window_start: usize,
    input: Option<InputState>,
    show_help: bool,
    status: String,
    should_quit: bool,
}

#[derive(Debug, Clone)]
struct InputState {
    target: EditTarget,
    label: String,
    value: String,
}

impl AppState {
    pub(crate) fn new(path: PathBuf, image: WinKeyerImage) -> Self {
        Self {
            path,
            original: image.clone(),
            image,
            focus: Focus::User1,
            profile_cursor: 0,
            global_cursor: 0,
            raw_offset: 0,
            raw_window_start: 0,
            input: None,
            show_help: false,
            status: "Tab changes pane, ↑/↓ moves, Enter edits, S saves, ? help, Q quits."
                .to_owned(),
            should_quit: false,
        }
    }

    fn is_dirty(&self) -> bool {
        self.image != self.original
    }

    fn handle_key(&mut self, key: KeyEvent) {
        if key.kind != KeyEventKind::Press {
            return;
        }
        if self.input.is_some() {
            self.handle_input_key(key);
            return;
        }
        if self.show_help {
            self.show_help = false;
            return;
        }

        match key.code {
            KeyCode::Char('q' | 'Q') | KeyCode::Esc => self.should_quit = true,
            KeyCode::Char('?') => self.show_help = true,
            KeyCode::Tab | KeyCode::Right => self.focus = self.focus.next(),
            KeyCode::BackTab | KeyCode::Left => self.focus = self.focus.previous(),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::PageUp => self.move_selection(-10),
            KeyCode::PageDown => self.move_selection(10),
            KeyCode::Home => self.move_home(),
            KeyCode::End => self.move_end(),
            KeyCode::Enter | KeyCode::Char(' ') => self.activate_selected(),
            KeyCode::Char('s' | 'S') => self.save(),
            KeyCode::Char('r' | 'R') => self.reload(),
            KeyCode::Char('g' | 'G') => {
                self.focus = Focus::Global;
                self.global_cursor = 0;
            }
            KeyCode::Char('x' | 'X') => self.focus = Focus::Raw,
            KeyCode::Char('1') => self.focus = Focus::User1,
            KeyCode::Char('2') => self.focus = Focus::User2,
            _ => {}
        }
    }

    fn handle_input_key(&mut self, key: KeyEvent) {
        let Some(input) = &mut self.input else {
            return;
        };
        match key.code {
            KeyCode::Esc => {
                self.input = None;
                "Edit cancelled.".clone_into(&mut self.status);
            }
            KeyCode::Enter => self.commit_input(),
            KeyCode::Backspace => {
                input.value.pop();
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                input.value.clear();
            }
            KeyCode::Char(ch) if ch.is_ascii_hexdigit() || ch == 'x' || ch == 'X' => {
                input.value.push(ch);
            }
            _ => {}
        }
    }

    fn move_selection(&mut self, delta: isize) {
        match self.focus {
            Focus::User1 | Focus::User2 => {
                self.profile_cursor =
                    clamp_move(self.profile_cursor, ProfileField::ALL.len(), delta);
            }
            Focus::Global => {
                self.global_cursor = clamp_move(self.global_cursor, GlobalField::ALL.len(), delta);
            }
            Focus::Raw => {
                self.raw_offset = clamp_move(self.raw_offset, crate::model::EEPROM_LEN, delta);
                self.adjust_raw_window();
            }
        }
    }

    fn move_home(&mut self) {
        match self.focus {
            Focus::User1 | Focus::User2 => self.profile_cursor = 0,
            Focus::Global => self.global_cursor = 0,
            Focus::Raw => {
                self.raw_offset = 0;
                self.raw_window_start = 0;
            }
        }
    }

    fn move_end(&mut self) {
        match self.focus {
            Focus::User1 | Focus::User2 => self.profile_cursor = ProfileField::ALL.len() - 1,
            Focus::Global => self.global_cursor = GlobalField::ALL.len() - 1,
            Focus::Raw => {
                self.raw_offset = crate::model::EEPROM_LEN - 1;
                self.adjust_raw_window();
            }
        }
    }

    fn activate_selected(&mut self) {
        match self.focus {
            Focus::User1 | Focus::User2 => {
                let index = if self.focus == Focus::User1 {
                    ProfileIndex::User1
                } else {
                    ProfileIndex::User2
                };
                if let Some(field) = ProfileField::ALL.get(self.profile_cursor).copied() {
                    self.activate_profile(index, field);
                }
            }
            Focus::Global => {
                if GlobalField::ALL.get(self.global_cursor).copied()
                    == Some(GlobalField::FirstExtension)
                {
                    self.start_input(
                        EditTarget::Global(GlobalField::FirstExtension),
                        "First element extension",
                        self.image.first_extension_ms().to_string(),
                    );
                }
            }
            Focus::Raw => {
                let value = match self.image.raw(self.raw_offset) {
                    Ok(value) => format!("{value:02X}"),
                    Err(error) => {
                        self.status = error.to_string();
                        return;
                    }
                };
                self.start_input(
                    EditTarget::Raw(self.raw_offset),
                    format!("Raw 0x{:03X}", self.raw_offset),
                    value,
                );
            }
        }
    }

    fn activate_profile(&mut self, index: ProfileIndex, field: ProfileField) {
        let profile = self.image.profile(index);
        match field {
            ProfileField::KeyerMode => {
                self.image.profile_mut(index).cycle_keyer_mode();
                self.status = format!("{} keyer mode changed.", index.label());
            }
            ProfileField::OutputPort => {
                self.image.profile_mut(index).cycle_output_port();
                self.status = format!("{} output port changed.", index.label());
            }
            ProfileField::PttEnabled => {
                self.toggle(index, BoolField::PttEnabled, !profile.ptt_enabled());
            }
            ProfileField::SidetoneEnabled => {
                self.toggle(
                    index,
                    BoolField::SidetoneEnabled,
                    !profile.sidetone_enabled(),
                );
            }
            ProfileField::Autospace => {
                self.toggle(index, BoolField::Autospace, !profile.autospace());
            }
            ProfileField::ContestSpacing => {
                self.toggle(index, BoolField::ContestSpacing, !profile.contest_spacing());
            }
            ProfileField::PaddleSwap => {
                self.toggle(index, BoolField::PaddleSwap, !profile.paddle_swap());
            }
            ProfileField::PaddleOnlySidetone => self.toggle(
                index,
                BoolField::PaddleOnlySidetone,
                !profile.paddle_only_sidetone(),
            ),
            ProfileField::PaddleMute => {
                self.toggle(index, BoolField::PaddleMute, !profile.paddle_mute());
            }
            ProfileField::So2r => self.toggle(index, BoolField::So2r, !profile.so2r()),
            ProfileField::FastCommand => {
                self.toggle(index, BoolField::FastCommand, !profile.fast_command());
            }
            ProfileField::CutZero => self.toggle(index, BoolField::CutZero, !profile.cut_zero()),
            ProfileField::CutNine => self.toggle(index, BoolField::CutNine, !profile.cut_nine()),
            ProfileField::Tune50 => self.toggle(index, BoolField::Tune50, !profile.tune_50()),
            other => self.start_input(
                EditTarget::Profile(index, other),
                format!("{} {}", index.label(), other.label()),
                profile_value(profile, other),
            ),
        }
    }

    fn toggle(&mut self, index: ProfileIndex, field: BoolField, value: bool) {
        self.image.profile_mut(index).set_bool(field, value);
        self.status = format!("{} flag changed.", index.label());
    }

    fn start_input(&mut self, target: EditTarget, label: impl Into<String>, value: String) {
        self.input = Some(InputState {
            target,
            label: label.into(),
            value,
        });
        "Type a number, Enter applies, Esc cancels.".clone_into(&mut self.status);
    }

    fn commit_input(&mut self) {
        let Some(input) = self.input.take() else {
            return;
        };
        let parsed = parse_number(&input.value);
        let result = match (input.target, parsed) {
            (_, Err(error)) => Err(error),
            (EditTarget::Profile(index, field), Ok(value)) => {
                self.apply_profile_value(index, field, value)
            }
            (EditTarget::Global(GlobalField::FirstExtension), Ok(value)) => self
                .image
                .set_first_extension_ms(value)
                .map_err(|error| error.to_string()),
            (EditTarget::Global(_), Ok(_)) => Err(
                "RTTY registers are decoded read-only for now; use raw pane for byte edits."
                    .to_owned(),
            ),
            (EditTarget::Raw(offset), Ok(value)) => {
                let value = u8::try_from(value).map_err(|_| "raw byte must be 0-255".to_owned());
                value.and_then(|byte| {
                    self.image
                        .set_raw(offset, byte)
                        .map_err(|error| error.to_string())
                })
            }
        };

        match result {
            Ok(()) => "Edit applied. Press S to save.".clone_into(&mut self.status),
            Err(error) => self.status = error,
        }
    }

    fn apply_profile_value(
        &mut self,
        index: ProfileIndex,
        field: ProfileField,
        value: u16,
    ) -> Result<(), String> {
        let mut profile = self.image.profile_mut(index);
        let result = match field {
            ProfileField::Speed => profile.set_speed_wpm(value),
            ProfileField::CommandSpeed => profile.set_command_wpm(value),
            ProfileField::SidetoneHz => profile.set_sidetone_hz(value),
            ProfileField::Weight => profile.set_weight_percent(value),
            ProfileField::PttLead => profile.set_ptt_lead(value),
            ProfileField::PttTail => profile.set_ptt_tail(value),
            ProfileField::MinWpm => profile.set_min_wpm(value),
            ProfileField::MaxWpm => profile.set_max_wpm(value),
            ProfileField::KeyComp => profile.set_key_comp_ms(value),
            ProfileField::Farnsworth => profile.set_farnsworth_wpm(value),
            ProfileField::PaddleSample => profile.set_paddle_sample_percent(value),
            ProfileField::DitDahRatio => profile.set_dit_dah_ratio(value),
            ProfileField::PaddleHang => profile.set_paddle_hang(value),
            ProfileField::Letterspace => profile.set_letterspace_percent(value),
            _ => return Err("selected field is not numeric".to_owned()),
        };
        result.map_err(|error| error.to_string())
    }

    fn save(&mut self) {
        match self.image.save(&self.path) {
            Ok(()) => {
                self.original = self.image.clone();
                self.status = format!("Saved {}.", self.path.display());
            }
            Err(error) => self.status = format!("Save failed: {error}"),
        }
    }

    fn reload(&mut self) {
        match WinKeyerImage::load(&self.path) {
            Ok(image) => {
                self.original = image.clone();
                self.image = image;
                self.status = format!("Reloaded {}.", self.path.display());
            }
            Err(error) => self.status = format!("Reload failed: {error}"),
        }
    }

    fn adjust_raw_window(&mut self) {
        if self.raw_offset < self.raw_window_start {
            self.raw_window_start = self.raw_offset / 16 * 16;
        }
        if self.raw_offset >= self.raw_window_start + 128 {
            self.raw_window_start = self.raw_offset.saturating_sub(112) / 16 * 16;
        }
    }
}

pub(crate) fn run(app: &mut AppState) -> Result<()> {
    crossterm::terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(
        stdout,
        crossterm::terminal::EnterAlternateScreen,
        crossterm::event::EnableMouseCapture
    )?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let result = run_loop(&mut terminal, app);
    crossterm::terminal::disable_raw_mode()?;
    crossterm::execute!(
        terminal.backend_mut(),
        crossterm::terminal::LeaveAlternateScreen,
        crossterm::event::DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    result
}

fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut AppState,
) -> Result<()> {
    loop {
        terminal.draw(|frame| render(frame, app))?;
        if app.should_quit {
            if app.is_dirty() {
                bail!("quit with unsaved WinKeyer edits; press S before Q to save");
            }
            return Ok(());
        }
        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                app.handle_key(key);
            }
        }
    }
}

fn render(frame: &mut ratatui::Frame<'_>, app: &AppState) {
    let area = frame.area();
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(10),
            Constraint::Length(4),
        ])
        .split(area);

    let header = rows.first().copied().unwrap_or(area);
    let main = rows.get(1).copied().unwrap_or(area);
    let footer = rows.get(2).copied().unwrap_or(area);

    render_header(frame, app, header);
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(35),
            Constraint::Percentage(35),
            Constraint::Percentage(30),
        ])
        .split(main);
    render_profile(
        frame,
        app,
        ProfileIndex::User1,
        body.first().copied().unwrap_or(main),
    );
    render_profile(
        frame,
        app,
        ProfileIndex::User2,
        body.get(1).copied().unwrap_or(main),
    );
    render_right_column(frame, app, body.get(2).copied().unwrap_or(main));
    render_footer(frame, app, footer);

    if app.show_help {
        render_help(frame, centered_rect(78, 70, area));
    }
    if let Some(input) = &app.input {
        render_input(frame, input, centered_rect(60, 24, area));
    }
}

fn render_header(frame: &mut ratatui::Frame<'_>, app: &AppState, area: Rect) {
    let dirty = if app.is_dirty() { "modified" } else { "clean" };
    let validation = app.image.validate();
    let validation_text = if validation.is_empty() {
        Span::styled("valid", Style::default().fg(Color::Green))
    } else {
        Span::styled(
            "invalid",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        )
    };
    let line = Line::from(vec![
        Span::styled(
            "WinKeyer EEPROM Editor  ",
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw(app.path.display().to_string()),
        Span::raw("  "),
        Span::styled(
            dirty,
            Style::default().fg(if app.is_dirty() {
                Color::Yellow
            } else {
                Color::Green
            }),
        ),
        Span::raw("  "),
        validation_text,
        Span::raw(format!("  Focus: {}", app.focus.label())),
    ]);
    frame.render_widget(
        Paragraph::new(line).block(Block::default().borders(Borders::ALL)),
        area,
    );
}

fn render_profile(frame: &mut ratatui::Frame<'_>, app: &AppState, index: ProfileIndex, area: Rect) {
    let profile = app.image.profile(index);
    let selected = match index {
        ProfileIndex::User1 => app.focus == Focus::User1,
        ProfileIndex::User2 => app.focus == Focus::User2,
    };
    let items = ProfileField::ALL
        .iter()
        .enumerate()
        .map(|(cursor, field)| {
            let marker = if selected && cursor == app.profile_cursor {
                ">"
            } else {
                " "
            };
            let style = if selected && cursor == app.profile_cursor {
                Style::default().fg(Color::Black).bg(Color::Cyan)
            } else {
                Style::default()
            };
            ListItem::new(Line::from(vec![
                Span::raw(marker),
                Span::raw(" "),
                Span::styled(
                    format!("{:<20}", field.label()),
                    Style::default().fg(Color::Gray),
                ),
                Span::styled(profile_value(profile, *field), style),
                Span::raw("  "),
                Span::styled(field.editable_hint(), Style::default().fg(Color::DarkGray)),
            ]))
        })
        .collect::<Vec<_>>();

    frame.render_widget(
        List::new(items).block(
            Block::default()
                .borders(Borders::ALL)
                .title(index.label())
                .border_style(if selected {
                    Style::default().fg(Color::Cyan)
                } else {
                    Style::default()
                }),
        ),
        area,
    );
}

fn render_right_column(frame: &mut ratatui::Frame<'_>, app: &AppState, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(8), Constraint::Min(8)])
        .split(area);
    render_global(frame, app, rows.first().copied().unwrap_or(area));
    render_raw(frame, app, rows.get(1).copied().unwrap_or(area));
}

fn render_global(frame: &mut ratatui::Frame<'_>, app: &AppState, area: Rect) {
    let selected = app.focus == Focus::Global;
    let values = [
        (
            "First extension ms",
            app.image.first_extension_ms().to_string(),
            "Enter edits",
        ),
        (
            "RTTY P1",
            format!("0x{:02X}", app.image.rtty_p1()),
            "raw editable",
        ),
        (
            "RTTY P2",
            format!("0x{:02X}", app.image.rtty_p2()),
            "raw editable",
        ),
    ];
    let items = values
        .iter()
        .enumerate()
        .map(|(cursor, (label, value, hint))| {
            let marker = if selected && cursor == app.global_cursor {
                ">"
            } else {
                " "
            };
            let style = if selected && cursor == app.global_cursor {
                Style::default().fg(Color::Black).bg(Color::Cyan)
            } else {
                Style::default()
            };
            ListItem::new(Line::from(vec![
                Span::raw(marker),
                Span::raw(" "),
                Span::styled(format!("{label:<20}"), Style::default().fg(Color::Gray)),
                Span::styled(value.clone(), style),
                Span::raw("  "),
                Span::styled(*hint, Style::default().fg(Color::DarkGray)),
            ]))
        })
        .collect::<Vec<_>>();

    frame.render_widget(
        List::new(items).block(
            Block::default()
                .borders(Borders::ALL)
                .title("Global")
                .border_style(if selected {
                    Style::default().fg(Color::Cyan)
                } else {
                    Style::default()
                }),
        ),
        area,
    );
}

fn render_raw(frame: &mut ratatui::Frame<'_>, app: &AppState, area: Rect) {
    let selected = app.focus == Focus::Raw;
    let mut lines = Vec::new();
    let end = (app.raw_window_start + 128).min(crate::model::EEPROM_LEN);
    for row in (app.raw_window_start..end).step_by(16) {
        let mut spans = vec![Span::styled(
            format!("{row:03X}  "),
            Style::default().fg(Color::Gray),
        )];
        for offset in row..(row + 16).min(crate::model::EEPROM_LEN) {
            let value = app.image.raw(offset).unwrap_or(0);
            let style = if selected && offset == app.raw_offset {
                Style::default().fg(Color::Black).bg(Color::Cyan)
            } else if value == 0xff {
                Style::default().fg(Color::DarkGray)
            } else {
                Style::default()
            };
            spans.push(Span::styled(format!("{value:02X} "), style));
        }
        lines.push(Line::from(spans));
    }
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title("Raw bytes")
                .border_style(if selected {
                    Style::default().fg(Color::Cyan)
                } else {
                    Style::default()
                }),
        ),
        area,
    );
}

fn render_footer(frame: &mut ratatui::Frame<'_>, app: &AppState, area: Rect) {
    let validation = app.image.validate();
    let mut text = vec![
        Line::from(vec![
            Span::styled("Keys ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("Tab pane  ↑/↓ move  Enter edit/toggle/cycle  S save  R reload  X raw  G global  ? help  Q quit"),
        ]),
        Line::from(app.status.clone()),
    ];
    if !validation.is_empty() {
        text.push(Line::from(Span::styled(
            validation.join("; "),
            Style::default().fg(Color::Red),
        )));
    }
    frame.render_widget(
        Paragraph::new(text)
            .wrap(Wrap { trim: true })
            .block(Block::default().borders(Borders::ALL).title("Status")),
        area,
    );
}

fn render_help(frame: &mut ratatui::Frame<'_>, area: Rect) {
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                "Keyboard-first WinKeyer editor",
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from("Tab / Shift+Tab / ← / →: switch panes"),
            Line::from("↑/↓ or k/j: move within a pane; PageUp/PageDown jump"),
            Line::from("Enter or Space: edit numeric fields, toggle booleans, cycle enum fields"),
            Line::from("S: save back to the opened .eep file"),
            Line::from("R: reload from disk and discard unsaved edits"),
            Line::from("1 / 2 / G / X: jump to User 1, User 2, Global, Raw panes"),
            Line::from(
                "Esc or Q: quit. Unsaved edits are refused so you do not lose work accidentally.",
            ),
            Line::from(""),
            Line::from("Raw byte edits accept decimal or hex, e.g. 78 or 0x4E."),
            Line::from("Press any key to close this help."),
        ])
        .wrap(Wrap { trim: true })
        .block(Block::default().borders(Borders::ALL).title("Help")),
        area,
    );
}

fn render_input(frame: &mut ratatui::Frame<'_>, input: &InputState, area: Rect) {
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(input.label.clone()),
            Line::from(""),
            Line::from(vec![
                Span::styled("> ", Style::default().fg(Color::Cyan)),
                Span::styled(
                    input.value.clone(),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(""),
            Line::from("Enter applies, Esc cancels, Ctrl+U clears."),
        ])
        .block(Block::default().borders(Borders::ALL).title("Edit value")),
        area,
    );
}

fn profile_value(profile: Profile<'_>, field: ProfileField) -> String {
    match field {
        ProfileField::Speed => format!("{} WPM", profile.speed_wpm()),
        ProfileField::CommandSpeed => format!("{} WPM", profile.command_wpm()),
        ProfileField::KeyerMode => profile.keyer_mode().to_string(),
        ProfileField::SidetoneHz => format!("{} Hz", profile.sidetone_hz()),
        ProfileField::Weight => format!("{}%", profile.weight_percent()),
        ProfileField::PttLead => profile.ptt_lead().to_string(),
        ProfileField::PttTail => profile.ptt_tail().to_string(),
        ProfileField::MinWpm => profile.min_wpm().to_string(),
        ProfileField::MaxWpm => profile.max_wpm().to_string(),
        ProfileField::KeyComp => format!("{} ms", profile.key_comp_ms()),
        ProfileField::Farnsworth => {
            if profile.farnsworth_wpm() == 0 {
                "off".to_owned()
            } else {
                format!("{} WPM", profile.farnsworth_wpm())
            }
        }
        ProfileField::PaddleSample => format!("{}%", profile.paddle_sample_percent()),
        ProfileField::DitDahRatio => {
            let tenths = profile.dah_ratio_tenths();
            format!(
                "{} (1:{}.{} )",
                profile.dit_dah_ratio_setting(),
                tenths / 10,
                tenths % 10
            )
        }
        ProfileField::OutputPort => profile.output_port().to_string(),
        ProfileField::PttEnabled => on_off(profile.ptt_enabled()),
        ProfileField::SidetoneEnabled => on_off(profile.sidetone_enabled()),
        ProfileField::Autospace => on_off(profile.autospace()),
        ProfileField::ContestSpacing => on_off(profile.contest_spacing()),
        ProfileField::PaddleSwap => on_off(profile.paddle_swap()),
        ProfileField::PaddleOnlySidetone => on_off(profile.paddle_only_sidetone()),
        ProfileField::PaddleMute => on_off(profile.paddle_mute()),
        ProfileField::So2r => on_off(profile.so2r()),
        ProfileField::FastCommand => format!(
            "{} / paddle status {}",
            on_off(profile.fast_command()),
            on_off(profile.paddle_status())
        ),
        ProfileField::CutZero => on_off(profile.cut_zero()),
        ProfileField::CutNine => on_off(profile.cut_nine()),
        ProfileField::PaddleHang => profile.paddle_hang().to_string(),
        ProfileField::Letterspace => format!("{}%", profile.letterspace_percent()),
        ProfileField::Tune50 => format!(
            "{} / bank2 {} / user2 {}",
            on_off(profile.tune_50()),
            on_off(profile.message_bank_2()),
            on_off(profile.selected_user_2())
        ),
    }
}

fn on_off(value: bool) -> String {
    if value {
        "on".to_owned()
    } else {
        "off".to_owned()
    }
}

fn parse_number(value: &str) -> Result<u16, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err("value cannot be empty".to_owned());
    }
    if let Some(hex) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    {
        u16::from_str_radix(hex, 16).map_err(|error| error.to_string())
    } else {
        value.parse::<u16>().map_err(|error| error.to_string())
    }
}

fn clamp_move(current: usize, len: usize, delta: isize) -> usize {
    let last = len.saturating_sub(1);
    if delta < 0 {
        current.saturating_sub(delta.unsigned_abs())
    } else {
        current.saturating_add(delta.unsigned_abs()).min(last)
    }
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    let middle = vertical.get(1).copied().unwrap_or(area);
    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(middle);
    horizontal.get(1).copied().unwrap_or(middle)
}

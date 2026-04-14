//! Binary entry point for the `QsoRipper` terminal UI.

mod app;
mod events;
mod form;
mod grpc;
mod ui;

use std::io;

use crossterm::{
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use tokio::sync::{mpsc, watch};

use app::App;
use events::{spawn_clock_task, spawn_key_task, spawn_lookup_task, AppEvent};
use form::{Field, LogForm, BANDS, MODES};

/// Application entry point.
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let endpoint = parse_endpoint_arg();

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run(&mut terminal, endpoint).await;

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

/// Parse `--endpoint <url>` from `argv`, defaulting to `http://127.0.0.1:50051`.
fn parse_endpoint_arg() -> String {
    let args: Vec<String> = std::env::args().collect();
    let mut endpoint = "http://127.0.0.1:50051".to_string();
    let mut iter = args.iter().peekable();
    while let Some(arg) = iter.next() {
        if arg == "--endpoint" {
            if let Some(next) = iter.next() {
                endpoint.clone_from(next);
            }
        }
    }
    endpoint
}

/// Main run loop — creates the app, spawns background tasks, and drives the event loop.
async fn run<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    endpoint: String,
) -> anyhow::Result<()> {
    let (event_tx, mut event_rx) = mpsc::unbounded_channel::<AppEvent>();
    let (lookup_tx, lookup_rx) = watch::channel(String::new());

    let mut app = App::new(endpoint);

    spawn_key_task(event_tx.clone());
    spawn_clock_task(event_tx.clone());
    spawn_lookup_task(lookup_rx, event_tx.clone(), app.endpoint.clone());

    // Prefetch space weather and recent QSOs on startup.
    {
        let tx = event_tx.clone();
        let ep = app.endpoint.clone();
        tokio::spawn(async move {
            if let Ok(ch) = grpc::create_channel(&ep).await {
                if let Ok(sw) = grpc::get_space_weather(ch).await {
                    let _ = tx.send(AppEvent::SpaceWeather(sw));
                }
            }
        });
    }
    {
        let tx = event_tx.clone();
        let ep = app.endpoint.clone();
        tokio::spawn(async move {
            if let Ok(ch) = grpc::create_channel(&ep).await {
                if let Ok(qsos) = grpc::list_recent_qsos(ch, 20).await {
                    let _ = tx.send(AppEvent::RecentQsos(qsos));
                }
            }
        });
    }

    terminal.draw(|f| ui::render_ui(&app, f))?;

    while app.running {
        if let Some(event) = event_rx.recv().await {
            let endpoint = app.endpoint.clone();
            handle_event(&mut app, event, &event_tx, &lookup_tx, &endpoint);
            app.expire_status();
            terminal.draw(|f| ui::render_ui(&app, f))?;
        }
    }

    Ok(())
}

/// Dispatch a single [`AppEvent`] to the appropriate handler.
fn handle_event(
    app: &mut App,
    event: AppEvent,
    event_tx: &mpsc::UnboundedSender<AppEvent>,
    lookup_tx: &watch::Sender<String>,
    endpoint: &str,
) {
    match event {
        AppEvent::Key(key) => handle_key(app, key, event_tx, lookup_tx, endpoint),
        AppEvent::Tick => {
            let now = chrono::Utc::now();
            app.form.time = now.format("%H:%M").to_string();
        }
        AppEvent::LookupResult(result) => {
            app.lookup_result = result;
        }
        AppEvent::SpaceWeather(sw) => {
            app.space_weather = sw;
        }
        AppEvent::QsoLogged(local_id) => {
            app.set_status(format!("QSO logged: {local_id}"));
            app.form = LogForm::new();
            app.lookup_result = None;
            refresh_recent_qsos(event_tx, endpoint);
        }
        AppEvent::QsoLogFailed(err) => {
            app.set_error(format!("Log failed: {err}"));
        }
        AppEvent::RecentQsos(qsos) => {
            app.recent_qsos = qsos;
        }
    }
}

/// Handle a key event in the current view.
fn handle_key(
    app: &mut App,
    key: crossterm::event::KeyEvent,
    event_tx: &mpsc::UnboundedSender<AppEvent>,
    lookup_tx: &watch::Sender<String>,
    endpoint: &str,
) {
    use crossterm::event::{KeyCode, KeyModifiers};

    if matches!(app.view, app::View::Help) {
        app.view = app::View::LogEntry;
        return;
    }

    if app.qso_list_focused {
        handle_qso_list_key(app, key, lookup_tx);
        return;
    }

    match key.code {
        KeyCode::Tab => match app.view {
            app::View::Advanced => app.form.next_advanced_field(),
            _ => app.form.next_field(),
        },
        KeyCode::BackTab => match app.view {
            app::View::Advanced => app.form.prev_advanced_field(),
            _ => app.form.prev_field(),
        },
        KeyCode::F(1) => app.view = app::View::Help,
        KeyCode::F(2) => match app.view {
            app::View::Advanced => {
                app.view = app::View::LogEntry;
                app.form.focused = Field::Callsign;
            }
            app::View::LogEntry => {
                app.view = app::View::Advanced;
                app.form.focused = Field::TxPower;
            }
            app::View::Help => {}
        },
        KeyCode::F(3) => {
            if !app.recent_qsos.is_empty() {
                app.qso_list_focused = true;
                app.qso_selected = Some(0);
            }
        }
        KeyCode::F(10) => spawn_log_qso(app, event_tx, endpoint),
        KeyCode::Esc => match app.view {
            app::View::Advanced => {
                app.view = app::View::LogEntry;
                app.form.focused = Field::Callsign;
            }
            app::View::LogEntry => {
                app.form = LogForm::new();
                app.lookup_result = None;
            }
            app::View::Help => {}
        },
        KeyCode::Char('q' | 'Q') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.running = false;
        }
        KeyCode::Left if app.form.is_cycle_field() => cycle_left(app),
        KeyCode::Right if app.form.is_cycle_field() => cycle_right(app),
        KeyCode::Backspace => {
            let focused = app.form.focused.clone();
            if let Some(text) = app.form.current_field_text_mut() {
                text.pop();
            }
            if focused == Field::Callsign {
                let callsign = app.form.callsign.clone();
                let _ = lookup_tx.send(callsign);
            }
        }
        KeyCode::Char(c) if key.modifiers.contains(KeyModifiers::ALT) => {
            jump_to_field(app, c);
        }
        KeyCode::Char(c) => handle_char_key(app, c, lookup_tx),
        _ => {}
    }
}

/// Navigate the QSO list with keyboard (active when `app.qso_list_focused` is true).
fn handle_qso_list_key(
    app: &mut App,
    key: crossterm::event::KeyEvent,
    lookup_tx: &watch::Sender<String>,
) {
    use crossterm::event::KeyCode;
    let max = app.recent_qsos.len().saturating_sub(1);
    match key.code {
        KeyCode::Up => {
            app.qso_selected = Some(match app.qso_selected {
                Some(i) if i > 0 => i - 1,
                _ => 0,
            });
        }
        KeyCode::Down => {
            app.qso_selected = Some(match app.qso_selected {
                Some(i) => (i + 1).min(max),
                None => 0,
            });
        }
        KeyCode::Enter => {
            if let Some(idx) = app.qso_selected {
                load_qso_into_form(app, idx, lookup_tx);
            } else {
                app.qso_list_focused = false;
            }
        }
        KeyCode::Esc | KeyCode::F(3) => {
            app.qso_list_focused = false;
            app.qso_selected = None;
        }
        _ => {}
    }
}

/// Handle a plain character key press — type-selects Band/Mode, or appends to text fields.
fn handle_char_key(app: &mut App, c: char, lookup_tx: &watch::Sender<String>) {
    let focused = app.form.focused.clone();
    match focused {
        Field::Band => app.form.type_select_band(c),
        Field::Mode => app.form.type_select_mode(c),
        _ => {
            if let Some(text) = app.form.current_field_text_mut() {
                if focused == Field::Callsign {
                    text.push(c.to_ascii_uppercase());
                } else {
                    text.push(c);
                }
            }
            if focused == Field::Callsign {
                let callsign = app.form.callsign.clone();
                let _ = lookup_tx.send(callsign);
            }
        }
    }
}

/// Jump form focus to the field bound to `ch` (Alt+key mapping).
fn jump_to_field(app: &mut App, ch: char) {
    let target = match ch.to_ascii_lowercase() {
        'c' => Field::Callsign,
        'b' => Field::Band,
        'm' => Field::Mode,
        's' => Field::RstSent,
        'r' => Field::RstRcvd,
        'o' => Field::Comment,
        'n' => Field::Notes,
        'f' => Field::FrequencyMhz,
        'd' => Field::Date,
        't' => Field::Time,
        _ => return,
    };
    if matches!(app.view, app::View::Advanced) {
        app.view = app::View::LogEntry;
    }
    app.form.focused = target;
    app.qso_list_focused = false;
}

/// Load the QSO at `idx` in `recent_qsos` into the form for re-logging.
fn load_qso_into_form(app: &mut App, idx: usize, lookup_tx: &watch::Sender<String>) {
    let Some(qso) = app.recent_qsos.get(idx) else {
        return;
    };
    app.form.callsign = qso.callsign.clone();
    if let Some(bi) = BANDS.iter().position(|&b| b == qso.band.as_str()) {
        app.form.band_idx = bi;
    }
    if let Some(mi) = MODES.iter().position(|&m| m == qso.mode.as_str()) {
        app.form.mode_idx = mi;
    }
    app.form.on_band_change();
    app.form.rst_sent = qso.rst_sent.clone();
    app.form.rst_rcvd = qso.rst_rcvd.clone();
    app.form.focused = Field::Callsign;
    app.qso_list_focused = false;
    app.qso_selected = None;
    if matches!(app.view, app::View::Advanced) {
        app.view = app::View::LogEntry;
    }
    let _ = lookup_tx.send(app.form.callsign.clone());
}

/// Spawn a task to log the current form contents and forward the result to the event channel.
fn spawn_log_qso(app: &App, event_tx: &mpsc::UnboundedSender<AppEvent>, endpoint: &str) {
    let tx = event_tx.clone();
    let ep = endpoint.to_string();
    let form_snap = app.form.clone();
    tokio::spawn(async move {
        match grpc::create_channel(&ep).await {
            Ok(ch) => match grpc::log_qso(ch, &form_snap).await {
                Ok(id) => {
                    let _ = tx.send(AppEvent::QsoLogged(id));
                }
                Err(e) => {
                    let _ = tx.send(AppEvent::QsoLogFailed(e.to_string()));
                }
            },
            Err(e) => {
                let _ = tx.send(AppEvent::QsoLogFailed(e.to_string()));
            }
        }
    });
}

/// Cycle the focused selector one step to the left (decreasing index).
fn cycle_left(app: &mut App) {
    match app.form.focused {
        Field::Band => {
            app.form.band_idx = if app.form.band_idx == 0 {
                BANDS.len().saturating_sub(1)
            } else {
                app.form.band_idx - 1
            };
            app.form.on_band_change();
        }
        Field::Mode => {
            app.form.mode_idx = if app.form.mode_idx == 0 {
                MODES.len().saturating_sub(1)
            } else {
                app.form.mode_idx - 1
            };
            app.form.on_mode_change();
        }
        _ => {}
    }
}

/// Cycle the focused selector one step to the right (increasing index).
fn cycle_right(app: &mut App) {
    match app.form.focused {
        Field::Band => {
            app.form.band_idx = (app.form.band_idx + 1) % BANDS.len();
            app.form.on_band_change();
        }
        Field::Mode => {
            app.form.mode_idx = (app.form.mode_idx + 1) % MODES.len();
            app.form.on_mode_change();
        }
        _ => {}
    }
}

/// Spawn a task to refresh the recent QSOs list and forward the result to the event channel.
fn refresh_recent_qsos(event_tx: &mpsc::UnboundedSender<AppEvent>, endpoint: &str) {
    let tx = event_tx.clone();
    let ep = endpoint.to_string();
    tokio::spawn(async move {
        if let Ok(ch) = grpc::create_channel(&ep).await {
            if let Ok(qsos) = grpc::list_recent_qsos(ch, 20).await {
                let _ = tx.send(AppEvent::RecentQsos(qsos));
            }
        }
    });
}

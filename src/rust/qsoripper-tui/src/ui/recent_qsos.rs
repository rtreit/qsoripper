//! Recent QSOs history panel.

use ratatui::{
    layout::{Constraint, Rect},
    style::{Color, Modifier, Style},
    widgets::{Block, Cell, Row, Table, TableState},
    Frame,
};

use crate::app::App;

/// Render the recent QSOs panel into `area`.
pub(super) fn render(app: &App, frame: &mut Frame, area: Rect) {
    let (border_color, title) = if app.qso_list_focused {
        (
            Color::Yellow,
            " Recent QSOs  \u{2191}\u{2193} navigate  Enter load  D delete  Esc exit ",
        )
    } else {
        (Color::Cyan, " Recent QSOs  (F3 to focus) ")
    };

    let block = Block::bordered()
        .title(title)
        .border_style(Style::default().fg(border_color));

    let header_cells = [
        "UTC",
        "Callsign",
        "Band",
        "Mode",
        "RST\u{2191}",
        "RST\u{2193}",
        "Country",
        "Grid",
    ]
    .iter()
    .map(|h| {
        Cell::from(*h).style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
    });

    let header = Row::new(header_cells).height(1);

    let rows = app.recent_qsos.iter().enumerate().map(|(i, qso)| {
        let row_style = if i % 2 == 0 {
            Style::default().fg(Color::White)
        } else {
            Style::default().fg(Color::Gray)
        };
        Row::new(vec![
            Cell::from(qso.utc.as_str()),
            Cell::from(qso.callsign.as_str()).style(
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Cell::from(qso.band.as_str()),
            Cell::from(qso.mode.as_str()),
            Cell::from(qso.rst_sent.as_str()),
            Cell::from(qso.rst_rcvd.as_str()),
            Cell::from(qso.country.as_deref().unwrap_or("")),
            Cell::from(qso.grid.as_deref().unwrap_or("")),
        ])
        .style(row_style)
        .height(1)
    });

    let widths = [
        Constraint::Length(5),
        Constraint::Length(10),
        Constraint::Length(5),
        Constraint::Length(6),
        Constraint::Length(5),
        Constraint::Length(5),
        Constraint::Fill(1),
        Constraint::Length(7),
    ];

    let highlight_style = if app.qso_list_focused {
        Style::default()
            .fg(Color::Black)
            .bg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().add_modifier(Modifier::BOLD)
    };

    let table = Table::new(rows, widths)
        .header(header)
        .block(block)
        .row_highlight_style(highlight_style);

    let mut state = TableState::default();
    state.select(if app.qso_list_focused {
        app.qso_selected
    } else {
        None
    });

    frame.render_stateful_widget(table, area, &mut state);
}

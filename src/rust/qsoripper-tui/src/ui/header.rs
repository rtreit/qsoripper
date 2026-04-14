//! Header bar showing the application name and UTC clock.

use ratatui::{
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::app::App;

/// Render the header bar into `area`.
pub(super) fn render(app: &App, frame: &mut Frame, area: Rect) {
    let halves = Layout::horizontal([Constraint::Fill(1), Constraint::Length(28)]).split(area);

    let title_area = halves.first().copied().unwrap_or(area);
    let clock_area = halves.get(1).copied().unwrap_or(area);

    let title_block = Block::default()
        .borders(Borders::TOP | Borders::LEFT | Borders::BOTTOM)
        .border_style(Style::default().fg(Color::Cyan));

    let title_text = Line::from(vec![
        Span::styled(
            " QsoRipper ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "— ham radio QSO logger",
            Style::default().fg(Color::DarkGray),
        ),
    ]);
    frame.render_widget(
        Paragraph::new(title_text)
            .block(title_block)
            .alignment(Alignment::Left),
        title_area,
    );

    let clock_block = Block::default()
        .borders(Borders::TOP | Borders::RIGHT | Borders::BOTTOM)
        .border_style(Style::default().fg(Color::Cyan));

    let utc_text = Line::from(vec![
        Span::styled("UTC ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("{} ", app.utc_now),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
    ]);
    frame.render_widget(
        Paragraph::new(utc_text)
            .block(clock_block)
            .alignment(Alignment::Right),
        clock_area,
    );
}

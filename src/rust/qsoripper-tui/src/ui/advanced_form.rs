//! Advanced QSO card editor rendering — compact, tabbed, keyboard-first layout.

use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph},
    Frame,
};

use crate::app::App;
use crate::form::{AdvancedTab, Field, LogForm};
use crate::ui::log_form::styled_field;

const LABEL_WIDTH: usize = 16;

struct FieldSpec {
    field: Field,
    key: char,
    label: &'static str,
}

/// Render the advanced QSO card editor into `area`.
pub(super) fn render(app: &App, frame: &mut Frame, area: Rect) {
    let title = if app.editing_local_id.is_some() {
        " QSO Card - Edit "
    } else {
        " QSO Card - New "
    };
    let block = Block::bordered()
        .title(title)
        .border_style(Style::default().fg(Color::Magenta));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.height < 6 {
        return;
    }

    let layout = Layout::vertical([
        Constraint::Length(2), // card summary
        Constraint::Length(1), // section tabs
        Constraint::Fill(1),   // selected section
        Constraint::Length(1), // action hints
    ])
    .split(inner);

    render_header(app, frame, layout.first().copied().unwrap_or(inner));
    render_tab_bar(
        frame,
        layout.get(1).copied().unwrap_or(inner),
        app.form.advanced_tab,
    );
    render_tab_content(
        frame,
        layout.get(2).copied().unwrap_or(inner),
        &app.form,
        app.form.advanced_tab,
    );
    render_footer(frame, layout.get(3).copied().unwrap_or(inner));
}

fn render_header(app: &App, frame: &mut Frame, area: Rect) {
    let call = if app.form.callsign.is_empty() {
        "new contact"
    } else {
        app.form.callsign.as_str()
    };
    let mode = if app.editing_local_id.is_some() {
        "Edit existing"
    } else {
        "New contact"
    };
    let subtitle = if app.editing_local_id.is_some() {
        "F10 updates, Esc/F2 returns to normal entry"
    } else {
        "Advanced entry: grouped fields, low-churn tab switching"
    };
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(vec![
                Span::styled(
                    call.to_string(),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw("  "),
                Span::styled(mode, Style::default().fg(Color::Black).bg(Color::DarkGray)),
                Span::raw("  "),
                Span::styled(
                    app.form.band_str().to_string(),
                    Style::default().fg(Color::Cyan),
                ),
                Span::raw(" / "),
                Span::styled(
                    app.form.mode_str().to_string(),
                    Style::default().fg(Color::Cyan),
                ),
            ]),
            Line::from(Span::styled(subtitle, Style::default().fg(Color::DarkGray))),
        ]),
        area,
    );
}

fn render_tab_bar(frame: &mut Frame, area: Rect, active: AdvancedTab) {
    let mut spans: Vec<Span<'static>> = Vec::new();
    for &tab in AdvancedTab::ALL {
        let digit = tab.shortcut_digit().to_string();
        let name = tab.label().to_string();
        let label_text = format!(" {name} ");
        if tab == active {
            let base = Style::default()
                .fg(Color::Black)
                .bg(Color::Magenta)
                .add_modifier(Modifier::BOLD);
            spans.push(Span::styled(" ", base));
            spans.push(Span::styled(digit, base.add_modifier(Modifier::UNDERLINED)));
            spans.push(Span::styled(label_text, base));
        } else {
            let base = Style::default().fg(Color::Magenta);
            spans.push(Span::styled(" ", base));
            spans.push(Span::styled(digit, base.add_modifier(Modifier::UNDERLINED)));
            spans.push(Span::styled(label_text, base));
        }
        spans.push(Span::raw(" "));
    }
    spans.push(Span::styled(
        "  Alt+1-7 / F5-F6",
        Style::default().fg(Color::DarkGray),
    ));
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_tab_content(frame: &mut Frame, area: Rect, form: &LogForm, tab: AdvancedTab) {
    match tab {
        AdvancedTab::Core => render_core_tab(frame, area, form),
        AdvancedTab::Lookup => render_lookup_tab(frame, area, form),
        AdvancedTab::Qsl => render_qsl_tab(frame, area, form),
        AdvancedTab::Contest => render_contest_tab(frame, area, form),
        AdvancedTab::Station => render_station_tab(frame, area, form),
        AdvancedTab::Transcript => render_transcript_tab(frame, area, form),
        AdvancedTab::Metadata => render_metadata_tab(frame, area, form),
    }
}

fn render_core_tab(frame: &mut Frame, area: Rect, form: &LogForm) {
    let cols = two_columns(area);
    render_group(
        frame,
        cols[0],
        " Contact ",
        form,
        &[
            FieldSpec {
                field: Field::Callsign,
                key: 'C',
                label: "Worked call",
            },
            FieldSpec {
                field: Field::Date,
                key: 'D',
                label: "Date",
            },
            FieldSpec {
                field: Field::Time,
                key: 'T',
                label: "Start",
            },
            FieldSpec {
                field: Field::TimeOff,
                key: 'E',
                label: "End",
            },
        ],
    );
    render_group(
        frame,
        cols[1],
        " Band / signal ",
        form,
        &[
            FieldSpec {
                field: Field::Band,
                key: 'B',
                label: "Band",
            },
            FieldSpec {
                field: Field::Mode,
                key: 'M',
                label: "Mode",
            },
            FieldSpec {
                field: Field::FrequencyMhz,
                key: 'F',
                label: "Freq MHz",
            },
            FieldSpec {
                field: Field::RstSent,
                key: 'S',
                label: "RST sent",
            },
            FieldSpec {
                field: Field::RstRcvd,
                key: 'R',
                label: "RST rcvd",
            },
            FieldSpec {
                field: Field::StationCallsign,
                key: 'A',
                label: "Station call",
            },
            FieldSpec {
                field: Field::TxPower,
                key: 'W',
                label: "TX power",
            },
            FieldSpec {
                field: Field::Submode,
                key: 'U',
                label: "Submode",
            },
            FieldSpec {
                field: Field::CwDecodeRxWpm,
                key: 'Y',
                label: "CW RX WPM",
            },
            FieldSpec {
                field: Field::Comment,
                key: 'O',
                label: "Comment",
            },
            FieldSpec {
                field: Field::Notes,
                key: 'N',
                label: "Notes",
            },
        ],
    );
}

fn render_lookup_tab(frame: &mut Frame, area: Rect, form: &LogForm) {
    let cols = two_columns(area);
    render_group(
        frame,
        cols[0],
        " Worked operator ",
        form,
        &[
            FieldSpec {
                field: Field::WorkedOperatorCallsign,
                key: 'C',
                label: "Operator call",
            },
            FieldSpec {
                field: Field::WorkedName,
                key: 'A',
                label: "Name",
            },
            FieldSpec {
                field: Field::WorkedGrid,
                key: 'L',
                label: "Grid",
            },
            FieldSpec {
                field: Field::WorkedCountry,
                key: 'C',
                label: "Country",
            },
            FieldSpec {
                field: Field::WorkedDxcc,
                key: 'D',
                label: "DXCC",
            },
            FieldSpec {
                field: Field::WorkedState,
                key: 'H',
                label: "State",
            },
            FieldSpec {
                field: Field::WorkedCqZone,
                key: 'Z',
                label: "CQ zone",
            },
        ],
    );
    render_group(
        frame,
        cols[1],
        " Lookup details ",
        form,
        &[
            FieldSpec {
                field: Field::WorkedItuZone,
                key: 'T',
                label: "ITU zone",
            },
            FieldSpec {
                field: Field::WorkedCounty,
                key: 'Y',
                label: "County",
            },
            FieldSpec {
                field: Field::Iota,
                key: 'I',
                label: "IOTA",
            },
            FieldSpec {
                field: Field::WorkedContinent,
                key: 'V',
                label: "Continent",
            },
            FieldSpec {
                field: Field::ArrlSection,
                key: 'X',
                label: "ARRL sec",
            },
            FieldSpec {
                field: Field::Skcc,
                key: 'K',
                label: "SKCC",
            },
        ],
    );
}

fn render_qsl_tab(frame: &mut Frame, area: Rect, form: &LogForm) {
    let cols = two_columns(area);
    render_group(
        frame,
        cols[0],
        " Paper QSL ",
        form,
        &[
            FieldSpec {
                field: Field::QslSentStatus,
                key: 'S',
                label: "Sent status",
            },
            FieldSpec {
                field: Field::QslSentDate,
                key: 'D',
                label: "Sent date",
            },
            FieldSpec {
                field: Field::QslReceivedStatus,
                key: 'R',
                label: "Rcvd status",
            },
            FieldSpec {
                field: Field::QslReceivedDate,
                key: 'E',
                label: "Rcvd date",
            },
            FieldSpec {
                field: Field::QrzLogId,
                key: 'L',
                label: "QRZ log ID",
            },
            FieldSpec {
                field: Field::QrzBookId,
                key: 'B',
                label: "QRZ book ID",
            },
        ],
    );
    render_group(
        frame,
        cols[1],
        " Electronic QSL ",
        form,
        &[
            FieldSpec {
                field: Field::LotwSent,
                key: 'T',
                label: "LoTW sent",
            },
            FieldSpec {
                field: Field::LotwReceived,
                key: 'W',
                label: "LoTW rcvd",
            },
            FieldSpec {
                field: Field::EqslSent,
                key: 'Q',
                label: "eQSL sent",
            },
            FieldSpec {
                field: Field::EqslReceived,
                key: 'V',
                label: "eQSL rcvd",
            },
        ],
    );
}

fn render_station_tab(frame: &mut Frame, area: Rect, form: &LogForm) {
    let cols = two_columns(area);
    render_group(
        frame,
        cols[0],
        " Local station ",
        form,
        &[
            FieldSpec {
                field: Field::SnapshotStationCallsign,
                key: 'S',
                label: "Station call",
            },
            FieldSpec {
                field: Field::SnapshotOperatorCallsign,
                key: 'C',
                label: "Operator call",
            },
            FieldSpec {
                field: Field::SnapshotOperatorName,
                key: 'N',
                label: "Operator name",
            },
            FieldSpec {
                field: Field::SnapshotGrid,
                key: 'G',
                label: "Grid",
            },
            FieldSpec {
                field: Field::SnapshotCountry,
                key: 'Y',
                label: "Country",
            },
            FieldSpec {
                field: Field::SnapshotState,
                key: 'T',
                label: "State",
            },
            FieldSpec {
                field: Field::SnapshotCounty,
                key: 'O',
                label: "County",
            },
        ],
    );
    render_group(
        frame,
        cols[1],
        " Station details ",
        form,
        &[
            FieldSpec {
                field: Field::SnapshotProfileName,
                key: 'P',
                label: "Profile",
            },
            FieldSpec {
                field: Field::SnapshotArrlSection,
                key: 'A',
                label: "ARRL section",
            },
            FieldSpec {
                field: Field::SnapshotDxcc,
                key: 'D',
                label: "DXCC",
            },
            FieldSpec {
                field: Field::SnapshotCqZone,
                key: 'Q',
                label: "CQ zone",
            },
            FieldSpec {
                field: Field::SnapshotItuZone,
                key: 'I',
                label: "ITU zone",
            },
            FieldSpec {
                field: Field::SnapshotLatitude,
                key: 'L',
                label: "Latitude",
            },
            FieldSpec {
                field: Field::SnapshotLongitude,
                key: 'V',
                label: "Longitude",
            },
        ],
    );
}

fn render_contest_tab(frame: &mut Frame, area: Rect, form: &LogForm) {
    let cols = two_columns(area);
    render_group(
        frame,
        cols[0],
        " Contest ",
        form,
        &[
            FieldSpec {
                field: Field::ContestId,
                key: 'G',
                label: "Contest ID",
            },
            FieldSpec {
                field: Field::SerialSent,
                key: 'J',
                label: "Serial sent",
            },
            FieldSpec {
                field: Field::SerialRcvd,
                key: 'Z',
                label: "Serial rcvd",
            },
        ],
    );
    render_group(
        frame,
        cols[1],
        " Exchange ",
        form,
        &[
            FieldSpec {
                field: Field::ExchangeSent,
                key: 'O',
                label: "Exch sent",
            },
            FieldSpec {
                field: Field::ExchangeRcvd,
                key: 'N',
                label: "Exch rcvd",
            },
            FieldSpec {
                field: Field::PropMode,
                key: 'P',
                label: "Prop mode",
            },
            FieldSpec {
                field: Field::SatName,
                key: 'L',
                label: "Satellite",
            },
            FieldSpec {
                field: Field::SatMode,
                key: 'V',
                label: "Sat mode",
            },
        ],
    );
}

fn render_transcript_tab(frame: &mut Frame, area: Rect, form: &LogForm) {
    let rows = Layout::vertical([Constraint::Length(4), Constraint::Fill(1)]).split(area);
    render_group(
        frame,
        rows.first().copied().unwrap_or(area),
        " CW summary ",
        form,
        &[FieldSpec {
            field: Field::CwDecodeRxWpm,
            key: 'W',
            label: "RX WPM",
        }],
    );
    render_group(
        frame,
        rows.get(1).copied().unwrap_or(area),
        " CW transcript ",
        form,
        &[FieldSpec {
            field: Field::CwDecodeTranscript,
            key: 'T',
            label: "Transcript",
        }],
    );
}

fn render_metadata_tab(frame: &mut Frame, area: Rect, form: &LogForm) {
    let cols = two_columns(area);
    render_group(
        frame,
        cols[0],
        " Engine metadata ",
        form,
        &[
            FieldSpec {
                field: Field::LocalId,
                key: 'L',
                label: "Local ID",
            },
            FieldSpec {
                field: Field::SyncStatus,
                key: 'S',
                label: "Sync status",
            },
            FieldSpec {
                field: Field::CreatedAt,
                key: 'C',
                label: "Created",
            },
            FieldSpec {
                field: Field::UpdatedAt,
                key: 'U',
                label: "Updated",
            },
        ],
    );
    render_group(
        frame,
        cols[1],
        " Extra ADIF fields ",
        form,
        &[FieldSpec {
            field: Field::ExtraFields,
            key: 'E',
            label: "KEY=value",
        }],
    );
}

fn render_group(
    frame: &mut Frame,
    area: Rect,
    title: &'static str,
    form: &LogForm,
    fields: &[FieldSpec],
) {
    let block = Block::bordered()
        .title(title)
        .border_style(Style::default().fg(Color::DarkGray));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height == 0 {
        return;
    }
    if fields.is_empty() {
        return;
    }

    let constraints: Vec<Constraint> = fields.iter().map(|_| Constraint::Length(1)).collect();
    let rows = Layout::vertical(constraints).split(inner);
    for (idx, spec) in fields.iter().enumerate() {
        if let Some(row) = rows.get(idx).copied() {
            render_field(frame, row, form, spec);
        }
    }
}

fn render_field(frame: &mut Frame, area: Rect, form: &LogForm, spec: &FieldSpec) {
    let focused = form.focused == spec.field;
    let selected = focused && form.field_selected;
    let value_width = (area.width as usize).saturating_sub(LABEL_WIDTH + 2).max(5);
    let value = if matches!(spec.field, Field::Band | Field::Mode) {
        cycle_value(field_text(form, spec.field), focused)
    } else {
        adv_field(field_text(form, spec.field), focused, selected, value_width)
    };

    let mut spans = Vec::new();
    spans.extend(shortcut_label(spec.key, spec.label));
    spans.push(Span::raw(" "));
    if matches!(spec.field, Field::Band | Field::Mode) {
        spans.push(styled_cycle(value, focused));
    } else {
        spans.push(styled_field(value, focused, selected));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn field_text(form: &LogForm, field: Field) -> &str {
    match field {
        Field::Callsign => &form.callsign,
        Field::Band => form.band_str(),
        Field::Mode => form.mode_str(),
        Field::RstSent => &form.rst_sent,
        Field::RstRcvd => &form.rst_rcvd,
        Field::Comment => &form.comment,
        Field::Notes => &form.notes,
        Field::FrequencyMhz => &form.frequency_mhz,
        Field::Date => &form.date,
        Field::Time => &form.time,
        Field::TimeOff => &form.time_off,
        Field::Qth => &form.qth,
        Field::StationCallsign => &form.station_callsign,
        Field::TxPower => &form.tx_power,
        Field::Submode => &form.submode_override,
        Field::ContestId => &form.contest_id,
        Field::SerialSent => &form.serial_sent,
        Field::SerialRcvd => &form.serial_rcvd,
        Field::ExchangeSent => &form.exchange_sent,
        Field::ExchangeRcvd => &form.exchange_rcvd,
        Field::PropMode => &form.prop_mode,
        Field::SatName => &form.sat_name,
        Field::SatMode => &form.sat_mode,
        Field::Iota => &form.iota,
        Field::ArrlSection => &form.arrl_section,
        Field::WorkedState => &form.worked_state,
        Field::WorkedCounty => &form.worked_county,
        Field::WorkedOperatorCallsign => &form.worked_operator_callsign,
        Field::WorkedName => &form.worked_name,
        Field::WorkedGrid => &form.worked_grid,
        Field::WorkedCountry => &form.worked_country,
        Field::WorkedDxcc => &form.worked_dxcc,
        Field::WorkedCqZone => &form.worked_cq_zone,
        Field::WorkedItuZone => &form.worked_itu_zone,
        Field::WorkedContinent => &form.worked_continent,
        Field::Skcc => &form.skcc,
        Field::QslSentStatus => &form.qsl_sent_status,
        Field::QslSentDate => &form.qsl_sent_date,
        Field::QslReceivedStatus => &form.qsl_received_status,
        Field::QslReceivedDate => &form.qsl_received_date,
        Field::LotwSent => &form.lotw_sent,
        Field::LotwReceived => &form.lotw_received,
        Field::EqslSent => &form.eqsl_sent,
        Field::EqslReceived => &form.eqsl_received,
        Field::QrzLogId => &form.qrz_log_id,
        Field::QrzBookId => &form.qrz_book_id,
        Field::SnapshotProfileName => &form.snapshot_profile_name,
        Field::SnapshotStationCallsign => &form.snapshot_station_callsign,
        Field::SnapshotOperatorCallsign => &form.snapshot_operator_callsign,
        Field::SnapshotOperatorName => &form.snapshot_operator_name,
        Field::SnapshotGrid => &form.snapshot_grid,
        Field::SnapshotCountry => &form.snapshot_country,
        Field::SnapshotState => &form.snapshot_state,
        Field::SnapshotCounty => &form.snapshot_county,
        Field::SnapshotArrlSection => &form.snapshot_arrl_section,
        Field::SnapshotDxcc => &form.snapshot_dxcc,
        Field::SnapshotCqZone => &form.snapshot_cq_zone,
        Field::SnapshotItuZone => &form.snapshot_itu_zone,
        Field::SnapshotLatitude => &form.snapshot_latitude,
        Field::SnapshotLongitude => &form.snapshot_longitude,
        Field::CwDecodeRxWpm => &form.cw_decode_rx_wpm,
        Field::CwDecodeTranscript => &form.cw_decode_transcript,
        Field::LocalId => &form.local_id,
        Field::SyncStatus => &form.sync_status,
        Field::CreatedAt => &form.created_at,
        Field::UpdatedAt => &form.updated_at,
        Field::ExtraFields => &form.extra_fields,
    }
}

fn two_columns(area: Rect) -> [Rect; 2] {
    let cols =
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).split(area);
    [
        cols.first().copied().unwrap_or(area),
        cols.get(1).copied().unwrap_or(area),
    ]
}

/// Format an advanced field value with a fixed display width and optional cursor.
fn adv_field(text: &str, focused: bool, selected: bool, width: usize) -> String {
    if selected {
        let len = text.chars().count();
        if len >= width {
            text.chars().take(width).collect()
        } else {
            format!("{text:<width$}")
        }
    } else {
        let mut s = text.to_string();
        if focused {
            s.push('|');
        }
        let len = s.chars().count();
        if len > width {
            s.chars().skip(len - width).collect()
        } else {
            format!("{s:<width$}")
        }
    }
}

fn shortcut_label(key: char, label: &'static str) -> [Span<'static>; 3] {
    let label_style = Style::default().fg(Color::Cyan);
    [
        Span::styled(
            key.to_ascii_uppercase().to_string(),
            label_style.add_modifier(Modifier::UNDERLINED),
        ),
        Span::styled(" ", label_style),
        Span::styled(
            format!("{label:<width$}", width = LABEL_WIDTH - 2),
            label_style,
        ),
    ]
}

fn cycle_value(text: &str, focused: bool) -> String {
    if focused {
        format!("< {text} >")
    } else {
        format!("  {text}  ")
    }
}

fn styled_cycle(value: String, focused: bool) -> Span<'static> {
    Span::styled(
        value,
        Style::default()
            .fg(if focused { Color::Yellow } else { Color::Gray })
            .add_modifier(if focused {
                Modifier::BOLD
            } else {
                Modifier::empty()
            }),
    )
}

fn render_footer(frame: &mut Frame, area: Rect) {
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                " F2/Esc Return ",
                Style::default().fg(Color::Black).bg(Color::DarkGray),
            ),
            Span::raw("  "),
            Span::styled(
                " Tab/Shift+Tab Field ",
                Style::default().fg(Color::Black).bg(Color::DarkGray),
            ),
            Span::raw("  "),
            Span::styled(
                " Alt+key Jump ",
                Style::default().fg(Color::Black).bg(Color::DarkGray),
            ),
            Span::raw("  "),
            Span::styled(
                " F10 Log/Save ",
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        ])),
        area,
    );
}

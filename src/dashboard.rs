use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::Frame;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Gauge, Paragraph, Row, Table, Wrap};

use crate::server::{Diagnostics, diagnostics_snapshot};

const BG: Color = Color::Rgb(16, 16, 16);
const FG: Color = Color::Rgb(255, 255, 255);
const MUTED: Color = Color::Rgb(160, 160, 160);
const MINT: Color = Color::Rgb(153, 255, 228);
const PEACH: Color = Color::Rgb(255, 199, 153);
const VIOLET: Color = Color::Rgb(185, 153, 255);
const RED: Color = Color::Rgb(255, 128, 128);

pub fn run(project: &Path) -> Result<()> {
    let mut terminal = ratatui::init();
    let result = terminal
        .clear()
        .map_err(Into::into)
        .and_then(|_| run_loop(&mut terminal, project));
    ratatui::restore();
    result
}

fn run_loop(terminal: &mut ratatui::DefaultTerminal, project: &Path) -> Result<()> {
    let mut snapshot = diagnostics_snapshot(project)?;
    let mut refreshed = Instant::now();
    loop {
        terminal.draw(|frame| render(frame, &snapshot, refreshed.elapsed()))?;
        if event::poll(Duration::from_secs(1))?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            if matches!(key.code, KeyCode::Esc | KeyCode::Char('q'))
                || (key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL))
            {
                return Ok(());
            }
            if key.code == KeyCode::Char('r') {
                snapshot = diagnostics_snapshot(project)?;
                refreshed = Instant::now();
            }
        }
        if refreshed.elapsed() >= Duration::from_secs(1) {
            snapshot = diagnostics_snapshot(project)?;
            refreshed = Instant::now();
        }
    }
}

fn render(frame: &mut Frame, snapshot: &Diagnostics, age: Duration) {
    frame.render_widget(
        Block::new().style(Style::default().bg(BG).fg(FG)),
        frame.area(),
    );
    let area = frame.area();
    if area.width < 76 || area.height < 24 {
        frame.render_widget(
            Paragraph::new("Clarra dashboard needs a 76 x 24 terminal")
                .alignment(Alignment::Center)
                .style(Style::default().fg(PEACH).bg(BG))
                .block(
                    panel()
                        .title(" RESIZE ")
                        .title_style(Style::default().fg(MINT)),
                ),
            area,
        );
        return;
    }
    let rows = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(if area.width >= 88 { 5 } else { 8 }),
        Constraint::Min(9),
        Constraint::Length(3),
        Constraint::Length(1),
    ])
    .split(area);

    header(frame, rows[0], snapshot);
    metrics(frame, rows[1], snapshot);
    detail(frame, rows[2], snapshot);
    cache(frame, rows[3], snapshot);
    footer(frame, rows[4], age);
}

fn header(frame: &mut Frame, area: Rect, snapshot: &Diagnostics) {
    let title = Line::from(vec![
        Span::styled(" CLARRA ", Style::default().bg(MINT).fg(BG).bold()),
        Span::styled("  PROJECT PULSE", Style::default().fg(FG).bold()),
    ]);
    let id = snapshot.project_id.chars().take(8).collect::<String>();
    frame.render_widget(
        Paragraph::new(vec![
            title,
            Line::from(vec![
                Span::styled(format!(" ledger {id}"), Style::default().fg(MUTED)),
                Span::styled("  live refresh 1s", Style::default().fg(MINT)),
            ]),
        ])
        .block(
            Block::new()
                .borders(Borders::BOTTOM)
                .border_style(Style::default().fg(VIOLET)),
        ),
        area,
    );
}

fn metrics(frame: &mut Frame, area: Rect, snapshot: &Diagnostics) {
    let items = [
        ("PEOPLE", snapshot.participants.len().to_string(), MINT),
        ("FILES", snapshot.files.to_string(), PEACH),
        ("SYNC EVENTS", snapshot.sequence.to_string(), VIOLET),
        (
            "TRANSFERRED",
            bytes(
                snapshot
                    .uploaded_bytes
                    .saturating_add(snapshot.downloaded_bytes),
            ),
            RED,
        ),
    ];
    let columns = if area.width >= 88 { 4 } else { 2 };
    let row_areas = Layout::vertical(vec![
        Constraint::Ratio(1, (4 / columns) as u32);
        4 / columns
    ])
    .split(area);
    for (row, chunk) in items.chunks(columns).zip(row_areas.iter()) {
        let cells =
            Layout::horizontal(vec![Constraint::Ratio(1, columns as u32); columns]).split(*chunk);
        for ((label, value, color), cell) in row.iter().zip(cells.iter()) {
            frame.render_widget(
                Paragraph::new(vec![
                    Line::styled(*label, Style::default().fg(MUTED)),
                    Line::styled(
                        value,
                        Style::default().fg(*color).add_modifier(Modifier::BOLD),
                    ),
                ])
                .alignment(Alignment::Center)
                .block(panel()),
                *cell,
            );
        }
    }
}

fn detail(frame: &mut Frame, area: Rect, snapshot: &Diagnostics) {
    let direction = if area.width >= 76 {
        Direction::Horizontal
    } else {
        Direction::Vertical
    };
    let panes = Layout::default()
        .direction(direction)
        .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
        .split(area);
    let participant_rows = if snapshot.participants.is_empty() {
        vec![Row::new(["No participants yet", "-", "-"])]
    } else {
        snapshot
            .participants
            .iter()
            .map(|person| {
                Row::new([
                    person.name.clone(),
                    person.devices.to_string(),
                    "not tracked".into(),
                ])
            })
            .collect()
    };
    frame.render_widget(
        Table::new(
            participant_rows,
            [
                Constraint::Min(16),
                Constraint::Length(9),
                Constraint::Length(13),
            ],
        )
        .header(
            Row::new(["PARTICIPANT", "DEVICES", "ONLINE"]).style(Style::default().fg(MINT).bold()),
        )
        .column_spacing(2)
        .block(
            panel()
                .title(" PEOPLE ")
                .title_style(Style::default().fg(PEACH).bold()),
        ),
        panes[0],
    );

    let health_color = if snapshot.conflicts > 0 { RED } else { MINT };
    let health = vec![
        Line::from(vec![
            Span::styled("conflicts       ", Style::default().fg(MUTED)),
            Span::styled(
                snapshot.conflicts.to_string(),
                Style::default().fg(health_color).bold(),
            ),
        ]),
        Line::from(vec![
            Span::styled("pending         ", Style::default().fg(MUTED)),
            Span::styled(
                snapshot.pending_deliveries.to_string(),
                Style::default().fg(PEACH).bold(),
            ),
        ]),
        Line::from(vec![
            Span::styled("revisions       ", Style::default().fg(MUTED)),
            Span::styled(
                snapshot.revisions.to_string(),
                Style::default().fg(VIOLET).bold(),
            ),
        ]),
        Line::from(vec![
            Span::styled("storage         ", Style::default().fg(MUTED)),
            Span::styled(
                bytes(snapshot.storage_bytes),
                Style::default().fg(FG).bold(),
            ),
        ]),
        Line::from(vec![
            Span::styled("uploaded        ", Style::default().fg(MUTED)),
            Span::styled(bytes(snapshot.uploaded_bytes), Style::default().fg(MINT)),
        ]),
        Line::from(vec![
            Span::styled("downloaded      ", Style::default().fg(MUTED)),
            Span::styled(bytes(snapshot.downloaded_bytes), Style::default().fg(PEACH)),
        ]),
    ];
    frame.render_widget(
        Paragraph::new(health).wrap(Wrap { trim: true }).block(
            panel()
                .title(" LEDGER ")
                .title_style(Style::default().fg(VIOLET).bold()),
        ),
        panes[1],
    );
}

fn cache(frame: &mut Frame, area: Rect, snapshot: &Diagnostics) {
    let ratio = snapshot.cache_bytes as f64 / snapshot.cache_limit.max(1) as f64;
    frame.render_widget(
        Gauge::default()
            .block(
                panel()
                    .title(" CACHE ")
                    .title_style(Style::default().fg(MINT).bold()),
            )
            .gauge_style(Style::default().fg(MINT).bg(Color::Rgb(38, 38, 38)))
            .ratio(ratio.clamp(0.0, 1.0))
            .label(format!(
                "{} / {}",
                bytes(snapshot.cache_bytes),
                bytes(snapshot.cache_limit)
            )),
        area,
    );
}

fn footer(frame: &mut Frame, area: Rect, age: Duration) {
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" q/esc ", Style::default().bg(PEACH).fg(BG).bold()),
            Span::styled(" quit   ", Style::default().fg(MUTED)),
            Span::styled(" r ", Style::default().bg(VIOLET).fg(BG).bold()),
            Span::styled(" refresh", Style::default().fg(MUTED)),
            Span::styled(
                format!("   updated {}ms ago", age.as_millis()),
                Style::default().fg(MUTED),
            ),
        ])),
        area,
    );
}

fn panel<'a>() -> Block<'a> {
    Block::new()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Rgb(60, 60, 60)))
        .style(Style::default().bg(BG).fg(FG))
}

fn bytes(value: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut size = value as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{value} B")
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use ratatui::{Terminal, backend::TestBackend};

    use super::*;
    use crate::server::ParticipantDiagnostics;

    #[test]
    fn dashboard_renders_core_metrics_and_participants() {
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let snapshot = Diagnostics {
            project_id: "12345678-project".into(),
            sequence: 42,
            participants: vec![ParticipantDiagnostics {
                id: "501".into(),
                name: "alice".into(),
                devices: 2,
            }],
            devices: 2,
            files: 7,
            revisions: 9,
            conflicts: 1,
            pending_deliveries: 3,
            cache_bytes: 1024,
            cache_limit: 10 * 1024 * 1024 * 1024,
            storage_bytes: 2048,
            uploaded_bytes: 4096,
            downloaded_bytes: 8192,
        };
        terminal
            .draw(|frame| render(frame, &snapshot, Duration::ZERO))
            .unwrap();
        let screen = terminal.backend().to_string();
        for expected in [
            "PROJECT PULSE",
            "SYNC EVENTS",
            "alice",
            "12.0 KiB",
            "not tracked",
        ] {
            assert!(
                screen.contains(expected),
                "missing {expected:?} in dashboard"
            );
        }
    }

    #[test]
    fn formats_bytes_compactly() {
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(1536), "1.5 KiB");
    }

    #[test]
    fn small_terminals_get_a_resize_message() {
        let backend = TestBackend::new(60, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        let snapshot = Diagnostics {
            project_id: String::new(),
            sequence: 0,
            participants: Vec::new(),
            devices: 0,
            files: 0,
            revisions: 0,
            conflicts: 0,
            pending_deliveries: 0,
            cache_bytes: 0,
            cache_limit: 1,
            storage_bytes: 0,
            uploaded_bytes: 0,
            downloaded_bytes: 0,
        };
        terminal
            .draw(|frame| render(frame, &snapshot, Duration::ZERO))
            .unwrap();
        assert!(terminal.backend().to_string().contains("76 x 24"));
    }
}

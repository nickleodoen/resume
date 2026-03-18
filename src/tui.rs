// Live terminal UI for resume sessions.
//
// Visual style mirrors Claude Code:
//   - Large pixel-art title rendered with colored background cells (2-space pixels)
//   - Small pixel mascot beside the info line
//   - No box borders — clean output with a single separator line
//   - RGB brand color: Color::Rgb(200, 120, 255) "Electric Orchid"
//     (same role as Claude Code's salmon/coral, just in purple)

use anyhow::{Context, Result};
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event as CEvent, KeyCode, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{List, ListItem, Paragraph},
    Terminal,
};
use std::io;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

use crate::session::{self, EventType, SessionEvent};
use crate::watcher;

// ── Brand palette ─────────────────────────────────────────────────────────────
const BRAND: Color = Color::Rgb(200, 120, 255); // Electric Orchid — vivid light purple
const BRAND_DIM: Color = Color::Rgb(110, 60, 160); // dim purple for separators
const GRAY: Color = Color::Rgb(160, 160, 160); // secondary text

// ── Pixel font ────────────────────────────────────────────────────────────────
// Each glyph: [row0..row6][col0..col4] — true = colored pixel (2 terminal cols wide)
// RESUME uses 6 glyphs × 5 pixels × 2 cols + 5 gaps × 2 cols = 70 terminal cols.
type Glyph = [[bool; 5]; 7];

#[rustfmt::skip]
const G_R: Glyph = [
    [true,  true,  true,  true,  false],
    [true,  false, false, false, true ],
    [true,  false, false, false, true ],
    [true,  true,  true,  true,  false],
    [true,  false, true,  false, false],
    [true,  false, false, true,  false],
    [true,  false, false, false, true ],
];

#[rustfmt::skip]
const G_E: Glyph = [
    [true,  true,  true,  true,  true ],
    [true,  false, false, false, false],
    [true,  false, false, false, false],
    [true,  true,  true,  true,  false],
    [true,  false, false, false, false],
    [true,  false, false, false, false],
    [true,  true,  true,  true,  true ],
];

#[rustfmt::skip]
const G_S: Glyph = [
    [false, true,  true,  true,  true ],
    [true,  false, false, false, false],
    [true,  false, false, false, false],
    [false, true,  true,  true,  false],
    [false, false, false, false, true ],
    [false, false, false, false, true ],
    [true,  true,  true,  true,  false],
];

#[rustfmt::skip]
const G_U: Glyph = [
    [true,  false, false, false, true ],
    [true,  false, false, false, true ],
    [true,  false, false, false, true ],
    [true,  false, false, false, true ],
    [true,  false, false, false, true ],
    [true,  false, false, false, true ],
    [false, true,  true,  true,  false],
];

#[rustfmt::skip]
const G_M: Glyph = [
    [true,  false, false, false, true ],
    [true,  true,  false, true,  true ],
    [true,  false, true,  false, true ],
    [true,  false, false, false, true ],
    [true,  false, false, false, true ],
    [true,  false, false, false, true ],
    [true,  false, false, false, true ],
];

const RESUME_GLYPHS: &[&Glyph] = &[&G_R, &G_E, &G_S, &G_U, &G_M, &G_E];

// ── Resy — pixel stingray mascot ─────────────────────────────────────────────
// Top-down view: 6 pixels wide × 4 pixels tall, 2 terminal cols per pixel = 12 cols.
// Slightly asymmetric (wider left wing pixel on row 1) = that unhinged energy.
#[rustfmt::skip]
const RESY: [[bool; 6]; 4] = [
    [false, false, true,  true,  false, false], // head/dorsal
    [true,  true,  true,  true,  true,  false], // wider left wing (unhinged asymmetry)
    [false, true,  true,  true,  true,  false], // lower body
    [false, false, true,  true,  false, false], // tail
];

// Render one row of the RESUME pixel-art banner.
fn resume_row(row: usize, pixel_style: Style) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = vec![Span::raw("  ")]; // left margin
    for (gi, glyph) in RESUME_GLYPHS.iter().enumerate() {
        for col in 0..5 {
            if glyph[row][col] {
                spans.push(Span::styled("  ", pixel_style));
            } else {
                spans.push(Span::raw("  "));
            }
        }
        if gi < RESUME_GLYPHS.len() - 1 {
            spans.push(Span::raw("  ")); // inter-letter gap
        }
    }
    Line::from(spans)
}

// Build the 4 info rows: each row is [Resy pixel row] + [info text].
fn info_lines(project: String, elapsed: String) -> Vec<Line<'static>> {
    let pixel_on = Style::default().bg(BRAND);

    let texts: [Vec<Span<'static>>; 4] = [
        // Row 0: "resume  v0.1"
        vec![
            Span::styled("resume", Style::default().fg(BRAND).add_modifier(Modifier::BOLD)),
            Span::raw("  "),
            Span::styled("v0.1", Style::default().fg(GRAY)),
        ],
        // Row 1: project name
        vec![Span::styled(
            format!("{project}"),
            Style::default().fg(GRAY),
        )],
        // Row 2: status + elapsed
        vec![Span::styled(
            format!("watching · {elapsed}"),
            Style::default().fg(GRAY),
        )],
        // Row 3: empty (tail row)
        vec![],
    ];

    texts
        .into_iter()
        .enumerate()
        .map(|(i, text_spans)| {
            let mut spans: Vec<Span<'static>> = vec![Span::raw("  ")]; // left margin

            // Resy pixel row (6 pixels × 2 cols = 12 terminal cols)
            for col in 0..6 {
                if RESY[i][col] {
                    spans.push(Span::styled("  ", pixel_on));
                } else {
                    spans.push(Span::raw("  "));
                }
            }

            spans.push(Span::raw("  ")); // gap between mascot and text
            spans.extend(text_spans);
            Line::from(spans)
        })
        .collect()
}

struct App {
    events: Vec<SessionEvent>,
    project: String,
    started: Instant,
}

impl App {
    fn new(project: String) -> Self {
        Self { events: Vec::new(), project, started: Instant::now() }
    }

    fn push(&mut self, ev: SessionEvent) {
        self.events.push(ev);
    }

    fn elapsed(&self) -> String {
        let secs = self.started.elapsed().as_secs();
        let h = secs / 3600;
        let m = (secs % 3600) / 60;
        let s = secs % 60;
        if h > 0 { format!("{h:02}:{m:02}:{s:02}") } else { format!("{m:02}:{s:02}") }
    }
}

pub async fn run() -> Result<()> {
    session::init()?;

    let project = std::env::current_dir()
        .unwrap_or_default()
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "unknown".to_string());

    let (tx, mut rx) = mpsc::unbounded_channel::<SessionEvent>();
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

    tokio::spawn(async move {
        if let Err(e) = watcher::watch(Some(tx), Some(shutdown_rx)).await {
            eprintln!("watcher error: {e}");
        }
    });

    enable_raw_mode().context("failed to enable raw mode")?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)
        .context("failed to enter alternate screen")?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).context("failed to create terminal")?;

    let mut app = App::new(project);
    let result = run_loop(&mut terminal, &mut app, &mut rx, shutdown_tx).await;

    disable_raw_mode().ok();
    execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture).ok();
    terminal.show_cursor().ok();

    result?;
    println!(
        "Session ended  ·  {} event(s) recorded  ·  run `resume show` for a briefing.",
        session::load().map(|s| s.events.len()).unwrap_or(0)
    );
    Ok(())
}

async fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    rx: &mut mpsc::UnboundedReceiver<SessionEvent>,
    shutdown_tx: tokio::sync::oneshot::Sender<()>,
) -> Result<()> {
    let tick = Duration::from_millis(250);
    loop {
        terminal.draw(|f| render(f, app))?;

        tokio::select! {
            maybe_ev = rx.recv() => {
                match maybe_ev {
                    Some(ev) => app.push(ev),
                    None => break,
                }
            }
            _ = tokio::time::sleep(tick) => {
                while event::poll(Duration::ZERO).unwrap_or(false) {
                    if let Ok(CEvent::Key(key)) = event::read() {
                        if key.code == KeyCode::Char('c')
                            && key.modifiers.contains(KeyModifiers::CONTROL)
                        {
                            let _ = shutdown_tx.send(());
                            return Ok(());
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

fn render(f: &mut ratatui::Frame, app: &App) {
    let area = f.area();

    // Layout (no box borders — clean like Claude Code):
    //   banner : 7 pixel art rows + 1 blank + 4 info rows = 12
    //   sep    : 1 (horizontal rule)
    //   events : fill
    //   footer : 1
    let chunks = Layout::vertical([
        Constraint::Length(12),
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .split(area);

    // ── Banner ────────────────────────────────────────────────────────────────
    let pixel_on = Style::default().bg(BRAND);
    let mut banner: Vec<Line> = (0..7).map(|r| resume_row(r, pixel_on)).collect();
    banner.push(Line::raw("")); // blank row between art and info
    banner.extend(info_lines(app.project.clone(), app.elapsed()));

    f.render_widget(Paragraph::new(banner), chunks[0]);

    // ── Separator ─────────────────────────────────────────────────────────────
    let sep = "─".repeat(area.width as usize);
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(sep, Style::default().fg(BRAND_DIM)))),
        chunks[1],
    );

    // ── Event list ────────────────────────────────────────────────────────────
    let items: Vec<ListItem> = app
        .events
        .iter()
        .rev()
        .map(|ev| {
            let (tag, tag_color) = match ev.event_type {
                EventType::FileChange => ("FILE", Color::Cyan),
                EventType::GitDiff => ("GIT ", Color::Yellow),
                EventType::Command => ("CMD ", Color::Green),
            };
            let time_str = ev.timestamp.format("%H:%M:%S").to_string();
            let content = ev.content.lines().next().unwrap_or("").to_string();
            let display = if content.len() > 42 {
                format!("{}~", &content[..41])
            } else {
                content
            };
            ListItem::new(Line::from(vec![
                Span::raw("  "),
                Span::styled(tag, Style::default().fg(tag_color).add_modifier(Modifier::BOLD)),
                Span::raw("  "),
                Span::styled(display, Style::default().fg(Color::White)),
                Span::styled(
                    format!("  {time_str}"),
                    Style::default().fg(GRAY),
                ),
            ]))
        })
        .collect();

    f.render_widget(List::new(items), chunks[2]);

    // ── Footer ────────────────────────────────────────────────────────────────
    let count = app.events.len();
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw("  "),
            Span::styled(
                format!("{count} event{}", if count == 1 { "" } else { "s" }),
                Style::default().fg(BRAND),
            ),
            Span::styled("  ·  ", Style::default().fg(BRAND_DIM)),
            Span::styled("Ctrl-C", Style::default().fg(BRAND).add_modifier(Modifier::BOLD)),
            Span::styled(" or ", Style::default().fg(GRAY)),
            Span::styled("finish", Style::default().fg(BRAND).add_modifier(Modifier::BOLD)),
            Span::styled(" to stop", Style::default().fg(GRAY)),
        ])),
        chunks[3],
    );
}

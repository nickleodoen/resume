// Live terminal UI for resume sessions.
//
// Visual style mirrors Claude Code:
//   - Bordered welcome box with left (mascot + info) and right (tips) panels
//   - Resy pixel mascot rendered with colored background cells (2-space pixels)
//   - Command input area at the bottom with "> " prompt
//   - RGB brand color: Color::Rgb(200, 120, 255) "Electric Orchid"

use anyhow::{Context, Result};
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event as CEvent, KeyCode, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Terminal,
};
use std::io;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

use crate::session::{self, SessionEvent};
use crate::watcher;

// ── Brand palette ─────────────────────────────────────────────────────────────
const BRAND: Color = Color::Rgb(200, 120, 255); // Electric Orchid
const BRAND_DIM: Color = Color::Rgb(110, 60, 160); // dim purple
const GRAY: Color = Color::Rgb(160, 160, 160);
const DARK_PX: Color = Color::Rgb(20, 20, 40); // eye
const PURPLE_MAIN: Color = Color::Rgb(180, 100, 255);
const PURPLE_LIGHT: Color = Color::Rgb(210, 150, 255);
const PURPLE_DARK: Color = Color::Rgb(100, 50, 160);

// ── Resy pixel art ─────────────────────────────────────────────────────────────
// 9 cols × 7 rows — purple jellyfish mascot
// 0=transparent  1=PURPLE_MAIN (body)  2=PURPLE_LIGHT (highlight)
//                3=DARK_PX (eyes)      4=PURPLE_DARK (fringe/tentacles)
// Each pixel = 2 terminal columns wide (≈ square pixels).
const RESY_W: usize = 9;
const RESY_H: usize = 7;

#[rustfmt::skip]
const RESY: [[u8; RESY_W]; RESY_H] = [
    [0, 0, 1, 1, 1, 1, 1, 0, 0], // dome arc top       (5 wide)
    [0, 1, 2, 1, 1, 1, 1, 1, 0], // dome + highlight   (7 wide)
    [1, 1, 1, 3, 1, 3, 1, 1, 1], // full dome + 2 eyes (symmetric)
    [1, 1, 1, 1, 1, 1, 1, 1, 1], // dome base flat     (9 wide)
    [0, 4, 1, 4, 1, 4, 1, 4, 0], // fringe/ruffle row  (jellyfish!)
    [0, 0, 4, 0, 4, 0, 4, 0, 0], // three tentacles
    [0, 0, 4, 0, 0, 0, 4, 0, 0], // outer tentacles taper
];

fn resy_lines() -> Vec<Line<'static>> {
    (0..RESY_H)
        .map(|row| {
            let spans: Vec<Span<'static>> = RESY[row]
                .iter()
                .map(|&px| match px {
                    1 => Span::styled("  ", Style::default().bg(PURPLE_MAIN)),
                    2 => Span::styled("  ", Style::default().bg(PURPLE_LIGHT)),
                    3 => Span::styled("  ", Style::default().bg(DARK_PX)),
                    4 => Span::styled("  ", Style::default().bg(PURPLE_DARK)),
                    _ => Span::raw("  "),
                })
                .collect();
            Line::from(spans)
        })
        .collect()
}

// ── App state ─────────────────────────────────────────────────────────────────
struct App {
    events: Vec<SessionEvent>,
    project: String,
    started: Instant,
    input: String,
    show_requested: bool,
    message: Option<String>,
}

impl App {
    fn new(project: String) -> Self {
        Self {
            events: Vec::new(),
            project,
            started: Instant::now(),
            input: String::new(),
            show_requested: false,
            message: None,
        }
    }

    fn push(&mut self, ev: SessionEvent) {
        self.events.push(ev);
    }

    fn elapsed(&self) -> String {
        let secs = self.started.elapsed().as_secs();
        let h = secs / 3600;
        let m = (secs % 3600) / 60;
        let s = secs % 60;
        if h > 0 {
            format!("{h:02}:{m:02}:{s:02}")
        } else {
            format!("{m:02}:{s:02}")
        }
    }

    /// Process the current input buffer. Returns true if the TUI should exit.
    fn handle_command(&mut self) -> bool {
        let cmd = self.input.trim().to_lowercase();
        self.input.clear();
        match cmd.as_str() {
            "show" => {
                self.show_requested = true;
                true
            }
            "finish" | "quit" | "exit" => true,
            "" => false,
            _ => {
                self.message = Some("Unknown command — try `show` or `finish`".to_string());
                false
            }
        }
    }
}

// ── Public entry point ────────────────────────────────────────────────────────
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

    if app.show_requested {
        println!("Generating briefing...");
        let sess = session::load()?;
        let briefing = crate::summarize::generate(&sess).await?;
        println!("{}", briefing);
    } else {
        let count = session::load().map(|s| s.events.len()).unwrap_or(0);
        println!(
            "Session ended  ·  {count} event(s) recorded  ·  run `resume show` for a briefing."
        );
    }

    Ok(())
}

// ── Event loop ────────────────────────────────────────────────────────────────
async fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    rx: &mut mpsc::UnboundedReceiver<SessionEvent>,
    shutdown_tx: tokio::sync::oneshot::Sender<()>,
) -> Result<()> {
    let tick = Duration::from_millis(50);
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
                        match key.code {
                            KeyCode::Char('c')
                                if key.modifiers.contains(KeyModifiers::CONTROL) =>
                            {
                                let _ = shutdown_tx.send(());
                                return Ok(());
                            }
                            KeyCode::Enter => {
                                if app.handle_command() {
                                    let _ = shutdown_tx.send(());
                                    return Ok(());
                                }
                            }
                            KeyCode::Char(c) => {
                                app.input.push(c);
                                app.message = None;
                            }
                            KeyCode::Backspace => {
                                app.input.pop();
                                app.message = None;
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

// ── Rendering ─────────────────────────────────────────────────────────────────
fn render(f: &mut ratatui::Frame, app: &App) {
    let area = f.area();

    // Layout:
    //   welcome box  : RESY_H + 5 inner rows + 2 border = 14 rows
    //   spacer       : 1
    //   input line   : 1
    //   separator    : 1
    //   footer/hint  : 1
    let box_height = (RESY_H as u16) + 5 + 2; // inner content + borders
    let chunks = Layout::vertical([
        Constraint::Length(box_height),
        Constraint::Length(1), // spacer
        Constraint::Length(1), // separator above input
        Constraint::Length(1), // "> " input
        Constraint::Length(1), // separator below input
        Constraint::Length(1), // hint / error message
        Constraint::Min(0),
    ])
    .split(area);

    // ── Welcome box ───────────────────────────────────────────────────────────
    let box_block = Block::default()
        .title(Span::styled(
            "─ Resume ",
            Style::default().fg(BRAND).add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(BRAND));

    let inner = box_block.inner(chunks[0]);
    f.render_widget(box_block, chunks[0]);

    // Split inner area: left (mascot + info) | right (tips)
    let panels = Layout::horizontal([
        Constraint::Length(30), // left panel: mascot + info
        Constraint::Min(0),     // right panel: tips
    ])
    .split(inner);

    render_left(f, app, panels[0]);
    render_right(f, app, panels[1]);

    // ── Separator above input ────────────────────────────────────────────────
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "─".repeat(area.width as usize),
            Style::default().fg(Color::White),
        ))),
        chunks[2],
    );

    // ── Input line ────────────────────────────────────────────────────────────
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("> ", Style::default().fg(Color::White)),
            Span::styled(app.input.clone(), Style::default().fg(Color::White)),
            Span::styled("█", Style::default().fg(Color::White)),
        ])),
        chunks[3],
    );

    // ── Separator below input ────────────────────────────────────────────────
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "─".repeat(area.width as usize),
            Style::default().fg(Color::White),
        ))),
        chunks[4],
    );

    // ── Footer / hint ─────────────────────────────────────────────────────────
    if let Some(msg) = &app.message {
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::raw("  "),
                Span::styled(msg.clone(), Style::default().fg(GRAY)),
            ])),
            chunks[5],
        );
    }
}

fn render_left(f: &mut ratatui::Frame, app: &App, area: Rect) {
    let mut lines: Vec<Line> = Vec::new();

    // "Welcome" bold
    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled(
            "Welcome",
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
        ),
    ]));
    lines.push(Line::raw(""));

    // Resy pixel art (6-col left margin + 18 cols of art)
    for row in resy_lines() {
        let mut spans = vec![Span::raw("      ")];
        spans.extend(row.spans);
        lines.push(Line::from(spans));
    }

    lines.push(Line::raw(""));

    // "resume  ·  elapsed"
    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled("resume", Style::default().fg(BRAND).add_modifier(Modifier::BOLD)),
        Span::styled(format!("  ·  {}", app.elapsed()), Style::default().fg(GRAY)),
    ]));

    // Current directory, home-dir abbreviated
    let cwd_display = std::env::current_dir()
        .ok()
        .and_then(|p| {
            dirs::home_dir().and_then(|h| {
                p.strip_prefix(&h)
                    .ok()
                    .map(|rel| format!("~/{}", rel.display()))
            })
        })
        .unwrap_or_else(|| app.project.clone());
    // Truncate only if truly overflowing the panel
    let max_path = (area.width.saturating_sub(2)) as usize;
    let cwd_display = if cwd_display.len() > max_path && max_path > 3 {
        format!("{}...", &cwd_display[..max_path - 3])
    } else {
        cwd_display
    };

    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled(cwd_display, Style::default().fg(GRAY)),
    ]));

    f.render_widget(Paragraph::new(lines), area);
}

fn render_right(f: &mut ratatui::Frame, app: &App, area: Rect) {
    // Left border acts as the panel divider
    let block = Block::default()
        .borders(Borders::LEFT)
        .border_style(Style::default().fg(GRAY));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let count = app.events.len();
    let elapsed = app.elapsed();
    let sep_width = inner.width.saturating_sub(1) as usize;

    let lines: Vec<Line> = vec![
        Line::from(Span::styled(
            "Tips for getting started",
            Style::default().fg(BRAND).add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            "─".repeat(sep_width),
            Style::default().fg(BRAND_DIM),
        )),
        Line::raw(""),
        Line::from(vec![
            Span::styled("finish", Style::default().fg(BRAND).add_modifier(Modifier::BOLD)),
            Span::styled("  or Ctrl-C to stop recording", Style::default().fg(GRAY)),
        ]),
        Line::from(vec![
            Span::styled("show", Style::default().fg(BRAND).add_modifier(Modifier::BOLD)),
            Span::styled("    to get a briefing on this session", Style::default().fg(GRAY)),
        ]),
        Line::raw(""),
        Line::from(vec![
            Span::styled(
                format!("{count} event{}", if count == 1 { "" } else { "s" }),
                Style::default().fg(GRAY),
            ),
            Span::styled(format!("  ·  {elapsed}"), Style::default().fg(BRAND_DIM)),
        ]),
    ];

    f.render_widget(Paragraph::new(lines), inner);
}

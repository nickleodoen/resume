// Live terminal UI for resume sessions.
//
// Layout (top → bottom):
//   Welcome box (mascot + info | tips)
//   Separator ─────────────────────────
//   > input prompt
//   Separator ─────────────────────────
//   Hint / error line
//   Live feed (commits + files) + briefing below it
//
// Commands:
//   show    — fetch AI briefing, display below live feed, stay in TUI
//   finish  — archive current session, start fresh (does not exit)
//   Ctrl+C  — press twice within 1.5 s to exit

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
const BRAND: Color = Color::Rgb(200, 120, 255);
const BRAND_DIM: Color = Color::Rgb(110, 60, 160);
const GRAY: Color = Color::Rgb(160, 160, 160);
const DARK_PX: Color = Color::Rgb(20, 20, 40);
const PURPLE_MAIN: Color = Color::Rgb(180, 100, 255);
const PURPLE_LIGHT: Color = Color::Rgb(210, 150, 255);
const PURPLE_DARK: Color = Color::Rgb(100, 50, 160);

// ── Resy pixel art ────────────────────────────────────────────────────────────
const RESY_W: usize = 9;
const RESY_H: usize = 7;

#[rustfmt::skip]
const RESY: [[u8; RESY_W]; RESY_H] = [
    [0, 0, 1, 1, 1, 1, 1, 0, 0],
    [0, 1, 2, 1, 1, 1, 1, 1, 0],
    [1, 1, 1, 3, 1, 3, 1, 1, 1],
    [1, 1, 1, 1, 1, 1, 1, 1, 1],
    [0, 4, 1, 4, 1, 4, 1, 4, 0],
    [0, 0, 4, 0, 4, 0, 4, 0, 0],
    [0, 0, 4, 0, 0, 0, 4, 0, 0],
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

// ── Briefing message type ─────────────────────────────────────────────────────
// Distinguishes the auto-fetch on startup (silent on error) from user-typed `show`.
enum BriefingMsg {
    Startup(Result<String, String>),
    UserRequested(Result<String, String>),
}

// ── Command result ────────────────────────────────────────────────────────────
enum Cmd {
    Finish,        // archive session, start fresh — stays in TUI
    SpawnBriefing, // kick off API call
    Stay,          // no-op
}

// ── App state ─────────────────────────────────────────────────────────────────
struct App {
    events: Vec<SessionEvent>,
    project: String,
    started: Instant,
    input: String,
    message: Option<String>,
    briefing: Option<String>,
    briefing_header: Option<String>,
    briefing_loading: bool,
}

impl App {
    fn new(project: String) -> Self {
        Self {
            events: Vec::new(),
            project,
            started: Instant::now(),
            input: String::new(),
            message: None,
            briefing: None,
            briefing_header: None,
            briefing_loading: false,
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

    fn handle_command(&mut self) -> Cmd {
        let cmd = self.input.trim().to_lowercase();
        self.input.clear();
        match cmd.as_str() {
            "show" => {
                if std::env::var("ANTHROPIC_API_KEY").is_err() {
                    self.message = Some(
                        "ANTHROPIC_API_KEY is not set — export it and try again".to_string(),
                    );
                    Cmd::Stay
                } else if self.briefing_loading {
                    self.message = Some("Already generating a briefing…".to_string());
                    Cmd::Stay
                } else {
                    self.briefing_loading = true;
                    self.briefing = None;
                    self.briefing_header = None;
                    self.message = None;
                    Cmd::SpawnBriefing
                }
            }
            "finish" => Cmd::Finish,
            "" => Cmd::Stay,
            _ => {
                self.message =
                    Some("Unknown command — try `show` or `finish`".to_string());
                Cmd::Stay
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

    let (watcher_tx, mut watcher_rx) = mpsc::unbounded_channel::<SessionEvent>();
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let (briefing_tx, mut briefing_rx) = mpsc::unbounded_channel::<BriefingMsg>();

    tokio::spawn(async move {
        if let Err(e) = watcher::watch(Some(watcher_tx), Some(shutdown_rx)).await {
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

    // Auto-fetch briefing from the previous session immediately on startup.
    if std::env::var("ANTHROPIC_API_KEY").is_ok() {
        app.briefing_loading = true;
        let tx = briefing_tx.clone();
        tokio::spawn(async move {
            let result = async {
                let sess = crate::session::load_latest()?;
                crate::summarize::generate(&sess).await
            }
            .await;
            let _ = tx.send(BriefingMsg::Startup(result.map_err(|e| e.to_string())));
        });
    }

    let result =
        run_loop(&mut terminal, &mut app, &mut watcher_rx, &mut briefing_rx, briefing_tx, shutdown_tx).await;

    disable_raw_mode().ok();
    execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture).ok();
    terminal.show_cursor().ok();

    result?;

    let count = session::load().map(|s| s.events.len()).unwrap_or(0);
    println!(
        "Session ended  ·  {count} event(s) recorded  ·  run `resume show` for a briefing."
    );

    Ok(())
}

// ── Event loop ────────────────────────────────────────────────────────────────
async fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    rx: &mut mpsc::UnboundedReceiver<SessionEvent>,
    briefing_rx: &mut mpsc::UnboundedReceiver<BriefingMsg>,
    briefing_tx: mpsc::UnboundedSender<BriefingMsg>,
    shutdown_tx: tokio::sync::oneshot::Sender<()>,
) -> Result<()> {
    let tick = Duration::from_millis(50);
    let mut shutdown_tx = Some(shutdown_tx);
    let mut last_ctrl_c: Option<Instant> = None;

    loop {
        terminal.draw(|f| render(f, app))?;

        tokio::select! {
            maybe_ev = rx.recv() => {
                match maybe_ev {
                    Some(ev) => app.push(ev),
                    None => break,
                }
            }
            maybe_briefing = briefing_rx.recv() => {
                if let Some(msg) = maybe_briefing {
                    app.briefing_loading = false;
                    match msg {
                        BriefingMsg::Startup(Ok(text)) => {
                            app.briefing = Some(text);
                            app.briefing_header = Some("Previous session".to_string());
                        }
                        BriefingMsg::Startup(Err(_)) => {
                            // No previous session or API error on startup — silent.
                        }
                        BriefingMsg::UserRequested(Ok(text)) => {
                            app.briefing = Some(text);
                            app.briefing_header = Some("Briefing".to_string());
                        }
                        BriefingMsg::UserRequested(Err(e)) => {
                            app.message = Some(format!("show error: {e}"));
                        }
                    }
                }
            }
            _ = tokio::time::sleep(tick) => {
                // Expire the double-Ctrl+C window.
                if let Some(t) = last_ctrl_c {
                    if t.elapsed() >= Duration::from_millis(1500) {
                        last_ctrl_c = None;
                        if matches!(app.message.as_deref(), Some("Press Ctrl+C again to exit")) {
                            app.message = None;
                        }
                    }
                }

                while event::poll(Duration::ZERO).unwrap_or(false) {
                    if let Ok(CEvent::Key(key)) = event::read() {
                        match key.code {
                            KeyCode::Char('c')
                                if key.modifiers.contains(KeyModifiers::CONTROL) =>
                            {
                                let now = Instant::now();
                                if last_ctrl_c.map_or(false, |t| {
                                    now.duration_since(t) < Duration::from_millis(1500)
                                }) {
                                    // Second Ctrl+C within window — exit.
                                    if let Some(tx) = shutdown_tx.take() {
                                        let _ = tx.send(());
                                    }
                                    return Ok(());
                                }
                                last_ctrl_c = Some(now);
                                app.message =
                                    Some("Press Ctrl+C again to exit".to_string());
                            }
                            KeyCode::Enter => {
                                match app.handle_command() {
                                    Cmd::Finish => {
                                        let count = app.events.len();
                                        match session::init() {
                                            Ok(()) => {
                                                app.events.clear();
                                                app.briefing = None;
                                                app.briefing_header = None;
                                                app.message = Some(format!(
                                                    "Session saved · {} event{} · fresh session started",
                                                    count,
                                                    if count == 1 { "" } else { "s" }
                                                ));
                                            }
                                            Err(e) => {
                                                app.message =
                                                    Some(format!("finish error: {e}"));
                                            }
                                        }
                                    }
                                    Cmd::SpawnBriefing => {
                                        let tx = briefing_tx.clone();
                                        tokio::spawn(async move {
                                            let result = async {
                                                let sess =
                                                    crate::session::load_latest()?;
                                                crate::summarize::generate(&sess).await
                                            }
                                            .await;
                                            let _ = tx.send(BriefingMsg::UserRequested(
                                                result.map_err(|e| e.to_string()),
                                            ));
                                        });
                                    }
                                    Cmd::Stay => {}
                                }
                            }
                            KeyCode::Char(c) => {
                                last_ctrl_c = None;
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

    // chunks[0] welcome box  — fixed height
    // chunks[1] separator    — 1
    // chunks[2] input        — 1
    // chunks[3] separator    — 1
    // chunks[4] hint/error   — 1
    // chunks[5] feed+briefing — remaining
    let box_height = (RESY_H as u16) + 5 + 2;
    let chunks = Layout::vertical([
        Constraint::Length(box_height),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
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

    let panels = Layout::horizontal([
        Constraint::Length(30),
        Constraint::Min(0),
    ])
    .split(inner);
    render_left(f, app, panels[0]);
    render_right(f, app, panels[1]);

    // ── Separator above input ─────────────────────────────────────────────────
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "─".repeat(area.width as usize),
            Style::default().fg(Color::White),
        ))),
        chunks[1],
    );

    // ── Input line ────────────────────────────────────────────────────────────
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("> ", Style::default().fg(Color::White)),
            Span::styled(app.input.clone(), Style::default().fg(Color::White)),
            Span::styled("█", Style::default().fg(Color::White)),
        ])),
        chunks[2],
    );

    // ── Separator below input ─────────────────────────────────────────────────
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "─".repeat(area.width as usize),
            Style::default().fg(Color::White),
        ))),
        chunks[3],
    );

    // ── Hint / error ──────────────────────────────────────────────────────────
    if let Some(msg) = &app.message {
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::raw("  "),
                Span::styled(msg.clone(), Style::default().fg(GRAY)),
            ])),
            chunks[4],
        );
    }

    // ── Live feed + briefing ──────────────────────────────────────────────────
    let feed_area = chunks[5];
    if feed_area.height == 0 {
        return;
    }
    let max_w = (feed_area.width.saturating_sub(6)) as usize;

    // Commits (newest first)
    let commits: Vec<Line> = app
        .events
        .iter()
        .rev()
        .filter(|e| matches!(e.event_type, crate::session::EventType::GitDiff))
        .map(|e| {
            let msg = e.content.lines().next().unwrap_or("").to_string();
            let msg = if msg.len() > max_w && max_w > 3 {
                format!("{}…", &msg[..max_w - 1])
            } else {
                msg
            };
            Line::from(vec![
                Span::raw("  "),
                Span::styled("↑ ", Style::default().fg(BRAND).add_modifier(Modifier::BOLD)),
                Span::styled(msg, Style::default().fg(Color::White)),
            ])
        })
        .collect();

    // Unique files touched (newest first, deduped)
    let mut seen_files = std::collections::HashSet::new();
    let files: Vec<Line> = app
        .events
        .iter()
        .rev()
        .filter(|e| matches!(e.event_type, crate::session::EventType::FileChange))
        .filter(|e| seen_files.insert(e.content.clone()))
        .map(|e| {
            let path = if e.content.len() > max_w && max_w > 3 {
                format!("{}…", &e.content[..max_w - 1])
            } else {
                e.content.clone()
            };
            Line::from(vec![
                Span::raw("  "),
                Span::styled("  ", Style::default().fg(GRAY)),
                Span::styled(path, Style::default().fg(GRAY)),
            ])
        })
        .collect();

    let mut feed: Vec<Line> = Vec::new();
    if commits.is_empty() && files.is_empty() {
        feed.push(Line::from(vec![
            Span::raw("  "),
            Span::styled("Watching for changes…", Style::default().fg(BRAND_DIM)),
        ]));
    } else {
        feed.extend(commits);
        if !files.is_empty() {
            feed.push(Line::raw(""));
            feed.extend(files);
        }
    }

    // Briefing — shown below the live feed
    if app.briefing_loading {
        feed.push(Line::raw(""));
        feed.push(Line::from(vec![
            Span::raw("  "),
            Span::styled("Generating briefing…", Style::default().fg(BRAND_DIM)),
        ]));
    } else if let Some(briefing) = &app.briefing {
        let sep_w = feed_area.width.saturating_sub(4) as usize;
        feed.push(Line::raw(""));
        feed.push(Line::from(Span::styled(
            "─".repeat(sep_w),
            Style::default().fg(BRAND_DIM),
        )));
        if let Some(header) = &app.briefing_header {
            feed.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    header.clone(),
                    Style::default().fg(BRAND).add_modifier(Modifier::BOLD),
                ),
            ]));
        }
        feed.push(Line::raw(""));
        for line in briefing.lines() {
            feed.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(line.to_string(), Style::default().fg(Color::White)),
            ]));
        }
    }

    f.render_widget(Paragraph::new(feed), feed_area);
}

fn render_left(f: &mut ratatui::Frame, app: &App, area: Rect) {
    let mut lines: Vec<Line> = Vec::new();

    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled(
            "Welcome",
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
        ),
    ]));
    lines.push(Line::raw(""));

    for row in resy_lines() {
        let mut spans = vec![Span::raw("      ")];
        spans.extend(row.spans);
        lines.push(Line::from(spans));
    }

    lines.push(Line::raw(""));

    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled(
            "resume",
            Style::default().fg(BRAND).add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("  ·  {}", app.elapsed()), Style::default().fg(GRAY)),
    ]));

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
            "Commands",
            Style::default().fg(BRAND).add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            "─".repeat(sep_width),
            Style::default().fg(BRAND_DIM),
        )),
        Line::raw(""),
        Line::from(vec![
            Span::styled(
                "show    ",
                Style::default().fg(BRAND).add_modifier(Modifier::BOLD),
            ),
            Span::styled("get a briefing on this session", Style::default().fg(GRAY)),
        ]),
        Line::from(vec![
            Span::styled(
                "finish  ",
                Style::default().fg(BRAND).add_modifier(Modifier::BOLD),
            ),
            Span::styled("save session, start fresh", Style::default().fg(GRAY)),
        ]),
        Line::from(vec![
            Span::styled(
                "Ctrl+C  ",
                Style::default().fg(BRAND).add_modifier(Modifier::BOLD),
            ),
            Span::styled("press twice to exit", Style::default().fg(GRAY)),
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

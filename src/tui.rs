// Live terminal UI for resume sessions.
// Uses ratatui + crossterm to render a real-time event dashboard.

use anyhow::{Context, Result};
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event as CEvent, KeyCode, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, List, ListItem, Paragraph},
    Terminal,
};
use std::io;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

use crate::session::{self, EventType, SessionEvent};
use crate::watcher;

struct App {
    events: Vec<SessionEvent>,
    project: String,
    started: Instant,
}

impl App {
    fn new(project: String) -> Self {
        Self {
            events: Vec::new(),
            project,
            started: Instant::now(),
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
}

pub async fn run() -> Result<()> {
    // Init session on disk.
    session::init()?;

    let project = std::env::current_dir()
        .unwrap_or_default()
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "unknown".to_string());

    // Channel: watcher → TUI
    let (tx, mut rx) = mpsc::unbounded_channel::<SessionEvent>();

    // Oneshot channel: TUI → watcher (shutdown signal)
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

    // Start the watcher in the background, piping events to us.
    tokio::spawn(async move {
        if let Err(e) = watcher::watch(Some(tx), Some(shutdown_rx)).await {
            eprintln!("watcher error: {e}");
        }
    });

    // Set up terminal.
    enable_raw_mode().context("failed to enable raw mode")?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)
        .context("failed to enter alternate screen")?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).context("failed to create terminal")?;

    let mut app = App::new(project);
    let tick = Duration::from_millis(250);

    let result = run_loop(&mut terminal, &mut app, &mut rx, tick, shutdown_tx).await;

    // Restore terminal unconditionally.
    disable_raw_mode().ok();
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )
    .ok();
    terminal.show_cursor().ok();

    // Print exit summary to the now-restored terminal.
    result?;
    println!(
        "Session ended · {} event(s) recorded · run `resume show` for a briefing.",
        // Load from disk to get the true count (includes events logged before TUI started).
        session::load().map(|s| s.events.len()).unwrap_or(0)
    );
    Ok(())
}

async fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    rx: &mut mpsc::UnboundedReceiver<SessionEvent>,
    tick: Duration,
    shutdown_tx: tokio::sync::oneshot::Sender<()>,
) -> Result<()> {
    loop {
        terminal.draw(|f| render(f, app))?;

        // Poll for either a new session event or a crossterm key event.
        tokio::select! {
            maybe_ev = rx.recv() => {
                match maybe_ev {
                    Some(ev) => app.push(ev),
                    None => break, // watcher closed the channel
                }
            }
            _ = tokio::time::sleep(tick) => {
                // Tick — just redraw (for the clock).
                // Also drain any pending crossterm events.
                while event::poll(Duration::ZERO).unwrap_or(false) {
                    if let Ok(CEvent::Key(key)) = event::read() {
                        if key.code == KeyCode::Char('c')
                            && key.modifiers.contains(KeyModifiers::CONTROL)
                        {
                            // Ctrl-C: signal the watcher to shut down.
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

    // Layout: header (3) | event list (fill) | footer (3)
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(1),
            Constraint::Length(3),
        ])
        .split(area);

    // -- Header --
    let header_text = Line::from(vec![
        Span::styled(" * resume", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
        Span::raw("  .  "),
        Span::styled(&app.project, Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
        Span::raw("  .  "),
        Span::styled(app.elapsed(), Style::default().fg(Color::White)),
    ]);
    let header = Paragraph::new(header_text)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(Color::DarkGray)),
        )
        .alignment(Alignment::Left);
    f.render_widget(header, chunks[0]);

    // -- Event list --
    let items: Vec<ListItem> = app
        .events
        .iter()
        .rev() // newest first
        .map(|ev| {
            let (badge, badge_color) = match ev.event_type {
                EventType::FileChange => (" FILE ", Color::Cyan),
                EventType::GitDiff   => (" GIT  ", Color::Yellow),
                EventType::Command   => (" CMD  ", Color::Green),
            };
            let time_str = ev.timestamp.format("%H:%M:%S").to_string();
            // Truncate content for display (single line)
            let content = ev.content.lines().next().unwrap_or("").to_string();
            let display_content = if content.len() > 60 {
                format!("{}...", &content[..59])
            } else {
                content
            };
            ListItem::new(Line::from(vec![
                Span::styled(badge, Style::default().fg(Color::Black).bg(badge_color).add_modifier(Modifier::BOLD)),
                Span::raw("  "),
                Span::styled(display_content, Style::default().fg(Color::White)),
                Span::styled(
                    format!("  {time_str}"),
                    Style::default().fg(Color::DarkGray),
                ),
            ]))
        })
        .collect();

    let list = List::new(items).block(
        Block::default()
            .borders(Borders::LEFT | Borders::RIGHT)
            .border_style(Style::default().fg(Color::DarkGray)),
    );
    f.render_widget(list, chunks[1]);

    // -- Footer --
    let footer_text = Line::from(vec![
        Span::styled(
            format!(" {} event{}", app.events.len(), if app.events.len() == 1 { "" } else { "s" }),
            Style::default().fg(Color::White),
        ),
        Span::styled("  .  watching  .  ", Style::default().fg(Color::DarkGray)),
        Span::styled("Ctrl-C", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
        Span::styled(" or ", Style::default().fg(Color::DarkGray)),
        Span::styled("`finish`", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
        Span::styled(" to stop ", Style::default().fg(Color::DarkGray)),
    ]);
    let footer = Paragraph::new(footer_text)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(Color::DarkGray)),
        )
        .alignment(Alignment::Left);
    f.render_widget(footer, chunks[2]);
}

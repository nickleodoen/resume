// Live terminal UI for resume sessions.
//
// Layout (top → bottom):
//   Welcome box (mascot + info | tips)   — fixed height
//   Briefing area                         — scrollable, takes remaining space
//   Separator ─────────────────────────
//   > input prompt
//   Separator ─────────────────────────
//   Hint / error line
//
// Commands:
//   show         — fetch AI briefing, display in briefing area, stay in TUI
//   finish       — archive current session, start fresh (does not exit)
//   note <text>  — save a note for this project (persists across sessions)
//   notes        — display all notes in the briefing area
//   notes open   — open notes.txt in $VISUAL / default app (non-blocking)
//   notes clear  — delete all notes for this project
//   ↑ ↓     — scroll briefing (PgUp / PgDn for faster scroll)
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
    widgets::{Block, Borders, Paragraph, Wrap},
    Terminal,
};
use std::io;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

use crate::models::{self, ModelInfo};
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
    [4, 0, 4, 0, 4, 0, 4, 0, 4],
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

// ── Help text ─────────────────────────────────────────────────────────────────
const HELP_TEXT: &str = "\
show                     get an AI briefing on this session
finish                   save session, start fresh
note <text>              save a note for this project
notes                    list all saved notes
notes open               open notes.txt in your editor
notes clear              delete all notes
model default <model>    set the default AI model

Model IDs:
  claude-haiku-4-5-20251001    Haiku           (Anthropic)
  claude-sonnet-4-6            Sonnet          (Anthropic)
  claude-opus-4-6              Opus            (Anthropic)
  qwen3.5                      Qwen 3.5        (Ollama local)
  qwen3-coder:30b              Qwen Coder 30B  (Ollama local)
  deepseek-coder-v2:16b        DeepSeek V2 16B (Ollama local)

Scroll: ↑ ↓ / PgUp PgDn / mouse wheel
Shift+Tab: cycle available models
Ctrl+C (twice): exit";

// ── Command result ────────────────────────────────────────────────────────────
enum Cmd {
    Finish,                  // archive session, start fresh — stays in TUI
    SpawnBriefing,           // kick off API call
    SaveNote(String),        // persist a note for this project
    ShowNotes,               // display all notes in the briefing area
    ClearNotes,              // delete all notes for this project
    OpenNotes,               // open notes.txt in $VISUAL / open (non-blocking)
    SetDefaultModel(String), // save a model preference globally
    ShowHelp,                // show help text in briefing area
    Stay,                    // no-op
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
    // Cache: briefing text + event count at generation time.
    // Invalidated when events.len() grows (new file/git activity).
    cached_briefing: Option<(String, usize)>,
    scroll_offset: u16,
    scroll_max: u16,
    available_models: Vec<&'static ModelInfo>,
    current_model_idx: usize,
}

impl App {
    fn new(project: String, available_models: Vec<&'static ModelInfo>, current_model_idx: usize) -> Self {
        Self {
            events: Vec::new(),
            project,
            started: Instant::now(),
            input: String::new(),
            message: None,
            briefing: None,
            briefing_header: None,
            briefing_loading: false,
            cached_briefing: None,
            scroll_offset: 0,
            scroll_max: 0,
            available_models,
            current_model_idx,
        }
    }

    fn current_model(&self) -> &'static ModelInfo {
        self.available_models
            .get(self.current_model_idx)
            .copied()
            .unwrap_or(&models::MODELS[0])
    }

    fn cycle_model(&mut self) {
        if self.available_models.is_empty() {
            return;
        }
        self.current_model_idx = (self.current_model_idx + 1) % self.available_models.len();
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
        let raw = self.input.trim().to_string();
        self.input.clear();
        let lower = raw.to_lowercase();

        match lower.as_str() {
            "help" | "--help" => Cmd::ShowHelp,
            "show" => {
                if self.available_models.is_empty() {
                    self.message = Some(
                        "No models available — set ANTHROPIC_API_KEY or start Ollama".to_string(),
                    );
                    Cmd::Stay
                } else if self.briefing_loading {
                    self.message = Some("Already generating a briefing…".to_string());
                    Cmd::Stay
                } else if let Some((text, event_count)) = &self.cached_briefing {
                    // Cache hit: serve instantly if no new events since generation.
                    if *event_count == self.events.len() {
                        self.briefing = Some(text.clone());
                        self.briefing_header = Some("Briefing".to_string());
                        self.scroll_offset = 0;
                        self.message = None;
                        Cmd::Stay
                    } else {
                        // New events recorded — cache is stale, fetch fresh.
                        self.briefing_loading = true;
                        self.briefing = None;
                        self.briefing_header = None;
                        self.message = None;
                        Cmd::SpawnBriefing
                    }
                } else {
                    self.briefing_loading = true;
                    self.briefing = None;
                    self.briefing_header = None;
                    self.message = None;
                    Cmd::SpawnBriefing
                }
            }
            "finish" => Cmd::Finish,
            "notes" => Cmd::ShowNotes,
            "notes open" | "note open" => Cmd::OpenNotes,
            "notes clear" | "notes --clear" | "note clear" | "note --clear" => Cmd::ClearNotes,
            "note" => {
                self.message = Some("Usage: note <text>".to_string());
                Cmd::Stay
            }
            "" => Cmd::Stay,
            _ if lower.starts_with("model default ") => {
                let name = raw["model default ".len()..].trim().to_string();
                Cmd::SetDefaultModel(name)
            }
            _ if lower.starts_with("note ") => {
                let text = raw["note ".len()..].trim().to_string();
                if text.is_empty() {
                    self.message = Some("Usage: note <text>".to_string());
                    Cmd::Stay
                } else {
                    Cmd::SaveNote(text)
                }
            }
            _ => {
                self.message = Some(
                    "Unknown command — try `show`, `finish`, `note <text>`, `notes`, `notes open`, `notes clear`"
                        .to_string(),
                );
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

    // Check which models are reachable before entering the TUI.
    // Ollama check is fast (connection refused is near-instant).
    let ollama_pulled = models::ollama_pulled_ids().await;
    let available = models::filter_available(&ollama_pulled);
    // Priority: RESUME_MODEL env var → saved default → deepseek → haiku → first available.
    let saved_default = session::load_default_model();
    let start_idx = std::env::var("RESUME_MODEL")
        .ok()
        .and_then(|id| available.iter().position(|m| m.id == id))
        .or_else(|| saved_default.as_deref().and_then(|id| available.iter().position(|m| m.id == id)))
        .or_else(|| available.iter().position(|m| m.id.starts_with("deepseek")))
        .or_else(|| available.iter().position(|m| m.id.starts_with("claude-haiku")))
        .unwrap_or(0);

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

    let mut app = App::new(project, available, start_idx);

    // Auto-fetch briefing from the previous session on startup if a model is ready.
    if !app.available_models.is_empty() {
        app.briefing_loading = true;
        let tx = briefing_tx.clone();
        let model_id = app.current_model().id;
        tokio::spawn(async move {
            let result = async {
                let sess = crate::session::load_latest()?;
                let notes_text = crate::session::load_notes_text().unwrap_or_default();
                crate::summarize::generate(&sess, &notes_text, model_id).await
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

    session::clear_sentinel();
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
        terminal.draw(|f| render(f, app))?;  // render takes &mut App to update scroll_max

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
                            app.cached_briefing = Some((text.clone(), app.events.len()));
                            app.briefing = Some(text);
                            app.briefing_header = Some("Briefing".to_string());
                            app.scroll_offset = 0;
                        }
                        BriefingMsg::Startup(Err(_)) => {
                            // No previous session or API error on startup — silent.
                        }
                        BriefingMsg::UserRequested(Ok(text)) => {
                            app.cached_briefing = Some((text.clone(), app.events.len()));
                            app.briefing = Some(text);
                            app.briefing_header = Some("Briefing".to_string());
                            app.scroll_offset = 0;
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
                    match event::read() {
                        Ok(CEvent::Mouse(mouse)) => {
                            use crossterm::event::MouseEventKind;
                            match mouse.kind {
                                MouseEventKind::ScrollUp => {
                                    app.scroll_offset = app.scroll_offset.saturating_sub(3);
                                }
                                MouseEventKind::ScrollDown => {
                                    app.scroll_offset = app.scroll_offset.saturating_add(3);
                                }
                                _ => {}
                            }
                        }
                        Ok(CEvent::Key(key)) => {
                            match key.code {
                                KeyCode::BackTab => {
                                    app.cycle_model();
                                    app.message = None;
                                }
                                KeyCode::Char('c')
                                    if key.modifiers.contains(KeyModifiers::CONTROL) =>
                                {
                                    let now = Instant::now();
                                    if last_ctrl_c.map_or(false, |t| {
                                        now.duration_since(t) < Duration::from_millis(1500)
                                    }) {
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
                                                    app.cached_briefing = None;
                                                    app.scroll_offset = 0;
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
                                            let model_id = app.current_model().id;
                                            tokio::spawn(async move {
                                                let result = async {
                                                    let sess = crate::session::load_latest()?;
                                                    let notes_text = crate::session::load_notes_text().unwrap_or_default();
                                                    crate::summarize::generate(&sess, &notes_text, model_id).await
                                                }
                                                .await;
                                                let _ = tx.send(BriefingMsg::UserRequested(
                                                    result.map_err(|e| e.to_string()),
                                                ));
                                            });
                                        }
                                        Cmd::ClearNotes => {
                                            match session::clear_notes() {
                                                Ok(()) => {
                                                    app.message =
                                                        Some("All notes cleared.".to_string());
                                                    if app.briefing_header.as_deref()
                                                        == Some("Notes")
                                                    {
                                                        app.briefing = None;
                                                        app.briefing_header = None;
                                                    }
                                                }
                                                Err(e) => {
                                                    app.message =
                                                        Some(format!("clear error: {e}"));
                                                }
                                            }
                                        }
                                        Cmd::OpenNotes => {
                                            match session::notes_path() {
                                                Ok(path) => {
                                                    // Ensure the file exists before opening.
                                                    if !path.exists() {
                                                        let _ = std::fs::File::create(&path);
                                                    }
                                                    // Prefer $VISUAL (GUI editor); fall back to macOS `open`.
                                                    let spawned = std::env::var("VISUAL")
                                                        .ok()
                                                        .map(|ed| {
                                                            std::process::Command::new(&ed)
                                                                .arg(&path)
                                                                .spawn()
                                                        })
                                                        .unwrap_or_else(|| {
                                                            std::process::Command::new("open")
                                                                .arg(&path)
                                                                .spawn()
                                                        });
                                                    app.message = match spawned {
                                                        Ok(_) => Some(
                                                            "Notes opened in editor.".to_string(),
                                                        ),
                                                        Err(e) => Some(format!(
                                                            "failed to open editor: {e}"
                                                        )),
                                                    };
                                                }
                                                Err(e) => {
                                                    app.message =
                                                        Some(format!("notes error: {e}"));
                                                }
                                            }
                                        }
                                        Cmd::SaveNote(text) => {
                                            match session::append_note(&text) {
                                                Ok(()) => {
                                                    app.message = Some(format!(
                                                        "Note saved: \"{}\"",
                                                        text
                                                    ));
                                                }
                                                Err(e) => {
                                                    app.message =
                                                        Some(format!("note error: {e}"));
                                                }
                                            }
                                        }
                                        Cmd::ShowNotes => {
                                            match session::load_notes_text() {
                                                Ok(text) if text.trim().is_empty() => {
                                                    app.message = Some(
                                                        "No notes yet — type `note <text>` to add one."
                                                            .to_string(),
                                                    );
                                                }
                                                Ok(text) => {
                                                    app.briefing = Some(text);
                                                    app.briefing_header =
                                                        Some("Notes".to_string());
                                                    app.scroll_offset = 0;
                                                    app.message = None;
                                                }
                                                Err(e) => {
                                                    app.message =
                                                        Some(format!("notes error: {e}"));
                                                }
                                            }
                                        }
                                        Cmd::SetDefaultModel(name) => {
                                            let found = app
                                                .available_models
                                                .iter()
                                                .enumerate()
                                                .find(|(_, m)| {
                                                    m.id == name
                                                        || m.display_name.to_lowercase()
                                                            == name.to_lowercase()
                                                });
                                            match found {
                                                Some((idx, m)) => {
                                                    let model_id = m.id;
                                                    match session::save_default_model(model_id) {
                                                        Ok(()) => {
                                                            app.current_model_idx = idx;
                                                            app.message = Some(format!(
                                                                "Default model set to: {}",
                                                                m.display_name
                                                            ));
                                                        }
                                                        Err(e) => {
                                                            app.message = Some(format!(
                                                                "failed to save default: {e}"
                                                            ));
                                                        }
                                                    }
                                                }
                                                None => {
                                                    let known = models::MODELS.iter().any(|m| {
                                                        m.id == name
                                                            || m.display_name.to_lowercase()
                                                                == name.to_lowercase()
                                                    });
                                                    app.message = Some(if known {
                                                        format!("'{name}' is not currently available — is Ollama running?")
                                                    } else {
                                                        format!("Unknown model: {name}  (type `help` for model IDs)")
                                                    });
                                                }
                                            }
                                        }
                                        Cmd::ShowHelp => {
                                            app.briefing = Some(HELP_TEXT.to_string());
                                            app.briefing_header = Some("Help".to_string());
                                            app.scroll_offset = 0;
                                            app.message = None;
                                        }
                                        Cmd::Stay => {}
                                    }
                                }
                                KeyCode::Up => {
                                    app.scroll_offset = app.scroll_offset.saturating_sub(1);
                                }
                                KeyCode::Down => {
                                    app.scroll_offset = app.scroll_offset.saturating_add(1);
                                }
                                KeyCode::PageUp => {
                                    app.scroll_offset = app.scroll_offset.saturating_sub(10);
                                }
                                KeyCode::PageDown => {
                                    app.scroll_offset = app.scroll_offset.saturating_add(10);
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
                        _ => {}
                    }
                }
            }
        }
    }
    Ok(())
}

// ── Content height estimator ──────────────────────────────────────────────────
// Estimates how many terminal rows the briefing content will occupy so the layout
// can decide whether to float the command bar up (fits) or pin it to the bottom.
fn estimate_content_rows(app: &App, area_width: u16) -> u16 {
    if app.briefing_loading {
        2 // blank line + "Generating briefing..."
    } else if let Some(briefing) = &app.briefing {
        let avail = (area_width.saturating_sub(4) as usize).max(1);
        let mut rows = 3u16; // blank + separator + blank
        if app.briefing_header.is_some() {
            rows += 1;
        }
        for line in briefing.lines() {
            let len = line.len() + 2; // +2 for "  " indent
            rows += ((len + avail - 1) / avail).max(1) as u16;
        }
        rows + 2 // safety margin: byte-length != display-width for unicode, and wrapping estimates can be off
    } else {
        0
    }
}

// ── Rendering ─────────────────────────────────────────────────────────────────
// Takes &mut App so it can update scroll_max and clamp scroll_offset each frame.
fn render(f: &mut ratatui::Frame, app: &mut App) {
    let area = f.area();
    let box_height = (RESY_H as u16) + 5 + 2;
    let cmd_bar_rows = 5u16; // sep + input + sep + hint + model bar

    let available = area.height
        .saturating_sub(box_height)
        .saturating_sub(cmd_bar_rows);

    let content_h = estimate_content_rows(app, area.width);
    let fits = content_h == 0 || content_h <= available;

    // Update scroll limits every frame so they react to terminal resize.
    if fits {
        app.scroll_max = 0;
        app.scroll_offset = 0;
    } else {
        app.scroll_max = content_h.saturating_sub(available);
        app.scroll_offset = app.scroll_offset.min(app.scroll_max);
    }

    // Layout:
    //   [0] welcome box  — fixed
    //   [1] content      — Length(content_h) when fits, Min(0) when overflows
    //   [2] sep          — 1
    //   [3] input        — 1  (command bar)
    //   [4] sep          — 1
    //   [5] model bar    — 1  (shift+tab indicator — always visible)
    //   [6] hint/error   — 1
    //   [7] trailing     — Min(0) spacer when fits, Length(0) dummy when overflows
    let chunks = if fits {
        Layout::vertical([
            Constraint::Length(box_height),
            Constraint::Length(content_h),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
    } else {
        Layout::vertical([
            Constraint::Length(box_height),
            Constraint::Min(0),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(0),
        ])
    }
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
        Constraint::Length(34),
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

    // ── Separator below input ─────────────────────────────────────────────────
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "─".repeat(area.width as usize),
            Style::default().fg(Color::White),
        ))),
        chunks[4],
    );

    // ── Model indicator ───────────────────────────────────────────────────────
    {
        let model = app.current_model();
        let provider_color = match model.provider {
            models::Provider::Anthropic => BRAND,
            models::Provider::Ollama => Color::Green,
        };
        let suffix = if app.available_models.len() > 1 {
            "  (shift+tab to cycle)"
        } else {
            ""
        };
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("  >> ", Style::default().fg(provider_color).add_modifier(Modifier::BOLD)),
                Span::styled(model.display_name, Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
                Span::styled(suffix, Style::default().fg(GRAY)),
            ])),
            chunks[5],
        );
    }

    // ── Hint / error ──────────────────────────────────────────────────────────
    if let Some(msg) = &app.message {
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::raw("  "),
                Span::styled(msg.clone(), Style::default().fg(GRAY)),
            ])),
            chunks[6],
        );
    }

    // ── Briefing (scrollable) ─────────────────────────────────────────────────
    let feed_area = chunks[1];
    if feed_area.height == 0 {
        return;
    }

    let mut feed: Vec<Line> = Vec::new();

    if app.briefing_loading {
        // Animate dots: phase cycles 0→1→2 every 400 ms
        let dot_phase = (app.started.elapsed().as_millis() / 400) % 3;
        let dots = match dot_phase {
            0 => ".  ",
            1 => ".. ",
            _ => "...",
        };
        feed.push(Line::raw(""));
        feed.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(
                format!("Generating briefing{dots}"),
                Style::default().fg(BRAND_DIM),
            ),
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
        const SECTION_HEADERS: &[&str] = &["Working on", "Progress", "Next step"];
        let text_w = (feed_area.width.saturating_sub(4) as usize).max(1);
        for line in briefing.lines() {
            if line.is_empty() {
                feed.push(Line::raw(""));
                continue;
            }
            let is_header = SECTION_HEADERS.contains(&line.trim());
            let style = if is_header {
                Style::default().fg(BRAND).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };
            // Pre-wrap at word boundaries so each chunk gets its own "  " indent.
            // Without this, ratatui wraps long Lines but only the first row gets
            // the leading indent span — continuation rows start at column 0.
            let mut s = line;
            loop {
                if s.len() <= text_w {
                    feed.push(Line::from(vec![
                        Span::raw("  "),
                        Span::styled(s.to_string(), style),
                    ]));
                    break;
                }
                let break_at = s[..text_w].rfind(' ').unwrap_or(text_w);
                feed.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(s[..break_at].to_string(), style),
                ]));
                s = s[break_at..].trim_start_matches(' ');
            }
        }
    }

    f.render_widget(
        Paragraph::new(feed)
            .wrap(Wrap { trim: false })
            .scroll((app.scroll_offset, 0)),
        feed_area,
    );
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
        Span::styled(format!(" · {}", app.elapsed()), Style::default().fg(GRAY)),
        Span::styled(
            format!(" · {} event{}", app.events.len(), if app.events.len() == 1 { "" } else { "s" }),
            Style::default().fg(GRAY),
        ),
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

fn render_right(f: &mut ratatui::Frame, _app: &App, area: Rect) {
    let block = Block::default()
        .borders(Borders::LEFT)
        .border_style(Style::default().fg(GRAY));
    let inner = block.inner(area);
    f.render_widget(block, area);

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
                "note    ",
                Style::default().fg(BRAND).add_modifier(Modifier::BOLD),
            ),
            Span::styled("note <text> — save a note", Style::default().fg(GRAY)),
        ]),
        Line::from(vec![
            Span::styled(
                "notes   ",
                Style::default().fg(BRAND).add_modifier(Modifier::BOLD),
            ),
            Span::styled("list · open · clear", Style::default().fg(GRAY)),
        ]),
        Line::from(vec![
            Span::styled(
                "help    ",
                Style::default().fg(BRAND).add_modifier(Modifier::BOLD),
            ),
            Span::styled("for help and available models", Style::default().fg(GRAY)),
        ]),
        Line::from(vec![
            Span::styled(
                "Ctrl+C  ",
                Style::default().fg(BRAND).add_modifier(Modifier::BOLD),
            ),
            Span::styled("press twice to exit", Style::default().fg(GRAY)),
        ]),
    ];

    f.render_widget(Paragraph::new(lines), inner);
}

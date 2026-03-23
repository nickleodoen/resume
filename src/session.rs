// Manages the session log: reading/writing SessionEvents to session.json.
// Session data is stored centrally at ~/.resume/projects/<mirrored-cwd>/ so
// that a globally-installed `resume` binary never scatters data across repos.
// Completed sessions are archived to the per-project sessions/ subdir (last 10 kept).
// A local .resume/.active sentinel file is created when a session starts so the
// shell hook can do a cheap stat check without spawning an extra process.
// Events are appended so no data is lost between watch cycles.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::path::PathBuf;

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "snake_case")]
pub enum EventType {
    FileChange,
    GitDiff,
    Command,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SessionEvent {
    pub timestamp: DateTime<Utc>,
    pub event_type: EventType,
    pub content: String,
}

impl SessionEvent {
    pub fn new(event_type: EventType, content: impl Into<String>) -> Self {
        Self {
            timestamp: Utc::now(),
            event_type,
            content: content.into(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Session {
    pub project: String,
    pub started_at: DateTime<Utc>,
    pub events: Vec<SessionEvent>,
}

impl Session {
    fn new(project: impl Into<String>) -> Self {
        Self {
            project: project.into(),
            started_at: Utc::now(),
            events: Vec::new(),
        }
    }
}


/// Central per-project data directory: ~/.resume/projects/<mirrored-cwd>/
/// e.g. /Users/alice/code/myapp  →  ~/.resume/projects/Users/alice/code/myapp/
fn resume_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().context("could not find home directory")?;
    let cwd = std::env::current_dir().context("failed to get current directory")?;
    // Strip leading "/" so the mirror path is relative.
    let relative = cwd.strip_prefix("/").unwrap_or(&cwd);
    let dir = home.join(".resume").join("projects").join(relative);
    fs::create_dir_all(&dir).context("failed to create resume project directory")?;
    Ok(dir)
}

/// Local .resume/ directory in the project root — used only for the sentinel file.
fn local_resume_dir() -> Result<PathBuf> {
    let dir = PathBuf::from(".resume");
    fs::create_dir_all(&dir).context("failed to create .resume directory")?;
    Ok(dir)
}

/// .resume/.active — presence indicates an active session; checked by the shell hook.
fn sentinel_path() -> Result<PathBuf> {
    Ok(local_resume_dir()?.join(".active"))
}

fn session_path() -> Result<PathBuf> {
    Ok(resume_dir()?.join("session.json"))
}

fn lock_path() -> Result<PathBuf> {
    Ok(resume_dir()?.join("session.lock"))
}

fn pid_path() -> Result<PathBuf> {
    Ok(resume_dir()?.join("resume.pid"))
}

fn sessions_dir() -> Result<PathBuf> {
    let dir = resume_dir()?.join("sessions");
    fs::create_dir_all(&dir).context("failed to create sessions directory")?;
    Ok(dir)
}

pub fn notes_path() -> Result<PathBuf> {
    Ok(resume_dir()?.join("notes.txt"))
}

/// Archive session.json to .resume/sessions/{started_at}.json if it has events.
fn archive_current_session() -> Result<()> {
    let path = match session_path() {
        Ok(p) => p,
        Err(_) => return Ok(()),
    };
    if !path.exists() {
        return Ok(());
    }
    let json = match fs::read_to_string(&path) {
        Ok(j) => j,
        Err(_) => return Ok(()),
    };
    let sess: Session = match serde_json::from_str(&json) {
        Ok(s) => s,
        Err(_) => return Ok(()),
    };
    if sess.events.is_empty() {
        return Ok(());
    }
    let name = sess.started_at.format("%Y-%m-%dT%H-%M-%S").to_string();
    let dest = sessions_dir()?.join(format!("{name}.json"));
    fs::copy(&path, &dest).context("failed to archive session")?;
    Ok(())
}

/// Delete oldest session archives, keeping only the `keep` most recent.
fn prune_archives(keep: usize) -> Result<()> {
    let dir = sessions_dir()?;
    let mut entries: Vec<PathBuf> = fs::read_dir(&dir)
        .context("failed to read sessions dir")?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
        .collect();
    entries.sort(); // ISO timestamp filenames sort lexicographically = chronologically
    if entries.len() > keep {
        for old in &entries[..entries.len() - keep] {
            let _ = fs::remove_file(old);
        }
    }
    Ok(())
}

/// Load the most recent session that has events.
/// Checks the current session first, then archives newest-first.
pub fn load_latest() -> Result<Session> {
    if let Ok(sess) = load() {
        if !sess.events.is_empty() {
            return Ok(sess);
        }
    }
    let dir = sessions_dir()?;
    let mut entries: Vec<PathBuf> = fs::read_dir(&dir)
        .context("failed to read sessions dir")?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
        .collect();
    entries.sort();
    for path in entries.into_iter().rev() {
        if let Ok(json) = fs::read_to_string(&path) {
            if let Ok(sess) = serde_json::from_str::<Session>(&json) {
                if !sess.events.is_empty() {
                    return Ok(sess);
                }
            }
        }
    }
    anyhow::bail!("No sessions with events found. Run `resume` to start a session.");
}

/// Acquire an exclusive lock on .resume/session.lock, call f(), then release.
fn with_session_lock<F, T>(f: F) -> Result<T>
where
    F: FnOnce() -> Result<T>,
{
    let lock_file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(lock_path()?)
        .context("failed to open session lock file")?;
    lock_file
        .lock_exclusive()
        .context("failed to acquire session lock")?;
    let result = f();
    let _ = lock_file.unlock();
    result
}

pub fn write_pid(pid: u32) -> Result<()> {
    fs::write(pid_path()?, pid.to_string()).context("failed to write PID file")?;
    Ok(())
}

pub fn read_pid() -> Result<Option<u32>> {
    let path = pid_path()?;
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(&path).context("failed to read PID file")?;
    let pid: u32 = raw.trim().parse().context("PID file contains invalid data")?;
    Ok(Some(pid))
}

pub fn clear_pid() -> Result<()> {
    let path = pid_path()?;
    if path.exists() {
        fs::remove_file(path).context("failed to remove PID file")?;
    }
    Ok(())
}

/// Create .resume/.active so the shell hook knows a session is live.
pub fn create_sentinel() -> Result<()> {
    fs::write(sentinel_path()?, b"").context("failed to create session sentinel")?;
    Ok(())
}

/// Remove .resume/.active when the session ends.
pub fn clear_sentinel() {
    if let Ok(p) = sentinel_path() {
        let _ = fs::remove_file(p);
    }
}

/// Archive any existing session, then create a fresh one for the current directory.
pub fn init() -> Result<()> {
    archive_current_session()?;
    prune_archives(10)?;

    let project = std::env::current_dir()
        .context("failed to get cwd")?
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "unknown".to_string());

    let sess = Session::new(project);
    let path = session_path()?;
    let json = serde_json::to_string_pretty(&sess)?;
    fs::write(&path, json).context("failed to write session.json")?;
    create_sentinel()?;
    Ok(())
}

/// Load all notes for the current project as raw text.
pub fn load_notes_text() -> Result<String> {
    let path = notes_path()?;
    if !path.exists() {
        return Ok(String::new());
    }
    fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))
}

/// Append a line to the notes file for the current project.
pub fn append_note(text: &str) -> Result<()> {
    let path = notes_path()?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .context("failed to open notes.txt")?;
    use std::io::Write;
    writeln!(file, "{}", text).context("failed to write note")?;
    Ok(())
}

/// Remove all notes for the current project.
pub fn clear_notes() -> Result<()> {
    let path = notes_path()?;
    if path.exists() {
        fs::remove_file(&path).context("failed to remove notes.txt")?;
    }
    Ok(())
}

/// Load the current session from disk.
/// If session.json is corrupt, prints a warning, overwrites with a fresh session, and returns it.
pub fn load() -> Result<Session> {
    let path = session_path()?;
    if !path.exists() {
        anyhow::bail!("no session found at {}. Run `resume` to start a session.", path.display());
    }
    let json = fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    match serde_json::from_str::<Session>(&json) {
        Ok(sess) => Ok(sess),
        Err(_) => {
            eprintln!("warn: session.json is corrupt, starting fresh");
            let project = std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("unknown"))
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "unknown".to_string());
            let sess = Session::new(project);
            let fresh_json = serde_json::to_string_pretty(&sess)?;
            fs::write(&path, fresh_json).context("failed to overwrite corrupt session.json")?;
            Ok(sess)
        }
    }
}

/// Append a single event to the session on disk (protected by an exclusive file lock).
pub fn append_event(event: SessionEvent) -> Result<()> {
    with_session_lock(|| {
        let mut sess = load()?;
        sess.events.push(event);
        let path = session_path()?;
        let json = serde_json::to_string_pretty(&sess)?;
        fs::write(path, json)?;
        Ok(())
    })
}

/// Append a shell command event. No-op (silent) if no session is active.
pub fn log_command(cmd: &str) -> Result<()> {
    // Fast sentinel check — avoids touching the central store when no session is live.
    if !PathBuf::from(".resume/.active").exists() {
        return Ok(());
    }
    let event = SessionEvent::new(EventType::Command, cmd);
    append_event(event)
}

/// Print a human-readable summary of captured events.
pub fn print_status(sess: &Session) {
    println!("Project: {}", sess.project);
    println!("Started: {}", sess.started_at.format("%Y-%m-%d %H:%M:%S UTC"));
    println!("Events:  {}", sess.events.len());
    println!();
    for event in &sess.events {
        let kind = match event.event_type {
            EventType::FileChange => "FILE",
            EventType::GitDiff => "GIT ",
            EventType::Command => "CMD ",
        };
        println!("[{}] {} — {}", kind, event.timestamp.format("%H:%M:%S"), event.content);
    }
}

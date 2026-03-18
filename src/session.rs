// Manages the session log: reading/writing SessionEvents to .resume/session.json.
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

fn resume_dir() -> Result<PathBuf> {
    let dir = PathBuf::from(".resume");
    fs::create_dir_all(&dir).context("failed to create .resume directory")?;
    Ok(dir)
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

/// Create a fresh session for the current directory. Overwrites any existing session.
pub fn init() -> Result<()> {
    let project = std::env::current_dir()
        .context("failed to get cwd")?
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "unknown".to_string());

    let sess = Session::new(project);
    let path = session_path()?;
    let json = serde_json::to_string_pretty(&sess)?;
    fs::write(&path, json).context("failed to write session.json")?;
    Ok(())
}

/// Load the current session from disk.
/// If session.json is corrupt, prints a warning, overwrites with a fresh session, and returns it.
pub fn load() -> Result<Session> {
    let path = session_path()?;
    if !path.exists() {
        anyhow::bail!("no session found at {}. Run `resume start` first.", path.display());
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

/// Mark the session as closed (prints a summary). No-op if no session exists.
pub fn close() -> Result<()> {
    let path = match session_path() {
        Ok(p) => p,
        Err(_) => {
            println!("No session found.");
            return Ok(());
        }
    };
    if !path.exists() {
        println!("No session found.");
        return Ok(());
    }
    let sess = load()?;
    println!(
        "Session closed. {} event(s) recorded since {}.",
        sess.events.len(),
        sess.started_at.format("%Y-%m-%d %H:%M:%S UTC")
    );
    Ok(())
}

/// Append a shell command event. No-op (silent) if no session is active.
pub fn log_command(cmd: &str) -> Result<()> {
    if !PathBuf::from(".resume/session.json").exists() {
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

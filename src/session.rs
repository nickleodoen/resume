// Manages the session log: reading/writing SessionEvents to .resume/session.json.
// Events are appended so no data is lost between watch cycles.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
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

fn session_path() -> Result<PathBuf> {
    let dir = PathBuf::from(".resume");
    fs::create_dir_all(&dir).context("failed to create .resume directory")?;
    Ok(dir.join("session.json"))
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
    println!("Session started for project '{}' — logging to {}", sess.project, path.display());
    Ok(())
}

/// Load the current session from disk. Fails if no session has been started.
pub fn load() -> Result<Session> {
    let path = session_path()?;
    let json = fs::read_to_string(&path)
        .with_context(|| format!("no session found at {}. Run `resume start` first.", path.display()))?;
    let sess: Session = serde_json::from_str(&json).context("failed to parse session.json")?;
    Ok(sess)
}

/// Append a single event to the session on disk.
pub fn append_event(event: SessionEvent) -> Result<()> {
    let mut sess = load()?;
    sess.events.push(event);
    let path = session_path()?;
    let json = serde_json::to_string_pretty(&sess)?;
    fs::write(path, json)?;
    Ok(())
}

/// Mark the session as closed (currently just confirms it's saved).
pub fn close() -> Result<()> {
    let sess = load()?;
    println!(
        "Session closed. {} event(s) recorded since {}.",
        sess.events.len(),
        sess.started_at.format("%Y-%m-%d %H:%M:%S UTC")
    );
    Ok(())
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

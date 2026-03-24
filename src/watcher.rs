// File system watcher using the `notify` crate. Watches the current directory
// recursively and logs FileChange events to the session. Skips noise directories.
// On each file change, checks if HEAD moved and logs the new commit message.
// Duplicate events for the same path within DEBOUNCE_SECS are suppressed.

use anyhow::{Context, Result};
use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::HashMap;
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};
use tokio::signal;
use tokio::sync::oneshot;

use crate::git;
use crate::session::{append_event, EventType, SessionEvent};

/// Suppress duplicate events for the same path within this window.
const DEBOUNCE_SECS: u64 = 2;

const IGNORED_DIRS: &[&str] = &[".git", "target", "node_modules", ".resume"];

/// Filename suffixes and patterns that are always noise (editor temps, swap files, etc.).
const NOISE_SUFFIXES: &[&str] = &[".swp", ".swo", ".swn", "~", ".DS_Store"];
const NOISE_CONTAINS: &[&str] = &[".tmp.", ".temp."];
const NOISE_PREFIXES: &[&str] = &[".#"]; // emacs lock files

fn is_ignored_dir(path: &Path) -> bool {
    path.components().any(|c| {
        IGNORED_DIRS
            .iter()
            .any(|ignored| c.as_os_str() == *ignored)
    })
}

fn is_noise_file(path: &Path) -> bool {
    let name = match path.file_name().and_then(|n| n.to_str()) {
        Some(n) => n,
        None => return false,
    };
    NOISE_SUFFIXES.iter().any(|s| name.ends_with(s))
        || NOISE_CONTAINS.iter().any(|s| name.contains(s))
        || NOISE_PREFIXES.iter().any(|s| name.starts_with(s))
}

fn is_filtered(path: &Path) -> bool {
    is_ignored_dir(path) || is_noise_file(path)
}

/// Build a relative-path description string for a notify event.
fn describe_event(event: &Event, cwd: &Path) -> Option<String> {
    match event.kind {
        EventKind::Create(_) | EventKind::Modify(_) => {
            let paths: Vec<String> = event
                .paths
                .iter()
                .filter(|p| !is_filtered(p))
                .map(|p| {
                    p.strip_prefix(cwd)
                        .unwrap_or(p)
                        .display()
                        .to_string()
                })
                .collect();
            if paths.is_empty() {
                None
            } else {
                Some(paths.join(", "))
            }
        }
        _ => None,
    }
}

/// Check if HEAD moved (new commit). If so, log the commit message as a GitDiff event.
fn check_for_new_commit(
    cwd: &Path,
    last_head: &mut String,
    ui_tx: Option<&tokio::sync::mpsc::UnboundedSender<SessionEvent>>,
) {
    let current_head = git::head_hash(cwd);
    if current_head.is_empty() || current_head == *last_head {
        return;
    }
    *last_head = current_head;
    let message = git::latest_commit_message(cwd);
    if message.is_empty() {
        return;
    }
    let ev = SessionEvent::new(EventType::GitDiff, message);
    if let Err(e) = append_event(ev.clone()) {
        eprintln!("warn: failed to log commit: {e}");
    }
    if let Some(tx) = ui_tx {
        let _ = tx.send(ev);
    }
}

/// Start watching the current directory. Runs until SIGINT (Ctrl-C), SIGTERM, or shutdown signal.
pub async fn watch(
    ui_tx: Option<tokio::sync::mpsc::UnboundedSender<SessionEvent>>,
    shutdown: Option<oneshot::Receiver<()>>,
) -> Result<()> {
    let cwd = std::env::current_dir().context("failed to get cwd")?;

    let (tx, rx) = mpsc::channel::<notify::Result<Event>>();

    let mut watcher =
        RecommendedWatcher::new(tx, Config::default().with_poll_interval(Duration::from_secs(1)))
            .context("failed to create file watcher")?;

    watcher
        .watch(&cwd, RecursiveMode::Recursive)
        .context("failed to watch directory")?;

    let cwd_clone = cwd.clone();
    let ui_tx_clone = ui_tx.clone();

    tokio::task::spawn_blocking(move || {
        let mut last_head = git::head_hash(&cwd_clone);
        // Maps path string → last time it was logged.
        let mut last_seen: HashMap<String, Instant> = HashMap::new();

        for res in rx {
            match res {
                Ok(event) => {
                    if let Some(desc) = describe_event(&event, &cwd_clone) {
                        // Debounce: skip if this path was logged within the window.
                        let now = Instant::now();
                        if let Some(last) = last_seen.get(&desc)
                            && now.duration_since(*last) < Duration::from_secs(DEBOUNCE_SECS) {
                                continue;
                            }
                        last_seen.insert(desc.clone(), now);

                        let ev = SessionEvent::new(EventType::FileChange, desc);
                        if let Err(e) = append_event(ev.clone()) {
                            eprintln!("warn: failed to log event: {e}");
                        }
                        if let Some(ref tx) = ui_tx_clone {
                            let _ = tx.send(ev);
                        }

                        // Check for a new commit on every file change.
                        check_for_new_commit(&cwd_clone, &mut last_head, ui_tx_clone.as_ref());
                    }
                }
                Err(e) => eprintln!("watch error: {e}"),
            }
        }
    });

    // Wait for Ctrl-C, SIGTERM, or TUI shutdown signal.
    #[cfg(unix)]
    {
        let mut sigterm = tokio::signal::unix::signal(
            tokio::signal::unix::SignalKind::terminate(),
        )
        .context("failed to listen for sigterm")?;

        if let Some(shutdown_rx) = shutdown {
            tokio::select! {
                _ = signal::ctrl_c() => {},
                _ = sigterm.recv() => {},
                _ = shutdown_rx => {},
            }
        } else {
            tokio::select! {
                _ = signal::ctrl_c() => {},
                _ = sigterm.recv() => {},
            }
        }
    }
    #[cfg(not(unix))]
    {
        if let Some(shutdown_rx) = shutdown {
            tokio::select! {
                _ = signal::ctrl_c() => {},
                _ = shutdown_rx => {},
            }
        } else {
            signal::ctrl_c().await.context("failed to listen for ctrl-c")?;
        }
    }

    Ok(())
}

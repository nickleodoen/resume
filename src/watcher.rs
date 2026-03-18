// File system watcher using the `notify` crate. Watches the current directory
// recursively and logs FileChange events to the session. Skips noise directories.
// Every GIT_DIFF_INTERVAL distinct file changes, also captures a git diff snapshot.
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

const GIT_DIFF_INTERVAL: u32 = 5;

/// Suppress duplicate events for the same path within this window.
const DEBOUNCE_SECS: u64 = 2;

/// Maximum bytes to store for a single git diff snapshot.
const MAX_DIFF_BYTES: usize = 8_000;

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

/// Truncate a diff to at most `max` bytes, appending a note if truncated.
fn truncate_diff(diff: String, max: usize) -> String {
    if diff.len() <= max {
        return diff;
    }
    let total = diff.len();
    // Truncate at a UTF-8 boundary.
    let mut end = max;
    while !diff.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n[diff truncated — {total} bytes total]", &diff[..end])
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

fn log_git_diff(
    cwd: &Path,
    last_diff: &mut String,
    ui_tx: Option<&tokio::sync::mpsc::UnboundedSender<SessionEvent>>,
) {
    match git::current_diff(cwd) {
        Ok(diff) if !diff.is_empty() && diff != *last_diff => {
            let stored = truncate_diff(diff.clone(), MAX_DIFF_BYTES);
            let ev = SessionEvent::new(EventType::GitDiff, stored);
            if let Err(e) = append_event(ev.clone()) {
                eprintln!("warn: failed to log git diff: {e}");
            }
            if let Some(tx) = ui_tx {
                let _ = tx.send(ev);
            }
            *last_diff = diff;
        }
        Err(e) => eprintln!("warn: failed to capture git diff: {e}"),
        _ => {}
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
        let mut file_change_count: u32 = 0;
        let mut last_diff = String::new();
        // Maps path string → last time it was logged.
        let mut last_seen: HashMap<String, Instant> = HashMap::new();

        for res in rx {
            match res {
                Ok(event) => {
                    if let Some(desc) = describe_event(&event, &cwd_clone) {
                        // Debounce: skip if this path was logged within the window.
                        let now = Instant::now();
                        if let Some(last) = last_seen.get(&desc) {
                            if now.duration_since(*last) < Duration::from_secs(DEBOUNCE_SECS) {
                                continue;
                            }
                        }
                        last_seen.insert(desc.clone(), now);

                        let ev = SessionEvent::new(EventType::FileChange, desc);
                        if let Err(e) = append_event(ev.clone()) {
                            eprintln!("warn: failed to log event: {e}");
                        }
                        if let Some(ref tx) = ui_tx_clone {
                            let _ = tx.send(ev);
                        }

                        file_change_count += 1;
                        if file_change_count % GIT_DIFF_INTERVAL == 0 {
                            log_git_diff(&cwd_clone, &mut last_diff, ui_tx_clone.as_ref());
                        }
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

    // Final diff snapshot on shutdown.
    let shutdown_cwd = std::env::current_dir().unwrap_or_default();
    if let Ok(diff) = crate::git::current_diff(&shutdown_cwd) {
        if !diff.is_empty() {
            let stored = truncate_diff(diff, MAX_DIFF_BYTES);
            let ev = SessionEvent::new(EventType::GitDiff, stored);
            if let Some(ref tx) = ui_tx {
                let _ = tx.send(ev.clone());
            }
            let _ = append_event(ev);
        }
    }

    Ok(())
}

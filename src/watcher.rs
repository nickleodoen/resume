// File system watcher using the `notify` crate. Watches the current directory
// recursively and logs FileChange events to the session. Skips noise directories.
// Every GIT_DIFF_INTERVAL file changes, also captures a git diff snapshot.

use anyhow::{Context, Result};
use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;
use tokio::signal;

use crate::git;
use crate::session::{append_event, EventType, SessionEvent};

/// Capture a git diff and log it, unless it's empty or identical to the last one logged.
const GIT_DIFF_INTERVAL: u32 = 5;

const IGNORED_DIRS: &[&str] = &[".git", "target", "node_modules", ".resume"];

fn is_ignored(path: &Path) -> bool {
    path.components().any(|c| {
        IGNORED_DIRS
            .iter()
            .any(|ignored| c.as_os_str() == *ignored)
    })
}

fn describe_event(event: &Event) -> Option<String> {
    match event.kind {
        EventKind::Create(_) | EventKind::Modify(_) => {
            let paths: Vec<String> = event
                .paths
                .iter()
                .filter(|p| !is_ignored(p))
                .map(|p| p.display().to_string())
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

fn log_git_diff(cwd: &Path, last_diff: &mut String) {
    match git::current_diff(cwd) {
        Ok(diff) if !diff.is_empty() && diff != *last_diff => {
            let ev = SessionEvent::new(EventType::GitDiff, diff.clone());
            if let Err(e) = append_event(ev) {
                eprintln!("warn: failed to log git diff: {e}");
            }
            *last_diff = diff;
        }
        Err(e) => eprintln!("warn: failed to capture git diff: {e}"),
        _ => {}
    }
}

/// Start watching the current directory. Runs until SIGINT (Ctrl-C).
pub async fn watch() -> Result<()> {
    let cwd = std::env::current_dir().context("failed to get cwd")?;
    println!("Watching {} — press Ctrl-C to stop.", cwd.display());

    let (tx, rx) = mpsc::channel::<notify::Result<Event>>();

    let mut watcher = RecommendedWatcher::new(tx, Config::default().with_poll_interval(Duration::from_secs(1)))
        .context("failed to create file watcher")?;

    watcher
        .watch(&cwd, RecursiveMode::Recursive)
        .context("failed to watch directory")?;

    // Clone cwd for use inside the blocking thread.
    let cwd_clone = cwd.clone();

    // Spawn a blocking thread to drain the sync receiver and log events.
    tokio::task::spawn_blocking(move || {
        let mut file_change_count: u32 = 0;
        let mut last_diff = String::new();

        for res in rx {
            match res {
                Ok(event) => {
                    if let Some(desc) = describe_event(&event) {
                        let ev = SessionEvent::new(EventType::FileChange, desc);
                        if let Err(e) = append_event(ev) {
                            eprintln!("warn: failed to log event: {e}");
                        }

                        file_change_count += 1;
                        if file_change_count % GIT_DIFF_INTERVAL == 0 {
                            log_git_diff(&cwd_clone, &mut last_diff);
                        }
                    }
                }
                Err(e) => eprintln!("watch error: {e}"),
            }
        }
    });

    // Block until Ctrl-C.
    signal::ctrl_c().await.context("failed to listen for ctrl-c")?;
    println!("\nWatcher stopped.");
    Ok(())
}

// File system watcher using the `notify` crate. Watches the current directory
// recursively and logs FileChange events to the session. Skips noise directories.

use anyhow::{Context, Result};
use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;
use tokio::signal;

use crate::session::{append_event, EventType, SessionEvent};

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

    // Spawn a blocking thread to drain the sync receiver and log events.
    tokio::task::spawn_blocking(move || {
        for res in rx {
            match res {
                Ok(event) => {
                    if let Some(desc) = describe_event(&event) {
                        let ev = SessionEvent::new(EventType::FileChange, desc);
                        if let Err(e) = append_event(ev) {
                            eprintln!("warn: failed to log event: {e}");
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

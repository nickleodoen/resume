// Git integration using the `git2` crate. Captures the current working diff
// (staged + unstaged changes) and returns it as a string for session logging.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use git2::{DiffOptions, Repository, Sort};

/// Return the current HEAD commit hash, or empty string if none.
pub fn head_hash(path: &std::path::Path) -> String {
    let repo = match Repository::discover(path) {
        Ok(r) => r,
        Err(_) => return String::new(),
    };
    repo.head()
        .ok()
        .and_then(|h| h.peel_to_commit().ok())
        .map(|c| c.id().to_string())
        .unwrap_or_default()
}

/// Return the summary line of the latest commit, or empty string if none.
pub fn latest_commit_message(path: &std::path::Path) -> String {
    let repo = match Repository::discover(path) {
        Ok(r) => r,
        Err(_) => return String::new(),
    };
    repo.head()
        .ok()
        .and_then(|h| h.peel_to_commit().ok())
        .and_then(|c| c.summary().map(|s| s.to_string()))
        .unwrap_or_default()
}

/// Return commits made since `since`, oldest-first, as (short_hash, HH:MM, subject) tuples.
/// Returns an empty vec if the repo has no commits, is unreachable, or `since` is in the future.
pub fn git_log_since(path: &std::path::Path, since: &DateTime<Utc>) -> Vec<(String, String, String)> {
    let repo = match Repository::discover(path) {
        Ok(r) => r,
        Err(_) => return vec![],
    };
    let mut revwalk = match repo.revwalk() {
        Ok(r) => r,
        Err(_) => return vec![],
    };
    if revwalk.push_head().is_err() {
        return vec![];
    }
    revwalk.set_sorting(Sort::TIME).ok();

    let since_ts = since.timestamp();
    let mut commits = Vec::new();
    for oid in revwalk.flatten() {
        let commit = match repo.find_commit(oid) {
            Ok(c) => c,
            Err(_) => continue,
        };
        if commit.time().seconds() < since_ts {
            break; // revwalk is newest-first; stop once we're before the session
        }
        let hash = commit.id().to_string()[..7].to_string();
        let time = DateTime::from_timestamp(commit.time().seconds(), 0)
            .map(|t| t.format("%H:%M").to_string())
            .unwrap_or_default();
        let subject = commit.summary().unwrap_or("(no message)").to_string();
        commits.push((hash, time, subject));
    }
    commits.reverse(); // return oldest-first
    commits
}

/// Return a unified diff of all uncommitted changes in the repo at `path`.
/// Returns an empty string if there is no repo or no changes.
pub fn current_diff(path: &std::path::Path) -> Result<String> {
    let repo = match Repository::discover(path) {
        Ok(r) => r,
        Err(_) => return Ok(String::new()), // not a git repo — silently skip
    };

    let mut opts = DiffOptions::new();
    opts.ignore_whitespace(false);

    // Diff HEAD → workdir (includes both staged and unstaged)
    let diff = match repo.head() {
        Ok(head) => {
            let tree = head
                .peel_to_tree()
                .context("failed to peel HEAD to tree")?;
            repo.diff_tree_to_workdir_with_index(Some(&tree), Some(&mut opts))
                .context("failed to compute diff")?
        }
        Err(_) => {
            // No commits yet — diff empty tree to workdir
            repo.diff_index_to_workdir(None, Some(&mut opts))
                .context("failed to compute diff for empty repo")?
        }
    };

    let mut output = String::new();
    diff.print(git2::DiffFormat::Patch, |_delta, _hunk, line| {
        let origin = line.origin();
        if matches!(origin, '+' | '-' | ' ') {
            output.push(origin);
        }
        if let Ok(s) = std::str::from_utf8(line.content()) {
            output.push_str(s);
        }
        true
    })
    .context("failed to format diff")?;

    Ok(output)
}

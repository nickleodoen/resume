// Git integration using the `git2` crate. Captures the current working diff
// (staged + unstaged changes) and returns it as a string for session logging.

use anyhow::{Context, Result};
use git2::{DiffOptions, Repository};

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

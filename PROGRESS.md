# PROGRESS.md — Human Changelog

---

## 2026-03-16 — Session 1: Initial Scaffold

### What Was Built
- Full project scaffold from scratch
- `Cargo.toml` with all required dependencies
- `src/main.rs` — clap-based CLI with 4 subcommands: `start`, `stop`, `show`, `status`
- `src/session.rs` — core data model (`SessionEvent`, `Session`) + disk I/O to `.resume/session.json`
- `src/watcher.rs` — async file watcher via `notify`, ignores irrelevant dirs
- `src/git.rs` — git diff capture via `git2`
- `src/summarize.rs` — Anthropic Messages API integration for LLM briefing
- `CLAUDE.md` — project context file for AI-assisted development
- `README.md` — project description

### What Changed
- First session; everything is new.

### What's Next
1. Wire `git::current_diff()` into the watcher so git state is captured alongside file changes
2. Add a background daemon mode (PID file + detach) so `resume start` doesn't block the terminal
3. Shell command capture via zsh hooks
4. `resume init` subcommand to set up `.gitignore` entry

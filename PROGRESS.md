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

---

## 2026-03-18 — Session 2: Git Diffs, Daemon Mode, Shell Hooks

### What Was Built
- Git diff capture wired into watcher loop (every 5 file changes, deduped, empty diffs skipped)
- Background daemon mode: `resume start` now detaches to background via `spawn_daemon()`, writes PID file
- Shell command capture: hidden `resume log <cmd>` subcommand; shell hook appends commands to session
- `resume init` / `resume init --install-hook`: sets up `.gitignore` entry and shell hook (zsh)
- Event deduplication: 2-second debounce per path, noise filters for editor temp files

---

## 2026-03-18 — Session 3: Production Hardening

### What Was Built
- File locking via `fs2`: concurrent writers (daemon + shell hook) can't corrupt `session.json`
- Relative paths in session events: `src/main.rs` instead of `/Users/.../src/main.rs`
- Large diff truncation: diffs capped at 8KB with a size note appended
- SIGTERM handling: watcher captures a final git diff before shutdown
- Bash hook support: `resume init --install-hook` detects `$SHELL` and writes to the right rc file
- Graceful no-session handling: `resume stop`/`resume status` print friendly messages instead of erroring
- Corrupt session recovery: bad JSON is overwritten with a fresh session instead of crashing

---

## 2026-03-18 — Session 4: TUI Mode

### What Was Built
- `src/tui.rs` — live ratatui/crossterm dashboard; `resume` (no args) launches it
- `finish` shell function added to both hook constants
- Clean watcher shutdown architecture: TUI sends oneshot signal on Ctrl-C
- VS Code integration: `.vscode/` tasks, truecolor env, font config

---

## 2026-03-18 — Session 5: Resy + Visual Polish

### What Was Built
- Resy the jellyfish: pixel-art mascot rendered as a colored 9×7 block grid in the TUI
- RESUME banner: replaced ambiguous Unicode block characters with ASCII-only figlet art
- Truecolor palette: `Color::Rgb()` values; VS Code task sets `COLORTERM=truecolor`

---

## 2026-03-23 — Session 6: Central Storage + Notes

### What Was Built
- Central per-project storage: all session data now lives under `~/.resume/projects/<path>/`
- `.resume/.active` sentinel file: shell hook uses a cheap stat instead of reading session JSON
- Shell hooks updated to check `.resume/.active`
- `resume note <text>` command: timestamped notes saved to `notes.json`, persist across sessions
- Notes included in briefing as high-priority context

---

## 2026-03-24 — Session 7: Briefing Quality + Cleanup

### What Was Built
- Claude Code conversation context: reads `~/.claude/history.jsonl` filtered by project and session time, adds recent user messages as briefing context to reveal developer intent
- Notes removed from briefing context: git diffs promoted to highest priority
- Briefing normalization: post-processes model output to strip blank lines after section headers, ensuring uniform layout across all models
- Filename annotations in briefing: progress bullets and next-step now include `(src/file.rs)` and `(+N more)` pills
- Resy recolored red
- Zero clippy warnings: all 10 warnings fixed; `LogCommand` enum variant renamed to `Log`
- All comments, CLAUDE.md, PROGRESS.md, and README.md brought up to date for packaging

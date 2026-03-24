# CLAUDE.md — Project Context File

> READ THIS AT THE START OF EVERY SESSION. UPDATE IT AT THE END.

## What This Project Is

`resume` is a Rust CLI tool that solves developer context-loss. It passively records
what you do during a coding session (file changes, git diffs, shell commands) and
stores them under `~/.resume/projects/<project-path>/`. When you return, `resume show`
calls an LLM (Anthropic or local Ollama) and prints a plain-English briefing: what you
were working on, where you left off, and what to do next.

## Architecture

```
src/
├── main.rs       — clap CLI, dispatches to subcommands
├── session.rs    — SessionEvent / Session structs, central storage I/O
├── watcher.rs    — notify-based file watcher, logs FileChange + GitDiff events
├── git.rs        — git2-based diff capture and commit log helpers
├── summarize.rs  — LLM prompt builder + Anthropic/Ollama API calls
├── models.rs     — model registry (Anthropic + Ollama), provider dispatch
└── tui.rs        — ratatui/crossterm live event dashboard (default `resume` mode)
```

### Data Flow

```
resume (no args)
  → tui::run()              calls session::init(), spawns watcher::watch(Some(tx), Some(shutdown_rx))
                            renders live ratatui dashboard; Ctrl-C sends shutdown signal and exits

resume start
  → session::init()         creates central session file, writes .resume/.active sentinel
  → spawn_daemon()          re-execs with hidden --daemon flag, detaches to background
  → watcher::watch()        loops until SIGTERM, appending FileChange/GitDiff events

resume stop
  → kills daemon PID        sends SIGTERM via `kill`, reads PID from central store
  → session::clear_pid()    removes .resume/resume.pid
  → session::clear_sentinel() removes .resume/.active

resume show
  → session::load_latest()
  → summarize::generate()   builds prompt from diff + commits + Claude Code history,
                            POSTs to Anthropic or Ollama, prints briefing

resume status
  → session::load_latest()
  → session::print_status() pretty-prints all events

resume note <text>
  → session::append_note()  appends timestamped note to notes.json (persists across sessions)
```

### Session Storage

Session data lives in `~/.resume/projects/<mirrored-project-path>/`:
- `session.json` — current session events
- `notes.json`   — persistent developer notes (never archived)
- `session.lock` — advisory lock for concurrent writers
- `resume.pid`   — daemon PID (present only while daemon is running)

A lightweight sentinel `.resume/.active` is written to the **project directory** so the
shell hook can cheaply check if a session is active without knowing the central store path.

```json
{
  "project": "resume",
  "started_at": "2026-03-16T00:00:00Z",
  "events": [
    { "timestamp": "...", "event_type": "file_change", "content": "src/main.rs" }
  ]
}
```

## What Was Built — Session 1 (2026-03-16)

- Cargo.toml with all dependencies (notify, git2, serde, tokio, reqwest, clap, chrono, anyhow, dirs)
- `main.rs` — clap CLI with `start`, `stop`, `show`, `status` subcommands
- `session.rs` — SessionEvent/Session structs + init/load/append_event/print_status
- `watcher.rs` — recursive notify watcher, ignores .git/target/node_modules/.resume
- `git.rs` — git2 diff capture (HEAD → workdir)
- `summarize.rs` — Anthropic Messages API call using `claude-haiku-4-5-20251001`

## What Was Built — Session 2 (2026-03-18)

- Wired `git::current_diff()` into the watcher loop in `watcher.rs`
  - Every `GIT_DIFF_INTERVAL` (5) file changes, a `GitDiff` event is captured and appended
  - Deduplication: identical consecutive diffs are skipped via `last_diff` comparison
  - Empty diffs are skipped (no uncommitted changes)
- Background daemon mode (`resume start` no longer blocks):
  - `resume start` re-execs itself with a hidden `--daemon` flag via `spawn_daemon()`
  - Child process is placed in its own process group (`process_group(0)`) so Ctrl-C doesn't kill it
  - PID written to central store; `resume stop` sends SIGTERM and clears it
- Shell command capture + `resume init`:
  - Hidden `resume log <cmd>` subcommand appends `Command` events; no-op if no session active
  - `resume init` adds `.resume/` to `.gitignore` (idempotent) and prints the shell hook snippet
  - `resume init --install-hook` auto-appends the hook to `~/.zshrc` or `~/.bashrc` (idempotent)
- Event deduplication + noise filtering:
  - `DEBOUNCE_SECS = 2`: same path logged within 2 seconds is suppressed
  - `NOISE_SUFFIXES`, `NOISE_CONTAINS`, `NOISE_PREFIXES` filter editor temp files and lock files

## What Was Built — Session 3 (2026-03-18)

1. **File locking** (`session.rs`): `with_session_lock()` wraps all `append_event` calls with `fs2` exclusive file locks so daemon + shell hook can't clobber each other.
2. **Relative paths in session.json** (`watcher.rs`): paths stored as `src/main.rs` not `/Users/.../src/main.rs`.
3. **Truncate large git diffs** (`watcher.rs`): `MAX_DIFF_BYTES = 8_000`; oversized diffs are cut at a newline boundary with a size note appended.
4. **SIGTERM handling + final diff on shutdown** (`watcher.rs`): `tokio::select!` on both `ctrl_c` and `sigterm`; captures a final git diff before stopping.
5. **Bash hook support** (`main.rs`): detects `$SHELL`, writes to `~/.zshrc` (zsh) or `~/.bashrc` (bash).
6. **Graceful no-session handling** (`session.rs`, `main.rs`): `resume stop`/`resume status` print friendly messages instead of erroring when no session exists.
7. **Corrupt session recovery** (`session.rs`): bad JSON is overwritten with a fresh session rather than crashing.

## Packaging Intent (important — read before distributing)

When this tool is packaged (Homebrew formula, cargo-install, installer script, etc.), the shell hook
setup **must be surfaced prominently**. The hook is what makes `resume show` actually useful —
without it, the session only contains file events, not the commands that caused them.

**Recommended packaging approach:**
- Post-install message: *"Run `resume init --install-hook` to finish setup, then open a new terminal."*
- Homebrew caveats block is the right place for this on macOS
- Any `curl | sh` installer should offer to run `resume init --install-hook` as a final step
- Do NOT silently modify `~/.zshrc` without the user running `resume init --install-hook` themselves

The `--install-hook` flag is idempotent (safe to re-run). Uses `# --- resume shell hook ---` sentinel.

## What Was Built — Session 4 (2026-03-18)

1. **TUI mode** (`src/tui.rs`, `Cargo.toml`): `resume` (no args) launches a live ratatui/crossterm dashboard. Added `ratatui = "0.29"` and `crossterm = "0.28"`.
2. **`finish` shell function** (`main.rs`): both shell hook constants include `finish() { resume stop "$@"; }`.
3. **Watcher signal architecture** (`watcher.rs`): `watch(ui_tx, shutdown_rx)` — TUI sends a oneshot to shut down the watcher cleanly on Ctrl-C.
4. **VS Code integration** (`.vscode/`): truecolor env, font config, three task shortcuts.

## What Was Built — Session 5 (2026-03-18)

1. **Resy redesigned as pixel-art jellyfish** (`src/tui.rs`): 9×7 pixel grid, rendered as colored 2-wide space blocks. Eyes are dark pixels embedded in the body.
2. **RESUME banner fixed** (`src/tui.rs`): replaced block/box Unicode chars (ambiguous double-width in Nerd Fonts) with ASCII-only figlet art using raw strings.
3. **Truecolor palette** (`src/tui.rs`): switched from named ANSI colors to `Color::Rgb()` values. VS Code task sets `COLORTERM=truecolor` so colors render correctly.

## What Was Built — Session 6 (2026-03-23)

1. **Central per-project storage** (`session.rs`): `resume_dir()` mirrors the project's absolute path under `~/.resume/projects/`. Nothing spills into the project repo.
2. **Local sentinel file** (`session.rs`): `.resume/.active` is created/removed on session start/stop. Shell hook checks this file instead of parsing session JSON.
3. **Shell hooks updated** (`main.rs`): both hooks now check `[[ -f .resume/.active ]]`.
4. **`resume note <text>` command** (`main.rs`, `session.rs`): appends a timestamped note to `notes.json` in the central store. Notes persist across sessions and are never archived.

## What Was Built — Session 7 (2026-03-24)

1. **Claude Code conversation context** (`summarize.rs`): `collect_claude_code_messages()` reads `~/.claude/history.jsonl`, filters by current project path and session start time, and includes up to 20 recent user messages as briefing context so the model can infer developer intent.
2. **Notes removed from briefing** (`summarize.rs`, `tui.rs`, `main.rs`): `generate()` no longer accepts or uses notes. Git diffs are now the highest-priority context.
3. **Briefing normalization** (`summarize.rs`): `normalize_briefing()` strips blank lines that models insert after section headers, ensuring uniform output across Haiku, Sonnet, and Ollama.
4. **Filename annotations** (`summarize.rs`): system prompt now instructs the model to append `(src/file.rs)` and `(+N more)` pills to progress bullets and next-step recommendations.
5. **Resy recolored red** (`src/tui.rs`): pixel art palette changed from purple to red tones.
6. **Zero clippy warnings**: all 10 warnings resolved; `LogCommand` enum variant renamed to `Log`.

## Known Issues / Bugs

- Shell hook requires a new terminal or `source ~/.zshrc` / `source ~/.bashrc` after install
- The `last_seen` debounce map in `watcher.rs` grows unbounded for very long sessions (low priority)
- Users who installed the old shell hook (pre-session-6, checking `.resume/session.json`) need to re-run `resume init --install-hook`

## Next Steps (Priority Order)

1. End-to-end packaging test: `cargo install --path .`, `resume init --install-hook`, open new terminal, run `resume`, do work, Ctrl-C, then `resume show`
2. Homebrew formula or install script
3. TUI: show a live git diff panel (split layout) for the most recent diff
4. `resume notes` management: `--clear` and `--delete <n>` flags
5. Periodically evict stale entries from the `last_seen` debounce map (only matters for multi-hour sessions)
6. Write a proper integration test (create temp dir, start session, append events, verify JSON, check lock)

## Key Decisions

| Decision | Rationale |
|---|---|
| `notify` over `kqueue` directly | Cross-platform, maintained, sane API |
| Central storage under `~/.resume/projects/` | Nothing committed to project repo; works across all projects |
| `append_event` reads+writes entire file | Simple; session files are small. Can optimize to append-only JSONL later if needed |
| `claude-haiku-4-5-20251001` default model | Fast and cheap for routine briefings; override with `RESUME_MODEL=` |
| `fs2` for file locking | Minimal dep, cross-platform, POSIX `flock` semantics |
| Corrupt session → overwrite with fresh | Better than crashing; session data is observability not source of truth |
| `MAX_DIFF_BYTES = 8_000` (watcher) / `4_000` (prompt) | Keeps API context lean; full diff rarely needed for a briefing |
| Detect `$SHELL` for hook install | Avoids requiring users to know which rc file to edit |
| Read `~/.claude/history.jsonl` for Claude Code context | Zero-config integration; reveals developer intent without any extra tooling |

# CLAUDE.md — Project Context File

> READ THIS AT THE START OF EVERY SESSION. UPDATE IT AT THE END.

## What This Project Is

`resume` is a Rust CLI tool that solves developer context-loss. It passively records
what you do during a coding session (file changes, git diffs, shell commands) and
stores them in `.resume/session.json`. When you return, `resume show` calls the
Anthropic API and prints a plain-English briefing: what you were working on, where
you left off, and what to do next.

## Architecture

```
src/
├── main.rs       — clap CLI, dispatches to subcommands
├── session.rs    — SessionEvent / Session structs, .resume/session.json read/write
├── watcher.rs    — notify-based file watcher, logs FileChange events
├── git.rs        — git2-based diff capture, returns unified diff string
├── summarize.rs  — Anthropic Messages API call, builds briefing from session log
└── tui.rs        — ratatui/crossterm live event dashboard (default `resume` mode)
```

### Data Flow

```
resume (no args)
  → tui::run()            calls session::init(), spawns watcher::watch(Some(tx), Some(shutdown_rx))
                          renders live ratatui dashboard; Ctrl-C sends shutdown signal and exits

resume start
  → session::init()       creates .resume/session.json
  → watcher::watch()      loops until Ctrl-C, appending FileChange events

resume stop
  → session::close()      prints summary (session stays on disk)

resume show
  → session::load()
  → summarize::generate() POSTs to Anthropic API, prints briefing

resume status
  → session::load()
  → session::print_status() pretty-prints all events
```

### Session Format (`.resume/session.json`)

```json
{
  "project": "resume",
  "started_at": "2026-03-16T00:00:00Z",
  "events": [
    {
      "timestamp": "...",
      "event_type": "file_change",
      "content": "src/main.rs"
    }
  ]
}
```

## What Was Built — Session 1 (2026-03-16)

- Cargo.toml with all dependencies (notify, git2, serde, tokio, reqwest, clap, chrono, anyhow, dirs)
- `main.rs` — clap CLI with `start`, `stop`, `show`, `status` subcommands
- `session.rs` — SessionEvent/Session structs + init/load/append_event/close/print_status
- `watcher.rs` — recursive notify watcher, ignores .git/target/node_modules/.resume
- `git.rs` — git2 diff capture (HEAD → workdir)
- `summarize.rs` — Anthropic Messages API call using `claude-opus-4-6`

## What Was Built — Session 2 (2026-03-18)

- Wired `git::current_diff()` into the watcher loop in `watcher.rs`
  - Every `GIT_DIFF_INTERVAL` (5) file changes, a `GitDiff` event is captured and appended
  - Deduplication: identical consecutive diffs are skipped via `last_diff` comparison
  - Empty diffs are skipped (no uncommitted changes)
  - `log_git_diff()` helper extracted for clarity
- Background daemon mode (`resume start` no longer blocks):
  - `resume start` re-execs itself with a hidden `--daemon` flag via `spawn_daemon()`
  - Child process is placed in its own process group (`process_group(0)`) so Ctrl-C doesn't kill it
  - PID written to `.resume/resume.pid`; read/write/clear helpers in `session.rs`
  - `resume start` checks for a stale/live PID file and bails with a clear error if already running
  - `resume stop` reads the PID, sends SIGTERM via `kill`, clears the PID file, then prints the session summary
  - No new dependencies — uses `kill` via `std::process::Command`
- `cargo build` confirmed clean

- Shell command capture + `resume init` (session 2, continued):
  - Hidden `resume log-command <cmd>` subcommand appends `Command` events; no-op if no session active
  - `resume init` adds `.resume/` to `.gitignore` (idempotent) and prints the zsh hook snippet
  - `resume init --install-hook` auto-appends the hook to `~/.zshrc` (idempotent via sentinel comment)
  - Uses `dirs::home_dir()` to locate `~/.zshrc` cross-platform
  - zsh hook uses `preexec_functions` and `&!` (disown) so it never blocks the shell

## What Was Built — Session 3 (2026-03-18)

Seven production-hardening improvements, all with a clean `cargo build`:

1. **File locking** (`session.rs`, `Cargo.toml`): Added `fs2 = "0.4"`. New `with_session_lock<F,T>()` helper opens/creates `.resume/session.lock`, calls `FileExt::lock_exclusive()`, runs the closure, then unlocks. `append_event` is now wrapped in this lock so concurrent writers (daemon + shell hook) can't clobber each other.

2. **Relative paths in session.json** (`watcher.rs`): `describe_event` now accepts `cwd: &Path` and strips it from each path via `path.strip_prefix(cwd).unwrap_or(path)`. Session events now store `src/main.rs` instead of `/Users/.../src/main.rs`.

3. **Truncate large git diffs** (`watcher.rs`): Added `MAX_DIFF_BYTES = 8_000`. New `truncate_diff(diff, max) -> String` helper truncates at a UTF-8 char boundary and appends `"[diff truncated — N bytes total]"`. Used in both `log_git_diff` and the shutdown path.

4. **SIGTERM handling + final diff on shutdown** (`watcher.rs`): On Unix, `watch()` now uses `tokio::select!` to listen for both `ctrl_c` and `sigterm.recv()`. After the signal, a final git diff is captured and appended before printing "Watcher stopped."

5. **Bash hook support** (`main.rs`): `install_zsh_hook` replaced by `install_shell_hook()` + `install_hook_into_file()`. Detects `$SHELL`: zsh → writes `SHELL_HOOK_ZSH` to `~/.zshrc`; bash → writes `SHELL_HOOK_BASH` to `~/.bashrc` (or `~/.bash_profile`); unknown → prints both snippets. Renamed constants to `SHELL_HOOK_ZSH` / `SHELL_HOOK_BASH` / `SHELL_HOOK_SENTINEL`.

6. **Graceful `resume stop` / `resume status` with no session** (`session.rs`, `main.rs`): `close()` now checks for file existence first and prints "No session found." instead of hard-erroring. `resume status` in `main.rs` catches `load()` errors and prints a friendly message.

7. **Corrupt session.json recovery** (`session.rs`): `load()` now catches `serde_json` parse failures, prints `"warn: session.json is corrupt, starting fresh"`, overwrites the file with a fresh empty session, and returns it instead of propagating the error.

## Packaging Intent (important — read before distributing)

When this tool is packaged (Homebrew formula, cargo-install, installer script, etc.), the shell hook
setup **must be surfaced prominently** to non-technical users. The hook is what makes `resume show`
actually useful — without it, the session only contains file events, not the commands that caused them.

**Recommended packaging approach:**
- Post-install message should say: *"Run `resume init --install-hook` to finish setup, then open a new terminal."*
- Homebrew caveats block is the right place for this on macOS
- Any install script (curl | sh style) should offer to run `resume init --install-hook` automatically as a final step
- Do NOT silently modify `~/.zshrc` without the user running `resume init --install-hook` themselves

The `--install-hook` flag is already implemented and idempotent (safe to re-run). It uses a sentinel
comment (`# --- resume shell hook ---`) to detect existing installs.

## In Progress / Unfinished

- Shell hook requires a new terminal or `source ~/.zshrc` / `source ~/.bashrc` after install
- The `last_seen` debounce map in `watcher.rs` grows unbounded for very long sessions (low priority)

- Event deduplication + noise filtering in `watcher.rs` (session 2, continued):
  - `DEBOUNCE_SECS = 2`: same path logged within 2 seconds is suppressed via `HashMap<String, Instant>`
  - `NOISE_SUFFIXES`: `.swp`, `.swo`, `.swn`, `~`, `.DS_Store` filtered out
  - `NOISE_CONTAINS`: paths with `.tmp.` or `.temp.` in the filename filtered out
  - `NOISE_PREFIXES`: `.#` (emacs lock files) filtered out
  - `is_filtered()` combines dir-ignore and filename-noise checks
  - Root cause was real: a single editor save fired 3–6 notify events; `.gitignore.tmp.7338...` was leaking through

## Next Steps (Priority Order)

1. End-to-end test: `cargo install --path .`, `resume init --install-hook`, open a new terminal, run `resume`, do work, press Ctrl-C or `finish`, then `resume show`
2. TUI: make the event list scrollable (currently just renders newest-first, no scroll state)
3. TUI: show a live git diff panel on the right side (split layout) for the most recent diff
4. Periodically evict stale entries from the `last_seen` debounce map (low priority — only matters for multi-hour sessions)
5. Write a proper integration test (create temp dir, start session, append events, verify JSON, check lock)

## Key Decisions

| Decision | Rationale |
|---|---|
| `notify` over `kqueue` directly | Cross-platform, maintained, sane API |
| Session stored in `.resume/session.json` per-project | Local to project, easy to inspect, no global state |
| `append_event` reads+writes entire file | Simple; session files are small. Can optimize to append-only JSONL later if needed |
| `claude-sonnet-4-6` model | Good balance of quality and cost; opus was too expensive for routine briefings |
| `watcher::watch()` blocks on Ctrl-C | Simple for v1; daemon mode is next step |
| `fs2` for file locking | Minimal dep, cross-platform, POSIX `flock` semantics; avoids rolling a custom advisory lock |
| Corrupt session → overwrite with fresh | Better than crashing; session data is observability not source of truth |
| `MAX_DIFF_BYTES = 8_000` | Keeps API context lean; full diff is rarely needed for a briefing |
| Detect `$SHELL` for hook install | Avoids requiring users to know which rc file to edit |

## What Was Built — Session 4 (2026-03-18)

1. **TUI mode** (`src/tui.rs`, `Cargo.toml`): `resume` (no args) now launches a live ratatui/crossterm terminal dashboard. Displays a header with project name + elapsed time, a scrolling event list (newest first) with color-coded FILE/GIT/CMD badges, and a footer with event count + keybindings. Added `ratatui = "0.29"` and `crossterm = "0.28"` to Cargo.toml.

2. **`finish` shell function** (`main.rs`): Both `SHELL_HOOK_ZSH` and `SHELL_HOOK_BASH` constants updated to include `finish() { resume stop "$@"; }` inside the hook fences. Running `finish` in the terminal now stops the session cleanly.

3. **Watcher signal architecture** (`watcher.rs`): `watch()` signature changed to `watch(ui_tx: Option<UnboundedSender<SessionEvent>>, shutdown: Option<oneshot::Receiver<()>>)`. In TUI mode, the TUI owns a oneshot sender; when Ctrl-C is pressed in the TUI, it sends on the channel, causing the watcher to cleanly exit. This avoids a conflict where both the TUI and the watcher would compete to handle Ctrl-C. `log_git_diff` also accepts `ui_tx` and sends GitDiff events to the TUI. All daemon call sites pass `(None, None)`.

4. **VS Code integration** (`.vscode/settings.json`, `.vscode/tasks.json`): Terminal font, size, and truecolor env vars configured for a clean TUI experience. Three VS Code tasks defined: `resume: start session`, `resume: show briefing`, `resume: status`.

5. **`cargo build --release`** confirmed clean with zero warnings.

## Known Issues / Bugs

- `reqwest 0.11` requires `openssl` on macOS; may need `brew install openssl` if build fails (using `rustls-tls` feature avoids this)
- `cargo build` passes clean as of session 4

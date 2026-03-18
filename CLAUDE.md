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
└── summarize.rs  — Anthropic Messages API call, builds briefing from session log
```

### Data Flow

```
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

## In Progress / Unfinished

- No shell command capture yet (planned: hook into zsh via PROMPT_COMMAND or a wrapper)

## Next Steps (Priority Order)

1. Shell command capture — zsh hook (PROMPT_COMMAND / precmd) or wrapper script
2. Add `.resume/session.json` and `.resume/resume.pid` to `.gitignore` template
3. Write `resume init` subcommand that adds the .gitignore entries automatically

## Key Decisions

| Decision | Rationale |
|---|---|
| `notify` over `kqueue` directly | Cross-platform, maintained, sane API |
| Session stored in `.resume/session.json` per-project | Local to project, easy to inspect, no global state |
| `append_event` reads+writes entire file | Simple; session files are small. Can optimize to append-only JSONL later if needed |
| `claude-sonnet-4-6` model | Good balance of quality and cost; opus was too expensive for routine briefings |
| `watcher::watch()` blocks on Ctrl-C | Simple for v1; daemon mode is next step |

## Known Issues / Bugs

- `reqwest 0.11` requires `openssl` on macOS; may need `brew install openssl` if build fails
- `cargo build` passes clean as of session 2

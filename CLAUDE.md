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

## In Progress / Unfinished

- `git.rs` is implemented but not yet wired into the watcher loop (next step)
- No shell command capture yet (planned: hook into zsh via PROMPT_COMMAND or a wrapper)
- No background daemon mode — `resume start` currently blocks the terminal

## Next Steps (Priority Order)

1. Wire `git::current_diff()` into the watcher loop — capture diffs periodically alongside file events
2. Add background daemon mode: write a PID file, detach with `nohup` or a proper daemonize approach
3. Shell command capture — document zsh hook or write a wrapper script
4. Add `.resume/session.json` to `.gitignore` template
5. Write `resume init` subcommand that adds the .gitignore entry automatically

## Key Decisions

| Decision | Rationale |
|---|---|
| `notify` over `kqueue` directly | Cross-platform, maintained, sane API |
| Session stored in `.resume/session.json` per-project | Local to project, easy to inspect, no global state |
| `append_event` reads+writes entire file | Simple; session files are small. Can optimize to append-only JSONL later if needed |
| `claude-sonnet-4-6` model | Good balance of quality and cost; opus was too expensive for routine briefings |
| `watcher::watch()` blocks on Ctrl-C | Simple for v1; daemon mode is next step |

## Known Issues / Bugs

- None confirmed yet — needs a `cargo build` and manual test run
- `reqwest 0.11` requires `openssl` on macOS; may need `brew install openssl` if build fails

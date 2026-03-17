# resume

A developer session recorder and context restorer.

**The problem:** You step away from a project, come back the next day, and spend
20 minutes rebuilding mental state — what were you doing? Where did you leave off?
What was the next step?

**The fix:** `resume` passively watches your project during a coding session
(file changes, git diffs) and stores them locally. When you return, one command
calls an LLM and prints a plain-English briefing.

---

## Usage

```bash
# Start a session in your project directory
resume start

# (work on your project...)
# Ctrl-C to stop watching

# When you come back later:
resume show

# See raw captured events:
resume status

# Explicitly close the session:
resume stop
```

## Setup

```bash
# Clone and build
git clone <repo>
cd resume
cargo build --release

# Add to PATH
export PATH="$PATH:$(pwd)/target/release"

# Set your Anthropic API key
export ANTHROPIC_API_KEY=sk-ant-...
```

## How It Works

1. `resume start` creates `.resume/session.json` in your current directory and
   begins watching for file changes using the `notify` crate.
2. Every file create/modify event (outside `.git/`, `target/`, `node_modules/`,
   `.resume/`) is appended to the session log.
3. `resume show` loads the session and sends the event log to the Anthropic API
   (`claude-opus-4-6`), which returns a 3-6 sentence briefing.

## Requirements

- Rust 1.80+
- `ANTHROPIC_API_KEY` environment variable
- macOS (Linux support is untested but should work)

## Project Layout

```
src/
├── main.rs       CLI entry point (clap)
├── session.rs    Session data model + .resume/session.json I/O
├── watcher.rs    File system watcher (notify)
├── git.rs        Git diff capture (git2)
└── summarize.rs  Anthropic API call
```

## Roadmap

- [ ] Background daemon mode (don't block the terminal)
- [ ] Git diff capture wired into the watcher loop
- [ ] Shell command capture via zsh hooks
- [ ] `resume init` to set up `.gitignore`

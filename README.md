# resume

Never lose context between coding sessions.

`resume` watches your project while you work — file changes, git diffs, shell commands —
and stores them locally. When you come back, one command calls an LLM and prints a
plain-English briefing of what you were doing, what you finished, and what to do next.

---

## Quick Start

```bash
# Build and install
cargo install --path .

# Set your Anthropic API key — add this to ~/.zshrc or ~/.bashrc to make it permanent
export ANTHROPIC_API_KEY=sk-ant-...

# Or store it in a .env file in your project (resume loads it automatically)
echo 'ANTHROPIC_API_KEY=sk-ant-...' >> .env

# Run in any git project
cd your-project
resume
```

`resume` opens a live dashboard. Work normally. Press **Ctrl-C** to stop the session.

Next time you open the project, run `resume show` to get your briefing.

---

## Shell Hook Setup (Recommended)

Without the shell hook, `resume` only captures file changes. With it, your shell commands
are captured too — this makes briefings significantly more useful.

```bash
resume init --install-hook
# then open a new terminal (or source ~/.zshrc / ~/.bashrc)
```

This appends a small hook to `~/.zshrc` (zsh) or `~/.bashrc` (bash) that logs commands
to the active session. It's idempotent — safe to re-run. It also adds `.resume/` to your
project's `.gitignore`.

---

## TUI Commands

While the dashboard is open, type any of these and press Enter:

| Command | What it does |
|---|---|
| `show` | Generate and display a briefing for this session |
| `cancel` | Cancel an in-progress briefing |
| `finish` | Archive this session and start a fresh one |
| `note <text>` | Save a persistent note for this project |
| `notes` | Show all saved notes |
| `notes clear` | Delete all notes for this project |
| `help` | Show the full command reference |

**Model switching:** Press **Shift+Tab** to cycle through available models (only models
that are actually reachable are shown — no Anthropic key means no cloud models, missing
Ollama pulls are hidden automatically). The current model is shown in the bottom bar.

**Scroll:** Arrow keys or PgUp/PgDn to scroll the briefing panel.

---

## CLI Commands

```bash
resume show                    # Generate a briefing (outside of TUI)
resume status                  # Print all captured events raw
resume note "something to remember"
resume notes                   # List all notes
resume init --install-hook     # Set up the shell hook
```

---

## Local Models (Ollama)

`resume` works without an Anthropic API key if you have [Ollama](https://ollama.ai) running locally.

**Install Ollama:** https://ollama.ai/download

**Supported models** (install any or all — only pulled models appear in Shift+Tab cycling):

```bash
ollama pull qwen3.5               # Fast, general-purpose (~2GB)
ollama pull qwen3-coder:30b       # Strong code understanding (~18GB)
ollama pull deepseek-coder-v2:16b # Code-focused (~9GB)
```

Make sure Ollama is running (`ollama serve`) before launching `resume`. The TUI detects
available local models on startup automatically.

---

## Requirements

- Rust 1.85+
- macOS or Linux
- `ANTHROPIC_API_KEY` **or** at least one Ollama model pulled and `ollama serve` running

---

## Project Layout

```
src/
├── main.rs       — CLI entry point and subcommand dispatch (clap)
├── session.rs    — Session data model, central storage under ~/.resume/projects/
├── watcher.rs    — File system watcher (notify), git diff capture, noise filtering
├── git.rs        — git2 diff and commit log helpers
├── summarize.rs  — Prompt builder + Anthropic/Ollama API calls
├── models.rs     — Model registry and provider dispatch
└── tui.rs        — Live ratatui/crossterm dashboard (Resy the jellyfish lives here)
```

Session data is stored in `~/.resume/projects/<your-project-path>/` — nothing is written
to your project repository.

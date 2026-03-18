// Entry point for the `resume` CLI. Wires up subcommands via clap and dispatches
// to the appropriate module: session, watcher, git, or summarize.

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};

mod git;
mod session;
mod summarize;
mod tui;
mod watcher;

#[derive(Parser)]
#[command(name = "resume", about = "Developer session recorder and context restorer")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Initialize resume for this project (adds .gitignore entries + shell hook)
    Init {
        /// Automatically install the shell hook into ~/.zshrc (no manual copy-paste needed)
        #[arg(long)]
        install_hook: bool,
    },
    /// Begin a session and watch for file/git changes in the background
    Start {
        /// Run the watcher in the foreground (internal — used by daemon re-exec)
        #[arg(long, hide = true)]
        daemon: bool,
    },
    /// Stop the background session watcher
    Stop,
    /// Call the LLM and print a briefing for the current project
    Show,
    /// Show what has been captured so far this session
    Status,
    /// Append a shell command to the session log (called by the shell hook)
    #[command(hide = true)]
    LogCommand {
        cmd: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        None => {
            // Default: run the live TUI session
            tui::run().await?;
        }
        Some(Command::Init { install_hook }) => {
            init_project(install_hook)?;
        }
        Some(Command::LogCommand { cmd }) => {
            // Silent — called by the shell hook in a background job.
            session::log_command(&cmd)?;
        }
        Some(Command::Start { daemon: true }) => {
            // Running as the background daemon — do the actual work.
            watcher::watch(None, None).await?;
        }
        Some(Command::Start { daemon: false }) => {
            // Check if a session is already running.
            if let Some(existing_pid) = session::read_pid()? {
                if process_is_running(existing_pid) {
                    bail!("A session is already running (PID {existing_pid}). Run `resume stop` first.");
                }
                // Stale PID file — clean it up.
                session::clear_pid()?;
            }

            session::init()?;

            // Re-exec ourselves with --daemon in the background.
            let exe = std::env::current_exe().context("failed to find current executable")?;
            let child = spawn_daemon(&exe)?;
            let pid = child.id();
            session::write_pid(pid)?;

            println!("Session running in background (PID: {pid}). Use `resume stop` to end it.");
        }
        Some(Command::Stop) => {
            match session::read_pid()? {
                Some(pid) => {
                    kill_process(pid);
                    session::clear_pid()?;
                    println!("Sent stop signal to watcher (PID {pid}).");
                }
                None => {
                    println!("No background watcher found (no PID file).");
                }
            }
            session::close()?;
        }
        Some(Command::Show) => {
            let sess = session::load()?;
            let briefing = summarize::generate(&sess).await?;
            println!("{}", briefing);
        }
        Some(Command::Status) => {
            match session::load() {
                Ok(sess) => session::print_status(&sess),
                Err(_) => println!("No session found. Run `resume start` to begin."),
            }
        }
    }

    Ok(())
}

#[cfg(unix)]
fn spawn_daemon(exe: &std::path::Path) -> Result<std::process::Child> {
    use std::os::unix::process::CommandExt;
    std::process::Command::new(exe)
        .args(["start", "--daemon"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0) // detach from terminal's process group
        .spawn()
        .context("failed to spawn background watcher")
}

#[cfg(not(unix))]
fn spawn_daemon(exe: &std::path::Path) -> Result<std::process::Child> {
    std::process::Command::new(exe)
        .args(["start", "--daemon"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .context("failed to spawn background watcher")
}

/// Returns true if a process with the given PID is currently running.
fn process_is_running(pid: u32) -> bool {
    // `kill -0` checks for process existence without sending a real signal.
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Send SIGTERM to the given PID.
fn kill_process(pid: u32) {
    let _ = std::process::Command::new("kill")
        .args([&pid.to_string()])
        .status();
}

const GITIGNORE_ENTRIES: &str = "\n# resume session files\n.resume/\n";

// Sentinel used to detect whether the hook is already installed (same for both shells).
const SHELL_HOOK_SENTINEL: &str = "# --- resume shell hook ---";

const SHELL_HOOK_ZSH: &str = "\
# --- resume shell hook ---\n\
_resume_preexec() {\n\
    [[ -f .resume/session.json ]] || return\n\
    resume log-command \"$1\" 2>/dev/null &!\n\
}\n\
preexec_functions+=(_resume_preexec)\n\
finish() { resume stop \"$@\"; }\n\
# --- end resume shell hook ---\n";

const SHELL_HOOK_BASH: &str = "\
# --- resume shell hook ---\n\
_resume_preexec() {\n\
    [ -f .resume/session.json ] || return\n\
    [ \"${BASH_SUBSHELL}\" -eq 0 ] || return\n\
    resume log-command \"$BASH_COMMAND\" 2>/dev/null &\n\
}\n\
trap '_resume_preexec' DEBUG\n\
finish() { resume stop \"$@\"; }\n\
# --- end resume shell hook ---\n";

fn init_project(install_hook: bool) -> Result<()> {
    // 1. Add .resume/ to .gitignore if not already present.
    let gitignore_path = std::path::Path::new(".gitignore");
    let existing = if gitignore_path.exists() {
        std::fs::read_to_string(gitignore_path).context("failed to read .gitignore")?
    } else {
        String::new()
    };

    if existing.contains(".resume/") {
        println!(".gitignore already contains .resume/ — skipping.");
    } else {
        let mut content = existing;
        content.push_str(GITIGNORE_ENTRIES);
        std::fs::write(gitignore_path, &content).context("failed to write .gitignore")?;
        println!("Added .resume/ to .gitignore.");
    }

    // 2. Shell hook — auto-install or print for manual setup.
    if install_hook {
        install_shell_hook()?;
    } else {
        println!("\nTo capture shell commands, add this to your ~/.zshrc (zsh) or ~/.bashrc (bash):\n");
        println!("zsh:\n{SHELL_HOOK_ZSH}");
        println!("bash:\n{SHELL_HOOK_BASH}");
        println!("Or run `resume init --install-hook` to do it automatically.");
    }

    Ok(())
}

fn install_shell_hook() -> Result<()> {
    let home = dirs::home_dir().context("could not find home directory")?;
    let shell = std::env::var("SHELL").unwrap_or_default();

    if shell.contains("zsh") {
        let rc_path = home.join(".zshrc");
        install_hook_into_file(&rc_path, SHELL_HOOK_ZSH, "~/.zshrc")?;
    } else if shell.contains("bash") {
        // Prefer ~/.bashrc; fall back to ~/.bash_profile if it doesn't exist.
        let bashrc = home.join(".bashrc");
        let rc_path = if bashrc.exists() {
            bashrc
        } else {
            home.join(".bash_profile")
        };
        let label = if rc_path.ends_with(".bashrc") { "~/.bashrc" } else { "~/.bash_profile" };
        install_hook_into_file(&rc_path, SHELL_HOOK_BASH, label)?;
    } else {
        println!("Could not detect shell (SHELL={:?}).", shell);
        println!("\nFor zsh, add to ~/.zshrc:\n\n{SHELL_HOOK_ZSH}");
        println!("For bash, add to ~/.bashrc:\n\n{SHELL_HOOK_BASH}");
    }

    Ok(())
}

fn install_hook_into_file(
    rc_path: &std::path::Path,
    hook: &str,
    label: &str,
) -> Result<()> {
    let existing = if rc_path.exists() {
        std::fs::read_to_string(rc_path)
            .with_context(|| format!("failed to read {label}"))?
    } else {
        String::new()
    };

    if existing.contains(SHELL_HOOK_SENTINEL) {
        println!("Shell hook already present in {label} — skipping.");
        return Ok(());
    }

    let mut content = existing;
    content.push('\n');
    content.push_str(hook);
    std::fs::write(rc_path, content)
        .with_context(|| format!("failed to write {label}"))?;

    println!("Shell hook installed in {label}.");
    println!("Run `source {label}` (or open a new terminal) to activate it.");
    Ok(())
}

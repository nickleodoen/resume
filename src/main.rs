// Entry point for the `resume` CLI. Wires up subcommands via clap and dispatches
// to the appropriate module: session, watcher, git, or summarize.

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};

mod git;
mod session;
mod summarize;
mod watcher;

#[derive(Parser)]
#[command(name = "resume", about = "Developer session recorder and context restorer")]
struct Cli {
    #[command(subcommand)]
    command: Command,
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
        Command::Init { install_hook } => {
            init_project(install_hook)?;
        }
        Command::LogCommand { cmd } => {
            // Silent — called by the shell hook in a background job.
            session::log_command(&cmd)?;
        }
        Command::Start { daemon: true } => {
            // Running as the background daemon — do the actual work.
            watcher::watch().await?;
        }
        Command::Start { daemon: false } => {
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
        Command::Stop => {
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
        Command::Show => {
            let sess = session::load()?;
            let briefing = summarize::generate(&sess).await?;
            println!("{}", briefing);
        }
        Command::Status => {
            let sess = session::load()?;
            session::print_status(&sess);
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

// Sentinel used to detect whether the hook is already installed.
const ZSH_HOOK_SENTINEL: &str = "# --- resume shell hook ---";

const ZSH_HOOK: &str = "\
# --- resume shell hook ---\n\
_resume_preexec() {\n\
    [[ -f .resume/session.json ]] || return\n\
    resume log-command \"$1\" 2>/dev/null &!\n\
}\n\
preexec_functions+=(_resume_preexec)\n\
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
        install_zsh_hook()?;
    } else {
        println!("\nTo capture shell commands, add this to your ~/.zshrc:\n");
        println!("{ZSH_HOOK}");
        println!("Or run `resume init --install-hook` to do it automatically.");
    }

    Ok(())
}

fn install_zsh_hook() -> Result<()> {
    let zshrc_path = dirs::home_dir()
        .context("could not find home directory")?
        .join(".zshrc");

    let existing = if zshrc_path.exists() {
        std::fs::read_to_string(&zshrc_path).context("failed to read ~/.zshrc")?
    } else {
        String::new()
    };

    if existing.contains(ZSH_HOOK_SENTINEL) {
        println!("Shell hook already present in ~/.zshrc — skipping.");
        return Ok(());
    }

    let mut content = existing;
    content.push('\n');
    content.push_str(ZSH_HOOK);
    std::fs::write(&zshrc_path, content).context("failed to write ~/.zshrc")?;

    println!("Shell hook installed in ~/.zshrc.");
    println!("Run `source ~/.zshrc` (or open a new terminal) to activate it.");
    Ok(())
}

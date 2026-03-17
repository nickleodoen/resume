// Entry point for the `resume` CLI. Wires up subcommands via clap and dispatches
// to the appropriate module: session, watcher, git, or summarize.

use anyhow::Result;
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
    /// Begin a session and start watching for file/git changes
    Start,
    /// End the session and save a final snapshot
    Stop,
    /// Call the LLM and print a briefing for the current project
    Show,
    /// Show what has been captured so far this session
    Status,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Start => {
            println!("Starting session...");
            session::init()?;
            watcher::watch().await?;
        }
        Command::Stop => {
            println!("Stopping session...");
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

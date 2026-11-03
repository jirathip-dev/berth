//! The `berth` binary: argument parsing and wiring only.
#![forbid(unsafe_code)]

use std::path::PathBuf;

use anyhow::Context as _;
use berth_server::Paths;
use clap::{Parser, Subcommand};

/// Keeps coding-agent terminals alive, reads their screens and reports their state.
#[derive(Parser)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Print the version.
    Hello,
    /// Run the per-user server: it owns the terminals, the state file and the socket.
    Serve {
        /// Keep the socket (berth.sock) and state (state.json) in this directory instead of the
        /// per-user defaults.
        #[arg(long)]
        dir: Option<PathBuf>,
    },
}

fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::Hello => println!("berth {}", env!("CARGO_PKG_VERSION")),
        Command::Serve { dir } => {
            let paths = match dir {
                Some(dir) => Paths::in_dir(&dir),
                None => Paths::user()?,
            };
            berth_server::serve(&paths).context("berth serve")?;
        }
    }
    Ok(())
}

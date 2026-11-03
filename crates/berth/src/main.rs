//! The `berth` binary: argument parsing and wiring only.
#![forbid(unsafe_code)]

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
}

fn main() {
    match Cli::parse().command {
        Command::Hello => println!("berth {}", env!("CARGO_PKG_VERSION")),
    }
}

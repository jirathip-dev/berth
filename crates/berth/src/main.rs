//! The `berth` binary: argument parsing and wiring only.
#![forbid(unsafe_code)]

mod attach;

use std::fmt::Write as _;
use std::io;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{ExitCode, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use berth_server::proto::{
    KeysParams, LsParams, Method, ReadParams, ReadResult, RunParams, SendParams, State, Target,
    TerminalInfo,
};
use berth_server::{Client, Paths};
use clap::{Parser, Subcommand};
use serde::Serialize;
use serde_json::Value;

/// Keeps coding-agent terminals alive, reads their screens and reports their state.
#[derive(Parser)]
#[command(version)]
struct Cli {
    /// Use the socket (berth.sock) and state (state.json) in this directory instead of the per-user
    /// defaults.
    #[arg(long, global = true)]
    dir: Option<PathBuf>,
    /// Print JSON instead of text.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Print the version.
    Hello,
    /// Run the per-user server: it owns the terminals, the state file and the socket.
    Serve {
        /// Start the server in the background and return once it answers.
        #[arg(long)]
        detach: bool,
    },
    /// Start a terminal; prints its name.
    Run {
        /// The terminal's name (default: the first free `tN`).
        #[arg(long)]
        name: Option<String>,
        /// A label, `k=v`; repeatable.
        #[arg(long = "label", value_name = "K=V", value_parser = label)]
        labels: Vec<(String, String)>,
        /// The working directory (default: the current one).
        #[arg(long)]
        cwd: Option<PathBuf>,
        /// Come back as a shell in the same cwd after a server restart.
        #[arg(long)]
        keep: bool,
        /// Start `berth serve --detach` first when no server runs.
        #[arg(long)]
        autostart: bool,
        /// The program and its arguments.
        #[arg(last = true, required = true)]
        cmd: Vec<String>,
    },
    /// List terminals.
    Ls {
        /// Only terminals carrying this label, `k=v`; repeatable.
        #[arg(long = "label", value_name = "K=V", value_parser = label)]
        labels: Vec<(String, String)>,
    },
    /// One terminal as JSON.
    Get {
        /// The terminal.
        name: String,
    },
    /// The current screen, or the scrollback, as plain text.
    Read {
        /// The terminal.
        name: String,
        /// Only the last N lines.
        #[arg(long)]
        lines: Option<usize>,
        /// The visible screen (the default).
        #[arg(long, conflicts_with = "history")]
        screen: bool,
        /// The scrollback and the screen.
        #[arg(long)]
        history: bool,
    },
    /// Type text.
    Send {
        /// The terminal.
        name: String,
        /// The text, sent as is (add a newline, or `berth keys <t> enter`, to submit it).
        text: String,
    },
    /// Press keys: `enter`, `esc`, `ctrl+c`, `up`, …
    Keys {
        /// The terminal.
        name: String,
        /// Key names, pressed in order.
        #[arg(required = true)]
        keys: Vec<String>,
    },
    /// End the terminal's process tree.
    Kill {
        /// The terminal.
        name: String,
    },
    /// Remove an exited terminal.
    Rm {
        /// The terminal.
        name: String,
    },
    /// Take over the terminal interactively; `Ctrl+\` detaches.
    Attach {
        /// The terminal.
        name: String,
    },
}

fn label(s: &str) -> Result<(String, String), String> {
    s.split_once('=')
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .ok_or_else(|| format!("{s:?} is not k=v"))
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("berth: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> anyhow::Result<()> {
    let paths = match &cli.dir {
        Some(dir) => Paths::in_dir(dir),
        None => Paths::user()?,
    };
    let json = cli.json;
    match cli.command {
        Command::Hello => println!("berth {}", env!("CARGO_PKG_VERSION")),
        Command::Serve { detach: false } => berth_server::serve(&paths).context("berth serve")?,
        Command::Serve { detach: true } => serve_detached(&paths, cli.dir.as_deref())?,
        Command::Run {
            name,
            labels,
            cwd,
            keep,
            autostart,
            cmd,
        } => {
            let here = std::env::current_dir().context("current directory")?;
            let params = RunParams {
                name,
                labels: labels.into_iter().collect(),
                cwd: cwd.map_or_else(|| here.clone(), |d| here.join(d)),
                keep,
                cmd,
            };
            let mut client = connect(&paths, cli.dir.as_deref(), Some(autostart))?;
            let info: TerminalInfo = client.call(Method::Run, &params)?;
            print(json, &info, || format!("{}\n", info.name))?;
        }
        Command::Ls { labels } => {
            let params = LsParams {
                labels: labels.into_iter().collect(),
            };
            let list: Vec<TerminalInfo> = connect(&paths, None, None)?.call(Method::Ls, &params)?;
            print(json, &list, || table(&list))?;
        }
        Command::Get { name } => {
            let info: TerminalInfo =
                connect(&paths, None, None)?.call(Method::Get, &Target { name })?;
            let pretty = serde_json::to_string_pretty(&info)?;
            print(json, &info, || format!("{pretty}\n"))?;
        }
        Command::Read {
            name,
            lines,
            history,
            ..
        } => {
            let params = ReadParams {
                name,
                lines,
                history,
            };
            let read: ReadResult = connect(&paths, None, None)?.call(Method::Read, &params)?;
            print(json, &read, || {
                if read.text.is_empty() {
                    String::new()
                } else {
                    format!("{}\n", read.text)
                }
            })?;
        }
        Command::Send { name, text } => {
            let done: Value =
                connect(&paths, None, None)?.call(Method::Send, &SendParams { name, text })?;
            print(json, &done, String::new)?;
        }
        Command::Keys { name, keys } => {
            let done: Value =
                connect(&paths, None, None)?.call(Method::Keys, &KeysParams { name, keys })?;
            print(json, &done, String::new)?;
        }
        Command::Kill { name } => {
            let info: TerminalInfo =
                connect(&paths, None, None)?.call(Method::Kill, &Target { name })?;
            print(json, &info, || match info.exit {
                Some(code) => format!("{} exited {code}\n", info.name),
                None => format!("{} exited\n", info.name),
            })?;
        }
        Command::Rm { name } => {
            let info: TerminalInfo =
                connect(&paths, None, None)?.call(Method::Rm, &Target { name })?;
            print(json, &info, || format!("{} removed\n", info.name))?;
        }
        Command::Attach { name } => attach::attach(connect(&paths, None, None)?, &name)?,
    }
    Ok(())
}

/// Prints `value` as one JSON line with `--json`, else `text()`.
fn print<T: Serialize>(json: bool, value: &T, text: impl FnOnce() -> String) -> anyhow::Result<()> {
    if json {
        println!("{}", serde_json::to_string(value)?);
    } else {
        print!("{}", text());
    }
    Ok(())
}

/// Connects to the server. `autostart` is `Some` for `run`: with `true` a missing server is started
/// (`dir` passed on), with `false` the error offers `--autostart`.
fn connect(paths: &Paths, dir: Option<&Path>, autostart: Option<bool>) -> anyhow::Result<Client> {
    match Client::connect(&paths.socket) {
        Err(berth_server::Error::Io(e))
            if matches!(
                e.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
            ) =>
        {
            if autostart == Some(true) {
                serve_detached(paths, dir)?;
                return Ok(Client::connect(&paths.socket)?);
            }
            let hint = if autostart.is_some() {
                " (or pass --autostart)"
            } else {
                ""
            };
            bail!(
                "no berth server on {}; start one with `berth serve --detach`{hint}",
                paths.socket.display()
            )
        }
        client => Ok(client?),
    }
}

/// Starts `berth serve` in its own process group, detached from this terminal, and waits until it
/// answers.
fn serve_detached(paths: &Paths, dir: Option<&Path>) -> anyhow::Result<()> {
    if UnixStream::connect(&paths.socket).is_ok() {
        bail!("a berth server already runs on {}", paths.socket.display());
    }
    let mut cmd = std::process::Command::new(std::env::current_exe()?);
    cmd.arg("serve");
    if let Some(dir) = dir {
        cmd.arg("--dir").arg(dir);
    }
    let mut child = cmd
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("starting berth serve")?;
    let deadline = Instant::now() + Duration::from_secs(5);
    while UnixStream::connect(&paths.socket).is_err() {
        if let Some(status) = child.try_wait()? {
            bail!("berth serve exited ({status}) before it answered");
        }
        if Instant::now() > deadline {
            bail!(
                "berth serve did not answer on {} within 5 s",
                paths.socket.display()
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    eprintln!("berth: server started on {}", paths.socket.display());
    Ok(())
}

/// `ls` as an aligned table: name, state, exit, pid, labels, command.
fn table(list: &[TerminalInfo]) -> String {
    let dash = |v: Option<i32>| v.map_or_else(|| "-".to_owned(), |v| v.to_string());
    let mut rows = vec![["NAME", "STATE", "EXIT", "PID", "LABELS", "CMD"].map(str::to_owned)];
    for t in list {
        let labels: Vec<String> = t.labels.iter().map(|(k, v)| format!("{k}={v}")).collect();
        rows.push([
            t.name.clone(),
            match t.state {
                State::Running => "running",
                State::Exited => "exited",
            }
            .to_owned(),
            dash(t.exit),
            dash(t.pid),
            if labels.is_empty() {
                "-".to_owned()
            } else {
                labels.join(",")
            },
            t.cmd.join(" "),
        ]);
    }
    let mut widths = [0; 6];
    for row in &rows {
        for (w, cell) in widths.iter_mut().zip(row) {
            *w = cell.chars().count().max(*w);
        }
    }
    let mut out = String::new();
    for row in &rows {
        let cells: Vec<String> = row
            .iter()
            .zip(widths)
            .map(|(c, w)| format!("{c:<w$}"))
            .collect();
        // Writing to a String cannot fail.
        let _ = writeln!(out, "{}", cells.join("  ").trim_end());
    }
    out
}

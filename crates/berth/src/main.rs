//! The `berth` binary: argument parsing and wiring only.
#![forbid(unsafe_code)]

mod attach;

use std::fmt::Write as _;
use std::io::{self, BufRead as _, Write as _};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{ExitCode, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use berth_server::machines::{self, Machine};
use berth_server::proto::{
    EventsParams, KeysParams, LsParams, Method, ReadParams, ReadResult, Reported, RunParams,
    SendParams, State, StateParams, Target, TerminalInfo, WaitOutcome, WaitParams, WaitResult,
};
use berth_server::{Client, Paths};
use clap::{Args, Parser, Subcommand};
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
    /// Run the command on this machine (`berth machine add`) over SSH.
    #[arg(long, global = true, value_name = "MACHINE")]
    on: Option<String>,
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
        /// Sample each terminal's memory and CPU every S seconds.
        #[arg(long, value_name = "S", default_value_t = 2.0)]
        sample: f64,
    },
    /// Start a terminal; prints its name.
    Run(RunArgs),
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
    /// Report a terminal's state: `berth state [<t>] working|blocked|done|error` (default `$BERTH`).
    State {
        /// The terminal, then the state; the terminal defaults to `$BERTH`, set inside every terminal.
        #[arg(required = true, num_args = 1..=2, value_name = "[T] STATE")]
        args: Vec<String>,
        /// A free-text note.
        #[arg(long)]
        note: Option<String>,
    },
    /// Block until the reported state or the screen matches; exits 0 on a match, 1 when the program
    /// exits, 124 on timeout.
    Wait(WaitArgs),
    /// Stream events as JSON lines: started, state, exited, killed and resources.
    Events {
        /// Only terminals carrying this label, `k=v`; repeatable.
        #[arg(long = "label", value_name = "K=V", value_parser = label)]
        labels: Vec<(String, String)>,
        /// First replay the kept events (the last 1,000) after this `seq`.
        #[arg(long, value_name = "N")]
        since: Option<u64>,
    },
    /// List terminals by memory use, largest first.
    Top,
    /// Speak the protocol over stdin/stdout, for `ssh host berth connect`.
    Connect {
        /// Start `berth serve --detach` first when no server runs.
        #[arg(long)]
        autostart: bool,
    },
    /// Machines that `--on` reaches over SSH (key login only).
    #[command(subcommand)]
    Machine(MachineCommand),
}

/// `berth machine …`.
#[derive(Subcommand)]
enum MachineCommand {
    /// Add a machine: a name for an ssh target, `[user@]host`.
    Add {
        /// The name `--on` takes.
        name: String,
        /// The ssh target.
        target: String,
    },
    /// List the machines.
    Ls,
    /// Remove a machine.
    Rm {
        /// The machine.
        name: String,
    },
}

/// Where the commands go: the local server, or the one on `--on`'s machine.
struct Server<'a> {
    paths: &'a Paths,
    dir: Option<&'a Path>,
    on: Option<Machine>,
}

impl Server<'_> {
    /// A client: over SSH with `--on` (`autostart` passed on), else the local one ([`connect`]).
    fn connect(&self, autostart: Option<bool>) -> anyhow::Result<Client> {
        match &self.on {
            Some(machine) => {
                let control = self.paths.socket.parent().unwrap_or(Path::new("."));
                Ok(machines::connect(
                    machine,
                    control,
                    autostart == Some(true),
                )?)
            }
            None => connect(self.paths, self.dir, autostart),
        }
    }
}

/// `berth run`'s arguments.
#[derive(Args)]
struct RunArgs {
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
    /// Report `idle` after S seconds without output, until the program reports a state.
    #[arg(long, value_name = "S")]
    idle_after: Option<u64>,
    /// The program and its arguments.
    #[arg(last = true, required = true)]
    cmd: Vec<String>,
}

/// `berth wait`'s arguments.
#[derive(Args)]
struct WaitArgs {
    /// The terminal.
    name: String,
    /// States that end the wait.
    #[arg(long, num_args = 1.., value_name = "STATE",
          value_parser = ["working", "blocked", "done", "error", "idle"])]
    until: Vec<String>,
    /// A pattern (`.`, `[…]`, `\d \w \s`, `* + ?`, `^ $`) that ends the wait when a screen line matches.
    #[arg(long, value_name = "RE")]
    output: Option<String>,
    /// Give up after S seconds.
    #[arg(long, value_name = "S")]
    timeout: Option<f64>,
}

fn label(s: &str) -> Result<(String, String), String> {
    s.split_once('=')
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .ok_or_else(|| format!("{s:?} is not k=v"))
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("berth: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> anyhow::Result<ExitCode> {
    let paths = match &cli.dir {
        Some(dir) => Paths::in_dir(dir),
        None => Paths::user()?,
    };
    let json = cli.json;
    let local = matches!(
        cli.command,
        Command::Hello | Command::Serve { .. } | Command::Connect { .. } | Command::Machine(_)
    );
    if local && cli.on.is_some() {
        bail!(
            "--on takes the terminal commands (run, ls, read, …), not hello, serve, connect or machine"
        );
    }
    let server = Server {
        paths: &paths,
        dir: cli.dir.as_deref(),
        on: cli
            .on
            .map(|name| machines::find(&paths.config, &name))
            .transpose()?,
    };
    match cli.command {
        Command::Hello => println!("berth {}", env!("CARGO_PKG_VERSION")),
        Command::Serve { detach, sample } => serve(&paths, cli.dir.as_deref(), detach, sample)?,
        Command::Run(args) => run_terminal(&server, json, args)?,
        Command::Ls { labels } => {
            let params = LsParams {
                labels: labels.into_iter().collect(),
            };
            let list: Vec<TerminalInfo> = server.connect(None)?.call(Method::Ls, &params)?;
            print(json, &list, || table(&list))?;
        }
        Command::Get { name } => {
            let info: TerminalInfo = server.connect(None)?.call(Method::Get, &Target { name })?;
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
            let read: ReadResult = server.connect(None)?.call(Method::Read, &params)?;
            print(json, &read, || {
                if read.text.is_empty() {
                    String::new()
                } else {
                    format!("{}\n", read.text)
                }
            })?;
        }
        Command::Send { name, text } => {
            let done: Value = server
                .connect(None)?
                .call(Method::Send, &SendParams { name, text })?;
            print(json, &done, String::new)?;
        }
        Command::Keys { name, keys } => {
            let done: Value = server
                .connect(None)?
                .call(Method::Keys, &KeysParams { name, keys })?;
            print(json, &done, String::new)?;
        }
        Command::Kill { name } => {
            let info: TerminalInfo = server.connect(None)?.call(Method::Kill, &Target { name })?;
            print(json, &info, || match info.exit {
                Some(code) => format!("{} exited {code}\n", info.name),
                None => format!("{} exited\n", info.name),
            })?;
        }
        Command::Rm { name } => {
            let info: TerminalInfo = server.connect(None)?.call(Method::Rm, &Target { name })?;
            print(json, &info, || format!("{} removed\n", info.name))?;
        }
        Command::Attach { name } => attach::attach(server.connect(None)?, &name)?,
        Command::State { args, note } => state(&server, json, args, note)?,
        Command::Wait(args) => return wait(&server, json, args),
        Command::Events { labels, since } => {
            let params = EventsParams {
                labels: labels.into_iter().collect(),
                since,
            };
            events(server.connect(None)?, &params)?;
        }
        Command::Top => top(&server, json)?,
        Command::Connect { autostart } => relay(&paths, cli.dir.as_deref(), autostart)?,
        Command::Machine(command) => machine(&paths, json, command)?,
    }
    Ok(ExitCode::SUCCESS)
}

/// `berth serve [--detach] [--sample S]`.
fn serve(paths: &Paths, dir: Option<&Path>, detach: bool, sample: f64) -> anyhow::Result<()> {
    if detach {
        return serve_detached(paths, dir, Some(sample));
    }
    let every = Duration::try_from_secs_f64(sample)
        .ok()
        .filter(|d| !d.is_zero())
        .context("--sample must be a positive number of seconds")?;
    berth_server::serve(paths, every).context("berth serve")
}

/// `berth run`; with `--on` the cwd must be given, as an absolute path on the machine.
fn run_terminal(server: &Server, json: bool, args: RunArgs) -> anyhow::Result<()> {
    let cwd = match (&server.on, args.cwd) {
        (None, cwd) => {
            let here = std::env::current_dir().context("current directory")?;
            cwd.map_or_else(|| here.clone(), |d| here.join(d))
        }
        (Some(_), Some(d)) if d.is_absolute() => d,
        (Some(m), _) => bail!("with --on, pass --cwd as an absolute path on {}", m.name),
    };
    let params = RunParams {
        name: args.name,
        labels: args.labels.into_iter().collect(),
        cwd,
        keep: args.keep,
        idle_after: args.idle_after,
        cmd: args.cmd,
    };
    let mut client = server.connect(Some(args.autostart))?;
    let info: TerminalInfo = client.call(Method::Run, &params)?;
    print(json, &info, || format!("{}\n", info.name))
}

/// `berth connect [--autostart]`: the protocol over stdin/stdout.
fn relay(paths: &Paths, dir: Option<&Path>, autostart: bool) -> anyhow::Result<()> {
    if UnixStream::connect(&paths.socket).is_err() {
        if !autostart {
            bail!(
                "no berth server on {}; start one with `berth serve --detach`",
                paths.socket.display()
            );
        }
        serve_detached(paths, dir, None)?;
    }
    berth_server::relay(&paths.socket).context("berth connect")
}

/// `berth top`: the terminals by memory, largest first.
fn top(server: &Server, json: bool) -> anyhow::Result<()> {
    let mut list: Vec<TerminalInfo> = server
        .connect(None)?
        .call(Method::Ls, &LsParams::default())?;
    list.sort_by_key(|t| std::cmp::Reverse(t.resources.map(|r| r.rss_mb)));
    print(json, &list, || table(&list))
}

/// `berth state [<t>] <state>`: the terminal defaults to `$BERTH`.
fn state(
    server: &Server,
    json: bool,
    mut args: Vec<String>,
    note: Option<String>,
) -> anyhow::Result<()> {
    let state = args.pop().unwrap_or_default();
    let name = match args.pop() {
        Some(name) => name,
        None => std::env::var("BERTH")
            .context("no terminal given and $BERTH is unset (it is set inside a terminal)")?,
    };
    let params = StateParams {
        name,
        report: Reported { state, note },
    };
    let info: TerminalInfo = server.connect(None)?.call(Method::State, &params)?;
    print(json, &info, String::new)
}

/// `berth wait`: exit 0 on a match, 1 when the program exits, 124 on timeout.
fn wait(server: &Server, json: bool, args: WaitArgs) -> anyhow::Result<ExitCode> {
    let params = WaitParams {
        name: args.name,
        until: args.until,
        output: args.output,
        timeout: args.timeout,
    };
    let done: WaitResult = server.connect(None)?.call(Method::Wait, &params)?;
    let t = &done.terminal;
    let state = t.reported.as_ref().map(|r| &r.state);
    print(json, &done, || match (done.outcome, t.exit) {
        (WaitOutcome::Matched, _) => match state.filter(|s| params.until.contains(s)) {
            Some(state) => format!("{} {state}\n", t.name),
            None => format!("{} output\n", t.name),
        },
        (WaitOutcome::Exited, Some(code)) => format!("{} exited {code}\n", t.name),
        (WaitOutcome::Exited, None) => format!("{} exited\n", t.name),
        (WaitOutcome::Timeout, _) => format!("{} timeout\n", t.name),
    })?;
    Ok(ExitCode::from(match done.outcome {
        WaitOutcome::Matched => 0,
        WaitOutcome::Exited => 1,
        WaitOutcome::Timeout => 124,
    }))
}

/// `berth events`: copies the event lines to stdout until the server closes the connection.
fn events(mut client: Client, params: &EventsParams) -> anyhow::Result<()> {
    let _: Value = client.call(Method::Events, params)?;
    let (reader, _writer) = client.split();
    let mut out = io::stdout().lock();
    for line in reader.lines() {
        writeln!(out, "{}", line?)?;
        out.flush()?;
    }
    Ok(())
}

/// `berth machine add|ls|rm`.
fn machine(paths: &Paths, json: bool, command: MachineCommand) -> anyhow::Result<()> {
    match command {
        MachineCommand::Add { name, target } => {
            let m = machines::add(&paths.config, &name, &target)?;
            print(json, &m, || format!("{} added\n", m.name))
        }
        MachineCommand::Ls => {
            let list = machines::load(&paths.config)?;
            print(json, &list, || {
                let width = list.iter().map(|m| m.name.len()).max().unwrap_or(0).max(4);
                let mut out = format!("{:<width$}  TARGET\n", "NAME");
                for m in &list {
                    // Writing to a String cannot fail.
                    let _ = writeln!(out, "{:<width$}  {}", m.name, m.target);
                }
                out
            })
        }
        MachineCommand::Rm { name } => {
            let m = machines::remove(&paths.config, &name)?;
            print(json, &m, || format!("{} removed\n", m.name))
        }
    }
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
                serve_detached(paths, dir, None)?;
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
fn serve_detached(paths: &Paths, dir: Option<&Path>, sample: Option<f64>) -> anyhow::Result<()> {
    if UnixStream::connect(&paths.socket).is_ok() {
        bail!("a berth server already runs on {}", paths.socket.display());
    }
    let mut cmd = std::process::Command::new(std::env::current_exe()?);
    cmd.arg("serve");
    if let Some(dir) = dir {
        cmd.arg("--dir").arg(dir);
    }
    if let Some(sample) = sample {
        cmd.arg("--sample").arg(sample.to_string());
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

/// `ls` and `top` as an aligned table: name, state, exit, pid, tree memory and CPU, labels, command.
fn table(list: &[TerminalInfo]) -> String {
    let dash = |v: Option<String>| v.unwrap_or_else(|| "-".to_owned());
    let mut rows = vec![
        [
            "NAME", "STATE", "EXIT", "PID", "RSS", "CPU", "LABELS", "CMD",
        ]
        .map(str::to_owned),
    ];
    for t in list {
        let labels: Vec<String> = t.labels.iter().map(|(k, v)| format!("{k}={v}")).collect();
        rows.push([
            t.name.clone(),
            match t.state {
                State::Running => "running",
                State::Exited => "exited",
            }
            .to_owned(),
            dash(t.exit.map(|v| v.to_string())),
            dash(t.pid.map(|v| v.to_string())),
            dash(t.resources.map(|r| format!("{}M", r.rss_mb))),
            dash(t.resources.map(|r| format!("{}%", r.cpu_pct))),
            if labels.is_empty() {
                "-".to_owned()
            } else {
                labels.join(",")
            },
            t.cmd.join(" "),
        ]);
    }
    let mut widths = [0; 8];
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

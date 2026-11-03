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
use berth_server::acp::{self, Input, Policy};
use berth_server::agents;
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
    /// Type text; to an ACP terminal, send it as a prompt.
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
    /// Stream events as JSON lines: started, state, exited, killed, resources, and ACP terminals' turn,
    /// text, `tool_call`, plan, mode, `approval_request`, approved and reply.
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
    /// ACP: answer the oldest permission request with an option id (or kind, e.g. `allow_once`).
    Approve {
        /// The terminal.
        name: String,
        /// The option.
        option: String,
    },
    /// ACP: cancel the running turn (as `berth keys <t> esc`).
    Cancel {
        /// The terminal.
        name: String,
    },
    /// List the installed ACP agents, or add or remove one.
    Agents {
        #[command(subcommand)]
        action: Option<AgentsCommand>,
    },
    /// The ACP bridge `run --acp` starts inside the terminal.
    #[command(hide = true)]
    AcpBridge {
        #[arg(long)]
        socket: PathBuf,
        #[arg(long)]
        state: PathBuf,
        #[arg(long, value_parser = ["allow", "deny", "ask"])]
        policy: String,
        #[arg(last = true, required = true)]
        cmd: Vec<String>,
    },
}

#[derive(Subcommand)]
enum AgentsCommand {
    /// Install an agent from the ACP registry, pinned to the version it lists now.
    Add {
        /// The registry id (`claude-acp`, `codex-acp`, `gemini`, `pi-acp`, …).
        id: String,
        /// The registry: a URL or a file.
        #[arg(long, value_name = "SRC", default_value = agents::REGISTRY)]
        registry: String,
    },
    /// Remove an installed agent.
    Rm {
        /// The agent id.
        id: String,
    },
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
    /// Run the installed ACP agent ID (`berth agents`) as a structured session; with a command
    /// after `--`, that command is its ACP adapter instead.
    #[arg(long, value_name = "ID", conflicts_with_all = ["keep", "idle_after"])]
    acp: Option<String>,
    /// ACP: answer permission requests `allow`, `deny`, or `ask` (wait for `berth approve`).
    #[arg(long, requires = "acp", value_parser = ["allow", "deny", "ask"], default_value = "ask")]
    policy: String,
    /// The program and its arguments.
    #[arg(last = true, required_unless_present = "acp")]
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
    match cli.command {
        Command::Hello => println!("berth {}", env!("CARGO_PKG_VERSION")),
        Command::Serve { detach, sample } => serve(&paths, cli.dir.as_deref(), detach, sample)?,
        Command::Run(args) => run_terminal(&paths, cli.dir.as_deref(), json, args)?,
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
        Command::Send { name, text } => send(&paths, json, name, text)?,
        Command::Approve { name, option } => {
            print(
                json,
                &to_acp(&paths, name, &Input::Approve(option))?,
                String::new,
            )?;
        }
        Command::Cancel { name } => {
            print(json, &to_acp(&paths, name, &Input::Cancel)?, String::new)?;
        }
        Command::Agents { action } => agents_cmd(&paths, json, action)?,
        Command::AcpBridge {
            socket,
            state,
            policy,
            cmd,
        } => return bridge(&cmd, &socket, &state, &policy),
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
        Command::State { args, note } => state(&paths, json, args, note)?,
        Command::Wait(args) => return wait(&paths, json, args),
        Command::Events { labels, since } => {
            let params = EventsParams {
                labels: labels.into_iter().collect(),
                since,
            };
            events(connect(&paths, None, None)?, &params)?;
        }
        Command::Top => top(&paths, json)?,
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

/// `berth run`: prints the new terminal's name.
fn run_terminal(paths: &Paths, dir: Option<&Path>, json: bool, a: RunArgs) -> anyhow::Result<()> {
    let here = std::env::current_dir().context("current directory")?;
    let cmd = match &a.acp {
        Some(id) => bridge_cmd(paths, id, &a.policy, a.cmd)?,
        None => a.cmd,
    };
    let params = RunParams {
        name: a.name,
        labels: a.labels.into_iter().collect(),
        cwd: a.cwd.map_or_else(|| here.clone(), |d| here.join(d)),
        keep: a.keep,
        idle_after: a.idle_after,
        cmd,
        acp: a.acp,
    };
    let mut client = connect(paths, dir, Some(a.autostart))?;
    let info: TerminalInfo = client.call(Method::Run, &params)?;
    print(json, &info, || format!("{}\n", info.name))
}

/// `berth send`: an ACP terminal gets a framed prompt, reported `working` first so `send; wait --until
/// done` never sees the last turn.
fn send(paths: &Paths, json: bool, name: String, text: String) -> anyhow::Result<()> {
    let mut client = connect(paths, None, None)?;
    let info: TerminalInfo = client.call(Method::Get, &Target { name: name.clone() })?;
    let text = if info.acp.is_some() {
        let report = Reported {
            state: "working".to_owned(),
            note: None,
        };
        let params = StateParams {
            name: name.clone(),
            report,
        };
        let _: TerminalInfo = client.call(Method::State, &params)?;
        acp::frame(&Input::Prompt(text))
    } else {
        text
    };
    let done: Value = client.call(Method::Send, &SendParams { name, text })?;
    print(json, &done, String::new)
}

/// `berth acp-bridge`: the bridge on this terminal, in raw mode; exits with the agent's code.
fn bridge(cmd: &[String], socket: &Path, state: &Path, policy: &str) -> anyhow::Result<ExitCode> {
    let policy = match policy {
        "allow" => Policy::Allow,
        "deny" => Policy::Deny,
        _ => Policy::Ask,
    };
    let here = std::env::current_dir().context("current directory")?;
    let _raw = attach::RawMode::enter()?;
    let code = acp::bridge(cmd, &here, socket, state, policy)?;
    Ok(ExitCode::from(u8::try_from(code).unwrap_or(1)))
}

/// `berth run --acp`: this binary's ACP bridge around the adapter `cmd`, else the installed agent's.
fn bridge_cmd(
    paths: &Paths,
    id: &str,
    policy: &str,
    cmd: Vec<String>,
) -> anyhow::Result<Vec<String>> {
    let cmd = if cmd.is_empty() {
        let installed = agents::list(&paths.state)?;
        let Some(agent) = installed.into_iter().find(|a| a.id == id) else {
            bail!("ACP agent {id} is not installed; `berth agents add {id}`");
        };
        agent.cmd
    } else {
        cmd
    };
    let exe = std::env::current_exe()?;
    let path = |p: &Path| p.to_string_lossy().into_owned();
    let bridge = [
        path(&exe),
        "acp-bridge".to_owned(),
        "--socket".to_owned(),
        path(&paths.socket),
        "--state".to_owned(),
        path(&paths.state),
        "--policy".to_owned(),
        policy.to_owned(),
        "--".to_owned(),
    ];
    Ok([bridge.to_vec(), cmd].concat())
}

/// Delivers `input` to an ACP terminal's bridge.
fn to_acp(paths: &Paths, name: String, input: &Input) -> anyhow::Result<Value> {
    let mut client = connect(paths, None, None)?;
    let info: TerminalInfo = client.call(Method::Get, &Target { name: name.clone() })?;
    if info.acp.is_none() {
        bail!("{name} is not an ACP terminal (berth run --acp)");
    }
    let text = acp::frame(input);
    Ok(client.call(Method::Send, &SendParams { name, text })?)
}

/// `berth agents [add|rm]`.
fn agents_cmd(paths: &Paths, json: bool, action: Option<AgentsCommand>) -> anyhow::Result<()> {
    let line = |a: &agents::Agent| format!("{} {} ({})\n", a.id, a.version, a.distribution);
    match action {
        None => {
            let list = agents::list(&paths.state)?;
            print(json, &list, || {
                list.iter()
                    .map(|a| format!("{}  {}", line(a).trim_end(), a.cmd.join(" ")) + "\n")
                    .collect()
            })
        }
        Some(AgentsCommand::Add { id, registry }) => {
            let agent = agents::add(&paths.state, &id, &registry)?;
            print(json, &agent, || format!("installed {}", line(&agent)))
        }
        Some(AgentsCommand::Rm { id }) => {
            let agent = agents::rm(&paths.state, &id)?;
            print(json, &agent, || format!("removed {}", line(&agent)))
        }
    }
}

/// `berth top`: the terminals by memory, largest first.
fn top(paths: &Paths, json: bool) -> anyhow::Result<()> {
    let mut list: Vec<TerminalInfo> =
        connect(paths, None, None)?.call(Method::Ls, &LsParams::default())?;
    list.sort_by_key(|t| std::cmp::Reverse(t.resources.map(|r| r.rss_mb)));
    print(json, &list, || table(&list))
}

/// `berth state [<t>] <state>`: the terminal defaults to `$BERTH`.
fn state(
    paths: &Paths,
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
    let info: TerminalInfo = connect(paths, None, None)?.call(Method::State, &params)?;
    print(json, &info, String::new)
}

/// `berth wait`: exit 0 on a match, 1 when the program exits, 124 on timeout.
fn wait(paths: &Paths, json: bool, args: WaitArgs) -> anyhow::Result<ExitCode> {
    let params = WaitParams {
        name: args.name,
        until: args.until,
        output: args.output,
        timeout: args.timeout,
    };
    let done: WaitResult = connect(paths, None, None)?.call(Method::Wait, &params)?;
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

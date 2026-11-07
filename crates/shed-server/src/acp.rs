//! ACP client mode. `shed run --acp` starts [`bridge`] as an ordinary terminal's program: it runs the
//! agent's ACP adapter on stdio, speaks the Agent Client Protocol (v1) to it through the
//! `agent-client-protocol` SDK, and turns the session into shed events (`emit`), reported states and a
//! JSONL transcript that `read` renders.
//!
//! The bridge reads its PTY in raw mode: a line framed by [`frame`] (`send`, `approve`, `cancel`,
//! `model`), a plain line typed in `attach` (a prompt), or `Esc` / `Ctrl+C` alone (cancel).
//!
//! Only this module is async: its own tokio runtime runs the SDK connection and the session task. The
//! glue to the sync rest is two threads: one reads the PTY into a channel of [`Input`]s, one drains the
//! task's outputs into the transcript, the terminal and the server (as notifications).

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs::{self, File};
use std::io::{self, BufWriter, Read as _, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::v1::schema::{
    CancelNotification, ContentBlock, Implementation, InitializeRequest, LoadSessionRequest,
    NewSessionRequest, PromptRequest, PromptResponse, RequestPermissionOutcome,
    RequestPermissionRequest, RequestPermissionResponse, SelectedPermissionOutcome,
    SessionConfigKind as Kind, SessionConfigOption, SessionConfigOptionCategory as Category,
    SessionConfigOptionValue, SessionConfigSelectOption, SessionConfigSelectOptions as Choices,
    SessionId, SessionNotification, SessionUpdate, SetSessionConfigOptionRequest, TextContent,
};
use agent_client_protocol::v1::{ConnectionTo, Responder};
use agent_client_protocol::{Agent, ErrorCode, JsonRpcMessage as _, Lines, UntypedMessage};
use futures::{sink, stream};
use serde::{Deserialize, Serialize};
use shed_proto::{
    AcpSession, ApprovalOption, EmitParams, EventKind, Method, Reported, StateParams,
};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use crate::{Client, Error};

type AcpResult<T> = Result<T, agent_client_protocol::Error>;
type Asked = Responder<RequestPermissionResponse>;

/// How the bridge answers the agent's permission requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Policy {
    /// Pick the first `allow_once` (else `allow_always`) option.
    Allow,
    /// Pick the first `reject_once` (else `reject_always`) option.
    Deny,
    /// Report `blocked` and wait for `shed approve`.
    Ask,
}

/// One framed input to the bridge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Input {
    /// Start a turn (`session/prompt`).
    Prompt(String),
    /// Answer the oldest permission request with this option id or kind.
    Approve(String),
    /// Cancel the turn (`session/cancel`).
    Cancel,
    /// Switch to this model (`session/set_config_option`, the `model` option).
    Model(String),
}

/// The bytes that deliver `input` to a bridge through its terminal: `RS`, the JSON, a newline.
#[must_use]
pub fn frame(input: &Input) -> String {
    // An enum of strings always serialises.
    format!("\x1e{}\n", serde_json::to_string(input).unwrap_or_default())
}

/// The transcript of terminal `name` of the server whose state file is `state`.
#[must_use]
pub fn transcript(state: &Path, name: &str) -> PathBuf {
    state.with_file_name("acp").join(format!("{name}.jsonl"))
}

/// Control characters (but newlines and tabs) as `?`, so agent text cannot drive a terminal.
fn clean(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '\n' | '\t' => c,
            c if c.is_control() => '?',
            c => c,
        })
        .collect()
}

/// A transcript (JSONL events) as text: each prompt (`> …`), one line per tool call with its last
/// status, and each turn's final reply.
#[must_use]
pub fn render(transcript: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut tools: BTreeMap<String, usize> = BTreeMap::new();
    for kind in transcript
        .lines()
        .filter_map(|l| serde_json::from_str::<EventKind>(l).ok())
    {
        match kind {
            EventKind::Turn { prompt } => lines.push(format!("> {prompt}")),
            EventKind::ToolCall { id, title, status } => {
                let line = format!("[tool] {title} ({status})");
                if let Some(&at) = tools.get(&id) {
                    lines[at] = line;
                } else {
                    tools.insert(id, lines.len());
                    lines.push(line);
                }
            }
            EventKind::Reply { text, .. } if !text.is_empty() => lines.push(text),
            _ => {}
        }
    }
    clean(&lines.join("\n"))
}

/// What the connection hands the session task, in the order the agent sent it.
enum FromAgent {
    /// A `session/update`, or why it does not parse (a kind this SDK does not know).
    Update(Result<Box<SessionUpdate>, String>),
    Permission(Box<RequestPermissionRequest>, Asked),
    /// The response to our prompt number `.0`.
    Prompt(u64, AcpResult<PromptResponse>),
    /// The config options after `session/set_config_option`.
    Config(AcpResult<Vec<SessionConfigOption>>),
}

/// What the session task hands the writer thread, in order.
enum Out {
    /// An event; `true` when replayed.
    Emit(EventKind, bool),
    State(Reported),
    /// A line for the terminal only.
    Say(String),
}

/// A typed value's wire name (`end_turn`, `allow_once`, …).
fn wire<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value)
        .unwrap_or_default()
        .trim_matches('"')
        .to_owned()
}

fn acp_error(why: impl Into<String>) -> agent_client_protocol::Error {
    agent_client_protocol::Error::new(ErrorCode::INTERNAL_ERROR, why)
}

/// The session task's state.
struct Session {
    cx: ConnectionTo<Agent>,
    /// Hands responses to our requests back to the task, in order with the agent's messages.
    back: UnboundedSender<FromAgent>,
    out: Sender<Out>,
    id: SessionId,
    policy: Policy,
    /// Our `session/prompt` requests in flight, by our number.
    prompts: BTreeSet<u64>,
    next: u64,
    /// Permission requests waiting for `approve`: their responder and options.
    asks: VecDeque<(Asked, Vec<ApprovalOption>)>,
    /// Tool call titles by id, for updates that carry none.
    titles: BTreeMap<String, String>,
    /// The current turn's reply so far.
    reply: String,
    auth: &'static [&'static str],
    heard: Instant,
    /// Replaying a resumed session's history.
    replay: bool,
    /// The agent's `model` config option, if it has one.
    model: Option<String>,
    shown: AcpSession,
}

impl Session {
    /// Hands `out` to the writer thread; an error once it is gone.
    fn send(&self, out: Out) -> AcpResult<()> {
        let gone = |_| acp_error("the writer thread is gone");
        self.out.send(out).map_err(gone)
    }

    fn emit(&self, kind: EventKind) {
        let _ = self.send(Out::Emit(kind, self.replay));
    }

    fn state(&self, state: &str, note: Option<String>) {
        let (state, state_source) = (state.to_owned(), None);
        let _ = self.send(Out::State(Reported {
            state,
            note,
            state_source,
        }));
    }

    fn say(&self, line: String) -> AcpResult<()> {
        self.send(Out::Say(line))
    }

    /// `auth: <line>` for the first line of `text` with one of the agent's auth-failure patterns.
    fn auth(&self, text: &str) -> Option<String> {
        let bad = |l: &&str| self.auth.iter().any(|p| l.contains(p));
        text.lines().find(bad).map(|l| format!("auth: {l}"))
    }

    /// Ends the turn as `error` with the note `why`, cancelling it at the agent. An auth failure in
    /// `why` makes the note `auth: <line>`, after a `reply` with the stop reason `auth_error`.
    fn fail(&mut self, why: String) -> AcpResult<()> {
        let cancelled = self.input(Input::Cancel);
        self.prompts.clear();
        let auth = self.auth(&why);
        if auth.is_some() {
            let (text, stop_reason) = (std::mem::take(&mut self.reply), "auth_error".to_owned());
            self.emit(EventKind::Reply { text, stop_reason });
        }
        self.state("error", Some(auth.unwrap_or(why)));
        cancelled
    }

    /// Answers a permission request with option `id`, or `cancelled`.
    fn answer(asked: Asked, id: Option<String>) -> AcpResult<()> {
        let outcome = match id {
            Some(id) => RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(id)),
            None => RequestPermissionOutcome::Cancelled,
        };
        asked.respond(RequestPermissionResponse::new(outcome))
    }

    /// One framed or typed input.
    fn input(&mut self, input: Input) -> AcpResult<()> {
        self.heard = Instant::now();
        let back = self.back.clone();
        match input {
            Input::Prompt(text) => {
                self.reply.clear();
                self.emit(EventKind::Turn {
                    prompt: text.clone(),
                });
                self.state("working", None);
                let blocks = vec![ContentBlock::Text(TextContent::new(text))];
                let n = self.next;
                self.next += 1;
                self.cx
                    .send_request(PromptRequest::new(self.id.clone(), blocks))
                    .on_receiving_result(async move |r| {
                        let _ = back.send(FromAgent::Prompt(n, r));
                        Ok(())
                    })?;
                self.prompts.insert(n);
            }
            Input::Approve(choice) => {
                let Some((asked, options)) = self.asks.pop_front() else {
                    return self.say("shed: no permission request is waiting".to_owned());
                };
                let Some(option) = options
                    .iter()
                    .find(|o| o.id == choice)
                    .or_else(|| options.iter().find(|o| o.kind == choice))
                    .map(|o| o.id.clone())
                else {
                    let ids: Vec<&str> = options.iter().map(|o| o.id.as_str()).collect();
                    let ids = ids.join(", ");
                    self.asks.push_front((asked, options));
                    return self.say(format!("shed: no option {choice:?}; one of {ids}"));
                };
                Self::answer(asked, Some(option.clone()))?;
                self.emit(EventKind::Approved { option });
                let state = if self.asks.is_empty() {
                    "working"
                } else {
                    "blocked"
                };
                self.state(state, None);
            }
            Input::Cancel => {
                if !self.prompts.is_empty() {
                    let cancel = CancelNotification::new(self.id.clone());
                    self.cx.send_notification(cancel)?;
                }
                while let Some((asked, _)) = self.asks.pop_front() {
                    Self::answer(asked, None)?;
                }
            }
            Input::Model(model) => {
                let Some(option) = self.model.clone() else {
                    return self.say("shed: the agent offers no models".to_owned());
                };
                let model = SessionConfigOptionValue::value_id(model);
                let set = SetSessionConfigOptionRequest::new(self.id.clone(), option, model);
                self.cx
                    .send_request(set)
                    .on_receiving_result(async move |r| {
                        let _ = back.send(FromAgent::Config(r.map(|r| r.config_options)));
                        Ok(())
                    })?;
            }
        }
        Ok(())
    }

    /// One message from the agent.
    fn message(&mut self, m: FromAgent) -> AcpResult<()> {
        self.heard = Instant::now();
        match m {
            FromAgent::Update(Ok(u)) => self.update(*u),
            FromAgent::Update(Err(why)) => {
                self.say(clean(&format!("shed: skipped a session/update: {why}")))
            }
            FromAgent::Permission(request, asked) => self.permission(*request, asked),
            FromAgent::Prompt(n, r) if self.prompts.remove(&n) => match r {
                Err(e) => self.fail(e.message),
                Ok(r) => {
                    let (text, stop_reason) =
                        (std::mem::take(&mut self.reply), wire(&r.stop_reason));
                    self.emit(EventKind::Reply {
                        text,
                        stop_reason: stop_reason.clone(),
                    });
                    self.state("done", Some(stop_reason));
                    Ok(())
                }
            },
            FromAgent::Prompt(..) => Ok(()),
            FromAgent::Config(Ok(options)) => self.models(&options),
            FromAgent::Config(Err(e)) => self.say(format!("shed: model: {e}")),
        }
    }

    fn update(&mut self, u: SessionUpdate) -> AcpResult<()> {
        let text = |c: ContentBlock| match c {
            ContentBlock::Text(t) => Some(t.text),
            _ => None,
        };
        match u {
            SessionUpdate::AgentMessageChunk(c) => {
                if let Some(chunk) = text(c.content) {
                    self.reply.push_str(&chunk);
                    self.emit(EventKind::Text { text: chunk });
                    if !self.prompts.is_empty() && self.auth(&self.reply).is_some() {
                        return self.fail(self.reply.clone());
                    }
                }
            }
            // A resumed session's prompts; each ends the reply before it.
            SessionUpdate::UserMessageChunk(c) if self.replay => {
                if let Some(prompt) = text(c.content) {
                    self.end_replayed_turn();
                    self.emit(EventKind::Turn { prompt });
                }
            }
            SessionUpdate::ToolCall(call) => {
                let (id, status) = (call.tool_call_id.to_string(), wire(&call.status));
                self.titles.insert(id.clone(), call.title.clone());
                let title = call.title;
                self.emit(EventKind::ToolCall { id, title, status });
            }
            SessionUpdate::ToolCallUpdate(call) => {
                let id = call.tool_call_id.to_string();
                let title = match call.fields.title {
                    Some(title) => title,
                    None => self.titles.get(&id).cloned().unwrap_or_else(|| id.clone()),
                };
                self.titles.insert(id.clone(), title.clone());
                let status = wire(&call.fields.status.unwrap_or_default());
                self.emit(EventKind::ToolCall { id, title, status });
            }
            SessionUpdate::Plan(plan) => {
                let entries = plan.entries.iter().map(serde_json::to_value);
                let entries = entries.collect::<Result<_, _>>().unwrap_or_default();
                self.emit(EventKind::Plan { entries });
            }
            SessionUpdate::CurrentModeUpdate(m) => {
                let mode = m.current_mode_id.to_string();
                self.emit(EventKind::Mode { mode });
            }
            SessionUpdate::ConfigOptionUpdate(c) => return self.models(&c.config_options),
            _ => {}
        }
        Ok(())
    }

    /// Emits a replayed turn's reply, if any.
    fn end_replayed_turn(&mut self) {
        if !self.reply.is_empty() {
            let (text, stop_reason) = (std::mem::take(&mut self.reply), "end_turn".to_owned());
            self.emit(EventKind::Reply { text, stop_reason });
        }
    }

    /// Takes the agent's `model` option (a select) from its config options, then emits the session.
    fn models(&mut self, options: &[SessionConfigOption]) -> AcpResult<()> {
        let model = options.iter().find_map(|o| match (&o.category, &o.kind) {
            (Some(Category::Model), Kind::Select(s)) => Some((o, s)),
            _ => None,
        });
        if let Some((option, select)) = model {
            let choices: Vec<_> = match &select.options {
                Choices::Ungrouped(o) => o.iter().collect(),
                Choices::Grouped(g) => g.iter().flat_map(|g| &g.options).collect(),
                _ => Vec::new(),
            };
            self.model = Some(option.id.to_string());
            self.shown.model = Some(select.current_value.to_string());
            let pair = |c: &&SessionConfigSelectOption| [c.value.to_string(), c.name.clone()];
            self.shown.models = choices.iter().map(pair).collect();
        }
        self.send(Out::Emit(
            EventKind::Session(self.shown.clone()),
            self.replay,
        ))
    }

    fn permission(&mut self, p: RequestPermissionRequest, asked: Asked) -> AcpResult<()> {
        let call = p.tool_call;
        let tool = (call.fields.title)
            .or_else(|| self.titles.get(&call.tool_call_id.to_string()).cloned())
            .unwrap_or_else(|| "tool".to_owned());
        let options: Vec<ApprovalOption> = (p.options.into_iter())
            .map(|o| ApprovalOption {
                id: o.option_id.to_string(),
                name: o.name,
                kind: wire(&o.kind),
            })
            .collect();
        self.emit(EventKind::ApprovalRequest {
            tool: tool.clone(),
            options: options.clone(),
        });
        let kinds = match self.policy {
            Policy::Ask => {
                self.asks.push_back((asked, options));
                self.state("blocked", Some(tool));
                return Ok(());
            }
            Policy::Allow => ["allow_once", "allow_always"],
            Policy::Deny => ["reject_once", "reject_always"],
        };
        let pick = kinds
            .iter()
            .find_map(|k| options.iter().find(|o| o.kind == *k));
        let option = pick.map(|o| o.id.clone());
        Self::answer(asked, option.clone())?;
        if let Some(option) = option {
            self.emit(EventKind::Approved { option });
        }
        Ok(())
    }
}

/// The session task: the handshake, then the agent's messages, the inputs and the watchdog, one at a
/// time, until the agent closes its stdout.
async fn drive(
    mut s: Session,
    mut from_agent: UnboundedReceiver<FromAgent>,
    mut inputs: UnboundedReceiver<Input>,
    cwd: &Path,
    resume: Option<String>,
) -> AcpResult<()> {
    let cx = s.cx.clone();
    let refused = |e| {
        let closed = cx.is_incoming_closed();
        acp_error(if closed {
            "the agent closed its stdout".to_owned()
        } else {
            format!("agent refused: {e}")
        })
    };
    let init = InitializeRequest::new(ProtocolVersion::V1)
        .client_info(Implementation::new("shed", shed_proto::VERSION));
    let init = cx.send_request(init).block_task().await.map_err(refused)?;
    let options = match resume {
        Some(_) if !init.agent_capabilities.load_session => {
            let why = "the agent cannot resume a session (no loadSession)";
            return Err(acp_error(why));
        }
        Some(id) => {
            (s.id, s.replay) = (SessionId::new(id), true);
            let load = LoadSessionRequest::new(s.id.clone(), cwd);
            let loaded = cx.send_request(load).block_task().await.map_err(refused)?;
            // The updates before the response, all queued by now, are the history.
            while let Ok(m) = from_agent.try_recv() {
                s.message(m)?;
            }
            s.end_replayed_turn();
            s.replay = false;
            loaded.config_options
        }
        None => {
            let new = NewSessionRequest::new(cwd);
            let new = cx.send_request(new).block_task().await.map_err(refused)?;
            s.id = new.session_id;
            new.config_options
        }
    };
    s.shown.id = s.id.to_string();
    s.models(&options.unwrap_or_default())?;
    s.say(format!("shed: ACP session ready ({}); send a prompt", s.id))?;
    let idle = std::env::var("SHED_TURN_IDLE_TIMEOUT").map_or(120, |s| s.parse().unwrap_or(120));
    let mut typing = true;
    loop {
        tokio::select! {
            biased;
            Some(m) = from_agent.recv() => s.message(m)?,
            input = inputs.recv(), if typing => match input {
                Some(input) => s.input(input)?,
                None => typing = false,
            },
            () = cx.incoming_closed() => return Ok(()),
            // Ends a turn that heard nothing for `idle` seconds, but while a request waits for `approve`.
            () = tokio::time::sleep(Duration::from_millis(100)) => {
                if !s.prompts.is_empty() && s.asks.is_empty() && s.heard.elapsed().as_secs() >= idle {
                    s.fail("no response".to_owned())?;
                }
            }
        }
    }
}

/// The writer thread's end: the transcript, the terminal and the server.
struct Writer {
    transcript: BufWriter<File>,
    server: Option<Client>,
    socket: PathBuf,
    name: String,
}

impl Writer {
    /// Writes each output to the transcript, the server and the terminal, flushing (in that order)
    /// whenever the session task has nothing more queued.
    fn run(mut self, outs: &Receiver<Out>) {
        while let Ok(first) = outs.recv() {
            let mut screen = io::stdout().lock();
            let mut calls = Vec::new();
            for out in std::iter::once(first).chain(outs.try_iter()) {
                let name = self.name.clone();
                let (shown, call) = match out {
                    Out::Emit(kind, replay) => {
                        let line = serde_json::to_string(&kind).unwrap_or_default();
                        let _ = writeln!(self.transcript, "{line}");
                        let shown = match &kind {
                            EventKind::ApprovalRequest { tool, options } => {
                                let ids: Vec<&str> =
                                    options.iter().map(|o| o.id.as_str()).collect();
                                clean(&format!("[approve?] {tool}: {}", ids.join(" | ")))
                            }
                            EventKind::Approved { option } => {
                                clean(&format!("[approved] {option}"))
                            }
                            _ => render(&line),
                        };
                        let params = EmitParams { name, kind, replay };
                        (shown, crate::notification(Method::Emit, &params))
                    }
                    Out::State(report) => {
                        let params = StateParams { name, report };
                        (String::new(), crate::notification(Method::State, &params))
                    }
                    Out::Say(line) => (line, Ok(Vec::new())),
                };
                if !shown.is_empty() {
                    let _ = write!(screen, "{}\r\n", shown.replace('\n', "\r\n"));
                }
                calls.extend(call.unwrap_or_default());
            }
            let _ = self.transcript.flush();
            self.send(&calls);
            let _ = screen.flush();
        }
    }

    /// Sends notification lines, reconnecting once when the connection broke (a server restart). The
    /// agent's session goes on when the server is gone.
    fn send(&mut self, calls: &[u8]) {
        for _ in 0..2 {
            if self.server.is_none() {
                self.server = Client::connect(&self.socket).ok();
            }
            let Some(server) = &mut self.server else {
                return;
            };
            let w = &mut server.writer;
            if w.write_all(calls).and_then(|()| w.flush()).is_ok() {
                return;
            }
            self.server = None;
        }
    }
}

/// Reads the terminal's input (raw mode) and hands each framed or typed line, and each lone
/// `Esc` / `Ctrl+C`, to the session task.
fn read_input(inputs: &UnboundedSender<Input>) {
    let mut stdin = io::stdin().lock();
    let mut line = Vec::new();
    let mut buf = [0u8; 4096];
    while let Ok(n @ 1..) = stdin.read(&mut buf) {
        for &b in &buf[..n] {
            let input = match b {
                0x1b | 0x03 => {
                    line.clear();
                    Some(Input::Cancel)
                }
                b'\r' | b'\n' => {
                    let text = String::from_utf8_lossy(&line).into_owned();
                    line.clear();
                    match text.strip_prefix('\x1e') {
                        Some(json) => serde_json::from_str(json).ok(),
                        None if text.trim().is_empty() => None,
                        None => Some(Input::Prompt(text)),
                    }
                }
                0x7f => {
                    line.pop();
                    None
                }
                b => {
                    line.push(b);
                    None
                }
            };
            if let Some(input) = input
                && inputs.send(input).is_err()
            {
                return;
            }
        }
    }
}

/// Runs the ACP adapter `cmd` in `cwd` for terminal `$SHED` of the server on `socket` (state file
/// `state`): `initialize`, then `session/load` of `$SHED_ACP_RESUME` (its history replayed) or
/// `session/new` (no MCP servers), then prompts, updates and permission requests until the agent exits.
/// Returns the agent's exit code.
///
/// A failure to start is reported as the terminal's state `error`, with the reason as its note.
///
/// # Errors
/// [`Error::Acp`] when `$SHED` is unset, the agent cannot start, or the handshake fails (including
/// an agent that does not speak ACP v1, or a resume the agent cannot load); [`Error::Io`] when the
/// transcript cannot be created.
pub fn bridge(
    cmd: &[String],
    cwd: &Path,
    socket: &Path,
    state: &Path,
    policy: Policy,
    agent_id: &str,
) -> Result<i32, Error> {
    let name = std::env::var("SHED").map_err(|_| Error::Acp("$SHED is unset".to_owned()))?;
    let result = run(cmd, cwd, socket, state, policy, agent_id, &name);
    if let (Err(e), Ok(mut server)) = (&result, Client::connect(socket)) {
        let report = Reported {
            state: "error".to_owned(),
            note: Some(e.to_string()),
            state_source: None,
        };
        let _: Result<serde_json::Value, _> =
            server.call(Method::State, &StateParams { name, report });
    }
    result
}

/// Creates terminal `name`'s transcript, empty.
fn open_transcript(state: &Path, name: &str) -> io::Result<File> {
    let path = transcript(state, name);
    if let Some(dir) = path.parent() {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
    }
    fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(&path)
}

fn run(
    cmd: &[String],
    cwd: &Path,
    socket: &Path,
    state: &Path,
    policy: Policy,
    agent_id: &str,
    name: &str,
) -> Result<i32, Error> {
    let (out, outs) = channel();
    let writer = Writer {
        transcript: BufWriter::new(open_transcript(state, name)?),
        server: None,
        socket: socket.to_owned(),
        name: name.to_owned(),
    };
    let writer = std::thread::spawn(move || writer.run(&outs));
    let (typed, inputs) = unbounded_channel();
    std::thread::spawn(move || read_input(&typed));
    let session = |cx, back| Session {
        cx,
        back,
        out,
        id: SessionId::new(""),
        policy,
        prompts: BTreeSet::new(),
        next: 0,
        asks: VecDeque::new(),
        titles: BTreeMap::new(),
        reply: String::new(),
        auth: crate::agents::auth_failures(agent_id),
        heard: Instant::now(),
        replay: false,
        model: None,
        shown: AcpSession::default(),
    };
    let resume = std::env::var("SHED_ACP_RESUME").ok();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let result = runtime.block_on(async {
        let (program, args) = cmd
            .split_first()
            .ok_or_else(|| Error::Acp("empty agent command".to_owned()))?;
        let mut child = tokio::process::Command::new(program)
            .args(args)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|e| Error::Acp(format!("{program}: {e}")))?;
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            return Err(Error::Acp("the agent's stdio is not piped".to_owned()));
        };
        let incoming = stream::unfold(BufReader::new(stdout).lines(), async |mut lines| {
            Some((lines.next_line().await.transpose()?, lines))
        });
        let outgoing = sink::unfold(stdin, async |mut stdin, line: String| {
            stdin.write_all(format!("{line}\n").as_bytes()).await?;
            stdin.flush().await?;
            Ok::<_, io::Error>(stdin)
        });
        let (back, from_agent) = unbounded_channel();
        let (updates, asks) = (back.clone(), back.clone());
        agent_client_protocol::Client
            .v1()
            .name("shed")
            .on_receive_notification(
                async move |n: UntypedMessage, _| {
                    if SessionNotification::matches_method(n.method()) {
                        let n = SessionNotification::parse_message(n.method(), n.params());
                        let update = n.map(|n| Box::new(n.update)).map_err(|e| e.to_string());
                        let _ = updates.send(FromAgent::Update(update));
                    }
                    Ok(())
                },
                agent_client_protocol::on_receive_notification!(),
            )
            .on_receive_request(
                async move |r: RequestPermissionRequest, asked, _| {
                    let _ = asks.send(FromAgent::Permission(Box::new(r), asked));
                    Ok(())
                },
                agent_client_protocol::on_receive_request!(),
            )
            .connect_with(
                Lines::new(Box::pin(outgoing), Box::pin(incoming)),
                async move |cx| drive(session(cx, back), from_agent, inputs, cwd, resume).await,
            )
            .await
            .map_err(|e| Error::Acp(e.to_string()))?;
        Ok(child.wait().await?.code().unwrap_or(1))
    });
    // The task's sender is gone with it: the writer drains what is queued and ends.
    let _ = writer.join();
    result
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    #[test]
    fn render_keeps_prompts_one_line_per_tool_call_and_replies() {
        let lines = [
            json!({"event": "turn", "prompt": "fix it"}),
            json!({"event": "text", "text": "Look"}),
            json!({"event": "tool_call", "id": "a", "title": "Edit", "status": "pending"}),
            json!({"event": "tool_call", "id": "b", "title": "Run", "status": "pending"}),
            json!({"event": "tool_call", "id": "a", "title": "Edit", "status": "completed"}),
            json!({"event": "reply", "text": "Done\u{1b}[2J", "stop_reason": "end_turn"}),
        ];
        let jsonl: Vec<String> = lines.iter().map(Value::to_string).collect();
        assert_eq!(
            render(&jsonl.join("\n")),
            "> fix it\n[tool] Edit (completed)\n[tool] Run (pending)\nDone?[2J"
        );
    }

    #[test]
    fn frames_are_one_line_of_json() {
        let framed = frame(&Input::Prompt("a\nb".to_owned()));
        assert_eq!(framed, "\x1e{\"prompt\":\"a\\nb\"}\n");
        assert_eq!(frame(&Input::Cancel), "\x1e\"cancel\"\n");
    }
}

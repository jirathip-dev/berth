//! ACP client mode. `shed run --acp` starts [`bridge`] as an ordinary terminal's program: it runs the
//! agent's ACP adapter on stdio, speaks the Agent Client Protocol (v1) to it, and turns the session into
//! shed events (`emit`), reported states and a JSONL transcript that `read` renders.
//!
//! The bridge reads its PTY in raw mode: a line framed by [`frame`] (`send`, `approve`, `cancel`), a
//! plain line typed in `attach` (a prompt), or `Esc` / `Ctrl+C` alone (cancel).

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs::{self, File};
use std::io::{self, BufRead as _, BufReader, Read as _, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::process::{ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex, PoisonError};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use shed_proto::{ApprovalOption, EmitParams, EventKind, Method, Reported, StateParams};

use crate::{Client, Error};

/// The ACP protocol version shed speaks.
pub const PROTOCOL_VERSION: u64 = 1;

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

/// The bridge's side of the session, shared by the agent reader and the input reader.
struct Session {
    agent: ChildStdin,
    next_id: u64,
    id: String,
    policy: Policy,
    /// Our `session/prompt` requests in flight.
    prompts: BTreeSet<u64>,
    /// Permission requests waiting for `approve`: their request id and options.
    asks: VecDeque<(Value, Vec<ApprovalOption>)>,
    /// Tool call titles by id, for updates that carry none.
    titles: BTreeMap<String, String>,
    /// The current turn's reply so far.
    reply: String,
    server: Option<Client>,
    socket: PathBuf,
    name: String,
    transcript: File,
}

impl Session {
    fn send(&mut self, message: &Value) -> io::Result<()> {
        let mut line = serde_json::to_vec(message)?;
        line.push(b'\n');
        self.agent.write_all(&line)?;
        self.agent.flush()
    }

    fn request(&mut self, method: &str, params: &Value) -> io::Result<u64> {
        let id = self.next_id;
        self.next_id += 1;
        let message = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        self.send(&message).map(|()| id)
    }

    fn answer(&mut self, id: &Value, outcome: &Value) -> io::Result<()> {
        self.send(&json!({"jsonrpc": "2.0", "id": id, "result": {"outcome": outcome}}))
    }

    /// Calls the server, reconnecting once when the connection broke (a server restart). The agent's
    /// session goes on when the server is gone.
    fn call<P: Serialize>(&mut self, method: Method, params: &P) {
        for _ in 0..2 {
            if self.server.is_none() {
                self.server = Client::connect(&self.socket).ok();
            }
            let Some(server) = &mut self.server else {
                return;
            };
            match server.call::<P, Value>(method, params) {
                Err(Error::Io(_) | Error::BadResponse(_)) => self.server = None,
                _ => return,
            }
        }
    }

    /// Writes `kind` to the transcript, emits it and shows its line on the terminal.
    fn emit(&mut self, kind: EventKind) {
        if let Ok(mut line) = serde_json::to_vec(&kind) {
            line.push(b'\n');
            let _ = self.transcript.write_all(&line);
        }
        let shown = match &kind {
            EventKind::ApprovalRequest { tool, options } => {
                let ids: Vec<&str> = options.iter().map(|o| o.id.as_str()).collect();
                clean(&format!("[approve?] {tool}: {}", ids.join(" | ")))
            }
            EventKind::Approved { option } => clean(&format!("[approved] {option}")),
            kind => render(&serde_json::to_string(kind).unwrap_or_default()),
        };
        if !shown.is_empty() {
            print!("{}\r\n", shown.replace('\n', "\r\n"));
            let _ = io::stdout().flush();
        }
        let params = EmitParams {
            name: self.name.clone(),
            kind,
        };
        self.call(Method::Emit, &params);
    }

    fn state(&mut self, state: &str, note: Option<String>) {
        let params = StateParams {
            name: self.name.clone(),
            report: Reported {
                state: state.to_owned(),
                note,
            },
        };
        self.call(Method::State, &params);
    }

    /// One framed or typed input.
    fn input(&mut self, input: Input) -> io::Result<()> {
        match input {
            Input::Prompt(text) => {
                self.reply.clear();
                self.emit(EventKind::Turn {
                    prompt: text.clone(),
                });
                self.state("working", None);
                let prompt =
                    json!({"sessionId": self.id, "prompt": [{"type": "text", "text": text}]});
                let id = self.request("session/prompt", &prompt)?;
                self.prompts.insert(id);
            }
            Input::Approve(choice) => {
                let Some((id, options)) = self.asks.pop_front() else {
                    print!("shed: no permission request is waiting\r\n");
                    return Ok(());
                };
                let Some(option) = options
                    .iter()
                    .find(|o| o.id == choice)
                    .or_else(|| options.iter().find(|o| o.kind == choice))
                    .map(|o| o.id.clone())
                else {
                    let ids: Vec<&str> = options.iter().map(|o| o.id.as_str()).collect();
                    print!("shed: no option {choice:?}; one of {}\r\n", ids.join(", "));
                    self.asks.push_front((id, options));
                    return Ok(());
                };
                self.answer(&id, &json!({"outcome": "selected", "optionId": option}))?;
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
                    let cancel = json!({"jsonrpc": "2.0", "method": "session/cancel", "params": {"sessionId": self.id}});
                    self.send(&cancel)?;
                }
                while let Some((id, _)) = self.asks.pop_front() {
                    self.answer(&id, &json!({"outcome": "cancelled"}))?;
                }
            }
        }
        Ok(())
    }

    /// One message from the agent.
    fn message(&mut self, m: &Value) -> io::Result<()> {
        let text = |v: &Value| v.as_str().unwrap_or_default().to_owned();
        match (m.get("method").and_then(Value::as_str), m.get("id")) {
            (Some("session/request_permission"), Some(id)) => {
                self.permission(id.clone(), &m["params"])
            }
            (Some("session/update"), None) => {
                self.update(&m["params"]["update"]);
                Ok(())
            }
            (Some(_), Some(id)) => {
                let error = json!({"code": -32601, "message": "method not found"});
                self.send(&json!({"jsonrpc": "2.0", "id": id, "error": error}))
            }
            (None, Some(id)) => {
                if !id.as_u64().is_some_and(|id| self.prompts.remove(&id)) {
                    return Ok(());
                }
                let reply = std::mem::take(&mut self.reply);
                if let Some(e) = m.get("error") {
                    self.state("error", Some(text(&e["message"])));
                    return Ok(());
                }
                let stop_reason = text(&m["result"]["stopReason"]);
                self.emit(EventKind::Reply {
                    text: reply,
                    stop_reason: stop_reason.clone(),
                });
                self.state("done", Some(stop_reason));
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn update(&mut self, u: &Value) {
        let text = |v: &Value| v.as_str().map(str::to_owned);
        match u["sessionUpdate"].as_str().unwrap_or_default() {
            "agent_message_chunk" => {
                if let Some(chunk) = text(&u["content"]["text"]) {
                    self.reply.push_str(&chunk);
                    self.emit(EventKind::Text { text: chunk });
                }
            }
            "tool_call" | "tool_call_update" => {
                let id = text(&u["toolCallId"]).unwrap_or_default();
                let title = match text(&u["title"]) {
                    Some(title) => title,
                    None => self.titles.get(&id).cloned().unwrap_or_else(|| id.clone()),
                };
                self.titles.insert(id.clone(), title.clone());
                let status = text(&u["status"]).unwrap_or_else(|| "pending".to_owned());
                self.emit(EventKind::ToolCall { id, title, status });
            }
            "plan" => {
                let entries = u["entries"].as_array().cloned().unwrap_or_default();
                self.emit(EventKind::Plan { entries });
            }
            "current_mode_update" => {
                let mode = text(&u["currentModeId"]).unwrap_or_default();
                self.emit(EventKind::Mode { mode });
            }
            _ => {}
        }
    }

    fn permission(&mut self, id: Value, p: &Value) -> io::Result<()> {
        let call = &p["toolCall"];
        let tool = call["title"]
            .as_str()
            .or_else(|| {
                call["toolCallId"]
                    .as_str()
                    .and_then(|i| self.titles.get(i).map(String::as_str))
            })
            .unwrap_or("tool")
            .to_owned();
        let options: Vec<ApprovalOption> = p["options"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|o| ApprovalOption {
                id: o["optionId"].as_str().unwrap_or_default().to_owned(),
                name: o["name"].as_str().unwrap_or_default().to_owned(),
                kind: o["kind"].as_str().unwrap_or_default().to_owned(),
            })
            .collect();
        self.emit(EventKind::ApprovalRequest {
            tool: tool.clone(),
            options: options.clone(),
        });
        let kinds = match self.policy {
            Policy::Ask => {
                self.asks.push_back((id, options));
                self.state("blocked", Some(tool));
                return Ok(());
            }
            Policy::Allow => ["allow_once", "allow_always"],
            Policy::Deny => ["reject_once", "reject_always"],
        };
        let pick = kinds
            .iter()
            .find_map(|k| options.iter().find(|o| o.kind == *k));
        match pick {
            Some(o) => {
                let option = o.id.clone();
                self.answer(&id, &json!({"outcome": "selected", "optionId": option}))?;
                self.emit(EventKind::Approved { option });
                Ok(())
            }
            None => self.answer(&id, &json!({"outcome": "cancelled"})),
        }
    }
}

/// Reads lines from the agent until the response to request `id`; other messages before it are
/// dropped (an agent sends none before the session exists).
fn response(lines: &mut impl Iterator<Item = io::Result<String>>, id: u64) -> Result<Value, Error> {
    for line in lines {
        let Ok(m) = serde_json::from_str::<Value>(&line?) else {
            continue;
        };
        if m.get("method").is_none() && m.get("id") == Some(&json!(id)) {
            if let Some(e) = m.get("error") {
                return Err(Error::Acp(format!("agent refused: {e}")));
            }
            return Ok(m["result"].clone());
        }
    }
    Err(Error::Acp("the agent closed its stdout".to_owned()))
}

/// Reads the terminal's input (raw mode) and hands each framed or typed line, and each lone
/// `Esc` / `Ctrl+C`, to the session.
fn read_input(session: &Mutex<Session>) {
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
            let mut session = session.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(input) = input
                && session.input(input).is_err()
            {
                return;
            }
        }
    }
}

/// Runs the ACP adapter `cmd` in `cwd` for terminal `$SHED` of the server on `socket` (state file
/// `state`): `initialize`, `session/new` (no MCP servers), then prompts, updates and permission requests
/// until the agent exits. Returns the agent's exit code.
///
/// A failure to start is reported as the terminal's state `error`, with the reason as its note.
///
/// # Errors
/// [`Error::Acp`] when `$SHED` is unset, the agent cannot start, or the handshake fails (including
/// an agent that does not speak ACP v1); [`Error::Io`] when the transcript cannot be created.
pub fn bridge(
    cmd: &[String],
    cwd: &Path,
    socket: &Path,
    state: &Path,
    policy: Policy,
) -> Result<i32, Error> {
    let name = std::env::var("SHED").map_err(|_| Error::Acp("$SHED is unset".to_owned()))?;
    let result = run(cmd, cwd, socket, state, policy, &name);
    if let (Err(e), Ok(mut server)) = (&result, Client::connect(socket)) {
        let report = Reported {
            state: "error".to_owned(),
            note: Some(e.to_string()),
        };
        let _: Result<Value, _> = server.call(Method::State, &StateParams { name, report });
    }
    result
}

fn run(
    cmd: &[String],
    cwd: &Path,
    socket: &Path,
    state: &Path,
    policy: Policy,
    name: &str,
) -> Result<i32, Error> {
    let path = transcript(state, name);
    if let Some(dir) = path.parent() {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
    }
    let file = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(&path)?;
    let (program, args) = cmd
        .split_first()
        .ok_or_else(|| Error::Acp("empty agent command".to_owned()))?;
    let mut child = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| Error::Acp(format!("{program}: {e}")))?;
    let (Some(agent), Some(out)) = (child.stdin.take(), child.stdout.take()) else {
        return Err(Error::Acp("the agent's stdio is not piped".to_owned()));
    };
    let mut session = Session {
        agent,
        next_id: 0,
        id: String::new(),
        policy,
        prompts: BTreeSet::new(),
        asks: VecDeque::new(),
        titles: BTreeMap::new(),
        reply: String::new(),
        server: None,
        socket: socket.to_owned(),
        name: name.to_owned(),
        transcript: file,
    };
    let mut lines = BufReader::new(out).lines();
    let init = json!({
        "protocolVersion": PROTOCOL_VERSION,
        "clientCapabilities": {"fs": {"readTextFile": false, "writeTextFile": false}, "terminal": false},
        "clientInfo": {"name": "shed", "version": shed_proto::VERSION},
    });
    let id = session.request("initialize", &init)?;
    let version = response(&mut lines, id)?["protocolVersion"].as_u64();
    if version != Some(PROTOCOL_VERSION) {
        return Err(Error::Acp(format!(
            "the agent speaks ACP {version:?}; shed speaks {PROTOCOL_VERSION}"
        )));
    }
    let new = json!({"cwd": cwd, "mcpServers": []});
    let id = session.request("session/new", &new)?;
    response(&mut lines, id)?["sessionId"]
        .as_str()
        .ok_or_else(|| Error::Acp("session/new returned no sessionId".to_owned()))?
        .clone_into(&mut session.id);
    print!("shed: ACP session ready; send a prompt\r\n");
    let session = Arc::new(Mutex::new(session));
    let input = Arc::clone(&session);
    std::thread::spawn(move || read_input(&input));
    for line in lines {
        let Ok(m) = serde_json::from_str::<Value>(&line?) else {
            continue;
        };
        let mut session = session.lock().unwrap_or_else(PoisonError::into_inner);
        if session.message(&m).is_err() {
            break;
        }
    }
    let status = child.wait()?;
    Ok(status.code().unwrap_or(1))
}

#[cfg(test)]
mod tests {
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

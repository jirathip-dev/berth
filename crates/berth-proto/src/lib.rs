//! berth's API types and JSON-RPC method list.
//!
//! The wire format is newline-delimited JSON-RPC 2.0 over a Unix socket: one [`Request`] per line, one
//! [`Response`] per line. Every connection starts with [`Method::Hello`]; a client and server of
//! different [`VERSION`]s are refused with [`ErrorCode::Version`]. `docs/protocol.md` is generated
//! from these types (`BERTH_BLESS=1 cargo test -p berth-proto`); CI fails when it drifts.
#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The protocol version: the berth version. Client and server must match exactly.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The JSON-RPC version string every message carries.
pub const JSONRPC: &str = "2.0";

/// A JSON-RPC request. Without an `id` it is a notification and gets no response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    /// Always `"2.0"`.
    pub jsonrpc: String,
    /// The request id, echoed in the response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    /// The method name; see [`Method`].
    pub method: String,
    /// The method's parameters.
    #[serde(default)]
    pub params: Value,
}

impl Request {
    /// A request for `method` with `params` and a numeric `id`.
    #[must_use]
    pub fn new(id: u64, method: Method, params: Value) -> Self {
        Self {
            jsonrpc: JSONRPC.to_owned(),
            id: Some(id.into()),
            method: method.name().to_owned(),
            params,
        }
    }
}

/// A JSON-RPC response: exactly one of `result` and `error` is set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    /// Always `"2.0"`.
    pub jsonrpc: String,
    /// The id of the request this answers (`null` when the request could not be read).
    pub id: Value,
    /// The result on success.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// The error on failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

impl Response {
    /// A success response.
    #[must_use]
    pub fn ok(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: JSONRPC.to_owned(),
            id,
            result: Some(result),
            error: None,
        }
    }

    /// An error response.
    #[must_use]
    pub fn err(id: Value, error: RpcError) -> Self {
        Self {
            jsonrpc: JSONRPC.to_owned(),
            id,
            result: None,
            error: Some(error),
        }
    }
}

/// A JSON-RPC error object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RpcError {
    /// The numeric code; see [`ErrorCode`].
    pub code: i64,
    /// `NAME: detail`, where `NAME` is the [`ErrorCode::name`].
    pub message: String,
}

impl RpcError {
    /// An error with `code` and a message `NAME: detail`.
    #[must_use]
    pub fn new(code: ErrorCode, detail: impl std::fmt::Display) -> Self {
        Self {
            code: code.code(),
            message: format!("{}: {detail}", code.name()),
        }
    }

    /// The version refusal: `BERTH_VERSION: client x, server y; restart the server`.
    #[must_use]
    pub fn version(client: &str, server: &str) -> Self {
        Self::new(
            ErrorCode::Version,
            format!("client {client}, server {server}; restart the server"),
        )
    }
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// The error codes: JSON-RPC's own, then berth's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    /// The line is not valid JSON.
    Parse,
    /// The JSON is not a valid request.
    InvalidRequest,
    /// No such method.
    MethodNotFound,
    /// The parameters do not fit the method.
    InvalidParams,
    /// The server failed (saving state, reading the process table).
    Internal,
    /// Client and server versions differ.
    Version,
    /// A method other than `hello` came before the handshake.
    NoHello,
    /// No terminal has that name.
    NoSuchTerminal,
    /// Another terminal already has that name.
    NameTaken,
    /// The terminal is still running (`rm` needs it exited; `kill` it first).
    StillRunning,
    /// The program could not start.
    SpawnFailed,
}

impl ErrorCode {
    /// Every code, in documentation order.
    pub const ALL: [Self; 11] = [
        Self::Parse,
        Self::InvalidRequest,
        Self::MethodNotFound,
        Self::InvalidParams,
        Self::Internal,
        Self::Version,
        Self::NoHello,
        Self::NoSuchTerminal,
        Self::NameTaken,
        Self::StillRunning,
        Self::SpawnFailed,
    ];

    /// The numeric JSON-RPC code.
    #[must_use]
    pub fn code(self) -> i64 {
        match self {
            Self::Parse => -32700,
            Self::InvalidRequest => -32600,
            Self::MethodNotFound => -32601,
            Self::InvalidParams => -32602,
            Self::Internal => -32603,
            Self::Version => -32000,
            Self::NoHello => -32001,
            Self::NoSuchTerminal => -32002,
            Self::NameTaken => -32003,
            Self::StillRunning => -32004,
            Self::SpawnFailed => -32005,
        }
    }

    /// The name that starts the error message.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Parse => "PARSE",
            Self::InvalidRequest => "INVALID_REQUEST",
            Self::MethodNotFound => "NO_SUCH_METHOD",
            Self::InvalidParams => "INVALID_PARAMS",
            Self::Internal => "INTERNAL",
            Self::Version => "BERTH_VERSION",
            Self::NoHello => "NO_HELLO",
            Self::NoSuchTerminal => "NO_SUCH_TERMINAL",
            Self::NameTaken => "NAME_TAKEN",
            Self::StillRunning => "STILL_RUNNING",
            Self::SpawnFailed => "SPAWN_FAILED",
        }
    }
}

/// The methods, with their parameter and result types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    /// The handshake: [`Hello`] → [`Hello`].
    Hello,
    /// Start a terminal: [`RunParams`] → [`TerminalInfo`].
    Run,
    /// List terminals: [`LsParams`] → `[TerminalInfo]`.
    Ls,
    /// One terminal: [`Target`] → [`TerminalInfo`].
    Get,
    /// Read the screen or scrollback: [`ReadParams`] → [`ReadResult`].
    Read,
    /// Type text: [`SendParams`] → `{}`.
    Send,
    /// Press keys: [`KeysParams`] → `{}`.
    Keys,
    /// End the process tree: [`Target`] → [`TerminalInfo`].
    Kill,
    /// Remove an exited terminal: [`Target`] → [`TerminalInfo`].
    Rm,
    /// Resize a terminal: [`ResizeParams`] → `{}`.
    Resize,
    /// Take over a terminal: [`Target`] → [`TerminalInfo`], then the connection streams raw output.
    Attach,
}

impl Method {
    /// Every method, in documentation order.
    pub const ALL: [Self; 11] = [
        Self::Hello,
        Self::Run,
        Self::Ls,
        Self::Get,
        Self::Read,
        Self::Send,
        Self::Keys,
        Self::Kill,
        Self::Rm,
        Self::Resize,
        Self::Attach,
    ];

    /// The wire name.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Hello => "hello",
            Self::Run => "run",
            Self::Ls => "ls",
            Self::Get => "get",
            Self::Read => "read",
            Self::Send => "send",
            Self::Keys => "keys",
            Self::Kill => "kill",
            Self::Rm => "rm",
            Self::Resize => "resize",
            Self::Attach => "attach",
        }
    }

    /// The method with this wire name.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.name() == name)
    }

    /// One line on what the method does.
    #[must_use]
    pub fn summary(self) -> &'static str {
        match self {
            Self::Hello => {
                "The handshake; must come first on every connection. A different version is refused with `BERTH_VERSION`."
            }
            Self::Run => {
                "Start a terminal. `name` defaults to the first free `tN`; `keep` brings it back as a shell in its cwd after a server restart."
            }
            Self::Ls => {
                "List terminals, by name; `labels` keeps those that carry every given label."
            }
            Self::Get => "One terminal.",
            Self::Read => {
                "The visible screen, or with `history` the scrollback and screen, as plain text; `lines` keeps the last N lines."
            }
            Self::Send => "Type text into the terminal.",
            Self::Keys => {
                "Press keys by name (`enter`, `esc`, `ctrl+c`, `up`, …); nothing is sent if any name is unknown."
            }
            Self::Kill => {
                "End the terminal's whole process tree (SIGTERM, then SIGKILL); returns the exited terminal."
            }
            Self::Rm => "Remove an exited terminal; returns it as it was.",
            Self::Resize => "Resize the terminal; the program gets SIGWINCH.",
            Self::Attach => {
                "Take over the terminal. After the response the server writes a redraw (clear, then the screen text) and the program's raw output, and closes the connection when the program's PTY closes. The client sends only notifications from then on (`send`, `keys`, `resize`); detaching is closing the connection."
            }
        }
    }
}

/// `hello` params and result: the sender's version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    /// The berth version.
    pub version: String,
}

/// `run` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunParams {
    /// The terminal's name, unique per server; `[A-Za-z0-9._-]+`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Labels, `k=v`.
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    /// The working directory (absolute).
    pub cwd: PathBuf,
    /// Come back as a shell after a server restart.
    #[serde(default)]
    pub keep: bool,
    /// The program and its arguments.
    pub cmd: Vec<String>,
}

/// `ls` params.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LsParams {
    /// Only terminals carrying every one of these labels.
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
}

/// `get`, `kill` and `rm` params: the terminal's name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Target {
    /// The terminal's name.
    pub name: String,
}

/// `read` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadParams {
    /// The terminal's name.
    pub name: String,
    /// Only the last N lines.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lines: Option<usize>,
    /// Scrollback and screen instead of the screen alone.
    #[serde(default)]
    pub history: bool,
}

/// `read` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadResult {
    /// Plain text, one line per row, trailing blanks dropped.
    pub text: String,
}

/// `send` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendParams {
    /// The terminal's name.
    pub name: String,
    /// The text, sent as UTF-8.
    pub text: String,
}

/// `keys` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeysParams {
    /// The terminal's name.
    pub name: String,
    /// Key names, pressed in order.
    pub keys: Vec<String>,
}

/// `resize` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResizeParams {
    /// The terminal's name.
    pub name: String,
    /// Rows, at least 1.
    pub rows: u16,
    /// Columns, at least 1.
    pub cols: u16,
}

/// Whether a terminal's program runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    /// The program runs.
    Running,
    /// The program has exited, or was lost in a server restart.
    Exited,
}

/// One terminal, as `run`, `ls`, `get`, `kill` and `rm` return it and the state file stores it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalInfo {
    /// The unique name.
    pub name: String,
    /// Labels, `k=v`.
    pub labels: BTreeMap<String, String>,
    /// The working directory.
    pub cwd: PathBuf,
    /// The program and its arguments.
    pub cmd: Vec<String>,
    /// Comes back as a shell after a server restart.
    pub keep: bool,
    /// Running or exited.
    pub state: State,
    /// The exit status once exited (128 + N for signal N); `null` while running or when lost.
    pub exit: Option<i32>,
    /// The program's pid while running.
    pub pid: Option<i32>,
}

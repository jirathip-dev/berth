//! berth's per-user server: the socket, JSON-RPC, the events stream and machines over SSH.
//!
//! [`serve`] owns the terminals and answers newline-delimited JSON-RPC 2.0 ([`berth_proto`]) on a
//! Unix socket, one thread per connection. [`Client`] is the other end.
#![forbid(unsafe_code)]

mod registry;

use std::ffi::OsString;
use std::fs;
use std::io::{self, BufRead as _, BufReader, Write as _};
use std::net::Shutdown;
use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use berth_core::{Pattern, STATES, Wake};
pub use berth_proto as proto;
use proto::{
    ErrorCode, Hello, Method, Request, Response, RpcError, Target, VERSION, WaitOutcome,
    WaitParams, WaitResult,
};
use registry::{Attached, Registry};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

/// Errors from the server and the client.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// `HOME` is not set.
    #[error("HOME is not set")]
    NoHome,
    /// `XDG_RUNTIME_DIR` is not set (Linux), so there is no default socket path.
    #[error("XDG_RUNTIME_DIR is not set; pass a directory with --dir")]
    NoRuntimeDir,
    /// A server already answers on the socket.
    #[error("a berth server already runs on {0}")]
    AlreadyRunning(PathBuf),
    /// The server answered with an error (including the `BERTH_VERSION` refusal).
    #[error("{0}")]
    Rpc(RpcError),
    /// The server closed the connection or sent something that is not a response.
    #[error("bad response from the server: {0}")]
    BadResponse(String),
    /// A JSON error.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    /// An I/O error.
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Where a server listens and keeps its state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    /// The Unix socket.
    pub socket: PathBuf,
    /// The state file.
    pub state: PathBuf,
}

impl Paths {
    /// `berth.sock` and `state.json`, both in `dir`.
    #[must_use]
    pub fn in_dir(dir: &Path) -> Self {
        Self {
            socket: dir.join("berth.sock"),
            state: dir.join("state.json"),
        }
    }

    /// The per-user defaults. macOS: `~/Library/Caches/berth/berth.sock` and
    /// `~/Library/Application Support/berth/state.json`. Elsewhere: `$XDG_RUNTIME_DIR/berth.sock` and
    /// `$XDG_STATE_HOME/berth/state.json` (default `~/.local/state`).
    ///
    /// # Errors
    /// [`Error::NoHome`] or [`Error::NoRuntimeDir`] when the variables they need are unset.
    pub fn user() -> Result<Self, Error> {
        let var = |k| std::env::var_os(k).filter(|v: &OsString| !v.is_empty());
        let home = var("HOME").map(PathBuf::from).ok_or(Error::NoHome);
        if cfg!(target_os = "macos") {
            let lib = home?.join("Library");
            return Ok(Self {
                socket: lib.join("Caches/berth/berth.sock"),
                state: lib.join("Application Support/berth/state.json"),
            });
        }
        let runtime = var("XDG_RUNTIME_DIR").ok_or(Error::NoRuntimeDir)?;
        let state = match var("XDG_STATE_HOME") {
            Some(dir) => PathBuf::from(dir),
            None => home?.join(".local/state"),
        };
        Ok(Self {
            socket: PathBuf::from(runtime).join("berth.sock"),
            state: state.join("berth/state.json"),
        })
    }
}

fn lock(m: &Mutex<Registry>) -> MutexGuard<'_, Registry> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Loads the state, binds the socket (mode 0600, its directory 0700) and serves until killed.
///
/// # Errors
/// [`Error::AlreadyRunning`] when another server answers on the socket; [`Error::Io`] or
/// [`Error::Json`] when the state file or socket cannot be set up.
pub fn serve(paths: &Paths) -> Result<(), Error> {
    if UnixStream::connect(&paths.socket).is_ok() {
        return Err(Error::AlreadyRunning(paths.socket.clone()));
    }
    let registry = Arc::new(Mutex::new(Registry::load(&paths.state)?));
    if let Some(dir) = paths.socket.parent() {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
    }
    match fs::remove_file(&paths.socket) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e.into()),
        _ => {}
    }
    let listener = UnixListener::bind(&paths.socket)?;
    fs::set_permissions(&paths.socket, fs::Permissions::from_mode(0o600))?;
    eprintln!("berth {VERSION}: listening on {}", paths.socket.display());
    for stream in listener.incoming() {
        let stream = stream?;
        let registry = Arc::clone(&registry);
        std::thread::spawn(move || connection(&registry, &stream));
    }
    Ok(())
}

/// One connection's state.
#[derive(Default)]
struct Session {
    /// The handshake passed.
    hello: bool,
    /// An `attach` just answered: the redraw and the output to stream next.
    attach: Option<Attached>,
    /// The connection streams a terminal's output; responses are no longer written.
    attached: bool,
}

/// Answers one connection's requests, line by line, until it closes.
fn connection(registry: &Mutex<Registry>, stream: &UnixStream) {
    let mut session = Session::default();
    let mut out = stream;
    for line in BufReader::new(stream).lines() {
        let Ok(line) = line else { return };
        if line.trim().is_empty() {
            continue;
        }
        if let Some(response) = respond(registry, &mut session, &line)
            && !session.attached
        {
            let Ok(mut bytes) = serde_json::to_vec(&response) else {
                return;
            };
            bytes.push(b'\n');
            if out.write_all(&bytes).is_err() {
                return;
            }
        }
        if let Some((redraw, output)) = session.attach.take() {
            let Ok(mut out) = stream.try_clone() else {
                return;
            };
            session.attached = true;
            std::thread::spawn(move || {
                let _ = out.write_all(&redraw);
                for chunk in output {
                    if out.write_all(&chunk).is_err() {
                        break;
                    }
                }
                let _ = out.shutdown(Shutdown::Both);
            });
        }
    }
}

/// The response to one request line; `None` for a notification.
fn respond(registry: &Mutex<Registry>, session: &mut Session, line: &str) -> Option<Response> {
    let value: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => {
            return Some(Response::err(
                Value::Null,
                RpcError::new(ErrorCode::Parse, e),
            ));
        }
    };
    let id = value.get("id").cloned().unwrap_or(Value::Null);
    let request: Request = match serde_json::from_value::<Request>(value) {
        Ok(r) if r.jsonrpc == proto::JSONRPC => r,
        Ok(_) => {
            let e = RpcError::new(ErrorCode::InvalidRequest, "jsonrpc must be \"2.0\"");
            return Some(Response::err(id, e));
        }
        Err(e) => {
            return Some(Response::err(
                id,
                RpcError::new(ErrorCode::InvalidRequest, e),
            ));
        }
    };
    let result = dispatch(registry, session, &request);
    let id = request.id?;
    Some(match result {
        Ok(v) => Response::ok(id, v),
        Err(e) => Response::err(id, e),
    })
}

fn params<T: DeserializeOwned>(request: &Request) -> Result<T, RpcError> {
    let p = match &request.params {
        Value::Null => Value::Object(serde_json::Map::new()),
        p => p.clone(),
    };
    serde_json::from_value(p).map_err(|e| RpcError::new(ErrorCode::InvalidParams, e))
}

fn json<T: Serialize>(value: &T) -> Result<Value, RpcError> {
    serde_json::to_value(value).map_err(|e| RpcError::new(ErrorCode::Internal, e))
}

fn dispatch(
    registry: &Mutex<Registry>,
    session: &mut Session,
    request: &Request,
) -> Result<Value, RpcError> {
    let method = Method::parse(&request.method)
        .ok_or_else(|| RpcError::new(ErrorCode::MethodNotFound, &request.method))?;
    if method == Method::Hello {
        let p: Hello = params(request)?;
        if p.version != VERSION {
            return Err(RpcError::version(&p.version, VERSION));
        }
        session.hello = true;
        return json(&Hello {
            version: VERSION.to_owned(),
        });
    }
    if !session.hello {
        return Err(RpcError::new(
            ErrorCode::NoHello,
            "send hello with your version first",
        ));
    }
    if method == Method::Kill {
        let Target { name } = params(request)?;
        let mut reg = lock(registry);
        reg.refresh()?;
        let Some(term) = reg.to_kill(&name)? else {
            return json(&reg.get(&name)?);
        };
        drop(reg);
        let status = term
            .kill(registry::GRACE)
            .map_err(|e| RpcError::new(ErrorCode::Internal, e))?;
        return json(&lock(registry).killed(&name, &term, status)?);
    }
    if method == Method::Wait {
        return json(&wait(registry, &params(request)?)?);
    }
    let mut reg = lock(registry);
    reg.refresh()?;
    match method {
        Method::Run => json(&reg.run(params(request)?)?),
        Method::Ls => json(&reg.ls(&params(request)?)),
        Method::Get => json(&reg.get(&params::<Target>(request)?.name)?),
        Method::Read => json(&reg.read(&params(request)?)?),
        Method::Send => reg
            .send(&params(request)?)
            .map(|()| Value::Object(serde_json::Map::new())),
        Method::Keys => reg
            .keys(&params(request)?)
            .map(|()| Value::Object(serde_json::Map::new())),
        Method::Rm => json(&reg.rm(&params::<Target>(request)?.name)?),
        Method::Resize => reg
            .resize(&params(request)?)
            .map(|()| Value::Object(serde_json::Map::new())),
        Method::Attach => {
            let (info, attached) = reg.attach(&params::<Target>(request)?.name)?;
            session.attach = Some(attached);
            json(&info)
        }
        Method::State => json(&reg.state(params(request)?)?),
        Method::Hello | Method::Kill | Method::Wait => {
            Err(RpcError::new(ErrorCode::Internal, "handled above"))
        }
    }
}

/// `wait`: blocks this connection, not the registry, until a state or output matches, the program
/// exits or the timeout passes.
fn wait(registry: &Mutex<Registry>, p: &WaitParams) -> Result<WaitResult, RpcError> {
    let bad = |e: &dyn std::fmt::Display| RpcError::new(ErrorCode::InvalidParams, e);
    if p.until.is_empty() && p.output.is_none() {
        return Err(bad(&"give until, output or both"));
    }
    if let Some(s) = p
        .until
        .iter()
        .find(|s| *s != "idle" && !STATES.contains(&s.as_str()))
    {
        return Err(bad(&format!("unknown state {s:?}")));
    }
    let pattern = p.output.as_deref().map(Pattern::new).transpose();
    let pattern = pattern.map_err(|e| bad(&e))?;
    let timeout = p.timeout.map(Duration::try_from_secs_f64).transpose();
    // A timeout too far out to add to the clock waits for ever.
    let deadline = timeout
        .map_err(|e| bad(&e))?
        .and_then(|d| Instant::now().checked_add(d));
    let (term, info) = {
        let mut reg = lock(registry);
        reg.refresh()?;
        reg.to_wait(&p.name)?
    };
    let reported = |state: &str| p.until.iter().any(|s| s == state);
    let outcome = match term {
        Some(term) => {
            let done = |t: &berth_core::Terminal| {
                t.report().is_some_and(|r| reported(&r.state))
                    || pattern
                        .as_ref()
                        .is_some_and(|re| t.screen().lines().any(|l| re.is_match(l)))
            };
            match term
                .wait_until(done, deadline)
                .map_err(|e| RpcError::new(ErrorCode::Internal, e))?
            {
                Wake::Matched => WaitOutcome::Matched,
                Wake::Exited => WaitOutcome::Exited,
                Wake::Timeout => WaitOutcome::Timeout,
            }
        }
        None if info.reported.is_some_and(|r| reported(&r.state)) => WaitOutcome::Matched,
        None => WaitOutcome::Exited,
    };
    let mut reg = lock(registry);
    reg.refresh()?;
    Ok(WaitResult {
        outcome,
        terminal: reg.get(&p.name)?,
    })
}

/// A connection to a server, past the version handshake.
pub struct Client {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
    next_id: u64,
}

impl Client {
    /// Connects to the server on `socket` and checks it runs this version.
    ///
    /// # Errors
    /// [`Error::Rpc`] with `BERTH_VERSION: client x, server y; restart the server` on a version
    /// mismatch; [`Error::Io`] when nothing listens.
    pub fn connect(socket: &Path) -> Result<Self, Error> {
        let writer = UnixStream::connect(socket)?;
        let mut client = Self {
            reader: BufReader::new(writer.try_clone()?),
            writer,
            next_id: 1,
        };
        let server: Hello = client.call(
            Method::Hello,
            &Hello {
                version: VERSION.to_owned(),
            },
        )?;
        if server.version != VERSION {
            return Err(Error::Rpc(RpcError::version(VERSION, &server.version)));
        }
        Ok(client)
    }

    /// Calls `method` with `params` and decodes its result.
    ///
    /// # Errors
    /// [`Error::Rpc`] when the server answers with an error; [`Error::BadResponse`],
    /// [`Error::Json`] or [`Error::Io`] when the exchange itself fails.
    pub fn call<P: Serialize, R: DeserializeOwned>(
        &mut self,
        method: Method,
        params: &P,
    ) -> Result<R, Error> {
        let request = Request::new(self.next_id, method, serde_json::to_value(params)?);
        self.next_id += 1;
        let mut line = serde_json::to_vec(&request)?;
        line.push(b'\n');
        self.writer.write_all(&line)?;
        let mut reply = String::new();
        if self.reader.read_line(&mut reply)? == 0 {
            return Err(Error::BadResponse("connection closed".to_owned()));
        }
        let response: Response = serde_json::from_str(&reply)?;
        if response.id != request.id.unwrap_or_default() {
            return Err(Error::BadResponse(format!("id {}", response.id)));
        }
        match (response.result, response.error) {
            (_, Some(e)) => Err(Error::Rpc(e)),
            (Some(result), None) => Ok(serde_json::from_value(result)?),
            (None, None) => Err(Error::BadResponse("neither result nor error".to_owned())),
        }
    }

    /// The connection's reader (with anything it buffered) and writer: after `attach`, the reader
    /// carries the terminal's output and the writer takes [`notification`]s.
    #[must_use]
    pub fn split(self) -> (BufReader<UnixStream>, UnixStream) {
        (self.reader, self.writer)
    }
}

/// One notification line (a request without an id, so no response) for `method` with `params`.
///
/// # Errors
/// [`Error::Json`] when `params` cannot be serialised.
pub fn notification<P: Serialize>(method: Method, params: &P) -> Result<Vec<u8>, Error> {
    let mut request = Request::new(0, method, serde_json::to_value(params)?);
    request.id = None;
    let mut line = serde_json::to_vec(&request)?;
    line.push(b'\n');
    Ok(line)
}

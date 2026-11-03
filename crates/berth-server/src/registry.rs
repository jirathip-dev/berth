//! The terminals a server owns, and the state file that outlives it.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::os::unix::process::ExitStatusExt as _;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::Duration;

use berth_core::{Size, Terminal};
use berth_proto::{
    ErrorCode, KeysParams, LsParams, ReadParams, ReadResult, ResizeParams, RpcError, RunParams,
    SendParams, State, TerminalInfo,
};
use serde::{Deserialize, Serialize};

/// The size new terminals start at.
const SIZE: Size = Size { rows: 24, cols: 80 };
/// How long `kill` waits after SIGTERM before SIGKILL.
pub(crate) const GRACE: Duration = Duration::from_secs(2);
/// The state file's format version.
const FILE_VERSION: u32 = 1;

/// What `attach` streams: a redraw of the screen, then the raw output.
pub(crate) type Attached = (Vec<u8>, Receiver<Vec<u8>>);

/// The state file: every terminal's last known info.
#[derive(Serialize, Deserialize)]
struct StateFile {
    version: u32,
    terminals: Vec<TerminalInfo>,
}

struct Entry {
    info: TerminalInfo,
    /// The live (or exited but still readable) terminal; `None` for one lost in a restart.
    term: Option<Arc<Terminal>>,
}

/// The server's terminals by name, saved to `path` after every change.
pub(crate) struct Registry {
    entries: BTreeMap<String, Entry>,
    path: PathBuf,
}

fn not_found(name: &str) -> RpcError {
    RpcError::new(ErrorCode::NoSuchTerminal, name)
}

fn internal(e: impl std::fmt::Display) -> RpcError {
    RpcError::new(ErrorCode::Internal, e)
}

fn exit_code(status: ExitStatus) -> Option<i32> {
    status.code().or_else(|| status.signal().map(|s| 128 + s))
}

fn spawn(cmd: &[String], cwd: &Path) -> Result<Terminal, berth_core::Error> {
    Terminal::spawn(cmd, cwd, std::iter::empty::<(&str, &str)>(), SIZE)
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

impl Registry {
    /// Loads the state file at `path` (none yet: no terminals). Running kept terminals come back as
    /// `$SHELL` (or `/bin/sh`) in their cwd; other running ones are listed as exited. The result is saved at once.
    pub(crate) fn load(path: &Path) -> io::Result<Self> {
        let file: StateFile = match fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes)?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => StateFile {
                version: FILE_VERSION,
                terminals: Vec::new(),
            },
            Err(e) => return Err(e),
        };
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_owned());
        let mut entries = BTreeMap::new();
        for mut info in file.terminals {
            let mut term = None;
            if info.state == State::Running {
                if info.keep {
                    let cmd = vec![shell.clone()];
                    term = spawn(&cmd, &info.cwd).ok().map(Arc::new);
                    if term.is_some() {
                        info.cmd = cmd;
                    }
                }
                if term.is_none() {
                    (info.state, info.exit) = (State::Exited, None);
                }
            }
            info.pid = term.as_ref().map(|t| t.pid());
            entries.insert(info.name.clone(), Entry { info, term });
        }
        let registry = Self {
            entries,
            path: path.to_owned(),
        };
        registry.save()?;
        Ok(registry)
    }

    /// Writes the state file atomically (temp file, fsync, rename), mode 0600.
    fn save(&self) -> io::Result<()> {
        if let Some(dir) = self.path.parent() {
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(dir)?;
        }
        let file = StateFile {
            version: FILE_VERSION,
            terminals: self.entries.values().map(|e| e.info.clone()).collect(),
        };
        let tmp = self.path.with_extension("json.tmp");
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        serde_json::to_writer_pretty(&mut f, &file)?;
        f.write_all(b"\n")?;
        f.sync_all()?;
        fs::rename(&tmp, &self.path)
    }

    fn saved<T>(&self, value: T) -> Result<T, RpcError> {
        self.save().map_err(internal)?;
        Ok(value)
    }

    /// Marks terminals whose program has exited, saving when any changed.
    pub(crate) fn refresh(&mut self) -> Result<(), RpcError> {
        let mut changed = false;
        for entry in self.entries.values_mut() {
            if entry.info.state != State::Running {
                continue;
            }
            if let Some(term) = &entry.term
                && let Some(status) = term.try_wait().map_err(internal)?
            {
                (entry.info.state, entry.info.exit) = (State::Exited, exit_code(status));
                entry.info.pid = None;
                changed = true;
            }
        }
        if changed {
            self.save().map_err(internal)?;
        }
        Ok(())
    }

    fn entry(&self, name: &str) -> Result<&Entry, RpcError> {
        self.entries.get(name).ok_or_else(|| not_found(name))
    }

    fn live(&self, name: &str) -> Result<&Terminal, RpcError> {
        let entry = self.entry(name)?;
        match (&entry.term, entry.info.state) {
            (Some(term), State::Running) => Ok(term),
            _ => Err(RpcError::new(
                ErrorCode::NoSuchTerminal,
                format!("{name} has exited"),
            )),
        }
    }

    /// `run`: starts a terminal.
    pub(crate) fn run(&mut self, p: RunParams) -> Result<TerminalInfo, RpcError> {
        let bad = |what: &str| RpcError::new(ErrorCode::InvalidParams, what);
        if p.cmd.is_empty() {
            return Err(bad("empty cmd"));
        }
        if !p.cwd.is_absolute() {
            return Err(bad("cwd must be absolute"));
        }
        if p.labels.keys().any(|k| k.is_empty() || k.contains('=')) {
            return Err(bad("label keys must be non-empty and have no '='"));
        }
        let name = match p.name {
            Some(name) if !valid_name(&name) => return Err(bad("name must match [A-Za-z0-9._-]+")),
            Some(name) if self.entries.contains_key(&name) => {
                return Err(RpcError::new(ErrorCode::NameTaken, name));
            }
            Some(name) => name,
            // len + 1 candidates for len names: one is always free, so `find` returns Some.
            None => (1..=self.entries.len() + 1)
                .map(|n| format!("t{n}"))
                .find(|n| !self.entries.contains_key(n))
                .unwrap_or_default(),
        };
        let term = spawn(&p.cmd, &p.cwd)
            .map_err(|e| RpcError::new(ErrorCode::SpawnFailed, format!("{name}: {e}")))?;
        let info = TerminalInfo {
            name: name.clone(),
            labels: p.labels,
            cwd: p.cwd,
            cmd: p.cmd,
            keep: p.keep,
            state: State::Running,
            exit: None,
            pid: Some(term.pid()),
        };
        self.entries.insert(
            name,
            Entry {
                info: info.clone(),
                term: Some(Arc::new(term)),
            },
        );
        self.saved(info)
    }

    /// `ls`: terminals carrying every label in `p`, by name.
    pub(crate) fn ls(&self, p: &LsParams) -> Vec<TerminalInfo> {
        self.entries
            .values()
            .filter(|e| {
                p.labels
                    .iter()
                    .all(|(k, v)| e.info.labels.get(k) == Some(v))
            })
            .map(|e| e.info.clone())
            .collect()
    }

    /// `get`: one terminal.
    pub(crate) fn get(&self, name: &str) -> Result<TerminalInfo, RpcError> {
        Ok(self.entry(name)?.info.clone())
    }

    /// `read`: screen or history text; empty for a terminal lost in a restart.
    pub(crate) fn read(&self, p: &ReadParams) -> Result<ReadResult, RpcError> {
        let Some(term) = &self.entry(&p.name)?.term else {
            return Ok(ReadResult {
                text: String::new(),
            });
        };
        let text = match (p.history, p.lines) {
            (true, n) => term.history(n.unwrap_or(usize::MAX)),
            (false, None) => term.screen(),
            (false, Some(n)) => {
                let screen = term.screen();
                let lines: Vec<&str> = screen.lines().collect();
                lines[lines.len().saturating_sub(n)..].join("\n")
            }
        };
        Ok(ReadResult { text })
    }

    /// `send`: types text.
    pub(crate) fn send(&self, p: &SendParams) -> Result<(), RpcError> {
        self.live(&p.name)?
            .write(p.text.as_bytes())
            .map_err(internal)
    }

    /// `keys`: presses keys; sends nothing if any name is unknown.
    pub(crate) fn keys(&self, p: &KeysParams) -> Result<(), RpcError> {
        let term = self.live(&p.name)?;
        let bytes = p
            .keys
            .iter()
            .map(|k| berth_core::keys::encode(k))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| RpcError::new(ErrorCode::InvalidParams, e))?;
        term.write(&bytes.concat()).map_err(internal)
    }

    /// `resize`: resizes a running terminal.
    pub(crate) fn resize(&self, p: &ResizeParams) -> Result<(), RpcError> {
        if p.rows == 0 || p.cols == 0 {
            return Err(RpcError::new(
                ErrorCode::InvalidParams,
                "rows and cols must be at least 1",
            ));
        }
        let size = Size {
            rows: p.rows,
            cols: p.cols,
        };
        self.live(&p.name)?.resize(size).map_err(internal)
    }

    /// `attach`: a running terminal, a redraw of its screen, and its raw output from now on.
    pub(crate) fn attach(&self, name: &str) -> Result<(TerminalInfo, Attached), RpcError> {
        let term = self.live(name)?;
        let output = term.subscribe();
        let redraw = format!("\x1b[H\x1b[2J{}", term.screen().replace('\n', "\r\n"));
        Ok((self.get(name)?, (redraw.into_bytes(), output)))
    }

    /// The terminal to kill, if it still runs; the caller kills it without holding the registry.
    pub(crate) fn to_kill(&self, name: &str) -> Result<Option<Arc<Terminal>>, RpcError> {
        let entry = self.entry(name)?;
        Ok(match entry.info.state {
            State::Running => entry.term.clone(),
            State::Exited => None,
        })
    }

    /// Records the exit of a terminal `kill` ended; returns its info.
    pub(crate) fn killed(
        &mut self,
        name: &str,
        term: &Arc<Terminal>,
        status: ExitStatus,
    ) -> Result<TerminalInfo, RpcError> {
        let entry = self.entries.get_mut(name).ok_or_else(|| not_found(name))?;
        if entry.term.as_ref().is_some_and(|t| Arc::ptr_eq(t, term)) {
            (entry.info.state, entry.info.exit) = (State::Exited, exit_code(status));
            entry.info.pid = None;
        }
        let info = entry.info.clone();
        self.saved(info)
    }

    /// `rm`: removes an exited terminal.
    pub(crate) fn rm(&mut self, name: &str) -> Result<TerminalInfo, RpcError> {
        if self.entry(name)?.info.state == State::Running {
            return Err(RpcError::new(ErrorCode::StillRunning, name));
        }
        let entry = self.entries.remove(name).ok_or_else(|| not_found(name))?;
        self.saved(entry.info)
    }
}

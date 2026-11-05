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
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use shed_core::proctree::{self, ProcessInfo};
use shed_core::{History, Report, STATES, Size, Terminal};
use shed_proto::{
    EmitParams, ErrorCode, EventKind, EventsParams, KeysParams, LsParams, ReadParams, ReadResult,
    Reported, ResizeParams, Resources, RpcError, RunParams, SendParams, State, StateParams,
    TerminalInfo,
};

use crate::events::Events;

/// The size new terminals start at.
const SIZE: Size = Size { rows: 24, cols: 80 };
/// How long `kill` waits after SIGTERM before SIGKILL.
pub(crate) const GRACE: Duration = Duration::from_secs(2);
/// The state file's format version.
const FILE_VERSION: u32 = 1;

/// What `attach` and `events` stream: a redraw of the screen or the replayed events, then the raw
/// output or the event lines.
pub(crate) type Stream = (Vec<u8>, Receiver<Vec<u8>>);

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
    /// `kill` is ending it: its exit is a `killed` event, not `exited`.
    killing: bool,
    /// The tree's use at the last sample, and the use the last `resources` event reported.
    resources: Option<Resources>,
    emitted: Option<Resources>,
    /// When the tree was last sampled and its CPU time then.
    sampled: (Instant, Duration),
}

impl Entry {
    fn new(info: TerminalInfo, term: Option<Arc<Terminal>>) -> Self {
        Self {
            info,
            term,
            killing: false,
            resources: None,
            emitted: None,
            sampled: (Instant::now(), Duration::ZERO),
        }
    }

    /// The info with the last sample, as `ls` and `get` show it.
    fn view(&self) -> TerminalInfo {
        TerminalInfo {
            resources: self.resources,
            ..self.info.clone()
        }
    }
}

/// The server's terminals by name, saved to `path` after every change.
pub(crate) struct Registry {
    entries: BTreeMap<String, Entry>,
    path: PathBuf,
    events: Events,
}

fn not_found(name: &str) -> RpcError {
    RpcError::new(ErrorCode::NoSuchTerminal, name)
}

fn internal(e: impl std::fmt::Display) -> RpcError {
    RpcError::new(ErrorCode::Internal, e)
}

/// More than 10 % away from the last reported value.
fn moved(old: u64, new: u64) -> bool {
    new.abs_diff(old) * 10 > old
}

fn exit_code(status: ExitStatus) -> Option<i32> {
    status.code().or_else(|| status.signal().map(|s| 128 + s))
}

/// Terminal `name`'s history: the default caps, and its output log beside the state file at `state`.
fn history(state: &Path, name: &str) -> History {
    History {
        log: Some(state.with_file_name("history").join(format!("{name}.log"))),
        ..History::default()
    }
}

/// Starts `cmd` with `$SHED` set to the terminal's name, so `shed state` inside needs no name. The
/// screen starts from the tail of the terminal's log, if any.
fn spawn(
    cmd: &[String],
    cwd: &Path,
    name: &str,
    idle_after: Option<u64>,
    history: &History,
) -> Result<Terminal, shed_core::Error> {
    // $BERTH, the old name, stays for one release.
    let env = [("SHED", name), ("BERTH", name)];
    let term = Terminal::spawn_with(cmd, cwd, env, SIZE, history)?;
    term.set_idle_after(idle_after.map(Duration::from_secs));
    Ok(term)
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
        let mut events = Events::default();
        for mut info in file.terminals {
            let mut term = None;
            if info.state == State::Running {
                if info.keep {
                    let cmd = vec![shell.clone()];
                    let history = history(path, &info.name);
                    term = spawn(&cmd, &info.cwd, &info.name, info.idle_after, &history)
                        .ok()
                        .map(Arc::new);
                    if term.is_some() {
                        info.cmd = cmd;
                    }
                }
                if term.is_none() {
                    (info.state, info.exit) = (State::Exited, None);
                }
            }
            info.pid = term.as_ref().map(|t| t.pid());
            if term.is_some() {
                events.emit(&info.name, &info.labels, EventKind::Started);
            }
            entries.insert(info.name.clone(), Entry::new(info, term));
        }
        let registry = Self {
            entries,
            path: path.to_owned(),
            events,
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

    /// Copies each terminal's reported state and marks those whose program has exited, emitting the
    /// events and saving when any changed.
    pub(crate) fn refresh(&mut self) -> Result<(), RpcError> {
        let mut changed = false;
        for entry in self.entries.values_mut() {
            let Some(term) = &entry.term else { continue };
            let reported = term.report().map(|r| Reported {
                state: r.state,
                note: r.note,
            });
            if entry.info.reported != reported {
                entry.info.reported.clone_from(&reported);
                if let Some(r) = reported {
                    let (name, labels) = (&entry.info.name, &entry.info.labels);
                    self.events.emit(name, labels, EventKind::State(r));
                }
                changed = true;
            }
            if entry.info.state == State::Running
                && let Some(status) = term.try_wait().map_err(internal)?
            {
                Self::exited(&mut self.events, entry, status);
                changed = true;
            }
        }
        if changed {
            self.save().map_err(internal)?;
        }
        Ok(())
    }

    /// Marks a running entry exited and emits `killed` or `exited`.
    fn exited(events: &mut Events, entry: &mut Entry, status: ExitStatus) {
        let code = exit_code(status);
        (entry.info.state, entry.info.exit) = (State::Exited, code);
        entry.info.pid = None;
        entry.resources = None;
        let kind = if entry.killing {
            EventKind::Killed
        } else {
            EventKind::Exited { code }
        };
        events.emit(&entry.info.name, &entry.info.labels, kind);
    }

    /// Records each running tree's memory and CPU from `procs`, emitting `resources` when either moved
    /// by more than 10 % since the last one.
    pub(crate) fn sample(&mut self, procs: &[ProcessInfo], now: Instant) {
        for entry in self.entries.values_mut() {
            let Some(pid) = entry.info.pid else { continue };
            let tree = proctree::tree(procs, pid, &std::collections::BTreeSet::new());
            if tree.is_empty() {
                continue;
            }
            let usage = proctree::usage(&tree);
            let (then, cpu) = entry.sampled;
            let wall = now.saturating_duration_since(then).as_micros().max(1);
            let busy = usage.cpu.saturating_sub(cpu).as_micros();
            entry.sampled = (now, usage.cpu);
            let cpu_pct = u64::try_from((busy * 100 + wall / 2) / wall).unwrap_or(u64::MAX);
            let now = Resources {
                rss_mb: usage.rss_bytes / (1 << 20),
                cpu_pct,
            };
            entry.resources = Some(now);
            if entry
                .emitted
                .is_none_or(|old| moved(old.rss_mb, now.rss_mb) || moved(old.cpu_pct, now.cpu_pct))
            {
                entry.emitted = Some(now);
                let (name, labels) = (&entry.info.name, &entry.info.labels);
                self.events.emit(name, labels, EventKind::Resources(now));
            }
        }
    }

    /// `events`: the replay and the stream.
    pub(crate) fn subscribe(&mut self, p: EventsParams) -> Stream {
        self.events.subscribe(p)
    }

    /// The live terminals, to watch.
    pub(crate) fn running(&self) -> Vec<Arc<Terminal>> {
        self.entries
            .values()
            .filter(|e| e.info.state == State::Running)
            .filter_map(|e| e.term.clone())
            .collect()
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
        // A new terminal starts a fresh log.
        let history = history(&self.path, &name);
        if let Some(log) = &history.log {
            let _ = fs::remove_file(log);
        }
        let term = spawn(&p.cmd, &p.cwd, &name, p.idle_after, &history)
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
            reported: None,
            idle_after: p.idle_after,
            resources: None,
            acp: p.acp,
        };
        self.events.emit(&name, &info.labels, EventKind::Started);
        let entry = Entry::new(info.clone(), Some(Arc::new(term)));
        self.entries.insert(name, entry);
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
            .map(Entry::view)
            .collect()
    }

    /// `get`: one terminal.
    pub(crate) fn get(&self, name: &str) -> Result<TerminalInfo, RpcError> {
        Ok(self.entry(name)?.view())
    }

    /// `read`: screen or history text, or an ACP terminal's transcript rendered; for a terminal lost in
    /// a restart, from the tail of its log.
    pub(crate) fn read(&self, p: &ReadParams) -> Result<ReadResult, RpcError> {
        let entry = self.entry(&p.name)?;
        if entry.info.acp.is_some() {
            let file = fs::read_to_string(crate::acp::transcript(&self.path, &p.name));
            let text = crate::acp::render(&file.unwrap_or_default());
            let lines: Vec<&str> = text.lines().collect();
            let n = p.lines.unwrap_or(usize::MAX);
            let text = lines[lines.len().saturating_sub(n)..].join("\n");
            return Ok(ReadResult { text });
        }
        let n = p.lines.unwrap_or(usize::MAX);
        let text = match &entry.term {
            Some(term) if p.history => term.history(n),
            Some(term) => term.screen(),
            None => {
                let screen = history(&self.path, &p.name)
                    .restore(SIZE)
                    .map_err(internal)?;
                if p.history {
                    screen.history(n)
                } else {
                    screen.text()
                }
            }
        };
        if p.history {
            return Ok(ReadResult { text });
        }
        let lines: Vec<&str> = text.lines().collect();
        let text = lines[lines.len().saturating_sub(n)..].join("\n");
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
            .map(|k| shed_core::keys::encode(k))
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
    pub(crate) fn attach(&self, name: &str) -> Result<(TerminalInfo, Stream), RpcError> {
        let term = self.live(name)?;
        let output = term.subscribe();
        let redraw = format!("\x1b[H\x1b[2J{}", term.screen().replace('\n', "\r\n"));
        Ok((self.get(name)?, (redraw.into_bytes(), output)))
    }

    /// `state`: records a running terminal's reported state.
    pub(crate) fn state(&mut self, p: StateParams) -> Result<TerminalInfo, RpcError> {
        if !STATES.contains(&p.report.state.as_str()) {
            return Err(RpcError::new(
                ErrorCode::InvalidParams,
                "state must be working, blocked, done or error",
            ));
        }
        self.live(&p.name)?.set_report(Report {
            state: p.report.state,
            note: p.report.note,
        });
        self.refresh()?;
        self.get(&p.name)
    }

    /// `emit`: an ACP terminal's structured event.
    pub(crate) fn emit(&mut self, p: EmitParams) -> Result<(), RpcError> {
        if !p.kind.is_acp() {
            return Err(RpcError::new(
                ErrorCode::InvalidParams,
                "emit takes only ACP events",
            ));
        }
        self.live(&p.name)?;
        let labels = self.entry(&p.name)?.info.labels.clone();
        self.events.emit(&p.name, &labels, p.kind);
        Ok(())
    }

    /// The terminal to wait on (`None` when lost in a restart) and its info; the caller waits without
    /// holding the registry.
    pub(crate) fn to_wait(
        &self,
        name: &str,
    ) -> Result<(Option<Arc<Terminal>>, TerminalInfo), RpcError> {
        let entry = self.entry(name)?;
        Ok((entry.term.clone(), entry.info.clone()))
    }

    /// The terminal to kill, if it still runs, marked so its exit is `killed`; the caller kills it
    /// without holding the registry.
    pub(crate) fn mark_kill(&mut self, name: &str) -> Result<Option<Arc<Terminal>>, RpcError> {
        let entry = self.entries.get_mut(name).ok_or_else(|| not_found(name))?;
        entry.killing = entry.info.state == State::Running;
        Ok(entry.term.clone().filter(|_| entry.killing))
    }

    /// Records the exit of a terminal `kill` ended; returns its info.
    pub(crate) fn killed(
        &mut self,
        name: &str,
        term: &Arc<Terminal>,
        status: ExitStatus,
    ) -> Result<TerminalInfo, RpcError> {
        let entry = self.entries.get_mut(name).ok_or_else(|| not_found(name))?;
        if entry.info.state == State::Running
            && entry.term.as_ref().is_some_and(|t| Arc::ptr_eq(t, term))
        {
            Self::exited(&mut self.events, entry, status);
        }
        let info = entry.view();
        self.saved(info)
    }

    /// `rm`: removes an exited terminal.
    pub(crate) fn rm(&mut self, name: &str) -> Result<TerminalInfo, RpcError> {
        if self.entry(name)?.info.state == State::Running {
            return Err(RpcError::new(ErrorCode::StillRunning, name));
        }
        let entry = self.entries.remove(name).ok_or_else(|| not_found(name))?;
        if let Some(log) = history(&self.path, name).log {
            let _ = fs::remove_file(log);
        }
        self.saved(entry.info)
    }
}

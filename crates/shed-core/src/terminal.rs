//! A terminal: one program on a PTY, its screen, and its process tree.

use std::ffi::OsStr;
use std::io::{Read as _, Write as _};
use std::path::Path;
use std::process::{Child, ExitStatus};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use rustix::process::{Pid, Signal, kill_process_group};

use crate::history::{History, Log};
use crate::proctree::{self, ProcessTable as _, Usage};
use crate::screen::{Report, Screen, Size};
use crate::{Error, keys};

/// How soon [`Terminal::wait_until`] looks again for the exit after the PTY closed but before the
/// program was reaped.
const REAP: Duration = Duration::from_millis(20);

/// Why [`Terminal::wait_until`] returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wake {
    /// The condition holds.
    Matched,
    /// The program exited first.
    Exited,
    /// The deadline passed first.
    Timeout,
}

/// What waiters watch, shared with the reader thread.
struct Status {
    /// Bumped on every output chunk, report and PTY close.
    changes: u64,
    closed: bool,
    last_output: Instant,
    report: Option<Report>,
    idle_after: Option<Duration>,
}

impl Status {
    /// When the silence fallback reports `idle`: only while nothing was reported.
    fn idle_at(&self) -> Option<Instant> {
        match self.report {
            Some(_) => None,
            None => self
                .idle_after
                .and_then(|d| self.last_output.checked_add(d)),
        }
    }
}

struct Watch {
    status: Mutex<Status>,
    changed: Condvar,
}

impl Watch {
    /// Applies `change` and wakes every waiter.
    fn bump(&self, change: impl FnOnce(&mut Status)) {
        let mut status = lock(&self.status);
        change(&mut status);
        status.changes += 1;
        self.changed.notify_all();
    }
}

/// A program running on a PTY, in its own session and process group.
///
/// Its output feeds a [`Screen`] from a reader thread. Dropping the terminal SIGKILLs its process group.
pub struct Terminal {
    pty: Arc<pty_process::blocking::Pty>,
    child: Mutex<Child>,
    screen: Arc<Mutex<Screen>>,
    outputs: Arc<Outputs>,
    watch: Arc<Watch>,
    pid: i32,
}

/// The senders of [`Terminal::subscribe`]rs; `None` once the PTY has closed.
type Outputs = Mutex<Option<Vec<Sender<Vec<u8>>>>>;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Terminal {
    /// Starts `cmd` (program and arguments) in `cwd` on a new PTY of `size`, with the default
    /// [`History`] caps and no log.
    ///
    /// The program inherits this process's environment plus `TERM=xterm-256color`, then `env`.
    ///
    /// # Errors
    /// [`Error::EmptyCommand`] when `cmd` is empty; [`Error::Pty`] when the PTY cannot be opened or the
    /// program cannot start.
    pub fn spawn<S, K, V>(
        cmd: &[S],
        cwd: &Path,
        env: impl IntoIterator<Item = (K, V)>,
        size: Size,
    ) -> Result<Self, Error>
    where
        S: AsRef<OsStr>,
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        Self::spawn_with(cmd, cwd, env, size, &History::default())
    }

    /// [`spawn`](Self::spawn) keeping `history`: its caps bound the scrollback, the screen starts from
    /// the tail of its log, and the output is appended to the log.
    ///
    /// # Errors
    /// As [`spawn`](Self::spawn); [`Error::Io`] when the log cannot be read or opened.
    pub fn spawn_with<S, K, V>(
        cmd: &[S],
        cwd: &Path,
        env: impl IntoIterator<Item = (K, V)>,
        size: Size,
        history: &History,
    ) -> Result<Self, Error>
    where
        S: AsRef<OsStr>,
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        let (program, args) = cmd.split_first().ok_or(Error::EmptyCommand)?;
        let screen = Arc::new(Mutex::new(history.restore(size)?));
        let log = Log::open(history)?;
        let (pty, pts) = pty_process::blocking::open()?;
        pty.resize(pty_size(size))?;
        let child = pty_process::blocking::Command::new(program)
            .args(args)
            .current_dir(cwd)
            .env("TERM", "xterm-256color")
            .envs(env)
            .spawn(pts)?;
        let pid = i32::try_from(child.id())
            .map_err(|_| Error::Io(std::io::Error::other("pid out of range")))?;
        let pty = Arc::new(pty);
        let outputs = Arc::new(Mutex::new(Some(Vec::new())));
        let watch = Arc::new(Watch {
            status: Mutex::new(Status {
                changes: 0,
                closed: false,
                last_output: Instant::now(),
                report: None,
                idle_after: None,
            }),
            changed: Condvar::new(),
        });
        let (reader, sink, subs) = (Arc::clone(&pty), Arc::clone(&screen), Arc::clone(&outputs));
        let watcher = Arc::clone(&watch);
        std::thread::spawn(move || {
            read_loop(&reader, &sink, &subs, &watcher, log);
            *lock(&subs) = None;
            watcher.bump(|s| s.closed = true);
        });
        Ok(Self {
            pty,
            child: Mutex::new(child),
            screen,
            outputs,
            watch,
            pid,
        })
    }

    /// The program's process id; it also leads the terminal's session and process group.
    #[must_use]
    pub fn pid(&self) -> i32 {
        self.pid
    }

    /// The visible screen as plain text.
    #[must_use]
    pub fn screen(&self) -> String {
        lock(&self.screen).text()
    }

    /// The last `n` lines of scrollback and screen as plain text.
    #[must_use]
    pub fn history(&self, n: usize) -> String {
        lock(&self.screen).history(n)
    }

    /// The state the program last reported (`shed state` or the OSC 7501 escape). Without one, `idle`
    /// once the program has been silent for the [`set_idle_after`](Self::set_idle_after) time.
    #[must_use]
    pub fn report(&self) -> Option<Report> {
        let status = lock(&self.watch.status);
        let idle = status.idle_at().is_some_and(|at| Instant::now() >= at);
        status.report.clone().or_else(|| {
            idle.then(|| Report {
                state: "idle".to_owned(),
                note: None,
            })
        })
    }

    /// Records a reported state, as if the program had sent the escape.
    pub fn set_report(&self, report: Report) {
        self.watch.bump(|s| s.report = Some(report));
    }

    /// Turns the silence fallback on (`Some`) or off: [`report`](Self::report) says `idle` after this
    /// long without output, until the program reports a state.
    pub fn set_idle_after(&self, after: Option<Duration>) {
        self.watch.bump(|s| s.idle_after = after);
    }

    /// Blocks until `done` holds, the program exits or `deadline` passes, checking `done` again on every
    /// output chunk and report and when the idle fallback comes due (no polling).
    ///
    /// # Errors
    /// [`Error::Io`] when the exit status cannot be read.
    pub fn wait_until(
        &self,
        mut done: impl FnMut(&Self) -> bool,
        deadline: Option<Instant>,
    ) -> Result<Wake, Error> {
        loop {
            let seen = lock(&self.watch.status).changes;
            if done(self) {
                return Ok(Wake::Matched);
            }
            if self.try_wait()?.is_some() {
                return Ok(Wake::Exited);
            }
            let mut status = lock(&self.watch.status);
            let now = Instant::now();
            if deadline.is_some_and(|d| now >= d) {
                return Ok(Wake::Timeout);
            }
            let idle = status.idle_at().filter(|at| *at > now);
            let reap = status.closed.then(|| now + REAP);
            let wake = [deadline, idle, reap].into_iter().flatten().min();
            while status.changes == seen {
                let Some(at) = wake else {
                    status = self
                        .watch
                        .changed
                        .wait(status)
                        .unwrap_or_else(PoisonError::into_inner);
                    continue;
                };
                let Some(left) = at.checked_duration_since(Instant::now()) else {
                    break;
                };
                status = match self.watch.changed.wait_timeout(status, left) {
                    Ok((status, _)) => status,
                    Err(e) => e.into_inner().0,
                };
            }
        }
    }

    /// The program's raw output from now on, chunk by chunk; the channel closes when the PTY does.
    #[must_use]
    pub fn subscribe(&self) -> Receiver<Vec<u8>> {
        let (tx, rx) = mpsc::channel();
        if let Some(subs) = lock(&self.outputs).as_mut() {
            subs.push(tx);
        }
        rx
    }

    /// Writes raw bytes to the program's input.
    ///
    /// # Errors
    /// [`Error::Io`] when the PTY is closed.
    pub fn write(&self, bytes: &[u8]) -> Result<(), Error> {
        Ok((&*self.pty).write_all(bytes)?)
    }

    /// Sends one key by name (`enter`, `ctrl+c`, `up`, …); see [`keys::encode`].
    ///
    /// # Errors
    /// [`Error::UnknownKey`] for a name not in the table; [`Error::Io`] when the PTY is closed.
    pub fn press(&self, key: &str) -> Result<(), Error> {
        self.write(&keys::encode(key)?)
    }

    /// Resizes the PTY (the program gets SIGWINCH) and the screen.
    ///
    /// # Errors
    /// [`Error::Pty`] when the PTY rejects the size.
    pub fn resize(&self, size: Size) -> Result<(), Error> {
        let mut screen = lock(&self.screen);
        self.pty.resize(pty_size(size))?;
        screen.resize(size);
        Ok(())
    }

    /// The program's exit status, if it has exited.
    ///
    /// # Errors
    /// [`Error::Io`] when the status cannot be read.
    pub fn try_wait(&self) -> Result<Option<ExitStatus>, Error> {
        Ok(lock(&self.child).try_wait()?)
    }

    /// The live processes of the terminal's tree and their summed RSS and CPU time.
    ///
    /// # Errors
    /// [`Error::Io`] when the process table cannot be read.
    pub fn usage(&self) -> Result<Usage, Error> {
        let procs = proctree::system().list()?;
        Ok(proctree::usage(&proctree::tree(
            &procs,
            self.pid,
            &std::collections::BTreeSet::new(),
        )))
    }

    /// Ends the whole process tree: SIGTERM, then SIGKILL after `grace`; returns the program's exit status.
    ///
    /// # Errors
    /// [`Error::Io`] when the process table or the exit status cannot be read.
    pub fn kill(&self, grace: Duration) -> Result<ExitStatus, Error> {
        let mut child = lock(&self.child);
        proctree::kill_tree(&proctree::system(), self.pid, grace, || {
            let _ = child.try_wait();
        })?;
        Ok(child.wait()?)
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        let child = self.child.get_mut().unwrap_or_else(PoisonError::into_inner);
        if matches!(child.try_wait(), Ok(None))
            && let Some(group) = Pid::from_raw(self.pid)
        {
            let _ = kill_process_group(group, Signal::KILL);
            let _ = child.wait();
        }
    }
}

fn pty_size(size: Size) -> pty_process::Size {
    pty_process::Size::new(size.rows.max(1), size.cols.max(1))
}

/// Feeds PTY output into the screen, the subscribers and the log until the PTY closes (EOF, or EIO once
/// every program left). A log that fails is dropped; the output goes on.
fn read_loop(
    pty: &pty_process::blocking::Pty,
    screen: &Mutex<Screen>,
    outputs: &Outputs,
    watch: &Watch,
    mut log: Option<Log>,
) {
    let mut buf = [0u8; 8192];
    loop {
        match (&*pty).read(&mut buf) {
            Ok(0) => return,
            Ok(n) => {
                let (replies, report) = {
                    let mut screen = lock(screen);
                    screen.feed(&buf[..n]);
                    (screen.take_replies(), screen.take_report())
                };
                watch.bump(|s| {
                    s.last_output = Instant::now();
                    s.report = report.or_else(|| s.report.take());
                });
                if let Some(subs) = lock(outputs).as_mut() {
                    subs.retain(|tx| tx.send(buf[..n].to_vec()).is_ok());
                }
                if log.as_mut().is_some_and(|l| l.append(&buf[..n]).is_err()) {
                    log = None;
                }
                if !replies.is_empty() && (&*pty).write_all(&replies).is_err() {
                    return;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return,
        }
    }
}

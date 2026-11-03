//! A terminal: one program on a PTY, its screen, and its process tree.

use std::ffi::OsStr;
use std::io::{Read as _, Write as _};
use std::path::Path;
use std::process::{Child, ExitStatus};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use rustix::process::{Pid, Signal, kill_process_group};

use crate::proctree::{self, ProcessTable as _, Usage};
use crate::screen::{Screen, Size};
use crate::{Error, keys};

/// Lines of scrollback each terminal keeps.
pub const SCROLLBACK: usize = 10_000;

/// A program running on a PTY, in its own session and process group.
///
/// Its output feeds a [`Screen`] from a reader thread. Dropping the terminal SIGKILLs its process group.
pub struct Terminal {
    pty: Arc<pty_process::blocking::Pty>,
    child: Mutex<Child>,
    screen: Arc<Mutex<Screen>>,
    outputs: Arc<Outputs>,
    pid: i32,
}

/// The senders of [`Terminal::subscribe`]rs; `None` once the PTY has closed.
type Outputs = Mutex<Option<Vec<Sender<Vec<u8>>>>>;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Terminal {
    /// Starts `cmd` (program and arguments) in `cwd` on a new PTY of `size`.
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
        let (program, args) = cmd.split_first().ok_or(Error::EmptyCommand)?;
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
        let screen = Arc::new(Mutex::new(Screen::new(size, SCROLLBACK)));
        let outputs = Arc::new(Mutex::new(Some(Vec::new())));
        let (reader, sink, subs) = (Arc::clone(&pty), Arc::clone(&screen), Arc::clone(&outputs));
        std::thread::spawn(move || {
            read_loop(&reader, &sink, &subs);
            *lock(&subs) = None;
        });
        Ok(Self {
            pty,
            child: Mutex::new(child),
            screen,
            outputs,
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

/// Feeds PTY output into the screen and the subscribers until the PTY closes (EOF, or EIO once every
/// program left).
fn read_loop(pty: &pty_process::blocking::Pty, screen: &Mutex<Screen>, outputs: &Outputs) {
    let mut buf = [0u8; 8192];
    loop {
        match (&*pty).read(&mut buf) {
            Ok(0) => return,
            Ok(n) => {
                let replies = {
                    let mut screen = lock(screen);
                    screen.feed(&buf[..n]);
                    screen.take_replies()
                };
                if let Some(subs) = lock(outputs).as_mut() {
                    subs.retain(|tx| tx.send(buf[..n].to_vec()).is_ok());
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

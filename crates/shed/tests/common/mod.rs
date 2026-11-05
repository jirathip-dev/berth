//! What the end-to-end tests share: temp dirs, polling and a real server.

use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use shed_server::Paths;

/// A fresh, canonical temp dir (macOS `/var` is a symlink, and the shell's `pwd` prints the real path).
pub fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("shed-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir.canonicalize().expect("canonical temp dir")
}

/// Polls `check` for up to ten seconds.
pub fn eventually(what: &str, mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !check() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A `shed serve --dir` process, killed (SIGKILL) on drop.
pub struct Server(Child);

impl Server {
    /// A server that samples resources once an hour, so no sample lands in a golden.
    pub fn start(dir: &Path) -> Self {
        Self::sampling(dir, "3600")
    }

    /// A server that samples resources every `secs` seconds.
    pub fn sampling(dir: &Path, secs: &str) -> Self {
        Self::with_env(dir, secs, &[])
    }

    /// A server that samples every `secs` seconds, with `env` in its (and its terminals') environment.
    pub fn with_env(dir: &Path, secs: &str, env: &[(&str, &str)]) -> Self {
        let child = Command::new(env!("CARGO_BIN_EXE_shed"))
            .args(["serve", "--sample", secs, "--dir"])
            .arg(dir)
            .env("SHELL", "/bin/sh")
            .env("PS1", "$ ")
            .env_remove("ENV")
            .envs(env.iter().copied())
            .stderr(Stdio::null())
            .spawn()
            .expect("shed serve starts");
        let server = Self(child);
        let socket = Paths::in_dir(dir).socket;
        eventually("the socket", || UnixStream::connect(&socket).is_ok());
        server
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

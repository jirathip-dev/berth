//! `berth attach`: this terminal in raw mode, wired to a terminal on the server.

use std::io::{self, Read as _, Write as _};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::time::Duration;

use berth_server::proto::{Method, ResizeParams, SendParams, Target, TerminalInfo};
use berth_server::{Client, notification};
use rustix::termios::{OptionalActions, Termios, tcgetattr, tcgetwinsize, tcsetattr};

/// The detach key: `Ctrl+\`.
const DETACH: u8 = 0x1c;
/// How often the window size is checked for changes.
const RESIZE_POLL: Duration = Duration::from_millis(100);

/// Stdin in raw mode while alive; restores the saved mode on drop. A no-op when stdin is not a tty.
struct RawMode(Option<Termios>);

impl RawMode {
    fn enter() -> io::Result<Self> {
        let Ok(saved) = tcgetattr(io::stdin()) else {
            return Ok(Self(None));
        };
        let mut raw = saved.clone();
        raw.make_raw();
        tcsetattr(io::stdin(), OptionalActions::Now, &raw)?;
        Ok(Self(Some(saved)))
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        if let Some(saved) = &self.0 {
            let _ = tcsetattr(io::stdin(), OptionalActions::Now, saved);
        }
    }
}

/// Attaches to `name` until `Ctrl+\` or the program's PTY closes.
pub(crate) fn attach(mut client: Client, name: &str) -> anyhow::Result<()> {
    let target = Target {
        name: name.to_owned(),
    };
    let _: TerminalInfo = client.call(Method::Attach, &target)?;
    let (mut reader, writer) = client.split();
    let raw = RawMode::enter()?;
    let (resizes, input) = (writer.try_clone()?, writer);
    let owned = name.to_owned();
    std::thread::spawn(move || forward_resizes(&resizes, &owned));
    let owned = name.to_owned();
    std::thread::spawn(move || forward_input(&input, &owned));
    let mut out = io::stdout().lock();
    let mut buf = [0u8; 8192];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n])?;
        out.flush()?;
    }
    drop(raw);
    out.write_all(b"\r\n")?;
    Ok(())
}

/// Sends stdin as `send` notifications; at `Ctrl+\` (or end of input) closes the connection.
fn forward_input(mut conn: &UnixStream, name: &str) {
    let mut stdin = io::stdin().lock();
    let mut buf = [0u8; 1024];
    while let Ok(n) = stdin.read(&mut buf) {
        let detach = buf[..n].iter().position(|&b| b == DETACH);
        let typed = &buf[..detach.unwrap_or(n)];
        if !typed.is_empty() {
            let params = SendParams {
                name: name.to_owned(),
                text: String::from_utf8_lossy(typed).into_owned(),
            };
            let sent = notification(Method::Send, &params).map(|line| conn.write_all(&line));
            if !matches!(sent, Ok(Ok(()))) {
                break;
            }
        }
        if detach.is_some() || n == 0 {
            break;
        }
    }
    let _ = conn.shutdown(Shutdown::Both);
}

/// Sends a `resize` notification at once and whenever the window size changes.
fn forward_resizes(mut conn: &UnixStream, name: &str) {
    let mut last = None;
    loop {
        if let Ok(size) = tcgetwinsize(io::stdin())
            && size.ws_row > 0
            && size.ws_col > 0
            && last != Some((size.ws_row, size.ws_col))
        {
            last = Some((size.ws_row, size.ws_col));
            let params = ResizeParams {
                name: name.to_owned(),
                rows: size.ws_row,
                cols: size.ws_col,
            };
            let sent = notification(Method::Resize, &params).map(|line| conn.write_all(&line));
            if !matches!(sent, Ok(Ok(()))) {
                return;
            }
        }
        std::thread::sleep(RESIZE_POLL);
    }
}

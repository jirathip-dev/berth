//! The CLI end to end, against a real server.
//!
//! The human and JSON output is checked against `tests/golden/cli.txt` (the temp dir and pids
//! normalised). Regenerate with `BERTH_BLESS=1 cargo test -p berth`.

mod common;

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use berth_core::{Size, Terminal};
use common::{Server, eventually, temp_dir};

const BERTH: &str = env!("CARGO_BIN_EXE_berth");

fn berth(args: &[&str]) -> String {
    let out = Command::new(BERTH).args(args).output().expect("berth runs");
    assert!(out.status.success(), "berth {args:?} failed: {out:?}");
    String::from_utf8(out.stdout).expect("utf-8 stdout")
}

/// `berth --dir <dir> args…`.
fn berth_in(dir: &Path, args: &[&str]) -> Output {
    Command::new(BERTH)
        .arg("--dir")
        .arg(dir)
        .args(args)
        .output()
        .expect("berth runs")
}

fn stdout(out: &Output) -> String {
    assert!(out.status.success(), "berth failed: {out:?}");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Replaces every number after `"pid":` or `"pid": ` with 0.
fn zero_pids(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(at) = rest.find("\"pid\":") {
        let (head, tail) = rest.split_at(at + 6);
        out.push_str(head);
        let spaces = tail.len() - tail.trim_start_matches(' ').len();
        let (gap, tail) = tail.split_at(spaces);
        out.push_str(gap);
        let digits = tail.len() - tail.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        out.push_str(if digits > 0 { "0" } else { "" });
        rest = &tail[digits..];
    }
    out + rest
}

/// A CLI session whose commands and output are collected for the golden transcript.
struct Transcript {
    dir: PathBuf,
    text: String,
    /// The file under `tests/golden/`.
    golden: &'static str,
}

impl Transcript {
    fn run(&mut self, args: &[&str]) -> Output {
        let out = berth_in(&self.dir, args);
        let dir = self.dir.to_str().expect("utf-8 dir");
        let mut text = format!(
            "$ berth {}\n{}",
            args.join(" "),
            String::from_utf8_lossy(&out.stdout)
        );
        if !out.status.success() {
            let code = out.status.code().unwrap_or(-1);
            let err = String::from_utf8_lossy(&out.stderr);
            let err = if err.is_empty() { "\n".into() } else { err };
            write!(text, "[exit {code}] {err}").expect("write to a String");
        }
        let text = text.replace(BERTH, "berth").replace(dir, "$DIR");
        self.text.push_str(&zero_pids(&text));
        out
    }

    fn check(&self) {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/golden")
            .join(self.golden);
        if std::env::var_os("BERTH_BLESS").is_some() {
            std::fs::write(&path, &self.text).expect("write golden");
        }
        let have = std::fs::read_to_string(&path).unwrap_or_default();
        assert_eq!(
            have,
            self.text,
            "{} differs; BERTH_BLESS=1 to update",
            path.display()
        );
    }
}

#[test]
fn hello_prints_the_version() {
    assert_eq!(
        berth(&["hello"]),
        format!("berth {}\n", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn version_flag_prints_the_version() {
    assert_eq!(
        berth(&["--version"]),
        format!("berth {}\n", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn help_lists_every_command() {
    let help = berth(&["--help"]);
    for cmd in [
        "hello", "serve", "run", "ls", "get", "read", "send", "keys", "kill", "rm", "attach",
        "state", "wait",
    ] {
        assert!(
            help.contains(&format!("\n  {cmd} ")),
            "{cmd} missing from {help}"
        );
    }
}

#[test]
fn run_read_send_exit_ls_and_labels_match_the_golden() {
    let dir = temp_dir("cli");
    let d = dir.to_str().expect("utf-8 dir");
    let mut t = Transcript {
        dir: dir.clone(),
        text: String::new(),
        golden: "cli.txt",
    };
    t.run(&["ls"]);
    t.run(&["run", "--cwd", d, "--", "true"]);
    let _server = Server::start(&dir);

    let run = [
        "run",
        "--name",
        "shell",
        "--label",
        "role=impl",
        "--label",
        "issue=7",
    ];
    t.run(&[&run[..], &["--cwd", d, "--", "sh"]].concat());
    eventually("the prompt", || {
        stdout(&berth_in(&dir, &["read", "shell"])) == "$\n"
    });
    t.run(&["read", "shell"]);
    t.run(&["--json", "read", "shell", "--lines", "1"]);
    t.run(&["send", "shell", "exit"]);
    t.run(&["keys", "shell", "enter"]);
    eventually("the shell to exit", || {
        stdout(&berth_in(&dir, &["get", "shell"])).contains("\"exited\"")
    });
    t.run(&["ls"]);

    let rev = ["run", "--name", "rev", "--label", "role=rev", "--cwd", d];
    t.run(&[&["--json"], &rev[..], &["--", "sleep", "100"]].concat());
    t.run(&["kill", "rev"]);
    t.run(&["ls", "--label", "role=impl"]);
    t.run(&["ls", "--label", "role=impl", "--label", "issue=8"]);
    t.run(&["--json", "ls", "--label", "role=rev"]);
    t.run(&["get", "shell"]);
    t.run(&["--json", "get", "shell"]);
    t.run(&["rm", "shell"]);
    t.run(&["--json", "rm", "rev"]);
    t.run(&["ls"]);
    t.run(&["keys", "rev", "enter"]);
    t.run(&["run", "--label", "nolabel", "--", "true"]);
    t.check();
}

#[test]
fn a_fake_agent_reports_by_escape_and_berth_state_and_wait_returns_on_each() {
    let dir = temp_dir("state");
    let d = dir.to_str().expect("utf-8 dir");
    let _server = Server::start(&dir);
    let mut t = Transcript {
        dir: dir.clone(),
        text: String::new(),
        golden: "state.txt",
    };
    // The agent reports `working` by escape, then `done` with `berth state` and no name ($BERTH).
    let agent = format!(
        "printf 'start\\033]7501;state=working;note=step 1\\007end\\n'; read x; \
         '{BERTH}' --dir '{d}' state done --note 'all green'; read y"
    );
    t.run(&[
        "run", "--name", "agent", "--cwd", d, "--", "sh", "-c", &agent,
    ]);
    t.run(&[
        "wait",
        "agent",
        "--until",
        "done",
        "working",
        "--timeout",
        "10",
    ]);
    t.run(&["read", "agent"]);
    t.run(&["read", "agent", "--history"]);
    t.run(&["send", "agent", "\n"]);
    t.run(&[
        "--json",
        "wait",
        "agent",
        "--until",
        "done",
        "--timeout",
        "10",
    ]);
    t.run(&["wait", "agent", "--until", "blocked", "--timeout", "0.2"]);
    t.run(&["state", "agent", "blocked", "--note", "needs review"]);
    t.run(&["--json", "get", "agent"]);
    t.run(&["state", "agent", "sleeping"]);
    t.run(&["send", "agent", "\n"]);
    t.run(&["wait", "agent", "--until", "working"]);
    t.check();
}

#[test]
fn wait_matches_output_by_pattern_and_falls_back_to_idle() {
    let dir = temp_dir("wait-output");
    let d = dir.to_str().expect("utf-8 dir");
    let _server = Server::start(&dir);
    let script = "sleep 0.3; echo 'ok 12 tests'; exec sleep 100";
    stdout(&berth_in(
        &dir,
        &[
            "run", "--name", "test", "--cwd", d, "--", "sh", "-c", script,
        ],
    ));
    let started = Instant::now();
    let out = berth_in(
        &dir,
        &[
            "wait",
            "test",
            "--output",
            "^ok \\d+ tests$",
            "--timeout",
            "10",
        ],
    );
    assert_eq!(stdout(&out), "test output\n");
    assert!(started.elapsed() < Duration::from_secs(5));

    let idle = [
        "run",
        "--name",
        "quiet",
        "--idle-after",
        "1",
        "--cwd",
        d,
        "--",
        "sleep",
        "100",
    ];
    stdout(&berth_in(&dir, &idle));
    let out = berth_in(
        &dir,
        &["wait", "quiet", "--until", "idle", "--timeout", "10"],
    );
    assert_eq!(stdout(&out), "quiet idle\n");
    let out = berth_in(
        &dir,
        &["wait", "test", "--until", "idle", "--timeout", "1.2"],
    );
    assert_eq!(
        out.status.code(),
        Some(124),
        "no fallback unless asked: {out:?}"
    );
}

#[test]
fn attach_types_resizes_and_detaches_in_a_pty() {
    let dir = temp_dir("attach");
    let d = dir.to_str().expect("utf-8 dir");
    let _server = Server::start(&dir);
    stdout(&berth_in(
        &dir,
        &["run", "--name", "shell", "--cwd", d, "--", "sh"],
    ));
    eventually("the prompt", || {
        stdout(&berth_in(&dir, &["read", "shell"])) == "$\n"
    });

    let size = Size { rows: 24, cols: 80 };
    let cmd = [BERTH, "--dir", d, "attach", "shell"];
    let outer = Terminal::spawn(&cmd, &dir, [("TERM", "xterm")], size).expect("attach in a pty");
    eventually("the redraw", || outer.screen().starts_with('$'));
    outer.write(b"echo hi-$((1+2))\r").expect("type");
    eventually("typed input to run", || outer.screen().contains("hi-3"));

    outer
        .resize(Size {
            rows: 30,
            cols: 100,
        })
        .expect("resize");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !outer.screen().contains("30 100") {
        assert!(
            Instant::now() < deadline,
            "resize not forwarded: {}",
            outer.screen()
        );
        outer.write(b"stty size\r").expect("type");
        std::thread::sleep(Duration::from_millis(300));
    }

    outer.write(&[0x1c]).expect("Ctrl+\\");
    eventually("detach", || matches!(outer.try_wait(), Ok(Some(_))));
    let status = outer.try_wait().expect("status").expect("exited");
    assert!(status.success(), "attach exits 0 on detach: {status:?}");
    let ls = stdout(&berth_in(&dir, &["ls"]));
    assert!(
        ls.contains("shell  running"),
        "still running after detach: {ls}"
    );
    let read = stdout(&berth_in(&dir, &["read", "shell", "--history"]));
    assert!(read.contains("hi-3"), "{read}");
}

#[test]
fn attach_ends_when_the_program_exits() {
    let dir = temp_dir("attach-exit");
    let d = dir.to_str().expect("utf-8 dir");
    let _server = Server::start(&dir);
    stdout(&berth_in(
        &dir,
        &["run", "--name", "cat", "--cwd", d, "--", "cat"],
    ));
    let size = Size { rows: 24, cols: 80 };
    let cmd = [BERTH, "--dir", d, "attach", "cat"];
    let outer = Terminal::spawn(&cmd, &dir, [("TERM", "xterm")], size).expect("attach in a pty");
    outer.write(b"ping\r").expect("type");
    eventually("the echo", || outer.screen().contains("ping"));
    stdout(&berth_in(&dir, &["kill", "cat"]));
    eventually("attach to end", || matches!(outer.try_wait(), Ok(Some(_))));
}

#[test]
fn run_autostart_starts_a_detached_server() {
    let dir = temp_dir("autostart");
    let d = dir.to_str().expect("utf-8 dir");
    let out = berth_in(
        &dir,
        &[
            "run",
            "--autostart",
            "--cwd",
            d,
            "--",
            "sh",
            "-c",
            "echo $PPID; exec sleep 100",
        ],
    );
    assert_eq!(stdout(&out), "t1\n");
    assert!(String::from_utf8_lossy(&out.stderr).contains("server started on"));
    let mut server = String::new();
    eventually("the server's pid", || {
        server = stdout(&berth_in(&dir, &["read", "t1"])).trim().to_owned();
        !server.is_empty()
    });
    let again = berth_in(&dir, &["serve", "--detach"]);
    assert!(!again.status.success(), "a second server is refused");
    let killed = Command::new("kill").arg(&server).status().expect("kill");
    assert!(
        killed.success(),
        "the server ({server}) is the program's parent"
    );
}

/// Soft: prints how long non-waiting calls take; CI shows it (`just bench`).
#[test]
fn timing_report() {
    const CALLS: usize = 30;
    let dir = temp_dir("timing");
    let d = dir.to_str().expect("utf-8 dir");
    let _server = Server::start(&dir);
    stdout(&berth_in(
        &dir,
        &["run", "--name", "t", "--cwd", d, "--", "sleep", "100"],
    ));
    for args in [&["ls"][..], &["get", "t"], &["read", "t"]] {
        let mut ms: Vec<f64> = (0..CALLS)
            .map(|_| {
                let start = Instant::now();
                stdout(&berth_in(&dir, args));
                start.elapsed().as_secs_f64() * 1000.0
            })
            .collect();
        ms.sort_by(f64::total_cmp);
        let median = ms[CALLS / 2];
        let note = if median < 20.0 { "ok" } else { "SLOW" };
        eprintln!(
            "timing: berth {:<6} median {median:5.1} ms, p90 {:5.1} ms, max {:5.1} ms over {CALLS} calls (target < 20 ms, soft): {note}",
            args.join(" "),
            ms[CALLS * 9 / 10],
            ms[CALLS - 1],
        );
    }
}

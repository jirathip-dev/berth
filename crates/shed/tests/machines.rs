//! Machines over SSH end to end: `shed connect` over a socketpair (simulated SSH), and `--on` through a
//! fake `ssh` that runs `shed connect` against a second server in a temp dir.
//!
//! The CLI output is checked against `tests/golden/machines.txt` (the temp dir and version
//! normalised). Regenerate with `SHED_BLESS=1 cargo test -p shed`.

mod common;

use std::fmt::Write as _;
use std::io::{BufRead as _, BufReader, Write as _};
use std::os::fd::OwnedFd;
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use common::{Server, temp_dir};
use serde_json::{Value, json};
use shed_server::proto::{Response, RpcError};

const SHED: &str = env!("CARGO_BIN_EXE_shed");
const VERSION: &str = env!("CARGO_PKG_VERSION");

#[test]
fn connect_carries_the_protocol_over_a_socketpair() {
    let dir = temp_dir("connect");
    let _server = Server::start(&dir);
    let (ours, theirs) = UnixStream::pair().expect("socketpair");
    let mut child = Command::new(SHED)
        .arg("--dir")
        .arg(&dir)
        .arg("connect")
        .stdin(Stdio::from(OwnedFd::from(
            theirs.try_clone().expect("clone"),
        )))
        .stdout(Stdio::from(OwnedFd::from(theirs)))
        .spawn()
        .expect("shed connect starts");
    let mut reader = BufReader::new(ours.try_clone().expect("clone"));
    let golden = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/hello.jsonl");
    let golden = std::fs::read_to_string(golden).expect("hello golden");
    let exchange: Vec<String> = golden
        .lines()
        .map(|l| l.replace("$VERSION", VERSION))
        .collect();
    let mut call = |request: &str| {
        (&ours)
            .write_all(format!("{request}\n").as_bytes())
            .expect("write");
        let mut line = String::new();
        reader.read_line(&mut line).expect("read");
        line.trim_end().to_owned()
    };
    assert_eq!(call(&exchange[0]), exchange[1]);
    assert_eq!(
        call(r#"{"jsonrpc":"2.0","id":2,"method":"ls","params":{}}"#),
        r#"{"jsonrpc":"2.0","id":2,"result":[]}"#
    );
    ours.shutdown(std::net::Shutdown::Write).expect("shutdown");
    assert!(child.wait().expect("shed connect ends").success());
}

/// Writes an executable shell script.
fn script(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
    std::fs::write(path, format!("#!/bin/sh\n{body}")).expect("write script");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
}

/// Two fake hosts under `root`: `user@box.example`, whose `shed` uses the server in `root/remote`,
/// and `old.example`, whose `shed` answers `hello` as another version would. Returns the `bin`
/// directory holding the fake `ssh`, which logs its arguments to `bin/ssh.log` and its pids (the
/// `shed connect` it becomes) to `bin/ssh.pids`.
fn fake_hosts(root: &Path) -> PathBuf {
    let bin = root.join("bin");
    script(
        &bin.join("ssh"),
        "# A fake ssh: logs its arguments, skips the options, runs the command on the target's PATH.\n\
         echo \"$*\" >> \"$(dirname \"$0\")/ssh.log\"\n\
         echo $$ >> \"$(dirname \"$0\")/ssh.pids\"\n\
         while [ \"$1\" = -o ]; do shift 2; done\n\
         [ \"$1\" = -- ] && shift\n\
         target=$1\nshift\n\
         PATH=\"$(dirname \"$0\")/../$target:$PATH\" exec \"$@\"\n",
    );
    let remote = root.join("remote");
    script(
        &root.join("user@box.example/shed"),
        &format!("exec '{SHED}' --dir '{}' \"$@\"\n", remote.display()),
    );
    let refusal = Response::err(1.into(), RpcError::version(VERSION, "9.9.9"));
    let refusal = serde_json::to_string(&refusal).expect("json");
    script(
        &root.join("old.example/shed"),
        &format!("read -r line\nprintf '%s\\n' '{refusal}'\n"),
    );
    bin
}

/// `shed --dir <root>/local args…` with the fake `ssh` first on `PATH`.
fn shed_on(root: &Path, args: &[&str]) -> Command {
    let path = std::env::var("PATH").unwrap_or_default();
    let mut cmd = Command::new(SHED);
    cmd.arg("--dir")
        .arg(root.join("local"))
        .args(args)
        .env("PATH", format!("{}:{path}", root.join("bin").display()));
    cmd
}

/// Runs `shed args…` and appends the command and its output to `text`.
fn record(root: &Path, text: &mut String, args: &[&str]) -> Output {
    let out = shed_on(root, args).output().expect("shed runs");
    let _ = write!(
        text,
        "$ shed {}\n{}",
        args.join(" "),
        String::from_utf8_lossy(&out.stdout)
    );
    if !out.status.success() {
        let code = out.status.code().unwrap_or(-1);
        let _ = write!(
            text,
            "[exit {code}] {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    out
}

#[test]
fn on_a_machine_run_read_kill_and_ls_reach_the_remote_server_and_versions_must_match() {
    let root = temp_dir("machines");
    let bin = fake_hosts(&root);
    let remote = root.join("remote");
    let _server = Server::start(&remote);
    let remote_arg = remote.to_str().expect("utf-8 dir");
    let mut text = String::new();
    for args in [
        &["machine", "ls"][..],
        &["machine", "add", "box", "user@box.example"],
        &["machine", "add", "old", "old.example"],
        &["machine", "add", "box", "other.example"],
        &["machine", "add", "bad", "--", "-oProxyCommand=x"],
        &["--json", "machine", "ls"],
        &["machine", "ls"],
        &["--on", "nope", "ls"],
        &["--on", "box", "serve"],
        &["--on", "box", "run", "--", "true"],
        &[
            "--on",
            "box",
            "run",
            "--name",
            "t1",
            "--cwd",
            remote_arg,
            "--",
            "sh",
            "-c",
            "echo hello from box; exec sleep 100",
        ],
        &["--on", "box", "wait", "t1", "--output", "hello from box"],
        &["--on", "box", "read", "t1"],
        &["--on", "box", "kill", "t1"],
        &["--on", "box", "ls"],
        &["--on", "old", "ls"],
        &["machine", "rm", "old"],
        &["machine", "rm", "old"],
        &["machine", "ls"],
    ] {
        record(&root, &mut text, args);
    }
    let direct = Command::new(SHED)
        .arg("--dir")
        .arg(&remote)
        .arg("ls")
        .output()
        .expect("shed ls");
    assert!(String::from_utf8_lossy(&direct.stdout).contains("t1"));
    let log = std::fs::read_to_string(bin.join("ssh.log")).expect("ssh ran");
    for option in [
        "-o BatchMode=yes",
        "-o ConnectTimeout=10",
        "-o ControlMaster=auto",
        "-- user@box.example shed connect",
    ] {
        assert!(log.contains(option), "{option} missing from {log}");
    }
    let text = text
        .replace(root.to_str().expect("utf-8 dir"), "$DIR")
        .replace(VERSION, "$VERSION");
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/machines.txt");
    if std::env::var_os("SHED_BLESS").is_some() {
        std::fs::write(&path, &text).expect("write golden");
    }
    let have = std::fs::read_to_string(&path).unwrap_or_default();
    assert_eq!(have, text, "machines.txt differs; SHED_BLESS=1 to update");
}

#[test]
fn events_on_a_machine_stream_prefixed_and_reconnect_after_the_link_drops() {
    let root = temp_dir("machine-events");
    let bin = fake_hosts(&root);
    let _server = Server::start(&root.join("remote"));
    let added = shed_on(&root, &["machine", "add", "box", "user@box.example"]).output();
    assert!(added.expect("machine add").status.success());
    let mut events = shed_on(&root, &["--on", "box", "events", "--since", "0"])
        .stdout(Stdio::piped())
        .spawn()
        .expect("events");
    let mut lines = BufReader::new(events.stdout.take().expect("piped")).lines();
    let mut next = || -> Value { serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap() };
    let cwd = root.to_str().expect("utf-8 dir");
    let run = |name: &str, script: &str| {
        let args = [
            "--on", "box", "run", "--name", name, "--cwd", cwd, "--", "sh", "-c", script,
        ];
        let ran = shed_on(&root, &args).output().expect("run");
        assert!(ran.status.success(), "{ran:?}");
    };
    run(
        "e1",
        "printf '\\033]7501;state=working\\007'; exec sleep 100",
    );
    let (started, working) = (next(), next());
    assert_eq!(
        (&started["event"], &started["name"]),
        (&json!("started"), &json!("box/e1"))
    );
    assert_eq!(
        (&working["state"], &working["name"]),
        (&json!("working"), &json!("box/e1"))
    );

    // Drop the link: kill every `shed connect` the fake ssh became (the events one is still up).
    let pids = std::fs::read_to_string(bin.join("ssh.pids")).expect("pids");
    for pid in pids.lines() {
        let _ = Command::new("kill").arg(pid).stderr(Stdio::null()).status();
    }
    let dropped = Instant::now();
    let reconnecting = next();
    assert_eq!(reconnecting["name"], "box/e1");
    assert_eq!(reconnecting["state"], "working");
    assert_eq!(reconnecting["note"], "machine: reconnecting");
    assert_eq!(reconnecting["seq"], working["seq"]);
    let connected = next();
    assert_eq!(connected["note"], "machine: connected");
    let waited = dropped.elapsed();
    assert!(
        waited >= Duration::from_secs(4) && waited < Duration::from_secs(10),
        "{waited:?}"
    );

    // The stream resumes after the last seq: no replay, the next terminal's start comes next.
    run("e2", "exec sleep 100");
    let started = next();
    assert_eq!(
        (&started["event"], &started["name"]),
        (&json!("started"), &json!("box/e2"))
    );
    let _ = events.kill();
    let _ = events.wait();
}

#[test]
fn on_a_machine_ssh_cannot_reach_fails_fast_with_the_hint() {
    let root = temp_dir("machine-down");
    script(
        &root.join("bin/ssh"),
        "echo 'ssh: connect to host down.example: Connection refused' >&2\nexit 255\n",
    );
    assert!(
        shed_on(&root, &["machine", "add", "down", "down.example"])
            .status()
            .expect("shed runs")
            .success()
    );
    let mut child = shed_on(&root, &["--on", "down", "ls"])
        .stderr(Stdio::piped())
        .spawn()
        .expect("shed runs");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while child.try_wait().expect("try_wait").is_none() {
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            panic!("shed --on hung after ssh exited");
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let out = child.wait_with_output().expect("output");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no answer from shed on down")
            && stderr.contains("`ssh down.example shed hello`"),
        "{stderr}"
    );
}

#[test]
fn machine_stats_here_and_on_a_machine_and_an_offline_one_answers_within_5_s() {
    let root = temp_dir("machine-stats");
    let bin = fake_hosts(&root);
    // With a `down` file beside it, the fake ssh hangs as a dead link would.
    let ssh = std::fs::read_to_string(bin.join("ssh")).expect("ssh");
    let hang = "[ -e \"$(dirname \"$0\")/down\" ] && exec sleep 30\n";
    script(&bin.join("ssh"), &ssh.replacen("#!/bin/sh\n", hang, 1));
    let _local = Server::start(&root.join("local"));
    let _remote = Server::start(&root.join("remote"));
    let shed = |args: &[&str]| {
        let out = shed_on(&root, args)
            .stderr(Stdio::null())
            .output()
            .expect("shed runs");
        let json = serde_json::from_slice(&out.stdout).unwrap_or(Value::Null);
        (out.status.code(), json)
    };
    assert_eq!(
        shed(&["run", "--name", "a", "--", "sleep", "30"]).0,
        Some(0)
    );
    assert_eq!(shed(&["state", "a", "working"]).0, Some(0));
    assert_eq!(shed(&["run", "--name", "b", "--", "true"]).0, Some(0));
    common::eventually("b exits", || {
        shed(&["--json", "get", "b"]).1["state"] == "exited"
    });
    let check = |s: &Value| {
        assert!(s["at"].as_u64().expect("at") > 1_700_000_000, "{s}");
        assert!(s["load_per_cpu"].as_f64().expect("load") >= 0.0, "{s}");
        assert!(
            s["mem_pressure_pct"].as_u64().expect("memory") <= 100,
            "{s}"
        );
        assert!(s["swap_used_mb"].is_u64(), "{s}");
        assert!(s["disk_free_mb"].as_u64().expect("disk") > 0, "{s}");
        assert!(s["cpus"].as_u64().expect("cpus") >= 1, "{s}");
        assert!(s["uptime_s"].as_u64().expect("uptime") > 0, "{s}");
    };
    let (code, here) = shed(&["--json", "machine", "stats", "--since", "0"]);
    assert_eq!(code, Some(0));
    check(&here);
    assert_eq!(here["terminals"], json!({"working": 1, "exited": 1}));
    // The server took a sample as it started, with the terminal counts of then (none).
    let history = here["history"].as_array().expect("history");
    assert_eq!(history.len(), 1, "{here}");
    check(&history[0]);
    assert_eq!(history[0]["terminals"], json!({}));
    let (code, without) = shed(&["--json", "machine", "stats"]);
    assert_eq!(code, Some(0));
    assert!(without.get("history").is_none(), "{without}");

    assert_eq!(
        shed(&["machine", "add", "box", "user@box.example"]).0,
        Some(0)
    );
    let (code, there) = shed(&["--json", "--on", "box", "machine", "stats"]);
    assert_eq!(code, Some(0));
    check(&there);
    assert_eq!(there["terminals"], json!({}));
    let seen = std::fs::read_to_string(root.join("local/seen/box")).expect("seen");

    std::fs::write(bin.join("down"), "").expect("down");
    let started = Instant::now();
    let (code, offline) = shed(&["--json", "--on", "box", "machine", "stats"]);
    let took = started.elapsed();
    assert_eq!(code, Some(1));
    let last_seen: u64 = seen.parse().expect("unix seconds");
    assert_eq!(
        offline,
        json!({"machine": "box", "online": false, "last_seen": last_seen})
    );
    assert!(
        took < Duration::from_millis(5_500),
        "answered after {took:?}"
    );
    let text = shed_on(&root, &["--on", "box", "machine", "stats"])
        .stderr(Stdio::null())
        .output()
        .expect("shed runs");
    assert_eq!(
        String::from_utf8_lossy(&text.stdout),
        format!("box offline, last seen {last_seen}\n")
    );
}

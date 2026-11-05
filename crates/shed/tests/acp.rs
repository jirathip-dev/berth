//! ACP client mode end to end: `tests/fake_acp.py`, a scripted ACP agent, installed from a fixture
//! registry and driven through the CLI against a real server.
//!
//! The turn with an approval is checked against `tests/golden/acp.txt` (commands, output and the event
//! log). Regenerate with `SHED_BLESS=1 cargo test -p shed --test acp`.

mod common;

use std::fmt::Write as _;
use std::io::BufRead as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use common::{Server, temp_dir};
use serde_json::{Value, json};
use shed_server::proto::{EventsParams, Method};
use shed_server::{Client, Paths};

const SHED: &str = env!("CARGO_BIN_EXE_shed");

fn fake_agent() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fake_acp.py")
}

fn shed(dir: &Path, args: &[&str]) -> Output {
    Command::new(SHED)
        .arg("--dir")
        .arg(dir)
        .args(args)
        .output()
        .expect("shed runs")
}

fn ok(dir: &Path, args: &[&str]) -> String {
    let out = shed(dir, args);
    assert!(out.status.success(), "shed {args:?} failed: {out:?}");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn sha256(path: &Path) -> String {
    let out = Command::new("shasum")
        .args(["-a", "256"])
        .arg(path)
        .output()
        .or_else(|_| Command::new("sha256sum").arg(path).output())
        .expect("a sha256 tool");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.split_whitespace().next().expect("a sum").to_owned()
}

fn platform() -> String {
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        os => os,
    };
    format!("{os}-{}", std::env::consts::ARCH)
}

/// A registry file in `dir`: `fake` as this platform's binary (a tar.gz of the fake agent, built here),
/// `npx-agent` with variables and `uvx-agent`; `bad-sum` has a wrong checksum.
fn registry(dir: &Path) -> PathBuf {
    let pkg = dir.join("pkg");
    std::fs::create_dir_all(&pkg).expect("pkg dir");
    std::fs::copy(fake_agent(), pkg.join("fake-acp")).expect("copy the agent");
    let mode = std::fs::Permissions::from_mode(0o755);
    std::fs::set_permissions(pkg.join("fake-acp"), mode).expect("chmod");
    let archive = dir.join("fake-acp.tar.gz");
    let tar = Command::new("tar")
        .arg("-czf")
        .arg(&archive)
        .arg("-C")
        .arg(&pkg)
        .arg("fake-acp")
        .status()
        .expect("tar");
    assert!(tar.success());
    let url = format!("file://{}", archive.display());
    let binary =
        |sum: &str| json!({platform(): {"archive": url, "cmd": "./fake-acp", "sha256": sum}});
    let registry = json!({"version": "1.0.0", "agents": [
        {"id": "fake", "version": "0.1.0", "distribution": {"binary": binary(&sha256(&archive))}},
        {"id": "npx-agent", "version": "1.2.3", "distribution": {"npx": {
            "package": "npx-agent@1.2.3", "args": ["--acp"], "env": {"NO_UPDATE": "1"}}}},
        {"id": "uvx-agent", "version": "0.4.0", "distribution": {"uvx": {"package": "uvx-agent==0.4.0"}}},
        {"id": "bad-sum", "version": "0.1.0", "distribution": {"binary": binary("00")}},
    ]});
    let path = dir.join("registry.json");
    std::fs::write(&path, registry.to_string()).expect("write the registry");
    path
}

/// The event lines of terminals labelled `run=<run>` from seq 0 until one matches `last`.
fn events_until(dir: &Path, run: &str, last: impl Fn(&Value) -> bool) -> Vec<String> {
    let mut client = Client::connect(&Paths::in_dir(dir).socket).expect("connect");
    let params = EventsParams {
        labels: [("run".to_owned(), run.to_owned())].into(),
        since: Some(0),
    };
    let _: Value = client.call(Method::Events, &params).expect("events");
    let (reader, _writer) = client.split();
    reader
        .get_ref()
        .set_read_timeout(Some(std::time::Duration::from_secs(10)))
        .expect("timeout");
    let mut lines = Vec::new();
    for line in reader.lines() {
        let line = line.expect("an event line");
        let done = last(&serde_json::from_str(&line).expect("event JSON"));
        lines.push(line);
        if done {
            return lines;
        }
    }
    panic!("the events ended early: {lines:?}");
}

fn is_state(state: &str) -> impl Fn(&Value) -> bool {
    move |e| e["event"] == "state" && e["state"] == state
}

#[test]
fn registry_installs_pinned_agents_from_a_fixture() {
    let dir = temp_dir("acp-registry");
    let reg = registry(&dir);
    let reg = reg.to_str().expect("utf-8");
    let installed = ok(&dir, &["agents", "add", "fake", "--registry", reg]);
    assert_eq!(installed, "installed fake 0.1.0 (binary)\n");
    let agent = dir.join("agents/fake/0.1.0/fake-acp");
    assert!(agent.is_file(), "{} unpacked", agent.display());
    ok(&dir, &["agents", "add", "npx-agent", "--registry", reg]);
    ok(&dir, &["agents", "add", "uvx-agent", "--registry", reg]);
    let list: Value = serde_json::from_str(&ok(&dir, &["--json", "agents"])).expect("JSON");
    let cmds: Vec<(&str, &Value)> = list
        .as_array()
        .expect("a list")
        .iter()
        .map(|a| (a["id"].as_str().expect("id"), &a["cmd"]))
        .collect();
    assert_eq!(
        cmds,
        [
            ("fake", &json!([agent])),
            (
                "npx-agent",
                &json!([
                    "env",
                    "NO_UPDATE=1",
                    "npx",
                    "-y",
                    "npx-agent@1.2.3",
                    "--acp"
                ])
            ),
            ("uvx-agent", &json!(["uvx", "uvx-agent==0.4.0"])),
        ]
    );

    let again = shed(&dir, &["agents", "add", "fake", "--registry", reg]);
    assert!(!again.status.success());
    let err = String::from_utf8_lossy(&again.stderr);
    assert!(
        err.contains("fake is installed at 0.1.0; rm it first"),
        "{err}"
    );
    let bad = shed(&dir, &["agents", "add", "bad-sum", "--registry", reg]);
    assert!(String::from_utf8_lossy(&bad.stderr).contains("sha256 mismatch"));
    let missing = shed(&dir, &["agents", "add", "nope", "--registry", reg]);
    assert!(String::from_utf8_lossy(&missing.stderr).contains("no agent \"nope\""));

    assert_eq!(
        ok(&dir, &["agents", "rm", "fake"]),
        "removed fake 0.1.0 (binary)\n"
    );
    assert!(!dir.join("agents/fake").exists());
    assert!(ok(&dir, &["agents"]).starts_with("npx-agent 1.2.3 (npx)  env NO_UPDATE=1 npx"));
}

#[test]
fn a_turn_with_an_approval_matches_the_golden_event_log() {
    let dir = temp_dir("acp-golden");
    let _server = Server::start(&dir);
    let reg = registry(&dir);
    ok(
        &dir,
        &[
            "agents",
            "add",
            "fake",
            "--registry",
            reg.to_str().expect("utf-8"),
        ],
    );
    let mut log = String::new();
    let mut step = |args: &[&str]| {
        let out = ok(&dir, args);
        write!(log, "$ shed {}\n{out}", args.join(" ")).expect("write to a String");
    };
    step(&[
        "run",
        "--name",
        "a",
        "--label",
        "run=golden",
        "--acp",
        "fake",
    ]);
    step(&[
        "wait",
        "a",
        "--output",
        "ACP session ready",
        "--timeout",
        "10",
    ]);
    step(&["send", "a", "tool"]);
    step(&["wait", "a", "--until", "blocked", "--timeout", "10"]);
    step(&["approve", "a", "allow-once"]);
    step(&["wait", "a", "--until", "done", "--timeout", "10"]);
    step(&["read", "a"]);
    step(&["send", "a", "hi"]);
    step(&["wait", "a", "--until", "done", "--timeout", "10"]);
    step(&["read", "a", "--lines", "2"]);
    let dones = std::cell::Cell::new(0);
    let events = events_until(&dir, "golden", |e| {
        dones.set(dones.get() + usize::from(is_state("done")(e)));
        dones.get() == 2
    });
    let events = events.join("\n");
    writeln!(log, "$ shed events --label run=golden --since 0\n{events}").expect("write");
    let transcript = std::fs::read_to_string(dir.join("acp/a.jsonl")).expect("the transcript");
    assert_eq!(transcript.lines().count(), 13, "{transcript}");

    let golden = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/acp.txt");
    if std::env::var_os("SHED_BLESS").is_some() {
        std::fs::write(&golden, &log).expect("write golden");
    }
    let have = std::fs::read_to_string(&golden).unwrap_or_default();
    assert_eq!(
        have,
        log,
        "{} differs; SHED_BLESS=1 to update",
        golden.display()
    );
}

#[test]
fn cancel_ends_a_turn_mid_way() {
    let dir = temp_dir("acp-cancel");
    let _server = Server::start(&dir);
    let agent = fake_agent();
    let agent = agent.to_str().expect("utf-8");
    let run = [
        "run",
        "--name",
        "c",
        "--label",
        "run=cancel",
        "--acp",
        "fake",
        "--",
    ];
    ok(&dir, &[&run[..], &["python3", agent]].concat());
    ok(
        &dir,
        &[
            "wait",
            "c",
            "--output",
            "ACP session ready",
            "--timeout",
            "10",
        ],
    );
    for cancel in [&["cancel", "c"][..], &["keys", "c", "esc"][..]] {
        ok(&dir, &["send", "c", "slow"]);
        let seen = std::cell::Cell::new(0);
        events_until(&dir, "cancel", |e| {
            if e["event"] == "text" {
                seen.set(seen.get() + 1);
            }
            e["event"] == "text" && seen.get() == if cancel[0] == "cancel" { 1 } else { 2 }
        });
        ok(&dir, cancel);
        ok(&dir, &["wait", "c", "--until", "done", "--timeout", "10"]);
        let info: Value = serde_json::from_str(&ok(&dir, &["get", "c"])).expect("JSON");
        assert_eq!(
            info["reported"],
            json!({"state": "done", "note": "cancelled"})
        );
    }
    assert_eq!(
        ok(&dir, &["read", "c"]),
        "> slow\nthinking\n> slow\nthinking\n"
    );
}

#[test]
fn the_deny_policy_rejects_permission_requests() {
    let dir = temp_dir("acp-deny");
    let _server = Server::start(&dir);
    let agent = fake_agent();
    let agent = agent.to_str().expect("utf-8");
    let args = [
        "run", "--name", "d", "--label", "run=deny", "--acp", "fake", "--policy", "deny", "--",
    ];
    ok(&dir, &[&args[..], &["python3", agent]].concat());
    ok(
        &dir,
        &[
            "wait",
            "d",
            "--output",
            "ACP session ready",
            "--timeout",
            "10",
        ],
    );
    ok(&dir, &["send", "d", "tool"]);
    ok(&dir, &["wait", "d", "--until", "done", "--timeout", "10"]);
    let events = events_until(&dir, "deny", is_state("done"));
    let kinds: Vec<Value> = events
        .iter()
        .map(|l| serde_json::from_str::<Value>(l).expect("JSON"))
        .filter(|e| e["event"] == "approved" || e["event"] == "state")
        .map(|e| json!([e["event"], e.get("option").or(e.get("state"))]))
        .collect();
    assert_eq!(
        kinds,
        [
            json!(["state", "working"]),
            json!(["approved", "reject-once"]),
            json!(["state", "done"])
        ]
    );
    assert_eq!(
        ok(&dir, &["read", "d"]),
        "> tool\n[tool] Write notes.txt (failed)\ndenied\n"
    );
}

#[test]
fn an_agent_that_fails_the_handshake_reports_error() {
    let dir = temp_dir("acp-error");
    let _server = Server::start(&dir);
    let args = [
        "run", "--name", "e", "--acp", "nope", "--", "sh", "-c", "read x",
    ];
    ok(&dir, &args);
    let out = shed(&dir, &["wait", "e", "--until", "error", "--timeout", "10"]);
    let info: Value = serde_json::from_str(&ok(&dir, &["get", "e"])).expect("JSON");
    assert_eq!(
        info["reported"],
        json!({"state": "error", "note": "acp: the agent closed its stdout"}),
        "{out:?}"
    );
    let missing = shed(&dir, &["run", "--acp", "nope"]);
    let err = String::from_utf8_lossy(&missing.stderr);
    assert!(err.contains("ACP agent nope is not installed"), "{err}");
}

/// Starts the fake agent as terminal `name` (labelled `run=<name>`) under registry id `id`, ready for a
/// prompt.
fn start_fake(dir: &Path, name: &str, id: &str) {
    let agent = fake_agent();
    let agent = agent.to_str().expect("utf-8");
    let label = format!("run={name}");
    let run = ["run", "--name", name, "--label", &label, "--acp", id, "--"];
    ok(dir, &[&run[..], &["python3", agent]].concat());
    let ready = [
        "wait",
        name,
        "--output",
        "ACP session ready",
        "--timeout",
        "10",
    ];
    ok(dir, &ready);
}

fn reported(dir: &Path, name: &str) -> Value {
    let info: Value = serde_json::from_str(&ok(dir, &["get", name])).expect("JSON");
    info["reported"].clone()
}

#[test]
fn an_expired_login_ends_the_turn_as_an_auth_error() {
    let dir = temp_dir("acp-auth");
    let _server = Server::start(&dir);
    start_fake(&dir, "x", "claude-acp");
    let line = "Failed to authenticate: OAuth session expired and could not be refreshed";
    let sent = std::time::Instant::now();
    ok(&dir, &["send", "x", "auth"]);
    let until = ["wait", "x", "--until", "done", "blocked", "error"];
    assert_eq!(
        ok(&dir, &[&until[..], &["--timeout", "10"]].concat()),
        "x error\n"
    );
    let took = sent.elapsed();
    assert!(took < std::time::Duration::from_secs(1), "took {took:?}");
    assert_eq!(
        reported(&dir, "x"),
        json!({"state": "error", "note": format!("auth: {line}")})
    );
    let events = events_until(&dir, "x", |e| e["event"] == "reply");
    let reply: Value = serde_json::from_str(events.last().expect("a reply")).expect("JSON");
    assert_eq!(reply["text"], line);
    assert_eq!(reply["stop_reason"], "auth_error");
    // The same text from an agent with no auth patterns is an ordinary chunk; the turn goes on.
    start_fake(&dir, "y", "fake");
    ok(&dir, &["send", "y", "auth"]);
    let out = shed(
        &dir,
        &["wait", "y", "--until", "done", "error", "--timeout", "1"],
    );
    assert_eq!(out.status.code(), Some(124), "{out:?}");
}

#[test]
fn the_watchdog_ends_a_silent_turn_but_not_a_busy_one() {
    let dir = temp_dir("acp-idle");
    let env = [("SHED_TURN_IDLE_TIMEOUT", "1")];
    let _server = Server::with_env(&dir, "3600", &env);
    start_fake(&dir, "w", "fake");
    let until = [
        "wait",
        "w",
        "--until",
        "done",
        "blocked",
        "error",
        "--timeout",
        "10",
    ];
    ok(&dir, &["send", "w", "busy"]);
    assert_eq!(ok(&dir, &until), "w done\n");
    assert_eq!(
        reported(&dir, "w"),
        json!({"state": "done", "note": "end_turn"})
    );
    ok(&dir, &["send", "w", "silent"]);
    assert_eq!(ok(&dir, &until), "w error\n");
    assert_eq!(
        reported(&dir, "w"),
        json!({"state": "error", "note": "no response"})
    );
    assert_eq!(
        ok(&dir, &["read", "w"]),
        "> busy\n[tool] Build (completed)\nbusy done\n> silent\n"
    );
}

//! The ACP benches (`scripts/bench-acp.sh` runs them): `fake_acp.py` streams 1,000 `session/update`s
//! through the bridge to a socket client (at a PR's base and head), and a 1,000-update `session/load`
//! replay (at the head).

mod common;

use std::io::BufRead as _;
use std::path::Path;
use std::process::Command;
use std::time::Instant;

use common::{Server, temp_dir};
use serde_json::Value;
use shed_server::acp::{Input, frame};
use shed_server::proto::{EventsParams, Method, SendParams};
use shed_server::{Client, Paths};

/// Updates per turn or replay.
const UPDATES: usize = 1_000;

fn shed(dir: &Path, args: &[&str]) {
    let out = Command::new(env!("CARGO_BIN_EXE_shed"))
        .arg("--dir")
        .arg(dir)
        .args(args)
        .output()
        .expect("shed runs");
    assert!(out.status.success(), "shed {args:?} failed: {out:?}");
}

/// Starts the fake agent as terminal `name`, labelled `run=<name>`, `extra` passed to `run` and `fake`
/// to the agent.
fn start(dir: &Path, name: &str, extra: &[&str], fake: &[&str]) {
    let agent = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fake_acp.py");
    let label = format!("run={name}");
    let run = ["run", "--name", name, "--label", &label, "--acp", "fake"];
    let cmd = ["--", "python3", agent.to_str().expect("utf-8")];
    shed(dir, &[&run[..], extra, &cmd, fake].concat());
}

/// The events of terminals labelled `run=<name>` from now on.
fn events(dir: &Path, name: &str) -> impl Iterator<Item = Value> {
    let mut client = Client::connect(&Paths::in_dir(dir).socket).expect("connect");
    let params = EventsParams {
        labels: [("run".to_owned(), name.to_owned())].into(),
        since: None,
    };
    let _: Value = client.call(Method::Events, &params).expect("events");
    let lines = client.split().0.lines();
    lines.map(|l| serde_json::from_str(&l.expect("an event line")).expect("event JSON"))
}

/// Reads `events` until one is `last`; returns how many were `text`.
fn texts_until(events: &mut impl Iterator<Item = Value>, last: &str) -> usize {
    let mut texts = 0;
    for e in events {
        if e["event"] == last {
            return texts;
        }
        texts += usize::from(e["event"] == "text");
    }
    panic!("the events ended before {last}");
}

#[test]
#[ignore = "a timing bench: scripts/bench-acp.sh"]
fn throughput() {
    let dir = temp_dir("acp-bench");
    let _server = Server::start(&dir);
    start(&dir, "b", &[], &[]);
    let ready = [
        "wait",
        "b",
        "--output",
        "ACP session ready",
        "--timeout",
        "10",
    ];
    shed(&dir, &ready);
    let mut client = Client::connect(&Paths::in_dir(&dir).socket).expect("connect");
    // The best of 3 turns: from the prompt to its reply at a socket client.
    let us = (0..3)
        .map(|_| {
            let mut events = events(&dir, "b");
            let text = frame(&Input::Prompt(format!("flood {UPDATES}")));
            let start = Instant::now();
            let params = SendParams {
                name: "b".to_owned(),
                text,
            };
            let _: Value = client.call(Method::Send, &params).expect("send");
            assert_eq!(texts_until(&mut events, "reply"), UPDATES);
            start.elapsed().as_micros()
        })
        .min()
        .unwrap_or_default();
    println!("acp throughput: {us} us");
}

#[test]
#[ignore = "a timing bench: scripts/bench-acp.sh"]
fn replay() {
    let dir = temp_dir("acp-replay");
    let history = dir.join("history.jsonl");
    let chunk =
        r#"{"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "x"}}"#;
    std::fs::write(&history, format!("{chunk}\n").repeat(UPDATES)).expect("the history");
    let _server = Server::start(&dir);
    let mut events = events(&dir, "r");
    let began = Instant::now();
    let history = history.to_str().expect("utf-8");
    start(&dir, "r", &["--resume", "sess-1"], &["--history", history]);
    // The session event follows the replay.
    assert_eq!(texts_until(&mut events, "session"), UPDATES);
    println!("acp replay: {} us", began.elapsed().as_micros());
}

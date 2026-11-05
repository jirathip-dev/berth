//! `docs/protocol.md` is generated from the types; this test fails when it drifts.
//! Regenerate with `SHED_BLESS=1 cargo test -p shed-proto`.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde_json::{Value, json, to_value};
use shed_proto::{
    ApprovalOption, EmitParams, ErrorCode, Event, EventKind, EventsParams, Hello, KeysParams,
    LsParams, Method, ReadParams, ReadResult, Reported, Request, ResizeParams, Resources, Response,
    RpcError, RunParams, SendParams, State, StateParams, Target, TerminalInfo, VERSION,
    WaitOutcome, WaitParams, WaitResult,
};

fn info(state: State) -> TerminalInfo {
    TerminalInfo {
        name: "build".into(),
        labels: BTreeMap::from([("issue".into(), "7".into()), ("role".into(), "impl".into())]),
        cwd: PathBuf::from("/home/user/project"),
        cmd: vec!["make".into(), "test".into()],
        keep: true,
        state,
        exit: (state == State::Exited).then_some(0),
        pid: (state == State::Running).then_some(4242),
        reported: None,
        idle_after: None,
        resources: (state == State::Running).then_some(RESOURCES),
        acp: None,
    }
}

const RESOURCES: Resources = Resources {
    rss_mb: 52,
    cpu_pct: 3,
};

/// One example of each event kind, in the order a terminal's life emits them.
fn events() -> Vec<Event> {
    [
        EventKind::Started,
        EventKind::State(done()),
        EventKind::Resources(RESOURCES),
        EventKind::Exited { code: Some(0) },
        EventKind::Killed,
        EventKind::Turn {
            prompt: "Fix the failing test".into(),
        },
        EventKind::Text {
            text: "Looking at the test".into(),
        },
        EventKind::ToolCall {
            id: "call_1".into(),
            title: "Edit src/lib.rs".into(),
            status: "pending".into(),
        },
        EventKind::Plan {
            entries: vec![
                json!({"content": "Fix the test", "priority": "high", "status": "in_progress"}),
            ],
        },
        EventKind::Mode {
            mode: "default".into(),
        },
        EventKind::ApprovalRequest {
            tool: "Edit src/lib.rs".into(),
            options: vec![
                ApprovalOption {
                    id: "allow".into(),
                    name: "Allow".into(),
                    kind: "allow_once".into(),
                },
                ApprovalOption {
                    id: "reject".into(),
                    name: "Reject".into(),
                    kind: "reject_once".into(),
                },
            ],
        },
        EventKind::Approved {
            option: "allow".into(),
        },
        EventKind::Reply {
            text: "Fixed; all 12 tests pass.".into(),
            stop_reason: "end_turn".into(),
        },
    ]
    .into_iter()
    .zip(1..)
    .map(|(kind, seq)| Event {
        seq,
        name: "build".into(),
        labels: info(State::Running).labels,
        kind,
    })
    .collect()
}

fn done() -> Reported {
    Reported {
        state: "done".into(),
        note: Some("tests pass".into()),
    }
}

fn target() -> Value {
    to_value(Target {
        name: "build".into(),
    })
    .unwrap()
}

fn events_params() -> Value {
    to_value(EventsParams {
        labels: BTreeMap::from([("issue".into(), "7".into())]),
        since: Some(120),
    })
    .unwrap()
}

fn run_params() -> Value {
    to_value(RunParams {
        name: Some("build".into()),
        labels: info(State::Running).labels,
        cwd: PathBuf::from("/home/user/project"),
        keep: true,
        idle_after: None,
        cmd: vec!["make".into(), "test".into()],
        acp: None,
    })
    .unwrap()
}

fn emit_params() -> Value {
    to_value(EmitParams {
        name: "agent".into(),
        kind: EventKind::Reply {
            text: "All 12 tests pass.".into(),
            stop_reason: "end_turn".into(),
        },
    })
    .unwrap()
}

#[test]
fn emit_params_round_trip() {
    let p: EmitParams = serde_json::from_value(emit_params()).unwrap();
    assert_eq!(p.name, "agent");
    assert!(p.kind.is_acp());
}

/// An example params and result for each method, built from the typed structs.
fn example(method: Method) -> (Value, Value) {
    let running = || to_value(info(State::Running)).unwrap();
    let exited = || to_value(info(State::Exited)).unwrap();
    match method {
        Method::Hello => {
            let hello = to_value(Hello::ours()).unwrap();
            (hello.clone(), hello)
        }
        Method::Run => (run_params(), running()),
        Method::Ls => (
            to_value(LsParams {
                labels: BTreeMap::from([("issue".into(), "7".into())]),
            })
            .unwrap(),
            json!([running()]),
        ),
        Method::Get | Method::Attach => (target(), running()),
        Method::Read => (
            to_value(ReadParams {
                name: "build".into(),
                lines: Some(2),
                history: false,
            })
            .unwrap(),
            to_value(ReadResult {
                text: "ok 12 tests\n$".into(),
            })
            .unwrap(),
        ),
        Method::Send => (
            to_value(SendParams {
                name: "build".into(),
                text: "make lint\n".into(),
            })
            .unwrap(),
            json!({}),
        ),
        Method::Keys => (
            to_value(KeysParams {
                name: "build".into(),
                keys: vec!["ctrl+c".into(), "enter".into()],
            })
            .unwrap(),
            json!({}),
        ),
        Method::Resize => (
            to_value(ResizeParams {
                name: "build".into(),
                rows: 50,
                cols: 120,
            })
            .unwrap(),
            json!({}),
        ),
        Method::Kill | Method::Rm => (target(), exited()),
        Method::State => (
            to_value(StateParams {
                name: "build".into(),
                report: done(),
            })
            .unwrap(),
            to_value(TerminalInfo {
                reported: Some(done()),
                ..info(State::Running)
            })
            .unwrap(),
        ),
        Method::Wait => (
            to_value(WaitParams {
                name: "build".into(),
                until: vec!["done".into(), "blocked".into()],
                output: Some("^ok \\d+ tests$".into()),
                timeout: Some(600.0),
            })
            .unwrap(),
            to_value(WaitResult {
                outcome: WaitOutcome::Matched,
                terminal: TerminalInfo {
                    reported: Some(done()),
                    ..info(State::Running)
                },
            })
            .unwrap(),
        ),
        Method::Events => (events_params(), json!({})),
        Method::Emit => (emit_params(), json!({})),
    }
}

fn line(v: &impl serde::Serialize) -> String {
    serde_json::to_string(v).unwrap()
}

fn markdown() -> String {
    let mut md = format!(
        "# shed protocol\n\n\
         <!-- Generated from crates/shed-proto by `SHED_BLESS=1 cargo test -p shed-proto`; do not edit. -->\n\n\
         Version: `{VERSION}`. Client and server must run the same version.\n\n\
         Newline-delimited [JSON-RPC 2.0](https://www.jsonrpc.org/specification) over a Unix socket, mode 0600:\n\
         `$XDG_RUNTIME_DIR/shed.sock` (macOS: `~/Library/Caches/shed/shed.sock`). One request per line, one\n\
         response per line; a request without an `id` is a notification and gets no response.\n\n\
         Every connection starts with `hello`. Any other method first is refused with `NO_HELLO`; another\n\
         product or version is refused with `SHED_VERSION: client shed x, server shed y; restart the server`\n\
         (a peer that sends no `product` shows as `unnamed`).\n\n\
         A terminal, as `run`, `ls`, `get`, `kill` and `rm` return it: `state` is `running` or `exited`; `exit` is\n\
         the exit status (128 + N for signal N), `null` while running or when the program was lost in a server\n\
         restart; `pid` is set while running; `reported` is the state the program last reported (absent until\n\
         then); `idle_after` is set when the `idle` fallback is on; `resources` is the process tree's memory\n\
         (MiB) and CPU (percent of one core) at the last sample, while running.\n\n## Methods\n"
    );
    for method in Method::ALL {
        let (params, result) = example(method);
        let request = Request::new(1, method, params);
        let response = Response::ok(1.into(), result);
        write!(
            md,
            "\n### `{}`\n\n{}\n\n```json\n{}\n{}\n```\n",
            method.name(),
            method.summary(),
            line(&request),
            line(&response)
        )
        .unwrap();
    }
    md.push_str(
        "\n## Events\n\nAn event names its terminal, carries its labels and counts up in `seq`; `event` is \
         `started`, `state` (the reported state changed), `exited` (by itself, with its `code`), `killed` (by \
         `kill`) or `resources` (memory or CPU moved by more than 10 % since the last one).\n\n\
         An ACP terminal (`shed run --acp`) adds its agent's events, emitted by its bridge: `turn` (a prompt), \
         `text` (a reply chunk), `tool_call` (started or changed), `plan`, `mode`, `approval_request` (answer \
         with `shed approve`), `approved` and `reply` (the turn's whole reply and its `stop_reason`).\n\n```json\n",
    );
    for event in events() {
        writeln!(md, "{}", line(&event)).unwrap();
    }
    md.push_str("```\n");
    md.push_str("\n## Errors\n\nAn error's `message` starts with its name: `NAME: detail`.\n\n");
    md.push_str("| Code | Name |\n|---|---|\n");
    for code in ErrorCode::ALL {
        writeln!(md, "| {} | `{}` |", code.code(), code.name()).unwrap();
    }
    let refusal = Response::err(1.into(), RpcError::version("0.1.0", VERSION));
    write!(md, "\n```json\n{}\n```\n", line(&refusal)).unwrap();
    md
}

#[test]
fn protocol_md_matches_the_types() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/protocol.md");
    let want = markdown();
    if std::env::var_os("SHED_BLESS").is_some() {
        std::fs::write(&path, &want).unwrap();
    }
    let have = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        have == want,
        "docs/protocol.md is stale; run `SHED_BLESS=1 cargo test -p shed-proto`"
    );
}

#[test]
fn method_names_round_trip() {
    for method in Method::ALL {
        assert_eq!(Method::parse(method.name()), Some(method));
    }
    assert_eq!(Method::parse("nope"), None);
}

#[test]
fn version_refusal_reads_as_specified() {
    assert_eq!(
        RpcError::version("0.1.0", "0.2.0").message,
        "SHED_VERSION: client 0.1.0, server 0.2.0; restart the server"
    );
}

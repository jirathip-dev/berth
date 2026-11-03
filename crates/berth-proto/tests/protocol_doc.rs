//! `docs/protocol.md` is generated from the types; this test fails when it drifts.
//! Regenerate with `BERTH_BLESS=1 cargo test -p berth-proto`.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use berth_proto::{
    ErrorCode, Hello, KeysParams, LsParams, Method, ReadParams, ReadResult, Request, ResizeParams,
    Response, RpcError, RunParams, SendParams, State, Target, TerminalInfo, VERSION,
};
use serde_json::{Value, json, to_value};

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
    }
}

fn target() -> Value {
    to_value(Target {
        name: "build".into(),
    })
    .unwrap()
}

/// An example params and result for each method, built from the typed structs.
fn example(method: Method) -> (Value, Value) {
    let running = || to_value(info(State::Running)).unwrap();
    let exited = || to_value(info(State::Exited)).unwrap();
    match method {
        Method::Hello => {
            let hello = to_value(Hello {
                version: VERSION.into(),
            })
            .unwrap();
            (hello.clone(), hello)
        }
        Method::Run => (
            to_value(RunParams {
                name: Some("build".into()),
                labels: info(State::Running).labels,
                cwd: PathBuf::from("/home/user/project"),
                keep: true,
                cmd: vec!["make".into(), "test".into()],
            })
            .unwrap(),
            running(),
        ),
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
    }
}

fn line(v: &impl serde::Serialize) -> String {
    serde_json::to_string(v).unwrap()
}

fn markdown() -> String {
    let mut md = format!(
        "# berth protocol\n\n\
         <!-- Generated from crates/berth-proto by `BERTH_BLESS=1 cargo test -p berth-proto`; do not edit. -->\n\n\
         Version: `{VERSION}`. Client and server must run the same version.\n\n\
         Newline-delimited [JSON-RPC 2.0](https://www.jsonrpc.org/specification) over a Unix socket, mode 0600:\n\
         `$XDG_RUNTIME_DIR/berth.sock` (macOS: `~/Library/Caches/berth/berth.sock`). One request per line, one\n\
         response per line; a request without an `id` is a notification and gets no response.\n\n\
         Every connection starts with `hello`. Any other method first is refused with `NO_HELLO`; a different\n\
         version is refused with `BERTH_VERSION: client x, server y; restart the server`.\n\n\
         A terminal, as `run`, `ls`, `get`, `kill` and `rm` return it: `state` is `running` or `exited`; `exit` is\n\
         the exit status (128 + N for signal N), `null` while running or when the program was lost in a server\n\
         restart; `pid` is set while running.\n\n## Methods\n"
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
    if std::env::var_os("BERTH_BLESS").is_some() {
        std::fs::write(&path, &want).unwrap();
    }
    let have = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        have == want,
        "docs/protocol.md is stale; run `BERTH_BLESS=1 cargo test -p berth-proto`"
    );
}

#[test]
fn method_names_round_trip() {
    for method in Method::ALL {
        assert_eq!(Method::parse(method.name()), Some(method));
    }
    assert_eq!(Method::parse("wait"), None);
}

#[test]
fn version_refusal_reads_as_specified() {
    assert_eq!(
        RpcError::version("0.1.0", "0.2.0").message,
        "BERTH_VERSION: client 0.1.0, server 0.2.0; restart the server"
    );
}

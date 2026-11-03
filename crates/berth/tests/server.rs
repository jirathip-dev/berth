//! `berth serve` end to end: a real server in a temp dir, driven over its socket.
//!
//! Every method's request and response is checked against `tests/golden/<method>.jsonl` (the temp
//! dir, version and pids normalised). Regenerate with `BERTH_BLESS=1 cargo test -p berth`.

mod common;

use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

use berth_server::proto::{Method, ReadParams, ReadResult, SendParams, State, TerminalInfo};
use berth_server::{Client, Paths};
use common::{Server, eventually, temp_dir};
use serde_json::{Value, json};

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// A raw connection: exact request and response lines.
struct Conn {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
}

impl Conn {
    fn open(dir: &Path) -> Self {
        let writer = UnixStream::connect(Paths::in_dir(dir).socket).expect("connect");
        Self {
            reader: BufReader::new(writer.try_clone().expect("clone")),
            writer,
        }
    }

    fn line(&mut self, request: &str) -> String {
        self.writer
            .write_all(format!("{request}\n").as_bytes())
            .expect("write");
        let mut reply = String::new();
        self.reader.read_line(&mut reply).expect("read");
        reply.trim_end().to_owned()
    }

    /// Sends one request; returns the request line, the response line and the parsed response.
    fn call(&mut self, id: u64, method: &str, params: &Value) -> (String, String, Value) {
        let request = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let request = request.to_string();
        let response = self.line(&request);
        let value = serde_json::from_str(&response).expect("response is JSON");
        (request, response, value)
    }

    fn result(&mut self, method: &str, params: &Value) -> Value {
        let (_, line, response) = self.call(0, method, params);
        response
            .get("result")
            .cloned()
            .unwrap_or_else(|| panic!("{method} failed: {line}"))
    }
}

/// Collects normalised exchanges per golden file and checks them at the end.
struct Goldens {
    dir: PathBuf,
    files: Vec<(String, String)>,
}

impl Goldens {
    fn record(&mut self, file: &str, (request, response, _): &(String, String, Value)) {
        let normal = |s: &str| {
            let s = s
                .replace(self.dir.to_str().expect("utf-8 dir"), "$DIR")
                .replace(&format!("\"{VERSION}\""), "\"$VERSION\"")
                .replace(&format!("server {VERSION};"), "server $VERSION;");
            let mut out = String::new();
            let mut rest = s.as_str();
            while let Some(at) = rest.find("\"pid\":") {
                let (head, tail) = rest.split_at(at + 6);
                out.push_str(head);
                let digits =
                    tail.len() - tail.trim_start_matches(|c: char| c.is_ascii_digit()).len();
                out.push_str(if digits > 0 { "0" } else { "" });
                rest = &tail[digits..];
            }
            out + rest
        };
        let text = format!("{}\n{}\n", normal(request), normal(response));
        match self.files.iter_mut().find(|(name, _)| name == file) {
            Some((_, body)) => body.push_str(&text),
            None => self.files.push((file.to_owned(), text)),
        }
    }

    fn check(&self) {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden");
        let bless = std::env::var_os("BERTH_BLESS").is_some();
        for (name, want) in &self.files {
            let path = root.join(format!("{name}.jsonl"));
            if bless {
                std::fs::create_dir_all(&root).expect("golden dir");
                std::fs::write(&path, want).expect("write golden");
            }
            let have = std::fs::read_to_string(&path).unwrap_or_default();
            assert_eq!(
                &have,
                want,
                "{} differs; BERTH_BLESS=1 to update",
                path.display()
            );
        }
    }
}

#[test]
fn run_ls_read_kill_over_the_socket_match_the_goldens() {
    let dir = temp_dir("golden");
    let _server = Server::start(&dir);
    let mut goldens = Goldens {
        dir: dir.clone(),
        files: Vec::new(),
    };
    let mut c = Conn::open(&dir);
    let socket_mode = std::fs::metadata(Paths::in_dir(&dir).socket)
        .expect("socket")
        .permissions()
        .mode();
    assert_eq!(socket_mode & 0o777, 0o600);

    let first = c.call(1, "ls", &json!({}));
    goldens.record("errors", &first);
    assert_eq!(first.2["error"]["code"], -32001);

    goldens.record("hello", &c.call(1, "hello", &json!({"version": VERSION})));
    let run = c.call(
        2,
        "run",
        &json!({
            "name": "build",
            "labels": {"issue": "7", "role": "impl"},
            "cwd": dir,
            "cmd": ["sh", "-c", "printf 'hello\\n'; exec sleep 100"],
        }),
    );
    goldens.record("run", &run);
    assert_eq!(run.2["result"]["state"], "running");

    let other =
        json!({"name": "other", "labels": {"role": "rev"}, "cwd": dir, "cmd": ["sleep", "100"]});
    c.result("run", &other);
    let ls = c.call(3, "ls", &json!({"labels": {"role": "impl"}}));
    goldens.record("ls", &ls);
    assert_eq!(ls.2["result"].as_array().map(Vec::len), Some(1));

    goldens.record("get", &c.call(4, "get", &json!({"name": "build"})));
    eventually("hello on screen", || {
        c.result("read", &json!({"name": "build"}))["text"] == "hello"
    });
    let read = c.call(5, "read", &json!({"name": "build"}));
    goldens.record("read", &read);
    assert_eq!(read.2["result"]["text"], "hello");

    goldens.record(
        "send",
        &c.call(6, "send", &json!({"name": "build", "text": "x"})),
    );
    goldens.record(
        "keys",
        &c.call(7, "keys", &json!({"name": "build", "keys": ["enter"]})),
    );

    goldens.record(
        "state",
        &c.call(
            16,
            "state",
            &json!({"name": "build", "state": "working", "note": "step 1"}),
        ),
    );
    goldens.record(
        "wait",
        &c.call(
            17,
            "wait",
            &json!({"name": "build", "until": ["working"], "timeout": 10}),
        ),
    );
    goldens.record(
        "wait",
        &c.call(
            18,
            "wait",
            &json!({"name": "build", "until": ["done"], "timeout": 0.1}),
        ),
    );

    for (id, method, params) in [
        (8, "events", json!({"name": "build"})),
        (19, "state", json!({"name": "build", "state": "idle"})),
        (20, "wait", json!({"name": "build"})),
        (21, "wait", json!({"name": "build", "output": "[a"})),
        (9, "get", json!({"name": "nope"})),
        (10, "rm", json!({"name": "build"})),
        (11, "run", other.clone()),
        (12, "keys", json!({"name": "build", "keys": ["hyper"]})),
        (13, "run", json!({"cwd": dir, "cmd": []})),
    ] {
        goldens.record("errors", &c.call(id, method, &params));
    }
    goldens.record("errors", &(String::from("{"), c.line("{"), Value::Null));

    let kill = c.call(14, "kill", &json!({"name": "build"}));
    goldens.record("kill", &kill);
    assert_eq!(kill.2["result"]["state"], "exited");
    assert_eq!(kill.2["result"]["exit"], 143);
    let rm = c.call(15, "rm", &json!({"name": "build"}));
    goldens.record("rm", &rm);
    let left = c.result("ls", &json!({}));
    assert_eq!(left.as_array().map(Vec::len), Some(1));
    goldens.check();
}

#[test]
fn a_kept_terminal_comes_back_as_a_shell_in_its_cwd() {
    let dir = temp_dir("restart");
    let work = dir.join("work");
    std::fs::create_dir(&work).expect("work dir");
    let paths = Paths::in_dir(&dir);
    let server = Server::start(&dir);
    let mut client = Client::connect(&paths.socket).expect("connect");
    let run = |client: &mut Client, name: &str, keep: bool| -> TerminalInfo {
        let params = json!({
            "name": name, "labels": {"issue": "7"}, "cwd": work, "keep": keep,
            "cmd": ["sh", "-c", format!("echo {name} >> ran; exec sleep 100")],
        });
        client.call(Method::Run, &params).expect("run")
    };
    let kept = run(&mut client, "kept", true);
    run(&mut client, "lost", false);
    let ran = || std::fs::read_to_string(work.join("ran")).unwrap_or_default();
    eventually("both tasks ran", || ran().lines().count() == 2);
    run(&mut client, "killed", true);
    eventually("the third task ran", || ran().lines().count() == 3);
    let killed: TerminalInfo = client
        .call(Method::Kill, &json!({"name": "killed"}))
        .expect("kill");
    assert_eq!(killed.state, State::Exited);
    let state_mode = std::fs::metadata(&paths.state)
        .expect("state")
        .permissions()
        .mode();
    assert_eq!(state_mode & 0o777, 0o600);
    drop(client);
    drop(server);

    let _server = Server::start(&dir);
    let mut client = Client::connect(&paths.socket).expect("reconnect");
    let back: TerminalInfo = client
        .call(Method::Get, &json!({"name": "kept"}))
        .expect("get");
    assert_eq!(back.state, State::Running);
    assert_eq!(back.cmd, ["/bin/sh"]);
    assert_eq!((back.cwd, back.labels), (work.clone(), kept.labels));
    assert_ne!(back.pid, kept.pid);
    let lost: TerminalInfo = client
        .call(Method::Get, &json!({"name": "lost"}))
        .expect("get");
    assert_eq!(
        (lost.state, lost.exit, lost.pid),
        (State::Exited, None, None)
    );

    let still: TerminalInfo = client
        .call(Method::Get, &json!({"name": "killed"}))
        .expect("get");
    assert_eq!(
        still, killed,
        "a kept terminal that had exited stays exited"
    );

    let send = SendParams {
        name: "kept".into(),
        text: "echo \"in:$PWD\"\n".into(),
    };
    client.call::<_, Value>(Method::Send, &send).expect("send");
    let want = format!("in:{}", work.display());
    eventually("the shell's pwd", || {
        let read = ReadParams {
            name: "kept".into(),
            lines: None,
            history: false,
        };
        let screen: ReadResult = client.call(Method::Read, &read).expect("read");
        screen.text.replace('\n', "").contains(&want)
    });
    assert_eq!(
        ran(),
        "kept\nlost\nkilled\n",
        "a kept task must never re-run"
    );
}

#[test]
fn a_version_mismatch_is_refused() {
    let dir = temp_dir("version");
    let _server = Server::start(&dir);
    let mut c = Conn::open(&dir);
    let (_, line, _) = c.call(1, "hello", &json!({"version": "0.0.0-other"}));
    assert_eq!(
        line,
        format!(
            r#"{{"jsonrpc":"2.0","id":1,"error":{{"code":-32000,"message":"BERTH_VERSION: client 0.0.0-other, server {VERSION}; restart the server"}}}}"#
        )
    );
    let (_, _, after) = c.call(2, "ls", &json!({}));
    assert_eq!(
        after["error"]["code"], -32001,
        "a refused client stays refused"
    );
}

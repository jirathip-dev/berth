# berth protocol

<!-- Generated from crates/berth-proto by `BERTH_BLESS=1 cargo test -p berth-proto`; do not edit. -->

Version: `0.0.0`. Client and server must run the same version.

Newline-delimited [JSON-RPC 2.0](https://www.jsonrpc.org/specification) over a Unix socket, mode 0600:
`$XDG_RUNTIME_DIR/berth.sock` (macOS: `~/Library/Caches/berth/berth.sock`). One request per line, one
response per line; a request without an `id` is a notification and gets no response.

Every connection starts with `hello`. Any other method first is refused with `NO_HELLO`; a different
version is refused with `BERTH_VERSION: client x, server y; restart the server`.

A terminal, as `run`, `ls`, `get`, `kill` and `rm` return it: `state` is `running` or `exited`; `exit` is
the exit status (128 + N for signal N), `null` while running or when the program was lost in a server
restart; `pid` is set while running; `reported` is the state the program last reported (absent until
then); `idle_after` is set when the `idle` fallback is on; `resources` is the process tree's memory
(MiB) and CPU (percent of one core) at the last sample, while running.

## Methods

### `hello`

The handshake; must come first on every connection. A different version is refused with `BERTH_VERSION`.

```json
{"jsonrpc":"2.0","id":1,"method":"hello","params":{"version":"0.0.0"}}
{"jsonrpc":"2.0","id":1,"result":{"version":"0.0.0"}}
```

### `run`

Start a terminal. `name` defaults to the first free `tN`; `keep` brings it back as a shell in its cwd after a server restart.

```json
{"jsonrpc":"2.0","id":1,"method":"run","params":{"cmd":["make","test"],"cwd":"/home/user/project","keep":true,"labels":{"issue":"7","role":"impl"},"name":"build"}}
{"jsonrpc":"2.0","id":1,"result":{"cmd":["make","test"],"cwd":"/home/user/project","exit":null,"keep":true,"labels":{"issue":"7","role":"impl"},"name":"build","pid":4242,"resources":{"cpu_pct":3,"rss_mb":52},"state":"running"}}
```

### `ls`

List terminals, by name; `labels` keeps those that carry every given label.

```json
{"jsonrpc":"2.0","id":1,"method":"ls","params":{"labels":{"issue":"7"}}}
{"jsonrpc":"2.0","id":1,"result":[{"cmd":["make","test"],"cwd":"/home/user/project","exit":null,"keep":true,"labels":{"issue":"7","role":"impl"},"name":"build","pid":4242,"resources":{"cpu_pct":3,"rss_mb":52},"state":"running"}]}
```

### `get`

One terminal.

```json
{"jsonrpc":"2.0","id":1,"method":"get","params":{"name":"build"}}
{"jsonrpc":"2.0","id":1,"result":{"cmd":["make","test"],"cwd":"/home/user/project","exit":null,"keep":true,"labels":{"issue":"7","role":"impl"},"name":"build","pid":4242,"resources":{"cpu_pct":3,"rss_mb":52},"state":"running"}}
```

### `read`

The visible screen, or with `history` the scrollback and screen, as plain text; `lines` keeps the last N lines.

```json
{"jsonrpc":"2.0","id":1,"method":"read","params":{"history":false,"lines":2,"name":"build"}}
{"jsonrpc":"2.0","id":1,"result":{"text":"ok 12 tests\n$"}}
```

### `send`

Type text into the terminal.

```json
{"jsonrpc":"2.0","id":1,"method":"send","params":{"name":"build","text":"make lint\n"}}
{"jsonrpc":"2.0","id":1,"result":{}}
```

### `keys`

Press keys by name (`enter`, `esc`, `ctrl+c`, `up`, …); nothing is sent if any name is unknown.

```json
{"jsonrpc":"2.0","id":1,"method":"keys","params":{"keys":["ctrl+c","enter"],"name":"build"}}
{"jsonrpc":"2.0","id":1,"result":{}}
```

### `kill`

End the terminal's whole process tree (SIGTERM, then SIGKILL); returns the exited terminal.

```json
{"jsonrpc":"2.0","id":1,"method":"kill","params":{"name":"build"}}
{"jsonrpc":"2.0","id":1,"result":{"cmd":["make","test"],"cwd":"/home/user/project","exit":0,"keep":true,"labels":{"issue":"7","role":"impl"},"name":"build","pid":null,"state":"exited"}}
```

### `rm`

Remove an exited terminal; returns it as it was.

```json
{"jsonrpc":"2.0","id":1,"method":"rm","params":{"name":"build"}}
{"jsonrpc":"2.0","id":1,"result":{"cmd":["make","test"],"cwd":"/home/user/project","exit":0,"keep":true,"labels":{"issue":"7","role":"impl"},"name":"build","pid":null,"state":"exited"}}
```

### `resize`

Resize the terminal; the program gets SIGWINCH.

```json
{"jsonrpc":"2.0","id":1,"method":"resize","params":{"cols":120,"name":"build","rows":50}}
{"jsonrpc":"2.0","id":1,"result":{}}
```

### `attach`

Take over the terminal. After the response the server writes a redraw (clear, then the screen text) and the program's raw output, and closes the connection when the program's PTY closes. The client sends only notifications from then on (`send`, `keys`, `resize`); detaching is closing the connection.

```json
{"jsonrpc":"2.0","id":1,"method":"attach","params":{"name":"build"}}
{"jsonrpc":"2.0","id":1,"result":{"cmd":["make","test"],"cwd":"/home/user/project","exit":null,"keep":true,"labels":{"issue":"7","role":"impl"},"name":"build","pid":4242,"resources":{"cpu_pct":3,"rss_mb":52},"state":"running"}}
```

### `state`

Report the terminal's state (`working`, `blocked`, `done` or `error`) with an optional note; a program can also print `ESC ] 7501 ; state=<s>[;note=<n>] BEL`, which never reaches the screen.

```json
{"jsonrpc":"2.0","id":1,"method":"state","params":{"name":"build","note":"tests pass","state":"done"}}
{"jsonrpc":"2.0","id":1,"result":{"cmd":["make","test"],"cwd":"/home/user/project","exit":null,"keep":true,"labels":{"issue":"7","role":"impl"},"name":"build","pid":4242,"reported":{"note":"tests pass","state":"done"},"resources":{"cpu_pct":3,"rss_mb":52},"state":"running"}}
```

### `wait`

Answer once the reported state is one of `until` (`idle` too, from the `idle_after` fallback) or a visible screen line matches the pattern `output` (`matched`), the program exits (`exited`) or `timeout` seconds pass (`timeout`). The server blocks; nothing polls.

```json
{"jsonrpc":"2.0","id":1,"method":"wait","params":{"name":"build","output":"^ok \\d+ tests$","timeout":600.0,"until":["done","blocked"]}}
{"jsonrpc":"2.0","id":1,"result":{"outcome":"matched","terminal":{"cmd":["make","test"],"cwd":"/home/user/project","exit":null,"keep":true,"labels":{"issue":"7","role":"impl"},"name":"build","pid":4242,"reported":{"note":"tests pass","state":"done"},"resources":{"cpu_pct":3,"rss_mb":52},"state":"running"}}}
```

### `events`

Stream events. After the response the server writes one event per line (see Events) for the terminals carrying every one of `labels`, first those after `since` still in the ring of the last 1,000, and never closes the connection; closing it unsubscribes.

```json
{"jsonrpc":"2.0","id":1,"method":"events","params":{"labels":{"issue":"7"},"since":120}}
{"jsonrpc":"2.0","id":1,"result":{}}
```

### `emit`

Emit one of an ACP terminal's structured events (`turn`, `text`, `tool_call`, `plan`, `mode`, `approval_request`, `approved`, `reply`); its bridge (`berth run --acp`) calls this. Other events are refused.

```json
{"jsonrpc":"2.0","id":1,"method":"emit","params":{"event":"reply","name":"agent","stop_reason":"end_turn","text":"All 12 tests pass."}}
{"jsonrpc":"2.0","id":1,"result":{}}
```

## Events

An event names its terminal, carries its labels and counts up in `seq`; `event` is `started`, `state` (the reported state changed), `exited` (by itself, with its `code`), `killed` (by `kill`) or `resources` (memory or CPU moved by more than 10 % since the last one).

An ACP terminal (`berth run --acp`) adds its agent's events, emitted by its bridge: `turn` (a prompt), `text` (a reply chunk), `tool_call` (started or changed), `plan`, `mode`, `approval_request` (answer with `berth approve`), `approved` and `reply` (the turn's whole reply and its `stop_reason`).

```json
{"seq":1,"name":"build","labels":{"issue":"7","role":"impl"},"event":"started"}
{"seq":2,"name":"build","labels":{"issue":"7","role":"impl"},"event":"state","state":"done","note":"tests pass"}
{"seq":3,"name":"build","labels":{"issue":"7","role":"impl"},"event":"resources","rss_mb":52,"cpu_pct":3}
{"seq":4,"name":"build","labels":{"issue":"7","role":"impl"},"event":"exited","code":0}
{"seq":5,"name":"build","labels":{"issue":"7","role":"impl"},"event":"killed"}
{"seq":6,"name":"build","labels":{"issue":"7","role":"impl"},"event":"turn","prompt":"Fix the failing test"}
{"seq":7,"name":"build","labels":{"issue":"7","role":"impl"},"event":"text","text":"Looking at the test"}
{"seq":8,"name":"build","labels":{"issue":"7","role":"impl"},"event":"tool_call","id":"call_1","title":"Edit src/lib.rs","status":"pending"}
{"seq":9,"name":"build","labels":{"issue":"7","role":"impl"},"event":"plan","entries":[{"content":"Fix the test","priority":"high","status":"in_progress"}]}
{"seq":10,"name":"build","labels":{"issue":"7","role":"impl"},"event":"mode","mode":"default"}
{"seq":11,"name":"build","labels":{"issue":"7","role":"impl"},"event":"approval_request","tool":"Edit src/lib.rs","options":[{"id":"allow","name":"Allow","kind":"allow_once"},{"id":"reject","name":"Reject","kind":"reject_once"}]}
{"seq":12,"name":"build","labels":{"issue":"7","role":"impl"},"event":"approved","option":"allow"}
{"seq":13,"name":"build","labels":{"issue":"7","role":"impl"},"event":"reply","text":"Fixed; all 12 tests pass.","stop_reason":"end_turn"}
```

## Errors

An error's `message` starts with its name: `NAME: detail`.

| Code | Name |
|---|---|
| -32700 | `PARSE` |
| -32600 | `INVALID_REQUEST` |
| -32601 | `NO_SUCH_METHOD` |
| -32602 | `INVALID_PARAMS` |
| -32603 | `INTERNAL` |
| -32000 | `BERTH_VERSION` |
| -32001 | `NO_HELLO` |
| -32002 | `NO_SUCH_TERMINAL` |
| -32003 | `NAME_TAKEN` |
| -32004 | `STILL_RUNNING` |
| -32005 | `SPAWN_FAILED` |

```json
{"jsonrpc":"2.0","id":1,"error":{"code":-32000,"message":"BERTH_VERSION: client 0.1.0, server 0.0.0; restart the server"}}
```

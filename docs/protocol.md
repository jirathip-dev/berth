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
restart; `pid` is set while running.

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
{"jsonrpc":"2.0","id":1,"result":{"cmd":["make","test"],"cwd":"/home/user/project","exit":null,"keep":true,"labels":{"issue":"7","role":"impl"},"name":"build","pid":4242,"state":"running"}}
```

### `ls`

List terminals, by name; `labels` keeps those that carry every given label.

```json
{"jsonrpc":"2.0","id":1,"method":"ls","params":{"labels":{"issue":"7"}}}
{"jsonrpc":"2.0","id":1,"result":[{"cmd":["make","test"],"cwd":"/home/user/project","exit":null,"keep":true,"labels":{"issue":"7","role":"impl"},"name":"build","pid":4242,"state":"running"}]}
```

### `get`

One terminal.

```json
{"jsonrpc":"2.0","id":1,"method":"get","params":{"name":"build"}}
{"jsonrpc":"2.0","id":1,"result":{"cmd":["make","test"],"cwd":"/home/user/project","exit":null,"keep":true,"labels":{"issue":"7","role":"impl"},"name":"build","pid":4242,"state":"running"}}
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

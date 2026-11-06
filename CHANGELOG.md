# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- Renamed from berth to shed: the crates (`shed`, `shed-core`, `shed-proto`, `shed-server`), the binary,
  the socket (`shed.sock`), the state and config dirs, the service units (`dev.shed.serve`,
  `shed.service`) and the docs. On its first start on the per-user paths, `shed serve` moves an existing
  `berth` state or config dir to the `shed` one (one rename, one log line); when both exist it leaves the
  old one alone and warns. Terminals get `$SHED`; `$BERTH` is still set for one release. The `hello`
  carries `product: "shed"`, so a berth client or server is refused with `SHED_VERSION` instead of
  half-working. OSC 7501 is unchanged.

### Fixed

- One server per state file: `shed serve` holds a lock (`state.lock` beside the state file) for its life, so a
  second server is refused even when the first one's socket file is gone, instead of restoring the kept
  terminals twice and taking the socket. A killed server's lock goes with it. Two `run --autostart` at once
  share the one server that wins.
- `shed rm` also removes an ACP terminal's transcript, and `shed serve` removes the logs and transcripts of
  terminals its state file no longer names.
- `shed machine add|rm` writes the config through a temp file of its own, so writers running at once no longer
  fail on a shared one.

### Added

- `shed machine stats [--on M] [--since T]` and the `machine.stats` method: load per CPU, memory pressure,
  swap used, free disk on the state volume, CPUs, uptime and the terminals by state. The server keeps one
  sample a minute for 24 h (`stats.json` beside the state file); `--since` returns them. With `--on`, a
  machine that gives no answer within 5 s is reported offline with when it was last seen (exit 1).
- `shed install [--force]` writes the per-user job for `shed serve` (macOS: `~/Library/LaunchAgents/dev.shed.serve.plist`,
  RunAtLoad, KeepAlive, a PATH with `~/.cargo/bin` and `~/.local/bin`, logs to `~/Library/Logs/shed.log`;
  Linux: `~/.config/systemd/user/shed.service`) and prints the line that loads it; a file with other contents
  is refused without `--force`. `shed uninstall` prints the lines that unload and remove it. `shed doctor`
  checks the job file, that it is loaded, the socket, the server's version and the log, with a fix line
  for each failure (exit 1).
- Other agents' state escapes: Jan's OSC 9999 (`{"agentType":…,"state":…}`) sets the reported state
  (`waiting` reads as `blocked`) with `state_source: "jan-osc9999"`; garbled or unknown payloads are
  ignored. Each agent is one row in one table.
- `shed events --on <machine>` survives a dropped SSH link: each terminal seen gets a state line noted
  `machine: reconnecting`, the link is retried every 5 s (backing off to 60 s after 5 failures), a
  `machine: connected` line follows once it is back, and the stream resumes after the last `seq`. Names in
  that stream are `<machine>/<name>`.
- `shed serve` prints one line, `shed ready <version> <socket>`, to stdout once its socket accepts
  connections (it replaces the `listening on` stderr line). `serve --detach` and `run --autostart` wait for
  it and say whether the server exited first or was not ready within 10 s.

- ACP turns no longer hang: a reply or adapter error with one of the agent's known auth-failure lines (for
  `claude-acp`: `Failed to authenticate`, `OAuth session expired`) ends the turn as `error`
  (`auth: <the line>`) after a `reply` with `stop_reason` `auth_error`, and a turn that hears nothing from
  the agent for `$SHED_TURN_IDLE_TIMEOUT` seconds (default 120; a waiting permission request aside) ends
  as `error` (`no response`).
- Bounded terminal history: each terminal keeps at most 5,000 lines and 8 MiB of scrollback (was 10,000
  lines), dropping the oldest whole lines. Raw output is appended to `history/<name>.log` beside the state
  file, cut back to its last 8 MiB once it passes 16 MiB. After a server restart, `read` of a lost
  terminal and a kept terminal's new shell start from only the log's last 8 MiB (an incomplete UTF-8
  character at the cut is skipped). `just bench` also reports the history throughput.
- Machines over SSH (M3): `shed machine add <name> <ssh-target>`, `ls` and `rm` (kept in `config.toml`
  under `[machines]`), a global `--on <machine>` that runs the terminal commands (including `attach`,
  `wait` and streaming `events`) on that machine through one `ssh <target> shed connect` (key login only:
  `BatchMode=yes`, a 10 s connect timeout, `ControlMaster=auto` where ssh supports it), and `shed connect
  [--autostart]`, which carries the protocol over stdin/stdout. A shed of another version on the machine
  is refused with the exact `cargo install` command to match it. With `--on`, `run` needs an absolute
  `--cwd`.
- ACP client mode (M2c): `shed run --acp <agent> [--policy allow|deny|ask]` runs any Agent Client Protocol
  (v1) agent as a structured terminal. `send` is a prompt, `approve` answers permission requests (state
  `blocked`), `cancel` (or `keys <t> esc`) cancels the turn, the state is `working` then `done` with the stop
  reason, `read` renders the JSONL transcript (prompts, one line per tool call, replies), and `events` gains
  `turn`, `text`, `tool_call`, `plan`, `mode`, `approval_request`, `approved` and `reply`. `shed agents
  [add|rm]` installs agents from the ACP registry, pinned. The protocol gains `emit`; a terminal carries
  `acp`. See docs/acp.md.
- Events and resources (M2b): `shed events [--label k=v] [--since N]` streams `started`, `state`,
  `exited {code}`, `killed` and `resources {rss_mb, cpu_pct}` as JSON lines, each with a `seq`; `--since`
  replays from a ring of the last 1,000. `shed serve --sample S` (default 2) samples every process
  tree's memory and CPU; `resources` fires when either moves by more than 10 %. `ls` gains RSS and CPU
  columns, `get` a `resources` field, and `shed top` lists terminals by memory. The protocol gains
  `events`.
- State reporting and waiting (M2a): `shed state [<t>] working|blocked|done|error [--note]` (inside a
  terminal `$SHED` names it), the OSC escape `ESC ] 7501 ; state=<s>[;note=<n>] BEL` parsed from the
  output and kept off the screen, `shed wait <t> --until <state>… [--output RE] [--timeout S]` that blocks
  server-side and exits 0 on a match, 1 on exit and 124 on timeout, and `shed run --idle-after S` (report
  `idle` after S silent seconds until the program reports). The protocol gains `state` and `wait`; a
  terminal carries `reported` and `idle_after`.
- The `shed` CLI (M1c): `run`, `ls`, `get`, `read`, `send`, `keys`, `kill`, `rm` and `attach` (raw mode,
  `Ctrl+\` detaches, window resizes forwarded) as a thin client, `--json` on every command, a global
  `--dir`, `shed serve --detach` and `shed run --autostart`. The protocol gains `resize` and `attach`.
- `shed serve` (M1b): the per-user server on a Unix socket (mode 0600) speaking newline-delimited
  JSON-RPC 2.0 (`hello run ls get read send keys kill rm`), unique names and `k=v` labels, a state file
  (atomic writes) that brings `keep` terminals back as shells in their cwd after a restart, and a
  same-version handshake (`SHED_VERSION`). `shed-proto` holds the types; `docs/protocol.md` is
  generated from them. launchd/systemd user-job templates in `dist/` (docs/service.md).
- `shed-core` terminals (M1a): programs on real PTYs, a VT screen model with bounded scrollback,
  named keys, and process-tree kill (SIGTERM, a grace period, SIGKILL) with per-tree RSS/CPU.
- The workspace skeleton (M0): `shed hello`, `--help`, `--version`, and every CI gate.

# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Bounded terminal history: each terminal keeps at most 5,000 lines and 8 MiB of scrollback (was 10,000
  lines), dropping the oldest whole lines. Raw output is appended to `history/<name>.log` beside the state
  file, cut back to its last 8 MiB once it passes 16 MiB. After a server restart, `read` of a lost
  terminal and a kept terminal's new shell start from only the log's last 8 MiB (an incomplete UTF-8
  character at the cut is skipped). `just bench` also reports the history throughput.
- Machines over SSH (M3): `berth machine add <name> <ssh-target>`, `ls` and `rm` (kept in `config.toml`
  under `[machines]`), a global `--on <machine>` that runs the terminal commands (including `attach`,
  `wait` and streaming `events`) on that machine through one `ssh <target> berth connect` (key login only:
  `BatchMode=yes`, a 10 s connect timeout, `ControlMaster=auto` where ssh supports it), and `berth connect
  [--autostart]`, which carries the protocol over stdin/stdout. A berth of another version on the machine
  is refused with the exact `cargo install` command to match it. With `--on`, `run` needs an absolute
  `--cwd`.
- ACP client mode (M2c): `berth run --acp <agent> [--policy allow|deny|ask]` runs any Agent Client Protocol
  (v1) agent as a structured terminal. `send` is a prompt, `approve` answers permission requests (state
  `blocked`), `cancel` (or `keys <t> esc`) cancels the turn, the state is `working` then `done` with the stop
  reason, `read` renders the JSONL transcript (prompts, one line per tool call, replies), and `events` gains
  `turn`, `text`, `tool_call`, `plan`, `mode`, `approval_request`, `approved` and `reply`. `berth agents
  [add|rm]` installs agents from the ACP registry, pinned. The protocol gains `emit`; a terminal carries
  `acp`. See docs/acp.md.
- Events and resources (M2b): `berth events [--label k=v] [--since N]` streams `started`, `state`,
  `exited {code}`, `killed` and `resources {rss_mb, cpu_pct}` as JSON lines, each with a `seq`; `--since`
  replays from a ring of the last 1,000. `berth serve --sample S` (default 2) samples every process
  tree's memory and CPU; `resources` fires when either moves by more than 10 %. `ls` gains RSS and CPU
  columns, `get` a `resources` field, and `berth top` lists terminals by memory. The protocol gains
  `events`.
- State reporting and waiting (M2a): `berth state [<t>] working|blocked|done|error [--note]` (inside a
  terminal `$BERTH` names it), the OSC escape `ESC ] 7501 ; state=<s>[;note=<n>] BEL` parsed from the
  output and kept off the screen, `berth wait <t> --until <state>… [--output RE] [--timeout S]` that blocks
  server-side and exits 0 on a match, 1 on exit and 124 on timeout, and `berth run --idle-after S` (report
  `idle` after S silent seconds until the program reports). The protocol gains `state` and `wait`; a
  terminal carries `reported` and `idle_after`.
- The `berth` CLI (M1c): `run`, `ls`, `get`, `read`, `send`, `keys`, `kill`, `rm` and `attach` (raw mode,
  `Ctrl+\` detaches, window resizes forwarded) as a thin client, `--json` on every command, a global
  `--dir`, `berth serve --detach` and `berth run --autostart`. The protocol gains `resize` and `attach`.
- `berth serve` (M1b): the per-user server on a Unix socket (mode 0600) speaking newline-delimited
  JSON-RPC 2.0 (`hello run ls get read send keys kill rm`), unique names and `k=v` labels, a state file
  (atomic writes) that brings `keep` terminals back as shells in their cwd after a restart, and a
  same-version handshake (`BERTH_VERSION`). `berth-proto` holds the types; `docs/protocol.md` is
  generated from them. launchd/systemd user-job templates in `dist/` (docs/service.md).
- `berth-core` terminals (M1a): programs on real PTYs, a VT screen model with bounded scrollback,
  named keys, and process-tree kill (SIGTERM, a grace period, SIGKILL) with per-tree RSS/CPU.
- The workspace skeleton (M0): `berth hello`, `--help`, `--version`, and every CI gate.

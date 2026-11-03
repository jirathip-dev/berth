# berth

A small runtime that keeps coding-agent terminals alive, reads their screens and reports their state.
Every lane docks in a berth and keeps running there, across restarts and machines.

> **Status: spec only.** No code yet. Experimental; built to be one runtime option for
> [herdsman](https://github.com/jirathip-dev/herdsman) (`runtime = "berth"`) next to herdr.

## Why

herdsman drives a fleet of coding agents. It needs only a few things from a terminal runtime:

- run a program in a terminal that keeps running when nobody watches;
- read the screen;
- send text and keys;
- wait until it stops working;
- list what runs;
- do the same on another machine over SSH.

herdr does all of this and much more (a full TUI, layouts, plugins). berth is the **small, headless** version, shaped
around lanes:

- **State is reported, not guessed.** A program reports `working / blocked / done / error` (and an optional note),
  through a CLI call, the socket or an OSC escape. berth stores and streams it. Screen-scraping is only a fallback.
- **Lanes are first-class.** A terminal carries labels (`fleet`, `issue`, `role`, `round`) and a worktree. List, wait
  and events filter by them.
- **No UI to keep in sync.** berth draws nothing. herdsman's board is the UI, and any terminal can `attach`.
- **One version everywhere.** Client and server are the same binary. A mismatch is refused with a clear message
  instead of half-working (herdr's old-server/new-client gap).
- **Process hygiene built in.** Closing a terminal ends its whole process tree. Per-terminal memory and CPU are
  reported, so a forgotten dev server or language server is visible.

## Scope (v0.1)

| Command | What it does |
|---|---|
| `berth serve` | The per-user server (launchd/systemd user job). Owns the PTYs, the state and the socket. |
| `berth run [--name N] [--label k=v…] [--cwd D] [--keep] -- <cmd…>` | Start a terminal. `--keep`: a lost program is replaced by a shell. |
| `berth ls [--label k=v] [--json]` | List terminals: name, labels, cwd, state, pid tree, RSS/CPU, age. |
| `berth get <t>` | One terminal as JSON. |
| `berth read <t> [--lines N] [--screen\|--history]` | The current screen, or scrollback, as plain text. |
| `berth send <t> <text>` / `berth keys <t> enter esc ctrl+c…` | Type text or press keys. |
| `berth wait <t> [--until working\|idle\|done\|blocked] [--output RE] [--timeout S]` | Block until the state or output matches. |
| `berth state <t> working\|blocked\|done\|error [--note TEXT]` | Report state (also via `$BERTH` + OSC 7501-compatible escape). |
| `berth kill <t>` / `berth rm <t>` | End the process tree; remove a finished terminal. |
| `berth attach <t>` | Take over the terminal interactively; `Ctrl+\` detaches. |
| `berth events [--label k=v]` | Stream state changes and exits as JSON lines. |
| `berth --on <machine> …` / `berth machine add\|ls\|rm` | The same commands on another host over SSH (key login only). |

**Persistence:** the server keeps PTYs alive while it runs. On server restart, kept terminals come back as shells in
their last cwd (never re-running a task). State and labels are saved to a small JSON file (atomic writes).

**The API** is a Unix socket with JSON-RPC (documented schema). The CLI is a thin client. `berth connect` carries the
protocol over stdin/stdout for SSH.

## Non-goals

- No TUI layouts, tabs, splits, themes or plugins.
- No worktree management (herdsman does `git worktree` itself).
- No agent-brand detection. Programs report their state; a screen-text fallback is optional and per-harness.
- No Windows in v0.1.

## Constraints

- **Rust, ≤ 5,000 non-test lines in `src/`** (CI-enforced, like herdsman).
- **Few dependencies:** a PTY crate, a VT parser for the screen, serde/serde_json, clap. No async runtime unless a
  measured need appears.
- **Fast:** every CLI call that doesn't wait is < 20 ms locally.
- **Public repo:** placeholders only in docs and tests, no setup-specific names.

## Integration with herdsman

herdsman gets a `runtime = "herdr" | "berth"` setting (see herdsman's runtime spike). Lanes then:
- run with `berth run --label fleet=… --label issue=… --label role=…`;
- report state through the harness hook (`berth state … working|done|blocked`);
- are read with `berth read`;
- are watched with `berth events` instead of polling.

## Milestones

1. **M1, local core:** `serve`, `run`, `ls`, `get`, `read`, `send`/`keys`, `kill`/`rm`, `attach`, process-tree cleanup,
   the socket API, persistence.
2. **M2, state:** `state`, `wait`, `events`, OSC reporting, the per-terminal resource report.
3. **M3, machines:** `--on`, `machine add/ls/rm`, `connect`, the same-version check.
4. **M4, herdsman integration:** the `runtime = "berth"` adapter in herdsman, tried on one fleet.

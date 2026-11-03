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
| `berth events [--label k=v] [--since N]` | Stream starts, state changes, exits, kills and resource moves as JSON lines; `--since` replays the last 1,000. |
| `berth top` | The terminals by memory use, largest first. |
| `berth run --acp <agent>`, `berth agents`, `berth approve`, `berth cancel` | ACP agents as structured terminals; see below. |
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

## Two modes: terminals and ACP agents

Many agents speak the [Agent Client Protocol](https://agentclientprotocol.com) (ACP): prompts, tool calls, approvals and
the final reply as JSON-RPC messages instead of screen text (an idea from [Codync](https://github.com/leepokai/Codync)).
berth runs them as a second kind of terminal, through the same API:

| Command | What it does |
|---|---|
| `berth agents` / `berth agents add <id>` / `berth agents rm <id>` | The installed ACP agents. `add` installs the version the [ACP registry](https://agentclientprotocol.com) lists now (its binary, `npx` or `uvx` entry), pinned; nothing upgrades it. |
| `berth run --acp <id> [--policy allow\|deny\|ask] [--cwd D] [--label k=v…]` | Start the agent's ACP adapter on stdio: `initialize`, `session/new` (no MCP servers). |
| `berth send <t> <text>` | `session/prompt`; the state is `working` until the turn ends, then `done` (note: the stop reason). |
| `berth approve <t> <option>` | Answer a permission request (state `blocked`); `--policy` answers them instead. |
| `berth cancel <t>` / `berth keys <t> esc` | `session/cancel`. |
| `berth read <t>` | The transcript as text: prompts, one line per tool call, final replies. |

`berth events` adds `turn`, `text`, `tool_call`, `plan`, `mode`, `approval_request`, `approved` and `reply`, and
`berth wait --until done` is exact. The PTY mode stays for agents without ACP. berth never handles provider
credentials: each agent logs in its own way. See [docs/acp.md](docs/acp.md), including the Claude adapter caveat.

## Integration with herdsman

herdsman gets a `runtime = "herdr" | "berth"` setting (see herdsman's runtime spike). Lanes then:
- run with `berth run --label fleet=… --label issue=… --label role=…`;
- report state through the harness hook (`berth state … working|done|blocked`);
- are read with `berth read`;
- are watched with `berth events` instead of polling.

## Engineering standards (from day one)

### Repository layout

```
berth/
├── crates/
│   ├── berth-proto/    # the API types (serde) + JSON-RPC method list; the generated docs/protocol.md comes from here
│   ├── berth-core/     # terminals, screen model (VT parser), state machine, persistence, process trees; no sockets
│   ├── berth-server/   # the per-user server: socket, JSON-RPC, events stream, machines over SSH
│   └── berth/          # the CLI binary (thin client + `berth serve`)
├── tests/              # end-to-end: real PTYs, a fake agent that reports state
├── docs/               # architecture.md, protocol.md, decisions/, cap-history/
└── xtask/ or justfile  # `just check` runs every CI gate locally
```

- **Shipping:** one binary, with a library-first core. The binary crate holds only argument parsing and wiring.
- **Dependency direction** is enforced in CI (a small `cargo metadata` check): `berth-core` never depends on the server or
  CLI crates.
- `docs/` holds `architecture.md` (one page, with a diagram), `protocol.md` (generated from the types), `decisions/`
  (short ADRs, one per real choice) and `cap-history/`.

### Code quality gates (CI blocks a merge on any failure)

- **Formatting:** `cargo fmt --check`.
- **Lints:** `cargo clippy --all-targets -- -D warnings` with `clippy::pedantic` on. Every `allow` carries a reason,
  at most 10 in the workspace.
- **No unsafe:** `#![forbid(unsafe_code)]` in every crate, except a single, reviewed PTY module
  (berth) with a `// SAFETY:` note per block.
- **Tests:** `cargo test --workspace` (unit tests next to the code; integration tests in `tests/`; golden files for every
  protocol message and CLI output). New behaviour comes with a test that fails without it.
- **Dependencies:** `cargo deny check` (licences: MIT/Apache/BSD/ISC only; advisories; no duplicate major versions) and
  a lockfile committed; Dependabot weekly, grouped.
- **Docs:** `cargo doc --no-deps -D warnings` (every public item documented).
- **Size:** the line cap check (`≤ 5,000` non-test lines across `crates/*/src`), plus a soft per-PR line budget stated in
  the PR.
- **MSRV:** pinned in `rust-toolchain.toml` and the workspace `rust-version`; CI tests MSRV and stable.
- **Platforms:** macOS (arm64) and Linux (x86_64, arm64) in the CI matrix.

### Error handling and style

- **Errors:** `thiserror` error enums in library crates, and `anyhow` only in the binary.
- **No panics:** no `unwrap()`/`expect()` outside tests and proven invariants (each with a comment).
- **No async by default:** add it only for a measured need, and write that down as an ADR.
- **Logging:** structured logs with a level (`--log json|text`); secrets are never logged (redaction tested).
- **Configuration:** one TOML file with a documented schema and defaults; unknown keys are an error with a hint.

### Workflow

- `main` is protected: changes land by PR with green CI and a review.
- The PR template asks for: what changed, a `## For humans` section (2–4 plain lines), tests, the line count, and
  "not verified".
- No AI co-author or "generated with" trailers.
- **Versioning:** SemVer, with a `CHANGELOG.md` (Keep a Changelog). Releases are tagged builds with checksums (GitHub
  Actions); `cargo install` works from the repo.
- **Repo files:** `LICENSE` (MIT OR Apache-2.0), `SECURITY.md` (private reporting), `CONTRIBUTING.md` (how to run the
  gates locally: `just check`), and issue templates for bug and feature.
- **Public-repo hygiene:** placeholders only in docs, tests and fixtures; a CI check bans setup-specific names; secret
  scanning on push; secret-shaped test fixtures are built at runtime.

## Milestones

0. ✅ **M0, the skeleton:** the workspace layout above, every CI gate green on an empty `hello` command, plus the PR template, LICENSE, SECURITY, CONTRIBUTING and justfile. Nothing else lands before M0.
1. **M1, local core:** `serve`, `run`, `ls`, `get`, `read`, `send`/`keys`, `kill`/`rm`, `attach`, process-tree cleanup,
   the socket API, persistence.
2. **M2, state:** `state`, `wait`, `events`, OSC reporting, the per-terminal resource report.
3. **M3, machines:** `--on`, `machine add/ls/rm`, `connect`, the same-version check.
4. **M4, herdsman integration:** the `runtime = "berth"` adapter in herdsman, tried on one fleet.

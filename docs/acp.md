# ACP agents

shed runs any [Agent Client Protocol](https://agentclientprotocol.com) agent (Claude Code, Codex, Pi, Gemini, …)
as a structured terminal: prompts, tool calls, approvals and replies come back as events and states instead of
screen text. ACP v1 only; v2 becomes an opt-in once it is stable.

## Install an agent

```sh
shed agents add claude-acp      # or codex-acp, pi-acp, gemini, …
shed agents                     # id, pinned version, distribution, command
shed agents rm claude-acp
```

`add` reads the ACP registry (`--registry <url|file>`, default
`https://cdn.agentclientprotocol.com/registry/v1/latest/registry.json`) and installs the version it lists now:
this platform's `binary` entry (downloaded with `curl`, checked against its `sha256`, unpacked under
`agents/<id>/<version>/` beside the state file), else `npx -y <package@version>`, else `uvx <package>`. The
entry's `args` and `env` are kept. The result goes to `agents.json` beside the state file. Nothing upgrades an
agent: `rm` it and `add` it again.

## Run one

```sh
shed run --acp claude-acp --label issue=7 --cwd /home/user/project
shed send t1 "Fix the failing test"
shed wait t1 --until done blocked
shed approve t1 allow-once        # an option id, or a kind: allow_once, reject_once, …
shed read t1                      # > prompt, [tool] title (status), the final reply
shed cancel t1                    # or: shed keys t1 esc
```

`--policy allow|deny|ask` (default `ask`) answers permission requests: `allow` and `deny` pick the first
allow or reject option at once; `ask` reports `blocked` (note: the tool) until `shed approve`. With a command
after `--`, `run --acp <id> -- <cmd…>` runs that command as the adapter instead of the installed one.

The state is `working` from `send` until the turn ends, `blocked` while a permission request waits, and `done`
(note: the stop reason, e.g. `end_turn` or `cancelled`) after; an agent error is `error`. `shed events`
streams `turn`, `text` (reply chunks), `tool_call` (each status change), `plan`, `mode`, `approval_request`,
`approved` and `reply` (the whole reply and its `stop_reason`) next to the usual `started`, `state` and
`exited`; see [protocol.md](protocol.md).

## How it works

`run --acp` starts an ordinary terminal whose program is shed's bridge (`shed acp-bridge`, hidden). The
bridge runs the adapter with piped stdio, sends `initialize` (no `fs` or `terminal` client capabilities) and
`session/new` (the terminal's cwd, no MCP servers), and then:

- reads its PTY in raw mode: `send`, `approve` and `cancel` write one framed line (`RS`, JSON, newline); a line
  typed in `attach` is a prompt; `Esc` or `Ctrl+C` alone cancels;
- reports its events with the `emit` method and its states with `state`, over the server socket;
- appends every event to `acp/<name>.jsonl` beside the state file, which `read` renders;
- answers requests it does not offer (file system, terminals) with "method not found".

So kill, process-tree cleanup, resources and `wait` work as for any terminal. Agent text is shown with its
control characters replaced, so it cannot drive the reader's terminal. See
[decisions/0003-acp-bridge.md](decisions/0003-acp-bridge.md).

## Accounts

shed passes its environment (the server's, `PATH` included) through to the agent and never handles provider credentials. Each agent logs in
its own way (its own login command, config file or API key variable); log in once outside shed first.

**The Claude adapter** (`claude-acp`, `@agentclientprotocol/claude-agent-acp`) runs Claude Code through the
Claude Agent SDK, not the interactive `claude` CLI. Whether its usage counts against a Claude subscription plan
login, or needs an API key, depends on the adapter's login and Anthropic's current terms for the Agent SDK.
Check both before relying on a plan login, and note that an `ANTHROPIC_API_KEY` in shed's environment
reaches the agent and is likely to be used instead of the plan.

## Manual smoke (owner's machine)

Not run in CI (it needs a real login and network):

1. `shed agents add claude-acp`, with Claude Code already logged in on the plan and no `ANTHROPIC_API_KEY`
   set in the environment of `shed serve`.
2. `shed run --acp claude-acp --name smoke --cwd <a scratch repo>`.
3. `shed wait smoke --output "ACP session ready" --timeout 60`, then `shed send smoke "Say hello in one word"`.
4. `shed wait smoke --until done blocked --timeout 300`; `shed read smoke` shows the reply.
5. The plan's usage page shows the request; nothing is billed to an API key.
6. `shed kill smoke && shed rm smoke`.

## Smoke result (2026-11-05)

Run on the mini with berth `8f077cb` (shed before the rename; `--dir` in a scratch folder, `berth serve` started
under `env -i` with only `HOME`, `PATH`, `USER` and `TERM`, so no `ANTHROPIC_*` variable), adapter `claude-acp` 0.100.0
(`npx -y @agentclientprotocol/claude-agent-acp@0.100.0`, Claude Agent SDK 0.3.316, Claude Code 2.1.316 inside),
Node 24.22.2, the `claude` CLI 2.1.311 logged in with `claude.ai` on a Max plan.

- **Works on the plan login.** Steps 2–6 all exit 0: `send` "Say hello in one word", `wait --until done` reaches
  `done`, `read` shows `Hello!`, and the event log ends with `reply` (`stop_reason` `end_turn`). The session's
  transcript names `claude-sonnet-5-5`. With no API key set and the only credential the `claude.ai` login, the
  request went to the plan; the plan's usage page was not checked (the herdsman meter tracks its own accounts,
  not this login).
- **An expired login hangs the turn.** The first run, before the owner logged in again, got this as reply
  text: `Failed to authenticate: OAuth session expired and could not be refreshed` (plain `claude -p` failed the
  same way). The turn never ended: no `reply` event, so `wait --until done blocked` timed out (exit 124) after
  300 s. Check `claude auth status` (`"loggedIn": true`) before a run.

So cooee can talk to Claude through `claude-acp` on the plan login; the `claude -p` stream-json fallback is not
needed.

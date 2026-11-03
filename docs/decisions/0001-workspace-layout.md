# 0001: Workspace layout

**Status:** accepted (M0)

## Context

berth ships as one binary, but its logic should be testable without a socket or a CLI, and the API types
should be shared by the client and the server without pulling in either.

## Decision

A Cargo workspace with four crates under `crates/`: `berth-proto` (API types), `berth-core` (terminals,
screen, state, persistence), `berth-server` (socket and JSON-RPC) and `berth` (the binary). `berth-core`
depends on no other workspace crate, enforced in CI by `scripts/dep-direction.sh`. A `justfile` instead of
an `xtask` crate runs the gates locally.

## Consequences

- Core logic is tested directly, with no server running.
- Adding a crate dependency that points back up the stack fails CI.
- One more manifest per crate to keep in step; shared settings live in `[workspace.package]`.

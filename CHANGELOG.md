# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `berth serve` (M1b): the per-user server on a Unix socket (mode 0600) speaking newline-delimited
  JSON-RPC 2.0 (`hello run ls get read send keys kill rm`), unique names and `k=v` labels, a state file
  (atomic writes) that brings `keep` terminals back as shells in their cwd after a restart, and a
  same-version handshake (`BERTH_VERSION`). `berth-proto` holds the types; `docs/protocol.md` is
  generated from them. launchd/systemd user-job templates in `dist/` (docs/service.md).
- `berth-core` terminals (M1a): programs on real PTYs, a VT screen model with bounded scrollback,
  named keys, and process-tree kill (SIGTERM, a grace period, SIGKILL) with per-tree RSS/CPU.
- The workspace skeleton (M0): `berth hello`, `--help`, `--version`, and every CI gate.

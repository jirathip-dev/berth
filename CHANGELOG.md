# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `berth-core` terminals (M1a): programs on real PTYs, a VT screen model with bounded scrollback,
  named keys, and process-tree kill (SIGTERM, a grace period, SIGKILL) with per-tree RSS/CPU.
- The workspace skeleton (M0): `berth hello`, `--help`, `--version`, and every CI gate.

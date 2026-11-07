Closes #35

## What changed

- **`machine.stats`** (JSON-RPC, `shed-proto`): `StatsParams { since }` → `MachineStats`, a `Sample` taken now (`at`, `load_per_cpu`, `mem_pressure_pct`, `swap_used_mb`, `disk_free_mb` on the state file's volume, `cpus`, `uptime_s`, `terminals` by state: reported state while running, else `running`; `exited`) plus `history` (kept samples after `since`). `docs/protocol.md` regenerated.
- **Sampling** (`shed-server/src/stats.rs`): Linux reads `/proc/{loadavg,meminfo,uptime}`, macOS one `sysctl -n vm.loadavg kern.memorystatus_level vm.swapusage kern.boottime`, both `df -Pk` — no new deps. The server records a sample at start and then whenever one is due (checked every 10 s, due once the last is ≥ 60 s old), drops samples older than 24 h (= 1,440), and saves them atomically to `stats.json` beside `state.json` so history survives a restart. The request path samples outside the registry lock.
- **CLI**: `shed machine stats [--since T] [--json]`, and with `--on M` over the existing SSH path. Remote calls run under a hard 5 s deadline (`machines::STATS_TIMEOUT`) regardless of how ssh hangs; no answer → `{"machine":M,"online":false,"last_seen":<unix s|null>}` / `M offline, last seen <t>`, exit 1. The last successful answer time is kept in `seen/<machine>` beside the state file. A version mismatch is still the `SHED_VERSION` error, not "offline".
- Cap raised 5,400 → 5,750 with `docs/cap-history/2026-11-06-35.md` (AC 1 allows it); README/CONTRIBUTING cap numbers, README command table and CHANGELOG updated.

Decisions: memory pressure = percent of memory not available (Linux `MemAvailable/MemTotal`, macOS `kern.memorystatus_level`); `last_seen` and `at` are unix seconds; offline exits 1 so scripts can branch on it while the JSON is still printed.

## For humans

`shed machine stats` shows a machine's load, memory, swap, free disk, uptime and how many terminals are in each state, here or on another machine with `--on`.
The server keeps a sample every minute for the last day, so a dashboard can draw a 24-hour history.
A machine that cannot be reached is reported offline, with when it was last seen, within 5 seconds instead of hanging.

## Tests

At head `e0adeac612b3f1ccd1faf5f8d3c62a69452164e6` (merge of `origin/staging` 74b83a1; conflict in `crates/shed-server/src/machines.rs` imports only, both kept; macOS, `CARGO_TARGET_DIR` = lane scratch target; `just` and `cargo-deny` are not installed on this host, so each recipe's commands were run directly):

| Command | Exit |
|---|---|
| `cargo fmt --all --check` | 0 |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | 0 |
| `test "$(grep -rE 'allow\(' crates \| wc -l)" -le 10` | 0 |
| `cargo test --workspace --locked` | 0 |
| `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked` | 0 |
| `scripts/line-cap.sh` (5749 / 5750) | 0 |
| `scripts/dep-direction.sh` | 0 |
| `scripts/hygiene.sh` | 0 |

New tests (fail without this change):
- `shed-server` `stats::tests::one_sample_a_minute_on_a_fake_clock_and_24_hours_kept`: a fake clock steps 10 s for 25 h through the real `Stats::due`/`push`; asserts exactly 1,440 kept, 60 s apart, the oldest exactly 24 h before the newest, `since` slicing, and the saved file loads back equal.
- `shed-server` `stats::tests::parses_macos_sysctl`: the sysctl parser on a fixture and on empty output.
- `shed` `tests/machines.rs::machine_stats_here_and_on_a_machine_and_an_offline_one_answers_within_5_s`: real servers; stats shape and value ranges locally (terminal counts `{"working":1,"exited":1}`, the start-up sample in `history`, no `history` without `--since`), the same shape via `--on` through the fake ssh, then the fake ssh made to hang (`sleep 30`): the CLI answers `online:false` with the `last_seen` of the earlier success, exit 1, in under 5.5 s; and the text form.

## Line count

`scripts/line-cap.sh`: 5708 at this branch's own commit, 5749 / 5750 after merging staging (#36 added 41). `src/ net +361` (no budget stated in the issue): a new method across proto, server, CLI, plus the per-OS system probe and the minute history with persistence.

## Not verified

- Linux: the `/proc` probe was not run (no Linux host or target here); it was type-checked and clippy-clean by temporarily swapping the `cfg`s on macOS. CI runs the tests on Linux.
- `cargo deny` (not installed here; no dependency changes).

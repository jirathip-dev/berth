# Running berth serve as a user job

`berth serve` is the per-user server: it owns the terminals, the state file and the socket. Run it as a
user job so it starts at login and comes back after a crash. berth does not install these files yet (a
later milestone will); copy them by hand.

| | Socket (mode 0600) | State file (mode 0600) |
|---|---|---|
| macOS | `~/Library/Caches/berth/berth.sock` | `~/Library/Application Support/berth/state.json` |
| Linux | `$XDG_RUNTIME_DIR/berth.sock` | `$XDG_STATE_HOME/berth/state.json` (default `~/.local/state`) |

`berth serve --dir DIR` puts both in `DIR` instead. Only one server answers per socket; a second one
exits with an error.

## macOS (launchd)

[`dist/launchd/berth.plist`](../dist/launchd/berth.plist): set the binary's absolute path in
`ProgramArguments` (launchd does not expand `~`), then:

```sh
cp dist/launchd/berth.plist ~/Library/LaunchAgents/berth.serve.plist
launchctl bootstrap "gui/$(id -u)" ~/Library/LaunchAgents/berth.serve.plist
launchctl bootout "gui/$(id -u)/berth.serve"   # stop and unload
```

## Linux (systemd)

[`dist/systemd/berth.service`](../dist/systemd/berth.service): set `ExecStart` to your binary, then:

```sh
mkdir -p ~/.config/systemd/user
cp dist/systemd/berth.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now berth.service
loginctl enable-linger "$USER"   # optional: keep it running while logged out
```

## Restarts

The state file keeps every terminal's name, labels, cwd, command, state and exit status, written
atomically (temp file, then rename). When the server starts again, terminals run with `keep` come back
as a shell (`$SHELL`, else `/bin/sh`) in the cwd they were started in; their task never runs again.
The others are listed as `exited` with no exit status.

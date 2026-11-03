# Architecture

berth is one binary built from four crates. The core holds all the logic and knows nothing about sockets
or the CLI; the server puts it behind a socket; the binary only parses arguments and wires the rest.

```
             ┌──────────────┐
             │    berth     │  the CLI binary: thin client + `berth serve`
             └──────┬───────┘
                    │
             ┌──────▼───────┐
             │ berth-server │  socket, JSON-RPC, events stream, machines over SSH
             └───┬──────┬───┘
                 │      │
     ┌───────────▼┐    ┌▼─────────────┐
     │ berth-core │    │ berth-proto  │  API types + JSON-RPC method list
     └────────────┘    └──────────────┘
  terminals, screen model, state machine,
  persistence, process trees; no sockets
```

Arrows point at dependencies. `berth-core` depends on no other workspace crate; CI checks it
(`just deps`). See [decisions/0001-workspace-layout.md](decisions/0001-workspace-layout.md). ACP agents run
through a bridge inside an ordinary terminal ([decisions/0003-acp-bridge.md](decisions/0003-acp-bridge.md)).

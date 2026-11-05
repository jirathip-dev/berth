# Architecture

shed is one binary built from four crates. The core holds all the logic and knows nothing about sockets
or the CLI; the server puts it behind a socket; the binary only parses arguments and wires the rest.

```
             ┌──────────────┐
             │    shed     │  the CLI binary: thin client + `shed serve`
             └──────┬───────┘
                    │
             ┌──────▼───────┐
             │ shed-server │  socket, JSON-RPC, events stream, machines over SSH
             └───┬──────┬───┘
                 │      │
     ┌───────────▼┐    ┌▼─────────────┐
     │ shed-core │    │ shed-proto  │  API types + JSON-RPC method list
     └────────────┘    └──────────────┘
  terminals, screen model, state machine,
  persistence, process trees; no sockets
```

Arrows point at dependencies. `shed-core` depends on no other workspace crate; CI checks it
(`just deps`). See [decisions/0001-workspace-layout.md](decisions/0001-workspace-layout.md). ACP agents run
through a bridge inside an ordinary terminal ([decisions/0003-acp-bridge.md](decisions/0003-acp-bridge.md)).

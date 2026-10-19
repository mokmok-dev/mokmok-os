# mokmok web console

This README covers the JavaScript workspace. It lives at the repository root, next to the Rust
crates, and the npm packages all sit under `packages/`.

A browser console for the mokmok event bus. One tab is one subscriber and one publisher on a single
topic. It shows the live event stream, the bus checkpoint, and a composer that appends events.

The bus serves WebSocket on a Unix socket, which a browser cannot dial, so the workspace ships a
small Node process that owns the bus connections and forwards them to the browser over a local
WebSocket.

```
Rust bus (UDS WebSocket)
   ▲ subscriber connection (persisted SubscriberId)
   ▲ publisher connection
   │
@mokmok/web-bridge (Node, Effect v4) ── one browser WebSocket ── @mokmok/web-console
                                                                  React + Kumo + TanStack
```

## Quick start

```bash
pnpm install
pnpm dev
```

`pnpm dev` starts the bridge on `http://127.0.0.1:8787` and the Vite dev server on
`http://127.0.0.1:5173`. Vite proxies `/api` (including the WebSocket upgrade) to the bridge.

For the production shape:

```bash
pnpm build
pnpm start
```

`pnpm start` serves `packages/console/dist` and the API from one origin.

The bridge needs a running mokmok bus. Start one from the repository root:

```bash
mkdir -p /tmp/mokmok-web
OLLAMA_API_KEY=unused target/debug/mokmokd --data-dir /tmp/mokmok-web/data \
  serve --socket /tmp/mokmok-web/bus.sock
```

## Configuration

| Variable | Default | Meaning |
| --- | --- | --- |
| `MOKMOK_WEB_SOCKET` | `$XDG_RUNTIME_DIR/mokmok/bus.sock`, else `/tmp/mokmok/bus.sock` | bus Unix socket |
| `MOKMOK_WEB_TOPIC` | `global` | the one topic the console reads and publishes |
| `MOKMOK_WEB_DATA_DIR` | `.mokmok-web` | holds the persisted subscriber id |
| `MOKMOK_WEB_HOST` | `127.0.0.1` | listen address |
| `MOKMOK_WEB_PORT` | `8787` | listen port |
| `MOKMOK_WEB_STATIC_DIR` | `packages/console/dist` | built console assets |

## Packages

| Package | Role |
| --- | --- |
| `@mokmok/web-protocol` | Effect Schema codecs for the bus and bridge frames, plus the pure readers |
| `@mokmok/web-consumer` | the subscriber state machine, local checkpoint, payload validation, presentation helpers |
| `@mokmok/web-bridge` | the UDS WebSocket client, reconnect loops, browser WebSocket, health endpoint, static serving |
| `@mokmok/web-console` | the React UI |

`DESIGN.md` carries the wire protocol, the bridge protocol, and the exact consumer API.

## Delivery and acknowledgement

The bus delivers at least once. The bridge pulls one batch, forwards it to the browser, and waits
for the browser to persist the events and acknowledge the batch. Only then does the bridge advance
the bus checkpoint. If the browser dies, the bus replays the batch on the next connection; the
consumer suppresses the duplicate by sequence. This is why the events are written to localStorage
before the ack is sent.

## Verification

```bash
pnpm typecheck     # every tsconfig
pnpm test          # protocol, consumer, bridge, console suites
pnpm build         # Vite production build
pnpm mutants       # Stryker over the domain packages
pnpm mutants:gate  # fails unless every mutant was killed
pnpm e2e           # bridge against a real mokmokd bus (skips if the binary is absent)
pnpm verify        # the whole chain
```

Mutation testing runs over `packages/protocol/src` and `packages/consumer/src`, the two packages
whose behaviour is pure and deterministic. The bridge and the console are adapters; they are
verified by integration tests against a fake bus, by the real-bus end-to-end test, and by the
production build. The Stryker config sets `break: 100`, and `scripts/mutation-gate.mjs` fails with
the surviving mutant listed, so a merged change cannot lower the score silently. The current gate
covers 376 mutants, all killed.

## Limits

- One browser client at a time; the second connection is closed by the bridge.
- One topic per bridge instance, selected at startup.
- No authentication; the listener binds loopback by default.
- The local event list is capped in memory and in localStorage. The bus log is the durable record.

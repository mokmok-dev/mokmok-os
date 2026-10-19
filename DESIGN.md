# The web console

A pnpm workspace at the repository root, with every npm package under `packages/`, that renders the
mokmok event bus as a live console. One browser tab is one
subscriber and one publisher. The bus speaks WebSocket over a Unix socket, and a browser cannot
open a Unix socket, so a small Node process owns the bus connection and the browser owns the
presentation.

```
Rust bus (UDS WebSocket, JSON frames)
   ▲ one subscriber connection, resumed by SubscriberId        ▲ one publisher connection
   │                                                            │
   └────────────── @mokmok/web-bridge (Node, Effect v4) ────────┘
                         │ one browser WebSocket (/api/stream), one client at a time
                         ▼
                 @mokmok/web-console (React, Kumo, TanStack)
```

## Packages

| Package | Responsibility | Mutation tested |
| --- | --- | --- |
| `@mokmok/web-protocol` | Effect Schema codecs for the bus frames and the bridge frames, plus the small pure helpers that read them | yes |
| `@mokmok/web-consumer` | The subscriber state machine: batch application, duplicate suppression, ack sequencing, local checkpoint hydration, publish form validation, presentation helpers | yes |
| `@mokmok/web-bridge` | The UDS WebSocket client, the reconnect loops, the browser WebSocket server, the health endpoint, static file serving | no, integration tested |
| `@mokmok/web-console` | The React UI: routes, the live event table, the publish composer, connection status | no, rendered tests |

Mutation testing covers the two packages whose behaviour is pure and deterministic. The bridge and
the console are adapters verified against a fake bus, the real bus, and the built bundle.

## Bus wire protocol (observed from the Rust bus)

Requests are one JSON object per WebSocket text frame. The first frame on a connection is `open`.
All requests and responses use `op` as the discriminant.

| Request | Shape |
| --- | --- |
| open | `{"op":"open","topic":"global","subscriber":null\|"<uuid>"}` |
| publish | `{"op":"publish","payload":<any JSON>}` |
| pull | `{"op":"pull","max":<positive int>}` |
| ack | `{"op":"ack","through":{"epoch":"<uuid>","seq":<int>}}` |

| Response | Shape |
| --- | --- |
| opened | `{"op":"opened","window":{"epoch":"<uuid>","first":<int>,"next":<int>}}` |
| published | `{"op":"published","id":"<uuid>","seq":<int>,"at":{"secs_since_epoch":<int>,"nanos_since_epoch":<int>}}` |
| batch | `{"op":"batch","window":<window>,"lapsed":null\|{"held":<cursor>,"from":<int>},"events":[<event>...]}` |
| acked | `{"op":"acked","through":<cursor>}` |
| refused | `{"op":"refused","reason":"topic_limit"\|"connection_limit"\|"shutting_down"\|{"unresumable":{"window":<window>}}}` |

An event is `{"id":"<uuid>","topic":"<topic>","seq":<int>,"at":<instant>,"payload":<any JSON>}`.
A topic is `global`, `agent:<uuid>`, `session:<uuid>`, or `execution:<uuid>`.

The bus delivers at least once: a batch is replayed until its position is acknowledged. A batch's
position after the batch is the last event's `seq + 1`, or `lapsed.from` when the batch is a lapse,
or `window.next` when it is empty.

## Bridge protocol (browser WebSocket at `/api/stream`)

Server to browser:

| Frame | Shape |
| --- | --- |
| hello | `{"op":"hello","topic":<topic>,"subscriber":"<uuid>"}` |
| status | `{"op":"status","connection":"connecting"\|"live"\|"refused"\|"offline","detail":null\|"<text>"}` |
| batch | `{"op":"batch","batch_id":"<id>","window":<window>,"lapsed":null\|<lapsed>,"events":[<event>...]}` |
| published | `{"op":"published","request_id":"<id>","id":"<uuid>","seq":<int>,"at":<instant>}` |
| publish_failed | `{"op":"publish_failed","request_id":"<id>","detail":"<text>","indeterminate":<bool>}` |
| acked | `{"op":"acked","through":<cursor>}` |

Browser to server:

| Frame | Shape |
| --- | --- |
| ack | `{"op":"ack","batch_id":"<id>","through":<cursor>}` |
| publish | `{"op":"publish","request_id":"<id>","payload":<any JSON>}` |

The bridge sends one batch at a time and does not pull the next one until the browser's ack for it
arrives. The browser persists the batch to `localStorage` before acking. Only then does the bridge
ack the bus. A browser crash therefore leaves the bus checkpoint behind the persisted copy and the
batch is replayed; the consumer state machine suppresses the duplicate by sequence.

`GET /api/health` returns the `HealthResponse` schema from the protocol package:
`status`, `topic`, `subscriber`, `socket`, `events_delivered`, `last_seq`, `pending_batch`,
`connected_at_ms`.

## Consumer state machine (`@mokmok/web-consumer`)

The state is a value; every message produces a new value.

```ts
interface ConsumerState {
  topic: string;
  phase: "connecting" | "live" | "refused" | "offline";
  detail: string | null;
  subscriber: string | null;
  window: Window | null;
  checkpoint: Cursor | null;      // furthest position acknowledged by the bridge
  pending: { batchId: string; through: Cursor } | null;
  events: readonly Event[];       // oldest first, capped at EVENT_CAP
  lapsed: Lapsed | null;
  receipts: readonly Receipt[];   // newest first, capped
  failures: readonly PublishFailure[];
  evicted: number;
}
```

`initialState(topic)` opens empty. `reduce(state, message)` handles `hello`, `status`, `batch`,
`acked`, `published`, `publish_failed`, `clear_events`.

Rules that tests and properties pin:

- The initial phase is `connecting`; `hello` sets only the topic and the subscriber, a `status`
  frame owns the phase.
- A batch whose epoch differs from the state's resets the event list, the window, the lapse, and the
  eviction count.
- An event is applied only when its sequence is at least one past the last applied event of the same
  epoch. Gaps are accepted (the bus log is authoritative); replays are suppressed. Replaying a batch
  is idempotent.
- A same-epoch batch stores its own lapse, so a normal batch clears an earlier lapse.
- `pending` is set to the batch just delivered and cleared only by an `acked` frame with the same
  cursor. A late or foreign ack changes nothing, including the checkpoint. A matching ack replaces
  the checkpoint when the epoch changed and takes the maximum within an epoch.
- `checkpoint` never moves backwards within an epoch.
- The event list never exceeds `EVENT_CAP`; dropped events increment `evicted`. Receipts and failures
  are capped at 100 each, newest first.
- `hydrate(raw, topic)` restores `subscriber`, `checkpoint`, `events`, and `evicted` from the stored
  document and returns `initialState(topic)` for missing, malformed, other-topic, or wrong-version
  documents. Window, lapse, pending batch, receipts, and failures are session state and reset.
- `persistable(state)` is the inverse document for `hydrate`.

Helpers the UI imports: `batchThrough(frame)`, `parsePayload(text)` (any JSON value after trimming,
at most `MAX_PAYLOAD_BYTES` UTF-8 bytes; `1e999` and other non-JSON numbers are rejected),
`payloadPreview(payload, limit)` (compact JSON, default limit 80, an ellipsis when truncated),
`payloadBytes(payload)` (compact UTF-8 JSON length), `formatBytes(bytes)` (binary units, one
decimal), `relativeTime(nowMs, thenMs)` (coarse seconds, minutes, hours, days).

## Bridge configuration

| Variable | Default | Meaning |
| --- | --- | --- |
| `MOKMOK_WEB_SOCKET` | `$XDG_RUNTIME_DIR/mokmok/bus.sock` or `/tmp/mokmok/bus.sock` | bus socket |
| `MOKMOK_WEB_TOPIC` | `global` | the one topic this console reads and writes |
| `MOKMOK_WEB_DATA_DIR` | `.mokmok-web` | directory holding the persisted subscriber id |
| `MOKMOK_WEB_HOST` | `127.0.0.1` | listen address |
| `MOKMOK_WEB_PORT` | `8787` | listen port |
| `MOKMOK_WEB_STATIC_DIR` | `packages/console/dist` | built console assets |

The subscriber id is a file `subscriber-id` in the data directory. Writing it once is what makes a
bridge restart resume the same bus checkpoint. A second browser connection is closed with code
`4009`; the subscription only exists while the first browser is connected, so `/api/health` reports
`offline` before the first connection and after the last one.

## Verification

- `pnpm typecheck` runs every tsconfig.
- `pnpm test` runs the package suites.
- `pnpm build` builds the console with Vite.
- `pnpm mutants` runs Stryker over the protocol and consumer sources; `pnpm mutants:gate` fails
  unless every mutant was killed. Stryker runs from the repository root, so its `ignorePatterns`
  keep the Rust crates, `.direnv`, and Nix results out of the sandbox copy.
- `pnpm e2e` runs the bridge against a real `mokmokd` bus binary and a browser-protocol client.
- `pnpm verify` is the whole chain.

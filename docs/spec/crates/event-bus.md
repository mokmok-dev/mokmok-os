---
type: Crate
title: mokmok-event-bus
description: A WebSocket server on a Unix socket that commits every published event to its own SQLite log before a reader sees it, and streams retained events per topic from durable checkpoints.
resource: ../../../mokmok-event-bus
tags: [mokmok-os, crate, delivery]
status: draft
generated: { by: grok-build/grok-4.7, at: 2026-10-15T02:00:00Z }
stale_after: 2026-11-15T00:00:00Z
sources:
  - id: architecture
    resource: ../../../mokmok-os-architecture.md
    title: mokmok OS architecture and crate responsibilities, section 5.3
    author: codex/gpt-6
  - id: crate
    resource: ../../../mokmok-event-bus
    title: mokmok-event-bus sources
    last_modified: 2026-10-12T16:24:11Z
---

# Responsibility

Manages subscription lifecycle and delivery from committed history, over a WebSocket served on a
Unix socket. Every published event reaches durable storage before a reader can see it, and each
subscriber resumes from a checkpoint the bus owns.[^crate]

# Public surface

Modules `bus`, `client`, `config`, `event`, `payload`, `server`, `store`, `subscriber`, `topic`, and
`wire`.

| Item | Where | What it is |
| --- | --- | --- |
| `Publisher` | `src/client.rs:36` | `open`, `publish` |
| `Subscriber` | `src/client.rs:40` | `open`, `next`, `ack` |
| `serve`, `ServeError`, `signals` | `src/server.rs:50,34,79` | The server, its failure, and its shutdown signals |
| `Config`, `Limits`, `Retention`, `Timeouts` | `src/config.rs:7,19,31,38` | Paths, ceilings, retention, and deadlines |
| `Cursor`, `Window`, `Lapsed`, `Refusal`, `Event`, `Published`, `Batch` | `src/event.rs:42,68,100,132,159,211,243` | The delivery vocabulary |
| `Seq`, `LogEpoch` | `src/event.rs:13,29` | Within-topic order and the retention generation |
| `SubscriberId`, `Topic`, `Payload` | `src/subscriber.rs:12`, `src/topic.rs:7`, `src/payload.rs:3` | The identity, the scope, and the bytes |

# Owned state

Its own SQLite file, named by `Config::store` and kept away from the socket because the runtime
directory is a tmpfs (`src/config.rs:9-11`). The schema at `src/store.rs:24-49` creates `topic`
(name, epoch, first, next, bytes, last_append), `event` (topic, seq, id, at_secs, at_nanos,
payload, keyed by topic and seq), and `checkpoint` (subscriber, topic, epoch, acked_seq).

One connection on one operating-system thread behind a bounded channel of 64
(`src/store.rs:22,80`). In memory: a read-write lock over the topic map
(`src/bus.rs:21`), a connection count (`src/bus.rs:22`), a shutdown watch channel
(`src/bus.rs:23`), and one `watch::Sender<Window>` per topic (`src/bus.rs:29`).

# Invariants

- Uncommitted events are never delivered. `TopicEntry::append` (`src/bus.rs:279`) awaits the store
  commit and calls `send_replace` afterwards.
- A missed notification cannot lose an event. The session snapshots the watch with
  `borrow_and_update` before it reads the log (`src/server.rs:309`), and every pull re-reads the
  store (`src/server.rs:613`).
- A pull with no credit left is not woken by a commit. `Parked::Spent` (`src/server.rs:607`) pairs
  with the `select!` guard at `src/server.rs:278`.
- A checkpoint never moves backward and never passes what was delivered. The advance is clamped to
  the acknowledged and delivered range (`src/server.rs:682`) and to the stored maximum
  (`src/store.rs:616`).
- An acknowledgement naming a foreign epoch changes nothing (`src/store.rs:609`), and a subscriber
  whose checkpoint names another epoch is refused at open with `Unresumable` (`src/server.rs:504`).
- Slow subscribers are bounded and isolated. Each session has an in-flight credit
  (`src/server.rs:568`), a write that does not drain inside the deadline closes the socket
  (`src/server.rs:724`), and an unanswered ping closes with code 1008 (`src/server.rs:386`).
- Connections and topics are default-deny at their ceilings (`src/bus.rs:221,102`).
- A reclaim never drops events a live reader has not pulled. `sweep` rechecks the receiver count
  under the lock (`src/bus.rs:187`).
- A reclaim keeps the epoch (`src/store.rs:1144`), and eviction keeps the newest event whatever its
  size.

# Dependencies

`axum` with `ws`, `futures-util`, `mokmok-protocol`, `rusqlite` with `bundled`, `serde`,
`serde_json` with `raw_value`, `thiserror`, `tokio`, `tokio-tungstenite`, `tracing`, and `uuid`.
Development adds `proptest` and `tracing-subscriber`. The bus does not depend on
`mokmok-event-store`.

# Reconciliation

The design has the bus read committed events from the store and lists `mokmok-event-store` among its
dependencies.[^architecture] The bus opens a second SQLite file with its own schema
(`src/store.rs:24-49`), imports only `EventID` from the protocol crate, and never handles a
`CommitPosition`.

The design describes a `watch<CommitPosition>` announcing durable progress. The channel carries a
per-topic `Window` (`src/bus.rs:246`), and a second watch channel carries shutdown.

The design asks for a rule covering positions with no matching event for a filtered subscription.
There is no filtering. A subscription's only scope is its topic (`src/wire.rs:15`), and every
retained event is handed over.

The design asks for a reconciliation mechanism covering the crash window between the database commit
and the notification. There is no startup or periodic reconciliation. The window is closed on the
read path instead, by reading the log before parking with the watch snapshotted first and by
re-reading the store on every pull.

The design's `publisher`, `dispatcher`, `notification`, `websocket`, `connection`, and
`backpressure` modules do not exist. Publishing lives in `src/client.rs`, and there is no dispatcher
or notification type.

A new subscriber identity starts at the retained floor without an error
(`src/server.rs:497`). Retention bounds bytes and idle topics in addition to event count
(`src/config.rs:31`), and the bus refuses a topic and a connection in addition to a subscription
(`src/event.rs:132`).

# Tests

Eight `#[cfg(test)]` modules hold 57 cases, of which 24 are `#[tokio::test]` and 6 are proptest
cases. `tests/over_socket.rs` adds 25 cases over a real socket: publish before subscribe, two
subscribers each seeing every event, an event surviving a restart, reconnect replaying unacknowledged
events with the same identifier, a slow consumer receiving at most its in-flight credit, a
checkpoint older than retention lapsing, a previous-epoch subscriber refused at open, the connection
ceiling, a parked subscriber freeing its connection, a bad frame closing the session, an oversized
frame refused without stopping the bus, and a peer that stops reading being closed.

```
cargo test -p mokmok-event-bus
cargo run --example loopback -p mokmok-event-bus
```

[^architecture]: Section 5.3 of `mokmok-os-architecture.md`.
[^crate]: The re-export list is `mokmok-event-bus/src/lib.rs:57-62`.

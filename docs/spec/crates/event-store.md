---
type: Crate
title: mokmok-event-store
description: One SQLite file holding a global ordered commit log, per-stream versions, consumer projections, inbox records, checkpoints, and an outbox.
resource: ../../../mokmok-event-store
tags: [mokmok-os, crate, durability]
status: draft
generated: { by: grok-build/grok-4.7, at: 2026-10-15T02:00:00Z }
stale_after: 2026-11-15T00:00:00Z
sources:
  - id: architecture
    resource: ../../../mokmok-os-architecture.md
    title: mokmok OS architecture and crate responsibilities, section 5.2
    author: codex/gpt-6
  - id: crate
    resource: ../../../mokmok-event-store
    title: mokmok-event-store sources
    last_modified: 2026-10-12T00:39:10Z
---

# Responsibility

Guarantees durable transaction boundaries and one authoritative event order over a single SQLite
file. An append and a consume are each one transaction, and both can carry the caller's own
state.[^crate]

# Public surface

Modules `append`, `error`, `event`, `model`, `payload`, `store`, and `write`.

| Item | Where | What it is |
| --- | --- | --- |
| `Store` | `src/store.rs:80` | `open`, `append`, `consume`, `history`, `log`, `checkpoint`, `pending_outbox`, `settle_outbox` |
| `Append`, `Committed`, `Outcome` | `src/append.rs:12,171,208` | The append program and its two answers |
| `OutboxMessage`, `OutboxId`, `OutboxEntry` | `src/append.rs:81,111,126` | A message written in the same transaction as the events |
| `WriteTx` | `src/write.rs:15` | `recorded`, `append`, `history`, `projection`, `put_projection` inside a consume |
| `StreamId`, `ConsumerId`, `CommandId`, `Version`, `CommitPosition`, `Expected` | `src/model.rs:14,61,108,161,187,222` | The identifiers and the concurrency contract |
| `NewEvent`, `Event`, `Slice`, `Kind` | `src/event.rs:58,75,144,14` | Stored events and a read window |
| `Payload` | `src/payload.rs:20` | Raw JSON that survives the round trip |

# Owned state

One `rusqlite::Connection` (`src/store.rs:81`) over one file named by the caller. The schema at
`src/store.rs:17-67` creates seven tables: `commit_log` (position, stream, version, count, command
unique, at_millis), `event` (position, ordinal, stream, version, id unique, kind, payload, with
`unique(stream, version)`), `stream` (id, version, state), `projection` keyed by consumer and key,
`outbox` (autoincrement id, position, topic, payload, at_millis), `inbox` keyed by consumer and
position, and `checkpoint` keyed by consumer.

# Invariants

- An append batch commits atomically. `BEGIN IMMEDIATE` (`src/store.rs:294`), one `COMMIT`
  (`src/store.rs:128`), and a savepoint around the append program that rolls back on error
  (`src/store.rs:349`).
- A repeated command identity commits nothing new. `expected` and `recorded_in`
  (`src/store.rs:403`, `src/store.rs:535`) are the version check and the dedup read that decide it,
  and `commit_log.command` is unique (`src/store.rs:23`). The answer is `Outcome::Replayed` with the
  original rows.
- An expected version is enforced. `Expected::accepts` (`src/model.rs:233`) is checked at
  `src/store.rs:386`, and a mismatch answers `Outcome::Conflicted` with nothing written.
- Versions are unique and contiguous per stream, through `unique(stream, version)`
  (`src/store.rs:35`) and numbering at `src/store.rs:446`.
- Visible commit positions agree with the commit order. The position is the `commit_log` rowid
  (`src/store.rs:19`) and `CommitPosition` is a nonzero integer (`src/model.rs:187`).
- A checkpoint never moves backward. `advance_checkpoint` (`src/store.rs:753`) uses the SQL `max`
  function, and its only caller is `consume` (`src/store.rs:161`).
- A checkpoint cannot name processing that did not commit. `consume` (`src/store.rs:152-163`)
  interleaves the inbox check, the caller's writes, the inbox row, and the checkpoint inside one
  transaction, and `live` (`src/store.rs:355`) fails when SQLite has already ended it.
- An unknown schema is refused. `prepare` (`src/store.rs:317`) accepts `user_version` 0 or 1 and
  stamps 1.

# Dependencies

`mokmok-protocol`, `rusqlite` with `bundled`, `serde`, `serde_json` with `raw_value`, `thiserror`,
and `uuid`. Development: `proptest`. No tokio and no axum, which is what the design asks of the
storage layer. The crate ships one binary, `mokmok-event-store-crash`
(`src/bin/mokmok-event-store-crash.rs`), used by its own crash tests.

# Reconciliation

The design names the modules `transaction`, `reader`, `checkpoint`, `inbox`, `outbox`, `migration`,
and `sqlite`.[^architecture] The crate names them `append`, `error`, `event`, `model`, `payload`,
`store`, and `write`. The transaction handle is `WriteTx` in `src/write.rs`. There is no `migration`
module anywhere in the workspace.

The design places the shared transaction machinery in this crate and the domain repositories near
their owners, joined by an adapter layer at the infrastructure boundary.[^architecture] There is no
adapter layer. The store is the only persistence layer in the workspace, and the scheduler, the
capability registry, and the artifact metadata each hold their state in memory or in files of their
own.

Two behaviours the design does not describe. The store adopts a version-0 SQLite file, stamps it
with version 1, and refuses anything above version 1 with `Error::Schema`.
And `consume` documents (`src/store.rs:162`) that a `Committed` value from a different file
whose position collides is accepted, after which the inbox and the checkpoint name an unrelated
commit.

# Tests

30 unit cases across six `#[cfg(test)]` modules, and 14 cases in `tests/`. The largest module is
`src/store.rs:905` with 16 cases: append, replay, a stale expectation, multi-event deduplication,
`history` resuming at a commit boundary, log order, duplicate consume, checkpoint movement,
outbox settling, the refusal of an unknown schema, and the `user_version` stamp.
`src/write.rs:95` holds 4 cases for `WriteTx` inside a consume transaction.

`tests/properties.rs:403` runs one proptest case against an in-file model of versions, commits, and
checkpoints. `tests/transactions.rs` runs 8 cases on a real file, including two stores racing one
expected version and a rollback trigger inside the closure. `tests/crash.rs` runs 5 cases that
`SIGKILL` the helper binary at named points.

```
cargo test -p mokmok-event-store
MUTANTS_PACKAGE=mokmok-event-store ./scripts/mutants.sh --jobs 8
```

[^architecture]: Section 5.2 of `mokmok-os-architecture.md`.
[^crate]: The re-export list is `mokmok-event-store/src/lib.rs:60-65`.

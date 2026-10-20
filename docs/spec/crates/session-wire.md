---
type: Crate
title: mokmok-session-wire
description: The command, reply, commit, history, roster, and progress payloads a terminal and a daemon exchange over the event bus, with the topic and subscriber identities that carry them.
resource: ../../../mokmok-session-wire
tags: [mokmok-os, crate, contract]
status: draft
generated: { by: grok-build/grok-4.7, at: 2026-10-19T18:00:00Z }
stale_after: 2026-11-19T00:00:00Z
sources:
  - id: architecture
    resource: ../../../mokmok-os-architecture.md
    title: mokmok OS architecture and crate responsibilities, sections 4 and 5.3
    author: codex/gpt-6
  - id: crate
    resource: ../../../mokmok-session-wire
    title: mokmok-session-wire sources
    last_modified: 2026-10-19T18:00:00Z
---

# Responsibility

Owns the vocabulary a terminal and a daemon exchange over the event bus. The bus carries opaque
JSON, so every idempotence and routing rule lives in these payloads. One `SessionCommit` is one
ledger transaction: the records it carries are exactly the records the fold replays, which is what
keeps a consumer's fold equal to the daemon's transcript.

# Public surface

| Item | Where | What it is |
| --- | --- | --- |
| `PROTOCOL` | `src/lib.rs:27` | The wire version both ends name |
| `Command` | `src/lib.rs:35` | A client's intent: `Hello`, `List`, `Create`, `Open`, `Speak`, `History` |
| `Reply`, `Outcome` | `src/lib.rs:85,99` | The daemon's `Welcome` or `Ack`, and an accepted session or a refusal |
| `Refusal` | `src/lib.rs:108` | The domain refusals the daemon decides at its boundary |
| `SessionEvent` | `src/lib.rs:134` | A commit, a history chunk, a history refusal, a seed, or a trouble phrase |
| `SessionCommit`, `Notice` | `src/lib.rs:161,187` | One ledger transaction and its routing header |
| `SessionCommit::outbox` | `src/lib.rs:177` | The outbox row the relay publishes after the transaction commits |
| `HistoryChunk` | `src/lib.rs:196` | One replacement chunk, its request identity, and the seed a created session has |
| `DaemonEvent`, `RosterChunk`, `RosterEntry` | `src/lib.rs:209,220,230` | Roster revisions, streaming prose, and one session as a listing shows it |
| `speech_step` | `src/lib.rs:240` | The step id a spoken request derives |
| `command_topic`, `session_topic`, `daemon_topic` | `src/topic.rs:11,17,23` | Where each payload travels |
| `daemon_subscriber`, `session_subscriber`, `daemon_view_subscriber` | `src/topic.rs:33,41,50` | The checkpoints a daemon and a terminal resume |
| `HISTORY_BYTES`, `ROSTER_BYTES` | `src/chunk.rs:11,14` | The ceiling one replacement chunk stays under |
| `ChunkError`, `history_chunks`, `roster_chunks` | `src/chunk.rs:22,38,82` | How a snapshot becomes ordered, bounded payloads |

# Owned state

The crate owns no process state and no durable file. It owns the shape of the values that cross the
bus, the topic grammar they travel on, and the rule that derives a subscriber identity from the data
directory or a process nonce. The bus crate owns the socket and the checkpoint; the event store owns
the outbox row a `SessionCommit` becomes.

# Invariants

- One `SessionCommit` is one ledger transaction. Its records are the `Record` values the fold
  replays, in log order, so a consumer never reconstructs a summary that can drift (`src/lib.rs:161`).
- A `Command` names a `request` identity. `Speak` derives its ledger step from that identity, so a
  redelivered command replays its own write instead of opening the next turn (`src/lib.rs:240`).
- A commit's outbox row is written inside the transaction that wrote the commit, so it exists only
  after its history committed (`src/lib.rs:177`).
- A refusal is a domain value, not a transport error. It names a session, a checkout, a phase, or
  the daemon's own inability to build a task, and it never names a grant (`src/lib.rs:108`).
- A history replacement is ordered and bounded. Chunks carry a request identity, a zero-based index,
  and a last flag; the seed rides the last chunk. One record larger than the ceiling goes alone, and
  a record larger than a bus frame is refused (`src/chunk.rs:38`).
- The daemon's command checkpoint is derived from the data directory, so a restart resumes the
  commands it had not handled. A terminal's session checkpoint is derived from a process nonce and
  the session, so a reconnect resumes and a restart re-bootstraps (`src/topic.rs:33,41`).
- The wire version is explicit. A daemon answers a newer client with `Refusal::Unsupported` rather
  than guessing at a shape it does not know (`src/lib.rs:27`).

# Dependencies

`mokmok-agent-runtime` for `Record`, `StepId`, `StepKey`, and `TaskRecord`; `mokmok-event-bus` for
`Topic` and `SubscriberId`; `mokmok-event-store` for `OutboxMessage` and `Payload`; `mokmok-protocol`
for the identifiers and `Moment`; `mokmok-state-machine` for `Phase`, `Ending`, and `Resolution`;
`serde` and `serde_json`; `thiserror`; and `uuid`. The crate declares the `adapter` role.

# Reconciliation

The design names no section for this crate. It is the contract the terminal and the daemon share,
drawn from the bus's opaque payload rule rather than from a design boundary. The bus's wire types
remain private to `mokmok-event-bus`; this crate holds the domain payloads and the topic grammar.

# Tests

Fourteen cases. They cover every command, reply, refusal, session event, commit, and roster value
through a `Payload` round trip; the three topic and subscriber identities; an empty history as one
empty last chunk; order preservation across chunks; the ceiling with a normal record and with one
record that goes alone; and a roster split that marks only its last chunk.

```
cargo test -p mokmok-session-wire
MUTANTS_PACKAGE=mokmok-session-wire ./scripts/mutants.sh --jobs 8
```

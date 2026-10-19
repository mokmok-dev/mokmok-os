---
type: Crate
title: mokmok-protocol
description: Typed identifiers and the serde message contracts, with no transport, storage, or clock.
resource: ../../../mokmok-protocol
tags: [mokmok-os, crate, protocol]
status: draft
generated: { by: grok-build/grok-4.7, at: 2026-10-15T02:00:00Z }
stale_after: 2026-11-15T00:00:00Z
sources:
  - id: architecture
    resource: ../../../mokmok-os-architecture.md
    title: mokmok OS architecture and crate responsibilities, section 5.1
    author: codex/gpt-6
  - id: crate
    resource: ../../../mokmok-protocol
    title: mokmok-protocol sources
    last_modified: 2026-10-12T06:37:04Z
---

# Responsibility

Defines the contracts that travel between components as values: typed identifiers, the envelope a
message arrives in, the command shape, the instant the kernel records, and the schema-version
policy. The crate owns no transport, no storage, and no clock.[^crate]

# Public surface

Modules `envelope`, `error`, `id`, `message`, `moment`, and `version`.

| Item | Where | What it is |
| --- | --- | --- |
| `ID<M>` | `src/id.rs:13` | A UUID branded by a sealed marker type |
| Agent, Session, Execution, Turn, Command, Event, Artifact, Message, Correlation, Principal, Worker, Trace, Span markers | `src/id/marker.rs:15-75` | The marker set `ID<M>` is generic over |
| `Envelope`, `Source`, `MediaType`, `Data`, `Subject`, `TraceContext` | `src/envelope.rs:358,34,79,118,303,184` | The message frame and its parts |
| `MessageKind` | `src/message.rs:14` | Command, Event, Query, Control, Telemetry |
| `Command` | `src/message.rs:29` | A requested operation with its envelope |
| `Moment` | `src/moment.rs:28` | Milliseconds since the epoch, with checked arithmetic |
| `SchemaVersion`, `Compatibility` | `src/version.rs:11,72` | The version policy, `CURRENT = 1.0` |
| `ProtocolError` | `src/error.rs:14` | The decode and validation failure |

# Owned state

None. The crate holds no static, no file, and no map.

# Invariants

- Decode re-validates. `Envelope::from_json` (`src/envelope.rs:439`) routes through `validate`
  (`src/envelope.rs:455`), which checks version, source, media type, and payload in that order.
- A `Source` is non-empty, free of whitespace, and at most 253 bytes. One function, `ensure_source`
  (`src/envelope.rs:64`), serves the constructor and the decoder, so bytes cannot reach a state the
  constructor refuses.
- Media type is default-deny. Anything other than `application/json` is refused
  (`src/envelope.rs:90`).
- One version policy serves every message. A newer minor version or any other major version is
  `Unsupported` (`src/version.rs:48`).
- Arithmetic on `Moment` refuses or clamps: `checked_add` and `checked_sub` answer `None` when the
  sum leaves the range, `saturating_add` clamps, and `since` clamps at zero
  (`src/moment.rs:49,61,74,86`).
- Cross-domain identifier substitution is a type error. `ID<M>` is generic over a sealed marker
  (`src/id.rs:5,13`).
- A trace context is strict: version `00`, exactly four fields, a non-zero trace identifier, and
  lowercase hex (`src/envelope.rs:200`).

# Dependencies

`serde` with `alloc` and `derive`, `serde_json` without default features, and `uuid` with
`fast-rng`, `serde`, and `v7`. Development: `proptest`, `serde_json` with `std`. The crate is
`#![no_std]` with `extern crate alloc` (`src/lib.rs:15-17`). No tokio, no rusqlite, no axum.

# Reconciliation

The design gives the modules the names `identifiers`, `command`, `event`, `envelope`,
`subscription`, `error`, and `version`.[^architecture] The crate names them `id`, `message`,
`envelope`, `moment`, `error`, and `version`, and it holds no subscription type.

The commit positions, publish receipts, subscription requests, and ACK frames the design places here
live in the crates that use them. `CommitPosition` is in `mokmok-event-store/src/model.rs:187`, the
`Cursor`, `Batch`, and `Refusal` types are in `mokmok-event-bus/src/event.rs:42,269,132`, and the
acknowledgement frames are in `mokmok-event-bus/src/wire.rs:28`.

The design names CloudEvents as the envelope. The token appears in no Rust file in the workspace.

Two names reach other crates. A workspace-wide search finds `Moment` and the identifier markers
imported elsewhere, and finds no consumer for `Envelope`, `MessageKind`, `Command`, `TraceContext`,
`SchemaVersion`, `Compatibility`, or `ProtocolError` outside this crate's own tests.

# Tests

Six `#[cfg(test)]` modules hold 43 `#[test]` cases and 8 proptest cases: envelope construction,
decode refusals, and the trace-context grammar from `src/envelope.rs:473`, the version policy in
`src/version.rs:81`, `Moment` arithmetic in `src/moment.rs:94`, the command round trip in
`src/message.rs:75`, identifier ordering in `src/id.rs:44`, and `Display` in `src/error.rs:92`.

```
cargo test -p mokmok-protocol
```

[^architecture]: Section 5.1 of `mokmok-os-architecture.md`.
[^crate]: The module list is `mokmok-protocol/src/lib.rs:19-24`.

---
type: Reference
title: The shared contracts
description: Message kinds, identifiers, the envelope, the schema-version policy, and where the contracts that left the protocol crate now live.
resource: ../../mokmok-protocol
tags: [mokmok-os, architecture, contracts]
status: draft
generated: { by: grok-build/grok-4.7, at: 2026-10-15T02:00:00Z }
stale_after: 2026-11-15T00:00:00Z
sources:
  - id: architecture
    resource: ../../mokmok-os-architecture.md
    title: mokmok OS architecture and crate responsibilities, sections 5.1 and 6
    author: codex/gpt-6
  - id: protocol
    resource: ../../mokmok-protocol
    title: mokmok-protocol sources
    last_modified: 2026-10-12T06:37:04Z
---

# Message kinds

The design separates five kinds of message, and the separation is what keeps a command from being
read as an event.[^architecture]

| Kind | Meaning | In the tree |
| --- | --- | --- |
| Command | A requested operation | `Command` in `mokmok-protocol/src/message.rs:29`, and the kernel's `submit` path |
| Event | A fact that has occurred | `Fact` in `mokmok-state-machine/src/fact.rs:12` and `NewEvent` in `mokmok-event-store/src/event.rs:137` |
| Query | A request for state or history | The kernel's `status` and `reconcile` subcommands; no query type in the protocol crate |
| Control | Subscription, acknowledgement, replay | The bus's request and response frames in `mokmok-event-bus/src/wire.rs:14,34` |
| Telemetry | Transient progress or measurement | `mokmok-observability`; no protocol type |

`MessageKind` (`mokmok-protocol/src/message.rs:14`) carries the five names as an enum, and only the
command variant has a payload type behind it.

# Identifiers

Thirteen marker types brand one UUID type, so a session identifier cannot be passed where an
execution identifier is expected. The markers are `AgentID`, `SessionID`, `ExecutionID`, `TurnID`,
`CommandID`, `EventID`, `ArtifactID`, `MessageID`, `CorrelationID`, `PrincipalID`, `WorkerID`,
`TraceID`, and `SpanID` (`mokmok-protocol/src/id/marker.rs:15-75`).

Eight of the thirteen reach another crate. `CommandID`, `MessageID`, `CorrelationID`, `TraceID`,
and `SpanID` have no consumer in the workspace.

# The envelope

`Envelope` (`mokmok-protocol/src/envelope.rs:358`) carries a source, a media type, a subject, a
payload, and optional correlation, causation, and trace context. Every field is validated on the way
in, including on decode, so a value that arrives over a wire passes the same checks a constructor
applies.

Only `application/json` is accepted as a media type (`mokmok-protocol/src/envelope.rs:90`), and a
`traceparent` is held to version `00`, four fields, a nonzero trace identifier, and lowercase hex
(`mokmok-protocol/src/envelope.rs:200`).

The design names CloudEvents as the envelope format.[^architecture] The envelope is a Rust type
with no CloudEvents attribute set, and the name appears in no source file.

# Versioning

`SchemaVersion` (`mokmok-protocol/src/version.rs:11`) is the version type, its `CURRENT` constant is
`1.0` (`mokmok-protocol/src/version.rs:18`), `compatibility` (`mokmok-protocol/src/version.rs:44`)
answers with the four cases, and any other major version or a newer minor version is `Unsupported`.

The event store applies the same idea at the SQLite level. `prepare`
(`mokmok-event-store/src/store.rs:309`) accepts a `user_version` of 0 or 1, stamps 1, and refuses
anything else with `Error::Schema`.

# Contracts that live elsewhere

The design places commit positions, publish receipts, subscription requests, and acknowledgement
frames in the protocol crate.[^architecture] Each of them lives with the component that uses it:

| Contract | Where it lives |
| --- | --- |
| `CommitPosition`, `Version`, `Expected` | `mokmok-event-store/src/model.rs:187,161,222` |
| `Cursor`, `Window`, `Lapsed`, `Batch` | `mokmok-event-bus/src/event.rs:42,68,100,263` |
| Request and response frames | `mokmok-event-bus/src/wire.rs:28,53` |
| `Payload` | `mokmok-event-store/src/payload.rs:20` and `mokmok-event-bus/src/payload.rs:3` |

Two crates each define a `Payload` type and a `Kind` or `Seq` type of their own. The protocol crate
owns neither.

# What the protocol crate carries today

`Moment` and the identifier markers are the two parts of this crate that other crates use. The
envelope, the trace context, the message kind, the schema version, and the protocol error are
exercised by the crate's own tests and by nothing else.

That is a fact about composition, and it has a consequence for anyone extending the system: an
envelope-shaped contract is available and untested against a second consumer.

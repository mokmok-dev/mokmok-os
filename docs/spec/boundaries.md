---
type: Reference
title: Port ownership and transaction boundaries
description: Where each port is defined, who implements it, and what commits as one unit of work.
resource: ../../mokmok-os-architecture.md
tags: [mokmok-os, architecture, ports]
status: draft
generated: { by: grok-build/grok-4.7, at: 2026-10-15T02:00:00Z }
sources:
  - id: architecture
    resource: ../../mokmok-os-architecture.md
    title: mokmok OS architecture and crate responsibilities, section 6
    author: codex/gpt-6
  - id: repo
    resource: ../../..
    title: the mokmok-os repository at commit bbe2ea2
    last_modified: 2026-10-14T03:34:14Z
---

# The rule the design states

A port is defined next to the code that consumes it, and concrete adapters are composed in
infrastructure code. The protocol crate carries data contracts and does not accumulate every trait
in the system.[^architecture]

The tree follows that rule. Each port below is declared in the crate that needs the capability, and
the implementation that ships is named beside it.

| Port | Declared in | Implemented in the tree |
| --- | --- | --- |
| `Sandbox` | `mokmok-sandbox/src/sandbox.rs:19` | `Confined` and `LimitsOnly` in the same crate, driven by `mokmok-kernel/src/execution.rs` |
| `Tools` | `mokmok-agent-runtime/src/tool.rs:90` | `Shell` in `mokmok-kernel/src/agent.rs` |
| `Provider` | `mokmok-agent-runtime/src/model.rs:55` | `Ollama` in `mokmok-kernel/src/provider.rs`, and `Model` and `Scripted` in `mokmok-kernel/src/agent.rs` |
| `Journal` | `mokmok-agent-runtime/src/model.rs:70` | `Ledger` in `mokmok-kernel/src/agent.rs` |
| `Finder`, `Index` | `mokmok-agent-runtime/src/find.rs:18` | `Index` in `mokmok-kernel/src/index.rs:19`, one BM25 corpus per granted root |
| `Clock` | `mokmok-scheduler/src/clock.rs:10` | The scheduler's own tests. The scheduler is composed nowhere |
| `Resolver` | `mokmok-egress/src/dns.rs:10` | `SystemResolver` in `mokmok-kernel/src/provider.rs:349` |
| `PolicyClient` | `mokmok-egress/src/policy_client.rs:34` | `Permit` in `mokmok-kernel/src/provider.rs` |
| `AuditSink` | `mokmok-egress/src/audit.rs:45` | `AuditToLog` in `mokmok-kernel/src/provider.rs:398` |
| `Authorizer` | `mokmok-artifact/src/metadata.rs:94` | Test fixtures only |
| `ReferenceStore` | `mokmok-artifact/src/metadata.rs:13` | Test fixtures only |
| `Exporter` | `mokmok-observability/src/exporter.rs:12` | A test fixture in the same crate |
| `AuditLog` | `mokmok-observability/src/audit.rs:116` | `Journal` in the same crate, used directly by `mokmok-kernel/src/provider.rs` |
| `Engine`, `Screen` | `mokmok-tui/src/port.rs:15,52` | `Session` and `Terminal` in the same crate, over the kernel's driver |

Four of these rows have no implementation outside their own crate's tests: `Clock`, `Authorizer`,
`ReferenceStore`, and `Exporter`. Each of them belongs to a crate whose port has no production
caller, which is the subject of [the acceptance table](verification/acceptance.md).

A doc example is not an implementation. The `Frozen` clock in `mokmok-scheduler/src/lib.rs`, the
`Permit` policy client and the `Discard` audit sink in `mokmok-egress/src/lib.rs`, and the
`Registry` reference store with its `Unrestricted` authorizer in `mokmok-artifact/src/lib.rs` are
doctest fixtures, and each one is compiled and run by `cargoDocTest`.

# One unit of work

State and events that must commit together need one unit of work. Calling independent asynchronous
repositories one after another cannot establish atomicity.[^architecture]

The tree has one such boundary. `mokmok_event_store::WriteTx` (`mokmok-event-store/src/write.rs:15`)
lets a consumer write its own projection, read history, and append an event inside the transaction
`Store::consume` opened, and the whole transaction commits or rolls back as one.

No other pair of stores shares a transaction. The audit journal is a separate file, so a security
decision and the event it accompanies cannot commit together, which
[the observability crate](crates/observability.md) records.

# Storage

The design proposes `rusqlite` with a dedicated database worker and one serialized append
path.[^architecture] `rusqlite` is the driver in both SQLite-owning crates.

The event store holds one `rusqlite::Connection` (`mokmok-event-store/src/store.rs:81`) and takes
writes through one `&mut self` path, so the type provides the serialization and no worker thread is
involved. The event bus puts its connection on one operating-system thread behind a bounded channel
of 64 (`mokmok-event-bus/src/store.rs:22,80`), which is the worker the design describes.

The kernel holds one appender, `Db`, over the store (`mokmok-kernel/src/db.rs:276`), so a process
has one serialized append path into the log.

# The rules that guard the boundaries

The dependency rules that keep these boundaries intact are stated as a computation a reader can
run, in [the dependency rules](verification/dependency-rules.md).

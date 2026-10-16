---
type: Crate
title: mokmok-observability
description: Exports traces, metrics, and logs, and keeps a durable fsynced record of security decisions with redaction applied on the way out.
resource: ../../../mokmok-observability
tags: [mokmok-os, crate, observability, audit]
status: draft
generated: { by: grok-build/grok-4.7, at: 2026-10-15T02:00:00Z }
stale_after: 2026-11-15T00:00:00Z
sources:
  - id: architecture
    resource: ../../../mokmok-os-architecture.md
    title: mokmok OS architecture and crate responsibilities, section 5.11
    author: codex/gpt-6
  - id: crate
    resource: ../../../mokmok-observability
    title: mokmok-observability sources
    last_modified: 2026-10-12T16:42:42Z
---

# Responsibility

Carries two kinds of signal out of the process. Traces, metrics, and logs are transient and go
through the tracing surface. A security decision goes to a durable journal that fsyncs before it
returns.[^crate]

# Public surface

| Item | Where | What it is |
| --- | --- | --- |
| `AuditEntry`, `AuditKind`, `Outcome` | `src/audit.rs:43,17,30` | One durable decision record |
| `AuditLog` | `src/audit.rs:116` | `append` and `read` |
| `append_and_export` | `src/audit.rs:151` | Append to the journal, then hand the entry to the exporter |
| `Journal` | `src/journal.rs:17` | The JSON-lines implementation, `open` and `path` |
| `Exporter` | `src/exporter.rs:12` | The export port |
| `Metrics`, `Snapshot` | `src/metrics.rs:13,86` | Counters and gauges in memory, and a serializable view |
| `Redaction`, `Rule`, `InvalidRule`, `RedactingFormat` | `src/redaction.rs:76,10,53`, `src/format.rs:18` | Literal replacement rules and the tracing layer that applies them |
| `Config`, `Guard`, `init` | `src/init.rs:16,47,74` | The subscriber installation and its reload handle |

# Owned state

One JSON-lines file, created append-only by `Journal::open` (`src/journal.rs:26-33`), holding one
entry per line and never truncated. In memory: two maps of counters and gauges
(`src/metrics.rs:13-15`), a vector of redaction rules (`src/redaction.rs:76-78`), and the installed
global subscriber with its reload handle (`src/init.rs:47-49`). The crate holds no database.

# Invariants

- `Journal::append` calls fsync before it returns (`src/journal.rs:45-61`), so a caller told the
  entry landed may report the decision as recorded.
- `append_and_export` appends first and exports afterwards. An append failure returns without
  calling the exporter (`src/audit.rs:157-162`).
- A redaction rule with an empty pattern is refused with its index, because it would rewrite every
  line while removing nothing (`src/redaction.rs:92-96`).
- Rules apply in order, each rewriting the previous output (`src/redaction.rs:109`), and the
  `Debug` rendering of a field reaches the same redaction (`src/format.rs:57-67`).
- The subscriber installs once per process; a second install is `Error::Install`
  (`src/init.rs:85`).
- A gauge is replaced by `set_gauge`, and `add` saturates at the maximum instead of wrapping
  (`src/metrics.rs:37-43`).
- The counter names are private constants (`src/audit.rs:10-14`), so only `append_and_export` moves
  them.

# Dependencies

`mokmok-protocol`, `serde`, `serde_json`, `thiserror`, `tracing`, and `tracing-subscriber` with
`env-filter` and `json`. Development adds `proptest` and `uuid`. The crate declares no tokio, no
rusqlite, and no axum dependency.

# Reconciliation

The design names the modules `tracing`, `metrics`, `audit`, `exporter`, and `redaction`.[^architecture]
There is no `tracing` module. The layer lives in `src/format.rs` and the installation in
`src/init.rs`.

The design lists this crate's job as instrumentation and export, with durable audit facts belonging
in a durable transaction.[^architecture] The journal is a separate file beside the event log. It
fsyncs per entry, and an audit entry and the decision it records cannot commit together.

`src/exporter.rs` holds the port and no implementation, and `Metrics`, `Snapshot`,
`append_and_export`, and `RedactingFormat` have no caller outside this crate. The daemon reaches the
audit journal directly: `mokmok-kernel/src/provider.rs:342-356` calls `AuditLog::append` and
downgrades a failure to a log line, so a security fact can be lost with a line printed.

The design has redaction remove protected payloads according to policy. `Redaction` is literal
string replacement over rules the caller supplies, nothing reads a policy, and the daemon installs
`Redaction::none()` (`mokmok-kernel/src/serve.rs:300`).

Behaviour the design does not name: the JSON-lines shape with its level and target framing
(`src/format.rs:88-95`), stderr as the writer with a reloadable `EnvFilter` (`src/init.rs:74-88`),
and a serde-serializable `Snapshot` (`src/metrics.rs:85`).

# Tests

17 cases in six `#[cfg(test)]` modules and no `tests/` directory. `src/redaction.rs` holds 7 cases,
including a literal secret removed with the trace identifier kept, a shorter earlier pattern
pre-empting a longer later one, and an empty pattern refused with its index. `src/audit.rs` holds
2 cases for a failing exporter erroring after the entry landed while both counters move.
`src/format.rs` holds 2 cases asserting that both a `str` field and a `?value` field are redacted
before they are written. `src/init.rs`, `src/journal.rs`, and `src/metrics.rs` hold the remaining
cases.

```
cargo test -p mokmok-observability
```

[^architecture]: Section 5.11 of `mokmok-os-architecture.md`.
[^crate]: The re-export list is `mokmok-observability/src/lib.rs:40-46`.

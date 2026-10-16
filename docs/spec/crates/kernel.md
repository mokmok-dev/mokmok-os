---
type: Crate
title: mokmok-kernel
description: The composition root. It resolves the command line into one configuration, owns the serialized appender into the event log, runs recovery before new work, drives the vertical slice and the agent loop, and serves the bus.
resource: ../../../mokmok-kernel
tags: [mokmok-os, crate, composition]
status: draft
generated: { by: grok-build/grok-4.7, at: 2026-10-15T02:00:00Z }
stale_after: 2026-11-15T00:00:00Z
sources:
  - id: architecture
    resource: ../../../mokmok-os-architecture.md
    title: mokmok OS architecture and crate responsibilities, section 5.12
    author: codex/gpt-6
  - id: crate
    resource: ../../../mokmok-kernel
    title: mokmok-kernel sources
    last_modified: 2026-10-14T03:34:14Z
---

# Responsibility

Assembles the services and coordinates their lifecycle. It wires adapters to ports, opens the
databases, runs recovery before the daemon reports ready, and exposes the five subcommands the
`mokmokd` binary carries.[^crate]

# Public surface

| Item | Where | What it is |
| --- | --- | --- |
| `Config`, `Args`, `Command` | `src/config.rs:218,102,112` | The resolved configuration and the five subcommands: submit, status, reconcile, serve, agent |
| `Db`, `Log`, `Entry`, `TurnRecord`, `ResultRecord` | `src/db.rs:276,103,45,89,96` | The appender over the event log and the folded view of one session |
| `Authority` | `src/authority.rs:45` | The policy built from flags, with `access_at` and `may_invoke` |
| `Plan`, `Ran`, `Refused`, `ExecutionResult`, `Blob` | `src/execution.rs:59,75,86,44,35` | One sandboxed execution and its recorded result |
| `Relay`, `serve` | `src/serve.rs:52,138` | The outbox relay and the daemon loop |
| `submit`, `status`, `reconcile`, `outcome_of` | `src/slice.rs:97,351,464,483` | The vertical slice |
| `Index` | `src/index.rs:19` | The finder: one BM25 corpus per root the operator granted, read in the daemon's own process |
| `agent::run`, `agent::Driver`, `agent::Report`, `agent::Scripted` | `src/agent.rs:661,542,525,363` | The agent loop and its scripted model |
| `Error` | `src/error.rs:17` | 26 variants |

`authority`, `config`, `db`, `error`, `execution`, `notify`, `provider`, `recovery`, `serve`, and
`slice` are private modules. `agent` is public. The daemon binary is
`mokmok-kernel/src/bin/mokmokd.rs`.

# Owned state

Every durable file the system uses passes through this crate: `events.sqlite`
(`src/config.rs:324`) opened by `Db::open` into `mokmok_event_store::Store`, `bus.sqlite`
(`src/serve.rs:154`), the `artifacts/` directory (`src/config.rs:260`), the `sandbox/` directory
(`src/config.rs:261`), the `bus.sock` socket (`src/config.rs:345`), `audit.jsonl`
(`src/provider.rs:45`), and one `search/<key>/` index directory per granted search root
(`src/config.rs:349`). In memory: the folded `Log` for a session (`src/db.rs:103-114`), one
publisher per topic in `Relay` (`src/serve.rs:54`), the scripted model's answer queue
(`src/agent.rs:361`), the notification socket (`src/notify.rs:43-48`), the provider credential
(`src/provider.rs:136`), and one opened corpus per search root (`src/index.rs:19`).

# Invariants

- Authority is default-deny. `Policy::new()` holds no rules and every rule comes from a named flag
  (`src/authority.rs:60-92`).
- A search root must sit inside a read or write grant, or the run is refused before any index is
  opened (`src/config.rs:311`).
- A credential file must be mode 0600 (`src/config.rs:424`) and must not sit inside a write grant
  (`src/config.rs:437`).
- An artifact path must be plain and relative. A parent-directory component, a root component, and
  the empty string are refused (`src/config.rs:394-407`).
- Crash injection needs two signals, the `--crash-after` flag and a set `MOKMOKD_CRASH`
  (`src/config.rs:355-362`).
- Every append carries the version it read back from the stream, so a moved stream is a conflict
  (`src/slice.rs:487-497`).
- One command identity commits once, and a retry is answered from the deduplication record
  (`src/db.rs:335-341`).
- A step the state machines refuse is never written. `Log::verify` folds it first
  (`src/db.rs:208`).
- An entry of an unknown kind stops the read, and no entry is skipped (`src/db.rs:477`).
- Recovery settles only executions folded to in-flight and records the outcome as unknown
  (`src/recovery.rs:40-48`). It never records success or failure.
- A notice is written inside the transaction that records the change it names (`src/db.rs:339`).
- Recovery runs before the bus accepts a client and before readiness is reported
  (`src/serve.rs:196-207`, readiness at `src/serve.rs:265`).
- The provider validates its destination through `mokmok-egress`, pins the validated address, and
  disables redirects (`src/provider.rs:495-517`).
- The keep-alive interval is half of `WATCHDOG_USEC` and is ignored when `WATCHDOG_PID` names
  another process (`src/notify.rs:148-167`).
- A turn's identity derives from the command that opened it, so a retried command owns the turn it
  already has (`src/slice.rs:206-220`).

# Dependencies

All twelve other crates of the workspace except `mokmok-tui`, plus `clap`, `reqwest`, `serde`,
`serde_json`, `thiserror`, `tokio`, `tracing`, `tracing-subscriber`, and `uuid`. The crate holds no
rusqlite dependency, so SQLite reaches it through `mokmok-event-store` and `mokmok-event-bus`. It
declares no axum dependency.

# Reconciliation

The design names the modules `config`, `bootstrap`, `services`, `lifecycle`, `recovery`, and
`shutdown`.[^architecture] The crate names them `authority`, `config`, `db`, `error`, `execution`,
`agent`, `notify`, `provider`, `recovery`, `serve`, and `slice`. There is no service registry and no
lifecycle type; serving and shutdown both live in `serve_until_stopped` (`src/serve.rs:165-285`).

The design has the kernel initiate migrations. There is no migration module, no schema-version row,
and no expected-version check. Schema creation is `CREATE TABLE IF NOT EXISTS` inside
`mokmok_event_store::Store::open`.

The design has the kernel expose health and readiness. Readiness is an `sd_notify` datagram
(`src/notify.rs:24`, sent at `src/serve.rs:270`), not an endpoint. `Command` carries submit, status,
reconcile, serve, and agent, and there is no health subcommand and no HTTP surface.

The design puts a thin binary at `apps/mokmokd` invoking kernel bootstrap. There is no `apps`
directory and no bootstrap function. The binary is `mokmok-kernel/src/bin/mokmokd.rs`, and its
`run` resolves a `Config` and dispatches on the subcommand (`src/bin/mokmokd.rs:29-59`).

The design keeps individual domain policies out of this crate.[^architecture] The kernel builds the
policy content from flags (`src/authority.rs:56-107`) and owns the provider's egress policy
(`src/provider.rs:468-533`).

`mokmok-scheduler` is a declared dependency of this crate, and no symbol from it appears under
`src/` or `tests/`. The scheduler is composed nowhere.

The kernel owns more than the design's list: the agent loop (`src/agent.rs:635`), the systemd
notification protocol (`src/notify.rs`), the egress-backed model provider (`src/provider.rs`), crash
injection (`src/config.rs:355`), and the workspace's only wall-clock read
(`src/bin/mokmokd.rs:78-88`).

# Tests

93 cases: 61 in `#[cfg(test)]` modules and 32 across five integration files.

`src/provider.rs` holds 24 cases for the measured model exchange, malformed output, the request
body, endpoint parsing, credential secrecy, and not following a redirect. `src/config.rs` holds 15
cases for resolution and refusal. `tests/slice.rs` holds 10 cases including one confined tool, a
replay, a refused tool, an unknown outcome after a crash, the relay reaching a subscriber, and
recovery before readiness. `tests/supervisor.rs` holds 12 cases over the real daemon: readiness
after the bus answers, a foreign socket never reporting ready, a stop draining and exiting zero, a
`SIGKILL` restart over the stale socket, the keep-alive following the relay, and an uninstallable
subscriber reported without failing the daemon. `tests/agent.rs` holds 8 cases for the whole loop,
a crash before dispatch, cancellation, a command no grant reaches, scripted prose, and the two
ceilings.

```
cargo test -p mokmok-kernel
MOKMOKD=target/debug/mokmokd bash scripts/daemon.sh
```

[^architecture]: Section 5.12 of `mokmok-os-architecture.md`.
[^crate]: The module list is `mokmok-kernel/src/lib.rs:11-23`.

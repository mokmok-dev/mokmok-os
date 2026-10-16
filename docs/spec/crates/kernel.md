---
type: Crate
title: mokmok-kernel
description: The composition root. It resolves the command line and the operator's configuration file into one configuration, owns the serialized appender into the event log, runs recovery before new work, drives the vertical slice and the agent loop, and serves the bus.
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

Assembles the services and coordinates their lifecycle. It resolves the command line and the
operator's configuration file into one configuration, wires adapters to ports, opens the databases,
runs recovery before the daemon reports ready, and exposes the five subcommands the `mokmokd` binary
carries.[^crate]

# Public surface

| Item | Where | What it is |
| --- | --- | --- |
| `Config`, `Args`, `Command` | `src/config.rs:365,249,259` | The resolved configuration and the five subcommands: submit, status, reconcile, serve, agent |
| `ProviderLayer`, `ProviderSettings`, `config_path`, `ProcessEnvironment` | `src/config.rs:61,89,521,555` | The provider group as the flags and the configuration file's keys at once, the same group once every layer has spoken, which owns the credential the environment answered, the path discovery looks in, and the process environment a value may name and a credential arrives in, read once by a binary and handed in |
| `Db`, `Log`, `Entry`, `TurnRecord`, `ResultRecord` | `src/db.rs:276,103,45,89,96` | The appender over the event log and the folded view of one session |
| `Authority` | `src/authority.rs:45` | The policy built from flags, with `access_at` and `may_invoke` |
| `Plan`, `Ran`, `Refused`, `ExecutionResult`, `Blob` | `src/execution.rs:59,75,86,44,35` | One sandboxed execution and its recorded result |
| `Relay`, `serve` | `src/serve.rs:52,138` | The outbox relay and the daemon loop |
| `submit`, `status`, `reconcile`, `outcome_of` | `src/slice.rs:97,351,485,504` | The vertical slice |
| `Index` | `src/index.rs:19` | The finder: one BM25 corpus per root the operator granted, read in the daemon's own process |
| `editor::Editor` | `src/editor.rs:32` | The model's editor: it plans a patch against the checkout, asks the authority for the write, and commits the bytes through a resolved directory |
| `Authority::may_write_at`, `access_at` | `src/authority.rs:127,163` | The one place the write answer is decided, from the grants the operator named |
| `agent::run`, `agent::Driver`, `agent::Report`, `agent::Scripted` | `src/agent.rs:696,573,556,373` | The agent loop and its scripted model |
| `Db::streams`, `Summary`, `sessions`, `mint_session` | `src/db.rs:388`, `src/roster.rs:20,43`, `src/slice.rs:578` | The streams the log holds and what a chooser reads of each session, and a fresh identity for one that does not exist yet |
| `Error` | `src/error.rs:17` | 26 variants |

`authority`, `config`, `db`, `error`, `execution`, `notify`, `provider`, `recovery`, `roster`, `serve`,
and `slice` are private modules. `agent` is public. The daemon binary is
`mokmok-kernel/src/bin/mokmokd.rs`.

# Owned state

Every durable file the system uses passes through this crate: `events.sqlite`
(`src/config.rs:493`) opened by `Db::open` into `mokmok_event_store::Store`, `bus.sqlite`
(`src/serve.rs:154`), the `artifacts/` directory (`src/config.rs:416`), the `sandbox/` directory
(`src/config.rs:417`), the `bus.sock` socket (`src/config.rs:510`), `audit.jsonl`
(`src/provider.rs:48`), and one `search/<key>/` index directory per granted search root
(`src/config.rs:500`). In memory: the folded `Log` for a session (`src/db.rs:103-114`), one
publisher per topic in `Relay` (`src/serve.rs:54`), the scripted model's answer queue
(`src/agent.rs:363`), the notification socket (`src/notify.rs:43-48`), the credential resolution
answered from the environment (`src/config.rs:175`), and one opened corpus per search root
(`src/index.rs:19`).

# Invariants

- Authority is default-deny. `Policy::new()` holds no rules and every rule comes from a named flag
  (`src/authority.rs:60-92`).
- A search root must sit inside a read or write grant, or the run is refused before any index is
  opened (`src/config.rs:467`).
- Every stream the commit log holds is one session's, so `Db::streams` is the whole list of them and
  the position of a stream's newest commit is the order a chooser reads (`src/db.rs:388-414`).
- A session that committed no task is listed with no goal and no checkout, and a stream that
  committed no session fact at all folds to the phase an empty history reaches rather than being
  skipped (`src/roster.rs:43-64`).
- `env_key` is answered once precedence has chosen it, so a flag beats the file and only the winning
  name is read from the environment (`src/config.rs:869`).
- The name has to be one the environment can hold, which `variable_name` decides, so
  `env_key = "$X"` is refused as not a name rather than expanded (`src/config.rs:880-886`).
- `Credential` holds the trimmed value the named variable answered, and a value that is empty or
  whitespace-only is refused as an unset one rather than reaching an Authorization header
  (`src/config.rs:890-897`).
- An artifact path must be plain and relative. A parent-directory component, a root component, and
  the empty string are refused (`src/config.rs:992-1005`).
- The configuration file supplies the provider group and nothing else, so an edited file cannot widen
  the sandbox: `ProviderLayer` is the flag surface and the file's key set at once (`src/config.rs:61`).
- A value the file writes may name a variable of the process as `$NAME` or `${NAME}`, and `expand` is
  the one place a dollar means anything: `$$` writes one literal dollar, and any other dollar is
  refused rather than passed through (`src/config.rs:685`).
- The environment a reference names is read once by the two binaries and handed to resolution as
  data, which `ProcessEnvironment` carries (`src/config.rs:555`).
- `config_path` decides the file from the environment, and `--config` names it outright; the two
  binaries also read the environment a value may name, and a file the operator named and that is not
  there is refused, while a file discovery looked for and did not find is silence
  (`src/config.rs:521`).
- The file is read for every command, so one file is valid or invalid for the whole CLI rather than
  for a subset (`src/config.rs:787`).
- A path key in the file must be plain and relative, and `answered_path` expands the written key and
  resolves it beside the file, which is where a written path keeps its own refusal
  (`src/config.rs:915`).
- Crash injection needs two signals, the `--crash-after` flag and a set `MOKMOKD_CRASH`
  (`src/config.rs:953-963`).
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
  disables redirects (`src/provider.rs:442-465`).
- The keep-alive interval is half of `WATCHDOG_USEC` and is ignored when `WATCHDOG_PID` names
  another process (`src/notify.rs:148-167`).
- A turn's identity derives from the command that opened it, so a retried command owns the turn it
  already has (`src/slice.rs:206-220`).

# Dependencies

All twelve other crates of the workspace except `mokmok-tui`, plus `clap`, `reqwest`, `serde`,
`serde_json`, `thiserror`, `tokio`, `toml`, `tracing`, `tracing-subscriber`, and `uuid`. The crate
holds no rusqlite dependency, so SQLite reaches it through `mokmok-event-store` and
`mokmok-event-bus`. It declares no axum dependency.

# Reconciliation

The design names the modules `config`, `bootstrap`, `services`, `lifecycle`, `recovery`, and
`shutdown`.[^architecture] The crate names them `authority`, `config`, `db`, `error`, `execution`,
`agent`, `notify`, `provider`, `recovery`, `roster`, `serve`, and `slice`. There is no service
registry and no lifecycle type; serving and shutdown both live in `serve_until_stopped`
(`src/serve.rs:165-285`).

The design has the kernel initiate migrations. There is no migration module, no schema-version row,
and no expected-version check. Schema creation is `CREATE TABLE IF NOT EXISTS` inside
`mokmok_event_store::Store::open`.

The design has the kernel expose health and readiness. Readiness is an `sd_notify` datagram
(`src/notify.rs:24`, sent at `src/serve.rs:270`), not an endpoint. `Command` carries submit, status,
reconcile, serve, and agent, and there is no health subcommand and no HTTP surface.

The design puts a thin binary at `apps/mokmokd` invoking kernel bootstrap. There is no `apps`
directory and no bootstrap function. The binary is `mokmok-kernel/src/bin/mokmokd.rs`, and its
`run` resolves a `Config` and dispatches on the subcommand (`src/bin/mokmokd.rs:29-61`).

The design keeps individual domain policies out of this crate.[^architecture] The kernel builds the
policy content from flags (`src/authority.rs:56-107`) and owns the provider's egress policy
(`src/provider.rs:403-481`).

`mokmok-scheduler` is a declared dependency of this crate, and no symbol from it appears under
`src/` or `tests/`. The scheduler is composed nowhere.

The kernel owns more than the design's list: the agent loop (`src/agent.rs:670`), the systemd
notification protocol (`src/notify.rs`), the egress-backed model provider (`src/provider.rs`), crash
injection (`src/config.rs:953`), and the workspace's only wall-clock read
(`src/bin/mokmokd.rs:80-88`).

# Tests

221 cases: 166 in `#[cfg(test)]` modules, one of them ignored, and 55 across five integration files.

`src/provider.rs` holds 31 cases for the measured model exchange, malformed output, the request
body, endpoint parsing, the tool list a session's grants decide, and not following a redirect. One of
them is ignored: it reaches a live endpoint by hand. `src/config.rs` holds 66 cases for resolution
and refusal, 33 of them driving a configuration file the test wrote.
`src/editor.rs` holds 26 cases for planning an edit and committing one: the plan for one file, for a
file that does not exist, at the size ceiling, and through a checkout the operator reached by a
symlink; the commit that creates the file the plan read nothing for, writes the bytes with the mode
preserved, answers `Stale` for a file that moved under it, refuses one that is gone, and refuses a
target that appeared while a create was being written, taking the target's name only while it is
absent; the ceiling itself, which is four mebibytes; the directory sync that fails after the write and
the rename that fails before it, neither of which is a refusal; the placement a matched and an
inserted hunk renders, and the two sentences a refusal's reason and a refused report render; and the
refusals, a two-file patch, a deletion, a create over an existing file, an `expect` that does not
match or is not a 64-character hexadecimal hash, a non-UTF-8 file, a hunk that matches nothing and is
counted from one, a symlinked target, a directory on the way to the file that is a symlink, a read-only
root, and a file over the ceiling. `src/authority.rs` holds 4 cases for the write answer a target
gets: a path under a write root, one under a read root, one under no root, and the deepest write root
winning a nested pair.
`src/roster.rs` holds 5 cases for the listing: an empty log, the session written most recently coming
first, a session that recorded a task carrying its goal and its checkout, the turn count, and a
stream with no session fact. `tests/slice.rs` holds 12 cases including one confined tool, a replay, a
refused tool, an unknown outcome after a crash, the relay reaching a subscriber, and recovery before
readiness. `tests/supervisor.rs` holds 13 cases over the real daemon: readiness after the bus answers,
a foreign socket never reporting ready, a stop draining and exiting zero, a `SIGKILL` restart over the
stale socket, the keep-alive following the relay, and an uninstallable subscriber reported without
failing the daemon. `tests/agent.rs` holds 28 cases for the whole loop, one of them driving a run
whose provider group comes from a configuration file, one the credential the environment answered to a
stub endpoint, a crash before dispatch, cancellation, a command no grant reaches, scripted prose, an
edit that lands its bytes and one a refusal writes nothing for, and the two ceilings.

```
cargo test -p mokmok-kernel
MOKMOKD=target/debug/mokmokd bash scripts/daemon.sh
```

[^architecture]: Section 5.12 of `mokmok-os-architecture.md`.
[^crate]: The module list is `mokmok-kernel/src/lib.rs:11-23`.

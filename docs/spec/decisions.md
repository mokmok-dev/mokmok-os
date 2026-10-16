---
type: Reference
title: The decision register
description: The decisions the design left open, what the tree settled, and the decisions the tree made that the design did not name.
resource: ../../mokmok-os-architecture.md
tags: [mokmok-os, architecture, decisions]
status: draft
generated: { by: grok-build/grok-4.7, at: 2026-10-15T02:00:00Z }
sources:
  - id: architecture
    resource: ../../mokmok-os-architecture.md
    title: mokmok OS architecture and crate responsibilities, section 11
    author: codex/gpt-6
  - id: repo
    resource: ../../..
    title: the mokmok-os repository at commit bbe2ea2
    last_modified: 2026-10-14T03:34:14Z
---

# Decisions the design left open

The design names eleven decisions and the evidence each one needs.[^architecture] The third column
records what the tree holds today.

| Decision | The design's direction | Today |
| --- | --- | --- |
| SQLite driver | `rusqlite` with a dedicated worker | `rusqlite` in both SQLite-owning crates. The event bus runs its connection on one thread behind a bounded channel; the event store serializes by taking `&mut self` |
| Commit positions | A single append path | The store's position is the `commit_log` rowid, and the kernel holds one appender. A concurrent-writer case checks that positions stay dense |
| Commit notification | `watch` as a wakeup hint | The bus carries a per-topic `Window`, and the read path closes the gap by snapshotting the channel before it reads the log |
| Replay and retention | Checkpoint-aware retention and snapshots | Retention bounds event count, bytes, and idle topics. A checkpoint behind the floor is answered with `Lapsed` and no events. There is no snapshot mechanism |
| Acknowledgement semantics | Durable monotonic checkpoints | Checkpoints are monotonic and clamped to what was delivered. There is no filtering, so no rule for filtered positions exists |
| sandbox-runtime integration | Compare the CLI and the sidecar | Neither was adopted. The confiner is a Landlock program in the sandbox crate |
| Egress integration | Align with sandbox isolation | The sandbox denies TCP through Landlock and never consults the egress crate. UDP to a literal address and Unix sockets stay open |
| Rust and Effect-TS boundary | Typed RPC plus event contracts | No TypeScript in the workspace, so the boundary is untested |
| IPC identity | Unix socket peer credentials plus capabilities | The socket is mode 0600 in a 0700 directory, owned by the daemon's user. No peer-credential check exists |
| External effect recovery | Idempotency where available, reconciliation otherwise | The state machine carries a `Resolution::Unknown`, and one kernel case reconciles an execution whose outcome a crash hid |
| Artifact publication | Persist the blob before publishing the reference | The kernel writes bytes before the entry that names them. No `Reference` is ever built, so the ordering rule is not exercised |

Two decisions in that table are still open in the sense the design meant. The commit-to-notification
gap is closed by the reader on every pull, and no test constructs the gap to prove the closure. The
egress boundary and the sandbox isolation boundary remain separate components with no shared
enforcement.

# Decisions the tree made

Five choices shape the tree and appear in no design section.

The event bus keeps its own log. The design has the bus read committed events from the event
store.[^architecture] The bus opens a second SQLite file with its own schema, and the two logs share
no position, no transaction, and no identifier space. A system that needed one durable order across
both would have to build it.

Recovery reads the commit log directly. `Db::unsettled_sessions` (`mokmok-kernel/src/db.rs:392`)
groups the log by event stream and folds each one, so the log is the session index and no registry
of live sessions exists. A session whose log is unreadable stops startup by design.

Readiness is a notification datagram. The daemon announces readiness through the systemd notify
protocol (`mokmok-kernel/src/notify.rs`), so there is no endpoint to probe and nothing to poll. The
supervision tests bind their own notification socket to assert it.

Crash injection takes two signals. A crash point fires only when the `--crash-after` flag names it
and `MOKMOKD_CRASH` is set (`mokmok-kernel/src/config.rs:570`), so a production process cannot be
crashed by a flag alone.

The provider group comes from a file, and authority does not. Both binaries read the provider, the
model, the endpoint, the script, the credential path, and the answer ceiling from
`$XDG_CONFIG_HOME/mokmok/config.toml`, falling back to `$HOME/.config`, with a flag above the file
and the file above the built-in default (`mokmok-kernel/src/config.rs:454`). The authority grants,
the data directory, the tool environment, and the agent-run budgets stay on the command line, so a
file the operator can edit cannot widen what the sandbox lets a workload reach. The design names no
configuration file at all, and its default-deny authority section assumes every rule arrives as a
named flag.

# New decisions the register should carry

These are not in the design's table and each one has a cost that a later reader will meet.

The three crates with an unimplemented port. `mokmok-scheduler`, `mokmok-artifact`, and
`mokmok-observability` each declare a port whose only implementation is a test fixture: the clock,
the authorizer and the reference store, and the exporter. Each is a decision to ship the boundary
before its second user.

The protocol crate's envelope. The envelope, the trace context, and the version policy are written
and unused. Either a second consumer appears or the crate carries machinery with one test suite
behind it.

The scheduler's place. `mokmok-kernel` declares `mokmok-scheduler` and never calls it. The crate is
either waiting for a caller or the manifest line is stale, and the difference matters to anyone
reading the dependency graph.

The mutation gate's reach. `scripts/mutants.sh` scores one package per run, selected by
`MUTANTS_PACKAGE`, and enforces a 95% floor. It is not part of `nix flake check`, and nothing in the
tree records which packages have been scored. A package nobody has run it against is a package whose
assertion strength is unknown, and a match-heavy module can sit outside the score while the gate
reports a high number.

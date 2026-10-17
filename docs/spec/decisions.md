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

Six choices shape the tree and appear in no design section.

The event bus keeps its own log. The design has the bus read committed events from the event
store.[^architecture] The bus opens a second SQLite file with its own schema, and the two logs share
no position, no transaction, and no identifier space. A system that needed one durable order across
both would have to build it.

Recovery reads the commit log directly. `Db::unsettled_sessions` (`mokmok-kernel/src/db.rs:425`)
groups the log by event stream and folds each one, so the log is the session index and no registry
of live sessions exists. A session whose log is unreadable stops startup by design.

Readiness is a notification datagram. The daemon announces readiness through the systemd notify
protocol (`mokmok-kernel/src/notify.rs`), so there is no endpoint to probe and nothing to poll. The
supervision tests bind their own notification socket to assert it.

Crash injection takes two signals. A crash point fires only when the `--crash-after` flag names it
and `MOKMOKD_CRASH` is set (`mokmok-kernel/src/config.rs:953`), so a production process cannot be
crashed by a flag alone.

The provider group comes from a file, and authority does not. Both binaries read the provider, the
model, the endpoint, the script, the variable a credential arrives in, and the answer ceiling from
`$XDG_CONFIG_HOME/mokmok/config.toml`, falling back to `$HOME/.config`, with a flag above the file
and the file above the built-in default (`mokmok-kernel/src/config.rs:521`). The authority grants,
the data directory, the tool environment, and the agent-run budgets stay on the command line, so a
file the operator can edit cannot widen what the sandbox lets a workload reach. A value in that file
may name a variable of the daemon's environment as `$NAME` or `${NAME}`, and the two binaries read
the environment once at their boundary and hand it to resolution as data. A reference reaches only
the three keys serde reads as text, so `endpoint` is one of them, and the environment a unit,
container, or wrapper starts the daemon with picks the host that receives the goal. `env_key` is the
exception that is read literally and names the variable a credential comes from. The design names no
configuration file at all, and its default-deny authority section assumes every rule arrives as a
named flag.

Skills are advertised and loaded, and the design names neither. `--skill-root` names each root whose
immediate subdirectories are candidates, and the kernel reads them the way it reads its own
configuration file, so a root needs no `--allow-path` grant: a skill adds instructions the operator
chose to hand over, not reach the sandbox can use (`mokmok-kernel/src/config.rs:383`). Discovery
freezes the validated name, description, source, and content hash into the recorded task, so the
advertisement is durable and a resumed session keeps the skills it was opened with. Activation is
read-only, so it writes no intent and no resolution and commits the instructions and the turn's
settlement in one transaction, and a skill whose file changed after discovery is refused rather than
read. A frontmatter key the spec does not name is skipped, value and all, because nothing reads it,
and a file the parser refuses is left out of the catalogue with its reason rather than aborting the
run. The cost is that a misspelt known key arrives as a missing required field, which refuses the
whole skill, and that until the model asks, the advertisement is all it has of the instructions.

# The credential moved into the environment

This one is a reversal of an earlier decision, and the reversal is the point of writing it down.

The credential was a file whose path the operator named, and resolution refused one that was not mode
0600 or that sat inside a `--allow-write` grant. The mode check protected the value from a second
user; the write-grant check protected it from the model's own tools, because a run inside such a
grant could rewrite the file the next invocation would read. The value is now the one the variable
`env_key` names holds, trimmed, so both checks are gone with the file.

The write-grant check is subsumed rather than dropped. A confined workload can no longer influence
the daemon's environment at all: `agent run` starts its tools with `Environment::Exact`, so the state
the check refused is unrepresentable rather than tested for, and `mokmok-kernel/tests/environment.rs`
pins that a variable the daemon holds does not cross into a tool.

What the mode check protected is not subsumed. The secret now lives in the daemon's own environment,
which is a worse place for it: every process running as the same user can read it out of
`/proc/<pid>/environ`, and `systemctl show` prints a unit's environment to anyone who can ask. A
credential is now as exposed as the daemon's `PATH` is, where before a second user needed the file's
group or other bits to reach it. The kernel reads `/proc` nowhere, so nothing here exploits that,
but a hostile same-user process does not need the kernel's help.

The shipped `mokmokd.service` reads no configuration file and opens no provider, so it carries no
credential and is unaffected. The cost arrives for an operator who gives the daemon a file that
names `env_key`: the variable has to reach the daemon's own environment through `Environment=` or
`EnvironmentFile=`, and resolution reads that file before it matches the subcommand, so an absent
variable refuses `serve` and `status` as well as `agent run`.

One consequence is a loss of compatibility rather than a hazard. systemd's own secret handling is
`LoadCredential=`, which writes a 0400 file under `$CREDENTIALS_DIRECTORY` and hands the daemon its
path in an environment variable. That variable holds a path and not a secret, and `env_key` reads a
variable's value as the credential itself, so a unit that used `LoadCredential=` cannot name what it
puts there. An operator who wants that shape has to point `env_key` at nothing and arrange the file
themselves, which is what this change removed the support for.

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

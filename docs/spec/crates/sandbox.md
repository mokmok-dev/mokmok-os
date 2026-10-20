---
type: Crate
title: mokmok-sandbox
description: Supervises one execution of a command under a Landlock-confined process, gating every filesystem exposure against issued capabilities.
resource: ../../../mokmok-sandbox
tags: [mokmok-os, crate, isolation]
status: draft
generated: { by: grok-build/grok-4.7, at: 2026-10-15T02:00:00Z }
stale_after: 2026-11-15T00:00:00Z
sources:
  - id: architecture
    resource: ../../../mokmok-os-architecture.md
    title: mokmok OS architecture and crate responsibilities, section 5.8
    author: codex/gpt-6
  - id: crate
    resource: ../../../mokmok-sandbox
    title: mokmok-sandbox sources
    last_modified: 2026-10-13T06:22:35Z
---

# Responsibility

Runs one untrusted command inside an operating-system boundary it can enforce, and refuses an
execution whose requested filesystem exposures no issued capability covers. Limits, a deadline, and
a cleanup step bound the run. A session whose reach changes between runs opens a `Scope`, which
materializes an isolated view of one checkout and freezes its grant ledger for each spawn.[^crate]

# Public surface

| Item | Where | What it is |
| --- | --- | --- |
| `Sandbox` | `src/sandbox.rs:19` | The backend port: `spawn(&Request, &[Capability]) -> Result<Run, Refusal>` |
| `Confined`, `LimitsOnly`, `Refusal` | `src/sandbox.rs:56,148,207` | The confined backend, the degraded backend, and the refusal |
| `Capability`, `Exposure`, `Access`, `uncovered` | `src/capability.rs:120,75,9,156` | Issued authority over a path, and the gate that checks it |
| `Request`, `Environment` | `src/request.rs:31,16` | The command, its arguments, its workspace, and its environment |
| `Request::workdir` | `src/request.rs:46` | The workload's directory when it is not the execution's own workspace |
| `Scope`, `Backing`, `OpenSpec` | `src/scope.rs:195,24,52` | A session's isolated view, how it is materialized, and what one is made from |
| `Snapshot`, `Grant`, `GrantID` | `src/scope.rs:100,66,20` | One spawn's frozen grants, one recorded grant, and the id that revokes it |
| `Retired`, `ScopeError` | `src/scope.rs:151,169` | What a revocation left, and why a scope refused |
| `Run`, `Outcome`, `Failure`, `Signal`, `Completed`, `Started` | `src/run.rs:89,52,402,24,311,66` | The handle, its outcome, and its two phases |
| `Limits`, `Captured` | `src/limits.rs:9`, `src/output.rs:9` | Address space, cpu, file size, open files, output; and the captured bytes |
| `Config`, `Reached` | `src/config.rs:8`, `src/plan.rs:257` | The root directory, the confiner path, the grace period; and the plan's verdict |
| `Artifact` | `src/artifact.rs:8` | A plain relative path the workload may publish |

The crate also ships the `mokmok-sandbox-confine` binary, which applies the plan and then executes
the workload.

# Owned state

Files on disk per execution. `Workspace::create` (`src/workspace.rs:26-51`) makes
`<root>/<execution-id>/work` and `<root>/<execution-id>/ctl` at mode 0700. `Plan::write` writes
`ctl/plan.json` at mode 0600 (`src/plan.rs:73-86`), and `StatusFile::create` writes
`ctl/status.json`, a fixed 4096-byte record overwritten in place (`src/plan.rs:245-266`). A `Scope`
adds one directory under the root its spec names, conventionally `<root>/views/<name>`, holding a
worktree, a copy, or an existing directory (`src/scope.rs:24`). In memory, `Run` holds the child,
the workspace, the directory it runs in, two output pumps, a watchdog, and an overrun flag
(`src/run.rs:89-104`), and `Scope` holds its grant ledger. The crate holds no database.

# Invariants

- The exposure gate runs before anything is created. `check` precedes `prepare_workspace`
  (`src/sandbox.rs:95`), so a `Refusal` leaves no directory behind, and `uncovered_index` names the
  first exposure no capability covers (`src/capability.rs:169`).
- A capability covers component-wise and only through a real ancestor directory
  (`src/capability.rs:193`).
- A program must be absolute (`src/sandbox.rs:260`), and an artifact must be a relative path with no
  parent-directory component (`src/artifact.rs:41`).
- A scope records authority it was handed. `Scope::record` takes an already-constructed
  `Capability` (`src/scope.rs:232`) and never builds one from a path and an access set.
- A spawn freezes the ledger. `Scope::snapshot` clones the grants (`src/scope.rs:299`), and
  `Scope::spawn` replaces the request's exposures with that snapshot (`src/scope.rs:346`).
- A view presents the checkout in its own coordinates. `View::project` maps a host path under the
  checkout to its view path and refuses an ancestor of the checkout (`src/view.rs:114`).
- Revocation reaches the next spawn and reports the ancestor case. Landlock rules are fixed per
  process and union, so `Retired::Covered` names the grant that still covers (`src/scope.rs:267`).
- A named workdir must be an exposure. `workdir_of` resolves it and refuses one outside every
  exposure (`src/sandbox.rs:314`).
- The execution identifier is exclusive. `private(&directory)` fails with `AlreadyExists`, mapped
  to `Refusal::Exists` (`src/workspace.rs:31`, `src/sandbox.rs:358`).
- The kernel floor is enforced, not assumed. `Confined::new` refuses when `plan::probe` cannot build
  the ruleset (`src/sandbox.rs:79`), and `ruleset` uses `CompatLevel::HardRequirement` over
  `AccessFs::from_all(ABI::V3)` and `AccessNet::from_all(ABI::V4)` (`src/plan.rs:119-128`), so TCP
  is denied to the workload.
- No symlink escape between planning and confinement. Rule paths are opened with
  `RESOLVE_NO_SYMLINKS` through `openat2` (`src/plan.rs:149`).
- The deadline is enforced by a watchdog task holding the process id (`src/run.rs:429`), which
  `wait` aborts after reaping (`src/run.rs:211`).
- Output capture is capped without deadlocking the pipe. The reader keeps the first bytes and
  discards the rest (`src/output.rs:82`).

# Dependencies

`landlock`, `mokmok-protocol`, `nix`, `serde`, `serde_json`, `thiserror`, `tokio` with `io-util`,
`process`, `rt`, and `time`, `tracing`, and `uuid`. Development adds `proptest` and `tokio` with
`macros` and `rt-multi-thread`. The crate declares no rusqlite, no axum, no `mokmok-capability`,
and no `mokmok-egress` dependency.

# Reconciliation

The design names the modules `supervisor`, `backend`, `execution`, `process`, `workspace`,
`limits`, `cleanup`, and `srt`.[^architecture] The crate names them `artifact`, `capability`,
`config`, `confine`, `limits`, `output`, `plan`, `request`, `run`, `sandbox`, `scope`, `view`, and
`workspace`. The backend is the `Sandbox` trait with two implementations, `Confined` and
`LimitsOnly`. Cleanup lives on `Completed::cleanup` (`src/run.rs:367`) and in
`workspace::remove_directory` (`src/workspace.rs:97`). There is no `srt` backend; the confiner is a
Landlock program in this crate.

The design has the supervisor consume validated authority from the capability crate. The sandbox
gates its own `Capability` type, and this crate does not depend on `mokmok-capability`.

The design requires that a direct network path cannot bypass the approved egress boundary. The
confined backend denies TCP through the Landlock network rule set, and the crate's own documentation
records what remains open: a workload can send UDP to a literal address and can reach a Unix socket
(`src/lib.rs:27-31`).

The design requires that teardown leaves no unmanaged child processes within the supported
supervision model. The supported model excludes a workload that calls `setsid` itself
(`src/lib.rs:17-21`).

Behaviour the design does not name: the `LimitsOnly` backend for a host whose kernel cannot confine,
which runs the same capability gate because refusing an uncovered exposure is the fail-closed
direction and enforces no filesystem exposure at all (`src/sandbox.rs:148-152`); the
`Confined::new` kernel probe; the 0600 plan and the fixed-size status record; the `Started` and
`Overran` reporting; and the `Scope` manager, which materializes an isolated view under
`<root>/views` and freezes its grant ledger for each spawn (`src/scope.rs:195`). The design's
execution-scoped workspace stays the private `<root>/<id>/work`; a named `Request::workdir` moves the
workload into the view without widening the plan (`src/sandbox.rs:314`).

# Tests

43 `#[test]` cases, 64 `#[tokio::test]` cases, and 1 proptest case.

`tests/supervision.rs` holds 33 cases: exit codes, streams, the capture cap, a private tree, a drop
leaving no task, a deadline overrun including one that is never waited on, a grandchild receiving
the signal, idempotent cleanup, artifacts, the refusals, confiner failure states, reuse of a
live identifier, and signalling without a session. `tests/filesystem.rs` holds 8 cases for denial
outside the exposures, success inside them, a named workdir, and a file refused as a workdir.
`tests/scope.rs` holds 20 cases for
worktree, copy, and existing views, grant union and revocation, the ancestor report, a grant the
view cannot present, a refused workdir, the host checkout staying unreachable, idempotent open and
teardown, and the error branches a crash or a non-repository reaches.
`tests/symlink.rs` holds 3 cases for an exposure
resolving out of the grant. `tests/network.rs` holds 1 case asserting that a confined workload
cannot reach a loopback TCP peer.

```
cargo test -p mokmok-sandbox
```

[^architecture]: Section 5.8 of `mokmok-os-architecture.md`.
[^crate]: The re-export list is `mokmok-sandbox/src/lib.rs:95-104`.

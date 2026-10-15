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
a cleanup step bound the run.[^crate]

# Public surface

| Item | Where | What it is |
| --- | --- | --- |
| `Sandbox` | `src/sandbox.rs:19` | The backend port: `spawn(&Request, &[Capability]) -> Result<Run, Refusal>` |
| `Confined`, `LimitsOnly`, `Refusal` | `src/sandbox.rs:54,139,194` | The confined backend, the degraded backend, and the refusal |
| `Capability`, `Exposure`, `Access`, `uncovered` | `src/capability.rs:120,75,9,156` | Issued authority over a path, and the gate that checks it |
| `Request`, `Environment` | `src/request.rs:31,16` | The command, its arguments, its workspace, and its environment |
| `Run`, `Outcome`, `Failure`, `Signal`, `Completed`, `Started` | `src/run.rs:89,52,399,24,308,66` | The handle, its outcome, and its two phases |
| `Limits`, `Captured` | `src/limits.rs:9`, `src/output.rs:9` | Address space, cpu, file size, open files, output; and the captured bytes |
| `Config`, `Reached` | `src/config.rs:8`, `src/plan.rs:228` | The root directory, the confiner path, the grace period; and the plan's verdict |
| `Artifact` | `src/artifact.rs:8` | A plain relative path the workload may publish |

The crate also ships the `mokmok-sandbox-confine` binary, which applies the plan and then executes
the workload.

# Owned state

Files on disk per execution. `Workspace::create` (`src/workspace.rs:26-51`) makes
`<root>/<execution-id>/work` and `<root>/<execution-id>/ctl` at mode 0700. `Plan::write` writes
`ctl/plan.json` at mode 0600 (`src/plan.rs:73-86`), and `StatusFile::create` writes
`ctl/status.json`, a fixed 4096-byte record overwritten in place (`src/plan.rs:245-266`). In memory,
`Run` holds the child, the workspace, two output pumps, a watchdog, and an overrun flag
(`src/run.rs:89-102`). The crate holds no database.

# Invariants

- The exposure gate runs before anything is created. `check` precedes `prepare_workspace`
  (`src/sandbox.rs:94`), so a `Refusal` leaves no directory behind, and `uncovered_index` names the
  first exposure no capability covers (`src/capability.rs:169`).
- A capability covers component-wise and only through a real ancestor directory
  (`src/capability.rs:193`).
- A program must be absolute (`src/sandbox.rs:240`), and an artifact must be a relative path with no
  parent-directory component (`src/artifact.rs:41`).
- The execution identifier is exclusive. `private(&directory)` fails with `AlreadyExists`, mapped
  to `Refusal::Exists` (`src/workspace.rs:31`, `src/sandbox.rs:291`).
- The kernel floor is enforced, not assumed. `Confined::new` refuses when `plan::probe` cannot build
  the ruleset (`src/sandbox.rs:79`), and `ruleset` uses `CompatLevel::HardRequirement` over
  `AccessFs::from_all(ABI::V3)` and `AccessNet::from_all(ABI::V4)` (`src/plan.rs:119-128`), so TCP
  is denied to the workload.
- No symlink escape between planning and confinement. Rule paths are opened with
  `RESOLVE_NO_SYMLINKS` through `openat2` (`src/plan.rs:149`).
- The deadline is enforced by a watchdog task holding the process id (`src/run.rs:131`), which
  `wait` aborts after reaping (`src/run.rs:204`).
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
`config`, `confine`, `limits`, `output`, `plan`, `request`, `run`, `sandbox`, and `workspace`. The
backend is the `Sandbox` trait with two implementations, `Confined` and `LimitsOnly`. Cleanup lives
on `Completed::cleanup` (`src/run.rs:365`) and in `workspace::remove_directory`
(`src/workspace.rs:96`). There is no `srt` backend; the confiner is a Landlock program in this
crate.

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
direction and enforces no filesystem exposure at all (`src/sandbox.rs:134-138`); the
`Confined::new` kernel probe; the 0600 plan and the fixed-size status record; and the `Started` and
`Overran` reporting.

# Tests

29 `#[test]` cases, 44 `#[tokio::test]` cases, and 1 proptest case.

`tests/supervision.rs` holds 30 cases: exit codes, streams, the capture cap, a private tree, a drop
leaving no task, a deadline overrun including one that is never waited on, a grandchild receiving
the signal, idempotent cleanup, artifacts, the four refusals, confiner failure states, reuse of a
live identifier, and signalling without a session. `tests/filesystem.rs` holds 5 cases for denial
outside the exposures and success inside them. `tests/symlink.rs` holds 3 cases for an exposure
resolving out of the grant. `tests/network.rs` holds 1 case asserting that a confined workload
cannot reach a loopback TCP peer.

```
cargo test -p mokmok-sandbox
```

[^architecture]: Section 5.8 of `mokmok-os-architecture.md`.
[^crate]: The re-export list is `mokmok-sandbox/src/lib.rs:95-104`.

---
type: Crate
title: mokmok-scheduler
description: Turns persisted timing and wake conditions into work claims and fenced leases, with the clock as a parameter and the durable boundary at snapshot and restore.
resource: ../../../mokmok-scheduler
tags: [mokmok-os, crate, scheduling]
status: draft
generated: { by: grok-build/grok-4.7, at: 2026-10-15T02:00:00Z }
stale_after: 2026-11-15T00:00:00Z
sources:
  - id: architecture
    resource: ../../../mokmok-os-architecture.md
    title: mokmok OS architecture and crate responsibilities, section 5.6
    author: codex/gpt-6
  - id: crate
    resource: ../../../mokmok-scheduler
    title: mokmok-scheduler sources
    last_modified: 2026-10-12T06:37:04Z
---

# Responsibility

Decides when work is eligible and hands out claims that carry a lease generation. Time enters as a
parameter, and the durable boundary is the `Snapshot` the caller stores and later restores.[^crate]

# Public surface

Modules `clock`, `error`, `job`, `lease`, and `scheduler`.

| Item | Where | What it is |
| --- | --- | --- |
| `Scheduler<C: Clock>` | `src/scheduler.rs:22` | `new`, `now`, `add`, `due`, `claim`, `renew`, `settle`, `expire`, `snapshot`, `restore`, `job`, `jobs` |
| `Snapshot` | `src/scheduler.rs:33` | The serialized form the caller stores |
| `Job` | `src/job.rs:94` | `id`, `schedule`, `state`, `generation`, `attempts`, `next_at`, `is_due` |
| `JobId`, `Schedule`, `Outcome`, `JobState` | `src/job.rs:34,47,58,68` | Identity; once, after, interval; succeeded or failed; pending, leased, succeeded, failed, dead |
| `Generation`, `Lease`, `Stale` | `src/lease.rs:18,37,84` | The fence, the claim, and the refusal |
| `Clock` | `src/clock.rs:10` | The time port |
| `ClaimError`, `Corrupt` | `src/error.rs:12,43` | Unknown, not due, finished, held; and a rejected snapshot |

# Owned state

Nothing durable. `Scheduler` holds the clock, a map of jobs by identifier, and the next identifier
(`src/scheduler.rs:22-26`). The caller stores `Snapshot` (`src/scheduler.rs:33-36`) and passes it
back to `restore`.

# Invariants

- Generation fencing applies to every protected update. `renew` and `settle` compare the presented
  lease's generation with the job's and answer `Stale` on a mismatch
  (`src/scheduler.rs:142,169`).
- A claim issues a new generation, one above the last (`src/job.rs:197`). `Generation(0)` marks a
  job that was never claimed, and the first claim is 1 (`src/lease.rs:22`).
- A lease expires at the named instant, so `at` equal to `expires` is already expired
  (`src/lease.rs:76`).
- A duplicate wakeup from the same holder returns the same lease instead of a second claim
  (`src/scheduler.rs:117`), and another holder cannot claim a live lease (`src/scheduler.rs:119`).
- `restore` validates the snapshot and returns `Corrupt` with the scheduler unchanged
  (`src/scheduler.rs:218,245`). The checks are a nonzero next identifier, unique identifiers below
  it, a lease naming its own job, a next instant on every pending or failed job, none on a terminal
  job, and agreement between the failed state's attempt count and the job's.
- A job stops after three attempts (`src/job.rs:10`), with the next attempt 30 seconds times the
  attempt count away (`src/job.rs:13,266`).

# Dependencies

`mokmok-protocol` and `serde` with `alloc` and `derive`. Development adds `proptest`, `serde_json`,
and `uuid`.
The crate is `#![no_std]` with `extern crate alloc` (`src/lib.rs:44-46`), and it
declares no tokio, rusqlite, axum, or model-provider dependency.

# Reconciliation

The design names the modules `schedule`, `heartbeat`, `lease`, `fencing`, `wakeup`, and
`recovery`.[^architecture] The crate names them `clock`, `error`, `job`, `lease`, and `scheduler`.

The design lists cron, one-shot, heartbeat, event, retry-deadline, and expired-lease triggers.
`Schedule` carries `Once`, `After`, and `Interval` only (`src/job.rs:46-54`), and no cron or
heartbeat trigger exists anywhere in the crate.

The design has storage and the clock both arrive through ports. `Clock` is a port
(`src/clock.rs:10`); storage is a plain `Snapshot` value the caller owns.

The crate is composed nowhere. `mokmok-kernel/Cargo.toml` declares it as a dependency and no symbol
from it appears under `mokmok-kernel/src` or `mokmok-kernel/tests`.

Behaviour the design does not name: the retry policy with its attempt cap and backoff, the interval
grid anchored at the scheduled instant (`src/job.rs:275`), and the snapshot validation.

# Tests

21 `#[test]` cases and 1 proptest case, in three files and no `#[cfg(test)]` module.

`tests/scheduler.rs` holds 16 cases: the reported clock, a not-due refusal, reclaiming an expired
lease, refusing a held lease, a same-holder reclaim keeping its generation, stale renew and settle,
relative and interval schedules, a retry from settlement, a restart restoring leases and
generations, snapshot validation, and the terminal predicate. `tests/model.rs` holds one proptest
case asserting that generations never decrease and a settled job never returns to pending.
`tests/errors.rs` holds 5 cases covering the `Display` of every refusal.

```
cargo test -p mokmok-scheduler
```

[^architecture]: Section 5.6 of `mokmok-os-architecture.md`.
[^crate]: The re-export list is `mokmok-scheduler/src/lib.rs:54-58`.

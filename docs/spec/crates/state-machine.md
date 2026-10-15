---
type: Crate
title: mokmok-state-machine
description: Pure, deterministic folds of recorded session facts and turn facts back into their state, with a rejection vocabulary that names the offset of a refused fact.
resource: ../../../mokmok-state-machine
tags: [mokmok-os, crate, domain]
status: draft
generated: { by: grok-build/grok-4.7, at: 2026-10-15T02:00:00Z }
stale_after: 2026-11-15T00:00:00Z
sources:
  - id: architecture
    resource: ../../../mokmok-os-architecture.md
    title: mokmok OS architecture and crate responsibilities, section 5.4
    author: codex/gpt-6
  - id: crate
    resource: ../../../mokmok-state-machine
    title: mokmok-state-machine sources
    last_modified: 2026-10-12T06:16:06Z
---

# Responsibility

Folds the recorded fact history of one session and of one turn back into their state. The fold is
pure and deterministic, so a history replays to the same state on any machine.[^crate]

# Public surface

Modules `fact`, `session`, `turn`, and `verdict`.

| Item | Where | What it is |
| --- | --- | --- |
| `Fact` | `src/fact.rs:11` | `SessionOpened`, `ExecutionLaunched`, `ExecutionSettled`, `CancelRequested`, `ResultApplied`, `SessionClosed` |
| `Deadline`, `Resolution` | `src/fact.rs:33,41` | Bounded or unbounded; finished, killed, timed out, cancelled, refused, unknown |
| `Session` | `src/session.rs:18` | `new`, `replay`, `apply`, `id`, `phase`, `execution`, `executions` |
| `Phase`, `Ending`, `Execution`, `State` | `src/session.rs:26,37,44,51` | Unopened, accepting, draining, closed; in flight, settled, applied |
| `Turn` | `src/turn.rs:107` | `new`, `replay`, `apply`, `number`, `outcome`, `answered`, `tool`, `tools`, `attempts` |
| `TurnFact`, `TurnNumber`, `Attempt`, `TurnOutcome`, `Tool` | `src/turn.rs:77,15,37,54,65` | The turn vocabulary |
| `Ack`, `Rejection`, `Rejected` | `src/verdict.rs:7,19,100` | Applied or duplicate; 14 rejection reasons; the offset that carries one |

# Owned state

Nothing durable. `Session` holds an identifier, a phase, and a map of executions
(`src/session.rs:18-22`). `Turn` holds a number, a set of attempts, the answered flag, a map of
tools, and an outcome (`src/turn.rs:106-113`). Neither type serializes, and the crate holds no
file, no table, and no clock.

# Invariants

- A fact that arrives before `SessionOpened` is refused (`src/session.rs:148`), and a turn fact
  before `TurnOpened` is refused (`src/turn.rs:164`).
- A closed session admits only a redelivery of a fact it already holds
  (`src/session.rs:182`), and a launch after the close is `SessionOver` (`src/session.rs:222`).
- A resolution is applied once. A repeated settlement with the same answer is `Duplicate`, and a
  different answer is `ResolutionConflict` (`src/session.rs:247`). A second `ResultApplied` is
  `Duplicate` (`src/session.rs:267`).
- `Resolution::TimedOut` requires a bounded deadline (`src/session.rs:249`), and a cancellation
  outside the draining phase is `UnrequestedCancel` (`src/session.rs:248`).
- Replay stops at the first refusal and names the offset that caused it (`src/session.rs:121`).
- A tool call before the model answers is `ModelNotAnswered` (`src/turn.rs:235`), an answer with no
  request is `UnrequestedAnswer` (`src/turn.rs:211`), a settled turn refuses new facts
  (`src/turn.rs:196`), and a tool resolves at most once (`src/turn.rs:254`).

# Dependencies

`mokmok-protocol` and `serde` with `alloc` and `derive`. Development adds `proptest`,
`proptest-state-machine`, `serde_json`, and `uuid`. The crate is `#![no_std]` with
`extern crate alloc` (`src/lib.rs:113-115`), and it declares no tokio, rusqlite, or axum dependency.

# Reconciliation

The design names the modules `agent`, `session`, `turn`, `execution`, `transition`, and
`invariant`.[^architecture] The crate names them `fact`, `session`, `turn`, and `verdict`. There is
no `Agent` state machine; the crate holds `Session` and `Turn`.

The design sketches a `decide` and `evolve` pair. The crate's contract is `apply` and `replay` on
both machines, with `Ack` and `Rejected` as the answers.

The design's dependency list for this crate names only the protocol types, and the code also depends
on serde. The design claims stateful property tests, mutation tests, and focused formal
verification; the property tests exist, the mutation gate has no configuration covering this crate,
and there is no formal harness.

Several facts the design does not name: the `Rejected` value carries the offset of the refused fact,
`Resolution::Unknown` records an outcome nobody can prove, and the serde representation of the fact
vocabulary is a contract its own tests pin.

# Tests

32 `#[test]` cases and 10 proptest cases, in three files and no `#[cfg(test)]` module.

`tests/model.rs` runs `prop_state_machine!` against a hand-written reference model and adds five
proptest cases: arbitrary facts, an idempotent re-fold, replay equality, a prefix split, and a
refusal leaving the state untouched. `tests/rules.rs` has 17 cases covering the verdict matrix
cell by cell, replay offsets, and every `Display` string. `tests/turn.rs` has 15 cases and 4
proptest cases covering turn gating, duplicate resolution, replay, and the serde vocabulary.

```
cargo test -p mokmok-state-machine
```

[^architecture]: Section 5.4 of `mokmok-os-architecture.md`.
[^crate]: The module list is `mokmok-state-machine/src/lib.rs:117-120`.

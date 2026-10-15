---
type: Crate
title: mokmok-agent-runtime
description: A pure decision over the transcript fold, driven one bounded step at a time, with every effect committed as an intent and then an outcome.
resource: ../../../mokmok-agent-runtime
tags: [mokmok-os, crate, agent]
status: draft
generated: { by: grok-build/grok-4.7, at: 2026-10-15T02:00:00Z }
stale_after: 2026-11-15T00:00:00Z
sources:
  - id: architecture
    resource: ../../../mokmok-os-architecture.md
    title: mokmok OS architecture and crate responsibilities, section 5.5
    author: codex/gpt-6
  - id: crate
    resource: ../../../mokmok-agent-runtime
    title: mokmok-agent-runtime sources
    last_modified: 2026-10-14T03:34:14Z
---

# Responsibility

Decides what one turn does next. `next` is a pure total function of the transcript fold, `advance`
performs one bounded step, and every effect is committed as an intent before it runs so that a
replayed intent performs nothing.[^crate]

# Public surface

Modules `advance`, `context`, `dialogue`, `error`, `model`, `record`, `speech`, `task`, and `tool`.

| Item | Where | What it is |
| --- | --- | --- |
| `next`, `advance`, `turn_id`, `execution_id` | `src/advance.rs:87,113,378,390` | The decision, the step, and the two derived identifiers |
| `Move`, `Halt`, `Progress` | `src/advance.rs:23,56,71` | Begin, ask, act, close, await, halt; the five halt reasons; the step result |
| `Transcript` | `src/dialogue.rs:23` | `replay`, `apply`, `turn`, `newest_turn`, `holds_speech`, `call_of`, `newest_answer`, `observation` |
| `Provider`, `Journal` | `src/model.rs:49,64` | The two ports the runtime states for itself |
| `Action`, `Answer`, `Usage` | `src/model.rs:13,27,43` | A final answer, a command, or malformed output |
| `Tools`, `ToolCall`, `ToolReport`, `Exit` | `src/tool.rs:90,22,76,31` | The execution port and its vocabulary |
| `Record`, `Step`, `StepId`, `RecordKey`, `Receipt`, `Refusal` | `src/record.rs:89,186,119,218,209,233` | The durable record, the step key, and the two answers to an append |
| `Task`, `Setting`, `Budget` | `src/task.rs:8,19,34` | The goal, its limits, and the ceilings |

# Owned state

Nothing durable. `Transcript` holds in-memory maps of turns, speeches, prompts, answers, calls, and
runs (`src/dialogue.rs:23-41`). Persistence arrives through the `Journal` port
(`src/model.rs:64-105`), and large bytes are addressed by an artifact hash
(`src/record.rs:28`).

# Invariants

- An intent is appended before the effect, and only `Receipt::Written` performs it. A
  `Receipt::Replayed` intent yields instead, at every move that appends
  (`src/advance.rs:145,164,176,188`).
- The fold refuses a content record with no admitting machine fact (`Floating`) and a second record
  under one key with different content (`Conflict`) (`src/dialogue.rs:183-190`).
- A call's command must equal the command the answer named (`src/dialogue.rs:376`).
- A speech record folded after an attempt was requested is `Late` (`src/dialogue.rs:295`), and
  `speak` is refused unless the fold is at `Begin` or `Await` (`src/speech.rs:40`).
- A step identifier is derived from the session and the step key as a version-5 UUID, so a repeated
  step replays (`src/record.rs:145`).
- Three ceilings halt the loop: tokens (`src/advance.rs:238`), turns (`src/advance.rs:275`), and
  attempts (`src/advance.rs:336`).- Every refusal returns before any mutation, so a refused record leaves the fold untouched.

# Dependencies

`mokmok-protocol`, `mokmok-state-machine`, `serde`, `serde_json`, `thiserror`, `tracing`, and `uuid`
with `v5` and `v7`. Development adds `proptest` and `tokio` for its macros and runtime.

# Reconciliation

The design names the modules `agent`, `session`, `turn`, `orchestrator`, `model`, `tool`, `context`,
`budget`, and `recovery`.[^architecture] The crate names them `advance`, `context`, `dialogue`,
`error`, `model`, `record`, `speech`, `task`, and `tool`. There is no `budget` module, and the
`Budget` type sits in `src/task.rs`.

There is no `recovery` module. Recovery lives in `mokmok-kernel/src/recovery.rs` and settles
in-flight executions from the durable log.

The design names `ToolExecutor` and persistence interfaces as the ports. The ports the crate
declares are `Provider`, `Journal`, and `Tools`. The name `SrtBackend` appears in no Rust file in
the workspace.

The design's `orchestrator` module has no counterpart. The kernel owns the loop that calls
`advance`, in `mokmok-kernel/src/agent.rs`.

Behaviour the design does not name: speech turns, the derived step identifier, the two byte caps on
speech and prose, the `Await` move, and the three budget fields.

# Tests

There is no `tests/` directory. Four `#[cfg(test)]` modules hold 61 cases and 1 proptest case.

`src/advance.rs:650` holds 18 cases for the begin, ask, act, and close walk, a replayed intent
performing no effect, the three ceilings, and the draining halt. `src/context.rs:226` holds 20
cases for byte accounting, budget trimming, an unknown outcome rendered as its warning, and a
byte-identical re-ask. `src/dialogue.rs:403` holds 18 cases and one proptest case for the floating,
late, and conflicting refusals and for refolding a history being invariant. `src/tool.rs:181` holds
5 cases for the exit-to-outcome map.

```
cargo test -p mokmok-agent-runtime
MUTANTS_PACKAGE=mokmok-agent-runtime ./scripts/mutants.sh --jobs 8
```

[^architecture]: Section 5.5 of `mokmok-os-architecture.md`.
[^crate]: The re-export list is `mokmok-agent-runtime/src/lib.rs:28-41`.

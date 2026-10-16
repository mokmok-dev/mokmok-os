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

Modules `advance`, `context`, `dialogue`, `error`, `find`, `model`, `record`, `speech`, `task`, and
`tool`.

| Item | Where | What it is |
| --- | --- | --- |
| `next`, `advance`, `turn_id`, `execution_id` | `src/advance.rs:94,124,406,418` | The decision, the step, and the two derived identifiers |
| `Move`, `Halt`, `Progress` | `src/advance.rs:24,63,78` | Begin, ask, act, find, close, await, halt; the five halt reasons; the step result |
| `Transcript` | `src/dialogue.rs:23` | `replay`, `apply`, `turn`, `newest_turn`, `holds_speech`, `call_of`, `find_of`, `newest_answer`, `observation` |
| `Provider`, `Journal` | `src/model.rs:55,70` | The two ports the runtime states for itself |
| `Finder`, `FindReport` | `src/find.rs:18,8` | The third port: the one tool that reads and changes nothing |
| `Action`, `Answer`, `Usage` | `src/model.rs:13,33,49` | A final answer, a command, a find, or malformed output |
| `Tools`, `ToolCall`, `ToolReport`, `Exit` | `src/tool.rs:90,22,76,31` | The execution port and its vocabulary |
| `Record`, `Step`, `StepId`, `RecordKey`, `Receipt`, `Refusal` | `src/record.rs:134,240,170,272,263,289` | The durable record, the step key, and the two answers to an append |
| `FindRecord`, `MAX_QUERY_BYTES`, `clip_query`, `clip_prose` | `src/record.rs:120,22,29,39` | One find's durable record, and the caps on the query it holds and the prose the model said |
| `Task`, `Setting`, `Budget` | `src/task.rs:8,19,40` | The goal, its limits, and the ceilings |
| `Context`, `ContextAct` | `src/context.rs:14,43` | The bytes the model is handed, and what one settled turn reads as |

# Owned state

Nothing durable. `Transcript` holds in-memory maps of turns, speeches, prompts, answers, calls,
finds, and runs (`src/dialogue.rs:23-43`). Persistence arrives through the `Journal` port
(`src/model.rs:66-107`), and large bytes are addressed by an artifact hash
(`src/record.rs:66`).

# Invariants

- An intent is appended before the effect, and only `Receipt::Written` performs it. A
  `Receipt::Replayed` intent yields instead, at every move that appends
  (`src/advance.rs:145,164,176,188`).
- The fold refuses a content record with no admitting machine fact (`Floating`) and a second record
  under one key with different content (`Conflict`), at the one dispatch every record passes,
  `Record::apply` (`src/dialogue.rs:206`).
- A call's command must equal the command the answer named (`src/dialogue.rs:372`), and a find's
  query must equal the query its answer named (`src/dialogue.rs:415`).
- A speech record folded after an attempt was requested is `Late` (`src/dialogue.rs:312`), and
  `speak` is refused unless the fold is at `Begin` or `Await` (`src/speech.rs:40`).
- A find commits in one transaction, and only one: the record and the turn's settlement share a
  step, so a crash before it re-derives the same move and searches again, because a find changes
  nothing (`src/advance.rs:120`).
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
declares are `Provider`, `Journal`, `Tools`, and `Finder`. The name `SrtBackend` appears in no Rust
file in the workspace.

The design's `orchestrator` module has no counterpart. The kernel owns the loop that calls
`advance`, in `mokmok-kernel/src/agent.rs`.

Behaviour the design does not name: speech turns, the derived step identifier, the two byte caps on
speech and prose, the `Await` move, the three budget fields, and the fourth port. The second tool is
not a design decision either: the design's `ToolExecutor` is one interface, and the tree gives it
one implementation that runs a command and one that reads an index.

# Tests

There is no `tests/` directory. Six `#[cfg(test)]` modules hold 91 cases, one of them a proptest.

`src/advance.rs:712` holds 30 cases for the begin, ask, act, find, and close walk, a replayed intent
performing no effect, an interrupted find re-deriving its step, the three ceilings, the draining
halt, and a line at the log's ceiling. `src/context.rs:271` holds 26 cases for byte accounting,
budget trimming, an unknown outcome rendered as its warning, a byte-identical re-ask, and the prompt
a granted root adds. `src/dialogue.rs:458` holds 23 cases and one proptest case for the floating,
late, and conflicting refusals, the query mismatch, and for refolding a history being invariant.
`src/tool.rs:181` holds 5 cases for the exit-to-outcome map, `src/record.rs:378` holds 6 for the
query and prose caps, and `src/task.rs:73` holds 1 for a task record written before the finding grant
existed.

```
cargo test -p mokmok-agent-runtime
MUTANTS_PACKAGE=mokmok-agent-runtime ./scripts/mutants.sh --jobs 8
```

[^architecture]: Section 5.5 of `mokmok-os-architecture.md`.
[^crate]: The re-export list is `mokmok-agent-runtime/src/lib.rs:29-45`.

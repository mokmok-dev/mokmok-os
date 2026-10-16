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
| `next`, `advance`, `turn_id`, `execution_id` | `src/advance.rs:100,130,470,482` | The decision, the step, and the two derived identifiers |
| `Move`, `Halt`, `Progress` | `src/advance.rs:26,69,84` | Begin, ask, act, find, edit, close, await, halt; the five halt reasons; the step result |
| `Transcript` | `src/dialogue.rs:23` | `replay`, `apply`, `turn`, `newest_turn`, `holds_speech`, `call_of`, `find_of`, `newest_answer`, `observation`, `shown_turns`, `shown` |
| `Provider`, `Journal` | `src/model.rs:60,75` | The two ports the runtime states for itself |
| `Finder`, `FindReport` | `src/find.rs:18,8` | The third port: the one tool that reads and changes nothing |
| `Editor`, `EditCall`, `Plan`, `Ready` | `src/edit.rs:80,11,19,28` | The edit port, one patch about to be written, and the two answers planning gives |
| `EditReport`, `EditOutcome` | `src/edit.rs:74,43` | What one edit produced: applied, stale, or refused |
| `Action`, `Answer`, `Usage` | `src/model.rs:13,38,49` | A final answer, a command, a find, an edit, or malformed output |
| `Tools`, `ToolCall`, `ToolReport`, `Exit` | `src/tool.rs:90,22,76,31` | The execution port and its vocabulary |
| `Record`, `Step`, `StepId`, `RecordKey`, `Receipt`, `Refusal` | `src/record.rs:134,298,222,330,321,349` | The durable record, the step key, and the two answers to an append |
| `FindRecord`, `MAX_QUERY_BYTES`, `clip_query`, `clip_prose` | `src/record.rs:140,22,35,59` | One find's durable record, and the caps on the query it holds and the prose the model said |
| `EditIntentRecord`, `EditOutcomeRecord`, `MAX_PATCH_BYTES`, `patch_refusal` | `src/record.rs:148,168,28,41` | One edit's two durable records, and the cap on the patch it carries |
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
  `Record::apply` (`src/dialogue.rs:230`).
- A call's command must equal the command the answer named (`src/dialogue.rs:372`), and a find's
  query must equal the query its answer named (`src/dialogue.rs:415`).
- A speech record folded after an attempt was requested is `Late` (`src/dialogue.rs:342`), and
  `speak` is refused unless the fold is at `Begin` or `Await` (`src/speech.rs:40`).
- A find commits in one transaction, and only one: the record and the turn's settlement share a
  step, so a crash before it re-derives the same move and searches again, because a find changes
  nothing (`src/advance.rs:120`).
- An edit spends two transactions, because it changes the world. The intent records the file's
  pre-image and post-image as content-addressed references before anything is written
  (`src/advance.rs:646`), so a crash between the two steps leaves a state the daemon can name rather
  than assume: recovery settles the execution `Unknown`, and the next prompt prints both hashes.
- A refusal decided before anything was written spends one transaction and records no intent
  (`src/advance.rs:733`), because nothing was about to happen. An edit's outcome still reaches the
  model through `ContextAct::Edited` when no intent stands beside it, so a refused edit never reads
  as a command that was not recorded.
- A step identifier is derived from the session and the step key as a version-5 UUID, so a repeated
  step replays (`src/record.rs:145`).
- Three ceilings halt the loop: tokens (`src/advance.rs:238`), turns (`src/advance.rs:275`), and
  attempts (`src/advance.rs:336`).
- Every refusal returns before any mutation, so a refused record leaves the fold untouched.
- `shown_turns` pairs each rendering with the turn it names (`src/context.rs:172`), which is what a
  front end draws a turn's own settlement, attempts, and answers beside; `shown` is its projection.
- An absent `act` means the turn being asked now, or a settled turn the model never finished: such
  a turn requested no tool, so no command ran and its prompt carries only the human's line
  (`src/context.rs:178`). A settled turn that did dispatch a command without a call record still
  renders `MISSING_COMMAND` (`src/context.rs:158`).

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
declares are `Provider`, `Journal`, `Tools`, `Finder`, and `Editor`. The name `SrtBackend` appears in
no Rust file in the workspace.

The design's `orchestrator` module has no counterpart. The kernel owns the loop that calls
`advance`, in `mokmok-kernel/src/agent.rs`.

Behaviour the design does not name: speech turns, the derived step identifier, the two byte caps on
speech and prose, the `Await` move, the three budget fields, and the fourth port. The other tools
are not design decisions either: the design's `ToolExecutor` is one interface, and the tree gives it
one implementation that runs a command, one that reads an index, and one that writes a file through
the kernel's own process.

# Tests

There is no `tests/` directory. Eight `#[cfg(test)]` modules hold 126 cases, two of them property
cases.

`src/advance.rs:925` holds 35 cases for the begin, ask, act, find, edit, and close walk, a replayed
intent performing no effect, an interrupted find re-deriving its step, a refused edit spending one
step, the three ceilings, the draining halt, and a line at the log's ceiling. `src/dialogue.rs:606`
holds 35 cases and one proptest for the floating, late, and conflicting refusals, the query and
patch mismatches, an edit outcome with no intent, and for refolding a history being invariant.
`src/context.rs:350` holds 33 cases for byte accounting, budget trimming, an unknown outcome
rendered as its warning, a byte-identical re-ask, the prompt a granted root or a writable checkout
adds, a rendering paired with the turn it names, a settled edit shown from its outcome, an
interrupted one shown from its intent, and a settled turn the model never finished carrying no act.
`src/record.rs:469` holds 8 cases for the query, prose, and patch caps and the two byte bounds the
log declares, and `src/edit.rs:149` holds 7 for the outcome-to-settlement map and the three
renderers. `src/tool.rs:183` holds 5 cases for the exit-to-outcome map, `src/speech.rs:106` holds 2
cases and one property case for the line bound in both directions, and `src/task.rs:74` holds 1 for a
task record written before the finding grant existed.

```
cargo test -p mokmok-agent-runtime
MUTANTS_PACKAGE=mokmok-agent-runtime ./scripts/mutants.sh --jobs 8
```

[^architecture]: Section 5.5 of `mokmok-os-architecture.md`.
[^crate]: The re-export list is `mokmok-agent-runtime/src/lib.rs:29-45`.

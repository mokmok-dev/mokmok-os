---
type: Crate
title: mokmok-tui
description: A terminal front end that drives the kernel's agent loop one bounded step at a time and renders a pure function of the durable transcript.
resource: ../../../mokmok-tui
tags: [mokmok-os, crate, interface]
status: draft
generated: { by: grok-build/grok-4.7, at: 2026-10-15T02:00:00Z }
stale_after: 2026-11-15T00:00:00Z
sources:
  - id: architecture
    resource: ../../../mokmok-os-architecture.md
    title: mokmok OS architecture and crate responsibilities
    author: codex/gpt-6
  - id: crate
    resource: ../../../mokmok-tui
    title: mokmok-tui sources
    last_modified: 2026-10-14T03:34:14Z
---

# Responsibility

Holds a conversation with the agent in a terminal. It steps the same kernel loop `mokmokd agent run`
drives, one bounded step per keystroke-triggered turn, and draws a pure function of the transcript
the kernel keeps.[^crate]

# Public surface

| Item | Where | What it is |
| --- | --- | --- |
| `core::run` | `src/core.rs:213` | The driver loop |
| `Engine` | `src/port.rs:15` | `fold`, `speak`, `step`, `reopen` |
| `Screen` | `src/port.rs:52` | `event`, `draw` |
| `Key`, `Event`, `Line`, `View` | `src/port.rs:73,90,103,259` | Input, the five line kinds, and the rendered rows |
| `Session` | `src/session.rs:15` | The kernel-backed engine |
| `Terminal` | `src/terminal.rs:22` | The terminal mode, its restoration, and the key reader |
| `Outcome` | `src/lib.rs:24` | The ending and the printed transcript |

# Owned state

Two in-memory fields, the input line and the scroll offset (`src/core.rs:16-18`). `Terminal` also
owns the terminal mode it entered, raw mode and the alternate screen, and restores both in its
`Drop` (`src/terminal.rs:60-64`). The crate holds no file, no directory, and no database. The log,
the artifact store, and the sandbox belong to the kernel.

# Invariants

- `Terminal::open` refuses when standard input or standard output is not a terminal, so a run in a
  pipe leaves the screen alone (`src/terminal.rs:34-38`).
- Raw mode and the alternate screen are left even on an early return (`src/terminal.rs:52-54`).
- A control or alt chord other than `Ctrl-C` is dropped instead of being typed as its letter
  (`src/terminal.rs:124-126`).
- Only a key press is the conversation's key; a release and a repeat are dropped
  (`src/terminal.rs:116-118`).
- An event the conversation does not act on is dropped where the terminal's report is classified,
  so the driver only ever sees a key or a resize (`src/terminal.rs:101-107`).
- An empty or whitespace-only line does not open a turn (`src/core.rs:161-163`).
- The typed line is capped at `MAX_SPEECH_BYTES` (`src/core.rs:150-151`).
- A yielded step, or a draining halt, reopens the ledger so recovery settles what is in flight
  before the next step (`src/core.rs:234-239`).
- A provider failure does not end the conversation: the turn spends its next attempt, and the
  attempt ceiling settles it failed (`src/core.rs:240-246`).
- A settled turn that renders no act is one the model never finished, and it draws `Line::Trouble`
  (`src/core.rs:83`) naming the reason its highest attempt answered with, or the attempts it spent
  without answering.
- The trouble row renders through `prose_rows` with the gutter `TROUBLE_GUTTER` (`src/port.rs:128`),
  so the frame and the plain replay show the same text.
- The library reads no clock; the binary reads one once and passes it in
  (`src/bin/mokmok-tui.rs:78-86`).
- The binary declares no flag of its own. It flattens the kernel's `Global` and `Run`, so every rule
  the kernel applies to a flag applies to a conversation, and `--config` and the provider keys reach
  the TUI through the kernel's own resolution (`src/bin/mokmok-tui.rs:45-46`).

# Dependencies

`clap`, `mokmok-agent-runtime`, `mokmok-kernel`, `mokmok-protocol`, `mokmok-state-machine`,
`ratatui`, `thiserror`, and `tokio` with `macros` and `rt-multi-thread`. Development adds
`mokmok-artifact`, `serde_json`, and `uuid`. The crate declares no rusqlite and no axum dependency.

# Reconciliation

The architecture document has no section for this crate. Section 7's crate table stops at
`mokmok-kernel`, and section 8 places a complex user interface outside the first milestone. The
facts in this document are observations of the code, and there is no target contract to reconcile
them against.

Two notes on the wiring. This crate is the only workspace member other than the kernel that depends
on `mokmok-kernel`, and it sits above it as an application. It is also the only place outside
`mokmok-kernel/src/bin/mokmokd.rs` that reads a clock.

Behaviour the document does not describe: the alternate-screen presentation with a plain replay
printed after the terminal is handed back (`src/bin/mokmok-tui.rs:61-70`), scrollback, `Ctrl-C` as
the only way out of a live session, `--steps` accepted and ignored, and the `Engine` port that lets
the loop be driven without a terminal (`src/port.rs:12-16`).

# Tests

45 cases: 5 in `tests/` and 40 across four `#[cfg(test)]` modules, two of them property cases.

Four of the five cases in `tests/` are asynchronous conversation cases.
One asserts that a conversation shows the line it answers and survives two exchanges
(`tests/conversation.rs:215`).
One asserts that a provider holding no answer settles turn 1 failed and leaves the conversation
open, with the human's line and a trouble line on screen (`tests/conversation.rs:303`).
One asserts that a line is refused while the turn is in flight (`tests/conversation.rs:370`).
One asserts that a find reaches the screen as its own line kind rather than as a command
(`tests/conversation.rs:398`).
One asserts that a binary with a pipe on each standard stream refuses the run and says why
(`tests/binary.rs:76`).

Four modules hold the rest.
One holds 14 cases and one property case for the empty-line guard, the byte ceiling of the typed
line, the status phrase of each mode, a settled turn the model never finished, a recorded call's
work row, a dispatched command with no call record, and an answer that went stale at a later
attempt (`src/core.rs:265`).
One holds 13 cases and one property case for gutters, multi-row replies, row-wise windowing, the
trouble row, a find row, escaped control characters, and the replay (`src/port.rs:294`).
One holds one case for a step another writer committed becoming visible after the engine reopens
the ledger (`src/session.rs:66`).
One holds 12 cases for the layout, scrollback, the key map, the press rule, the event
classification, the terminal check, and a find row (`src/terminal.rs:171`).

```
cargo test -p mokmok-tui
MUTANTS_PACKAGE=mokmok-tui ./scripts/mutants.sh --jobs 8
```

[^architecture]: `mokmok-os-architecture.md` carries no `mokmok-tui` section, so the only claim
    taken from it is the exclusion in section 8.
[^crate]: The re-export list is `mokmok-tui/src/lib.rs:17-20`.

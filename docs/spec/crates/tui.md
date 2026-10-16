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
| `core::run` | `src/core.rs:173` | The driver loop |
| `Engine` | `src/port.rs:15` | `fold`, `speak`, `step`, `reopen` |
| `Screen` | `src/port.rs:52` | `event`, `draw` |
| `Key`, `Event`, `Line`, `View` | `src/port.rs:73,90,99,250` | Input, the four line kinds, and the rendered rows |
| `Session` | `src/session.rs:15` | The kernel-backed engine |
| `Terminal` | `src/terminal.rs:22` | The terminal mode, its restoration, and the key reader |
| `Outcome` | `src/lib.rs:24` | The ending and the printed transcript |

# Owned state

Two in-memory fields, the input line and the scroll offset (`src/core.rs:13-16`). `Terminal` also
owns the terminal mode it entered, raw mode and the alternate screen, and restores both in its
`Drop` (`src/terminal.rs:57`). The crate holds no file, no directory, and no database. The log,
the artifact store, and the sandbox belong to the kernel.

# Invariants

- `Terminal::open` refuses when standard input or standard output is not a terminal, so a run in a
  pipe leaves the screen alone (`src/terminal.rs:34-38`).
- Raw mode and the alternate screen are left even on an early return (`src/terminal.rs:56-61`).
- A control or alt chord other than `Ctrl-C` is dropped instead of being typed as its letter
  (`src/terminal.rs:96-101`).
- An empty or whitespace-only line does not open a turn (`src/core.rs:122-127`).
- The typed line is capped at `MAX_SPEECH_BYTES` (`src/core.rs:110-116`).
- A yielded step, or a draining halt, reopens the ledger so recovery settles what is in flight
  before the next step (`src/core.rs:197-199`).
- The library reads no clock; the binary reads one once and passes it in
  (`src/bin/mokmok-tui.rs:81-88`).

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
printed after the terminal is handed back (`src/bin/mokmok-tui.rs:65-76`), scrollback, `Ctrl-C` as
the only way out of a live session, `--steps` accepted and ignored, and the `Engine` port that lets
the loop be driven without a terminal (`src/port.rs:12-16`).

# Tests

22 cases: 3 in `tests/conversation.rs` and 19 across three `#[cfg(test)]` modules.

All three conversation cases are asynchronous. One asserts that a conversation shows the line it
answers and survives two exchanges (`tests/conversation.rs:217`), one asserts that a line is refused
while the turn is in flight (`tests/conversation.rs:305`), and one asserts that a find reaches the
screen as its own line kind rather than as a command (`tests/conversation.rs:333`). `src/port.rs`
holds 10 cases for gutters, multi-row replies, row-wise windowing, and the replay.
`src/terminal.rs` holds 8 cases for the layout, scrollback, and control keys.

```
cargo test -p mokmok-tui
```

[^architecture]: `mokmok-os-architecture.md` carries no `mokmok-tui` section, so the only claim
    taken from it is the exclusion in section 8.
[^crate]: The re-export list is `mokmok-tui/src/lib.rs:17-20`.

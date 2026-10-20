---
type: Crate
title: mokmok-tui
description: A terminal front end that is an independent producer and consumer of the event bus, holding as many sessions as the human opens and folding the daemon's record stream.
resource: ../../../mokmok-tui
tags: [mokmok-os, crate, interface]
status: draft
generated: { by: grok-build/grok-4.7, at: 2026-10-19T18:00:00Z }
stale_after: 2026-11-19T00:00:00Z
sources:
  - id: architecture
    resource: ../../../mokmok-os-architecture.md
    title: mokmok OS architecture and crate responsibilities
    author: codex/gpt-6
  - id: crate
    resource: ../../../mokmok-tui
    title: mokmok-tui sources
    last_modified: 2026-10-19T18:00:00Z
---

# Responsibility

Holds a conversation with the agent in a terminal, and holds as many of them at once as the human
opens or adopts. The crate is a bus client: it publishes what the human types and folds the records
the daemon commits, so the daemon owns the provider, the grants, the log, and the step. The screen
reads keys, draws the cached folds, and never opens a driver, reads `events.sqlite`, or resolves a
configuration.

# Public surface

| Item | Where | What it is |
| --- | --- | --- |
| `run` | `src/core.rs:972` | The driver loop over every session the process holds |
| `Engine` | `src/port.rs:20` | One session's world: `session`, `fold`, `speak`, `step`, `step_watched`, `reopen`, `busy`, `drain` |
| `Update`, `Link` | `src/port.rs:100,117` | What a remote step reported, and whether the bus is answering |
| `Roster` | `src/port.rs:140` | The daemon's sessions: `listed`, `resume`, `create` |
| `Screen` | `src/port.rs:176` | `poll`, `event`, `draw` |
| `Listing`, `Key`, `Event`, `Line` | `src/port.rs:128,204,229,242` | What the daemon lists of one session, the keys, and the seven line kinds |
| `Span`, `Row`, `View`, `Typing`, `Entry` | `src/port.rs:418,439,469,500,531` | How one drawn span reads, one span, one row, the frame, the typed line, and one listing entry |
| `clipped` | `src/port.rs:1031` | The display-width clip a heading, a status, and a goal share |
| `BusRoster` | `src/bus.rs:78` | The bus-backed roster, its thread, and its shared cache |
| `BusRoster::attach`, `start` | `src/bus.rs:95,138` | The handshake before the screen exists, and the one blocking open |
| `RemoteEngine` | `src/bus.rs:210` | One session's fold cache and its channel to the bus thread |
| `Terminal` | `src/terminal.rs:30` | The terminal mode, its restoration, and the key reader |
| `Outcome`, `Left` | `src/lib.rs:32,39` | What each held session left behind, and how it stands |

# Owned state

Per session, the fold cache the bus thread keeps current, the line being typed, how far back the
transcript is scrolled, the prose a step is streaming, the last trouble phrase, and the link state
(`src/bus.rs:210`, `src/core.rs:25`). Per process, which session the human is in, the log's listing
as it was when he opened the pane, at most one open pane, the keys typed while a step held the loop,
and the spinner phase (`src/core.rs:68`). The bus thread owns one `Client`, one reader task per
subscription, the roster cache it shares with the screen thread, and the request channel
(`src/bus.rs:452`). The crate holds no durable file, directory, database, or subscription checkpoint
of its own; a restart re-bootstraps from the daemon.

# Invariants

- The crate declares no `mokmok-kernel` dependency, and no TUI code path constructs a `Driver`,
  resolves a `Config`, or opens the event log. `BusRoster` and `RemoteEngine` are the only adapters
  (`src/bus.rs:78`, `src/bus.rs:210`).
- One `SessionCommit` is one ledger transaction, and its records fold through the same
  `Transcript::apply` the daemon folds (`src/bus.rs:327`). A repeated commit folds as
  `Ack::Duplicate` and draws no second line.
- A history replacement is a full replay. Chunks are assembled in index order and the fold is
  replaced only on the chunk that says it is last, so a retained-window lapse costs a read rather
  than a hole (`src/bus.rs:339`).
- A command is published once. `speak` sends `Command::Speak` and marks a dispatch pending
  (`src/bus.rs:407`). A command whose acknowledgement is lost is not retried; the record stream
  says whether it landed, and the daemon makes a redelivered `Speak` a replay through its
  request-derived step.
- `busy` is true while a command is pending or while the fold carries a task the daemon still owes
  work for (`src/bus.rs:431`). The core therefore never asks a remote session to step locally.
- `step` answers `Progress::Awaiting` and performs nothing (`src/bus.rs:423`). The daemon steps.
- The screen thread never awaits the bus. `fold` clones the cached transcript, `drain` is
  `try_recv`, and every request is an unbounded send (`src/bus.rs:444`).
- The bus thread owns the socket and gives each subscription its own reader task. A
  `Subscription::next` future is never cancelled and asked again, because the server keeps the
  parked pull and refuses the second one (`src/bus.rs:452`, `src/bus.rs:624`).
- A refusal or a trouble phrase is a session's own news and draws as `Line::Trouble`; the process
  keeps the other sessions and the keys (`src/core.rs:526`).
- A session the daemon refuses leaves an empty fold. The engine reports it as not busy, so the core
  neither spins on it nor hands it to the local step path (`src/bus.rs:431`).
- The binary reads one clock value and mints a created session's identity from it
  (`src/bin/mokmok-tui.rs:115`). The binary declares its own flags and names the bus, the session,
  the goal, and the checkout; it names no provider and no grant (`src/bin/mokmok-tui.rs:33`).
- The frame is a heading, a rule, the body, a composer, and a status row; the rule disappears below
  ten rows and the status row below five (`src/terminal.rs`). The rules are unchanged from the
  in-process front end: every row wraps to display width, the typed line wraps and places the
  cursor, the listing drops the full identity for eight characters, and a control chord other than
  `Ctrl-C`, `Ctrl-N`, and `Ctrl-O` is dropped (`src/port.rs`).
- `Terminal::open` refuses when standard input or output is not a terminal (`src/terminal.rs:42`).

# Dependencies

`clap`, `mokmok-agent-runtime`, `mokmok-event-bus`, `mokmok-protocol`, `mokmok-session-wire`,
`mokmok-state-machine`, `ratatui`, `serde`, `thiserror`, `tokio`, and `unicode-width`.
Development adds `mokmok-artifact`, `nix`, `proptest`, and `serde_json`. The crate declares no
`mokmok-kernel` and no `rusqlite` dependency.

# Reconciliation

The architecture document has no section for this crate. Section 7's crate table stops at
`mokmok-kernel`, and section 8 places a complex user interface outside the first milestone. The
facts here are observations of the code, and there is no target contract to reconcile them against.

The change from the in-process front end is the point of the crate's current shape. The kernel
dependency, the per-session `Worker`, and the kernel-backed `Session`/`Sessions` adapter are gone.
The TUI produces commands and consumes records, history replacements, roster revisions, and
streaming prose over one bus connection. The daemon's `Host` owns the `Driver`; its `Slot` thread
runs one bounded step at a time for that session.

# Tests

Two integration cases and 128 unit cases. `tests/bus.rs` starts a real `mokmokd serve` with a
scripted provider and a scratch bus, creates a session through `BusRoster`, speaks a line, and
asserts the folded answer. The second case drops the terminal's engine, attaches a new one with the
same session identity, and asserts the transcript came from the daemon rather than from the first
process. The unit cases keep the frame, the core rotation, the markdown, the terminal, and the bus
engine's fold state machine. The bus engine's cases cover a commit folding, a duplicate commit
drawing one fold, a refusal and a link update reaching the screen, a full history replacement, a
commit queued during the history and applied after it, and a seeded task visible before its record.

```
cargo test -p mokmok-tui
MUTANTS_PACKAGE=mokmok-tui ./scripts/mutants.sh --jobs 8
```

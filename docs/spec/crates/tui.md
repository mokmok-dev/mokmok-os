---
type: Crate
title: mokmok-tui
description: A terminal front end that holds as many agent sessions as the human opens, steps the ones behind the screen, and renders a pure function of their durable transcripts.
resource: ../../../mokmok-tui
tags: [mokmok-os, crate, interface]
status: draft
generated: { by: grok-build/grok-4.7, at: 2026-10-16T07:30:00Z }
stale_after: 2026-11-16T00:00:00Z
sources:
  - id: architecture
    resource: ../../../mokmok-os-architecture.md
    title: mokmok OS architecture and crate responsibilities
    author: codex/gpt-6
  - id: crate
    resource: ../../../mokmok-tui
    title: mokmok-tui sources
    last_modified: 2026-10-16T07:20:00Z
---

# Responsibility

Holds a conversation with the agent in a terminal, and holds as many of them at once as the human
opens or adopts. It steps the same kernel loop `mokmokd agent run` drives, one bounded step of
whichever session's turn it is, and draws a pure function of the transcripts the kernel keeps. A
session the human is not looking at still moves, between his keystrokes.[^crate]

# Public surface

| Item | Where | What it is |
| --- | --- | --- |
| `run` | `src/core.rs:657` | The driver loop over every session the process holds |
| `Engine` | `src/port.rs:17` | One session's world: `session`, `fold`, `speak`, `step`, `reopen` |
| `Roster` | `src/port.rs:71` | The log's sessions: `listed`, `resume`, `create` |
| `Screen` | `src/port.rs:107` | `poll`, `event`, `draw` |
| `Listing`, `Key`, `Event`, `Line` | `src/port.rs:59,135,160,173` | What the log holds of one session, the keys, and the five line kinds |
| `View`, `Sheet`, `Row` | `src/port.rs:358,382,399` | The frame, the pane over the transcript, and one row of the listing |
| `Session` | `src/session.rs:15` | The kernel-backed engine |
| `Sessions` | `src/session.rs:76` | The kernel-backed roster |
| `Terminal` | `src/terminal.rs:28` | The terminal mode, its restoration, and the key reader |
| `Outcome`, `Left` | `src/lib.rs:27,34` | What each held session left behind, and how it stands |

# Owned state

Per session, the engine that runs it, the line being typed in it, and how far back its transcript is
scrolled (`src/core.rs:19-24`). Per process, which session the human is in, the log's own listing as
it was when he opened the pane, and at most one open pane (`src/core.rs:66-74`). `Terminal` also owns
the terminal mode it entered, raw mode and the alternate screen, and restores both in its `Drop`
(`src/terminal.rs:66-71`). The crate holds no file, no directory, and no database. The log, the
artifact store, and the sandbox belong to the kernel.

# Invariants

- `Terminal::open` refuses when standard input or standard output is not a terminal, so a run in a
  pipe leaves the screen alone (`src/terminal.rs:40-44`).
- Raw mode and the alternate screen are left even on an early return (`src/terminal.rs:57-61`).
- A control or alt chord other than `Ctrl-C`, `Ctrl-N`, and `Ctrl-O` is dropped instead of being
  typed as its letter (`src/terminal.rs:129-151`).
- Only a key press is one of the conversation's keys; a release and a repeat are dropped
  (`src/terminal.rs:130-132`).
- An event the conversation does not act on is dropped where the terminal's report is classified,
  so the driver only ever sees a key or a resize (`src/terminal.rs:115-119`).
- A step is taken only when the terminal reports nothing, so a keystroke is answered before another
  session moves (`src/core.rs:618-649`).
- The rotation starts at the session on screen, so it moves first and the ones behind it take their
  turns in between the human's keystrokes (`src/core.rs:350-359`).
- A session this process already holds is focused instead of opened a second time: two engines on one
  session would make the second one's recovery report the first one's work in flight as unknown
  (`src/core.rs:400-421`).
- The process ends when every session it holds has ended, or when the human leaves
  (`src/core.rs:174-176`, `src/core.rs:544-565`).
- An empty or whitespace-only line does not open a turn (`src/core.rs:423-446`).
- The typed line, and the goal a session is named with, are capped at `MAX_SPEECH_BYTES`, and a
  character that does not fit whole is dropped (`src/core.rs:488-546`).
- A yielded step, or a draining halt, reopens the ledger so recovery settles what is in flight
  before the next step (`src/core.rs:608-627`).
- A provider failure does not end the conversation: the turn spends its next attempt, and the
  attempt ceiling settles it failed (`src/core.rs:622-627`).
- A settled turn that renders no act is one the model never finished, and it draws `Line::Trouble`
  (`src/core.rs:88-124`) naming the reason its highest attempt answered with, or the attempts it
  spent without answering.
- The trouble row renders through `prose_rows` with the gutter `TROUBLE_GUTTER` (`src/port.rs:198`),
  so the frame and the plain replay show the same text.
- The listing draws the sessions this process holds first and the ones only the log holds after them,
  and a row's status is the phrase the header shows when the process holds that session and a phrase
  for its folded phase when it does not (`src/core.rs:300-331`).
- A session whose log holds a history and no task in it is refused every way in. Its row is marked
  `(no task recorded)` and choosing it does nothing (`src/core.rs:171-182`, `src/core.rs:512`), and
  both `Sessions::start` and `Sessions::run_for` answer [`Error::Untasked`] before the session is
  opened (`src/session.rs:98-124`). The runtime continues a session under the task its log recorded
  and has no other goal, so opening one would carry the command line's goal into a session nobody
  asked that of. Its history is read with `mokmokd status`.
- The library reads no clock; the binary reads one once and passes it in, and every session it opens
  or mints takes that one reading (`src/bin/mokmok-tui.rs:91-99`).
- The binary declares no flag of its own. It flattens the kernel's `Global` and `Run`, so every rule
  the kernel applies to a flag applies to a conversation, and `--config` and the provider keys reach
  the TUI through the kernel's own resolution, which reads the process environment once into
  `ProcessEnvironment` and hands it in (`src/bin/mokmok-tui.rs:53-55`).

# Dependencies

`clap`, `mokmok-agent-runtime`, `mokmok-kernel`, `mokmok-protocol`, `mokmok-state-machine`,
`ratatui`, `thiserror`, and `tokio` with `macros` and `rt-multi-thread`. Development adds
`mokmok-artifact`, `nix`, `proptest`, `serde_json`, and `uuid`. The crate declares no rusqlite and no
axum dependency, and it reaches the log through `mokmok-kernel`'s `sessions` rather than by opening
the file itself.

# Reconciliation

The architecture document has no section for this crate. Section 7's crate table stops at
`mokmok-kernel`, and section 8 places a complex user interface outside the first milestone. The
facts in this document are observations of the code, and there is no target contract to reconcile
them against.

Three notes on the wiring. This crate is the only workspace member other than the kernel that depends
on `mokmok-kernel`, and it sits above it as an application. It is also the only place outside
`mokmok-kernel/src/bin/mokmokd.rs` that reads a clock. And a session it adopts is resumed from the
task that session recorded, so a session this process was never told about still runs under the goal
and the checkout it was given.

Behaviour the document does not describe: the alternate-screen presentation with a plain replay
printed after the terminal is handed back (`src/bin/mokmok-tui.rs:57-77`), scrollback, `Ctrl-C` as
the only way out of a live process, `--steps` accepted and ignored, and the `Engine` and `Roster`
ports that let the loop be driven without a terminal (`src/port.rs:17-104`).

One limit is deliberate. Background progress is cooperative: the loop steps a session between the
human's keystrokes, and a step runs to completion, so a keystroke waits for the step in flight the
way a single-session conversation already did. Nothing here runs a step on another thread or in
another process.

# Tests

90 cases: 9 in `tests/` and 81 across four `#[cfg(test)]` modules, two of them property cases.

Six of the nine cases in `tests/` are asynchronous conversation cases.
One asserts that a conversation shows the line it answers and survives two exchanges.
One asserts that a provider holding no answer settles turn 1 failed and leaves the conversation open.
One asserts that a line is refused while the turn is in flight.
One asserts that a find reaches the screen as its own line kind.
One asserts that a session the human left keeps moving while he is in another, and that a frame in
the new session reports the one working in the background (`tests/conversation.rs:530`).
One asserts that the listing shows a session only the log holds and that choosing it opens it
(`tests/conversation.rs:608`).
Three are binary cases: one asserts that a pipe on each standard stream refuses the run and says why,
one drives a conversation on a pseudo-terminal through a frame and back to the shell, and one asserts
that a frame drawn while a turn is in flight, the listing, and the prompt that names a goal all reach
a real terminal (`tests/binary.rs:374`).

Four modules hold the rest.
One holds 39 cases and one property case for the trouble row, the work row, a dispatched command with
no call record, the empty-line guard, the byte ceiling of the typed line and of a named goal, the
status phrase of each mode, the rotation and its order, the listing's rows and cursor, adopting and
creating a session, each pane's keys, and the frame the manager builds (`src/core.rs:681`).
One holds 19 cases and one property case for gutters, multi-row replies, row-wise windowing, the
trouble row, a find row, an edit row, escaped control characters, the replay, and the rows of the
listing (`src/port.rs:497`).
One holds 7 cases for a step another writer committed becoming visible after the engine reopens the
ledger, the goal and the checkout the log holds being the ones a resumed session runs under, a
session with no task being refused, a created session running under the goal the human typed, and the
listing mapping onto what the kernel holds (`src/session.rs:194`).
One holds 16 cases for the layout, scrollback, the key map, the press rule, the event
classification, the terminal check, a find row, and the two panes (`src/terminal.rs:208`).

```
cargo test -p mokmok-tui
MUTANTS_PACKAGE=mokmok-tui ./scripts/mutants.sh --jobs 8
```

[^architecture]: `mokmok-os-architecture.md` carries no `mokmok-tui` section, so the only claim
    taken from it is the exclusion in section 8.
[^crate]: The re-export list is `mokmok-tui/src/lib.rs:20-23`.

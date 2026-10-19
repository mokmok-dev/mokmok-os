---
type: Crate
title: mokmok-tui
description: A terminal front end that holds as many agent sessions as the human opens, steps the ones behind the screen, and draws their durable transcripts plus the answer a focused step is streaming.
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
opens or adopts. Each session's world runs on a worker thread of its own, so a step that blocks
holds only that session while the screen keeps reading keys and drawing what the step reports. It
steps the same kernel loop `mokmokd agent run` drives, one bounded step of whichever session's turn
it is, and draws the durable transcripts, with the answer a focused step is streaming appended until
the turn settles.[^crate]

# Public surface

| Item | Where | What it is |
| --- | --- | --- |
| `run` | `src/core.rs:915` | The driver loop over every session the process holds |
| `Engine` | `src/port.rs:19` | One session's world: `session`, `fold`, `speak`, `step`, `step_watched`, `reopen`, `busy`, `drain` |
| `Update` | `src/port.rs:99` | What a step working elsewhere reported: a fragment, its result, or a failure |
| `Roster` | `src/port.rs:126` | The log's sessions: `listed`, `resume`, `create` |
| `Screen` | `src/port.rs:162` | `poll`, `event`, `draw` |
| `Listing`, `Key`, `Event`, `Line` | `src/port.rs:114,190,215,228` | What the log holds of one session, the keys, and the seven line kinds, including a delegation's child, goal, and reply |
| `Tone`, `Span`, `Row` | `src/port.rs:381,404,425` | How one drawn span reads, one span, and one drawn row with its spans and selection |
| `View`, `Typing`, `Sheet`, `Entry` | `src/port.rs:455,486,500,517` | The frame, the typed line laid out for a width, the pane over the transcript, and one entry of the listing |
| `clipped` | `src/port.rs:1013` | The display-width clip a heading, a status, and a goal share |
| `Session` | `src/session.rs:16` | The kernel-backed engine |
| `Sessions` | `src/session.rs:84` | The kernel-backed roster, which opens every session on a worker |
| `Worker` | `src/worker.rs:32` | The thread one session's world runs on, and the cached fold it reports |
| `Terminal` | `src/terminal.rs:35` | The terminal mode, its restoration, and the key reader |
| `Outcome`, `Left` | `src/lib.rs:33,40` | What each held session left behind, and how it stands |

# Owned state

Per session, the engine that runs it, the line being typed in it, how far back its transcript is
scrolled, and the prose its step has streamed (`src/core.rs:23-29`). Per process, which session the
human is in, the log's own listing as it was when he opened the pane, at most one open pane, the
keys typed while an engine run on this thread held the loop, and the spinner phase
(`src/core.rs:60-72`). A worker owns the
command and message channels, the last fold the engine reported, and whether a command is in flight
(`src/worker.rs:32-47`). `Terminal` also owns the terminal mode it entered, raw mode and the alternate
screen, and restores both in its `Drop` (`src/terminal.rs:66-71`). The crate holds no durable file,
directory, or database of its own. The log, the artifact store, the sandbox, and the provider belong
to the kernel.

# Invariants

- `Terminal::open` refuses when standard input or standard output is not a terminal, so a run in a
  pipe leaves the screen alone (`src/terminal.rs:47-51`).
- Raw mode and the alternate screen are left even on an early return.
- Every drawn row is wrapped to the pane's width before it is windowed, and the width is display
  width, so a wide character counts as two columns (`src/port.rs:1076-1099`). A break takes the last
  space that fits and falls inside a word wider than a row.
- The typed line wraps the same way and the input area takes the rows it occupies. The cursor is
  placed after the last character, or at the start of a fresh row when that character fills the last
  column, so it never leaves the pane (`src/port.rs:1118-1185`).
- A line kind carries a prefix, a continuation prefix of the same width, and a semantic tone; the
  adapter maps a tone to a style and chooses no wording or count (`src/port.rs:381-429`).
- The port draws only inline code, bold, headings, and fenced blocks from agent prose; a fence line
  itself draws nothing and a block line draws behind a bar (`src/markdown.rs:17-60`).
- The frame is a heading, a rule, the body, a composer, and a status row; the rule disappears below
  ten rows and the status row below five (`src/terminal.rs:191-240`).
- The status row prefixes a braille spinner exactly when `View::spinner` is `Some`, and the phase
  advances only while the session on screen is the one moving (`src/core.rs:364-373`).
- The hints and the placeholder are chosen by the core for the open pane and the mode, so the
  adapter cannot invent a key or a phrase.
- An empty transcript draws a welcome naming the goal and the checkout, and drops rows from the
  bottom when the pane is short (`src/port.rs:1098-1131`).
- The listing drops the full identity for eight characters, pads its status column, marks the
  cursor's entry selected, and says how many sessions the process holds (`src/port.rs:1135-1230`).
- The status row drops its hints whole when they do not fit beside the phrase, rather than showing
  half a shortcut (`src/terminal.rs:398-419`).
- A control or alt chord other than `Ctrl-C`, `Ctrl-N`, and `Ctrl-O` is dropped instead of being
  typed as its letter (`src/terminal.rs:129-151`).
- Only a key press is one of the conversation's keys; a release and a repeat are dropped
  (`src/terminal.rs:130-132`).
- An event the conversation does not act on is dropped where the terminal's report is classified,
  so the driver only ever sees a key or a resize (`src/terminal.rs:115-119`).
- A worker opens with the fold it was handed, runs the engine's step on its own thread, and returns
  from the dispatch at once (`src/worker.rs:50-74`, `src/worker.rs:83-118`).
- A step that blocks inside its engine holds its own worker, not the screen: the loop reads keys and
  draws the fragments already reported (`src/core.rs:695-776`, `src/core.rs:878-906`).
- A session that can move is dispatched before another session's step, and while every session that
  can move is working elsewhere the loop polls keys and waits one frame (`src/core.rs:878-906`).
- A fragment belongs to the screen until its step returns; the fold the worker reports then carries
  the same prose, and the streamed copy is cleared (`src/core.rs:415-446`).
- A line spoken, or a reopen, is in flight until its fold comes back; the rotation does not dispatch
  over it (`src/worker.rs:93-118`, `src/core.rs:396-408`).
- Each pass yields before it drains, so a driver whose step answered without yielding stays
  cancellable (`src/core.rs:849-855`).
- The rotation starts at the session on screen, so it moves first and the ones behind it take their
  turns in between the human's keystrokes (`src/core.rs:396-408`).
- A session this process already holds is focused instead of opened a second time: two engines on one
  session would make the second one's recovery report the first one's work in flight as unknown
  (`src/core.rs:493-514`).
- The process ends when every session it holds has ended, or when the human leaves
  (`src/core.rs:230-234`, `src/core.rs:667-689`).
- An empty or whitespace-only line does not open a turn (`src/core.rs:516-578`).
- The typed line, and the goal a session is named with, are capped at `MAX_SPEECH_BYTES`, and a
  character that does not fit whole is dropped (`src/core.rs:516-578`).
- A yielded step, or a draining halt, reopens the ledger so recovery settles what is in flight
  before the next step (`src/core.rs:415-446`).
- A provider failure does not end the conversation: the turn spends its next attempt, and the
  attempt ceiling settles it failed (`src/core.rs:415-446`).
- A settled turn that renders no act is one the model never finished, and it draws `Line::Trouble`
  (`src/core.rs:86-166`) naming the reason its highest attempt answered with, or the attempts it
  spent without answering.
- The trouble row renders through `prose_rows` with the gutter `TROUBLE_GUTTER`
  (`src/port.rs:368,254`), so the frame and the plain replay show the same text.
- The listing draws the sessions this process holds first and the ones only the log holds after them,
  and a row's status is the phrase the header shows when the process holds that session and a phrase
  for its folded phase when it does not (`src/core.rs:320-379`).
- A session whose log holds a history and no task in it is refused every way in. Its row is marked
  `(no task recorded)` and choosing it does nothing (`src/core.rs:171-205`, `src/core.rs:612`), and
  both `Sessions::start` and `Sessions::run_for` answer [`Error::Untasked`] before the session is
  opened (`src/session.rs:106-159`). The runtime continues a session under the task its log recorded
  and has no other goal, so opening one would carry the command line's goal into a session nobody
  asked that of. Its history is read with `mokmokd status`.
- The library reads no clock; the binary reads one once and passes it in, and every session it opens
  or mints takes that one reading (`src/bin/mokmok-tui.rs:91-99`).
- The binary declares no flag of its own. It flattens the kernel's `Global` and `Run`, so every rule
  the kernel applies to a flag applies to a conversation, and `--config` and the provider keys reach
  the TUI through the kernel's own resolution, which reads the process environment once into
  `ProcessEnvironment` and hands it in (`src/bin/mokmok-tui.rs:71`).
- A bare start fills only the flags the operator did not name (`src/bin/mokmok-tui.rs:107`). It
  mints a session, takes a default goal, anchors the data directory, checkout, search root, and
  write grant at the canonical working directory, and discovers skills under the configuration
  directory and `.mokmok/skills`. The data directory becomes absolute because the confiner does not
  share this process's working directory and opens the plan beside the sandbox root.
- A delegation reaches the screen as `Line::Delegate`: the goal, the child session identity, and the
  child's reply. The child's own find, edit, and command lines stay in its transcript, which the
  listing opens like any other session (`src/core.rs:110`, `src/port.rs:227`).

# Dependencies

`clap`, `mokmok-agent-runtime`, `mokmok-kernel`, `mokmok-protocol`, `mokmok-state-machine`,
`ratatui`, `thiserror`, `tokio` with `macros`, `rt-multi-thread`, and `time`, and `unicode-width`,
which measures every row and the composer's cursor. Development adds
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
ports that let the loop be driven without a terminal (`src/port.rs:17-159`).

One boundary is deliberate. A session's world is opened on the screen thread before its worker
starts, because opening reads the log, the provider, and the sandbox and the caller has to be told
when that fails; that one call can wait. A step never does: it runs on the session's worker. A worker
is left to finish what it holds when the process leaves, and a step cut off there is a crash the log
already covers.

# Tests

143 cases: 14 in `tests/` and 129 across seven `#[cfg(test)]` modules, two of them property cases.

Six of the fourteen cases in `tests/` are asynchronous conversation cases.
One asserts that a conversation shows the line it answers and survives two exchanges.
One asserts that a provider holding no answer settles turn 1 failed and leaves the conversation open.
One asserts that a line is refused while the turn is in flight.
One asserts that a find reaches the screen as its own line kind.
One asserts that a session the human left keeps moving while he is in another, and that a frame in
the new session reports the one working in the background (`tests/conversation.rs:545`).
One asserts that the listing shows a session only the log holds and that choosing it opens it
(`tests/conversation.rs:623`).
Eight are binary cases: one asserts that a pipe on each standard stream refuses the run and says why,
one drives a conversation on a pseudo-terminal through a frame and back to the shell, one submits a
line wider than the pane and asserts that its tail reaches the screen
(`tests/binary.rs:413`), one types three wide characters and asserts that the cursor sits past the
six columns they occupy (`tests/binary.rs:455`), one asserts
that a frame drawn while a turn is in flight, the listing, and the prompt that names a goal all reach
a real terminal (`tests/binary.rs:492`), one serves a held server-sent stream and asserts that the
first fragment is drawn before the second is sent, that a key typed during the stream echoes, and
that the rest of the answer follows in order (`tests/binary.rs:806`), one runs a command that sleeps
and asserts that a key typed while it runs is drawn before the answer
(`tests/binary.rs:741`), and one starts with no session, goal, checkout, grant, search root, or skill
root and asserts that the current checkout, the configured skill, and the reply all reach the
replay. The command case returns early when the sandbox's confiner is absent, which is the shape of a
package-scoped build.

Six modules hold the rest. The binary holds 4 cases for a bare start's minted session and anchored
paths, an explicit flag surviving the defaults, the path guards skipping a directory the working
directory already covers, and the shell rule.
One holds 51 cases and one property case for the trouble row, the work row, a dispatched command with
no call record, the empty-line guard, the byte ceiling of the typed line and of a named goal, the
status phrase of each mode, the one-session phrase, the hints and placeholder of each pane, the
spinner phase advancing only while the session on screen works, the rotation and its order, the
listing's entries and cursor, adopting and creating a session, each pane's keys, the frame the
manager builds, a streamed fragment being drawn before the next arrives, a stream drawing once per
fragment, a key typed during a stream reaching the line, a deferred key waiting for the step and
being answered in order, a background stream drawing nothing, a pane covering the session keeping the
stream off the screen, a terminal that fails while a stream is read or drawn, a key drawn while a
worker's step is parked, and an interrupt answered while a session can move leaving
(`src/core.rs:975`).
One holds 34 cases and one property case for wrapping at display width, a wide character, a word
wider than the line, the gutters and tones of each line kind, the markdown spans and fences, row-wise
windowing, the welcome, the listing's entries and cursor, the typed line's own wrapping and cursor,
escaped control characters, and the replay (`src/port.rs:1333`).
One holds 6 cases for plain prose, bold, inline code, an unclosed marker, a heading, and a fenced
block (`src/markdown.rs:170`).
One holds 7 cases for a step another writer committed becoming visible after the engine reopens the
ledger, the goal and the checkout the log holds being the ones a resumed session runs under, a
session with no task being refused, a created session running under the goal the human typed, and the
listing mapping onto what the kernel holds (`src/session.rs:203`).
One holds 23 cases for the layout, the rule, the heading and its truncation, a wrapped body,
scrollback, the composer's prompt, placeholder, wrapped input, and cursor, the spinner and the
hints, the key map, the press rule, the event classification, the terminal check, a find row, and
the two panes (`src/terminal.rs:404`).
One holds 4 cases for a step's fragments and result, a parked step leaving the screen thread free, a
spoken line's fold ending its flight, and a reopen reaching the engine (`src/worker.rs:213`).

```
cargo test -p mokmok-tui
MUTANTS_PACKAGE=mokmok-tui ./scripts/mutants.sh --jobs 8
```

[^architecture]: `mokmok-os-architecture.md` carries no `mokmok-tui` section, so the only claim
    taken from it is the exclusion in section 8.
[^crate]: The re-export list is `mokmok-tui/src/lib.rs:24-26`.

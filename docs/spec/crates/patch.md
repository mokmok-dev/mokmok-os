---
type: Crate
title: mokmok-patch
description: A unified diff read into a structure whose hunk headers are recomputed from the body and whose hunks are applied forward from the line they state, with no file opened.
resource: ../../../mokmok-patch
tags: [mokmok-os, crate, patch]
status: draft
generated: { by: grok-build/grok-4.7, at: 2026-10-16T00:00:00Z }
stale_after: 2026-11-16T00:00:00Z
sources:
  - id: crate
    resource: ../../../mokmok-patch
    title: mokmok-patch sources
    last_modified: 2026-10-16T00:00:00Z
---

# Responsibility

Reads one unified diff into a `Patch`, recomputes every hunk header's counts from the hunk body,
and applies one file section to bytes the caller already holds. The crate opens no file, reads no
clock, and reaches no socket: a caller owns every effect, and the bytes it hands in and gets back
are the only ones that move.[^crate]

It reads a diff as a model writes one rather than as a repository writes one. A header's counts are
arithmetic a model can get wrong, so they are recomputed rather than refused, and
`Patch::to_unified` renders the numbers that were used. A header's line numbers are an anchor a
model can get wrong, so `FilePatch::apply` looks for the hunk's old side at the stated line and then
forward to the end of the file, recording the distance in `Placed::Matched::offset` rather than
moving the text somewhere it was not asked to go. A hunk whose body matches nowhere fails rather
than guesses.

The design names no section for this crate. It is the fifteenth member and the third one the plan
does not carry, beside [`mokmok-tui`](tui.md) and [`mokmok-search`](search.md).

# Public surface

One module, `src/lib.rs`, with no `mod` inside it.

| Item | Where | What it is |
| --- | --- | --- |
| `Patch`, `FilePatch` | `src/lib.rs:19,26` | One diff, and one file's section of it |
| `Label` | `src/lib.rs:37` | Which file a side names, with `/dev/null` as `Absent` |
| `Hunk`, `Header` | `src/lib.rs:46,56` | One hunk and the four numbers its `@@` line stated |
| `Line`, `Kind`, `FileLine` | `src/lib.rs:87,98,678` | One body line, what it does, and the equality its matching uses |
| `Applied`, `Placed` | `src/lib.rs:121,130` | A patched file and where each hunk went |
| `ParseError`, `ApplyError` | `src/lib.rs:149,164` | Why a diff could not be read, and why a section could not be applied |
| `Patch::parse`, `recounted`, `to_unified` | `src/lib.rs:197,252,265` | Read a diff, recompute its counts, and render it again |
| `FilePatch::apply` | `src/lib.rs:286` | Apply one section to the bytes the caller holds |

# Owned state

Nothing durable. Every type is a value, and `Applied` borrows nothing from the input it was built
from: the caller keeps the bytes it passed in.

# Invariants

- The counts are derived, not trusted. `Patch::recounted` recomputes every header from its body, and
  `Patch::to_unified` writes the recomputed numbers, so a rendered patch is one whose counts are
  true.[^crate]
- A body's end is decided in one place. It is read with the header's own counts when those counts
  fit the text and the line after them opens a section, and by prefix scan when they do not. Two
  limits follow, both stated on `Patch::parse`: a diff whose counts are wrong and whose body removes
  a line beginning `-- ` immediately before adding one beginning `++ ` has those two read as a
  section header, and a count reached exactly at a section's own `--- `/`+++ ` label pair reads those
  labels as body lines and joins the two sections.
- An anchor is honoured or the hunk fails. The old side is looked for from the stated line forward,
  the signed distance is reported, and a hunk that matches nowhere is `ApplyError::NotFound`. A hunk
  with no old side is `Placed::Inserted`, whose start past the end of the file clamps to the last
  line.
- A path is relative and single. A label that is absolute, that holds a `..` component, or that
  differs from the other side of its section is `ParseError::InvalidPath`, and a section whose both
  sides are `/dev/null` is too. A leading `a/` or `b/` is stripped, so a `git diff` reads.
- A hunk body line always carries its leading space, `-`, or `+`, so no body line can begin with the
  `@@` that opens a hunk. The compact `@@ -1 +1 @@` form parses to the counts it means.
- Text only. A section applied to bytes that are not UTF-8 is `ApplyError::NotText`.

# Dependencies

`thiserror` alone. Development adds `proptest`, which the property cases in `src/lib.rs` are the
only consumer of. The crate is `#![no_std]` with `extern crate alloc` (`src/lib.rs:6-8`), and it
declares `purity = "pure"` in its manifest.[^manifest]

# Reconciliation

The design does not name this crate, so there is no reconciliation against it.

The tree makes one choice the design's crate map would have had to make too: the crate holds no port
and no trait, and no type of its own crosses the workspace. `Patch` is a value a caller owns, and
the model's edit tool keeps it behind the kernel, which is what keeps the diff format out of
[`mokmok-agent-runtime`](agent-runtime.md).

# Tests

One `#[cfg(test)] mod tests` in `src/lib.rs` holds 35 cases, two of them property cases inside a
single `proptest!` block. The property strategy generates removed lines as well as added ones, which
is what lets a body's reading be falsified rather than assumed.

```
cargo test -p mokmok-patch
MUTANTS_PACKAGE=mokmok-patch ./scripts/mutants.sh --jobs 8
```

The mutation gate records 100% over 82 tested mutants for this member.

[^crate]: The crate doc and the module are `mokmok-patch/src/lib.rs:1`.
[^manifest]: The manifest is `mokmok-patch/Cargo.toml:7`.

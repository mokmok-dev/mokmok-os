---
type: Crate
title: mokmok-skill
description: A SKILL.md read into a validated skill, with its frontmatter shape-checked and its name checked against the directory it was read from, and no file opened.
resource: ../../../mokmok-skill
tags: [mokmok-os, crate, skill]
status: draft
generated: { by: grok-build/grok-4.7, at: 2026-10-17T00:00:00Z }
stale_after: 2026-11-15T00:00:00Z
sources:
  - id: crate
    resource: ../../../mokmok-skill
    title: mokmok-skill sources
    last_modified: 2026-10-17T00:00:00Z
---

# Responsibility

Reads one `SKILL.md` into a `Skill`, with its frontmatter checked for shape and its fields checked
against the directory the file was read from. The crate opens no file, reads no clock, and reaches no
socket: a caller owns every effect, and the text it hands in is the only thing that moves.[^crate]

It reads a skill file as an operator writes one rather than as a schema demands one. A key the format
does not name is dropped, value and all, because a library shared with other tools carries vendor keys
a reader of this crate never consults. A value is read only as a scalar, a quoted scalar, or a block
scalar, and anything else is refused with the line that carried it. A name the file states must equal
the directory it was read from, because the model chooses a skill by name and a name that does not
name its own directory cannot be loaded.

The design names no section for this crate. It is the sixteenth member and the fourth one the plan
does not carry, beside [`mokmok-tui`](tui.md), [`mokmok-search`](search.md), and
[`mokmok-patch`](patch.md).

# Public surface

Modules `frontmatter`, `name`, and `refusal`, all private; the public items are re-exported at the
crate root. `frontmatter` reads the file's shape and gathers a raw value that `parse` then checks.

| Item | Where | What it is |
| --- | --- | --- |
| `SkillName` | `src/name.rs:18` | A validated name: 1..=64 characters, `[a-z0-9-]`, no leading or trailing hyphen, no `--` |
| `SkillSummary` | `src/lib.rs:71` | The name and description the prompt advertises |
| `Skill` | `src/lib.rs:80` | One `SKILL.md`: the summary, three optional fields, a `metadata` map, and the body |
| `Refusal` | `src/refusal.rs:15` | Why a file is not a skill, one variant per rule, one sentence on `Display` |
| `parse`, `MAX_FRONTMATTER_BYTES`, `MAX_INSTRUCTIONS_BYTES` | `src/lib.rs:108,56,61` | Read one file against its directory name, and the two caps |

`frontmatter::read` (`src/frontmatter.rs:113`) checks the fences, the value subset, and one level of
`metadata` nesting, and returns a shape-checked value. `parse` applies the field rules on top of it.

# Owned state

Nothing durable. Every type is a value, and `Skill` borrows nothing from the text it was built from:
the caller keeps the string it passed in.

# Invariants

- The name is the directory. A file whose `name` is absent, malformed, or different from the
  directory it was read from is refused, and a name entering a decode is re-validated by the same
  rule (`src/name.rs:27`, `src/lib.rs:115`).[^crate]
- A key the format does not name is skipped with its value, and its indented lines are swallowed
  rather than read as the next key (`src/frontmatter.rs:259`). A duplicate key naming a known field
  is refused, while a duplicate unknown key is tolerated (`src/frontmatter.rs:264`).
- The value subset is a plain scalar, a single-quoted scalar, a double-quoted scalar with `\"`, `\\`,
  and `\n`, or a `|`/`>` block with a `-`, `+`, or absent chomp. Anything else is refused with the
  line that carried it (`src/frontmatter.rs:477`, `src/frontmatter.rs:445`).
- The body is everything after the closing `---` line and its newline, verbatim, and the frontmatter
  region between the fences is capped before the body is measured (`src/frontmatter.rs:125`,
  `src/frontmatter.rs:140`).
- An oversized body is refused, never clipped, because a clipped instruction is a different
  instruction (`src/lib.rs:163`).

# Dependencies

`serde` and `thiserror` alone, with `serde` deriving on the types that ride a record and `SkillName`
re-validating through `#[serde(try_from = "String")]`. Development adds `proptest`, which the property
cases are the only consumer of, and `serde_json` for a decode case. The crate is `#![no_std]` with
`extern crate alloc` (`src/lib.rs:8-10`), it declares `purity = "pure"` in its manifest, and it
depends on no `mokmok-` crate.[^manifest]

# Reconciliation

The design does not name this crate, so there is no reconciliation against it.

The tree makes one choice the design's crate map would have had to make too: the crate holds no port
and no trait. `Skill`, `SkillName`, and `Refusal` are values a caller owns, and the runtime's `Skills`
port is declared in [`mokmok-agent-runtime`](agent-runtime.md), which is what keeps the file format out
of that crate.

# Tests

`src/lib.rs` holds 42 cases, four of them property cases inside a single `proptest!` block. The
property cases pin that `parse` never panics on arbitrary bytes, that a parsed name always equals its
directory, that a refused oversize body is never truncated, and that a decoded name re-validates.
`src/frontmatter.rs` holds 26 cases for the fences, the value subset, the block rendering, and the
`metadata` block, one of them for an empty `metadata` value that the closing fence ends rather than a
later key.

```
cargo test -p mokmok-skill
MUTANTS_PACKAGE=mokmok-skill ./scripts/mutants.sh --jobs 8
```

[^crate]: The crate doc is `mokmok-skill/src/lib.rs:1`.
[^manifest]: The manifest is `mokmok-skill/Cargo.toml:7`.

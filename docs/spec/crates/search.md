---
type: Crate
title: mokmok-search
description: A BM25 index over one directory tree, refreshed incrementally and queried fuzzily, that answers the model's find from names and contents.
resource: ../../../mokmok-search
tags: [mokmok-os, crate, search]
status: draft
generated: { by: grok-build/grok-4.7, at: 2026-10-15T08:39:00Z }
stale_after: 2026-11-15T00:00:00Z
sources:
  - id: crate
    resource: ../../../mokmok-search
    title: mokmok-search sources
    last_modified: 2026-10-15T08:39:00Z
---

# Responsibility

Binds one directory tree to one on-disk tantivy index and answers a query from it by BM25. The
index holds each file's name and its first bytes, so a model can find a file by a word in its path
or a word in its body.[^crate]

The design names no section for this crate. It is the fourteenth member and the second one the plan
does not carry, beside [`mokmok-tui`](tui.md).

# Public surface

Modules `corpus`, `error`, `query`, `render`, `token`, and `walk`.

| Item | Where | What it is |
| --- | --- | --- |
| `Corpus` | `src/corpus.rs:39` | One root and the index derived from it |
| `Corpus::DEFAULT_LIMIT`, `open`, `root`, `refresh`, `query` | `src/corpus.rs:49,58,85,100,169` | Open or create, reconcile with the tree, and answer |
| `Limits` | `src/corpus.rs:256` | `file_bytes`, `body_bytes`, `files`: what a pass will not read |
| `Refresh` | `src/corpus.rs:276` | What one pass did: indexed, updated, removed, unchanged, skipped |
| `Hit`, `Response` | `src/query.rs:16,26` | One scored path with its snippet, and the documents the index held |
| `render` | `src/render.rs:11` | One response as the text the model reads |
| `Error` | `src/error.rs:9` | `Io`, `Index`, and `Root` |

`refresh` and `query` both take `&self`. The writer is created for the length of a pass and dropped
with it, so a caller holds no lock around a corpus.

# Owned state

A tantivy index directory and a `manifest.json` (`src/corpus.rs:27`) beside it, both under the store
path the caller names. The manifest records the modification time and the length of every indexed
file; the index holds the documents. Nothing else is durable.

# Invariants

- The index is derived. It is never the authority on what exists: a lost or stale index is rebuilt
  from a walk, and a hit is a suggestion the caller must still be able to read.
- The walk follows no symlink, enters no directory whose name begins with a dot, and enters neither
  `target` nor `node_modules`.
- A file with a NUL byte in its first 8 KiB is not text and is not indexed. A file past
  `Limits::file_bytes`, or a tree past `Limits::files`, is skipped and counted rather than fatal. An
  unreadable file or directory is skipped and counted; an unreadable *root* is `Error::Root`.
- The store is skipped by resolved path when it lies inside the root, so the index never indexes
  itself.
- A pass reads only what changed: a file whose modification time and length match the manifest is
  left alone.
- The index is committed before the manifest is written. A crash between the two costs a re-index
  and never a document the manifest claims and the index does not hold.
- A document is replaced by deleting its path term and then adding, in that order, so a path
  re-added after a lost manifest lands once.
- Two identical queries over one index answer with the same list in the same order: score ties are
  broken by path, because tantivy's own tie-break is a document address that a later commit moves.

# Dependencies

`serde`, `serde_json`, `tantivy` with `mmap` and no codec, and `thiserror`. Development adds
`proptest`. tantivy is the crate's only dependency with a transitive tree of any size, and it is
compiled once for the workspace.

# Reconciliation

The design does not name this crate, so there is no reconciliation against it.

The tree makes one choice the design's crate map would have had to make too: the crate holds no
port and no trait. A `Corpus` is a value a caller owns, and the runtime's `Finder` port is declared
in `mokmok-agent-runtime`, which is what keeps tantivy out of that crate.

# Tests

There is a `#[cfg(test)]` module in `src/token.rs` with 5 cases, and `tests/corpus.rs` holds 33: 32
behaviour cases and one proptest case. Two doctests build a corpus in the temporary directory and
query it.

```
cargo test -p mokmok-search
MUTANTS_PACKAGE=mokmok-search ./scripts/mutants.sh --jobs 8
```

[^crate]: The crate doc is `mokmok-search/src/lib.rs:1-73`.

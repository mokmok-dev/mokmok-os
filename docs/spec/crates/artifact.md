---
type: Crate
title: mokmok-artifact
description: Stores artifact bytes in a content-addressed directory and gates reads behind a reference registry, an authority port, and a retention policy.
resource: ../../../mokmok-artifact
tags: [mokmok-os, crate, storage]
status: draft
generated: { by: grok-build/grok-4.7, at: 2026-10-15T02:00:00Z }
stale_after: 2026-11-15T00:00:00Z
sources:
  - id: architecture
    resource: ../../../mokmok-os-architecture.md
    title: mokmok OS architecture and crate responsibilities, section 5.10
    author: codex/gpt-6
  - id: crate
    resource: ../../../mokmok-artifact
    title: mokmok-artifact sources
    last_modified: 2026-10-12T06:37:04Z
---

# Responsibility

Keeps large results outside the event log. Bytes are written to a content-addressed file tree, and a
reference records that a hash is safe to read and by whom.[^crate]

# Public surface

| Item | Where | What it is |
| --- | --- | --- |
| `BlobStore` | `src/blob.rs:15` | `open`, `root`, `path`, `put`, `len`, `read`, `remove` |
| `ContentHash` | `src/content_hash.rs:13` | `from_bytes`, `as_bytes`, `of`, `Display`, `FromStr` |
| `Reference`, `ReferenceStore`, `Catalog`, `Authorizer` | `src/metadata.rs:34,13,104,94` | The durable reference, where references live, the read gate, and the authority port |
| `Collector`, `Report` | `src/gc.rs:29,14` | The sweep and what it collected |
| `Retention` | `src/retention.rs:13` | A minimum age and a byte budget |

# Owned state

A directory tree rooted where `BlobStore::open` was pointed. One file per blob at
`<root>/<first two hex>/<64 hex>` (`src/blob.rs:39-47`). A write stages a `.tmp-<pid>-<n>` file in
the same directory and renames it into place (`src/blob.rs:133`). The store holds only a root path
(`src/blob.rs:15-17`), with no database and no in-memory index.

# Invariants

- A reference cannot name bytes the store does not hold whole. `Reference::new` calls
  `store.len(hash)` and every field is private (`src/metadata.rs:52-58`).
- Authority is checked before the registry is touched, so a refused read answers the same whether
  or not the artifact exists (`src/metadata.rs:141-151`).
- A blob younger than the minimum age is never a collection candidate (`src/gc.rs:130`), and a
  candidate's reference is asked again immediately before the unlink (`src/gc.rs:92`).
- A byte budget never evicts a referenced blob, and a sweep stops as soon as the store fits
  (`src/gc.rs:76,111`).
- Staging-file bytes stay out of the budget, so a torn write cannot evict a complete blob
  (`src/gc.rs:71-75`).
- A modification time before the epoch reads as the epoch, which keeps the blob
  (`src/filesystem.rs:87`).
- Content hashes establish content identity and carry no reading authority of their own
  (`src/metadata.rs:94`).

# Dependencies

`mokmok-protocol`, `serde`, `serde_json`, `sha2`, and `thiserror`. Development adds `proptest` and
`uuid`. The crate declares no tokio, no rusqlite, and no axum dependency.

# Reconciliation

The design gives this crate the modules `blob`, `metadata`, `filesystem`, `content_hash`,
`retention`, and `gc`.[^architecture] All six exist under those names, and `content_hash` owns the
hash type.

The design requires that a durable reference never points at incompletely persisted data. The check
exists in `Reference::new`, and no production code builds a `Reference`. The kernel keeps its own
`Blob` type holding a hash string (`mokmok-kernel/src/execution.rs:35-40`), opens a `BlobStore`
(`mokmok-kernel/src/execution.rs:172`), and writes bytes before the entry that names them
(`mokmok-kernel/src/execution.rs:179,212`). Reference creation, `Catalog`, `Collector`, and
`Retention` have no consumer outside this crate and its tests.
The only `ReferenceStore` and `Authorizer` implementations in the tree are test fixtures and one
doc example in `src/lib.rs`.

The design states that the blob and event transaction boundary needs an explicit write order and a
recovery protocol. No shared protocol exists. `BlobStore::remove` is public
(`src/blob.rs:114`), and a reference to a removed blob is caught at read time.

`Catalog::read` never compares a blob's length with the size its reference recorded
(`src/metadata.rs:88`).

Behaviour the design does not name: the two-hex-character path split, `put` returning early when
the file already exists (`src/blob.rs:66`), the reported temporary and over-budget counts, and
uppercase hex accepted by `FromStr` (`src/content_hash.rs:71`).

# Tests

23 `#[test]` cases, in three files and no `#[cfg(test)]` module, with shared fixtures in
`tests/common/mod.rs`.

`tests/blob.rs` holds 10 cases: SHA-256 vectors, hex parsing, a put and read round trip, an
interrupted write staying invisible while its staging file is collected, `remove` reporting
presence, and an unsyncable directory failing the write. `tests/gc.rs` holds 9 cases: collecting
past the minimum age, keeping inside it, the mark and sweep race keeping a late reference,
oldest-first eviction to the byte budget, and the report's fields. `tests/authority.rs` holds 4
cases: a denied read, an allowed read, a reference to an unwritten blob refused, and the recorded
size.

```
cargo test -p mokmok-artifact
```

[^architecture]: Section 5.10 of `mokmok-os-architecture.md`.
[^crate]: The re-export list is `mokmok-artifact/src/lib.rs:67-71`.

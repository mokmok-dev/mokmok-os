---
type: Attested Computation
title: Line references against the tree
description: Holds every symbol and line citation in this bundle to the file and line it names.
runtime: bash
tags: [okf, verification, documentation]
status: draft
generated: { by: grok-build/grok-4.7, at: 2026-10-15T02:00:00Z }
verified:
  - { by: process:line-references, at: 2026-10-15T03:05:00Z }
stale_after: 2026-11-15T00:00:00Z
parameters:
  - { name: repo, type: string, required: true }
executor:
  resource: /references/executors/run-in-repo.md
  receipt: [repo, command, pairs_checked, unresolved]
attester:
  resource: /references/attesters/line-references.sh
  verdict: [verdict, reason]
sources:
  - id: repo
    resource: ../../..
    title: the mokmok-os repository at commit bbe2ea2
    last_modified: 2026-10-14T03:34:14Z
---

# Computation

```
docs/spec/references/attesters/line-references.sh check "$repo" $(find "$repo/docs/spec" -name '*.md')
```

# How a citation is read

A citation is a backticked path and a list of numbers. The path resolves against the repository root
when it names a crate, as in the `Store` type at `mokmok-event-store/src/store.rs:80`. A crate
document writes the same thing with a bare `src/` path, and that path resolves against the directory
named by the document's own `resource` field.

The symbols named before the citation on the same line pair with its numbers in order, and each pair
must find its symbol within three lines of the number it claims. In a table row the symbols come
from the first column. When a row names fewer symbols than it cites numbers, the weaker rule
applies: at least one named symbol must appear near each number.

# The run on 2026-10-15

570 pairs were checked across the bundle, and none failed.

The count is the point of the check. The first version of it read single-line citations only, so
every comma list went unread and it reported 23 problems while passing whole tables it could not
parse. Widening it to comma lists and per-row pairing raised the count to 25 real drifts. One table
row in [the event store](../crates/event-store.md) named four types and cited line numbers from a
different part of the file, and the `mokmok-kernel` row for `Db` cited the constructor instead of
the type.

The checker was falsified before it was trusted. Against a document that cited the `Store` type at
line 900 of its own `src/store.rs`, and cited it again at line 80 where the type actually sits, the
checker reported the first, passed the second, and exited nonzero. That run also showed the limit of
the weak rule: the line named one symbol before three citations, so the two later symbols were never
paired and never checked.

# What this proves

Every citation in this bundle names a file that resolves and a line where its symbol appears.

# What this does not prove

That the sentence carrying the citation is true. The check reads position, not meaning, so a
correct citation can sit under a wrong claim. A range is checked at its two endpoints, and a
multi-symbol row is checked positionally only when its symbol and number counts agree. Those are the
gaps a reader closes by opening the file.

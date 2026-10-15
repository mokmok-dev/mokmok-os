---
type: Executor
title: Run one sanctioned command against the repository
description: How a runner executes a bundle computation inside the mokmok-os tree and returns a receipt.
resource: ../../../..
tags: [mokmok-os, executor]
status: draft
generated: { by: grok-build/grok-4.7, at: 2026-10-15T02:00:00Z }
sources:
  - id: repo
    resource: ../../../..
    title: the mokmok-os repository
    last_modified: 2026-10-14T03:34:14Z
---

# Steps

1. Change to the repository root. Every computation in this bundle resolves paths from there.
2. Run the computation's command line as written, with no added flags.
3. Capture the exit status and the full output.
4. Write a receipt as one JSON object with one key per field the computation's `executor.receipt`
   names.
5. Hand the receipt to the attester the computation names.

# Receipt

A receipt records what one run observed. A field the runner did not observe does not belong in the
receipt, because a fabricated field is what the attester catches first.

Paths that name something on the machine that ran the computation, such as the repository root and
a captured log, are absolute. The attester reads them on the same machine, and a receipt does not
travel between machines.

# Reruns

A reader confirms a receipt by running the attester over the receipt the computation's own
`# Computation` block produced, then by running the command line again and attesting the new
receipt. The second path is what a claim of freshness rests on, and it is the reason the receipt is
not stored in the bundle.

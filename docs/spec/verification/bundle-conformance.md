---
type: Attested Computation
title: Conformance of this bundle
description: Checks that every concept in this bundle parses as OKF v0.2, that the reserved files follow their structure, and that every link resolves.
runtime: bash
tags: [okf, verification, documentation]
status: draft
generated: { by: grok-build/grok-4.7, at: 2026-10-15T02:00:00Z }
verified:
  - { by: process:okf-conformance, at: 2026-10-15T02:20:00Z }
stale_after: 2026-11-15T00:00:00Z
parameters:
  - { name: bundle, type: string, required: true }
executor:
  resource: /references/executors/run-in-repo.md
  receipt: [bundle, command, exit_code, files_checked, problems]
attester:
  resource: /references/attesters/okf-conformance.sh
  verdict: [verdict, reason]
sources:
  - id: okf
    resource: https://github.com/GoogleCloudPlatform/open-knowledge-format/blob/main/SPEC.md
    title: Open Knowledge Format v0.2 specification
    author: team:google-cloud-platform
  - id: architecture
    resource: ../../../mokmok-os-architecture.md
    title: mokmok OS architecture and crate responsibilities
    author: codex/gpt-6
---

# Computation

```
docs/spec/references/attesters/okf-conformance.sh check "$bundle"
```

# What the check reads

| Source rule | What it is held to |
| --- | --- |
| Section 11, item 1 and 2 | Every non-reserved `.md` file has a frontmatter block on line 1, the block closes, every line in it is a key or an indented continuation, and `type` carries a value |
| Section 11, item 3 | Every `index.md` follows section 8. Each entry is a bullet holding a bracketed title, a link in parentheses, and a description after a hyphen, and a frontmatter block holds `okf_version` and only at the bundle root |
| Section 11, item 3 | Every `log.md` follows section 9. Date headings read `## YYYY-MM-DD`, they run newest first, and entries are bullets |
| Sections 6.1 and 6.2 | Every markdown link target resolves, and so does every path-valued field: `resource`, `computation`, `executor.resource`, and `attester.resource` |

A path-valued field that is a `https` URL is skipped, and so is a value holding a space, which is
how section 5.1 allows a scope descriptor. Bundle-relative paths begin with `/`, and relative paths
resolve against the directory of the file that holds them.

# The run on 2026-10-15

The check reported zero problems across 31 files. The receipt recorded the file count and the
command, and [okf-conformance.sh](/references/attesters/okf-conformance.sh) recounted both from the
bundle and returned `verdict=pass`.

The check was falsified twice before it was trusted. On its first run it reported 50 problems: 45
path-valued fields at the wrong directory depth, and 5 link targets that had not been written yet.
Both counts were real. It also skipped both `index.md` files in silence, because a syntax error in
its own `awk` program made that check produce no output at all, and the count of 50 looked complete
without them. Fixing the program, the paths, and the documents brought the count to zero.

The program was then run against a fabricated bundle holding a concept with no frontmatter, one with
no `type`, an index entry off its shape, a log heading in the wrong form, a broken link, and a path
field naming a file that does not exist. It reported all six and exited nonzero.

# What this proves

This bundle is a conformant OKF v0.2 document set, and every path it names exists on the machine
that ran the check.

# What this does not prove

Conformance is a claim about shape. The check does not read a sentence for truth, so a bundle full
of confident falsehoods passes it. Whether the claims inside are right is a matter for
[the acceptance table](/verification/acceptance.md) and for the test suites the crate documents
name.

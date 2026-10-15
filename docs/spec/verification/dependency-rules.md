---
type: Attested Computation
title: The architectural dependency rules
description: Checks the six dependency rules the architecture document asks CI to enforce, one rule per run, by reading the manifests and the sources.
runtime: bash
tags: [mokmok-os, verification, architecture]
status: draft
generated: { by: grok-build/grok-4.7, at: 2026-10-15T02:00:00Z }
verified:
  - { by: process:dependency-rules, at: 2026-10-15T02:04:00Z }
stale_after: 2026-11-15T00:00:00Z
parameters:
  - { name: repo, type: string, required: true }
  - { name: rule, type: string, required: true }
executor:
  resource: /references/executors/run-in-repo.md
  receipt: [rule, repo, command, status]
attester:
  resource: /references/attesters/dependency-rules.sh
  verdict: [verdict, reason]
sources:
  - id: architecture
    resource: ../../../mokmok-os-architecture.md
    title: mokmok OS architecture and crate responsibilities, section 9
    author: codex/gpt-6
  - id: repo
    resource: ../../..
    title: the mokmok-os repository at commit bbe2ea2
    last_modified: 2026-10-14T03:34:14Z
---

# Computation

```
docs/spec/references/attesters/dependency-rules.sh check "$repo" "$rule"
```

Run the script with no rule name to check all six at once. Run it with `list` to enumerate the rule
names, which are the values the `rule` parameter takes.

# The rules

| Rule | What it forbids |
| --- | --- |
| `protocol-has-no-domain-dependency` | Any `mokmok-` crate in the protocol crate's dependencies |
| `pure-crates-have-no-io-dependency` | tokio, rusqlite, axum, reqwest, hyper, tokio-tungstenite, futures-util, nix, or landlock in any crate whose library is `#![no_std]` |
| `event-store-has-no-transport-dependency` | axum, tokio, tokio-tungstenite, futures-util, or hyper in the event store |
| `scheduler-has-no-model-dependency` | reqwest, hyper, or the agent runtime in the scheduler |
| `capability-performs-no-enforcement-io` | tokio, nix, or landlock in the capability crate, and any reach to `std::process`, `std::fs`, `std::net`, or `Command::new` in its sources |
| `kernel-dependents-are-applications` | A dependency on `mokmok-kernel` from any crate outside the allowlist, which holds `mokmok-tui` |

The design states these rules as constraints on which crate may depend on what, and asks CI to
check them.[^architecture] CI does not. This computation is the check.

# The run on 2026-10-15

All six rules returned `status=pass` on the repository at commit `bbe2ea2`.

A passing check means nothing unless it can fail, so each rule was run against a fabricated tree
under `/tmp` built to trip it. All six returned `status=fail` with a detail naming the offending
dependency or crate.

# What this proves

The six rules hold over the workspace manifests and the capability crate's sources at commit
`bbe2ea2`.

# What this does not prove

A rule reads a manifest and a source text. It cannot see a dependency that arrives transitively, and
it cannot see an architectural violation expressed through a port the caller implements. A rule name
outside the six is an error, so a misspelled parameter fails the run.

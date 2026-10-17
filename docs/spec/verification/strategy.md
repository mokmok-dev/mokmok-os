---
type: Reference
title: Verification strategy
description: The evidence layers the design asks for, what each layer is for, and which layers exist in this tree.
resource: ../../../mokmok-os-architecture.md
tags: [mokmok-os, verification]
status: draft
generated: { by: grok-build/grok-4.7, at: 2026-10-15T02:00:00Z }
sources:
  - id: architecture
    resource: ../../../mokmok-os-architecture.md
    title: mokmok OS architecture and crate responsibilities, section 9
    author: codex/gpt-6
  - id: flake
    resource: ../../../flake.nix
    title: the flake that declares the checks
    last_modified: 2026-10-12T16:23:50Z
  - id: mutants
    resource: ../../../scripts/mutants.sh
    title: the mutation gate script
    last_modified: 2026-10-16T03:55:52Z
---

# How to read this bundle

Two bodies of claims sit side by side, and the text keeps them apart. Design intent comes from
`mokmok-os-architecture.md` and is cited as `[^architecture]`. Observed behaviour comes from the
source tree at commit `bbe2ea2` and is cited as `[^crate]` or with a `path:line` reference in the
sentence itself.

Every concept in this bundle carries `status: draft`, which here means the document was generated
and has not been read by a person. No concept carries a human `verified` entry. The four
`Attested Computation` concepts carry a machine `verified` entry naming the process that ran, and
the rest carry none.

Provenance lives in each concept's `sources` list. A source pointing at a directory or a file in
the repository is a real path, and the conformance check fails when one stops resolving.

# The evidence layers

The design's quality strategy is property-first, mutation-verified, fault-injected.[^architecture]
The table below is that strategy with a third column for what exists in this tree.

| Layer | Purpose | In this tree |
| --- | --- | --- |
| Unit | Local behaviour | `#[test]` and `#[tokio::test]` cases in every crate |
| Property and model-based | Invariants across generated sequences | `proptest` in twelve crates, and `proptest-state-machine` in `mokmok-state-machine` |
| Mutation | Weak assertions | `scripts/mutants.sh`: one package per run at a 95% floor, `--all` scores every member into `mutation.json` |
| Fuzzing | Unexpected input | Absent |
| Concurrency | Controlled interleavings | Real concurrent tests using two stores, two writers, and a churning publisher; no Loom |
| Integration | Real boundary behaviour | Real SQLite files, real Unix sockets, real Landlock confinement, real subprocesses |
| Fault injection | Recovery and uncertainty | `SIGKILL` at named points in `mokmok-event-store/tests/crash.rs`, and crash points in the kernel's slice |
| Formal verification | Specified properties under stated assumptions | Absent |

# What runs in CI

One command, `nix flake check`, and the flake declares five checks over the whole workspace:
`cargoClippy` with warnings denied, `cargoDocTest`, `cargoTest` through crane's `cargoNextest`,
`treefmt` for Nix, Rust, and TOML, and `pre-commit` for `actionlint`, `deadnix`, and `statix`.[^flake]
Two further checks cover the systemd unit and the NixOS module.

The mutation gate is a script the operator runs, not a check. One package per invocation, selected
by `MUTANTS_PACKAGE`, enforces a floor of 95% by default. `--all` sweeps every member, writes each
one's score and target to `mutation.json`, and exports the confiner the kernel's and the tui's
integration tests need; `--check-baseline` reads that ledger back and reports which rows moved.
Twelve mutants across six members are excluded as equivalent, each with its proof beside the
exclusion in `.cargo/mutants.toml`, and one of those twelve is a scope limit rather than an
equivalence. All sixteen members score 100% over the mutants that remain.[^mutants]

# The four attested computations

[The nix gate](ci-gate.md) proves that the test suite passes at a tree, and attests the test count
against the nextest log it produced.

[The dependency rules](dependency-rules.md) proves that the six architectural dependency rules hold
over the manifests and sources. It recomputes each rule from the tree, so the receipt is held to a
fresh reading.

[Bundle conformance](bundle-conformance.md) proves that this bundle parses and resolves as OKF v0.2,
which is a claim about the document set and says nothing about whether a claim inside it is true.

[Line references](line-references.md) holds every symbol and line citation in the bundle to the
tree, which is what keeps the crate documents checkable as the code moves.

# What no layer here proves

A passing suite, a mutation score, and a conformant bundle together say nothing about isolation on a
platform nobody ran, exactly-once effects at an external service, or a design claim that no test
exercises. [Acceptance](acceptance.md) lists the design's own acceptance criteria and the evidence
that exists against each.

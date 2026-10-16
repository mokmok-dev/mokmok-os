---
type: Attested Computation
title: The architectural dependency rules
description: Checks the nine dependency rules the architecture document asks CI to enforce, one rule per run, by reading the manifests, the declared crate roles, and the sources.
runtime: bash
tags: [mokmok-os, verification, architecture]
status: draft
generated: { by: grok-build/grok-4.7, at: 2026-10-15T02:00:00Z }
verified:
  - { by: process:dependency-rules, at: 2026-10-15T02:04:00Z }
  - { by: process:dependency-rules, at: 2026-10-16T02:00:00Z }
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
    title: the mokmok-os repository at commit fbe5270
    last_modified: 2026-10-16T01:38:00Z
---

# Computation

```
docs/spec/references/attesters/dependency-rules.sh check "$repo" "$rule"
```

Run the script with no rule name to check all nine at once. Run it with `list` to enumerate the rule
names, which are the values the `rule` parameter takes.

# The rules

| Rule | What it forbids |
| --- | --- |
| `protocol-has-no-domain-dependency` | Any `mokmok-` crate in the protocol crate's dependencies |
| `every-member-declares-its-role` | A workspace member with no `[package.metadata.mokmok] purity` value in its own manifest, a value outside `pure`, `adapter`, and `shell`, or a `shell` declaration outside the composition roots |
| `pure-crates-are-no_std` | A crate whose declared role and whose sources disagree about `#![no_std]`, or a `#![no_std]` crate naming tokio, rusqlite, axum, reqwest, hyper, tokio-tungstenite, futures-util, nix, or landlock in its dependencies |
| `pure-crates-depend-only-on-pure-crates` | A `pure` crate with a `mokmok-` dependency edge onto a crate that does not declare `pure` |
| `the-role-census-is-not-empty` | A tree where no member declares `pure`, or none declares `adapter`, which is what a broken selector would look like |
| `event-store-has-no-transport-dependency` | axum, tokio, tokio-tungstenite, futures-util, or hyper in the event store |
| `scheduler-has-no-model-dependency` | reqwest, hyper, or the agent runtime in the scheduler |
| `capability-performs-no-enforcement-io` | tokio, nix, or landlock in the capability crate, and any reach to `std::process`, `std::fs`, `std::net`, or `Command::new` in its sources |
| `kernel-dependents-are-applications` | A dependency on `mokmok-kernel` from any crate outside the composition roots |

The design states these rules as constraints on which crate may depend on what, and asks CI to
check them.[^architecture] For four rules the check is the flake's `dependencyRules` attribute. Each
member declares its own role, so a member added without one fails rather than being silently treated
as std, and `kernel-dependents-are-applications` reads the same list of composition roots that
`every-member-declares-its-role` holds the `shell` role to.

# The run on 2026-10-15

All six rules of that date returned `status=pass` on the repository at commit `bbe2ea2`.

A passing check means nothing unless it can fail, so each rule was run against a fabricated tree
under `/tmp` built to trip it. All six returned `status=fail` with a detail naming the offending
dependency or crate.

# The run on 2026-10-16

Nine rules. All nine returned `status=pass` on the repository at commit `fbe5270` with the role
declarations added, and the census reported `pure=4 adapter=9 shell=2`, which is all fifteen members.

```
rule=protocol-has-no-domain-dependency status=pass
rule=every-member-declares-its-role status=pass
rule=pure-crates-are-no_std status=pass
rule=pure-crates-depend-only-on-pure-crates status=pass
rule=the-role-census-is-not-empty status=pass detail=pure=4 adapter=9 shell=2
rule=event-store-has-no-transport-dependency status=pass
rule=scheduler-has-no-model-dependency status=pass
rule=capability-performs-no-enforcement-io status=pass
rule=kernel-dependents-are-applications status=pass
```

Sensitivity, run as a mode rather than by hand. `dependency-rules.sh faults` lays down a four-member
repository in a temporary directory, asserts the four role rules pass over it, then applies one edit
per fault and asserts the intended rule reports `status=fail` with a detail naming the offender. The
base tree is the control: a fault case is only evidence if the unedited tree passes.

```
control=base rule=every-member-declares-its-role status=pass
control=base rule=pure-crates-are-no_std status=pass
control=base rule=pure-crates-depend-only-on-pure-crates status=pass
control=base rule=the-role-census-is-not-empty status=pass
fault=no-declaration rule=every-member-declares-its-role status=fail detail=mokmok-beta declares no role in its own manifest
fault=unknown-role rule=every-member-declares-its-role status=fail detail=mokmok-beta declares the unknown role adaptor
fault=shell-outside-the-roots rule=every-member-declares-its-role status=fail detail=mokmok-alpha declares the shell role and is not a composition root
fault=declared-pure-and-std rule=pure-crates-are-no_std status=fail detail=mokmok-alpha declares the pure role and is not no_std
fault=no_std-and-declared-adapter rule=pure-crates-are-no_std status=fail detail=mokmok-beta is no_std and declares the role adapter
fault=no_std-with-an-io-dependency rule=pure-crates-are-no_std status=fail detail=mokmok-alpha is no_std and depends on tokio
fault=pure-depending-on-an-adapter rule=pure-crates-depend-only-on-pure-crates status=fail detail=mokmok-alpha is pure and depends on mokmok-beta, which declares adapter
fault=no-pure-member rule=the-role-census-is-not-empty status=fail detail=no member declares the pure role, so the purity rule selects nothing
```

`nix flake check` builds the `dependencyRules` attribute over this same command, so the nine rules
are a gate rather than an operator step.

# What this proves

The nine rules hold over the workspace manifests, the declared roles, and the capability crate's
sources at commit `fbe5270` plus the role declarations. Each of the four role rules has a rerunnable
fault that makes it fail with a named reason.

That the four crates declaring `pure` are `#![no_std]` and free of an IO dependency is checked by
the compiler as well, because `#![no_std]` makes rustc reject `std`, and `unsafe_code = "deny"`
closes the `extern "C"` route. The rule adds the declaration, which the compiler cannot see.

# What this does not prove

A rule reads a manifest and a source text. It cannot see a dependency that arrives transitively, and
it cannot see an architectural violation expressed through a port the caller implements. A rule name
outside the nine is an error, so a misspelled parameter fails the run.

A `pure` declaration says nothing about how much of a crate is a decision and how much is arithmetic
on values it was handed. `mokmok-agent-runtime` is the crate where that gap is widest: its own
documentation claims it is pure, its declared role is `adapter`, and the rule cannot tell which is
right until the blockers in its manifest and its `Setting` type are gone.

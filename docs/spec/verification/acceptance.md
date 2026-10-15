---
type: Reference
title: Acceptance criteria against the tree
description: The design's twelve acceptance boundaries, the evidence each one asks for, and the evidence that exists today.
resource: ../../../mokmok-os-architecture.md
tags: [mokmok-os, verification, acceptance]
status: draft
generated: { by: grok-build/grok-4.7, at: 2026-10-15T02:00:00Z }
stale_after: 2026-11-15T00:00:00Z
sources:
  - id: architecture
    resource: ../../../mokmok-os-architecture.md
    title: mokmok OS architecture and crate responsibilities, section 10
    author: codex/gpt-6
  - id: repo
    resource: ../../..
    title: the mokmok-os repository at commit bbe2ea2
    last_modified: 2026-10-14T03:34:14Z
---

# The table

The design states twelve boundaries and the evidence each needs before it can be called
complete.[^architecture] The third column records what the tree holds. The fourth column records
what the evidence does not reach.

| Boundary | Evidence the design asks for | In this tree | What the evidence does not reach |
| --- | --- | --- | --- |
| Protocol | Rust and TypeScript conformance, explicit version compatibility | Rust fixtures, round-trip properties, malformed-input cases, and a version policy with four tests | No TypeScript consumer exists in the workspace, so cross-language conformance is untested |
| Event store | Correct concurrent and duplicate append, crash recovery | 44 cases including two stores racing one expected version, eight transaction cases on a real file, and five `SIGKILL` cases | Recovery is tested on one filesystem and one SQLite build |
| Event bus | Replay, reconnect, flow control, missed-notification recovery | 25 cases over a real socket, including reconnect with the same event identifier and a consumer whose checkpoint lapsed | There is no separate reconciliation path; the read path closes the commit-to-notification gap and no test constructs the gap |
| State machine | Illegal transitions rejected, deterministic replay, model agreement | 17 rule cases, a stateful property test against a reference model, and replay equality properties | No formal harness |
| Agent runtime | Safe mid-turn recovery, explicit unknown outcomes | Intent-before-effect cases in `src/advance.rs`, `Resolution::Unknown` in the state machine, and a kernel case that crashes between an intent and its outcome | Recovery of an external effect nobody can prove is left to reconciliation, which has one test |
| Scheduler | Lease expiry, stale fencing rejection, duplicate wake tolerance | 16 cases covering all three, plus snapshot validation on restore | The crate is composed nowhere, so no test exercises it under a live caller |
| Capability | Authority attenuation, approval binding, expiry and revocation | 49 cases and a property that every allow names a grant attenuated from a root | Nothing durable, and no test at the process boundary |
| Sandbox | Verified isolation, termination, cleanup on supported platforms | 30 supervision cases, 5 filesystem cases, 3 symlink cases, and 1 case that a confined workload cannot reach a loopback peer | The documented model excludes a workload that calls `setsid`, and UDP to a literal address remains open |
| Egress | Destination consistency, DNS rebinding resistance, bypass prevention | 8 enforcement cases including a second resolution unable to redirect and a forbidden address refused before any connection | No proxy listener, and the rate limiter has no caller |
| Artifact | Durable references, authorization, safe collection | 23 cases covering interrupted writes, the mark and sweep race, and eviction to a byte budget | The kernel does not build a reference, so the checked path is not on the shipped route |
| Observability | Traceable decisions and effects, durable critical audit facts | An fsynced journal with 17 cases, and append-then-export ordering | The daemon appends without exporting, and installs no redaction |
| Kernel | Consistent startup, shutdown, readiness, recovery | 12 supervisor cases including a `SIGKILL` restart over the stale socket, and 1 case that the bus accepts nobody before recovery ran | Readiness is a notification datagram, so no endpoint is tested |

# How to read the third and fourth columns

A row with evidence in the third column and a limit in the fourth is a boundary that holds inside a
stated model. The stated model matters more than the case count. The sandbox row is the clearest
example: process teardown holds for a workload that does not detach itself.

A row whose evidence is all inside the crate that owns the behaviour, with no caller above it, is a
boundary that is implemented and unexercised in composition. The scheduler, capability, artifact,
and egress rows are in that state.

# Rechecking

Every case count in this table comes from the crate's own test files at commit `bbe2ea2`. Rerun one
boundary with:

```
cargo test -p <crate>
```

The whole suite at once is [the nix gate](ci-gate.md), and the architectural rules that cut across
crates are [the dependency rules](dependency-rules.md).

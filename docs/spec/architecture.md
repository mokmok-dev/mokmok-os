---
type: Architecture
title: mokmok OS architecture
description: A durable, capability-constrained autonomous agent operating system, and the principles that hold its boundaries apart.
resource: ../../mokmok-os-architecture.md
tags: [mokmok-os, architecture]
status: draft
generated: { by: grok-build/grok-4.7, at: 2026-10-15T02:00:00Z }
sources:
  - id: architecture
    resource: ../../mokmok-os-architecture.md
    title: mokmok OS architecture and crate responsibilities, sections 1 to 4 and 12
    author: codex/gpt-6
  - id: repo
    resource: ../../..
    title: the mokmok-os repository at commit bbe2ea2
    last_modified: 2026-10-14T03:34:14Z
---

# What mokmok OS is

Models already supply the reasoning, so the problems this platform takes on are the ones that remain
after that: what survives a crash, what an agent is allowed to do, and what can be shown
afterwards.[^architecture]

Four commitments follow from that framing. Every commitment is durable before it is acted on. Every
execution is bounded by authority that was issued. Every effect leaves a trace. And the mechanisms
underneath stay useful to a runtime nobody has written yet.

The design separates kernel mechanisms from agent behaviour on purpose. Persistence, scheduling,
isolation, authorization, and audit are infrastructure. Reasoning, acting, learning, and discovery
belong to agents that use the infrastructure.

# The eight principles

The design states eight principles, and each one rules out a shape that would otherwise look
reasonable.[^architecture]

Authority is explicit and owned. Each component owns a defined set of state, transitions, or
enforcement guarantees, so there is always one place to look when a guarantee is questioned. A
guarantee split across two components is a guarantee nobody can test.

Dependencies stay acyclic. Lower-level components never depend on the composition root or on
agent-specific orchestration, which is what lets the durable foundation outlive the runtime above
it.

Persist before publishing. A durable event becomes eligible for delivery only after its transaction
commits. This is the rule that turns a notification channel from an authority into a hint, and it is
the reason a lost wakeup in this system costs latency instead of data.

Recover from durable state. Notifications and socket buffers are wakeup mechanisms. Reading them as
authoritative history is the classic way to lose work that was already committed.

Separate decisions from enforcement. Capability says what authority exists, egress enforces the
network decision, and sandboxing blocks the bypasses. One component deciding and enforcing is a
component whose bug is a breach.

Make uncertainty representable. A crash during an external operation leaves an outcome nobody knows.
A design that only has success and failure has to invent one, and it will pick success.

Keep domain transitions pure. Time, randomness, and I/O enter through explicit inputs or ports, so a
transition can be replayed and compared.

Treat verification as layered evidence. Property tests, mutation tests, model checking, and fault
injection each catch a different failure; the layers are not substitutes. The tree implements six of
the eight layers and none of the formal one, which
[the verification strategy](verification/strategy.md) records.

# The planes

| Plane | The design's foundation | In the tree |
| --- | --- | --- |
| Kernel infrastructure | Rust, Tokio, axum, SQLite | Rust 2024, tokio, axum for the bus's WebSocket, `rusqlite` in two crates |
| Event contracts | A CloudEvents envelope with typed control and command contracts | A Rust envelope type that no other crate consumes; see [the shared contracts](contracts.md) |
| Execution | Durable state machines, a scheduler, a sandbox supervisor | State machines and the sandbox supervisor; the scheduler has no caller |
| Security | Capability authority, a sandbox-runtime backend, an egress gateway | Capability authority and a Landlock confiner; the egress crate validates destinations and serves no proxy |
| Applications | Rust agents and external Effect-TS applications | A Rust agent runtime, a kernel loop over it, and a terminal front end |
| User interfaces | CLI and React, with desktop integration later | A CLI, and a terminal user interface |

Pure contract and transition crates avoid runtime dependencies. Six crates are `#![no_std]`:
protocol, state machine, scheduler, capability, patch, and skill. Each one declares
`extern crate alloc`, and each one passes a check that it holds no I/O dependency.

The design's cross-language claim has no counterpart yet. There is no TypeScript in the workspace,
so the shared-contract argument rests on Rust alone.

# The charter

The durable event foundation and the security and execution plane should stay useful independently
of any particular agent runtime.[^architecture] That is the test this design sets for itself, and
the tree meets it in an unexpected way.

The event log, the bus, the state machines, the sandbox, and the capability decision engine are all
usable on their own, and several of them are already used by exactly one caller. The runtime above
them arrived last and changed none of their contracts.

The cost of that separation is visible too. Three crates hold a port whose only implementation is a
test fixture, one crate is composed nowhere, and the protocol crate's envelope machinery has one
consumer, which is its own test suite. Those are the places where a boundary was drawn before the
second user existed, and [the acceptance table](verification/acceptance.md) names each one.

# Where to go next

The design's own next step is a first vertical slice that terminates the process at each boundary
and recovers without corrupt re-execution.[^architecture] That slice exists in the kernel, and
[the kernel's concept document](crates/kernel.md) describes its shape.

The decisions the design left open, and what the tree has since settled or left standing, are in
[the decision register](decisions.md).

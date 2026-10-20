---
type: Reference
title: The workspace as it stands
description: The seventeen workspace members, who composes each one, and how they map onto the design's crate set.
resource: ../../Cargo.toml
tags: [mokmok-os, architecture, workspace]
status: draft
generated: { by: grok-build/grok-4.7, at: 2026-10-15T02:00:00Z }
stale_after: 2026-11-15T00:00:00Z
sources:
  - id: architecture
    resource: ../../mokmok-os-architecture.md
    title: mokmok OS architecture and crate responsibilities, sections 4 and 7
    author: codex/gpt-6
  - id: manifest
    resource: ../../Cargo.toml
    title: the workspace manifest
    last_modified: 2026-10-15T08:39:00Z
---

# The seventeen members

`Cargo.toml` lists seventeen members.[^manifest] Each one carries a target logical boundary from the
design's crate map, and five of them have no section in the design at all.

| Member | Design section | Dependents | How it is composed |
| --- | --- | --- | --- |
| `mokmok-protocol` | 5.1 | Thirteen members | Used for `Moment` and eight identifier markers |
| `mokmok-event-store` | 5.2 | `mokmok-kernel`, `mokmok-session-wire` | The kernel's one appender, `Db`, and the outbox row a commit becomes |
| `mokmok-event-bus` | 5.3 | `mokmok-kernel`, `mokmok-session-wire` | The daemon's relay and host clients, and the terminal's one client |
| `mokmok-session-wire` | None | `mokmok-kernel`, `mokmok-tui` | The command and record vocabulary the daemon and the terminal share |
| `mokmok-state-machine` | 5.4 | `mokmok-agent-runtime`, `mokmok-kernel`, `mokmok-session-wire`, `mokmok-tui` | The fold behind the kernel's log verify path and the phase a listing carries |
| `mokmok-agent-runtime` | 5.5 | `mokmok-kernel`, `mokmok-session-wire`, `mokmok-tui` | Stepped by the daemon's host, folded by the terminal, and carried as records on the bus |
| `mokmok-scheduler` | 5.6 | Declared by `mokmok-kernel` | Nowhere. No symbol from it appears in kernel code or tests |
| `mokmok-capability` | 5.7 | `mokmok-kernel` | Wrapped by `mokmok-kernel/src/authority.rs` |
| `mokmok-sandbox` | 5.8 | `mokmok-kernel` | Driven by `mokmok-kernel/src/execution.rs` |
| `mokmok-egress` | 5.9 | `mokmok-kernel` | The model provider's destination validation |
| `mokmok-artifact` | 5.10 | `mokmok-kernel` | `BlobStore` and `ContentHash` only |
| `mokmok-observability` | 5.11 | `mokmok-kernel` | The tracing subscriber, and the audit journal |
| `mokmok-search` | None | `mokmok-kernel` | One corpus per granted root, read by the kernel's finder |
| `mokmok-patch` | None | `mokmok-kernel` | One unified diff read into recomputed counts, applied in memory by the kernel's editor |
| `mokmok-skill` | None | `mokmok-agent-runtime`, `mokmok-kernel` | One `SKILL.md` frozen into an advert by the kernel's discovery, activated through the kernel's library |
| `mokmok-kernel` | 5.12 | Nobody | The daemon binary and its session host |
| `mokmok-tui` | None | Nobody | A bus client above the daemon |

# The physical split differs from the plan

The design proposes seven physical crates with the target boundaries grouped inside them, and an
extraction step later.[^architecture] The tree holds sixteen, one per target boundary, and never
created the grouped crates `mokmok-agent`, `mokmok-security`, or `mokmok-execution`.

| Design's initial crate | What it grouped | What exists instead |
| --- | --- | --- |
| `mokmok-protocol` | Shared contracts | `mokmok-protocol` |
| `mokmok-event-store` | Event persistence and transaction machinery | `mokmok-event-store` |
| `mokmok-event-bus` | IPC and durable delivery | `mokmok-event-bus` |
| `mokmok-agent` | Pure transitions, runtime, scheduling | `mokmok-state-machine`, `mokmok-agent-runtime`, `mokmok-scheduler` |
| `mokmok-security` | Capability and egress | `mokmok-capability`, `mokmok-egress` |
| `mokmok-execution` | Sandbox and artifacts | `mokmok-sandbox`, `mokmok-artifact` |
| `mokmok-kernel` | Composition and lifecycle | `mokmok-kernel` |
| Not in the plan | Tracing | `mokmok-observability` |
| Not in the plan | A user interface | `mokmok-tui` |
| Not in the plan | The terminal protocol over the bus | `mokmok-session-wire` |
| Not in the plan | Search over the files an agent works on | `mokmok-search` |

Two of the design's ordering decisions held. The contract layer, the event store, and the event bus
came first, and the execution and security crates followed. The design places a complex user
interface outside the first milestone, and a terminal front end now exists.

# What the member list does not say

A member with no dependent is not necessarily finished and not necessarily idle. The scheduler and
the terminal front end are the two cases, and they are opposite ones.

`mokmok-scheduler` is implemented, tested, and composed nowhere. The kernel declares it as a
dependency and does not use it, so no running process claims a lease.

`mokmok-tui` is composed by nobody and composes the bus and the wire contract, which is what an
application does. It no longer depends on `mokmok-kernel`; [the dependency rules](verification/dependency-rules.md)
still name it as a composition root, and the bus carries the session now.

# Dependency shape

The graph is acyclic and the composition root sits at the top. Only `mokmok-kernel` and
`mokmok-tui` have more than three dependents' worth of reach, and no crate below the kernel imports
it.

Six rules keep that shape, and each one runs as a check. They are stated in
[the dependency rules](verification/dependency-rules.md).

# Where each crate stands

Each member carries its own concept document, with what it owns, what it enforces, and what the
design asked of it that the code does not do. The summary of that last column across all twelve
boundaries is [the acceptance table](verification/acceptance.md).

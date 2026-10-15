---
type: Architecture Design
title: mokmok OS — Architecture and Crate Responsibilities
description: Proposed boundaries, contracts, invariants, and verification criteria for a durable, capability-constrained autonomous agent operating system.
tags: [mokmok-os, rust, architecture, durable-execution, capability-security]
status: draft
generated:
  by: codex/gpt-6
  at: "2026-10-12T05:00:52Z"
sources:
  - id: design-discussion
    resource: "User-provided mokmok OS architecture and crate responsibility discussion in this conversation"
    title: mokmok OS architecture and crate responsibility design v0.1
  - id: okf-spec
    resource: https://github.com/GoogleCloudPlatform/open-knowledge-format/blob/main/SPEC.md
    title: Open Knowledge Format v0.2
design_version: "0.1"
language: en
---

# mokmok OS — Architecture and Crate Responsibilities

> Persist every commitment. Constrain every execution. Trace every effect. Let ideas emerge.

mokmok OS is a proposed durable, capability-constrained autonomous agent operating system. It provides a foundation for agents that work continuously while preserving explicit authority, recoverable execution, and an auditable history of effects.

This document consolidates the supplied architecture discussion.[^design-discussion] It describes a target design rather than an assessment of the current repository. The repository was not inspected for this document; no implementation completion, benchmark, or security verification is asserted.

## 1. Scope and Design Status

The design separates kernel mechanisms from agent behavior. Infrastructure guarantees persistence, scheduling, isolation, authorization, and auditability. Agents use these mechanisms to reason, act, learn, and discover.

The twelve crates below describe target logical boundaries. The initial implementation may group related responsibilities into fewer crates. A boundary should become a separate crate when its owned state, invariants, contracts, and validation requirements justify the split.

Statements using **must** define intended requirements. Proposed drivers, integration mechanisms, and deployment choices remain provisional until validated. API examples illustrate boundaries and are not complete implementations.

This is a standalone OKF concept document: UTF-8 Markdown with YAML frontmatter and source-keyed footnotes. The format follows OKF v0.2.[^okf-spec] The `design_version` and `language` fields are producer-defined extensions. No `verified` field is included because this artifact has not received independent architectural or implementation verification.

## 2. Architectural Principles

1. **Own invariants explicitly.** Every crate owns a defined set of state, transitions, or enforcement guarantees.
2. **Keep dependencies acyclic.** Lower-level components never depend on the composition root or agent-specific orchestration.
3. **Persist before publishing.** Durable events become eligible for delivery only after their transaction commits.
4. **Recover from durable state.** Notifications and socket buffers are wakeup mechanisms, not authoritative history.
5. **Separate decisions from enforcement.** Capability determines authority, egress enforces network decisions, and sandboxing blocks bypass paths.
6. **Make uncertainty representable.** A crash during an external operation may leave an unknown outcome requiring reconciliation.
7. **Keep domain transitions pure.** Time, randomness, and I/O enter through explicit inputs or ports.
8. **Treat verification as layered evidence.** Property tests, mutation tests, model checking, and fault injection address different failure modes.

## 3. Target Technology and Execution Model

| Plane | Proposed foundation | Purpose |
| --- | --- | --- |
| Kernel infrastructure | Rust, Tokio, axum, SQLite | Durable coordination and IPC |
| Event contracts | CloudEvents envelope plus typed control and command contracts | Shared event representation |
| Execution | Durable state machines, scheduler, sandbox supervisor | Recoverable work |
| Security | Capability authority, sandbox-runtime backend, egress gateway | Controlled effects |
| Applications | Rust agents and external Effect-TS applications | Agent behavior |
| User interfaces | CLI and React; desktop integration later | Inspection and control |

Pure contract and transition crates should avoid runtime dependencies. `no_std` is appropriate where supported by their actual data structures and dependencies; allocation requirements must be explicit. Async infrastructure crates use the standard library and Tokio where needed.

Rust and Effect-TS applications share contracts without requiring the kernel to understand application-specific agent logic.

## 4. Target Crate Responsibility Map

| Crate | Owns | Does not own |
| --- | --- | --- |
| `mokmok-protocol` | Message contracts, identifiers, envelope rules, schema evolution | Persistence or delivery |
| `mokmok-event-store` | Durable event transactions, positions, checkpoints, deduplication records | Socket transport or domain decisions |
| `mokmok-event-bus` | Subscriptions, replay, delivery flow control, ACK handling | Business transitions |
| `mokmok-state-machine` | Pure Agent, Session, Turn, and Execution transitions | LLM calls, storage, or OS operations |
| `mokmok-agent-runtime` | Context, model/tool orchestration, budgets, resumption, reconciliation | Sandbox backend internals |
| `mokmok-scheduler` | Durable wakeups, deadlines, leases, fencing | Agent reasoning or task content |
| `mokmok-capability` | Principals, policy, grants, approval requirements, revocation | Process or network enforcement |
| `mokmok-sandbox` | Process supervision, isolation, limits, termination, cleanup | Business authorization decisions |
| `mokmok-egress` | Proxying, destination enforcement, DNS/IP validation, network audit | SaaS operation authorization |
| `mokmok-artifact` | Blob storage, metadata, retention, garbage collection | Event history |
| `mokmok-observability` | Trace, metric, log, redaction, audit export | Authoritative domain state |
| `mokmok-kernel` | Composition, lifecycle, readiness, recovery coordination | Individual domain policies |

## 5. Crate Contracts and Invariants

### 5.1 `mokmok-protocol`

**Responsibility:** Define shared data contracts independently of transport and storage.

Suggested modules: `identifiers`, `command`, `event`, `envelope`, `subscription`, `error`, and `version`.

Contracts include typed Agent, Session, Turn, Execution, Command, and Event identifiers; commit positions; publish receipts; subscription requests; ACK frames; and protocol errors. Typed identifiers must prevent accidental cross-domain substitution.

CloudEvents envelopes describe domain events. Commands, queries, control frames, and telemetry retain distinct semantics:

| Message kind | Meaning |
| --- | --- |
| Command | A requested operation |
| Event | A fact that has occurred |
| Query | A request for state or history |
| Control | Subscription, acknowledgement, or replay coordination |
| Telemetry | Transient progress or measurement |

**Invariants:** Construction and decoding validate contract constraints. Unsupported schema versions have an explicit compatibility policy. Correlation, causation, and trace metadata preserve their distinct meanings.

**Dependencies:** Small contract libraries such as `serde`; no Tokio, SQLite driver, or axum dependency. Runtime-facing ports should not accumulate here merely because they are shared.

**Validation:** Rust/TypeScript fixtures, round-trip properties, malformed-input cases, and schema compatibility checks.

### 5.2 `mokmok-event-store`

**Responsibility:** Guarantee durable transaction boundaries and authoritative event order.

Suggested modules: `append`, `transaction`, `reader`, `checkpoint`, `inbox`, `outbox`, `migration`, and `sqlite`.

An append contract must support expected aggregate version checks, command deduplication, event insertion, and participating state/outbox updates in one transaction. Consumer inbox records and local state updates must likewise be able to share a transaction.

**Invariants:**

- An append batch commits atomically.
- Aggregate versions are unique and follow the aggregate's concurrency rules.
- Visible commit positions agree with the committed append order.
- Repeated command identities cannot commit the same state change twice.
- A durable checkpoint never moves backward and cannot advance beyond what its acknowledgement contract permits.
- Atomic inbox deduplication prevents repeated local effects when the consumer state participates in that transaction.

This does not establish exactly-once effects for arbitrary remote services. Those require an external idempotency contract or outcome reconciliation.

The store owns common transaction machinery and event persistence. Scheduler, approval, session, and artifact repositories keep their domain contracts near their owners. SQLite adapters join the required operations explicitly at the infrastructure boundary.

**Dependencies:** `mokmok-protocol` and persistence libraries; no axum dependency.

**Validation:** Real SQLite transactions, duplicate and concurrent append tests, model-based version/checkpoint properties, mutation testing, and crash injection.

### 5.3 `mokmok-event-bus`

**Responsibility:** Manage subscription lifecycle and delivery from committed history.

Suggested modules: `publisher`, `subscriber`, `dispatcher`, `notification`, `websocket`, `connection`, and `backpressure`.

The bus reads committed events from the store. A `tokio::sync::watch<CommitPosition>` channel announces durable progress; it does not carry authoritative event payloads. Subscription control and event delivery should be independently manageable.

**Invariants:**

- Uncommitted events are never delivered.
- A missing or coalesced notification cannot erase committed history.
- Subscribers recover from their durable checkpoints after disconnection.
- Unacknowledged deliveries remain replayable within the defined retention contract.
- Slow subscribers have bounded resources and do not block unrelated subscribers.

A successful socket write is not a processing acknowledgement. ACK scope, ordering, filtering, and checkpoint advancement must be defined together. Filtered subscriptions need an explicit rule for positions that contain no matching event.

Delivery should always catch up from the store before waiting. A reconciliation mechanism must cover the crash window between database commit and notification; the exact startup and periodic checks remain an implementation decision.

**Dependencies:** `mokmok-protocol`, `mokmok-event-store`, and transport/runtime libraries.

**Validation:** Replay, reconnect, bounded buffering, slow-consumer behavior, missed wakeups, and commit/notification race tests.

### 5.4 `mokmok-state-machine`

**Responsibility:** Define pure transitions for Agent, Session, Turn, and Execution state.

Suggested modules: `agent`, `session`, `turn`, `execution`, `transition`, and `invariant`.

```rust
// Illustrative contracts; concrete types are intentionally omitted.
pub fn decide(
    state: &TurnState,
    command: &TurnCommand,
) -> Result<Vec<TurnEvent>, DomainError>;

pub fn evolve(
    state: TurnState,
    event: &TurnEvent,
) -> Result<TurnState, DomainError>;
```

**Invariants:** Illegal transitions are rejected; terminal states cannot resume through unauthorized transitions; execution results cannot be applied twice; cancellation and timeout have explicit semantics; replay reconstructs deterministic state.

Do not gather unrelated state machines into this crate solely because they share a transition-shaped API.

**Dependencies:** `mokmok-protocol`; no Tokio, SQLite, or external I/O.

**Validation:** Stateful property tests against a reference model, mutation tests, and focused formal verification harnesses where useful. Bounded model checking results must state their bounds and assumptions.

### 5.5 `mokmok-agent-runtime`

**Responsibility:** Orchestrate model invocations, tools, sessions, and durable continuation.

Suggested modules: `agent`, `session`, `turn`, `orchestrator`, `model`, `tool`, `context`, `budget`, and `recovery`.

The runtime constructs context, chooses model providers, interprets tool calls, issues commands, applies persisted outcomes, and manages retry, cancellation, token/cost budgets, and reconciliation.

**Invariants:** An action requiring a durable commitment is recorded before dispatch. Recovery distinguishes pending work, completed work, and unknown external outcomes. Cancellation prevents acquisition of new execution authority under the cancelled execution's contract.

The runtime consumes ports such as `ToolExecutor` and persistence interfaces. It does not own the sandbox supervisor or depend on `SrtBackend` details. A built-in Rust runtime and an external Effect-TS application are separate consumers of kernel services.

**Dependencies:** `mokmok-protocol`, `mokmok-state-machine`, and consumer-owned ports.

**Validation:** Stop/resume at turn boundaries, duplicate results, provider failures, budget limits, cancellation races, and ambiguous external outcomes.

### 5.6 `mokmok-scheduler`

**Responsibility:** Convert persisted timing and wake conditions into work claims and commands.

Suggested modules: `schedule`, `heartbeat`, `lease`, `fencing`, `wakeup`, and `recovery`.

Supported triggers include cron, one-shot schedules, heartbeats, events, retry deadlines, and expired leases. The scheduler knows when work is eligible; it does not know how an agent should solve the task.

**Invariants:** Lease ownership and fencing generation are validated when protected state is updated. A stale worker cannot overwrite state owned by a newer generation. Duplicate wakeups do not bypass the work-claim contract.

Fencing only protects operations that enforce the token. It cannot independently prevent duplicate effects at an external API.

**Dependencies:** `mokmok-protocol`; storage and clock behavior enter through ports.

**Validation:** Lease expiry, restart, duplicate wakeups, time-related edge cases, stale workers, and concurrent claims.

### 5.7 `mokmok-capability`

**Responsibility:** Decide which principal may perform an operation on a resource in a given execution context.

Suggested modules: `principal`, `resource`, `permission`, `policy`, `grant`, `approval`, `revocation`, and `decision`.

The decision contract returns an allowed grant, a denial reason, or an approval requirement. Approval requirements must bind to the intended operation and context. A durable approval workflow may become a separate `mokmok-approval` crate when its lifecycle warrants it.

**Invariants:** Default denial; grants cannot exceed parent authority; expiry and revocation are enforced under explicit rules; authority changes retain durable rationale; an LLM's claim cannot expand its own privileges.

**Dependencies:** `mokmok-protocol`; no network forwarding or process control.

**Validation:** Grant attenuation, expiry, revocation, approval binding, policy combinations, and authorization decision properties.

### 5.8 `mokmok-sandbox`

**Responsibility:** Execute untrusted code inside an enforced OS isolation boundary.

Suggested modules: `supervisor`, `backend`, `execution`, `process`, `workspace`, `limits`, `cleanup`, and `srt`.

The supervisor consumes a backend interface. sandbox-runtime (`srt`) is a proposed backend rather than a domain-level dependency. Responsibilities include process creation/reaping, filesystem exposure, execution-scoped workspaces, limits, deadlines, signal handling, output collection, artifact handoff, and cleanup.

**Invariants:** Execution uses validated authority; direct network paths cannot bypass the approved egress boundary; teardown leaves no unmanaged child processes within the supported supervision model; limits and termination behavior are enforceable and observable.

OS resource limits and agent cost/token budgets are separate contracts.

**Dependencies:** Protocol and grant-validation contracts, OS/runtime libraries, and backend integration.

**Validation:** Real OS isolation tests, descendant-process cleanup, deadline enforcement, filesystem escape attempts, and network bypass attempts on every supported backend/platform.

### 5.9 `mokmok-egress`

**Responsibility:** Mediate sandbox network traffic and enforce destination decisions.

Suggested modules: `proxy`, `connect`, `dns`, `destination`, `policy_client`, `connection`, `rate_limit`, and `audit`.

Responsibilities include HTTP/CONNECT proxying, host/port/protocol validation, DNS and destination checks, private/loopback/link-local restrictions, rate limiting, and network audit records.

**Invariants:** The actual connected address agrees with the validated destination. Alternate addresses and resolution changes cannot bypass policy. Network decisions retain execution identity and audit context. Enforcement semantics stay aligned with the capability authority.

Application operations such as creating a GitHub issue or editing a Drive document require managed tools or a capability gateway; allowing a host does not authorize every operation on that host.

**Dependencies:** `mokmok-protocol`, capability validation ports, and network libraries; no direct agent-state mutation.

**Validation:** DNS rebinding scenarios, mixed IPv4/IPv6 addresses, forbidden destination classes, proxy bypass, malformed CONNECT requests, and audit attribution.

### 5.10 `mokmok-artifact`

**Responsibility:** Persist large results and generated files outside the event log.

Suggested modules: `blob`, `metadata`, `filesystem`, `content_hash`, `retention`, and `gc`.

Artifacts include stdout/stderr, large model responses, source files, experiment results, screenshots, binaries, and workspace snapshots. Events store references and essential metadata. Transient token streams may use ephemeral transport unless explicitly retained as artifacts.

**Invariants:** Durable references do not point to incompletely persisted data. Content hashes establish content identity, not reading authority. Referenced artifacts remain protected from premature collection under the retention contract. Interrupted writes and unreferenced blobs are recoverable or collectible.

The blob/event transaction boundary requires an explicit write-order and recovery protocol; content addressing alone does not solve it.

**Dependencies:** `mokmok-protocol`, storage libraries, and authorization ports as required.

**Validation:** Interrupted writes, reference publication, authorization, retention, and garbage-collection races.

### 5.11 `mokmok-observability`

**Responsibility:** Export operational traces, metrics, logs, and durable audit records.

Suggested modules: `tracing`, `metrics`, `audit`, `exporter`, and `redaction`.

Observations include event delivery lag, transaction latency, sandbox health, execution cost, security decisions, and effect traces.

**Invariants:** Important security audit facts do not depend solely on a transient telemetry exporter. Redaction preserves useful correlation while removing protected payloads according to policy. Export failure does not redefine authoritative domain outcomes.

Security-critical audit persistence belongs in a durable transaction or equivalent durable audit mechanism; this crate handles instrumentation and export.

**Dependencies:** Primarily generic tracing/telemetry libraries; avoid unnecessary domain dependencies.

**Validation:** Exporter outage, redaction, trace propagation, and durable-audit delivery behavior.

### 5.12 `mokmok-kernel`

**Responsibility:** Assemble services and coordinate their lifecycle and recovery.

Suggested modules: `config`, `bootstrap`, `services`, `lifecycle`, `recovery`, and `shutdown`.

The kernel wires adapters to ports, initiates migrations, starts the UDS listener and supervisors, coordinates recovery, and exposes health/readiness. `apps/mokmokd` is a thin binary invoking kernel bootstrap.

**Invariants:** Required recovery runs before affected services become ready. Startup/shutdown ordering respects service dependencies. Recovery invokes domain-owned logic instead of duplicating it inside the kernel.

**Dependencies:** The service crates needed for assembly. No lower-level crate depends on `mokmok-kernel`.

**Validation:** Startup failure, migration failure, orderly shutdown, abrupt termination, restart, and recovery sequencing.

## 6. Dependency and Transaction Boundaries

Ports are defined near their consumers. Concrete adapters are composed in infrastructure code. The protocol crate owns data contracts rather than every trait in the system.

| Consumer | Contract or dependency | Ownership rule |
| --- | --- | --- |
| Event store | Protocol types | Store owns durable event semantics |
| Event bus | Event-store read/append/checkpoint contracts | Bus owns delivery semantics |
| State machine | Protocol types | Transitions remain pure |
| Agent runtime | Transition functions, `ToolExecutor`, persistence ports | Runtime states its required capabilities |
| Scheduler | Job repository and clock ports | Scheduler owns eligibility and claims |
| Sandbox/egress | Validated authority and validation ports | Enforcement does not mint authority |
| Kernel | Concrete services and adapters | Composition stays at the top |

For state and events that must commit together, the infrastructure adapter exposes one unit of work. Calling independent async repositories sequentially is not sufficient to establish atomicity.

The proposed initial storage approach is `rusqlite` with a dedicated database worker and one serialized append path. Its transaction API, shutdown behavior, and commit/notification ordering must be validated before adoption.

## 7. Initial Workspace and Extraction Plan

Start with the following physical crates while preserving the target logical boundaries:

| Initial crate | Responsibilities initially grouped | Later extraction |
| --- | --- | --- |
| `mokmok-protocol` | Shared contracts | Remains independent |
| `mokmok-event-store` | Event persistence and transaction machinery | Shared persistence machinery only if justified |
| `mokmok-event-bus` | IPC and durable delivery | Remains independent |
| `mokmok-agent` | Pure transitions, runtime, scheduling | State machine, agent runtime, scheduler |
| `mokmok-security` | Capability and egress | Capability, egress; approval if needed |
| `mokmok-execution` | Sandbox and artifacts | Sandbox, artifact |
| `mokmok-kernel` | Composition and lifecycle | Remains composition root |

Introduce tracing immediately. Extract observability when reusable export and redaction contracts become substantial.

Implementation priority is protocol, event store, and event bus first; execution and security next; agent orchestration and complete lifecycle integration after those foundations. Minimal bootstrap code may be needed throughout this sequence.

## 8. First Vertical Slice

The first end-to-end milestone is:

1. Accept a CLI command with an idempotency identity.
2. Commit the requested transition and durable events.
3. Start an agent turn and record its tool execution request.
4. Validate authority and establish the sandbox.
5. Execute the tool under the enforced constraints.
6. Persist the result or the explicit unknown-outcome state.
7. Resume the turn from durable state.
8. Commit turn completion.

Terminate the process at each boundary and verify recovery without state corruption or unsafe blind re-execution. For external operations whose outcome cannot be proven, recovery must reconcile or surface uncertainty rather than assuming success or failure.

Memory, discovery, and a complex UI are outside this milestone. Future discovery features must distinguish Observation, Hypothesis, Evidence, and Validated Insight, preserve provenance, and use the normal capability and approval contracts for effects.

## 9. Verification Strategy

The quality strategy is **Property-first, Mutation-verified, Fault-injected**.

| Evidence layer | Rust | TypeScript | Main purpose |
| --- | --- | --- | --- |
| Unit | `cargo test` | Vitest | Local behavior |
| Property/model-based | proptest | fast-check | Invariants across generated sequences |
| Mutation | cargo-mutants | StrykerJS | Detect weaknesses in assertions |
| Fuzzing | cargo-fuzz | Dedicated fuzz/property harnesses | Unexpected input behavior |
| Concurrency | Loom and Tokio tests | fast-check scheduler | Controlled interleavings |
| Integration | Real SQLite and OS backends | Real IPC/API adapters | Actual boundary behavior |
| Fault injection | Process, storage, network failures | Protocol/workflow failures | Recovery and uncertainty handling |
| Formal verification, where applicable | Kani or focused model checking | Explicit reference models | Specified properties under stated assumptions |

Critical cross-component properties include: no delivery before commit; recovery despite notification loss; unique aggregate versions; monotonic valid checkpoints; command deduplication; no new authority for cancelled executions; rejection of stale fencing tokens; denied unauthorized connections; managed process cleanup; and reconciliation of incomplete work after a crash.

Investigate surviving mutants individually. A high mutation score measures test sensitivity; it is not a proof of durability, isolation, or exactly-once external effects.

CI should also check the dependency rules: protocol has no domain dependency; pure transitions have no I/O dependency; storage has no transport dependency; scheduler has no model-provider dependency; capability performs no enforcement I/O; and lower-level crates never import the kernel.

## 10. Acceptance Criteria

| Boundary | Required evidence before calling it complete |
| --- | --- |
| Protocol | Rust/TypeScript conformance and explicit version compatibility |
| Event store | Correct concurrent/duplicate append and crash recovery |
| Event bus | Replay, reconnect, flow control, and missed-notification recovery |
| State machine | Illegal transitions rejected; deterministic replay; model agreement |
| Agent runtime | Safe mid-turn recovery and explicit unknown outcomes |
| Scheduler | Lease expiry, stale fencing rejection, duplicate wake tolerance |
| Capability | Authority attenuation, approval binding, expiry/revocation |
| Sandbox | Verified isolation, termination, and cleanup on supported platforms |
| Egress | Destination consistency, DNS rebinding resistance, bypass prevention |
| Artifact | Durable references, authorization, and safe collection |
| Observability | Traceable decisions/effects and durable critical audit facts |
| Kernel | Consistent startup, shutdown, readiness, and recovery |

These are acceptance requirements, not statements of current implementation status.

## 11. Open Decisions

| Decision | Working direction | Evidence needed |
| --- | --- | --- |
| SQLite driver | `rusqlite` plus dedicated worker | Contention, cancellation, transaction ownership, shutdown tests |
| Commit positions | Single append path | Ordering under concurrency and restart |
| Commit notification | `watch` as wakeup hint | Crash gap, subscription race, recovery checks |
| Replay/retention | Checkpoint-aware retention and snapshots | Offline-consumer and expired-cursor semantics |
| ACK semantics | Durable monotonic checkpoints | Filters, out-of-order processing, cumulative ACK rules |
| sandbox-runtime integration | Compare CLI and sidecar | Lifecycle, authority transfer, platform behavior |
| Egress integration | Align with sandbox isolation | Actual bypass resistance and identity attribution |
| Rust/Effect-TS boundary | Typed RPC plus event contracts | Fixtures and reconnect/recovery behavior |
| IPC identity | UDS peer credentials plus capabilities | Platform support, credential binding, delegation model |
| External effect recovery | Idempotency where available; reconciliation otherwise | Provider-specific outcomes and failure windows |
| Artifact publication | Persist blob before publishing reference | Crash recovery and GC coordination |

The immediate design experiments should prioritize the SQLite commit/notification boundary and the sandbox/egress enforcement boundary.

## 12. Architecture Charter

The durable event foundation and security/execution plane should remain useful independently of a particular agent runtime. Stable contracts allow Rust agents, Effect-TS applications, and future agent implementations to coexist without weakening infrastructure guarantees.

Kernel mechanisms establish durable commitments, constrained execution, and traceable effects. Agent applications retain room to reason, learn, and discover within those boundaries.

[^design-discussion]: User-provided mokmok OS architecture and crate responsibility discussion supplied in this conversation; normative design input, not repository verification.
[^okf-spec]: [Open Knowledge Format v0.2 specification](https://github.com/GoogleCloudPlatform/open-knowledge-format/blob/main/SPEC.md); consulted for document structure and provenance metadata.

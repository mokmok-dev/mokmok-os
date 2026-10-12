# Round 1 frozen interface

The target is the crate map and invariants of `mokmok-os-architecture.md`. This file freezes the
public surface each crate in this round exposes, so several writers can build against one contract
at once. It is a brief for the delegate that owns a crate, not a second architecture document.

An implementer owns one crate directory. Everything outside it (the root `Cargo.toml`, every other
crate, `flake.nix`, `scripts/`) is owned by the integrator. If a signature below cannot work, say
so and stop; do not widen the surface, and do not edit a file outside the crate.

## Conventions the gate enforces

- `cargo fmt --all -- --check`, with `.rustfmt.toml`: `fn_params_layout = "Vertical"`, so a
  function's parameters go one per line once they do not fit on one line.
- `cargo clippy --workspace --all-targets --all-features -- --deny warnings`, with
  `[workspace.lints.clippy]` set to `all` plus `pedantic`, `unwrap_used`, `expect_used`, `panic`,
  `missing_const_for_fn`, `todo`, and more. `.clippy.toml` allows `unwrap`, `expect`, and `panic`
  in test code only.
- `cargo doc --workspace --no-deps --all-features` and doc tests (`cargoDocTest`), so every
  public item carries a doc comment and any ```` ```rust ```` example compiles.
- A private module's internal types are `pub`, never `pub(crate)`: `clippy::redundant_pub_crate`.
- `unsafe_code` is denied workspace-wide.
- A crate that tags a serde enum without pulling `std` needs `features = ["alloc", "derive"]`.
- The mutation gate `scripts/mutants.sh` is per package. Match-heavy code scores nothing, so
  express a rule as a comparison that a mutant can flip.
- `clippy::missing_const_for_fn` is denied, and it fires on a body that is const-evaluable. That
  forces a workaround where the natural body is not const: `TryFrom` is not const yet, so scale a
  `Duration` with `as_secs()`/`subsec_millis()` instead of `u128 as u64`. `mokmok-protocol`'s
  `moment.rs` shows the shape.
- A `#![no_std]` crate does not get `thiserror` here. Implement `core::fmt::Display` by hand and
  `impl core::error::Error for X {}`, which is stable. `std` crates use `thiserror` as the landed
  ones do.

## Comments

A comment earns its place only for a non-obvious why the code cannot show: an outside constraint, a
hazard the reader would walk into, the reason the obvious improvement breaks something, or the
source name behind an English-only symbol. The code carries how. The test name and its assertion
carry what. Do not narrate phases, do not restate the line below the comment, do not cite this file.

## Crate specs

### `mokmok-protocol` (extend; `#![no_std]` + `alloc`)

Owns message contracts, identifiers, envelope rules, schema evolution. No Tokio, no SQLite, no
axum. Modules to add: `moment`, `version`, `envelope`, `message`, `error`, and new markers.

```rust
// moment.rs: a wall-clock instant, so a pure crate can hold time without std.
pub struct Moment(u64);                 // milliseconds since the Unix epoch
impl Moment {
    pub const EPOCH: Self;
    pub const fn from_millis(millis: u64) -> Self;
    pub const fn as_millis(self) -> u64;
    pub fn checked_add(self, by: core::time::Duration) -> Option<Self>;
    pub fn checked_sub(self, by: core::time::Duration) -> Option<Self>;
    pub fn saturating_add(self, by: core::time::Duration) -> Self;   // clamping is the caller's call, not a wrap
}
// Ord, Copy, Eq, Hash, Debug, Display, serde transparent.

// version.rs: the compatibility policy for every contract in the workspace.
pub struct SchemaVersion { major: u16, minor: u16 }
impl SchemaVersion {
    pub const CURRENT: Self;
    pub const fn new(major: u16, minor: u16) -> Self;
    pub const fn major(self) -> u16;
    pub const fn minor(self) -> u16;
    // Same major, producer no newer than this reader.
    pub const fn accepts(self, producer: Self) -> bool;
}
pub enum Compatibility { Current, Older, Unsupported }

// envelope.rs
pub struct Source(String);              // non-empty, no whitespace, <= 253 bytes
pub struct Data { media_type: MediaType, bytes: Vec<u8> }
impl Data {
    pub fn json<T: serde::Serialize>(value: &T) -> Result<Self, ProtocolError>;
    pub fn from_json(bytes: impl Into<Vec<u8>>) -> Result<Self, ProtocolError>;  // validates UTF-8
    pub fn media_type(&self) -> &MediaType;
    pub fn bytes(&self) -> &[u8];
    pub fn to_json<T: serde::de::DeserializeOwned>(&self) -> Result<T, ProtocolError>;
}
pub struct TraceContext { trace: TraceID, span: SpanID, sampled: bool }
impl TraceContext { pub fn parse(header: &str) -> Result<Self, ProtocolError>; }  // W3C traceparent
pub struct Envelope {
    pub version: SchemaVersion,
    pub id: MessageID,
    pub source: Source,
    pub kind: MessageKind,
    pub subject: Subject,
    pub at: Moment,
    pub correlation: Option<CorrelationID>,
    pub causation: Option<EventID>,
    pub trace: Option<TraceContext>,
    pub data: Data,
}
impl Envelope {
    pub fn new(id: MessageID, source: Source, kind: MessageKind, subject: Subject, at: Moment, data: Data) -> Self;
    pub fn with_correlation(self, correlation: CorrelationID) -> Self;
    pub fn with_causation(self, causation: EventID) -> Self;
    pub fn with_trace(self, trace: TraceContext) -> Self;
    pub fn to_json(&self) -> Result<Vec<u8>, ProtocolError>;
    // Validates before it trusts: schema version, source, media type, and the empty payload.
    pub fn from_json(bytes: &[u8]) -> Result<Self, ProtocolError>;
}
pub enum Subject { Agent(AgentID), Session(SessionID), Turn(TurnID), Execution(ExecutionID), Artifact(ArtifactID), Principal(PrincipalID), Worker(WorkerID) }
impl Subject { pub const fn as_uuid(&self) -> uuid::Uuid; }

// message.rs: the taxonomy, and the one extra field a command carries.
pub enum MessageKind { Command, Event, Query, Control, Telemetry }
pub struct Command { pub id: CommandID, pub issuer: PrincipalID, pub envelope: Envelope }
impl Command {
    pub fn new(id: CommandID, issuer: PrincipalID, envelope: Envelope) -> Result<Self, ProtocolError>;
    // The command id is the idempotency identity: a retry that reuses it replays the recorded
    // outcome, which is what the store's dedup row is keyed on.
    pub fn to_json(&self) -> Result<Vec<u8>, ProtocolError>;
    pub fn from_json(bytes: &[u8]) -> Result<Self, ProtocolError>;
}

// error.rs
pub enum ProtocolError { UnsupportedVersion { seen: SchemaVersion, supported: SchemaVersion }, Malformed(serde_json::Error), InvalidSource, InvalidMediaType, InvalidData, EmptyPayload }
```

The scaffold already adds the markers `Message`, `Correlation`, `Principal`, `Worker` and the
aliases `MessageID`, `CorrelationID`, `PrincipalID`, `WorkerID` in `id/marker.rs`. Use them as they
are; do not rename them or add a second id for the same concept.

Tests. Round-trip properties for `Moment`, `TraceContext::parse`, and `Envelope`; a literal
malformed-traceparent case asserting the error variant; a version policy case pinning that a newer
minor is refused and an older one is accepted; a construction case proving `Source` and `Data`
reject what they say they reject.

### `mokmok-state-machine` (extend; stays `#![no_std]` + `alloc`, no I/O)

Add the explicit unknown outcome and a per-turn machine. Keep `session.rs`'s rules as they are.

- `Resolution::Unknown`: the executor could not prove the outcome, so reconciliation is required
  before any result is claimed. `ResultApplied` after it is the existing `Rejection::NoResult`.
- `turn.rs`: a `Turn` fact machine. Facts: `TurnOpened { number: TurnNumber }`,
  `ModelRequested { attempt: Attempt }`, `ModelAnswered`, `ToolRequested { execution: ExecutionID, deadline: Deadline }`,
  `ToolResolved { execution: ExecutionID, resolution: Resolution }`, `TurnSettled { outcome: TurnOutcome }`.
  `TurnOutcome::{Completed, Cancelled, Failed}`. Rules to enforce and test: no tool before a model
  answer (or an explicit refusal), a turn settles once, a settled turn admits no further fact,
  replay reconstructs the driven state, and a resolution for an execution the turn never requested
  is refused. Same `apply`/`Ack`/`Rejection` shape as `Session`.

Tests: property tests over generated fact sequences against a reference fold, plus focused cases
for each rule, each asserting a literal `Ack` or `Rejection`.

### `mokmok-capability` (new; `#![no_std]` + `alloc`, serde, no network, no process control)

```rust
pub struct Principal { id: PrincipalID, kind: PrincipalKind, name: Name }
pub enum PrincipalKind { Human, Agent, Service }
pub enum Resource { Path(String), Tool(Name), Model(Name), Host { host: String, port: u16 } }
pub enum Action { Read, Write, Execute, Connect, Invoke }
pub struct ActionSet(u8);   // set semantics, like the sandbox's Access
pub struct Permission { resource: Resource, actions: ActionSet }
pub struct GrantId(...);    // branded u64 or uuid; monotonic or caller-supplied, your call
pub struct Grant { /* private fields */ }
impl Grant { pub fn id(&self) -> GrantId; pub fn principal(&self) -> PrincipalID; pub fn permission(&self) -> &Permission; pub fn not_after(&self) -> Option<Moment>; pub fn parent(&self) -> Option<GrantId>; }
pub struct Policy { /* ordered rules, deny wins at equal specificity */ }
pub enum Decision { Allow(Grant), Deny(Denial), Approve(Requirement) }
pub struct Denial { reason: DenialReason }        // NoMatchingRule, Expired, Revoked, NotAttenuated, ...
pub struct Requirement { operation: Digest, context: Digest, not_after: Option<Moment> }
pub struct Authority { /* grants, revocations, policy */ }
impl Authority {
    pub fn new(policy: Policy, now: Moment) -> Self;
    // The only path that mints a Grant. `parent` is a grant the caller already holds, and the
    // requested permission must be a subset of it; an attenuated grant cannot outlive its parent.
    pub fn issue(&mut self, parent: &Grant, principal: PrincipalID, permission: Permission, not_after: Option<Moment>, now: Moment) -> Result<Grant, Attenuation>;
    pub fn root(&mut self, principal: Principal, permission: Permission, now: Moment) -> Grant; // setup
    pub fn decide(&self, request: &Request, now: Moment) -> Decision;
    pub fn approve(&mut self, requirement: &Requirement, by: PrincipalID, now: Moment) -> Result<Grant, ApprovalError>;
    pub fn revoke(&mut self, grant: GrantId, now: Moment) -> bool;
    pub fn revoke_principal(&mut self, principal: PrincipalID, now: Moment);
}
pub struct Request { pub principal: PrincipalID, pub action: Action, pub resource: Resource, pub context: Context }
pub struct Context { pub session: Option<SessionID>, pub execution: Option<ExecutionID>, pub turn: Option<TurnID> }
```

`Grant` has no public constructor: privilege cannot be widened because there is no expression that
builds one except `Authority::issue` against a held parent. `Requirement` binds a digest over
(principal, action, resource, context), so an approval for one operation cannot authorize another.

Tests: attenuation of each field refused; a grant cannot outlive its parent; expired and revoked
grants deny; default deny with no rule; approval binding (approve X, present Y, expect deny); a
denied action on one resource leaves another allowed.

### `mokmok-scheduler` (new; `#![no_std]` + `alloc`, serde; clock and storage enter as ports)

```rust
pub trait Clock { fn now(&self) -> Moment; }
pub struct JobId(...); pub struct Generation(u64); pub struct WorkerId — use protocol's WorkerID
pub enum Schedule { Once(Moment), After(Duration), Interval { anchor: Moment, every: Duration } }
pub enum JobState { Pending, Leased(Lease), Succeeded, Failed { attempts: u32 }, Dead }
pub struct Lease { holder: WorkerID, generation: Generation, expires: Moment }
pub struct Job { /* id, schedule, state, attempts, next_at */ }
pub struct Scheduler<C: Clock> { /* jobs, clock */ }
impl<C: Clock> Scheduler<C> {
    pub fn new(clock: C) -> Self;
    pub fn add(&mut self, schedule: Schedule, at: Moment) -> JobId;
    pub fn due(&self, at: Moment) -> Vec<JobId>;               // eligible now: pending, interval, retry deadline
    pub fn claim(&mut self, job: JobId, holder: WorkerID, ttl: Duration, at: Moment) -> Result<Lease, ClaimError>;
    pub fn renew(&mut self, lease: &Lease, ttl: Duration, at: Moment) -> Result<Lease, Stale>;
    pub fn settle(&mut self, lease: &Lease, outcome: Outcome, at: Moment) -> Result<(), Stale>;
    pub fn expire(&mut self, at: Moment) -> Vec<JobId>;
    pub fn snapshot(&self) -> Snapshot;   // serde; the durable adapter stores this
    pub fn restore(&mut self, snapshot: Snapshot) -> Result<(), Corrupt>;
}
```

Rules: a claim on an unexpired lease held by another worker is refused; the same holder re-claiming
gets the same generation back (a duplicate wakeup is not a second claim); every generation increase
is the fence, and `renew`/`settle` with an older generation answer `Stale`; an expired lease returns
its job to `Pending` with a new generation; `Settle(Failed)` sets the retry deadline from `at` and
`attempts`; attempts past a bound move to `Dead`.

Tests: lease expiry, stale fencing rejection, duplicate wake tolerance, restart via
snapshot/restore, interval eligibility across a clock jump.

### `mokmok-artifact` (new; std)

Blob storage, metadata, retention, GC. Modules: `blob`, `metadata`, `filesystem`, `content_hash`,
`retention`, `gc`.

```rust
pub struct ContentHash([u8; 32]);        // sha256; Display and FromStr as lowercase hex
pub struct BlobStore { root: PathBuf }
impl BlobStore {
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, Error>;
    // tmp file in the same directory, write, fsync, rename into place: a reader never sees a
    // partially written blob, and an interrupted write is invisible rather than referenced.
    pub fn put(&self, bytes: &[u8]) -> Result<ContentHash, Error>;
    pub fn path(&self, hash: ContentHash) -> PathBuf;
    pub fn len(&self, hash: ContentHash) -> Result<u64, Error>;
    pub fn read(&self, hash: ContentHash) -> Result<Vec<u8>, Error>;
    pub fn remove(&self, hash: ContentHash) -> Result<bool, Error>;
}
pub struct Catalog<S: ReferenceStore> { /* references by ArtifactID, store port */ }
pub trait ReferenceStore { fn referenced(&self, hash: ContentHash) -> Result<bool, Error>; fn list(&self) -> Result<Vec<Reference>, Error>; }
pub struct Authorizer — a port: fn may_read(&self, principal: PrincipalID, artifact: ArtifactID) -> bool
pub struct Retention { pub min_age: Duration, pub max_bytes: u64 }
pub struct Collector<'a> { ... }
impl Collector<'_> { pub fn sweep(&self, at: Moment) -> Result<Report, Error>; }
```

Rules to enforce and test: a referenced hash is never collected, including when a reference appears
between the mark and the sweep (re-check per candidate, or only sweep blobs older than the mark);
an unreferenced blob past `min_age` is collected; `min_age` protects a blob whose reference is still
being published; a read without authority is refused; `max_bytes` evicts oldest-first among
unreferenced blobs only. Include a test that a torn temp file left by an interrupted `put` is never
returned by `read` and never counted.

### `mokmok-egress` (new; std, tokio)

Proxy decisions, destination enforcement, DNS/IP validation, network audit. Modules: `proxy`,
`connect`, `dns`, `destination`, `policy_client`, `connection`, `rate_limit`, `audit`.

```rust
pub struct Destination { pub scheme: Scheme, pub host: Host, pub port: Port }
pub fn parse_connect(line: &str) -> Result<Destination, ParseError>;   // "CONNECT host:port HTTP/1.1"
pub fn parse_authority(authority: &str) -> Result<Destination, ParseError>;
pub enum AddressClass { Public, Loopback, Private, LinkLocal, UniqueLocal, Multicast, Unspecified, Documentation, Broadcast }
pub fn classify(address: IpAddr) -> AddressClass;
pub struct Rules { /* which classes are forbidden, and which are permitted */ }
pub struct Validated { destination: Destination, addresses: Vec<SocketAddr> }
impl Validated {
    // Rejects when every resolved address falls in a forbidden class or policy refuses.
    pub fn new(destination: Destination, addresses: Vec<SocketAddr>, rules: &Rules, policy: &impl PolicyClient, context: &CallContext) -> Result<Self, Refusal>;
    pub fn destination(&self) -> &Destination;
    pub fn addresses(&self) -> &[SocketAddr];
    // Connects to one of the addresses validated above. It has no resolver, so it cannot re-resolve:
    // a name that answers differently on a second lookup cannot redirect the connection.
    pub async fn connect(&self) -> Result<TcpStream, Error>;
}
pub trait PolicyClient { fn check(&self, destination: &Destination, context: &CallContext) -> Result<(), Refusal>; }
pub trait Resolver { fn resolve(&self, host: &str, port: u16) -> Result<Vec<SocketAddr>, io::Error>; }
pub struct RateLimit { ... }   // token bucket per key, refilled from a passed-in `Moment`
pub struct AuditRecord { pub at: Moment, pub execution: ExecutionID, pub principal: PrincipalID, pub destination: Destination, pub address: SocketAddr, pub decision: DecisionName }
pub trait AuditSink { fn record(&self, record: AuditRecord); }
pub struct CallContext { pub principal: PrincipalID, pub execution: ExecutionID }
```

Tests: malformed `CONNECT` lines (missing port, non-numeric port, absolute URI, whitespace, empty
host, IPv6 bracket form) each asserting `Err`; `classify` over literal addresses of every class;
a rebinding case where the resolver's first answer is public and a later answer is loopback,
asserting the connection went to the validated address (use `Validated` directly plus one real
connect against a loopback listener with loopback explicitly permitted); a forbidden class refused
with no socket opened; rate limit exhaustion then refill.

### `mokmok-observability` (new; std, tracing)

Instrumentation and export, no authoritative domain state.

```rust
pub fn init(config: &Config) -> Result<Guard, Error>;    // tracing-subscriber, redaction layer, env filter
pub struct Redaction { rules: Vec<Rule> }
impl Redaction {
    pub fn from_rules(rules: Vec<Rule>) -> Result<Self, InvalidRule>;
    pub fn apply<'a>(&self, value: &'a str) -> Cow<'a, str>;   // replaces only the secret, keeps the rest
}
pub struct AuditEntry { at: Moment, actor: PrincipalID, kind: AuditKind, outcome: Outcome, detail: Option<String> }
pub trait AuditLog { fn append(&mut self, entry: AuditEntry) -> Result<(), Error>; fn read(&self, since: Moment) -> Result<Vec<AuditEntry>, Error>; }
pub struct Journal { path: PathBuf }       // durable JSON-lines file adapter
pub struct Metrics { ... }                 // counters and gauges with a snapshot()
pub trait Exporter { fn export(&self, snapshot: &Snapshot) -> Result<(), Error>; }
```

Rules: redaction removes the protected value and keeps the correlation identifiers that make the
log useful; a redacted field never appears in the output; an exporter failure returns an error and
does not change the caller's authoritative outcome; the audit journal survives a restart and reads
back in order.

Tests: a literal secret in a literal line is removed while the trace id stays; exporter failure
reported as `Err` while the audit append still succeeded; journal round trip across a reopen.

### `mokmok-agent-runtime` (new; std, tokio; depends on protocol and state-machine)

Orchestrates model calls, tools, budgets, continuation. Consumes ports; owns no sandbox internals.

```rust
pub trait ModelProvider { fn complete(&self, request: ModelRequest) -> impl Future<Output = Result<ModelResponse, ModelError>> + Send; }
pub trait ToolExecutor { fn execute(&self, call: ToolCall) -> impl Future<Output = Result<ToolOutcome, ToolError>> + Send; }
pub trait Journal { fn record(&mut self, fact: RecordedFact) -> impl Future<Output = Result<Recorded, JournalError>> + Send; }
pub struct Budget { tokens: u64, cost_micros: u64, wall: Duration, steps: u32 }
impl Budget { pub fn spend(&mut self, cost: Spend) -> Result<(), Exhausted>; }
pub struct Orchestrator<M, T, J> { ... }
impl<M, T, J> Orchestrator<M, T, J> {
    pub fn new(model: M, tools: T, journal: J, budget: Budget) -> Self;
    pub async fn run_turn(&mut self, turn: &mut Turn, state: &Session) -> Result<TurnOutcome, Error>;
    pub async fn resume(&mut self, turn: &mut Turn, state: &Session) -> Result<TurnOutcome, Error>;
}
pub enum Reconciliation { Reissue, Settle(Resolution), Surface }
pub async fn reconcile(...) -> Result<Reconciliation, Error>;
```

Rules: the durable record of an intent is committed before the call that performs it, so a crash
between them leaves a recoverable `pending`; `resume` distinguishes pending, completed, and unknown
external outcomes and never re-dispatches an action whose outcome is unknown without an
idempotency answer; a cancelled turn acquires no new execution authority; the budget refuses the
next call before it is made, not after. `ToolOutcome` carries an explicit `Unknown` for an
operation whose effect cannot be proven.

Tests with scripted ports: stop at a turn boundary and resume with the same durable journal, no
duplicate effect; a duplicate result delivery is a no-op; a provider failure is typed and leaves
the turn resumable; budget exhaustion refuses the call; cancellation during a tool call prevents
the next dispatch; an unknown outcome surfaces rather than being recorded as success.

### `mokmok-kernel` (integrator-owned; not delegated)

Composition root: config, bootstrap, services, lifecycle, recovery, shutdown, plus
`src/bin/mokmokd.rs` and the vertical-slice integration test. The integrator writes this.

## What this round deliberately does not do

- `mokmok-approval`: the approval requirement lives in `mokmok-capability` until its lifecycle
  warrants a crate.
- Cron schedules, `mokmok-event-bus` wire-frame unification with the protocol, network namespace
  isolation for the sandbox, and the Effect-TS application.
- Full `## 9. Verification Strategy` coverage: this round lands property and integration tests,
  mutation gates where the crate's shape allows one, and one crash-injection test for the slice.

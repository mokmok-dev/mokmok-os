# Rationale

## Problem

The workspace lists fifteen members. Four of them are `#![no_std]`. Seven of the fifteen have a
recorded mutation score. `docs/spec/decisions.md:81-85` already names the second gap. The ask is to
organize the crate role division (役割分担), to move pure logic into `no_std` crates, and to firm the
base with mutation testing and property-based testing.

The load-bearing defect is not the take-up rate. It is that purity is unmeasured, and the tree
reports that it is measured.

`docs/spec/references/attesters/dependency-rules.sh` carries
`rule_pure-crates-have-no-io-dependency`, whose selector is
`no_std_crates() { grep -q '^#\[no_std\]' "$repo/$dir/src/lib.rs" ... }`. The source writes
`#![no_std]`. The pattern requires a `#`, then a literal `[`, then one of `n o _ s t d`, then `]`, so
it matches `#[no_std]` and never matches `#![no_std]`. Measured this session:
`grep -c '^#\[no_std\]' mokmok-protocol/src/lib.rs` prints `0`, `sed -n '15p' mokmok-protocol/src/lib.rs`
prints `#![no_std]`, and `dependency-rules.sh check "$PWD"` prints
`rule=pure-crates-have-no-io-dependency status=pass`. The loop body never runs. Every crate is
outside the rule, including the four that opted in. This one character is why a passing check and a
real check print the same word, and it is the reason the first deliverable is a rule that cannot
pass over an empty set.

Corrections to the grounding, each measured this session unless labelled.

- **`§3` says the purity rule fires only for a crate that is already `#![no_std]`.** It fires for no
  crate at all. See above. The consequence is larger than the section implies: there is no signal
  about purity anywhere in this tree today, so the eleven std crates are not a backlog of known
  offenders, they are the whole population.
- **`§6` gives the wrong mechanism for the topological order.** `#![no_std]` is a per-crate
  attribute that rustc enforces crate by crate. It never forces a dependency to be `no_std`, and a
  `no_std` crate may depend on a std crate and name its types. The tree gives no counterexample
  (the four pure crates depend only on `mokmok-protocol`, `serde`, and `uuid`), so the claim reads as
  confirmed when it is untested. The order is forced by a different and stricter rule: a pure crate
  may not have a std dependency at all. That rule is a design choice, it is checked by
  `rule_pure-crates-close-over-pure-crates` below, and it is what makes the order topological. The
  reason to adopt it is the one in `§5`: a pure crate whose surface names a type from a std crate
  cannot be exercised without that crate's IO.
- **`§5.3` calls `Setting`'s `std::path::PathBuf` "a wire change".** `mokmok-agent-runtime/src/task.rs:14`
  derives serde on `Setting`, and `serde`'s `PathBuf` impl is a plain string on both sides, so
  replacing the field with an owned string changes no byte of the durable log. Read from the derive
  and serde's documented impl; not verified by a round-trip test. It makes phase 7 cheaper than the
  grounding prices it.
- **`§4` is right that the kernel fails at the baseline, and there is a one-line fix it does not
  name.** `cargo mutants 27.1.0` has `--test-workspace`, whose `true` value runs all workspace tests
  for each mutant (measured from `cargo mutants --help`). A workspace-wide build produces
  `mokmok-sandbox-confine` beside `mokmokd` in `target/debug`, which is the sibling the five kernel
  integration files require: measured this session, `cargo build --workspace --bins` leaves
  `mokmokd`, `mokmok-sandbox-confine`, `mokmok-event-store-crash`, and `mokmok-tui` in `target/debug`
  together. That `cargo test --workspace` builds the same set is cargo's documented target selection,
  and the first sweep in phase 5 is what proves it.
  `mokmok-tui/tests/conversation.rs:112` carries a `PATH` fallback for the same reason and becomes
  dead once the workspace is the test unit, so it is deleted rather than kept.
- **`§5.8` says egress's tokio edge "binds the crate".** It does, and the edge is dead weight.
  `Validated::connect` has zero production callers: `mokmok-kernel/src/provider.rs:498` calls
  `Validated::new`, then uses `validated.addresses()` to pin the validated addresses into reqwest's
  resolver. The only callers of `connect` are three cases in `mokmok-egress/tests/enforcement.rs`.
  Deleting `connect` makes egress pure, so the tokio edge in `§5.8` is not a blocker, it is a
  deletion.
- Everything else in `§4`, `§5`, `§7`, `§8` that I spot-checked held: the kernel baseline failure and
  its five helpers, the `byte_bound_start` shadow at `store.rs:1144`, the six passing rules, the
  fifteen members against the bundle's thirteen and fourteen, and the absent `crates/patch.md`.

**Why the crate boundary is the only purity boundary.** Rust has no per-module `no_std`. Purity is
therefore not a property a module can have, and a pure function inside a std crate cannot be proven
pure, only tested. That fact fixes the shape of this design. The answer cannot be a module map; it
has to be a crate list, and every seam that is to be proven has to live in a crate whose whole
dependency graph is pure.

**What a std crate costs, in the units the operator cares about.** A test that wants a decision
behind a std crate first has to satisfy that crate's IO. The five kernel integration files need the
confiner binary, so `cargo mutants -p mokmok-kernel` never reaches a mutant and the crate holding the
largest body of decisions in the workspace has the lowest score and no reachable score at all. The
recovery fold needs a scratch SQLite file to observe what is a fold over a value. The pull credit
rule needs a tokio runtime, a real file, and a churning publisher to observe arithmetic over four
integers, and the defect it shipped (a ceiling anchored at the eviction floor rather than at the
stored checkpoint, so a subscriber that never acks keeps drawing events) is a fact about those four
integers.

## Usage (caller's view)

Each section below is what a caller imports and what it no longer does. The caller is a colleague
writing a test, a plugin author, or the next engineer who opens the crate.

**A test author proving retention.** Today: open a `Store` on a temp file, append N payloads of
chosen sizes, then read back and infer which events survived. After:
`mokmok_delivery::evict(first, next, bytes, &held_sizes, retention)` returns the new floor and byte
total, and the property test is a `proptest!` block over five values. The test the crate exists for
is "a retention that keeps two events but whose byte bound admits one keeps the newest" and its
sibling over the subscriber's credit. Neither is writable today without a store.

**A test author proving flow control.** After
`plan_pull(delivered, acked, window, limits, max) -> Handover`, the shipped defect becomes a
property: for every `(delivered, acked, window)`, the count handed over never exceeds
`acked + in_flight - max(delivered, window.first)`, and a checkpoint below the floor yields
`Handover::Lapse` rather than a batch. The event bus keeps the loop, the socket, and the SQLite
transaction, and executes what the plan says.

**A test author proving recovery.** Today `recover(&mut Db, &Log, session, at)` needs a database.
After `mokmok_log::recovery_entries(&log)` takes a `Log` that a test builds from two or three
constructors, and the property is "for every generated log, every execution left `InFlight` yields
exactly one `ExecutionSettled { resolution: Unknown }`, and one `ToolResolved` when the log has a
turn for it; a log with no in-flight execution yields no entries". The kernel keeps the append, the
expectation, and the error mapping.

**The operator reading a session.** Today `mokmokd status <session>` opens SQLite, folds every
commit, and formats a line per record inside the fold. After, `mokmok_log::Log` is the fold and
`line_of(record, &numbers)` is the projection, so "every record renders exactly one line, except the
two machine records that render none" is a property over the record enum rather than a fact
observed through a socket.

**A provider author.** Today `mokmok_egress::Validated::new` returns a value that also knows how to
open a `TcpStream`, and the crate pays for a tokio dependency to keep that method. After, `Validated`
is a decision plus the address list it admitted, and whoever opens the socket owns the socket. The
kernel already does, through reqwest and `resolve_to_addrs`, and the property that matters to it
becomes directly testable: every address in the returned list is one whose class the rules admit, and
the list never grows.

**Who reads the purity declaration.** A reviewer opening a stranger's crate. One table in a
manifest, `[package.metadata.mokmok] purity = "alloc"`, read by `cargo metadata`, by the attester,
and by the reader. Before this, purity was an attribute inside `lib.rs` that nothing counted.

**What no caller does after this.** No caller reads rows to build a `Log`. No caller re-derives the
turn number from a transcript length. No caller holds a socket type it did not open. No caller
imports a std crate through a pure crate's surface.

## Shape

**The ladder.** Three levels, declared once per member in
`[package.metadata.mokmok] purity`, in `Cargo.toml`, because `taplo` already formats that file and
`cargo metadata` already publishes `[package.metadata]` to any reader for free.

| level | means | checked by |
| --- | --- | --- |
| `core` | `#![no_std]`, no `extern crate alloc`, no std dependency | `#![no_std]` rejected by rustc on a std item, plus the closure rule |
| `alloc` | `#![no_std]` and `extern crate alloc`, no std dependency | as above, plus the presence of the `alloc` edge |
| `std` | needs the standard library or an IO crate | `rule_a-std-crate-names-an-adapter` |

The ladder is where allocation stops being a footnote. `mokmok-os-architecture.md:80-82` asks for
explicit allocation requirements; a level nobody occupies is a level the design does not need, so
`core` is a level a new pure crate starts at and leaves only when it needs a `Vec`.

**The rule, and the file CI runs.** Four new rules, appended to
`docs/spec/references/attesters/dependency-rules.sh`, plus a non-vacuity floor on the existing IO
rule.

1. `rule_every-member-declares-its-purity`. Every workspace member names a level. This is the census.
2. `rule_declared-purity-matches-the-tree`. A `core` or `alloc` member's `src/lib.rs` carries
   `#![no_std]` (grep the exact `#![no_std]` line, not a pattern one character from it), and carries
   `extern crate alloc` iff its level is `alloc`.
3. `rule_pure-crates-close-over-pure-crates`. Every direct non-dev dependency of a pure member is
   either a pure member or an entry on a frozen external allowlist that names the exact
   `default-features` and `features` string proven no_std. Four entries: `serde` with `alloc`,
   `serde_json` with `alloc`, `uuid` with `default-features = false`, and `thiserror` with
   `default-features = false`. The first three are proven by a crate that already builds,
   `mokmok-protocol`, which is `#![no_std]` with exactly those. `thiserror` is the one entry not
   proven in this tree; the grounding reads its manifest as `#![no_std]` with `default = ["std"]`,
   and the phase that promotes `mokmok-patch` is where that gets tested.
4. `rule_pure-crates-have-no-io-dependency` keeps its IO list and gains a non-vacuity floor. It must
   report at least one crate.
5. `rule_a-std-crate-names-an-adapter`. A member declaring `std` must name one of the workspace's
   adapter crates or use `std::` in its own sources. `ADAPTER_CRATES` is the existing `IO_CRATES`
   list plus the five adapters the eleven std members already name, so the rule is green on the day
   it lands: `tracing`, `tracing-subscriber`, `tantivy`, `clap`, `ratatui`. This is the drift rule: it
   is the failure mode the operator actually has, a crate sitting at `std` that could be pure, and no
   compiler can catch it.

Rules 1 and 5 are the two halves of measurement. Rule 1 makes every crate answer; rule 5 makes a
`std` answer carry a reason that rustc can be shown to disagree with. Rules 3 and 4 are what stop the
answer being decorative.

The body half of the purity proof is free and already running. `#![no_std]` makes rustc reject
`std::` in the crate's own code, `cargoClippy` compiles every member with `--deny warnings`, and
`[workspace.lints.rust] unsafe_code = "deny"` closes the `extern "C"` route to the OS. So "no std
dependency, and `#![no_std]`" is "no IO", with no new build step.

**Where it runs.** Two new checks in `flake.nix`, for seven total.

`checks.dependencyRules` is a `pkgs.runCommand` over `${./.}` that runs
`docs/spec/references/attesters/dependency-rules.sh check`. `checks.specConformance` runs
`line-references.sh` and `okf-conformance.sh`. Both are cheap, hermetic, and read the tree Nix
already copies. Nix cannot see an untracked file, so a new member whose `Cargo.toml` is not staged
fails rule 1 rather than passing unseen. That turns the platform's sharpest edge into a guard.

The mutation gate does not become a check. A sweep of seventeen packages is hours, and `nix flake
check` is the merge gate. What becomes a check is the record, in phase 5.

**The member list.** Seventeen members. Two are new.

| member | level | owns |
| --- | --- | --- |
| `mokmok-protocol` | alloc | message contracts, typed identifiers, the log's addressing values |
| `mokmok-state-machine` | alloc | Agent, Session, Turn, Execution transitions |
| `mokmok-scheduler` | alloc | durable wakeups, deadlines, leases, fencing |
| `mokmok-capability` | alloc | principals, policy, grants, and path coverage |
| `mokmok-patch` | alloc | reading a unified diff and applying it |
| `mokmok-egress` | alloc | destination classification, the rules, the policy decision, the rate limit |
| **`mokmok-delivery`** | alloc | retention bounds and what one pull may hand over |
| **`mokmok-log`** | alloc | the session log's record vocabulary, its folds, its batches |
| `mokmok-agent-runtime` | alloc | the loop's decisions, context assembly, the ports |
| `mokmok-event-store` | std | durable transactions over SQLite |
| `mokmok-event-bus` | std | the socket, the topic state, the store loop |
| `mokmok-artifact` | std | blobs on a filesystem, their references, collection |
| `mokmok-observability` | std | the tracing adapter, the audit log, redaction |
| `mokmok-search` | std | the tantivy corpus and the query that reads it |
| `mokmok-sandbox` | std | process supervision, landlock confinement, cleanup |
| `mokmok-kernel` | std | composition, lifecycle, readiness, the SQLite codec |
| `mokmok-tui` | std | the terminal front end |

**`mokmok-delivery`, justified against `mokmok-os-architecture.md:32`.** Owned state: a topic's
retention and a subscriber's delivered/acked window. Invariants: the newest event is never evicted;
the credit ceiling is measured from the stored checkpoint, never from the eviction floor; a lapse
spends no credit. Contracts: `evict`, `plan_pull`, and the five values they read. Validation:
properties over generated sequences. `:179` does not bite, because retention and flow control are one
body of knowledge, not two state machines that share a shape: the bound the eviction enforces is the
bound the credit is measured against, which is exactly how the shipped defect appeared. The seam
already exists twice in one file, once as production code interleaved with `DELETE` and once as a
test-only shadow, and the duplication is the symptom the split removes.

**`mokmok-log`, justified against `:32`.** Owned state: the log's record vocabulary and the folds
that read it. Invariants: an execution left in flight has no settlement; an unknown record kind
refuses the whole read; a retried command replays the turn it already owns rather than minting a
second. Contracts: the entry vocabulary, the replay folds, the intent and settlement batches, the
recovery entries, and one line per record. Validation: properties over generated logs, which are
lists of values. This is the largest single move in the design and it is the one that pays for the
phase list, because it takes the kernel's pure majority out of a 7199-line composition root and puts
it in a crate the mutation gate can score.

**No new crate for the other pure seams.** The seams in `§5.4` (search), `§5.8` (artifact,
observability) and `§5.7` (the TUI reducer) are named as deliberate non-goals in the sketch, each
with the seam that would change the answer. The two tests applied: does the body of logic have owned
state, and does it have a home in an existing pure crate. `mokmok-sandbox`'s path coverage fails the
first and passes the second, so it moves into `mokmok-capability` as a predicate over components and
a supplied `is_dir` fact. That deletes a duplicated rule instead of adding a crate, which is what the
Laziness Protocol asks for.

**Derived from the caller, not the file layout.** `mokmok-log` comes from the operator's question
"what does this session's history say", not from the kernel's `db.rs` boundary. `mokmok-delivery`
comes from "may this subscriber be handed this event", not from `server.rs`. The status line joins
the folds because both are answers to the same question; the daemon's operator log line stays in the
kernel because it renders a `Move`, which is the runtime's vocabulary and the kernel's control flow,
not the log's.

## Synthesis decision

filled in by arena

## Tradeoffs accepted

**A declaration can over-claim.** A crate can declare `alloc` and use nothing from `alloc`. Nothing
fails. The check catches the direction that breaks the tree (under-declaration fails to compile or
fails rule 2) and the direction the operator cares about (rule 5 makes `std` name an adapter). It does
not catch a lazy label. Accepted because rustc has no mode that proves "this crate does not need
alloc", and manufacturing one costs a second build per level per crate. The `core` level exists so
that the bottom of the ladder is expressible, not so that it is populated today.

**The external allowlist is a promise, held by four entries.** Rule 3 is a declaration check, not a
compile. Three of the four are proven by `mokmok-protocol` already building as `#![no_std]` with
exactly those feature sets, which is why the list lives next to that crate's manifest line and not in
a wiki. The rejected alternative below says why a compile-time proof for the graph was not adopted.

**Two new crates, and the diffusion of ownership they cause.** `mokmok-log` sits above
`mokmok-agent-runtime` because it names `Record`, so the runtime has to become pure first, and the
kernel then imports a crate that imports its own lower layer. The dependency graph gains two edges
and the phase order gains two prerequisites. Offset: the kernel loses roughly the same lines it
gains in imports, and its remaining survivors become the IO-reachable ones a reader can name.

**`mokmok-log` carries the log and the status line.** A rendering function in a log crate is a small
boundary violation. It is taken because the alternative is a third crate holding one match arm per
record, and because the projection is the fold's own answer: `status` is what the log says.

**The mutation sweep is not in CI.** Seventeen packages at 95 percent is hours of wall clock. The
record is checked and the run is not. Consequence: a stale score is possible between sweeps, and the
receipt's date is what a reader has. Accepted as the honest cost of the only gate the repository
runs.

**`test_workspace = true` makes a score a workspace property.** A mutant in the bus can be caught by
the kernel's tests, so per-package scores stop being independent, and every mutant run gets slower.
Accepted because the alternative is a kernel whose baseline cannot pass.

## Alternatives considered

**One crate per pure seam, about twenty members.** The naive reading of 純粋なロジックはno_std
crateに切り出す. It lost on interface depth: each new member would own one function and no state,
`evict` and `plan_pull` would sit in two crates that must agree about one bound, and rule 3 would make
a twenty-long closure obligation out of a set that should have two members. `:32` asks for owned state
and invariants; a function is neither.

**A `mokmok-pure` facade crate that re-exports the pure set, with CI probing only the facade.** It
lost because it does not work, and the failure is the same one the tree already has. `#![no_std]` does
not propagate to dependencies, so a `#![no_std]` facade that depends on eleven crates proves only that
the facade itself names no std item. It would have been a second check that passes over an empty set.

**Annotate the std crates instead of moving code.** Tag each module with an intent and lint for
`std::` outside the tagged ones. It lost to a fact about the language, not to effort: there is no
per-module `no_std`, so the annotation cannot be enforced, and a lint on `std::` paths misses every
re-exported std type.

**Put every pure seam in `mokmok-protocol`.** One pure crate, no new members, and it is already a
dependency of everyone. It lost on the same grounds as the facade: protocol's charter is contracts,
one of the dependency rules forbids it a domain dependency, and retention arithmetic or diff parsing there
would turn the contract crate into the workspace's utility drawer. `Minimize Reader Load` is the
decision this changed: a reader who sees `mokmok_protocol::evict` cannot answer "where does this come
from" in under thirty seconds.

**A cross-target probe as the purity rule, instead of the declaration rules.**
`cargo check -p <crate> --target thumbv7em-none-eabihf` proves a crate and its whole graph std-free in
one command, with no source analysis and no allowlist. It lost on a measured hazard: `mokmok-protocol`
takes `uuid` with the `v7` feature, which pulls `getrandom`, which has no bare-metal backend for that
target, so the probe would fail on a crate that is pure today. A rule that is red on the first
honest crate is worse than a declaration rule that is green and complete modulo four reviewed
entries. The probe survives as a hand-run mode for validating a new allowlist entry.

**Make the kernel pure and push the IO out to the binaries.** One purity boundary, no new crates,
the composition root becomes provable. It lost because the kernel is the composition root by charter,
and because the pure and IO halves would still be two crates once the sockets moved: the same split,
with the names reversed and every doc citation churned twice.

## Open questions and risks

**Two folds of one log.** `mokmok-agent-runtime::Transcript` folds `Record`s and
`mokmok-kernel::db::Log` folds `Entry`s, where `Entry::Agent` holds a `Record`. They are the same
question answered twice at two layers. `mokmok-log` sits above the runtime so that it can name
`Record`, which leaves both folds alive. If a defect lands in the duplication, the fix is to move the
record vocabulary down into `mokmok-log` and have the runtime depend on it, which is a larger diff
across the runtime's twelve re-exports and the TUI. Deferred until then, and named here so the deferral
is visible.

**The name `mokmok-log` collides.** The event store is also a log, and two traits already carry the
name `Journal`. A reader arriving at `Log` in `mokmok-log` and `Log` in `mokmok-kernel` has to hold two
meanings. If the reading test fails during phase 8, rename the crate, not the kernel type.

**Whether `mokmok-scheduler` should exist.** It has no caller, and
`docs/spec/decisions.md:77-80` says so. This design deletes the kernel's manifest edge and leaves the
crate standing on its own tests. Deleting the member is the operator's call, and one word reverses
the edge deletion.

**The kernel's remaining survivors.** After phase 8 they are all IO-reachable, which is the right
outcome and also an excuse: a survivor that is in fact a pure decision is a candidate for a later
move, and the only place it will be visible is `mutants.out/missed.txt`. Phase 5's record is what makes
that list readable.

**OKF citations are positional.** Every phase that edits a cited crate carries a bundle repair.
`docs/spec/verification/line-references.sh` counts 618 pairs today; the check makes a break visible in
the phase that causes it, not free. This is the largest recurring tax in the phase list and it is not
reducible by any choice this design makes.

**Rule 5 is a heuristic with an escape hatch.** "Uses `std::` in its own sources" is met by any crate
that writes `std::fmt` once. It is the fail-open arm. The fail-closed arm, `std` must name an adapter,
is what does the work, and the list of adapters needs review whenever a dependency is added.

**`test_workspace = true` has not been run end to end.** The flag exists and its meaning is read from
`cargo mutants --help`; a workspace test build producing the confiner beside `mokmokd` is the
mechanism `mokmok-kernel/tests/agent.rs:99` already documents in its own failure message, and
`cargo build --workspace --bins` was measured this session to leave the confiner beside `mokmokd`. The
first sweep in phase 5 is the proof, and phase 5 is the phase that can fail.

## Next implementation step

One pull request, and it changes no Rust. It is the census.

1. `[package.metadata.mokmok] purity` in all fifteen manifests, at the levels measured today, four
   `alloc` and eleven `std`. The declaration is the durable guardrail, so it exists before anything
   moves.
2. The five rules in `docs/spec/references/attesters/dependency-rules.sh`, including the non-vacuity
   floor on the IO rule.3. `checks.dependencyRules` and `checks.specConformance` in `flake.nix`, and the two attester files
   `git add`ed so Nix can see them.
4. `docs/spec/verification/dependency-rules.md` gains the purity rules and the census, so the bundle
   describes what the check does.

Its end state is checkable in one line. `nix flake check` is green with seven checks, and
`./docs/spec/references/attesters/dependency-rules.sh check "$PWD"` prints
`rule=pure-crates-have-no-io-dependency status=pass detail=` with four crates examined rather than
zero. That line, not the phase count, is the deliverable: purity starts being measured, and the
fifteen declarations make every later promotion a one-line diff that a reviewer can see.
# Sketch

The concrete shape: the target members, the purity level each declares, the types and signatures the
two new crates add, the module map, then the phase list with the check each phase ends in.

## 1. The target members

Seventeen. Fifteen today, plus `mokmok-delivery` and `mokmok-log`. The level is the string that goes in
`[package.metadata.mokmok] purity`.

| member | level | the one line a reviewer reads |
| --- | --- | --- |
| `mokmok-protocol` | `alloc` | contracts, identifiers, and the log's addressing values; owns no transport, storage, or clock |
| `mokmok-state-machine` | `alloc` | the transitions of Agent, Session, Turn, and Execution |
| `mokmok-scheduler` | `alloc` | durable wakeups, deadlines, leases, fencing |
| `mokmok-capability` | `alloc` | principals, policy, grants, revocation, and the path-coverage rule the sandbox enforces |
| `mokmok-patch` | `alloc` | reading a unified diff and applying it, with no repository behind it |
| `mokmok-egress` | `alloc` | which destinations are permitted, and the limit on how often |
| `mokmok-delivery` | `alloc` | what retention keeps and what one pull may hand over |
| `mokmok-log` | `alloc` | what a session's durable log says, and one line per record |
| `mokmok-agent-runtime` | `alloc` | the loop's decisions, context assembly, the four ports |
| `mokmok-event-store` | `std` | durable append and consume transactions over SQLite |
| `mokmok-event-bus` | `std` | the socket, the connection state, the store loop, the ack path |
| `mokmok-artifact` | `std` | blobs on a filesystem, their references, and collection |
| `mokmok-observability` | `std` | the tracing adapter, the durable audit log, redaction |
| `mokmok-search` | `std` | the tantivy corpus and the query that reads it |
| `mokmok-sandbox` | `std` | process supervision, landlock confinement, limits, cleanup |
| `mokmok-kernel` | `std` | composition, lifecycle, readiness, the SQLite codec, the supervisor socket |
| `mokmok-tui` | `std` | the terminal front end, an application above the kernel |

The four pure members today (`protocol`, `state-machine`, `scheduler`, `capability`) keep their
attribute and gain a declaration. `mokmok-patch` and `mokmok-egress` are promoted during the
migration. `core` is unoccupied and stays available.

## 2. What moves, and the test each move makes possible

| seam | from | to | the test that is not writable today |
| --- | --- | --- | --- |
| eviction policy, `store.rs:470-487` | `event-bus` | `mokmok-delivery::evict` | property over `(first, next, bytes, held sizes, retention)`: the newest event survives every bound, and the count bound is exactly `retention.events` |
| the test-only shadow `byte_bound_start`, `store.rs:1144` | `event-bus` (deleted) | the function above | the production policy becomes the function under test, so the shadow has nothing left to shadow |
| credit and lapse arithmetic, `server.rs:566-621` | `event-bus` | `mokmok-delivery::plan_pull` | property over `(delivered, acked, window)`: the count never exceeds `acked + in_flight - max(delivered, window.first)`, and a checkpoint below the floor lapses instead of drawing |
| `recover_unsettled`'s fold, `recovery.rs:40-69` | `kernel` | `mokmok_log::recovery_entries` | property over generated logs: one settlement per in-flight execution, and no entry at all for a settled log |
| `Log` and its folds, `db.rs:116-267` | `kernel` | `mokmok_log::Log` | property over generated entry sequences: replay then re-absorb is a fixed point, and an unknown kind refuses the whole read |
| `unsettled_sessions`, `db.rs:392` | `kernel` | `mokmok_log::unsettled` | property: the unsettled set is exactly the sessions with an execution whose last event has no settlement |
| `mint`, `launch`, `settled`, `slice.rs:206-292` | `kernel` | `mokmok_log::{mint, launch, settled}` | cross-check property: every batch `launch` builds replays through the state machine without a rejection |
| `Log::push`, `db.rs:455` | `kernel` | `mokmok_log::Entry::from_kind` | property: the kind round-trips for every entry variant, with no database |
| `transcript_line`, `slice.rs:396` | `kernel` | `mokmok_log::line_of` | property over the record enum: exactly one line per record, except the two machine records that render none |
| `Notice`, `slice.rs:64`, `agent.rs:118` | `kernel` | `mokmok_log::Notice` | property: the notice names the step's own command and carries the turn and execution the records name |
| `StreamId`, `ConsumerId`, `CommandId`, `Version`, `CommitPosition`, `model.rs` | `event-store` | `mokmok-protocol` | nothing new is provable; the pure crates that name them can exist, which is the point |
| `Validated::connect`, `egress::connect` | deleted | nobody | three tests in `egress/tests/enforcement.rs` go with it; the kernel's `resolve_to_addrs` path is what the production guarantee actually rests on |
| `now()` in `egress::audit` | `egress` | an `at: Moment` argument | property: the same inputs yield byte-identical audit records, which a wall clock forbids |
| `covers`, `sandbox/src/capability.rs:191` | `sandbox` | `mokmok_capability::covers_path` | cross-crate property: the sandbox's plan and the capability's decision agree over generated component lists, with `is_dir` passed in |
| `Index::find`'s merge, `kernel/src/index.rs:70-95` | `kernel` | `mokmok_search::{merge, Hit}` | property: `merge(hits, n)` is the score-descending, path-ascending prefix of its input, truncated to `n` |
| `Cadence::due` and `ping`, `notify.rs:130-165` | `kernel` | stay in place, take `Duration` and `Instant` as arguments | property: a deadline moves from the report, never from the schedule, so a late pass buys one interval |
| `outcome_of` / `detail_of`, `provider.rs:419-428` | `kernel` | stay in place, `pub(crate)` | property: every egress refusal maps to exactly one `Outcome`, and the mapping is total |

Stays where it is, deliberately: the TUI reducer and row renderer (`tui/src/core.rs`, `tui/src/port.rs`),
whose three survivors need a pty; `mokmok-search`'s token, snippet, clip, and render, which are pure
`&str` functions already callable from a test, so a crate would add a boundary without adding a
capability; `mokmok-artifact`'s hash and collector, where the crate's identity is the filesystem it
owns; `mokmok-observability`'s redaction, metrics, and audit records, all pure in place behind ports
with fakes.

## 3. `mokmok-delivery`

```rust
//! What retention keeps and what one pull may hand over.

//! The two bounds are one decision, not two: the credit a subscriber draws is measured against the
//! checkpoint it stored, and eviction moves the floor the events sit above. Reading the ceiling from
//! the floor lets a subscriber that never acks keep drawing events forever.

use core::num::{NonZeroU64, NonZeroUsize};

pub struct Seq(u64);
pub struct LogEpoch(uuid::Uuid);
pub struct Cursor {
    pub epoch: LogEpoch,
    pub seq: Seq,
}
pub struct Window {
    pub epoch: LogEpoch,
    pub first: Seq,
    pub next: Seq,
}
pub struct Lapsed {
    pub held: Cursor,
    pub from: Seq,
}
pub struct Retention {
    pub events: NonZeroU64,
    pub bytes: NonZeroU64,
}
pub struct Limits {
    pub batch: NonZeroUsize,
    pub in_flight: NonZeroU64,
}
pub struct Evicted {
    pub first: Seq,
    pub bytes: u64,
    pub dropped: u64,
}
pub enum Handover {
    Batch {
        from: Seq,
        count: NonZeroUsize,
        lapsed: Option<Lapsed>,
    },
    Lapse(Lapsed),
    Park,
}

/// The floor and byte total that keep a topic inside `retention`, given the sizes the topic holds
/// from `first` onward.
pub fn evict(
    first: Seq,
    next: Seq,
    bytes: u64,
    held: &[u64],
    retention: Retention,
) -> Evicted {
    unimplemented!("not implemented")
}

/// What one pull hands a subscriber that has been delivered `delivered` and has acked `acked`.
pub fn plan_pull(
    delivered: Seq,
    acked: Seq,
    window: Window,
    limits: Limits,
    max: NonZeroUsize,
) -> Handover {
    unimplemented!("not implemented")
}
```

Module map: `src/lib.rs` (the re-exports and the two functions' contracts), `src/seq.rs` (`Seq`,
`LogEpoch`, `Cursor`, `Window`, `Lapsed`), `src/bound.rs` (`Retention`, `Limits`, `Evicted`, `evict`),
`src/plan.rs` (`Handover`, `plan_pull`). `mokmok-event-bus` keeps `topic.rs`, `wire.rs`, `server.rs`,
`store.rs`, `bus.rs`, `client.rs`, `config.rs` (the socket and SQLite halves of `Limits` and
`Retention` re-exported from here), and `event.rs` shrinks to the store's row shape.

## 4. `mokmok-log`

```rust
//! What a session's durable log says, and one line per record.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use mokmok_agent_runtime::{Record, Turn, TurnNumber};
use mokmok_protocol::{CommandId, CommitPosition, ExecutionID, Moment, SessionID, TurnID};
use mokmok_state_machine::{Fact, Resolution, TurnFact, TurnOutcome};

pub enum Entry {
    Session(Fact),
    Turn(TurnID, TurnFact),
    Result(ExecutionID, ExecutionResult),
    Agent(Record),
}
pub struct TurnRecord {
    pub number: Option<TurnNumber>,
    pub outcome: Option<TurnOutcome>,
}
pub struct ResultRecord {
    pub result: ExecutionResult,
}
pub struct Log {
    pub session: Vec<Entry>,
    pub turns: BTreeMap<TurnID, TurnRecord>,
    pub results: BTreeMap<ExecutionID, ResultRecord>,
}
pub struct Notice {
    pub session: SessionID,
    pub command: CommandId,
    pub turn: Option<TurnID>,
    pub execution: Option<ExecutionID>,
    pub resolution: Option<Resolution>,
    pub at: Moment,
}

impl Entry {
    /// The event kind this entry is logged under, and the inverse of `from_kind`.
    pub const fn kind(&self) -> &'static str {
        unimplemented!("not implemented")
    }

    pub fn from_kind(kind: &str, payload: &[u8]) -> Result<Self, Error> {
        unimplemented!("not implemented")
    }
}

impl Log {
    pub fn state(&self, session: SessionID) -> Result<State, Error> {
        unimplemented!("not implemented")
    }

    pub fn turn(
        &self,
        turn: TurnID,
    ) -> Result<&TurnRecord, Error> {
        unimplemented!("not implemented")
    }

    pub fn turns_in_order(&self) -> Vec<TurnID> {
        unimplemented!("not implemented")
    }

    pub fn turn_of(
        &self,
        execution: ExecutionID,
    ) -> Option<TurnID> {
        unimplemented!("not implemented")
    }

    /// Replays `entries` against this log without absorbing them, so a batch is refused before it is
    /// written.
    pub fn verify(
        &self,
        session: SessionID,
        entries: &[Entry],
    ) -> Result<(), Error> {
        unimplemented!("not implemented")
    }

    pub fn absorb(
        &mut self,
        entries: &[Entry],
    ) -> Result<CommitPosition, Error> {
        unimplemented!("not implemented")
    }
}

/// The sessions holding an execution whose last event has no settlement.
pub fn unsettled(entries: &[Entry]) -> Vec<SessionID> {
    unimplemented!("not implemented")
}

/// The identities one attempt runs under, and the turn number it opens.
pub fn mint(
    command: CommandId,
    at: Moment,
    turns: usize,
) -> (TurnID, ExecutionID, TurnNumber) {
    unimplemented!("not implemented")
}

/// The transaction that records the intent, before anything runs. The first entry opens the session
/// when `opened` is false.
pub fn launch(
    opened: bool,
    turn: TurnID,
    execution: ExecutionID,
    number: TurnNumber,
) -> Vec<Entry> {
    unimplemented!("not implemented")
}

/// The transaction that records the outcome, once it is known.
pub fn settled(
    turn: TurnID,
    execution: ExecutionID,
    resolution: Resolution,
    result: Option<&ExecutionResult>,
) -> Vec<Entry> {
    unimplemented!("not implemented")
}

/// The unknown outcome for every execution a crash left in flight, and the executions it names.
pub fn recovery_entries(log: &Log) -> (Vec<ExecutionID>, Vec<Entry>) {
    unimplemented!("not implemented")
}

/// One record as the single line `status` prints for it, or none for the two machine records.
pub fn line_of(
    record: &Record,
    numbers: &BTreeMap<TurnID, Option<TurnNumber>>,
) -> Option<String> {
    unimplemented!("not implemented")
}

impl Notice {
    pub fn outbox(&self) -> Result<OutboxMessage, Error> {
        unimplemented!("not implemented")
    }
}
```

Module map: `src/lib.rs`, `src/entry.rs` (`Entry`, the kind table, the codec), `src/fold.rs` (`Log`,
`TurnRecord`, `ResultRecord`, `state`, `turn`, `turns_in_order`, `turn_of`, `verify`, `absorb`),
`src/batch.rs` (`mint`, `launch`, `settled`), `src/recovery.rs` (`recovery_entries`, `unsettled`),
`src/notice.rs` (`Notice`), `src/line.rs` (`line_of`), `src/error.rs`. The outbox payload shape
(`OutboxMessage`) is the event store's, so `Notice::outbox` moves to the kernel if the closure rule
turns out to forbid the edge; the alternative is a `Payload` newtype here. That is the one signature
in this file I am not certain about, and phase 8 settles it.

## 5. Edits to existing crates

**`mokmok-protocol`** gains `StreamId`, `ConsumerId`, `CommandId`, `Version`, `CommitPosition`, and
`InvalidId`, moved out of `mokmok-event-store/src/model.rs`. Measured: that file's whole vocabulary is
a `u64` or a `Uuid` in a newtype, over `core::num`, `std::fmt`, `std::str`, and `uuid`, and the two
`std` items are the same items in `core`, so the move changes no behaviour. `mokmok-event-store` keeps
`Expected` and re-exports the rest, so its own callers do not move in the same phase. The phase also
adds `Path`, the durable form of a path, with a parse at the boundary and the same serde form
`PathBuf` already writes. It is here rather than in a crate of its own because a newtype with no rule
of its own is not a role.

```rust
/// A path as the durable log holds it, and as a model is shown it.
///
/// The serde form is the string `PathBuf` writes, so a record written before this type existed
/// still reads. No component is resolved here: a name that a symlink could redirect is the
/// sandbox's question, and it is asked where the filesystem is.
pub struct Path(String);

impl Path {
    pub fn new(value: impl Into<String>) -> Self {
        unimplemented!("not implemented")
    }

    pub fn as_str(&self) -> &str {
        unimplemented!("not implemented")
    }
}
```

**`mokmok-capability`** gains `covers_path`.

```rust
/// Whether a granted path covers a candidate, compared by components and never as a string.
///
/// `granted_is_dir` is the fact the filesystem would have had to answer, supplied by the caller so
/// this decision stays pure and the fail-closed direction stays the caller's to choose.
pub fn covers_path(
    granted: &[&str],
    candidate: &[&str],
    granted_is_dir: bool,
) -> bool {
    unimplemented!("not implemented")
}
```

`mokmok-sandbox/src/capability.rs` keeps `Capability`, `Exposure`, `uncovered_index`, and the
`canonicalize` resolution, and calls this predicate with `capability.path.is_dir()`.

**`mokmok-egress`** deletes `src/connect.rs`, `Validated::connect`, and `connect::Error`, and moves
`Validated` into `src/destination.rs` with the address list and no socket. `audit.rs` takes
`at: Moment` instead of reading the clock. `rate_limit.rs` uses `alloc::collections::BTreeMap`. The
tokio dependency edge leaves the manifest.

**`mokmok-agent-runtime`** keeps its modules and its surface, and changes only what purity costs:
`core::error::Error`, `alloc::collections::BTreeMap`, `serde`/`serde_json`/`uuid` at their no_std
feature sets, `thiserror` with `default-features = false`, and the one `tracing::debug!` in
`advance.rs:132` gated behind a feature. `Setting`'s four `PathBuf` fields become `protocol::Path`.

**`mokmok-kernel`** keeps `bin/mokmokd.rs`, `config.rs`, `authority.rs`, `execution.rs`, `serve.rs`,
`notify.rs`, `agent.rs`, `index.rs`, `error.rs`, and shrinks `db.rs` to rows and `Entry`, `recovery.rs`
to the append and its error mapping, and `slice.rs` to sequencing. Its two line renderers leave;
`step_line` and `ask_line` stay, because they render a `Move`.

**`mokmok-search`** gains `merge` and `Hit`, and `corpus::Fields` becomes a local struct of three
`FieldID(u32)` values converted from `tantivy::schema::Field` at the one place the schema is built,
so `query::boolean` no longer names a tantivy type. Its `Hit.path` becomes `protocol::Path`. It stays
`std`.

**`mokmok-event-bus`** deletes `event.rs`'s five value types and the eviction loop's arithmetic, keeps
`Limits`/`Retention` as re-exports, and keeps one query that reads the sizes a topic holds.

**`mokmok-event-store`** loses `model.rs`'s pure value types and keeps `Append`, `NewEvent`, `Expected`,
`Outcome`, `Committed`, `Store`, `WriteTx`, and the row codecs. Its `rule_event-store-has-no-transport-dependency`
stays green.

**Deleted before any of this:** `mokmok-egress/src/connect.rs` and its three tests, the `PATH`
fallback in `mokmok-tui/tests/conversation.rs:112`, the `mokmok-scheduler` edge in
`mokmok-kernel/Cargo.toml`, the `byte_bound_start` shadow, and the `mokmok-data/` working-tree
directory (one `.gitignore` line, since the daemon defaults its `--data-dir` there).

## 6. Phase list

Each phase is one pull request that lands on its own, and each ends in a check that can be named and
run.

1. **The census.** `[package.metadata.mokmok] purity` in the fifteen manifests at today's levels; the
   five rules in `dependency-rules.sh`; `checks.dependencyRules` and `checks.specConformance` in
   `flake.nix`. No Rust changes. **Check:** `nix flake check` green with seven checks, and
   `dependency-rules.sh check "$PWD"` reports the IO rule over four crates instead of zero.
2. **Subtraction.** Delete the egress connector and its three tests, the TUI `PATH` fallback, the
   `mokmok-scheduler` edge, the `byte_bound_start` shadow, and the drift in `docs/spec` that phase 1's
   checks expose (thirteen and fourteen to fifteen, `crates/patch.md`, the kernel's case counts and
   its missing `index` row, the observability `error` row, the wall-clock claim, the stale receipts).
   **Check:** `specConformance` green over the repaired bundle, `cargoTest` green, zero rows in
   `unresolved` from `line-references.sh`.
3. **`mokmok-patch` to `alloc`.** One attribute, `extern crate alloc`, `thiserror` without default
   features. No prerequisite, so it is the pilot that proves the ladder end to end.
   **Check:** `dependencyRules` green with `purity = "alloc"` for patch, and
   `MUTANTS_PACKAGE=mokmok-patch ./scripts/mutants.sh` still reports 100 percent.
4. **`mokmok-egress` to `alloc`.** Phase 2 already removed the only std binding; this phase moves the
   audit clock to an argument and the `HashMap` to `alloc`. **Check:** `dependencyRules` green with
   `purity = "alloc"` for egress, and the new property tests over `parse_connect`, `classify`, and
   `RateLimit::acquire` run with no tokio in the test's graph.
5. **The gate reaches every member and records it.** `test_workspace = true` in
   `.cargo/mutants.toml`; `scripts/mutants-all.sh` reads `cargo metadata`, runs `scripts/mutants.sh`
   once per member with `MUTANTS_PACKAGE`, and writes `docs/spec/verification/mutation.md` with one
   row per member and the date; a `mutation-receipts.sh` rule requires a row per member and runs in
   `checks.dependencyRules`. **Check:** the sweep's first table has fifteen rows, the kernel's row
   exists, and `nix flake check` is green. This is the phase that can fail.
6. **`mokmok-protocol` gains the log's addressing values and the durable path.** A pure move of
   `StreamId`, `ConsumerId`, `CommandId`, `Version`, `CommitPosition`, and `InvalidId` out of
   `event-store/src/model.rs`, with event-store re-exporting, plus a `Path` value type whose serde form
   is the string `PathBuf` already writes. The path type has to land here because two crates above it
   need it pure: the runtime's `Setting` and the search crate's `Hit`. **Check:** `cargoTest` unchanged
   in count, `specConformance` green over the touched citations, `dependencyRules` unchanged.
7. **`mokmok-agent-runtime` to `alloc`.** The blocker list in `§5.3`, plus the `Setting` path fields
   taking `protocol::Path`. **Check:** `dependencyRules` green with `purity = "alloc"` for the
   runtime, and `cargo mutants -p mokmok-agent-runtime` still at or above 95 percent.
8. **`mokmok-log`.** The new crate, plus the kernel's `db.rs`, `slice.rs`, and `recovery.rs` shrinking
   to the codec and the IO. **Check:** a first-time receipt for `mokmok-log` at or above 95 percent,
   and the kernel's score rising because the moved code is no longer in it. Also adds
   `docs/spec/crates/log.md`.
9. **`mokmok-delivery`.** The new crate, and the event bus deleting its copy of the policy and its
   shadow. **Check:** a first-time receipt for `mokmok-delivery` at or above 95 percent, the shipped
   credit defect stated as a passing property, and the bus's two remaining survivors still named in
   its receipt.
10. **`mokmok-capability` gains path coverage, and the sandbox calls it.** Deletes a duplicated rule.
    **Check:** the cross-crate agreement property passes, and both crates' receipts are refreshed.
11. **The seams that stay in place.** The kernel's `Cadence` and `ping` take their instants as
    arguments; `bus_ended` stops naming `JoinError`; `outcome_of` and `detail_of` become `pub(crate)`;
    `merge` moves from `index.rs` into `mokmok-search`; the search and artifact pure seams gain inline
    property tests. **Check:** `nix flake check` green, and the kernel's receipt lists its remaining
    survivors in `missed.txt`, each one IO-reachable.

Phases 6, 7, 8, 9 and 10 are the ones that move logic, and they are ordered by the closure rule: a
pure crate's dependencies must already be pure, so `mokmok-protocol` moves first, then the runtime
that `mokmok-log` names, then the log. `mokmok-delivery` and the capability change have no prerequisite
and can be landed at any point after phase 5. Phases 1 through 5 are scaffold, subtraction, and
measurement, and they are ordered first because each of them makes the later ones cheaper: the census
makes a promotion a one-line diff, the subtraction removes the tokio edge that would have blocked
phase 4, and the sweep makes phase 8's gain a number instead of an argument.

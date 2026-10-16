# Rationale: crate roles, purity, and the mutation base

## Problem

The workspace holds fifteen members and four of them declare `#![no_std]`. The rule that checks
purity, `rule_pure-crates-have-no-io-dependency` in
`docs/spec/references/attesters/dependency-rules.sh`, selects its subjects with `no_std_crates()`,
which greps for `^#![no_std]` in the crate's own `src/lib.rs`. The rule therefore asks its question
only of crates that have already answered it, and nothing measures how many have. Run today,
`bash docs/spec/references/attesters/dependency-rules.sh check "$PWD"` prints six `status=pass`,
including that one, over four crates.

The pure logic with the most riding on it sits on the far side of an I/O surface. `mokmok-event-bus`
interleaves its eviction arithmetic with `DELETE` and `UPDATE` inside `append`
(`mokmok-event-bus/src/store.rs:432-487`), and its delivery credit arithmetic with
`store().read(...).await` inside `pull` (`mokmok-event-bus/src/server.rs:555-621`). The credit
ceiling was first anchored at the retained floor instead of the stored checkpoint and shipped. A
verifier driving a churning publisher from outside the tree falsified it, and the suite did not.
`mokmok-kernel` is 7199 source lines, scores 86.9% where it is scored at all, and cannot be scored
by `scripts/mutants.sh` at all, because its integration tests hard-assert that the confiner bin of
`mokmok-sandbox` sits beside `CARGO_BIN_EXE_mokmokd` (`mokmok-kernel/tests/agent.rs:99-107` and four
more files).

The constraints that make the shape non-obvious:

- `nix flake check` is the only CI gate. It declares `cargoClippy`, `cargoDocTest`, `cargoTest`,
  `mokmokdUnit`, and `nixosModule`, and none of the four attesters runs there.
- A crate can only become `#![no_std]` once every type its public surface mentions already is, so
  the work is a topological order rather than a fan-out.
- The repository squashes every PR, so each unit has to land alone.
- Adding a dependency edge stales `Cargo.lock` and fails the check before anything compiles.
- `docs/spec` holds 618 positional `path:line` citations, so every phase that edits a cited crate
  pays a repair.

## Usage (caller's view)

A reviewer asks what a crate's role is and reads one line of its manifest.

```
$ grep -A1 '\[package.metadata.mokmok\]' mokmok-*/Cargo.toml
mokmok-agent-runtime/Cargo.toml-  purity = "pure"
mokmok-event-bus/Cargo.toml-  purity = "adapter"
```

The gate tells them whether the line is true, and the count is the report.

```
$ nix flake check --print-build-logs
dependencyRules> pure=7 adapter=9
dependencyRules> rule_pure-crates-are-no_std status=pass
```

A kernel maintainer writes a recovery decision with no database open.

```rust
let plan = recovery_entries(&session, &log)?;
assert_eq!(plan.entries, vec![
    Entry::Session(Fact::ExecutionSettled { execution, resolution: Resolution::Unknown }),
    Entry::Turn(turn, TurnFact::ToolResolved { execution, resolution: Resolution::Unknown }),
]);
```

A bus maintainer changes the delivery policy against generated inputs.

```rust
proptest! {
    #[test]
    fn a_subscriber_that_never_acks_draws_no_more_than_its_credit(
        delivered in 0_u64..64,
        acked in 0_u64..64,
        in_flight in 1_usize..8,
    ) {
        let plan = plan_pull(PullRequest { delivered: Seq::new(delivered), acked: Seq::new(acked), .. });
        prop_assert!(plan.spends_at_most(in_flight));
    }
}
```

An operator refreshes the base, and the check holds everyone to it.

```
$ ./scripts/mutants.sh --all
mokmok-search score=100.0% target=95%: met
mokmok-kernel score=95.1% target=95%: met
wrote mutation.toml (16 members)
$ nix flake check     # mutationBaseline: every row current, every score at its target
```

## Shape

Three mechanisms, one new crate.

The first is the declaration. Every member carries `[package.metadata.mokmok] purity = "pure"` or
`"adapter"` in its own `Cargo.toml`. `pure` means the crate is `#![no_std]`, so the compiler proves
it and no convention does. `adapter` means the crate binds the world. A new crate that declares
nothing fails, which is the fix for the measured problem that purity is opt-in with nothing counting
the opt-ins. The declaration is one line per crate, per `principle-laziness-protocol`, and it
replaces a fact that currently lives in prose.

The second is the check. Three rules land in `docs/spec/references/attesters/dependency-rules.sh`,
the file that already owns the architecture's dependency rules, and one new flake check named
`dependencyRules` runs it:
`rule_every-member-declares-its-purity`, `rule_pure-crates-are-no_std`, and
`rule_pure-crates-depend-only-on-pure-crates`. The third is the topological constraint of grounding
section 6 as a rule. It passes over the four today and it makes "which crate can be pure next" a
machine fact. The check prints `pure=<n> adapter=<n>`, so a change in the role mix appears in CI's
log. This is `principle-encode-lessons-in-structure` applied to the operator's own ask, and the
mechanism is the strongest the situation allows, because a module inside a std crate cannot be
`no_std` and a grep for `std::` cannot tell a type from a string.

The third is the extraction. One new crate, `mokmok-delivery`, owns the retained window and the
delivery credit. It earns the split against `mokmok-os-architecture.md:32` on all four counts. It
owns state, the window floor and each subscriber's credit. It owns invariants, that the newest event
is never evicted, that the ceiling is measured from the stored checkpoint and never from the retained
floor, and that a lapse spends no credit. It owns a contract of two decisions and the types they are
decided over, and a validation requirement the current surface cannot meet, a property test over
generated window sizes and a property test over generated credit triples. It is one state machine, so
`mokmok-os-architecture.md:179` holds. Everything else in grounding section 5 becomes a pure function
in the crate that already owns the decision, which is the cheaper of the two moves and the one
`principle-laziness-protocol` prefers.

`mokmok-agent-runtime` becomes `#![no_std]` and `mokmok-patch` becomes `#![no_std]`, because both
already are pure and only their declarations were missing. The runtime's own doc at
`mokmok-agent-runtime/src/lib.rs:3` claims "The crate is pure: it reads no clock, no file, and no
socket". The design turns that sentence into a compiler fact. Sixteen members result, seven pure and
nine adapters.

The sequencing is the topological order, one PR per phase, each ending in a named check. P0 lands the
declaration and the check, and nothing else moves. P1 subtracts. P2 makes `mokmok-patch` pure by
hand, on the cheapest crate, to learn the recipe. P3 and P4 land `mokmok-delivery`, first the window
and the eviction decision, then the credit and the pull plan. P5 makes the gate reach all sixteen
members and records every score. P6 hardens the kernel against its own measurement. P7 makes the
runtime pure. P8 does the in-place seams one crate at a time. P9 brings the bundle to the new roles.
The full table is in the sketch.

Two mechanisms carry requirement five. `scripts/mutants.sh` gains `--all`, which builds the confiner,
exports `MOKMOK_CONFINER` (a hook the five kernel helpers already read and nothing ever set), sweeps
every member in topological order, and writes `mutation.toml`. It also gains `--check-baseline`,
which recomputes a digest per member through the same code path and fails when a row is missing, when
a digest moved, or when a score is below its target. One new check runs that mode. A member's target
only rises, and the check prints how many rows sit below 95, so the gap to the target state is a
number rather than a claim. The end state is sixteen rows, every target 95.

Invariants live in types where they can. `Seq` and `Cursor` stay distinct types inside
`mokmok-delivery`, so the shipped defect's confusion between the retained floor and the stored
checkpoint is a type error rather than a comment. `PullPlan` makes the three outcomes of a pull
unrepresentable in any other combination, so a batch can carry a lapse but a lapse cannot carry
events. Validation stays at the boundary, where the kernel's `Config::resolve` keeps its filesystem
checks and its directory creation together, per `principle-boundary-discipline`. The interface is no
larger than it needs to be. `retention_step` and `plan_pull` hide the retention policy, the credit
arithmetic, and the lapse rule behind two functions and ten value types, and the event bus keeps its
public surface byte-identical by re-exporting the five moved types.

## Synthesis decision

filled in by arena

## Tradeoffs accepted

- We accept `retention_step` trusting the retained byte total the store passes it rather than
  deriving the total from the sizes, in exchange for keeping one read and at most one delete per
  evicted event.
- We accept the kernel's hard confiner assert staying while the tui's shell fallback also stays,
  because the two files answer different questions. The kernel's tests dispatch a program, and a
  shell in place of a confiner would pass a test that claims confinement never ran.
- We accept one new crate and its third home for the window vocabulary in exchange for the delivery
  policy being compiler-proven pure, which is the only way a property test can be trusted over it.
- We accept a per-member digest in `mutation.toml` that forces a re-sweep of a scored crate after any
  edit to that crate, including a comment, in exchange for a recorded score that cannot silently
  describe different code.
- We accept nine adapter crates whose pure modules are pure by convention rather than by compiler, in
  exchange for not splitting search, egress, observability, and artifact over `String` and `usize`
  transforms.
- We accept one `#[cfg(test)] extern crate std;` in `mokmok-agent-runtime`, because its test support
  needs a `Mutex` and `core` has none. The shipped build is what the purity rule reads.
- We accept the runtime's `Setting` path fields changing type, in exchange for a wire form that does
  not move, because `PathBuf` already serializes as its string on Unix and errors on a non-UTF-8
  path, so no stored record can hold anything else.
- We accept the kernel's target starting at its measured 86.9 rather than at 95, in exchange for the
  first score the kernel has ever had. The target rises in the PR that earns it.
- We accept the new `specBundle` check, which turns 618 positional citations into a per-PR cost, in
  exchange for the drift grounding section 7 measured being paid when it is incurred.
- We accept a `--jobs 8` sweep of tens of minutes as an operator command rather than a CI check, in
  exchange for `nix flake check` staying usable locally.

## Alternatives considered

- **Extract a `mokmok-kernel-core` or `mokmok-recovery` crate for the kernel's pure seams.** Lost on
  interface depth. It would expose `Log`, `Entry`, `Fact`, and `Notice` as a second public vocabulary
  for a session's history that `mokmok-state-machine` already owns, so a caller would have two ways
  to name the same fold, and it gathers recovery, ranking, and transcript rendering under one roof,
  which `mokmok-os-architecture.md:179` forbids. Its hidden complexity is a move, not a reduction.
- **Make every crate `#![no_std]` and declare `extern crate std;` only inside the boundary modules.**
  This is compiler-enforced purity at module granularity, which is stronger than a declaration. It
  lost on interface depth as well as on diff size. It does not move a boundary, it renames the same
  modules' dependency, and it costs an `alloc` import on every file in nine crates while leaving the
  reader with a second purity idiom that says less than the level declaration says.
- **A grep-level rule over a `src/pure/` directory in each adapter crate.** This is the shape the tree
  already uses for `mokmok-capability`. It lost because a positional grep cannot tell a type from a
  string, which is the weakness the repository's own notes record, and because it gives a
  `pub(crate)` function in a std crate no more evidence than a comment does.
- **Add no crate roles and only add tests.** This is the mutation-first reading of the ask. It lost
  because eight members have never been scored and the kernel's survivors sit behind a socket, so a
  test-only change is capped by what a socket can drive. The instrument has to exist before the
  measurement can.
- **One new crate per pure seam, roughly eight of them.** Lost because most of the seams are pure
  functions over values the owning crate already defines, and a crate of its own would add a public
  vocabulary for one function. Only the window and the credit own state, and only they get a crate.

## Open questions and risks

Grounding, read back against the tree, is right where I re-ran it: six `status=pass` from
`dependency-rules.sh check`, 618 citation pairs with zero unresolved, 32 bundle files with zero
problems, four `#![no_std]` crates, eight members with no recorded mutation run, and the kernel's
five helpers hard-asserting the confiner. Three of its statements need correcting, and the design is
built on the corrected form. The member-count drift it cites as `docs/spec/index.md:7` and
`docs/spec/workspace.md:22,26,49` is at `index.md:8` and `workspace.md:21,23,46`. Its section 5.3
lists `std::path::PathBuf` in `Setting` as one blocker, and it is five fields, `checkout`,
`readable`, `writable`, `program`, and `search`, with the whole `Task` round-tripping through the
durable log, so the design's answer is a representation-preserving newtype plus a golden record
fixture. Its section 5.3 blocker table also omits that `mokmok-agent-runtime` is async, that
`advance` is `pub async fn` and three of its four ports return `impl Future + Send`. Async is not a
`no_std` blocker, and the table should say so, because it is the first thing a reader will worry
about.

- Does `#![no_std]` plus `alloc` actually compile for `mokmok-agent-runtime`, given its `impl Future
  + Send` ports and its `Box<dyn Error + Send + Sync>` source alias? Judgment says yes, since
  `core::future` and `alloc` cover both, but the first PR that tries it decides.
- Should `retention_step` derive the retained byte total from the sizes it is handed, rather than
  trust the total the store keeps in the `topic` row? A wrong total changes the policy silently.
  Deriving it would force a scan of the retained window on every over-bound append, which is the cost
  the single step exists to avoid, and the store is the only caller, with its own tests over the
  total.
- Is the per-member digest rule too strict? A comment edit inside a scored crate forces a re-sweep,
  and the harness digest misses a `[profile.mutants]` change in the root manifest unless that row also
  covers `Cargo.toml`. Should the digest narrow to `src/` and `tests/` and accept the gaps?
- Does the runtime's `at` parameter still have a reader after the `tracing::debug!` line goes? If a
  record builder needs it, the deletion is one line rather than two, and the sketch overstates the
  saving.
- Does the declaration earn its place for the nine adapters, where it asserts nothing the manifest
  does not already show? Its value is forcing the choice and making the mix visible in the check's
  log. If that is not enough, the rule reduces to `pure` and `not yet pure`, which loses the
  distinction between an adapter and a crate that has not been looked at.
- Should the tui's three terminal-only survivors be killed by a pty harness or should its floor be
  set deliberately below 95? A silent lowering is the outcome to avoid, and the honest answer is a
  decision rather than a default.
- Adding `specBundle` to CI means an unrelated PR can go red on a citation it did not touch, because
  the rule is positional. Measured today, the repair is mechanical and every pair resolves, but the
  first red will say whether the rule is worth its cost.
- Is `mokmok-delivery` the right name for the window and the credit? `mokmok-flow` reads closer to
  the bus's word for it, and `mokmok-window` reads closer to the state it owns.

## Next implementation step

P0, as one PR. Add `[package.metadata.mokmok] purity = "pure"` to `mokmok-protocol`,
`mokmok-state-machine`, `mokmok-scheduler`, and `mokmok-capability`, `purity = "adapter"` to the
other eleven, add the three rules to `docs/spec/references/attesters/dependency-rules.sh`, and add
the `dependencyRules` check to `flake.nix`. Prove it with
`bash docs/spec/references/attesters/dependency-rules.sh check "$PWD"` printing
`pure=4 adapter=11`, then `git add` the touched files and `nix flake check`.
# Sketch: crate roles, purity, and the mutation base

Measured against trunk `fbe5270`. Every claim here is either measured in the tree or labelled as a
guess. The bodies marked `unimplemented!()` are placeholders. `[workspace.lints.clippy]` turns
`todo` and `panic` into warnings and CI denies warnings, so `todo!()` cannot be the placeholder and
`unimplemented!()` is the one that passes the lint set. No merged code carries one.

## The purity ladder

One declaration, three rules, one new check. The declaration lives in the crate's own manifest so a
new crate that forgets it fails, and so a reviewer reads the role in one line.

```toml
# every member's Cargo.toml
[package.metadata.mokmok]
purity = "pure"      # or "adapter"
```

`pure` means the crate is `#![no_std]` and the compiler proves it. Nothing else proves it, because a
module inside a std crate cannot be `no_std`, and a grep for `std::` cannot tell a type from a
string. `adapter` means the crate binds the world, and its role is to be thin.

The check is a new flake check named `dependencyRules`. It runs the file that already owns the
architecture's dependency rules, so there is one place a maintainer looks and one place the
architecture's section 9 points at.

| item | value |
| --- | --- |
| file | `docs/spec/references/attesters/dependency-rules.sh` |
| rules | `rule_every-member-declares-its-purity`, `rule_pure-crates-are-no_std`, `rule_pure-crates-depend-only-on-pure-crates` |
| check | `dependencyRules` in `flake.nix` |
| declaration | `[package.metadata.mokmok]` in each member's `Cargo.toml` |

The three rules, exactly. `rule_pure-crates-have-no-io-dependency` is renamed to
`rule_pure-crates-are-no_std`, and its selector becomes the declared level instead of
`no_std_crates()`.

1. `rule_every-member-declares-its-purity`. Every member of `[workspace] members` carries a
   `purity` value, and the value is one of `pure` and `adapter`. This is the rule the tree lacks
   today. `no_std_crates()` in the same file asks its question only of a crate that already says
   `#![no_std]`, so the opt-in rate is unmeasured.
2. `rule_pure-crates-are-no_std`. A crate declared `pure` has `^#!\[no_std\]` in its own
   `src/lib.rs` and names no crate from `IO_CRATES` in `[dependencies]`. Dev-dependencies are
   exempt, because `proptest` and `serde_json` with `std` are test-only.
3. `rule_pure-crates-depend-only-on-pure-crates`. A `pure` crate's `mokmok-` dependency edges point
   at `pure` crates. This is the topological constraint of grounding section 6 as a rule. It passes
   today over the four, and it makes "which crate can be pure next" a machine fact rather than a
   reading of the graph.

The check prints the count as its output, so a PR that turns a pure crate into an adapter shows up
in CI's log rather than in a diff nobody reads.

```nix
# flake.nix, inside `checks`
dependencyRules =
  pkgs.runCommand "dependency-rules" { nativeBuildInputs = [ pkgs.bash ]; } ''
    bash ${src}/docs/spec/references/attesters/dependency-rules.sh check ${src} | tee $out
  '';
```

`$out` is the rule report, so `nix flake check --print-build-logs` shows
`rule_pure-crates-are-no_std status=pass` and `pure=<n> adapter=<n>`. The script needs `bash` only
for the `check` mode. The `attest` mode's `jq` stays an operator dependency.

## The target crate list

Sixteen members, seven pure and nine adapters. `mokmok-delivery` is the only new crate.

| member | level | why |
| --- | --- | --- |
| `mokmok-protocol` | pure | no_std today. Identifiers and envelopes, no transport, storage, or clock. |
| `mokmok-state-machine` | pure | no_std today. The agent, session, turn, and execution fold. |
| `mokmok-scheduler` | pure | no_std today. Wakeups, deadlines, and leases over its own clock port. |
| `mokmok-capability` | pure | no_std today. Grants and policy. Enforcement lives in sandbox and egress. |
| `mokmok-patch` | pure | Three core items today at `src/lib.rs:3-5`. One attribute and one feature change. |
| `mokmok-delivery` | pure | New. The retained window and the delivery credit, which own the invariants that shipped a defect. |
| `mokmok-agent-runtime` | pure | Its own crate doc at `src/lib.rs:3` already claims purity. Seven local blockers, all named. |
| `mokmok-event-store` | adapter | `rusqlite` owns the transaction. The append precedence becomes a pure function in place. |
| `mokmok-event-bus` | adapter | `rusqlite`, `tokio`, and `axum`. The policy leaves for `mokmok-delivery`. |
| `mokmok-search` | adapter | `tantivy` binds the crate. The token, query, snippet, and render half goes `pub` in place. |
| `mokmok-sandbox` | adapter | `landlock`, `nix`, `tokio`, and real processes. |
| `mokmok-egress` | adapter | `tokio` socket work and `std::net`. |
| `mokmok-observability` | adapter | The `tracing` subscriber is the crate's std half. |
| `mokmok-artifact` | adapter | `std::fs` blob storage. `ContentHash` and `mark` stay in place. |
| `mokmok-kernel` | adapter | Composition, the durable log, and the daemon binary. |
| `mokmok-tui` | adapter | `ratatui` above the kernel. |

The purity of a crate is not the purity of its module tree. `mokmok-search`'s pure half is pure
logic in an adapter crate, and that is the intended end state, because splitting it would give
`String` and `usize` transforms a crate of their own.

## New crate: `mokmok-delivery`

Justified against `mokmok-os-architecture.md:32`. It owns the retained window and the delivery
credit. Those are state. Its invariants are that the newest event is never evicted, that the credit
ceiling is measured from the stored checkpoint and never from the retained floor, and that a lapse
travels alone and spends no credit. Its contract is two decisions and the types they decide over. Its
validation requirement is a property test over generated sizes and a property test over generated
credit triples, neither of which the current surface admits. It is one state machine, so
`mokmok-os-architecture.md:179` is satisfied.

```
mokmok-delivery/
  Cargo.toml         # mokmok-protocol, serde (alloc), uuid (serde, no std)
  src/lib.rs         # #![no_std]; crate doc; re-exports
  src/window.rs      # Seq, LogEpoch, Cursor, Window, Lapsed
  src/retention.rs   # Bounds, RetentionStep, retention_step
  src/credit.rs      # Credit, PullRequest, PullPlan, plan_pull
```

`src/lib.rs`:

```rust
//! The retained window and the delivery credit, decided with no store behind them.
//!
//! The two decisions here were the bus's worst place to keep a rule. Measuring the credit ceiling
//! from the retained floor let a subscriber that never acks keep drawing events, because eviction
//! raised the floor the ceiling was measured against.

#![no_std]

extern crate alloc;

mod credit;
mod retention;
mod window;

pub use credit::{Credit, PullPlan, PullRequest, plan_pull};
pub use retention::{Bounds, RetentionStep, retention_step};
pub use window::{Cursor, Lapsed, LogEpoch, Seq, Window};
```

`src/window.rs` moves five types out of `mokmok-event-bus/src/event.rs` unchanged, accessors
included, because the delivery decision names a window and the window is what the decision is
about. `mokmok-event-bus/src/lib.rs:59` re-exports them from `mokmok-delivery`, so the bus's public
surface is byte-identical.

```rust
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, PartialOrd, serde::Deserialize, serde::Serialize)]
#[serde(transparent)]
pub struct Seq(u64);

impl Seq {
    #[must_use]
    pub const fn new(value: u64) -> Self { Self(value) }

    #[must_use]
    pub const fn get(self) -> u64 { self.0 }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(transparent)]
pub struct LogEpoch(uuid::Uuid);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct Cursor { epoch: LogEpoch, seq: Seq }

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct Window { epoch: LogEpoch, first: Seq, next: Seq }

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct Lapsed { held: Cursor, from: Seq }

impl Window {
    pub const fn new(epoch: LogEpoch, first: Seq, next: Seq) -> Self {
        Self { epoch, first, next }
    }

    #[must_use]
    pub const fn first(self) -> Seq { self.first }

    #[must_use]
    pub const fn next(self) -> Seq { self.next }
}
```

`src/retention.rs`:

```rust
/// The two ceilings the retained window is held under.
#[derive(Clone, Copy, Debug)]
pub struct Bounds {
    pub events: NonZeroUsize,
    pub bytes: NonZeroUsize,
}

/// What the next step of the retention policy does.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetentionStep {
    /// The oldest retained event goes, leaving this floor and this byte total.
    Drop { first: Seq, bytes: u64 },
    /// Both bounds hold. Nothing goes.
    Keep,
}

/// Decides the next step of the retention policy.
///
/// One step, not the whole plan, so the caller keeps its existing shape of one read and at most one
/// `DELETE` per evicted event and pays no scan of the retained window. Folding this over a size list
/// is the whole policy, which is what the property test does and what `append` does.
///
/// `bytes` must be the total size of the events the window holds. The newest event is never dropped,
/// however large it is, and the byte bound never drops the last event.
#[must_use]
pub const fn retention_step(
    window: Window,
    bytes: u64,
    bounds: Bounds,
    oldest_bytes: u64,
) -> RetentionStep {
    unimplemented!()
}
```

The contract the type does not enforce is that `bytes` is the sum of the retained events' sizes. The
store is the only caller and it keeps that total in the `topic` row, so a wrong total is a store bug
rather than a delivery bug, and the store's own tests cover the total. See open questions.

`src/credit.rs`:

```rust
/// The two ceilings one subscriber's credit is measured under.
#[derive(Clone, Copy, Debug)]
pub struct Credit {
    /// The most events one batch may carry, whatever the credit allows.
    pub batch: NonZeroUsize,
    /// The most events one subscriber may hold unacked.
    pub in_flight: NonZeroUsize,
}

/// One pull, as the numbers the decision needs and nothing else.
#[derive(Clone, Copy, Debug)]
pub struct PullRequest {
    /// The sequence the subscriber is holding after its last batch.
    pub delivered: Seq,
    /// The sequence the subscriber has acked. This is the anchor the ceiling is measured from.
    pub acked: Seq,
    /// The window the topic holds now.
    pub window: Window,
    /// The credit ceilings.
    pub credit: Credit,
    /// The count the caller asked for.
    pub max: NonZeroUsize,
}

/// What a pull does, decided before any read happens.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PullPlan {
    /// Hand over events at `from`, at most `count` of them.
    Batch { from: Seq, count: NonZeroUsize, lapsed: Option<Lapsed> },
    /// The checkpoint is behind the retained floor, so the gap travels alone, with no event and no
    /// credit spent. An event here would let a subscriber that never acks keep drawing.
    Lapse(Lapsed),
    /// The credit is spent. Only an ack can free it.
    Park,
}

/// Decides what one pull does.
///
/// The ceiling is `acked + in_flight` and never the retained floor, so eviction cannot refresh a
/// spent credit.
#[must_use]
pub fn plan_pull(request: PullRequest) -> PullPlan {
    unimplemented!()
}
```

## Changed signatures in place

These are visibility and parameter changes inside the crate that already owns the decision. No new
crate. The body of each is its current body, and the current body of each already reads only its
arguments.

### `mokmok-event-bus`

```rust
// src/store.rs, inside `append`. The loop keeps its shape and the policy leaves it.
while let RetentionStep::Drop { first: next_first, bytes: next_bytes } =
    retention_step(window_with(first, next), bytes, bounds, oldest_bytes()?)
{
    // one DELETE, then the floor and the total move on
}

// src/server.rs, inside `pull`.
match mokmok_delivery::plan_pull(request) {
    PullPlan::Batch { from, count, lapsed } => { /* store().read(topic, from, count).await */ },
    PullPlan::Lapse(lapsed) => { /* answer the gap with no event */ },
    PullPlan::Park => { /* park on spent credit */ },
}
```

`src/store.rs:1144`'s `byte_bound_start`, the `#[cfg(test)]` shadow of the eviction policy, is
deleted. The real policy becomes the tested one.

### `mokmok-agent-runtime`

The crate becomes `#![no_std]` plus `extern crate alloc;`, and gets one
`#[cfg(test)] extern crate std;` because `src/test_support.rs:4` needs `std::sync::Mutex` in the
test build and `core` has no `Mutex`. The shipped build is what the rule reads.

```rust
// src/advance.rs. The record list is split out of the store write. The other seven step builders
// already return a `Step` built from their arguments, so they take `pub(crate)` and a
// `Step::records()` accessor.
pub(crate) fn ask_records(
    transcript: &Transcript,
    turn: TurnID,
    number: TurnNumber,
    attempt: Attempt,
    context: ArtifactRef,
) -> Vec<Record> {
    unimplemented!()
}

pub(crate) fn number_of(transcript: &Transcript, turn: TurnID) -> u64 {
    unimplemented!()
}
```

```rust
// src/task.rs. Five `PathBuf` fields become one crate-owned path, and the wire form is unchanged
// because `PathBuf` already serializes as its string on Unix and errors on a non-UTF-8 path, so a
// stored record can only hold a UTF-8 one.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(transparent)]
pub struct Path(String);

impl Path {
    #[must_use]
    pub fn as_str(&self) -> &str { &self.0 }
}

pub struct Setting {
    pub checkout: Path,
    pub readable: Vec<Path>,
    pub writable: Vec<Path>,
    pub program: Path,
    pub deadline: Duration,
    #[serde(default)]
    pub search: Vec<Path>,
}
```

The rest of the crate's blockers, each a local edit. `src/error.rs:10`'s `Source` becomes
`Box<dyn core::error::Error + Send + Sync>`. `src/record.rs:343`'s `impl std::error::Error for
Refusal` becomes `core::error::Error`. `thiserror` drops its default features. `src/dialogue.rs:3`
moves to `alloc::collections::BTreeMap`. `serde_json` drops `std` for `alloc`. `uuid` drops `std`
and `v7` for `["serde", "v5"]`. `src/advance.rs:132`'s `tracing::debug!` is deleted, and with it
the `at` parameter of `advance` if nothing else reads it. `src/context.rs:288`'s `paths` takes
`&[Path]`.

### `mokmok-kernel`

```rust
// src/recovery.rs. The fold leaves the append.
pub(crate) struct RecoveryPlan {
    pub entries: Vec<Entry>,
    pub recovered: Vec<ExecutionID>,
}

pub(crate) fn recovery_entries(state: &Session, log: &Log) -> Result<RecoveryPlan, Error> {
    unimplemented!()
}

// src/agent.rs. The outbox notice leaves `Journal::append`.
pub(crate) fn notice_of(
    session: SessionID,
    command: CommandId,
    at: Moment,
    records: &[Record],
) -> Result<OutboxMessage, Error> {
    unimplemented!()
}

// src/agent.rs. The fold is the argument rather than something the ledger opens.
pub(crate) fn number_of(transcript: &Transcript, turn: TurnID) -> u64 {
    unimplemented!()
}

// src/slice.rs. The turn count is the argument rather than the log.
pub(crate) fn mint(command: CommandId, at: Moment, turns: usize) -> (TurnID, ExecutionID, TurnNumber) {
    unimplemented!()
}

// src/execution.rs. The result mapping leaves the blob store.
pub(crate) fn result_of(
    outcome: Outcome,
    started: Started,
    stdout: ArtifactRef,
    stderr: ArtifactRef,
) -> ExecutionResult {
    unimplemented!()
}

// src/serve.rs. The join error becomes a local fact.
pub(crate) enum Ended {
    Served,
    Failed(ServeError),
    Panicked,
}

pub(crate) fn bus_ended(ended: Ended) -> Error {
    unimplemented!()
}

// src/config.rs. The crash decision leaves the process exit, and the credential check leaves the
// metadata read.
pub(crate) const fn crash_now(
    point: CrashPoint,
    configured: Option<CrashPoint>,
    unlocked: bool,
) -> bool {
    unimplemented!()
}

pub(crate) fn credential_ok(mode: u32, path: &Path, writable: &[PathBuf]) -> Result<(), ConfigError> {
    unimplemented!()
}

// src/notify.rs. The deadline arithmetic takes the clock as a value.
pub(crate) fn due(next: Option<Duration>, every: Option<Duration>, now: Duration) -> Option<Duration> {
    unimplemented!()
}
```

`src/slice.rs`'s `transcript_line`, `launch`, and `settled` take `pub(crate)` and nothing else.
`src/db.rs`'s `Log::push` and `unsettled_sessions` gain a pure sibling over the decoded commits.
`src/provider.rs`'s `outcome_of` and `detail_of` take `pub(crate)`. `src/config.rs::resolve` keeps
its shape, because it interleaves real filesystem facts with creation and belongs at the boundary,
where `principle-boundary-discipline` puts validation.

The kernel does not become a pure crate. It composes twelve crates and runs the daemon.

### `mokmok-search`

```rust
// src/query.rs. The clause list is the pure half of `boolean`; the tantivy adapter maps it.
pub struct Clause { pub occur: Occur, pub field: FieldID, pub term: String, pub weight: u32, pub fuzzy: bool }

pub struct FieldID(u32);

#[must_use]
pub fn clauses(words: &[String], fields: [FieldID; 3]) -> Vec<Clause> {
    unimplemented!()
}

// src/query.rs. Arrives from the kernel's `Index::find`, which deletes its own comparator.
#[must_use]
pub fn rank(hits: &mut [Hit], limit: usize) -> usize {
    unimplemented!()
}
```

`src/token.rs`, `src/render.rs`, and `src/corpus.rs::capped` take `pub` and property tests.

### `mokmok-event-store`

```rust
// src/store.rs. The append precedence leaves the transaction.
pub enum Preflight {
    Replayed(Committed),
    Conflicted { expected: Expected, found: Version },
    Commit { version: Version },
}

#[must_use]
pub fn preflight(recorded: Option<Committed>, found: Version, expected: Expected) -> Preflight {
    unimplemented!()
}
```

The row codecs `position_from`, `version_from`, `time_from_millis`, `to_millis`, and
`StoredEvent::into_event` take `pub(crate)` and a round-trip property test.

### `mokmok-sandbox`

```rust
// src/capability.rs. The directory fact is passed in rather than read, so the decision is a value
// decision and the only test stops needing a temp file.
#[must_use]
pub fn covers(capability: &Capability, exposure: &Exposure, path_is_directory: bool) -> bool {
    unimplemented!()
}
```

### `mokmok-egress`, `mokmok-observability`, `mokmok-artifact`

Nothing moves. `classify`, `Rules::allows`, `parse_connect`, `parse_authority`, `parse_port`, and
`RateLimit::acquire` take `pub` with tests. `Redaction::apply`, `metrics`, `AuditEntry`, and
`append_and_export` take tests over their existing ports. `ContentHash::of`, its `FromStr`, and
`Collector::mark` take tests over the in-tree fake. Each crate's first mutation score lands as a row
in `mutation.toml`, which is the point of the phase.

### `mokmok-tui`

Nothing moves. The reducer is already behind `Screen` and `Engine`, and its purity cannot exceed the
runtime's while it also composes the kernel. The three terminal-only survivors need a pty harness,
which is a test, not an extraction.

## The mutation gate reaches sixteen members

`scripts/mutants.sh` is one package per run, selected by `MUTANTS_PACKAGE`, and nothing records
which packages were scored. Grounding section 4 measures eight members with no run at all and the
kernel failing at the unmutated baseline. Two mechanisms fix that, and both live in the one script
so the gate and its check cannot drift apart.

**Mode one, the one package.** Unchanged, and it keeps its default. The one addition is unconditional.
Before the run, the script builds the confiner and exports its absolute path:

```bash
cargo build -p mokmok-sandbox --bin mokmok-sandbox-confine >/dev/null
export MOKMOK_CONFINER="$PWD/target/debug/mokmok-sandbox-confine"
```

The five kernel helpers already read `MOKMOK_CONFINER` first (`mokmok-kernel/tests/agent.rs:99-107`
and four more), so the env route is a hook the tree built and nothing used. The path is absolute and
outside the tree cargo-mutants mutates, so it survives the harness's per-job copy. No test assertion
weakens, which is the difference between this and a fallback to the host's shell.

**Mode two, the sweep.** `./scripts/mutants.sh --all` walks every member in topological order, runs
`cargo mutants --package <member> --jobs 8 --output target/mutants/<member>`, scores it with the
existing arithmetic, and rewrites `mutation.toml` at the repository root. Topological order matters
because a member's integration tests may need another member's binary, which is the kernel's failure
today. `MUTANTS_PACKAGE` and `MUTANTS_TARGET` keep their meaning in both modes.

`mutation.toml` is taplo-formatted, because `treefmt` runs `taplo` in the gate.

```toml
[harness]
digest = "<sha256 of scripts/mutants.sh, .cargo/mutants.toml, and [profile.mutants]>"
recorded = "2026-10-16T00:00:00Z"

[member.mokmok-event-bus]
# caught, missed, timeout, and unviable as the run computed them
score = 98.3
caught = 183
missed = 3
timeout = 0
target = 95
digest = "<sha256 of the member's tracked files, sorted by relative path>"

[member.mokmok-kernel]
score = 86.9
target = 86
```

The `harness` row covers the three files that change what a score means. A member row covers every
tracked file in the member's directory, so a test edit moves the digest, which is correct, because a
test edit moves the score.

**Mode three, the check.** `./scripts/mutants.sh --check-baseline <repo>` reads `mutation.toml`,
recomputes both digests through the same code path that wrote them, and exits non-zero when a member
has no row, when the harness digest moved, when a member's digest moved, or when a score is below its
target. One implementation of the digest, three modes. A new flake check runs it.

```nix
mutationBaseline =
  pkgs.runCommand "mutation-baseline" { nativeBuildInputs = [ pkgs.bash ]; } ''
    bash ${src}/scripts/mutants.sh --check-baseline ${src} | tee $out
  '';
```

The floor stays 95 and stays in the script. The target is per member and it only rises. The kernel's
row starts at its measured 86.9, and P6 raises it to 95 in the same PR as the tests that earn it. The
check's output is the row count and the members below 95, so the distance to the target state is a
number in CI's log rather than a claim in a document. The end state is sixteen rows, every target 95,
every score at or above its target.

Adding a member adds a row or fails the check. That is the rule the tree lacks now, where a new crate
can be born with no assertion strength recorded at all.

## Seam table

| seam | today | after | into | test this makes possible |
| --- | --- | --- | --- | --- |
| eviction policy, `store.rs:432-487` | arithmetic interleaved with `DELETE` and `UPDATE` | `delivery::retention_step` | new crate | a property test that folds the step over `(window, bytes, bounds)` and a generated size list, proving the count bound, the byte bound, and that the newest never goes. Today one shadow under `#[cfg(test)]` and one path through SQLite. |
| delivery credit and lapse, `server.rs:555-621` | arithmetic interleaved with `store().read().await` | `delivery::plan_pull` | new crate | the property that would have caught the shipped defect, that a subscriber with no new acks draws at most `in_flight` events, for every generated `(delivered, acked, window)` triple. Today it needs a socket, a churning publisher, and a real store. |
| kernel recovery, `recovery.rs:29-69` | fold interleaved with `log.state`, `log.verify`, `db.append` | `recovery_entries` | in place | the exact entry vector for a hand-built `Log` and `Session`, including the third entry that exists only when the turn has no outcome. Today only through `Db`. |
| kernel step notice, `agent.rs:118` | reachable only by `Journal::append` writing SQLite | `notice_of` | in place | a literal `Record` list folded into a `Notice`. |
| kernel transcript line, `slice.rs:396` | reachable only through `status()`, which opens the DB | `pub(crate)` | in place | a nine-arm table over hand-built `Record`s. |
| kernel turn number, `agent.rs:872`, `advance.rs` | folds `ledger.transcript()` to read one number | `number_of` | in place | numbering a hand-built `Transcript`. |
| kernel mint, `slice.rs:206` | reads `log.turns_in_order()` | `mint(command, at, turns)` | in place | the number and the ids for a literal turn count. |
| kernel result mapping, `execution.rs:184-191` | interleaved with `BlobStore` and `std::fs::read` | `result_of` | in place | the three outcomes by two started states over an `ExecutionResult`, with no store. |
| kernel bus end, `serve.rs:331` | blocked by `tokio::task::JoinError` | `bus_ended(Ended)` | in place | a three-arm table with no task spawned. |
| kernel crash and credential decisions | decide then `process::exit(97)` / `std::fs::metadata` | `crash_now`, `credential_ok` | in place | the eight-case crash table and the mode table, with no environment write and no real file. |
| kernel notify arithmetic, `notify.rs:148` | blocked by `tokio::time::Instant` in the signature | `due(next, every, now)` | in place | due-ness over a table of offsets, with no runtime. |
| cross-root ranking, `index.rs:70-95` | duplicated comparator in the kernel | `mokmok_search::rank` | existing adapter | a property test over generated hits, and one comparator instead of two. |
| append precedence, `store.rs:378-395` | three reads interleaved with one commit | `preflight` | in place | a precedence table where a replayed command wins over a version conflict, which is the rule that is easy to lose. |
| coverage decision, `capability.rs:191` | `capability.path.is_dir()` inside the decision | `covers(.., path_is_directory)` | in place | component-wise coverage over generated path pairs, including `/a/b` against `/a/bc`. Today the only test writes a temp file. |
| runtime step records, `advance.rs:422-709` | eight builders, one of which needs a `Journal` to be observed | `ask_records` plus `pub(crate)` | in place | the exact `Vec<Record>` per step state against a hand-built `Transcript`. |
| runtime prompt bytes, `context.rs:244-288` | blocked by `PathBuf` and a display call | `paths`, `bytes_of`, `keep_newest` over the crate's own `Path` | in place | a property test that the kept turns fit the budget and never split a character. |
| search clause list, `query.rs:33` | built inside a tantivy `Query` | `clauses` | in place | the clause shape for generated word lists, with no index. |
| search snippet and cap, `query.rs:89`, `corpus.rs:347` | private, covered only through a real index | `pub` plus property tests | in place | the clip on a char boundary and the cap arithmetic on their own. |

## Phases

One independently landable PR each, in this order, each ending in a named check. The order is
topological because a crate can only be `#![no_std]` once every type its public surface mentions
already is.

| phase | what lands | ends in |
| --- | --- | --- |
| P0 | The declaration in fifteen manifests, the three rules in `dependency-rules.sh`, the `dependencyRules` check in `flake.nix`. Nothing else moves. | `nix flake check` green, `dependencyRules` logging `pure=4 adapter=11`. |
| P1 | Subtraction. The stale `mokmok-scheduler` edge, the five confiner helpers collapsed to one, the redundant half of `rule_capability-performs-no-enforcement-io`, the `docs/spec` drift, `mokmok-data/` in `.gitignore`, and the `specBundle` check that runs `okf-conformance.sh` and `line-references.sh`. | `cargo metadata` current, `nix flake check` green with `dependencyRules` and `specBundle`. |
| P2 | `mokmok-patch` becomes `#![no_std]` with `thiserror` off its default features. The cheapest possible purity win, done by hand to learn the recipe. | `dependencyRules` logging `pure=5 adapter=10`, and `MUTANTS_PACKAGE=mokmok-patch ./scripts/mutants.sh` at 100%. |
| P3 | `mokmok-delivery` lands with the five window types, `retention_step`, the folded eviction property test, the store's loop, and the deleted shadow. | `dependencyRules` logging `pure=6 adapter=10`, `cargo test -p mokmok-event-bus` green. |
| P4 | `plan_pull`, `PullPlan`, and the credit property test. `pull` executes the plan. | `MUTANTS_PACKAGE=mokmok-delivery ./scripts/mutants.sh` at 95% or above, the bus's socket tests green. |
| P5 | The gate reaches every member. `scripts/mutants.sh --all` builds the confiner, exports `MOKMOK_CONFINER`, sweeps all sixteen, and writes `mutation.toml`. The `mutationBaseline` check reads it. | `nix flake check` green, sixteen rows in `mutation.toml`, the check logging how many rows sit below 95. |
| P6 | The kernel. The in-place extractions of the seam table plus the integration assertions that kill the twenty-four survivors. | `MUTANTS_PACKAGE=mokmok-kernel ./scripts/mutants.sh` at 95%, the kernel's target raised to 95 in the same PR. |
| P7 | `mokmok-agent-runtime` becomes `#[no_std]`. | `dependencyRules` logging `pure=7 adapter=9`, the wire golden test green, the runtime's row in `mutation.toml` unchanged at 99.2%. |
| P8 | The in-place seams, one PR per crate. Search and `rank`, event store, sandbox, egress, observability, artifact. The tui pty harness is its own PR. | each crate's row in `mutation.toml` refreshed and its target raised toward 95. |
| P9 | The bundle describes the new roles. `docs/spec/crates/delivery.md`, the sixteen-member table, the closed gate decision. | `specBundle` green. |

Dependency edges that each phase must respect. P0 precedes everything, because it is the
instrument. P1 precedes P2 to P9, because it removes the base the additions sit on and turns the
citation check on while the debt is small. P3 depends on P0 only. P4 depends on P3. P5 depends on
nothing but P1, and its kernel row is the first score the kernel has ever had. P6 depends on P5 for
its measurement. P7 depends on P0. P8 depends on P5 for its receipts.

Every phase that edits a crate pays two fixed costs. `cargo metadata --format-version 1` after any
manifest edit that adds an edge, because a stale lock fails the check before anything compiles. The
citation repair in `docs/spec`, which is 618 positional pairs at trunk and breaks on any line move
inside a cited crate.

## What gets deleted first

Grounding sections 4, 7, and 9 name live candidates. All of these land in P1 or in the phase that
makes the replacement possible.

1. `mokmok-kernel/Cargo.toml`'s `mokmok-scheduler` line. Nothing in the kernel names a symbol from
   it, so the line records an edge that does not exist. Deleting it leaves the scheduler with no
   dependent, which is a fact worth seeing in the graph rather than a fact buried in a manifest.
2. The five copies of the confiner helper in `mokmok-kernel/tests/`, collapsed to one
   `tests/common/mod.rs`, and the three bespoke refusal messages in them. Five copies of one
   resolution order is five places a fix can miss. The hard assert itself stays, and the tui's
   first-shell-on-`PATH` fallback stays with it, because those two files answer different questions.
   The tui's file dispatches no tool, so its confiner only has to be runnable
   (`mokmok-tui/tests/conversation.rs:100-102`), while the kernel's tests dispatch a program and
   would quietly run it unconfined behind a shell. The fix for the kernel is a real confiner through
   the env route the five helpers already read, which is where the assertion belongs, not in the
   removal of a guard.
3. `rule_capability-performs-no-enforcement-io`'s source grep, and the `ENFORCEMENT_CRATES` list it
   alone uses. The grep looks for `std::(process|fs|net)` and `Command::new` in
   `mokmok-capability/src`, and the crate is already `#![no_std]`, so the compiler rejects every one
   of those paths before the grep can look. Keeping the rule's name and its dependency half, and
   deleting the half a stronger mechanism already covers, and correcting the rule's description in
   `docs/spec/verification/dependency-rules.md` in the same PR.
4. `mokmok-event-bus/src/store.rs:1144`'s `byte_bound_start`, once `retention_step` is the policy. A test-only
   shadow of production arithmetic is a second implementation waiting to differ.
5. `mokmok-kernel/src/index.rs`'s comparator and truncation, once `mokmok_search::rank` exists. The
   rule is the same in two crates today.
6. `mokmok-agent-runtime/src/advance.rs:132`'s `tracing::debug!` and, if nothing else reads it, the
   `at` parameter of `advance`. The parameter exists for one log line and no decision.
7. The `docs/spec` drift that grounding section 7 measured, member counts in `index.md:8` and
   `workspace.md:23`, the absent `patch.md`, the case counts and module list in `kernel.md`, the
   missing `error` module in `observability.md`, the wall-clock claim, `strategy.md:45`'s proptest
   count, and the stale receipts in `log.md`.
8. `mokmok-data/` from the tree's own view, by one `.gitignore` line. It is the daemon's default
   `--data-dir`, a runtime artifact, and not a crate.
9. The opt-in selector itself. `no_std_crates()` is replaced by the declared level, so the rule stops
   asking only the crates that already answered.

Grounding section 9's other candidate is the three ports that shipped before their second user
(`docs/spec/decisions.md:69-73`). This design keeps all three. Each is the boundary that makes an
otherwise untestable path testable, and deleting one to save a file would remove the seam the
operator asked to make firmer. The scheduler's manifest line is the only one of the four that goes.

## What this design does not do

- No crate per seam. `mokmok-delivery` is the only new crate, and it earns its place by owning the
  window and the credit.
- No `mokmok-kernel-core` and no `mokmok-recovery`. See the alternatives in the rationale.
- No `extern crate std;` per module. It is compiler-enforced purity, and it costs an `alloc` import
  on every file in nine crates without moving a boundary.
- No purity for `mokmok-search`, `mokmok-egress`, `mokmok-observability`, or `mokmok-artifact`. Their
  pure halves stay where they are, `pub` and tested.
- No lowering of the 95% floor. A member's recorded target rises; it never falls.
- No wire version bump. `Setting`'s path fields change type and keep their representation.
- No change to `mokmok-event-bus`'s public surface. The moved types are re-exported.
- No mutation sweep inside `nix flake check`. Sixteen packages at `--jobs 8` is tens of minutes. The
  check is tree-only and runs in seconds, and the sweep stays an operator command.
- No Loom, no fuzzing, and no formal verification. Their absence is recorded in
  `docs/spec/verification/strategy.md` and stays recorded.
- No fix for the load-sensitive `mokmok-sandbox` supervision test. It is a test-support defect in a
  crate this design touches, and it belongs in that crate's own PR.

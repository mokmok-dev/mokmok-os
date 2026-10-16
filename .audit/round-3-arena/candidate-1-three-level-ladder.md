# Crate roles and purity for the mokmok-os workspace

## Problem

The workspace has fifteen members and one statement of intent about purity
(`mokmok-os-architecture.md:80-82`). Four members are `#![no_std]`. The rule that checks the intent,
`rule_pure-crates-have-no-io-dependency` in `docs/spec/references/attesters/dependency-rules.sh`,
selects its subjects with `grep -q '^#\[no_std\]' "$repo/$dir/src/lib.rs"`. Purity is therefore
opt-in per crate, and the rule can only ever confirm a crate that already opted in. Nothing measures
the opt-in rate. The same script holds `rule_capability-performs-no-enforcement-io`, a grep over
`std::(process|fs|net)|Command::new` that passes on this tree, and it is the shape worth copying one
level down. The unit of purity that matters is the module, not the crate.

Three measured facts decide the shape.

- `mokmok-agent-runtime/src/lib.rs:4-6` already claims "The crate is pure: it reads no clock, no
  file, and no socket, and no sandbox, store, or HTTP type appears in it." The claim is true and
  unenforced. Six removable blockers stand between the claim and `#![no_std]` (grounding 5.3).
- The one defect that shipped in this tree was pure arithmetic. The credit ceiling in
  `mokmok-event-bus/src/server.rs` was first measured from the retained floor instead of the stored
  checkpoint, so eviction refreshed a subscriber's credit forever and a subscriber that never acked
  kept drawing events. No property test could reach it, because the arithmetic sits between
  `store().read(...).await` calls, and the same policy is re-implemented in a `#[cfg(test)]` shadow
  at `mokmok-event-bus/src/store.rs:1144`.
- `scripts/mutants.sh` scores one package per run, is not in `nix flake check`, and nothing in the
  tree says which packages were scored. `mokmok-kernel` cannot be scored at all. Its five
  integration helpers resolve the confiner as a sibling of `CARGO_BIN_EXE_mokmokd` and assert it
  exists, and the confiner is a bin of `mokmok-sandbox`, so a package-scoped build never makes it.

So the problem is not "extract more crates". It is that purity has no owner and no check, and the
extraction order is constrained. A crate can only be `#![no_std]` once every type its public surface
mentions is `#![no_std]`. That makes this a topological order, not a fan-out.

**Corrections to the grounding.** `docs/spec/references/attesters/` holds four scripts, not three;
the fourth, `nextest-count.sh`, attests a test-log receipt and reads no tree. The grounding calls
the runtime's `Setting.checkout` change "a wire change"; measured, `PathBuf`'s serde form is a string
and already fails on a path that is not UTF-8, so a transparent string newtype is wire-identical and
no log migration is needed. And grounding 5.8 lists egress's blockers without saying what this
session measured: `mokmok-egress` carries a `tokio` edge in `connect.rs` whose only callers are that
crate's own tests, and a `Resolver` port whose only implementer and only user is
`mokmok-kernel/src/provider.rs`. Those are deletions, not extractions.

## Usage (caller's view)

The bus executes a decision instead of making one.

```rust
// mokmok-event-bus/src/server.rs
let plan = mokmok_bus_core::plan_pull(window, delivered, acked, server.config.limits, max);
match plan {
    PullPlan::Batch { from, count } => server.bus.store().read(topic, from, count).await,
    PullPlan::Lapsed { held, from } => answer_the_lapse(held, from),
    PullPlan::Spent => Step::Park { parked: Parked::Spent },
}
```

```rust
// mokmok-event-bus/src/store.rs, inside append, before the transaction commits
let sizes = retained_sizes(&transaction, topic)?;
let evicted = mokmok_bus_core::evict(state.window(), bytes, appended, retention, &sizes)
    .ok_or(StoreError::Corrupt)?;
delete_oldest(&transaction, topic, evicted.first, evicted.delete)?;
write_window(&transaction, topic, evicted)?;
```

The kernel's finder ranks through the crate the index reads through.

```rust
// mokmok-kernel/src/index.rs, the end of Finder::find
let hits: Vec<Hit> = corpora.iter().flat_map(|c| c.query(query, LIMIT).hits).collect();
Ok(FindReport { observation: mokmok_search_core::render(&Response {
    indexed,
    hits: mokmok_search_core::rank(hits, Corpus::DEFAULT_LIMIT),
}) })
```

The test this buys, which nothing in the tree can write today:

```rust
// mokmok-bus-core/src/pull.rs, tests
proptest! {
    #[test]
    fn a_pull_never_pays_credit_the_subscriber_does_not_hold(
        (window, delivered, acked, limits, max) in a_subscriber_at_a_window(),
    ) {
        match plan_pull(window, delivered, acked, limits, max) {
            PullPlan::Batch { from, .. } => prop_assert!(
                from.get() >= acked.get()
                    && from.get() < acked.get() + limits.in_flight.get() as u64
            ),
            PullPlan::Spent => prop_assert!(
                acked.get() + limits.in_flight.get() as u64 <= delivered.get()
            ),
            PullPlan::Lapsed { from, .. } => prop_assert_eq!(from, window.first()),
        }
    }
}
```

That assertion is the shipped defect stated once. Today it needs SQLite, a running server, and a
publisher that churns the log past the subscriber's checkpoint.

## Shape

The workspace becomes **seventeen members with three declared purity levels**, and the declaration
is the role division. `crates.toml` at the root names every member, its role in one sentence, its
purity level, and, for a `std` member, the modules allowed to use `std` or a runtime crate.

- **`pure`** means the crate carries `#![no_std]`. The compiler proves it; no rule needs to.
- **`std`** means the crate is std and `allowed` lists its modules that may use `std`. Every other
  module in it is checked clean by grep.
- **`shell`** means a composition root the architecture names as one. Only `mokmok-kernel` and
  `mokmok-tui` may hold it, and the rule hardcodes those two names, so a third needs a reviewed edit.

The levels are a deliberate strength ladder, per the **encode-lessons-in-structure** principle
skill: an unrepresentable state beats a lint beats a convention. Where the compiler can hold the
line, `pure` makes it hold. Where it cannot, the module list holds it, and that list is also the debt
ledger, because it is a list of exceptions that should only shrink.

End state:

| member | purity | allowed | why it sits there |
| --- | --- | --- | --- |
| `mokmok-protocol` | pure | | unchanged |
| `mokmok-state-machine` | pure | | unchanged |
| `mokmok-scheduler` | pure | | unchanged |
| `mokmok-capability` | pure | | unchanged |
| `mokmok-patch` | pure | | **new.** `std::fmt::Write` and `std::str` are the same items in `core`, and `thiserror 2.0.21` is itself `#![no_std]` |
| `mokmok-agent-runtime` | pure | | **new.** Its own doc already claims it; six feature flags and one owned path type stood in the way |
| `mokmok-bus-core` | pure | | **new crate.** The bus's retention and delivery policy |
| `mokmok-search-core` | pure | | **new crate.** Tokenizing, snippetting, ranking and rendering |
| `mokmok-event-bus` | std | `bus.rs`, `client.rs`, `config.rs`, `event.rs`, `server.rs`, `store.rs`, `wire.rs` | rusqlite, axum, tokio |
| `mokmok-event-store` | std | `append.rs`, `error.rs`, `event.rs`, `store.rs`, `write.rs`, `bin/mokmok-event-store-crash.rs` | rusqlite |
| `mokmok-egress` | std | `audit.rs` | one deliberate clock read |
| `mokmok-sandbox` | std | `artifact.rs`, `capability.rs`, `config.rs`, `confine.rs`, `output.rs`, `plan.rs`, `request.rs`, `run.rs`, `sandbox.rs`, `workspace.rs`, `bin/mokmok-sandbox-confine.rs` | landlock, nix, process supervision |
| `mokmok-artifact` | std | `blob.rs`, `filesystem.rs`, `lib.rs` | the `std::fs` blob store |
| `mokmok-observability` | std | `error.rs`, `format.rs`, `init.rs`, `journal.rs`, `support.rs` | the fs journal and the tracing subscriber |
| `mokmok-search` | std | `corpus.rs`, `error.rs`, `walk.rs` | tantivy and the filesystem walk |
| `mokmok-kernel` | shell | | the composition root |
| `mokmok-tui` | shell | | an application above the kernel |

Eight pure, seven std, two shell.

The `allowed` lists are the measured output of `dependency-rules.sh propose` on trunk `fbe5270`,
with one comment-stripping step: `sed -e '/^#\[cfg(test)\]/,$d' -e 's,//.*$,,'` over each `src/**/*.rs`,
then three greps. They are not hand-written, so a reader can rerun them (the **build-the-lever**
principle skill). The pattern itself came from a census of the tree, `grep -oE '\bstd::[a-z_]+'` over
library source after the same strip, which showed which `std` modules are actually reached:
`fs` 47, `io` 40, `path` 37, `time` 27, `fmt` 22, `sync` 15, `process` 15, `env` 14, `os` 12,
`collections` 12, `ffi` 9, `num` 8, `net` 7, `error` 7. That census is why the pattern names three
kinds of thing rather than matching every `std::` path. The three are the modules with no
`core`/`alloc` counterpart, the individual items whose module also holds a `core` item, and the
runtime crates.

```sh
# A module may use std only if `crates.toml` names it. The first pattern is a module list (no
# core/alloc counterpart); the second is an item list, because `std::time` and `std::collections`
# each hold one of each and a brace import hides which is meant; the third is the runtimes.
IMPURE_MODULES='std::(fs|io|path|process|env|os|ffi|thread)\b'
IMPURE_ITEMS='\b(SystemTime|Instant|TcpStream|TcpListener|UdpSocket|Mutex|RwLock|Condvar|OnceLock|Barrier|HashMap|HashSet|OsString)\b'
IMPURE_CRATES='\b(tokio|rusqlite|tantivy|landlock|nix|axum|reqwest|tracing|tracing_subscriber)::|Command::new'

# The convention the tree already keeps: a file's `#[cfg(test)]` module is its last block.
library_source() { sed -e '/^#\[cfg(test)\]/,$d' -e 's,//.*$,,' "$1"; }
```

**The check.** `docs/spec/references/attesters/dependency-rules.sh` gains four rules beside the six
already there, sharing `run_rule`'s dispatch, and `flake.nix` gains one check.

- `members-have-declared-roles`. `crates.toml`'s member set equals the manifest's, in both
  directions.
- `pure-crates-are-no_std`. `purity = "pure"` holds if and only if `src/lib.rs` carries `#![no_std]`
  at the start of a line.
- `only-allowed-modules-use-std`. For a `std` member, every `src/**/*.rs` outside `allowed` is clean
  under the three patterns. An `allowed` name that does not exist, or one that no longer hits any
  pattern, is also a failure, so the list neither rots upward nor stays stale.
- `shells-are-the-composition-roots`. `purity = "shell"` appears only for `mokmok-kernel` and
  `mokmok-tui`.

```nix
checks.attesters =
  pkgs.runCommand "mokmok-attesters" { nativeBuildInputs = [ pkgs.findutils ]; } ''
    # `line-references.sh check` walks the docs it is handed, so the list comes from the source.
    docs=$(find ${src}/docs/spec -name '*.md' | sort)

    bash ${./docs/spec/references/attesters/dependency-rules.sh} check ${src} > $out
    bash ${./docs/spec/references/attesters/line-references.sh} check ${src} $docs | tee -a $out
    # This one takes the bundle directory, not the repository root.
    bash ${./docs/spec/references/attesters/okf-conformance.sh} check ${src}/docs/spec | tee -a $out
  '';
```

That is the sixth check beside `cargoClippy`, `cargoDocTest`, `cargoTest`, `mokmokdUnit` and
`nixosModule`. It closes the gap `mokmok-os-architecture.md:380-382` names, and it means the OKF
bundle's positional citations now fail CI when a cited file moves.

**The two new crates**, each judged against `mokmok-os-architecture.md:32`.

`mokmok-bus-core` owns the retention and delivery policy of one topic's log. Invariants: the newest
event survives its own eviction; only an ack frees credit; the floor never regresses. Contracts:
`evict` and `plan_pull`, both total over their inputs. It has a defect history, a duplicated
implementation, and it sits inside a package that builds SQLite and axum. This is the one extraction
that makes a *new* test possible; every other seam below becomes testable with a signature change in
place.

`mokmok-search-core` owns what a search result is: the tokenizer index time and query time share,
the snippet, the byte cap, the hit, the ranking and the rendering. Invariants: one tokenizer, so a
query reaches what the index wrote; no clip splits a character; the ranking is total and
deterministic. It has **two users today**. `mokmok-search` builds it, and
`mokmok-kernel/src/index.rs:70-95` ranks cross-root hits with the same rule, in code no test can
reach because `Index::find` opens a real index. It also owns the two functions that first scored
`mokmok-search` at 75%, the body-cap arithmetic and the snippet clip.

**What moves, and what each move makes possible.**

Into `mokmok-bus-core`:

- The eviction loop at `mokmok-event-bus/src/store.rs:470-487`, interleaved with `DELETE` and
  `UPDATE`, becomes `evict`.
  *New test.* Over generated `(first, next, bytes, retention, sizes)`, the returned floor either
  satisfies both bounds or leaves exactly the newest event.
- The credit and lapse arithmetic at `mokmok-event-bus/src/server.rs:555-630`, interleaved with
  `store().read(...).await`, becomes `plan_pull`.
  *New test.* The property quoted under Usage, which nothing can write today without a live bus.
- The `#[cfg(test)]` shadow at `mokmok-event-bus/src/store.rs:1144` is deleted. Its property test
  moves into `mokmok-bus-core` and now tests the production function instead of a re-implementation.

Into `mokmok-search-core`:

- `mokmok-search/src/token.rs` (`spans`, `tokens`), `query.rs` (`snippet`, `clipped`),
  `corpus.rs:347` (`capped`), `render.rs` (`render`), and the kernel's cross-root sort, tie-break
  and truncation at `mokmok-kernel/src/index.rs:70-95` become `spans`, `tokens`, `snippet`, `capped`,
  `rank`, `render`.
  *New test.* `tokens(text)` equals the token stream the tantivy adapter produces for the same text,
  over generated text; and `rank` is a total order a root's position in the grant list cannot
  change. Today the first needs an index and the second needs `Index::find` to run.

Into `mokmok-protocol`:

- `Setting.checkout`, `.readable`, `.writable`, `.program` and `.search` stop being
  `std::path::PathBuf` and become `mokmok_protocol::Path`, an owned path whose bytes are text.
  `PathBuf` cannot appear in a `no_std` surface, so this is the type that unblocks the runtime. The
  serde form is unchanged.
  *New test.* A path that is not UTF-8 is refused where the command line is parsed rather than when
  the task record is encoded.

Into pure functions in place, one per seam, each reachable from a hand-built input instead of a
database, a process, or a socket:

| crate | seam | signature | the test it makes possible |
| --- | --- | --- | --- |
| kernel | `recovery.rs:29` | `recovery_entries(state: &Session, log: &Log) -> (Vec<Entry>, Vec<ExecutionID>)` | a hand-built `Log` whose next entry is asserted literally |
| kernel | `slice.rs:396` | `transcript_line(record: &Record) -> String` | all nine arms from a `Record` literal |
| kernel | `agent.rs:118` | `notice_of(records: &[Record], session: SessionID, command: CommandId, at: Moment) -> Notice` | a notice from records, no journal |
| kernel | `agent.rs:795,872` | `step_line(&Step)`, `number_of(&Transcript, TurnID)` | the line and the number from a transcript |
| kernel | `slice.rs:206,542` | `mint(count: usize, command: CommandId, at: Moment)`, `timestamp(count: usize)` | identities from a count, no log |
| kernel | `slice.rs:223,258` | visibility only | the launch and settle entry lists asserted literally |
| kernel | `db.rs:392,455` | `kind_of(&Entry) -> Kind`, `is_in_flight(&Session) -> bool` | the four-way dispatch and the in-flight test |
| kernel | `provider.rs:419,428` | `outcome_of`/`detail_of(&AuditRecord)` | the egress-to-audit mapping, no file |
| kernel | `execution.rs:184-191` | `settlement(exit: &Exit, attempted: bool) -> (Resolution, Outcome)` | every outcome arm |
| kernel | `notify.rs:130,148` | `due(Cadence, now: Moment)`, `ping(now: Moment, deadline: Moment)` | deadlines as arithmetic, no tokio clock |
| kernel | `serve.rs:331` | `bus_ended(Result<(), Arc<str>>)` | the mapping, no `JoinError` |
| kernel | `config.rs:260,375,440`, `execution.rs:103`, `slice.rs:97`, `serve.rs:173` | decision split from enforcement | the decision alone, with the effect stubbed |
| runtime | `advance.rs:422-709` | one `*_records(...) -> Vec<Record>` per arm, with `journal.put` lifted to the caller | the record list of every arm, no journal |
| runtime | `context.rs:99,244,271,279,288` | `system_prompt(&Task)`, `context(&Transcript)`, `bytes_of`, `keep_newest`, `paths(&[Path])` | prompt bytes from a literal task |
| runtime | `record.rs:203`, `advance.rs:406,418` | same bodies, `uuid` built with `["serde", "v5"]` only | identity derivation with no rng |
| sandbox | `capability.rs:191` | `covers(capability, exposure, directory: bool)` | generated capability and exposure path pairs, no temp dir |
| event-store | `store.rs:378` | `preflight(recorded: Option<Committed>, found: Version, expected: Expected) -> Preflight` | the append precedence as a value |
| search | `query.rs:33` | `FieldID` in place of `tantivy::schema::Field` | the query builder without the schema |
| artifact | `content_hash.rs:29,76` | same bodies | hash uniqueness, and a parse that round-trips |
| observability | `redaction.rs:109`, `audit.rs:151` | same bodies | the protected value never appears in an export |
| egress | `destination.rs:86,192`, `proxy.rs:50,77,124`, `rate_limit.rs:39` | same bodies | an authority that parses re-parses; `allows` is monotone in the address list |
| tui | `core.rs`, `port.rs:168,217,241` | same bodies | the reducer's fold over a generated transcript |

The seams that **stay**, and why. `mokmok-observability`'s pure half and `mokmok-egress`'s pure half
would each become a crate under the same reasoning, but neither buys a test that is not already
possible with the fakes in the tree: `append_and_export` already runs against a proven `Exporter`
fake, and egress's tests already drive the clock through a `Moment` argument. Under the
**laziness-protocol** principle skill a pure function in place beats a new module and a new module
beats a new crate, so they stay and gain property tests. `mokmok-kernel` and `mokmok-tui` cannot be
`pure`: the kernel names twelve `mokmok-` crates and the TUI names the kernel. Both are `shell`, and
their decisions come out as free functions instead.

**Deliberately not done.** No crate is split merely because it both decides and does I/O. No
`mokmok-*-core` is created for observability, for sandbox or for artifact, and no shared
text-utility crate is created for the three char-boundary clips
(`mokmok-agent-runtime/src/record.rs:44`, `mokmok-search/src/query.rs:103`,
`mokmok-search/src/corpus.rs:347`). The kernel does not gain a session-policy crate, because its
decisions name `mokmok-event-store`, which is `std`, so the crate would not be `no_std` anyway.
`mokmok-scheduler` is not deleted even though its manifest line is: it is a member with 42 tests and
no caller, and removing a member is the operator's call, not this design's.

## Synthesis decision

filled in by arena

## Tradeoffs accepted

- We accept a `crates.toml` that duplicates the member list, in exchange for a check that catches
  drift in both directions. One file listing what the manifest lists is the only place a machine can
  read "this crate should have been pure and is not".
- We accept a grep as the purity check for the seven `std` crates, in exchange for reaching inside
  them at all. The compiler holds the line for the eight `pure` crates; a grep is the strongest
  mechanism available for the rest. An aliased import defeats it, and the `allowed` list for
  `mokmok-sandbox` is nearly the whole crate, which is itself the finding. What is pure in that
  crate is the gate, and everything around the gate is enforcement.
- We accept that the item list is not exhaustive about `std` spellings. `std::time::Duration` and
  `std::net::IpAddr` are `core` items under a `std` path and are not in it, so a module that writes
  them is not flagged even though a later flip would want the core spelling. Listing them would flag
  nearly every module and drain the check of meaning. The census is what settled that boundary, and
  the same rule names an `allowed` entry that has gone clean, so the list ratchets down.
- We accept that the tie-break for equal-score search hits becomes byte-wise. Today it compares
  `PathBuf`s built from the index's own stored strings, so it is component-wise, and the two differ
  when a `/` meets a `-`. The order is only observable when two hits tie on score and the limit
  truncates, and either order is deterministic.
- We accept one new type in `mokmok-protocol`. `Path` is a wire field of the durable task record, so
  the wire-contract crate owns it, not the runtime that most needs it.
- We accept `mokmok-egress` reading the wall clock in `audit.rs:58`, in exchange for the property
  that module documents, that a caller being audited cannot date its own record. That one read is
  why egress is `std` and not `pure`.
- We accept that scoring every package costs hours. The gate keeps one package per invocation, and
  `--all` stays a periodic job whose output is the committed ledger.
- We accept deleting `Validated::connect` in exchange for removing `tokio` from `mokmok-egress`
  entirely. Its only callers are its own tests, and the enforcement it demonstrates is the property
  `mokmok-kernel/src/provider.rs` exercises through reqwest's pinned addresses.
- We accept that adding `line-references.sh` to CI makes every line edit in a cited file a
  documentation edit. That tax is the point; the alternative is the drift measured in grounding 7.

## Alternatives considered

**One crate per mixed role.** Split each of the seven crates that both decide and touch the world
into a decision crate and an effect crate: `mokmok-bus-core`, `mokmok-event-store-core`,
`mokmok-egress-core`, `mokmok-sandbox-core`, `mokmok-artifact-core`, `mokmok-observability-core`,
`mokmok-search-core`. It is the most consistent rule, and the only shape under which every pure
module in the workspace is compiler-proven. It loses on interface depth. It doubles the member
count, and five of the seven new crates would have no caller outside their own tests.
`docs/spec/decisions.md:69-73` already names shipping a boundary before its second user as a cost
this project pays for `mokmok-scheduler`, `mokmok-artifact` and `mokmok-observability`. This shape
multiplies that debt instead of paying it.

**Flip the two crates that can be flipped and stop.** `mokmok-patch` and `mokmok-agent-runtime`
become `#![no_std]`, the bus's policy comes out, and the other nine stay `std` with no declaration
at all. It is the smallest diff and it answers the `no_std` half of the request literally. It loses
because it leaves the check exactly where it is today. Nine crates stay unmeasured, and a
sixteenth member added next month would still be silently `std`. The mechanism is the deliverable,
not the crates.

**One shared pure crate for all the pure logic.** A single `mokmok-core` holding the bus's policy,
search's text, the sandbox gate and egress's parsers. It maximises compiler-proven purity per crate
added. It loses on `mokmok-os-architecture.md:179`: unrelated state machines gathered into one crate
because they share a shape. The bus's policy, the tokenizer and the path gate share nothing but
their purity, and a reader looking for the credit ceiling would have no reason to open a crate
named after text.

## Open questions and risks

- Is `Validated::connect` worth keeping? Nothing outside `mokmok-egress/tests/enforcement.rs` calls
  it, and it carries the crate's only `tokio` edge. Deleting it is what makes `mokmok-egress` a
  one-world-module crate. But it is the only test of the crate's own acceptance criterion, "denied
  unauthorized connections", so deleting it trades a documented property for a purity level.
- Should `audit::now()` become a caller-supplied `Moment`, as `mokmok-capability`,
  `mokmok-scheduler` and `mokmok-egress/src/rate_limit.rs` already take it? That turns
  `mokmok-egress` into a sixth pure crate and contradicts a security decision its own module doc
  states. This design keeps the read and marks the crate `std`; the operator may prefer the opposite.
- Is `mokmok-scheduler` a member at all? `mokmok-kernel` declares it and never calls it, and the
  whole crate has no caller. This design deletes the manifest line and leaves the member. Removing a
  member is a larger call than a design should make alone.
- Is `mokmok-protocol`'s envelope, trace context and version policy worth its place?
  `docs/spec/decisions.md:75-79` records them as written and unused. This design keeps them, because
  a contract crate is where a contract waits, and deleting a `no_std` public surface is the one
  change a later consumer cannot absorb.
- Is two new crates too few? The test is whether each earns its manifest file. Both have a defect or
  a sub-target score behind them and a second user, and both make a test possible that is not
  possible in place. If the operator wants more compiler-proven crates after these two land, the
  naming convention admits `mokmok-*-core` without a redesign.
- The `unscored` budget in `mutation.toml` starts at ten and only ever falls. If a member is added
  before the ledger exists, the budget rises and the ratchet weakens for one PR.
- Risk: the strip-at-`#[cfg(test)]` rule assumes a file's test module is its last block. No file in
  the tree violates that today, and the rule's own doc says so, but a file that puts production code
  after its tests would hide the rest of itself.

## Next implementation step

Write `crates.toml` by running `dependency-rules.sh propose "$PWD"` and editing its output, then add
the four rules and `checks.attesters`, so every later phase lands against a check that can see it.
# Sketch: the target shapes

`not implemented` marks a body to write against this sketch. Everything else is the contract.

## 1. The target crate list

Seventeen members. `pure` is `#![no_std]` and compiler-checked; `std` is std with an enumerated
`allowed` list of modules; `shell` is a composition root. Bold marks a change from today.

| member | purity | allowed | role, one line |
| --- | --- | --- | --- |
| `mokmok-protocol` | pure | | Shared message contracts: identifiers, `Envelope`, `SchemaVersion`, **`Path`** |
| `mokmok-state-machine` | pure | | Validates a recorded session and turn history and folds it to state |
| `mokmok-scheduler` | pure | | Turns timing and wake conditions into work claims and fenced leases |
| `mokmok-capability` | pure | | Decides which principal may act on a resource |
| `mokmok-patch` | **pure** | | Reads a unified diff and applies it, with no repository behind it |
| `mokmok-agent-runtime` | **pure** | | One bounded step of a session, as a policy over four ports |
| **`mokmok-bus-core`** | **pure** | | The retention and delivery policy of one topic's log |
| **`mokmok-search-core`** | **pure** | | What a search result is, how it is ranked, how it is shown |
| `mokmok-event-bus` | std | `bus.rs`, `client.rs`, `config.rs`, `event.rs`, `server.rs`, `store.rs`, `wire.rs` | Serves one topic's durable log over a WebSocket |
| `mokmok-event-store` | std | `append.rs`, `error.rs`, `event.rs`, `store.rs`, `write.rs`, `bin/mokmok-event-store-crash.rs` | The one global commit log, in SQLite |
| `mokmok-egress` | std | `audit.rs` | Mediates network traffic and enforces destination decisions |
| `mokmok-sandbox` | std | `artifact.rs`, `capability.rs`, `config.rs`, `confine.rs`, `output.rs`, `plan.rs`, `request.rs`, `run.rs`, `sandbox.rs`, `workspace.rs`, `bin/mokmok-sandbox-confine.rs` | Supervises one execution under capabilities issued elsewhere |
| `mokmok-artifact` | std | `blob.rs`, `filesystem.rs`, `lib.rs` | Persists large results outside the event log |
| `mokmok-observability` | std | `error.rs`, `format.rs`, `init.rs`, `journal.rs`, `support.rs` | Exports traces, metrics, logs and durable audit records |
| `mokmok-search` | std | `corpus.rs`, `error.rs`, `walk.rs` | A BM25 index over a directory tree |
| `mokmok-kernel` | shell | | Assembles the services and coordinates lifecycle and recovery |
| `mokmok-tui` | shell | | A conversation with the agent, drawn on a terminal |

## 2. `crates.toml` and the rules

`taplo`-clean, one entry per member, and `rule_members-have-declared-roles` requires its member set
to equal the manifest's in both directions. A new member cannot land undeclared.

```toml
# The role division of the workspace. `dependency-rules.sh check` fails when this file and the
# manifest disagree, when a `pure` crate is not `#![no_std]`, when a `std` crate uses std outside
# `allowed`, or when a `shell` is neither of the two composition roots.

[members.mokmok-bus-core]
role = "The retention and delivery policy of one topic's log."
purity = "pure"

[members.mokmok-egress]
role = "Mediates network traffic and enforces destination decisions."
purity = "std"
# The one clock read left in the crate. `now` is read here and not taken from the caller so a
# caller being audited cannot date the record of its own decision.
allowed = ["audit.rs"]

[members.mokmok-kernel]
role = "Assembles the services and coordinates lifecycle and recovery."
purity = "shell"

[members.mokmok-tui]
role = "A conversation with the constrained agent, drawn on a terminal."
purity = "shell"
```

The four new rules go in `docs/spec/references/attesters/dependency-rules.sh` beside the six that
are already there, and share `run_rule`'s dispatch.

```sh
IMPURE_MODULES='std::(fs|io|path|process|env|os|ffi|thread)\b'
IMPURE_ITEMS='\b(SystemTime|Instant|TcpStream|TcpListener|UdpSocket|Mutex|RwLock|Condvar|OnceLock|Barrier|HashMap|HashSet|OsString)\b'
IMPURE_CRATES='\b(tokio|rusqlite|tantivy|landlock|nix|axum|reqwest|tracing|tracing_subscriber)::|Command::new'

library_source() { sed -e '/^#\[cfg(test)\]/,$d' -e 's,//.*$,,' "$1"; }

rule_members-have-declared-roles() { ... }       # the member set, both directions
rule_pure-crates-are-no_std() { ... }            # `pure` iff `#![no_std]`
rule_only-allowed-modules-use-std() { ... }      # `allowed` covers what hits, and only what hits
rule_shells-are-the-composition-roots() { ... }  # `mokmok-kernel`, `mokmok-tui`, and no third
```

`propose` is the lever that writes the file, so the `allowed` lists and the `pure`/`std` split come
from the tree and not from a reading of it. It is also how the three patterns above were chosen:
`propose` prints the modules that hit, and a census of `std::` paths says which patterns are real.

```sh
dependency-rules.sh propose <repo>    # one `[members.<name>]` block per member, with `allowed`
                                      # already listing the modules the patterns hit
dependency-rules.sh census <repo>     # `std::<module>` counts over library source, comment-stripped
```

`flake.nix` gains the sixth check:

```nix
attesters =
  pkgs.runCommand "mokmok-attesters" { nativeBuildInputs = [ pkgs.findutils ]; } ''
    # `line-references.sh check` walks the docs it is handed, so the list comes from the source and
    # not from a fixed glob: a new concept file has to be cited and checked like the rest.
    docs=$(find ${src}/docs/spec -name '*.md' | sort)

    bash ${./docs/spec/references/attesters/dependency-rules.sh} check ${src} > $out
    bash ${./docs/spec/references/attesters/line-references.sh} check ${src} $docs | tee -a $out
    bash ${./docs/spec/references/attesters/okf-conformance.sh} check ${src}/docs/spec | tee -a $out
  '';
```

## 3. `mokmok-bus-core` (new)

Born `pure`. Depends on `mokmok-protocol` for `EventID` and on `uuid` with `["serde"]`; no SQLite, no
tokio, no socket.

```rust
//! The retention and delivery policy of one topic's log, with no I/O.

#![no_std]

extern crate alloc;

use core::num::{NonZeroU64, NonZeroUsize};
use core::time::Duration;

/// One position in a topic's log.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Seq(u64);

impl Seq {
    /// The position `value` names.
    #[must_use]
    pub const fn new(value: u64) -> Self { not implemented }

    #[must_use]
    pub const fn get(self) -> u64 { not implemented }
}

/// The identity of one incarnation of a topic's log. A cursor from another epoch names nothing.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct LogEpoch(uuid::Uuid);

/// A subscriber's resume point, honored only while its epoch is the log's.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Cursor { epoch: LogEpoch, seq: Seq }

/// What a reader can be handed: the retained floor and the next position to write.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Window { epoch: LogEpoch, first: Seq, next: Seq }

impl Window {
    #[must_use]
    pub const fn new(epoch: LogEpoch, first: Seq, next: Seq) -> Self { not implemented }

    #[must_use]
    pub const fn epoch(self) -> LogEpoch { not implemented }

    #[must_use]
    pub const fn first(self) -> Seq { not implemented }

    #[must_use]
    pub const fn next(self) -> Seq { not implemented }
}

/// What a checkpoint behind the floor has to be told: where it is, and where the log starts.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Lapsed { held: Cursor, from: Seq }

impl Lapsed {
    #[must_use]
    pub const fn new(held: Cursor, from: Seq) -> Self { not implemented }

    /// How many events the reader can never be handed. Total rather than exact, because a frame
    /// from a peer could carry a lapse that never happened.
    #[must_use]
    pub const fn missed(self) -> u64 { not implemented }
}

/// How much of one topic's log the store keeps.
#[derive(Clone, Copy, Debug)]
pub struct Retention {
    pub events: NonZeroUsize,
    pub bytes: NonZeroU64,
    pub idle: Duration,
}

/// The ceilings one subscriber is served under.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub batch: NonZeroUsize,
    /// The most events one subscriber may hold un-ACKed.
    pub in_flight: NonZeroUsize,
}

/// How many of the oldest events one append evicts, and the window it leaves behind.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Evicted {
    /// The floor after the eviction, which the store writes back.
    pub first: Seq,
    /// The retained bytes after the eviction.
    pub bytes: u64,
    /// How many events from `first` forward to delete.
    pub delete: u64,
}

/// The retention policy's answer for appending `appended` bytes.
///
/// `sizes` is the size of each retained event, oldest first, so `sizes[0]` is the size of
/// `window.first()`. One append therefore reads its retained sizes once instead of querying the
/// oldest row per deletion. The newest event is never evicted: a log whose own newest event exceeds
/// the byte bound stays over the bound rather than empty. Returns `None` when the arithmetic does
/// not fit, which the store reports as a corrupt topic.
#[must_use]
pub fn evict(
    window: Window,
    bytes: u64,
    appended: u64,
    retention: Retention,
    sizes: &[u64],
) -> Option<Evicted> { not implemented }

/// What one pull should do.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PullPlan {
    /// Read `count` events from `from` and hand them over.
    Batch { from: Seq, count: NonZeroUsize },
    /// Answer the lapse alone, with no event and no credit spent.
    Lapsed { held: Cursor, from: Seq },
    /// No credit is free, so park until an ack frees some.
    Spent,
}

/// Decides one pull.
///
/// The credit ceiling is measured from the subscriber's own ack, never from the log's floor. A
/// floor that eviction raises would otherwise refresh credit for a subscriber that never acks, and
/// that subscriber would draw events forever.
#[must_use]
pub fn plan_pull(
    window: Window,
    delivered: Seq,
    acked: Seq,
    limits: Limits,
    max: NonZeroUsize,
) -> PullPlan { not implemented }
```

`mokmok-event-bus` keeps `Event`, `Published`, `Batch`, `Refusal`, `Payload`, `Topic`,
`SubscriberId`, `Config` and `Timeouts`, and re-exports the moved window types so no caller changes.
`config.rs` splits: `Limits` and `Retention` move, `Config` and `Timeouts` stay.

## 4. `mokmok-search-core` (new)

Born `pure`. Depends on `mokmok-protocol` for `Path`. It owns the hit model, so a hit's path is text
by construction, which `mokmok-search/src/walk.rs` already proves when it builds `Candidate.key`.

```rust
//! The tokenizer an index and a query share, and the hits a query answers with.

#![no_std]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use mokmok_protocol::Path;

/// One document the index answered with.
#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    /// The path as the index stores it, which the walk proved is text.
    pub path: Path,
    pub score: f32,
    /// The first line carrying one of the query's words, or nothing when no line does.
    pub snippet: String,
}

/// One query's answer, with the size of the tree it was asked of.
#[derive(Clone, Debug, PartialEq)]
pub struct Response {
    pub indexed: u64,
    pub hits: Vec<Hit>,
}

/// The words `text` is indexed and queried under, lowercased and in order.
///
/// A word is a run of alphanumeric characters, cut again where a lowercase or numeric character
/// meets an uppercase one, so a query for `agent runtime` reaches `agentRuntime.rs`.
#[must_use]
pub fn tokens(text: &str) -> Vec<String> { not implemented }

/// The byte spans of those words.
#[must_use]
pub fn spans(text: &str) -> Vec<(usize, usize)> { not implemented }

/// The first line of `body` carrying one of `words`, cut to a readable length.
#[must_use]
pub fn snippet(body: &str, words: &[String]) -> String { not implemented }

/// `text` cut to at most `limit` bytes, on a character boundary.
#[must_use]
pub fn capped(text: &str, limit: usize) -> String { not implemented }

/// The highest-scoring hits, at most `limit` of them, best first.
///
/// A score tie is settled by the path's bytes, so no root is privileged for coming first in the
/// grant list and two runs of one query answer the same.
#[must_use]
pub fn rank(hits: Vec<Hit>, limit: usize) -> Vec<Hit> { not implemented }

/// One response as the text the model reads.
#[must_use]
pub fn render(response: &Response) -> String { not implemented }
```

`mokmok-search` keeps `Corpus`, `Limits`, `Refresh`, `walk` and `error`, becomes the tantivy
adapter, and re-exports `Hit`, `Response` and `render`. Its `CodeTokenizer` keeps tantivy's
`Tokenizer` impl and delegates to `search_core::spans`. `render.rs` is deleted and `query.rs` shrinks
to the tantivy query builder, which folds into `corpus.rs`.

## 5. `mokmok-protocol` gains one type

```rust
/// A path as the durable log carries it: its bytes as text, never its platform form.
///
/// A path that is not UTF-8 has no value of this type, which is what the log already required:
/// `serde_json` cannot encode one. Refusing it where the command line is parsed names the failure
/// at the boundary instead of at the append.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, serde::Deserialize, serde::Serialize)]
#[serde(transparent)]
pub struct Path(String);

impl Path {
    #[must_use]
    pub fn new(text: impl Into<String>) -> Self { not implemented }

    #[must_use]
    pub fn as_str(&self) -> &str { not implemented }
}

impl core::fmt::Display for Path { ... }
```

## 6. The in-place seams

Signatures only. Each is the smallest change that lets a test name a hand-built input.

```rust
// mokmok-kernel/src/recovery.rs
pub(crate) fn recovery_entries(state: &Session, log: &Log) -> (Vec<Entry>, Vec<ExecutionID>) { not implemented }

// mokmok-kernel/src/slice.rs
pub(crate) fn transcript_line(record: &Record) -> String { not implemented }
pub(crate) fn mint(count: usize, command: CommandId, at: Moment) -> (TurnID, ExecutionID, TurnNumber) { not implemented }
pub(crate) fn timestamp(count: usize) -> Moment { not implemented }
pub(crate) fn launch(turn: TurnID, execution: ExecutionID, number: TurnNumber) -> Vec<Entry> { not implemented }
pub(crate) fn settled(execution: ExecutionID, resolution: Resolution, blob: Option<&Blob>) -> Vec<Entry> { not implemented }
pub(crate) fn verdict(state: &Session, turns: &[TurnRecord], command: CommandId) -> Submission { not implemented }

// mokmok-kernel/src/agent.rs
pub(crate) fn notice_of(records: &[Record], session: SessionID, command: CommandId, at: Moment) -> Notice { not implemented }
pub(crate) fn step_line(step: &Step) -> String { not implemented }
pub(crate) fn number_of(transcript: &Transcript, turn: TurnID) -> TurnNumber { not implemented }

// mokmok-kernel/src/db.rs
pub(crate) fn kind_of(entry: &Entry) -> Kind { not implemented }
pub(crate) fn is_in_flight(state: &Session) -> bool { not implemented }

// mokmok-kernel/src/provider.rs
pub(crate) fn outcome_of(record: &AuditRecord) -> Outcome { not implemented }
pub(crate) fn detail_of(record: &AuditRecord) -> String { not implemented }

// mokmok-kernel/src/execution.rs
pub(crate) fn settlement(exit: &Exit, attempted: bool) -> (Resolution, Outcome) { not implemented }

// mokmok-kernel/src/notify.rs
pub(crate) fn due(cadence: Cadence, now: Moment) -> Option<Moment> { not implemented }
pub(crate) fn ping(now: Moment, deadline: Moment) -> bool { not implemented }

// mokmok-kernel/src/serve.rs
pub(crate) fn bus_ended(outcome: Result<(), Arc<str>>) -> Step { not implemented }
pub(crate) fn readiness(relay: &Relay) -> Ready { not implemented }

// mokmok-agent-runtime/src/advance.rs
pub(crate) fn task_records(task: &Task) -> Vec<Record> { not implemented }
pub(crate) fn intent_records(transcript: &Transcript, turn: TurnID, attempt: Attempt, context: Blob) -> Vec<Record> { not implemented }
pub(crate) fn outcome_records(transcript: &Transcript, turn: TurnID, attempt: Attempt, report: &ToolReport) -> Vec<Record> { not implemented }
pub(crate) fn call_records(turn: TurnID, execution: ExecutionID, call: &ToolCall) -> Vec<Record> { not implemented }
pub(crate) fn settle_records(execution: ExecutionID, report: &ToolReport) -> Vec<Record> { not implemented }
pub(crate) fn close_records(turn: TurnID, session: bool) -> Vec<Record> { not implemented }

// mokmok-agent-runtime/src/task.rs  (a type change, not a function)
pub struct Setting {
    pub checkout: Path,
    pub readable: Vec<Path>,
    pub writable: Vec<Path>,
    pub program: Path,
    pub deadline: Duration,
    pub search: Vec<Path>,
}

// mokmok-sandbox/src/capability.rs
pub fn covers(capability: &Capability, exposure: &Exposure, directory: bool) -> bool { not implemented }

// mokmok-event-store/src/store.rs
pub enum Preflight { Commit, Replay(Committed), Conflict { found: Version } }
#[must_use]
pub fn preflight(recorded: Option<Committed>, found: Version, expected: Expected) -> Preflight { not implemented }
```

The decisions that split from their enforcement, per **boundary-discipline**:

```rust
// mokmok-kernel/src/config.rs
pub(crate) fn layout(config: &Config) -> Result<Layout, Error> { not implemented }
pub(crate) fn credential(meta: &std::fs::Metadata) -> Credential { not implemented }
pub(crate) fn crash_point(config: &Config) -> Option<CrashPoint> { not implemented }
// mokmok-kernel/src/execution.rs
pub(crate) fn may_invoke(plan: &Plan, capability: &Capability, exposure: &Exposure) -> bool { not implemented }
// mokmok-kernel/src/agent.rs
pub(crate) fn shell_command(request: &Request) -> Result<Command, Error> { not implemented }
```

## 7. Module map after the design

```
crates.toml                       the role division, read by the attester

mokmok-protocol/        pure      id, envelope, message, moment, path, version, error
mokmok-state-machine/   pure      fact, session, turn, verdict
mokmok-scheduler/       pure      clock, job, lease, scheduler, error
mokmok-capability/      pure      approvals, authority, decision, grants, name, permission,
                                   policy, principals, resource, revocations
mokmok-patch/           pure      lib (parse, recounted, to_unified, apply)
mokmok-bus-core/        pure      seq, window, retention, evict, pull
mokmok-search-core/     pure      token, snippet, cap, hit, rank, render
mokmok-agent-runtime/   pure      advance, context, dialogue, find, model, record, speech,
                                   task, tool, error
mokmok-event-bus/       std       bus, client, config, event, payload, server, store,
                                   subscriber, topic, wire
mokmok-event-store/     std       append, event, model, payload, store, write, error,
                                   bin/mokmok-event-store-crash
mokmok-egress/          std       validated, destination, proxy, rate_limit, policy_client,
                                   audit
mokmok-search/          std       corpus (holds the tantivy query builder), walk, error
mokmok-sandbox/         std       artifact, capability, config, confine, limits, output, plan,
                                   request, run, sandbox, workspace, bin/mokmok-sandbox-confine
mokmok-artifact/        std       blob, content_hash, filesystem, gc, metadata, retention
mokmok-observability/   std       audit, error, exporter, format, init, journal, metrics,
                                   redaction, support
mokmok-kernel/          shell     agent, authority, config, db, error, execution, index,
                                   notify, provider, recovery, serve, slice, bin/mokmokd
mokmok-tui/             shell     core, port, session, terminal
```

`mokmok-egress` loses `dns` to `mokmok-kernel/src/provider.rs`, which is its only implementer and
its only user, and loses `connect` with `Validated::connect`. What is left of `connect` is
`validated.rs`, and that file touches no I/O of its own.

## 8. Phases

One PR each, in this order, each ending in the named check. The order is topological: a crate can
only be `pure` once every type its public surface mentions is `pure`, and the docs repair precedes
the CI gate so the gate lands green. Phases 5 and 6 are independent, phase 8 is independent of
phase 7, phase 9 needs the `Path` type phase 7 adds, and phase 10 needs nothing but phase 1.

| phase | change | check that ends it |
| --- | --- | --- |
| **0. Subtract the tree junk** | `.gitignore` gains `/mokmok-data/`; the working tree's `mokmok-data/` and `mutants.out.old/` leave disk (`/mutants.out*` is already ignored) | `nix flake check` (five checks) and a clean `git status` |
| **1. Make the kernel scorable** | `scripts/mutants.sh` builds `-p mokmok-sandbox --bin mokmok-sandbox-confine`, then exports `MOKMOK_CONFINER="$PWD/target/debug/mokmok-sandbox-confine"` unless the package under mutation is `mokmok-sandbox` | `MUTANTS_PACKAGE=mokmok-kernel ./scripts/mutants.sh` reaches a baseline and prints a score |
| **2. Delete the stale edge and the duplicated helper** | `mokmok-kernel/Cargo.toml` drops `mokmok-scheduler`; the five integration helpers collapse into `mokmok-kernel/tests/common/mod.rs` | `nix flake check`; the kernel still builds and its tests still run, which is what proves the edge was dead |
| **3. Repair the drift** | `docs/spec/workspace.md` thirteen and fourteen become fifteen with a `mokmok-patch` row; `docs/spec/crates/patch.md` is written; `kernel.md` counts 93 to 118 and 24/15/8 to 30/20/14, plus the `index` row and the two unnamed integration files; `observability.md` gains `error`; `kernel.md:122-124`'s "only wall-clock read" goes; `strategy.md`'s ten proptest crates becomes fourteen; `log.md` gains a dated receipt | `line-references.sh check "$PWD" $(find docs/spec -name '*.md')` at `citations_unresolved=0`, and `okf-conformance.sh check docs/spec` |
| **4. Declare the roles and gate them** | `crates.toml` written from `dependency-rules.sh propose`; the four new rules; `checks.attesters` in `flake.nix`. Eleven members enter as `std` with their proposed `allowed` lists | `nix flake check` with the sixth check green, and `crates.toml` `git add`ed first so Nix can see it |
| **5. `mokmok-patch` is pure** | `#![no_std]`, `extern crate alloc`, `core::fmt::Write`, `core::str`, `thiserror` with `default-features = false` | `nix flake check`; `crates.toml` moves patch to `pure` and its `allowed` list is deleted, which is what the rule checks |
| **6. `mokmok-egress` shrinks to one allowed module** | delete `Validated::connect`, `connect::Error` and its three tests; move `Resolver` into the kernel and drop `mokmok-egress`'s `tokio`; swap `rate_limit.rs`'s `HashMap` for `alloc::collections::BTreeMap`, which `PrincipalID`'s `Ord` admits | `nix flake check`, then `dependency-rules.sh check` with egress's `allowed = ["audit.rs"]` |
| **7. `mokmok-agent-runtime` is pure** | `protocol::Path` replaces `PathBuf` in `Setting`; `core::error::Error`; `thiserror` default off; `uuid` `["serde", "v5"]`; `alloc::collections::BTreeMap`; the one `tracing` line gated; `serde_json` without `std`; the records become free functions | `nix flake check`; the new prompt-bytes and identity properties |
| **8. `mokmok-bus-core` is born** | the window types, `evict` and `plan_pull` move out; the `byte_bound_start` shadow is deleted; the store and the server execute the plan | `nix flake check`; `MUTANTS_PACKAGE=mokmok-bus-core ./scripts/mutants.sh` at 100% on a package that builds neither SQLite nor axum |
| **9. `mokmok-search-core` is born** | `tokens`, `spans`, `snippet`, `capped`, `Hit`, `Response`, `rank`, `render` move out; `render.rs` is deleted and `query.rs` folds into `corpus.rs`; the kernel's `Index::find` calls `rank` | `nix flake check`; `MUTANTS_PACKAGE=mokmok-search-core ./scripts/mutants.sh` |
| **10. The kernel's decisions become free functions** | the in-place seams of section 6, each with its property or unit test | `MUTANTS_PACKAGE=mokmok-kernel ./scripts/mutants.sh`, with the missed count below 24 |
| **11. The ledger** | `mutation.toml`; `mutants.sh --all` and `--record`; the ledger rule joins the attester check | `nix flake check`, then one `./scripts/mutants.sh --all --record` with every member scored |

Phase 3 must precede phase 4, because the gate runs the tree-reading attesters and would otherwise
land red. Phases 5 through 10 each move one entry out of a `std` crate's `allowed` list or delete
one, so every phase in the second half is a visible ratchet in the same file.

**Deleted before anything is added**, gathered from grounding 4, 7 and 9 plus what this session
measured:

1. `mokmok-data/` from the tree, with a `.gitignore` entry. It is the daemon's default `--data-dir`,
   untracked and unignored, and Nix cannot see an untracked file anyway.
2. `mutants.out.old/`. Already ignored, so this is disk only.
3. `mokmok-kernel/Cargo.toml`'s `mokmok-scheduler` line. Declared, never called.
4. `mokmok-event-bus/src/store.rs:1144`'s `byte_bound_start`, the test-only shadow of the eviction
   policy.
5. Four of the five copies of the confiner helper in `mokmok-kernel/tests/`.
6. `mokmok-egress/src/connect.rs`'s `Validated::connect` and `Error`, and `dns.rs`'s `Resolver`. The
   first has no caller but its own tests; the second has one implementer and one user, both in the
   kernel. This is the deletion that removes egress's `tokio` edge.
7. The false statements in `docs/spec`, listed in phase 3.

## 9. The mutation gate

`scripts/mutants.sh` keeps its one-package invocation and gains `--all` and `--record`.

```sh
readonly PACKAGE="${MUTANTS_PACKAGE:-mokmok-event-bus}"
readonly LEDGER="mutation.toml"

# The confiner is a bin of another package, so a package-scoped build never makes it and the
# kernel's helpers fail at the unmutated baseline. Point them at the pristine build instead. Never
# when the sandbox is the package under mutation: there the confiner is the code under test.
if [ "$PACKAGE" != mokmok-sandbox ]; then
  cargo build -p mokmok-sandbox --bin mokmok-sandbox-confine
  export MOKMOK_CONFINER="$PWD/target/debug/mokmok-sandbox-confine"
fi

# One run per member, each with its own outcomes, so the score stays a per-package number and the
# ledger stays comparable across runs. A row is written fresh, never appended to, because a
# second run of the same package would otherwise be read as two.
mutate() {
  cargo mutants --package "$1" --output "mutants.out/$1" || true
  jq -r --arg p "$1" '...' "mutants.out/$1/outcomes.json" > "mutants.out/$1/row.toml"
}

case ${1:-} in
  --all) while IFS= read -r member; do mutate "$member"; done < <(ledger_members); merge_rows "$LEDGER" ;;
  --record) mutate "$PACKAGE"; merge_rows "$LEDGER" ;;
esac
```

`mutation.toml` is committed, and it is the record:

```toml
# One row per workspace member. `floor` is the percent the member must not drop below; `score` is
# the last recorded run, and `unscored` says nobody has run it. `unscored_budget` may only fall.
unscored_budget = 10

[mokmok-event-bus]
score = 98.3
floor = 98
date = 2026-10-11

[mokmok-kernel]
score = 86.9
floor = 86
date = 2026-10-16

[mokmok-bus-core]
score = "unscored"
floor = 100
date = ""
```

A rule in `dependency-rules.sh`, `rule_the-mutation-ledger-covers-the-workspace`, asserts that the
row set equals the member set, that every recorded `score` is at or above its `floor`, and that the
count of `unscored` rows does not exceed `unscored_budget`. CI therefore checks coverage and each
floor, cheaply, on every PR; the `--all --record` run is what refreshes the numbers, and it is a
periodic job because scoring seventeen packages takes hours. Freshness, meaning a row older than the
last commit that touched its package, is checked by `mutants.sh --all` with git and not by `flake
check`, because `cleanSourceWith` strips `.git`.

Each new crate's `floor` starts at 100 and its row starts `unscored`, so phase 8 and phase 9 cannot
land without a green mutation run over the crate they add.

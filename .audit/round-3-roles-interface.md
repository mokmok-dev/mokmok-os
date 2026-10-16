# Round 3 interface: crate roles, purity, and the mutation base

Written 2026-10-16 in a worktree off `fbe5270`, on branch `feat/purity-ladder`. The operator's ask,
verbatim: cratesの役割分担を整理したいです。純粋なロジックはno_std crateに切り出すなどしてmutation
testingやproperty based testingで地盤を固めたいです.

This document is the design. The landing is one PR at a time, listed under "The phases".

## The problem, measured

The workspace has fifteen members and four of them declare `#![no_std]`. The rule that checks purity
selects its subjects by grepping for `#![no_std]` in the crate's own source, so it asks its question
only of crates that have already answered it and nothing counts the ones that have not. Measured:
the attester ran over four crates.

Nothing in CI ran that attester at all. `flake.nix` declared five checks and none of them read the
dependency rules, so `mokmok-os-architecture.md` section 9's request that CI enforce them was
unanswered.

`scripts/mutants.sh` scores one package per run and writes no record of which packages were scored.
Eight of the fifteen members have no recorded score. The kernel cannot be scored by the script as
written: its five integration helpers resolve the confiner as a sibling of `CARGO_BIN_EXE_mokmokd`
and assert it exists, and the confiner is a bin of `mokmok-sandbox`, which a package-scoped build
never produces. The helper reads `MOKMOK_CONFINER` first and nothing ever set it.

Measured with the confiner named, on this tree: **294 mutants tested in 4m, 171 caught, 26 missed, 2
timeouts, 85.9%**, exit 3. An earlier run of the same crate on 2026-10-16, the one this round
inherited through memory, split the same 199 mutants as 173 caught and 24 missed, 86.9%. Two mutants
between the two runs are a load-sensitive difference rather than a code difference, and it is worth
knowing before treating any kernel score as a fixed number. The 26 missed mutants of this run are the
extraction list, because every one of them sits where the grounding says pure logic is locked behind
I/O:

```
MISSED   src/agent.rs:847:5: replace ask_line -> String with String::new()
MISSED   src/agent.rs:877:5: replace number_of -> u64 with 0
MISSED   src/db.rs:213:9: replace Log::verify -> Result<(), Error> with Ok(())
MISSED   src/execution.rs:185:13: delete match arm Outcome::Exited(code) in store
MISSED   src/execution.rs:189:13: delete match arm Outcome::Signalled(signal) in store
MISSED   src/provider.rs:429:5: replace detail_of -> String with String::new()
MISSED   src/provider.rs:375:9: replace <impl PolicyClient for Permit>::check -> Result<(), Error> with Ok(())
MISSED   src/provider.rs:403:9: replace <impl AuditSink for AuditToLog>::record with ()
MISSED   src/serve.rs:127:9: replace Relay::forget with ()
MISSED   src/serve.rs:305:35: replace + with - in wait_for_bus
MISSED   src/slice.rs:177:13: delete match arm Phase::Draining in resume
MISSED   src/slice.rs:178:13: delete match arm Phase::Closed(_) in resume
TIMEOUT  src/db.rs:241:9: replace Log::absorb with ()
TIMEOUT  src/serve.rs:220:60: replace + with - in serve_until_stopped
```

The one defect that shipped out of this tree was pure arithmetic. The bus's delivery credit ceiling
was first measured from the retained floor instead of the stored checkpoint, so eviction refreshed a
subscriber's credit and a subscriber that never acks kept drawing events. It passed the suite and was
falsified by an independent verifier driving a churning publisher. The arithmetic sits between
`store().read(...).await` calls, and the same policy is re-implemented in a `#[cfg(test)]` shadow at
`mokmok-event-bus/src/store.rs:1144`.

## The shape

Three declared roles, one new crate, and a check that runs in CI.

Each member declares its own role in its own manifest, so a crate added next month fails rather than
being silently treated as std.

```toml
[package.metadata.mokmok]
purity = "pure"
```

- `pure` means the crate is `#![no_std]`, so rustc rejects `std` and no convention has to hold the
  line. `unsafe_code = "deny"` closes the `extern "C"` route, so the crate's own logic cannot reach
  an operating system interface. That is narrower than it sounds and the verification page says so:
  rustc constrains only the crate's own code, the closure rule holds the `mokmok-` edges, and the
  known IO crates are a fixed list.
- `adapter` means the crate binds the world. Its role is to be thin, and the pure logic inside it is
  a decision lifted out of the IO that carries it.
- `shell` means a composition root. Only `mokmok-kernel` and `mokmok-tui` may hold it, and the check
  holds that to the same list `kernel-dependents-are-applications` already reads.

End state, sixteen members:

| member | role | why |
| --- | --- | --- |
| `mokmok-protocol` | pure | gains `Path`, the owned path type the runtime's `Setting` needs |
| `mokmok-state-machine` | pure | unchanged |
| `mokmok-scheduler` | pure | unchanged |
| `mokmok-capability` | pure | unchanged |
| `mokmok-patch` | pure | **new.** Three core items at `src/lib.rs:3-5`; one attribute and one feature change |
| `mokmok-delivery` | pure | **new crate.** The retained window and the delivery credit |
| `mokmok-agent-runtime` | pure | **new.** Its own doc at `src/lib.rs:3` already claims it |
| `mokmok-event-store` | adapter | rusqlite owns the transaction |
| `mokmok-event-bus` | adapter | rusqlite, tokio, axum |
| `mokmok-search` | adapter | tantivy, and the filesystem walk |
| `mokmok-sandbox` | adapter | landlock, nix, real processes |
| `mokmok-egress` | adapter | the socket and one deliberate clock read |
| `mokmok-observability` | adapter | the tracing subscriber and the fs journal |
| `mokmok-artifact` | adapter | the `std::fs` blob store |
| `mokmok-kernel` | shell | the composition root |
| `mokmok-tui` | shell | an application above the kernel |

### The check

`docs/spec/references/attesters/dependency-rules.sh` owns the architecture's dependency rules. It had
six; one is retired and four are added, so it has nine. `flake.nix` gains one check,
`dependencyRules`, which runs it.

| Rule | What it forbids |
| --- | --- |
| `every-member-declares-its-role` | A member with no declared role, an unknown role, or `shell` outside the composition roots |
| `pure-crates-are-no_std` | A declared role and a source that disagree about `#![no_std]`, or a `#![no_std]` crate with an IO dependency |
| `pure-crates-depend-only-on-pure-crates` | A `pure` crate depending on one that is not, which is the topological constraint as a machine fact |
| `the-role-census-is-not-empty` | A tree where no member declares `pure` or none declares `adapter`, which is what a broken selector looks like |

The third rule is what makes "which crate can be pure next" a question the check answers. The fourth
is the non-vacuity floor: a rule that selects nothing must fail rather than pass.

The check prints `pure=<n> adapter=<n> shell=<n>` as its detail, so the role mix is in CI's log.

Sensitivity is a mode, not a sentence. `dependency-rules.sh faults` lays down an eight-member
repository, asserts all nine rules pass over it, then applies one edit per fault and asserts the
intended rule fails with a detail naming the offender. Today: nine controls pass and seventeen faults
trip the intended rule.

Every rule reads the dependency graph through `cargo metadata --no-deps --offline`. It did read
`Cargo.toml` as text until an independent verifier falsified the enforcement claim on the first
version: a crate declaring `pure` could depend on tokio under
`tokio_rt = { package = "tokio", version = "1" }` and the check stayed green, and the same held for
a `[target.'cfg(unix)'.dependencies]` table and for the dotted `[dependencies.mokmok-kernel]` form.
Three faults in the mode now hold that fix.

### The new crate

`mokmok-delivery` owns the retained window and the delivery credit. It earns the split against
`mokmok-os-architecture.md:32` on all four counts. It owns state, the window floor and each
subscriber's credit. It owns invariants: the newest event is never evicted, the ceiling is measured
from the stored checkpoint and never from the retained floor, and a lapse travels alone with no
event and no credit. It owns a contract of two decisions and the types they decide over. Its
validation requirement is a property test over generated window sizes and one over generated credit
triples, which the current surface cannot admit. It is one state machine, so `:179` holds.

```
mokmok-delivery/
  src/window.rs      Seq, LogEpoch, Cursor, Window, Lapsed   (moved from mokmok-event-bus/src/event.rs)
  src/retention.rs   Bounds, RetentionStep, retention_step
  src/credit.rs      Credit, PullRequest, PullPlan, plan_pull
```

`Seq` and `Cursor` stay distinct types, so the shipped defect's confusion between the retained floor
and the stored checkpoint is a type error rather than a comment. `PullPlan` makes the three outcomes
of a pull the only representable ones, so a batch can carry a lapse and a lapse cannot carry events.
`mokmok-event-bus/src/lib.rs` re-exports the five moved types, so the bus's public surface is
unchanged.

### What moves, and the test each move buys

Everything else is a pure function in the crate that already owns the decision. A pure function in
place beats a new module, and a new module beats a new crate.

| seam | today | after | the test it makes possible |
| --- | --- | --- | --- |
| eviction policy, `store.rs:432-487` | arithmetic interleaved with `DELETE` and `UPDATE` | `retention_step` | a property folding the step over `(window, bytes, bounds)` and a generated size list |
| delivery credit, `server.rs:555-621` | arithmetic interleaved with `store().read().await` | `plan_pull` | the property that would have caught the shipped defect, over generated `(delivered, acked, window)` |
| recovery fold, `recovery.rs:29-69` | interleaved with `log.state`, `log.verify`, `db.append` | `recovery_entries` | the exact entry vector for a hand-built `Log` and `Session` |
| step notice, `agent.rs:118` | only through `Journal::append` writing SQLite | `notice_of` | a literal `Record` list folded into a `Notice` |
| transcript line, `slice.rs:396` | only through `status()`, which opens the DB | `pub(crate)` | a nine-arm table over hand-built `Record`s |
| turn number, `agent.rs:872` | folds `ledger.transcript()` to read one number | `number_of(&Transcript, ..)` | numbering a hand-built transcript |
| mint, `slice.rs:206` | reads `log.turns_in_order()` | `mint(command, at, turns)` | ids from a literal turn count |
| result mapping, `execution.rs:184-191` | interleaved with `BlobStore` and `std::fs::read` | `result_of` | the outcomes by started state, no store |
| bus end, `serve.rs:331` | blocked by `tokio::task::JoinError` | `bus_ended(Ended)` | a three-arm table with no task spawned |
| crash and credential decisions | decide then `process::exit(97)` / `std::fs::metadata` | `crash_now`, `credential_ok` | the crash table and the mode table, no environment write |
| notify arithmetic, `notify.rs:148` | blocked by `tokio::time::Instant` | `due(next, every, now)` | due-ness over a table of offsets |
| cross-root ranking, `index.rs:70-95` | a comparator duplicated in the kernel | `mokmok_search::rank` | one comparator instead of two, over generated hits |
| append precedence, `store.rs:378-395` | three reads interleaved with one commit | `preflight` | a precedence table where a replay beats a version conflict |
| coverage decision, `capability.rs:191` | `capability.path.is_dir()` inside the decision | `covers(.., path_is_directory)` | component-wise coverage over generated pairs, no temp file |
| step records, `advance.rs:422-709` | one builder needs a `Journal` to be observed | `*_records` plus `pub(crate)` | the exact `Vec<Record>` per step state |
| prompt bytes, `context.rs:244-288` | blocked by `PathBuf` and a display call | over `mokmok_protocol::Path` | the budget never splits a character |
| search clauses, `query.rs:33` | built inside a tantivy `Query` | `clauses` | the clause shape for generated words, no index |
| snippet and cap, `query.rs:89`, `corpus.rs:347` | private, covered through a real index | `pub` plus properties | the clip and the cap on their own |

`mokmok-tui` moves nothing. Its reducer is already behind the `Screen` and `Engine` ports; the three
remaining survivors (`Terminal::leave`, its `Drop`, `Terminal::draw`) need a pty harness, which is a
test rather than an extraction.

## The phases

One PR each, in this order. The order is topological: a crate can only be `#![no_std]` once every
type its public surface mentions already is.

| phase | what lands | ends in |
| --- | --- | --- |
| P0 | The declaration in fifteen manifests, the four rules and the retirement of the opt-in selector that deletion 9 names, the `dependencyRules` check, the `faults` mode. No crate moves. | `dependencyRules` green logging `pure=4 adapter=9 shell=2`, `faults` green over nine controls and seventeen faults. **Landed.** |
| P1 | Subtraction: the stale `mokmok-scheduler` edge, the five confiner helpers collapsed to one, the half of `capability-performs-no-enforcement-io` the compiler already covers, the `docs/spec` drift, `mokmok-data/` ignored. | `cargo metadata` current, `specBundle` added and green. |
| P2 | `mokmok-patch` becomes `#![no_std]`. The cheapest purity win, done by hand to learn the recipe. | `dependencyRules` logging `pure=5`, `MUTANTS_PACKAGE=mokmok-patch` at 100%. |
| P3 | `mokmok-delivery` lands with the window types, `retention_step`, the eviction property, and the store's loop. The shadow is deleted. | `dependencyRules` logging `pure=6`, `cargo test -p mokmok-event-bus` green. |
| P4 | `plan_pull`, `PullPlan`, the credit property. `pull` executes the plan. | `MUTANTS_PACKAGE=mokmok-delivery` at 95% or above. |
| P5 | The gate reaches every member. `scripts/mutants.sh --all` builds the confiner, exports `MOKMOK_CONFINER`, sweeps all sixteen, and writes `mutation.toml`. `--check-baseline` reads it. | sixteen rows, the check logging how many sit below 95. |
| P6 | The kernel's in-place seams plus the integration assertions that kill the survivors. | `MUTANTS_PACKAGE=mokmok-kernel` at 95%, target raised in the same PR. |
| P7 | `mokmok-protocol` gains `Path` and `mokmok-agent-runtime` becomes `#![no_std]`, with that type replacing five `PathBuf` fields. Both edits are in this PR because protocol has to export the type before the runtime can name it. | `dependencyRules` logging `pure=7`, the runtime's row unchanged at 99.2%. |
| P8 | The in-place seams, one PR per crate. The tui pty harness is its own PR. | each crate's row refreshed and its target raised. |
| P9 | The bundle describes the new roles. | `specBundle` green. |

## The lever

`scripts/mutants.sh` gains three modes over one implementation:

```sh
./scripts/mutants.sh                     # one package, as today, with the confiner exported
./scripts/mutants.sh --all               # every member in topological order, writes mutation.toml
./scripts/mutants.sh --check-baseline    # fails on a missing row, a moved digest, or a score below target
```

The confiner export is `cargo build -p mokmok-sandbox --bin mokmok-sandbox-confine` followed by
`export MOKMOK_CONFINER="$PWD/target/debug/mokmok-sandbox-confine"`. The path is absolute and
outside the tree cargo-mutants mutates, so it survives the harness's per-job copy, and no test
assertion is weakened. The tui's `PATH` fallback stays where it is: that file dispatches no tool, so
its confiner only has to be runnable, while the kernel's tests dispatch a program and would quietly
run it unconfined behind a shell.

`mutation.toml` records a score, a target, and a digest per member, plus one digest over the harness
files that change what a score means. A target only rises. The check's output is the row count and
the members below 95, so the distance to the target state is a number rather than a claim.

## Deletions before additions

1. `mokmok-kernel/Cargo.toml`'s `mokmok-scheduler` line. Nothing in the kernel names a symbol from it.
2. The five copies of the confiner helper in `mokmok-kernel/tests/`, collapsed to one. The hard
   assert stays; the fix is a real confiner through the env route, not the removal of a guard.
3. The source grep in `capability-performs-no-enforcement-io`. The crate is already `#![no_std]`, so
   rustc rejects `std::process`, `std::fs`, and `std::net` before the grep can look.
4. `mokmok-event-bus/src/store.rs:1144`'s `byte_bound_start`, once `retention_step` is the policy.
5. `mokmok-kernel/src/index.rs`'s comparator, once `mokmok_search::rank` exists.
6. `mokmok-agent-runtime/src/advance.rs:132`'s `tracing::debug!`, and `advance`'s `at` parameter if
   nothing else reads it. The parameter exists for one log line and no decision.
7. The `docs/spec` drift: member counts in `index.md:8` and `workspace.md:21`, the absent `patch.md`,
   the case counts and module list in `kernel.md`, the missing `error` module in `observability.md`,
   the "only wall-clock read" claim, `strategy.md:45`'s proptest count, the stale receipts.
8. `mokmok-data/` from the tree's own view, by one `.gitignore` line. It is the daemon's default
   `--data-dir`, a runtime artifact, not a crate.
9. The opt-in selector itself. `no_std_crates()` is gone, and the declared role replaced it.

## What this design does not do

- No crate per seam. `mokmok-delivery` is the only new crate, and it earns its place by owning state.
- No `mokmok-kernel-core` and no `mokmok-recovery`. Such a crate would expose `Log`, `Entry`, `Fact`,
  and `Notice` as a second public vocabulary for a session's history that `mokmok-state-machine`
  already owns, and it gathers recovery, ranking, and transcript rendering under one roof.
- No purity for `mokmok-search`, `mokmok-egress`, `mokmok-observability`, or `mokmok-artifact`. Their
  pure halves stay where they are, `pub` and tested, because splitting them gives `String` and `usize`
  transforms a public vocabulary.
- No `extern crate std;` per module. It is compiler-enforced purity at module granularity, but it
  moves no boundary and costs an `alloc` import on every file in nine crates.
- No lowering of the 95% floor. A member's recorded target rises and never falls.
- No wire version bump. `Setting`'s path fields change type and keep their representation, because
  `PathBuf` already serializes as its string on Unix.
- No mutation sweep inside `nix flake check`. Sixteen packages at `--jobs 8` is tens of minutes; the
  check is tree-only and runs in seconds, and the sweep stays an operator command.
- No fix for the load-sensitive `mokmok-sandbox` supervision test. It belongs in that crate's PR.
- No removal of `mokmok-scheduler` from the workspace, even though its manifest edge goes. It is a
  member with 42 tests, and removing a member is the operator's call.

## Synthesis record

An arena of three candidates ran over this design, plus a cross-judge on the same rubric.

**Base: candidate 3.** It is the only candidate whose every factual claim survived the tree, and the
only one whose phase list is fully topological without an unstated prerequisite. It scored highest on
crate-count discipline, order, subtraction, and grounding accuracy while conceding the purity check.

**Rejected: candidate 2's central claim.** It reported `rule_pure-crates-have-no-io-dependency` as
vacuous, on the grounds that the selector greps `^#\[no_std\]` while the sources write `#![no_std]`.
The pattern includes the `!` and matches all four crates. Both the parent and the cross-judge
reproduced this independently. Its second deliverable rested on the same premise, so its R1 score
does not stand either, though its non-vacuity floor does as an idea and is grafted.

The blame is shared and the record should say so: the grounding this round handed every candidate
quotes the selector as `grep -q '^#\[no_std\]'`, dropping the `!`. Candidate 2 copied that pattern
from its brief. It is still wrong to build a design on a quoted pattern without running it, which is
why the claim is rejected, but the brief was the source and the brief has been corrected in
`.audit/round-3-arena/grounding.md`.

**Grafted from candidate 1:**
- The `shell` role and the composition-root allowlist, so a third root cannot appear silently. Its
  rationale cites `dependency-rules.sh propose`, a mode that does not exist; the lists themselves
  reproduce, so only the attribution was wrong.
- `mokmok_protocol::Path` rather than a runtime-local one, because the durable task record carries
  the value and the wire-contract crate owns it.

**Grafted from candidate 2:** the non-vacuity floor (the census rule), and `--test-workspace true`
recorded as the fallback if the confiner export regresses.

**Also recorded:** candidates 1 and 2 both measured that `mokmok-egress`'s `tokio` edge exists only
for `Validated::connect`, whose only callers are that crate's own tests, and that the `Resolver` port
has one implementer and one user in the kernel. Both are real. The deletion is not in this design:
`Validated::connect` is the only test of the crate's own acceptance criterion, "denied unauthorized
connections", so removing it trades a documented property for a role. It is an open decision below.

## Open decisions

- Is `audit::now()` in `mokmok-egress` worth keeping as a boundary read, or should the audit record
  take a `Moment` like `rate_limit.rs` already does? The second makes egress a sixth pure crate and
  contradicts a security property its own module documents, that a caller being audited cannot date
  its own record.
- Should `Validated::connect` and the egress `tokio` edge go? See above.
- Does `#![no_std]` plus `alloc` actually compile for `mokmok-agent-runtime`, with its
  `impl Future + Send` ports and its `Box<dyn Error + Send + Sync>` source alias? Judgment says yes,
  since `core::future` and `alloc` cover both. The P7 PR decides.
- Should the kernel's folds get their own crate once P6 has measured what is left? Candidate 2's case
  was the strongest of the three, and the design defers it to evidence rather than to a guess.
- Is a per-member digest in `mutation.toml` too strict, given a comment edit inside a scored crate
  forces a re-sweep? The alternative narrows the digest to `src/` and `tests/` and accepts the gaps.
- Should the tui's three terminal-only survivors be killed by a pty harness, or should its floor be
  set deliberately below 95? A silent lowering is the outcome to avoid.
- Adding `specBundle` to CI means an unrelated PR can go red on a citation it did not touch, because
  the rule is positional. The repair is mechanical. The first red will say whether it earns its cost.
- Is `mokmok-delivery` the right name? `mokmok-flow` and `mokmok-window` are the alternatives.

## Next step

P1, as one PR: the nine deletions, with the `specBundle` check landing in the same PR so the citation
debt is paid while it is small.

# Grounding: mokmok-os crate role division and purity

Measured 2026-10-16 by reading the tree at `/home/pranc1ngpegasus/ghq/github.com/mokmok-dev/mokmok-os`
(trunk `fbe5270`). Everything below is either measured or labelled as inferred. Line references are
`path:line`.

## 1. The request

The operator wants the crate role division (役割分担) organized, pure logic extracted into `no_std`
crates, and the base firmed up with mutation testing and property-based testing.

## 2. The manifest today

`Cargo.toml:2-18` lists fifteen members:

`mokmok-agent-runtime`, `mokmok-artifact`, `mokmok-capability`, `mokmok-egress`, `mokmok-event-bus`,
`mokmok-event-store`, `mokmok-kernel`, `mokmok-observability`, `mokmok-patch`, `mokmok-protocol`,
`mokmok-sandbox`, `mokmok-scheduler`, `mokmok-search`, `mokmok-state-machine`, `mokmok-tui`.

`mokmok-data/` sits in the working tree with two empty subdirectories (`artifacts/`, `sandbox/`), is
not a workspace member, is untracked, and is not in `.gitignore`. It is the daemon's default
`--data-dir` (`flake.nix:268-270`). It is a runtime artifact, not a crate.

## 3. Purity today, and the rule that checks it

Four crates are `#![no_std]`, each with the attribute below the module docs:

| crate | attribute |
| --- | --- |
| `mokmok-protocol` | `src/lib.rs:15` |
| `mokmok-state-machine` | `src/lib.rs:113` |
| `mokmok-scheduler` | `src/lib.rs:44` |
| `mokmok-capability` | `src/lib.rs:68` |

Eleven are std.

`docs/spec/references/attesters/dependency-rules.sh` holds six rules. Run today,
`check "$PWD"` prints six `status=pass`, exit 0. The purity rule is
`rule_pure-crates-have-no-io-dependency`, and its selector is:

```
no_std_crates() { ... grep -q '^#\[no_std\]' "$repo/$dir/src/lib.rs" ... }
```

So the rule fires **only for a crate that is already `#![no_std]`**. A crate that never opts in is
never asked whether it should have. Purity is opt-in per crate with nothing measuring the opt-in
rate.

**The attester does not run in CI.** `flake.nix:68` declares `checks = { cargoClippy, cargoDocTest,
cargoTest, mokmokdUnit, nixosModule }`; grep for `attester|docs/spec|okf|conformance` over
`flake.nix` returns nothing. `.github/workflows/nix.yaml` runs only `nix flake check
--print-build-logs`. The one tree-reading check of the architecture's own dependency rules is a
script an operator runs by hand.

## 4. The mutation gate, and what it cannot reach

`scripts/mutants.sh` is the gate. Defaults measured in the script:
`MUTANTS_PACKAGE` default `mokmok-event-bus`, `MUTANTS_TARGET` default `95`. It exits non-zero when
cargo-mutants exits with anything but 0 or 2, when `outcomes.json` is missing, when zero mutants were
tested, or when `caught*100 < target*tested`.

Recorded scores, and their dates:

| Package | caught | missed | timeout | score | recorded |
| --- | --- | --- | --- | --- | --- |
| `mokmok-search` | 76 | 0 | 0 | 100.0% | 2026-10-16 |
| `mokmok-agent-runtime` | 139 | 1 | 0 | 99.2% | 2026-10-16 |
| `mokmok-event-bus` | 183 | 3 | 0 | 98.3% | 2026-10-11 |
| `mokmok-event-store` | 87 | 0 | 0 | 100% | 2026-10-12 |
| `mokmok-patch` | 80 | 0 | 0 | 100% | 2026-10-15 |
| `mokmok-kernel` | 173 | 24 | 2 | 86.9% | 2026-10-16 |
| `mokmok-tui` | 65 | 3 | 0 | 95.5% | 2026-10-16 |

Eight members have no recorded run at all: `mokmok-protocol`, `mokmok-state-machine`,
`mokmok-scheduler`, `mokmok-capability`, `mokmok-artifact`, `mokmok-egress`, `mokmok-observability`,
`mokmok-sandbox`.

`docs/spec/decisions.md:81-85` already names this gap: the gate is one package per run and nothing
records which packages were scored.

### The kernel cannot be scored by the gate as written

`cargo mutants -p mokmok-kernel` fails at the unmutated baseline. Its five integration files resolve
the confiner as the sibling of `CARGO_BIN_EXE_mokmokd` and assert it exists with no fallback
(`mokmok-kernel/tests/agent.rs:99-107`, and the same helper in `environment.rs:70`,
`recovery_order.rs:66`, `slice.rs:105`, `supervisor.rs:32`). The confiner is a bin of
`mokmok-sandbox`, so a package-scoped build never produces it. The helper reads
`MOKMOK_CONFINER` first, but nothing in `.cargo/mutants.toml`, `scripts/`, or `flake.nix` sets it.

`mokmok-tui` had the same shape and was fixed: `mokmok-tui/tests/conversation.rs:112` falls back to
the first shell on `PATH` when the sibling is absent. Verify the asymmetry by reading both helpers.

### What the kernel's survivors are

The 24 missed mutants sit in pre-existing surfaces, all reachable only through I/O: the egress policy
client and the audit sink, `Relay::forget`, `Serve`'s `wait_for_bus`, the slice's `resume` arms,
`Log::verify`, `Execution::store`'s outcome arms, and `Shell::read`.

## 5. Pure logic trapped behind an I/O surface

Ranked by the test-sensitivity a free function would gain. Each entry names the seam, its blocker,
and the smallest change that frees it.

### 5.1 `mokmok-event-bus` — the highest-value seam, because a real defect shipped here

- Production eviction policy: `src/store.rs:470-487` inside `append` (`src/store.rs:432`). Pure
  arithmetic over `(first, next, bytes, retention.events, retention.bytes)` interleaved with `DELETE`
  and `UPDATE`.
- The same policy already exists twice. A test-only shadow lives at `src/store.rs:1144`
  (`byte_bound_start`, under `#[cfg(test)]`). Duplicated policy is the symptom of logic locked behind
  SQLite.
- The delivery credit bound and lapse arithmetic: `src/server.rs:555` (`pull`), arithmetic at
  `:566-575` and `:617-621`. Pure over `(delivered, acked, window, limits, max)`, interleaved with
  `store().read(...).await`.
- Why this ranks first: the credit ceiling was first anchored at the retained floor instead of the
  stored checkpoint. Measuring from the floor lets eviction refresh the credit, so a subscriber that
  never acks keeps drawing events forever. It shipped and was falsified by an independent verifier
  driving a churning publisher. A pure function plus a property test over generated
  `(delivered, acked, window)` triples is the instrument that would have caught it before the merge.
- Its two remaining survivors (`server.rs:413` `window_changed`, `store.rs:616`) both need a
  concurrency test today.
- Blocker: `rusqlite::Connection` inside `append`/`read`; tokio and `tracing` in `server.rs`.
- Smallest change: one value type plus one pure function. `struct Eviction { first: Seq, next: Seq,
  bytes: u64 }` with a pure `evict(...)`; `enum PullPlan { Batch { from: Seq, count: NonZeroUsize,
  lapsed: Option<Lapsed> }, Park(Parked) }` computed by a pure `plan_pull(...)` that `pull` executes.

### 5.2 `mokmok-kernel` — the largest, the lowest score, and unscorable by default

Kernel `src` is 7199 lines; 10231 with tests. Seams, in order of gain:

- `src/recovery.rs:29` (`recover`, `:112 recover_unsettled`). Computes which executions are
  `State::InFlight` and the exact recovery entries. The pure fold at `:40-69` is interleaved with
  `log.state`, `log.verify`, `db.append`. Fix: `fn recovery_entries(state: &Session, log: &Log) ->
  (Vec<Entry>, Vec<ExecutionID>)`, testable against a hand-built `Log` (`src/db.rs` `Log` fields are
  `pub`).
- `src/slice.rs:396` (`transcript_line`), a 9-arm match recording a `Record` as one `status` line.
  Reachable only through `status()` (`:351`), which opens the DB. Fix: `pub(crate)` plus hand-built
  `Record`s; no type change.
- `src/agent.rs:118` (`Ledger::notice`), folds a step's records into the outbox `Notice`.
  Reachable only by `Journal::append` writing to SQLite. Fix: a free `notice_of(records, session,
  command, at)`; the body already reads only `records` and `self.at`.
- `src/agent.rs:795` (`step_line`) and `:872` (`number_of`). `number_of` folds
  `ledger.transcript()` just to read a number. Fix: take `&Transcript`.
- `src/slice.rs:206` (`mint`) and `:542` (`timestamp`). Derives `TurnID`, `ExecutionID`,
  `TurnNumber`. Fix: take `turn_count: usize` instead of `&Log`.
- `src/slice.rs:223` (`launch`) and `:258` (`settled`). Assembles the intent and settlement
  `Vec<Entry>`. Fix: visibility only.
- `src/db.rs:455` (`Log::push`), 4-way kind dispatch; `:392` (`unsettled_sessions`), decides
  `state == InFlight` while reading every commit.
- `src/index.rs:59` (`Index::find`), whose cross-root merge, score sort, tie-break and truncation
  at `:70-95` is pure ranking. Fix: `fn merge(hits: Vec<Hit>, limit: usize) -> Vec<Hit>`.
- `src/provider.rs:419` (`outcome_of`) and `:428` (`detail_of`). Pure egress-decision to `Outcome`
  mapping. Reachable only through `AuditToLog::record` (file IO).
- `src/execution.rs:168` (`store`), where the `code`/`signal`/`attempted` mapping at `:184-191` is
  pure while the rest opens `BlobStore` and `std::fs::read`.
- `src/notify.rs:148` (`ping`), pure half-deadline arithmetic, and `:130` (`Cadence::due`), pure
  interval arithmetic blocked by `tokio::time::Instant` in the signature.
- `src/serve.rs:331` (`bus_ended`), pure result mapping blocked by `tokio::task::JoinError`.

Decision-and-enforcement in one body (each is a candidate to split, per boundary discipline):
`src/config.rs:260` (`resolve` validates then `create_directory`), `:375` (`crash` decides then
`process::exit(97)`), `:440` (`private_credential` decides while reading metadata),
`src/execution.rs:103` (`run` decides `may_invoke` then `sandbox.spawn`),
`src/agent.rs:283` (`Shell::run`), `:561` (`Driver::open`), `src/slice.rs:97` (`submit` decides
replay/divergent/conflicted at `:119-133` then commits and awaits),
`src/serve.rs:173` (`serve_until_stopped` decides readiness at `:265` then notifies).

### 5.3 `mokmok-agent-runtime` — 5853 src lines, 95 cases, 99.2% already

- `src/advance.rs:535` (`act_outcome`), with siblings `:422 begin_step`, `:435 ask_intent`,
  `:471 ask_outcome`, `:500 act_intent`, `:582 find_step`, `:611 close_step`, `:647 halt_close`,
  `:681 halt_settle`. Computes the exact `Vec<Record>` a step commits. 8 branch points over
  434-709. `ask_intent` alone calls `journal.put` plus `serde_json::to_vec` (`:444`); the rest are
  pure value transforms. Fix: split the record list out of the `put`, giving
  `fn prompt_records(...) -> Vec<Record>`.
- `src/context.rs:288` (`paths`), `:99` (`system_prompt`), `:271` (`bytes_of`), `:279`
  (`keep_newest`), `:244` (`context`). Computes the prompt bytes. Blocked by
  `&[std::path::PathBuf]` and `task.setting.checkout.display()`.
- `src/speech.rs:26` (`speak`) decides open-vs-extend-vs-refuse then appends. Already runs against
  `TestJournal`.
- `src/record.rs:203` (`StepId::derive`), `src/advance.rs:406` (`turn_id`), `:418`
  (`execution_id`). Pure uuid v5 derivation; `uuid` is built with its `std` feature only.
- `src/record.rs:44` (`clipped`), a head-clip on a char boundary using `str::floor_char_boundary`.
- `src/advance.rs:124` (`advance`) takes `at: Moment` used only by a `tracing::debug!` at `:132`,
  never in a decision.

What blocks a crate-wide `#![no_std]` here, and each is removable:

| blocker | where | change |
| --- | --- | --- |
| `std::path::PathBuf` in `Setting` | `src/task.rs:3` | an owned path value type; `Setting` is serde-serialized into the durable log, so this is a wire change |
| `std::error::Error` trait object, `impl std::error::Error for Refusal` | `src/error.rs:11`, `src/record.rs:343` | `core::error::Error` |
| `thiserror` with `std` | `Cargo.toml` | `default-features = false` |
| `tracing` | `src/advance.rs:132` | gate the one log line |
| `std::collections::BTreeMap` | `src/dialogue.rs:3` | `alloc::collections::BTreeMap` |
| `serde_json` with `std` | `Cargo.toml` | drop the feature |
| `uuid` with `std`, `v7` | `Cargo.toml` | `["serde", "v5"]` |
| `std::time::Duration` | `src/task.rs:4`, `src/tool.rs:3` | not a blocker: `std::time::Duration` is `core::time::Duration` |

### 5.4 `mokmok-search`

Pure and tantivy-free today: `src/token.rs:38` (`tokens`) and `:10` (`spans`), `src/query.rs:89`
(`snippet`) and `:103` (`clipped`), `src/corpus.rs:347` (`capped`), `src/render.rs:11` (`render`).
`src/query.rs` and `src/render.rs` carry zero inline tests; their only coverage is
`tests/corpus.rs`, which builds a real tantivy index under a temp dir.

The one function touching tantivy is `src/query.rs:33` (`boolean`), and only because `Fields`
(`src/corpus.rs:291`) wraps three `tantivy::schema::Field` values. `Field` is a newtype over `u32`,
so a local `struct FieldID(u32)` removes the type without removing the query. `render` needs no_std
only if `Hit.path` stops being a `PathBuf`.

Gain, measured: the survivors that first scored this crate 75% sat on "the body cap arithmetic, or
the snippet clip". Both are `&str -> String`/`&str -> usize`.

### 5.5 `mokmok-event-store`

`src/store.rs:378` (`append_program`) owns the append precedence: `recorded_in` (`:519`) wins over
the version check (`:385-395`), which wins over a commit. Only `Expected::accepts`
(`src/model.rs:233`) is already pure. Row codecs are pure but private to the DB path:
`position_from` (`:852`), `version_from` (`:846`), `time_from_millis` (`:893`), `to_millis` (`:887`),
`StoredEvent::into_event` (`:823`). Fix: a pure `fn preflight(recorded: Option<Committed>, found:
Version, expected: Expected) -> Preflight`. The crate already scores 100%, so this buys isolates.

### 5.6 `mokmok-sandbox`

`src/capability.rs:191` (`covers`) is a pure component-wise path decision broken by
`capability.path.is_dir()` at `:201`. `uncovered_index` (`:169`) and `uncovered` (`:156`) are
already pure. Its only test (`src/capability.rs:229`) creates a real temp file to make `is_dir()`
true. Fix: pass the fact in. Separately, `rights` (`src/plan.rs:192`) returns
`landlock::BitFlags<AccessFs>`, so the landlock edge reaches even the pure function.

### 5.7 `mokmok-tui`

The reducer is already in `src/core.rs` behind the `Screen`/`Engine` ports (`src/port.rs:13`,
`:52`), and the pure row renderer lives in `src/port.rs:168` (`rows_of`), `:217` (`prose_rows`),
`:241` (`escaped`). `src/terminal.rs` is the only ratatui surface. The three remaining survivors
(`Terminal::leave`, its `Drop`, `Terminal::draw`) need a pty, not an extraction.

A `no_std` reducer is blocked by the `mokmok-agent-runtime` edge (`Transcript`, `Move`, `Progress`),
which is std today. So this crate's purity follows the runtime's.

### 5.8 `mokmok-egress`, `mokmok-observability`, `mokmok-artifact`

These are the crates where almost nothing is locked.

- `mokmok-egress`: `src/destination.rs:86` (`classify`) and `:192` (`Rules::allows`),
  `src/proxy.rs:50` (`parse_connect`), `:77` (`parse_authority`), `:124` (`parse_port`),
  `src/rate_limit.rs:39` (`RateLimit::acquire`, which already takes the clock as a `Moment`
  argument, so its tests never sleep). Blockers for no_std: `std::net` (which is `core::net` since
  Rust 1.77), `std::time::Duration` (which is `core::time::Duration`), `HashMap` in
  `src/rate_limit.rs:19`, and `now()` at `src/audit.rs:58` reading the wall clock. The `tokio` edge
  exists for `src/connect.rs:6` and binds the crate.
- `mokmok-observability`: `src/redaction.rs:109` (`Redaction::apply`), all of `src/metrics.rs`,
  `src/audit.rs:42` (`AuditEntry`), `:151` (`append_and_export`, already pure over the `AuditLog`
  and `Exporter` ports with a proven fake). Blocker: `std::borrow::Cow` in `src/redaction.rs:3`
  (moves to `alloc::borrow::Cow`). `src/format.rs` and `src/init.rs` are the tracing adapter and
  keep the crate std.
- `mokmok-artifact`: `src/content_hash.rs:29` (`ContentHash::of`) and `:76` (`FromStr`) use only
  `core::fmt`, `core::str`, and `sha2`, which is no_std-capable. `src/gc.rs:122` (`Collector::mark`)
  is a pure filter over `Entry` calling a port, with an in-tree fake. `blob`/`filesystem`/`gc::sweep`
  need `std::fs` and bind the crate.
- `mokmok-patch`: needs no std. Exactly three external items today, at `src/lib.rs:3-5`:
  `std::fmt::Write`, `std::str`, and `thiserror::Error`. The first two are the same items in `core`;
  `thiserror 2.0.21` is itself `#![no_std]` with `default = ["std"]`. The change is one attribute,
  `extern crate alloc`, and dropping thiserror's default features. It already scores 100% with 34
  inline tests and property tests, so the gain is the proof, not the tests.

## 6. Dependency edges that order the work

A crate can only become `#![no_std]` once every type its public surface mentions is `no_std`. That
makes the work a topological order, not a fan-out:

- `mokmok-patch` depends on `thiserror` only. No prerequisite.
- `mokmok-egress`, `mokmok-observability`, `mokmok-artifact` depend on `mokmok-protocol` (no_std)
  plus std-only crates. Their pure modules are self-contained.
- `mokmok-event-store` and `mokmok-event-bus` take `mokmok-protocol` (no_std) plus `rusqlite`. Their
  pure modules are self-contained.
- `mokmok-search` takes `tantivy`. Its pure modules are self-contained.
- `mokmok-sandbox` takes `landlock` and `nix`. Its pure decision needs a local flag set.
- `mokmok-agent-runtime` takes `mokmok-protocol` and `mokmok-state-machine`, both no_std, and its
  blockers are all its own.
- `mokmok-kernel` takes twelve `mokmok-` crates including the runtime, so its decisions cannot be
  no_std before the runtime's.
- `mokmok-tui` takes the runtime and the kernel, so its reducer cannot be no_std before both.

`dependency-rules.sh`'s `kernel-dependents-are-applications` rule allowlists `mokmok-tui` as a
composition root.

## 7. Documentation drift, measured

`docs/spec/` is an OKF v0.2 bundle. Three attesters read the tree:
`dependency-rules.sh` (six passes), `line-references.sh` (618 pairs, 0 unresolved),
`okf-conformance.sh` (32 files, 0 problems). None runs in CI.

- `docs/spec/index.md:7` and `docs/spec/workspace.md:22,26,49` say thirteen and fourteen members.
  The manifest lists fifteen. `mokmok-patch` was added in `7194378`, after the bundle's last edit.
- `docs/spec/workspace.md:28-41` has no row for `mokmok-patch`, and `docs/spec/crates/` holds no
  `patch.md`, so one member has no concept file.
- `docs/spec/crates/kernel.md:128` says 93 cases; the tree holds 118 attributes. Its `:130-137`
  per-file numbers (24 for `provider.rs`, 15 for `config.rs`, 8 for `tests/agent.rs`) measure 30, 20
  and 14. Two integration files are named nowhere.
- `docs/spec/crates/kernel.md:41-42` lists the private modules without `index`, which
  `mokmok-kernel/src/lib.rs:19` declares.
- `docs/spec/crates/observability.md:30-38` omits the `error` module that `src/lib.rs:29` declares
  and `lib.rs:41` re-exports.
- `docs/spec/crates/kernel.md:122-124` calls `src/bin/mokmokd.rs:78-88` "the workspace's only
  wall-clock read"; library code reads a clock at `mokmok-egress/src/audit.rs:59` and
  `mokmok-event-bus/src/store.rs:406`.
- `docs/spec/verification/strategy.md:45` says proptest is in ten crates.
- The dated receipts in `docs/spec/log.md` and `docs/spec/verification/*` carry 608 tests, 31 files
  and 570 pairs; today's run of the three attesters reports 618 pairs and 32 files.

No crate document names a `src/` file that does not exist, and every `#![no_std]` claim in the
bundle holds.

## 8. Test-surface shape

| crate | `#[test]` src | `#[test]` tests | proptest src / tests | cross-package bin in tests | wall clock in tests |
| --- | --- | --- | --- | --- | --- |
| mokmok-agent-runtime | 86 (+9 tokio) | 0 | 2 / 0 | no | 0 |
| mokmok-artifact | 0 | 23 | 0 / 0 | no | 5 |
| mokmok-capability | 6 | 43 | 0 / 1 | no | 0 |
| mokmok-egress | 0 | 25 (+4 tokio) | 0 / 0 | no | 0 |
| mokmok-event-bus | 33 (+24 tokio) | 4 (+25 tokio) | 3 / 0 | no | 11 |
| mokmok-event-store | 30 | 14 | 0 / 1 | no (own bin) | 5 |
| mokmok-kernel | 77 (+3 tokio) | 35 (+3 tokio) | 0 / 0 | **yes, hard assert** | 16 |
| mokmok-observability | 17 | 0 | 0 / 0 | no | 0 |
| mokmok-patch | 34 | 0 | 1 / 0 | no | 0 |
| mokmok-protocol | 43 | 0 | 3 / 0 | no | 0 |
| mokmok-sandbox | 20 | 9 (+44 tokio) | 0 / 1 | no (own bin) | 6 |
| mokmok-scheduler | 0 | 22 | 0 / 1 | no | 0 |
| mokmok-search | 5 | 33 | 0 / 1 | no | 1 |
| mokmok-state-machine | 0 | 42 | 0 / 2 | no | 0 |
| mokmok-tui | 40 | 1 (+4 tokio) | 2 / 0 | fallback to shell | 0 |

`mokmok-capability`, `mokmok-egress`, `mokmok-observability`, `mokmok-search` and `mokmok-tui` have
no test that needs a real filesystem beyond a temp dir, and `mokmok-scheduler`,
`mokmok-state-machine`, `mokmok-protocol`, `mokmok-patch`, `mokmok-capability` and
`mokmok-observability` have no test file at all outside `src`.

## 9. What the repository already decided about this

- `mokmok-os-architecture.md:80-82` is the only statement of the purity intent: "Pure contract and
  transition crates should avoid runtime dependencies. `no_std` is appropriate where supported by
  their actual data structures and dependencies; allocation requirements must be explicit."
- `mokmok-os-architecture.md:179` forbids gathering unrelated state machines into one crate.
- `mokmok-os-architecture.md:32` states when a boundary becomes a crate: "when its owned state,
  invariants, contracts, and validation requirements justify the split."
- `mokmok-os-architecture.md:380-382` asks CI to check the dependency rules. It does not.
- `docs/spec/decisions.md:81-85` names the mutation gate's reach as an open problem.
- `docs/spec/decisions.md:69-73` names three crates that ship a port before its second user
  (scheduler, artifact, observability).
- `docs/spec/decisions.md:77-80` names the kernel's unused `mokmok-scheduler` dependency as either a
  waiting caller or a stale manifest line.

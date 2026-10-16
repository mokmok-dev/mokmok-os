# Cross-judge scorecard, round 3

The judge read the three candidates end to end and scored each against the rubric, then verified the
factual claims it was asked to check against the tree rather than against a candidate's word.

## Rubric

- **R1.** Purity is machine-checked, non-vacuously, inside CI: the check file, the rules, the
  `flake.nix` wiring, a mechanism that fails when a declared level disagrees with the tree, and one
  that fails when a purity rule selects zero crates.
- **R2.** Crate-count discipline. Every new crate justified against `mokmok-os-architecture.md:32`
  and `:179`. No crate whose only consumer is the check itself.
- **R3.** Topologically valid order, one independently landable PR per phase.
- **R4.** Every phase ends in a named measurement, and the three measured blockers drive the order:
  the kernel's baseline failure, the eight members with no recorded run, the kernel's 26 survivors.
- **R5.** Subtraction before addition, and the `docs/spec` drift closed by a mechanism.
- **R6.** Grounding accuracy: no load-bearing claim the tree contradicts.

## Scorecard

| criterion | candidate 1 | candidate 2 | candidate 3 |
| --- | --- | --- | --- |
| **R1** | 4/5, four rules and `checks.attesters`; non-vacuity rides on the `allowed` liveness check, with no explicit floor | 5/5, five rules, a stated floor, a declared-vs-tree check both ways | 3/5, three rules; the level check runs one direction only and no rule fails on a zero-sized selection |
| **R2** | 4/5, `bus-core` and `search-core` both clear `:32`; `search-core` leans on one duplicated comparator and is the weaker | 4/5, `delivery` and `log` both clear `:32`; `log` admits a renderer inside a log crate and a layering inversion | 5/5, one crate, justified on state, invariants, contracts and validation, safe under `:179` |
| **R3** | 5/5, topological, and phase 9's need for phase 7's `Path` is stated | 4/5, topological under its own closure rule, but its egress-purity phase as written cannot build | 5/5, fully topological, and `Path` stays crate-local so no cross-phase prerequisite exists |
| **R4** | 3/5, scores only in phases 8, 9, 10 | 4/5, scores in phases 3, 7, 8, 9, 10, 11 | 4/5, scores in P2, P4, P6, P7, P8 |
| **R5** | 4/5, the gate lands one phase after the hand repair | 4/5, subtraction is phase 2 while the spec check lands in phase 1 | 5/5, nine named deletions in P1 including a redundant rule half, with `specBundle` in the same PR |
| **R6** | 4/5, every correction verified true, but the `allowed` lists are attributed to a `propose` mode the script does not have | 2/5, the load-bearing selector claim is false against the tree | 5/5, every correction verified, including the async omission the others missed |

Totals: candidate 1 24/30, candidate 2 23/30, candidate 3 27/30.

## Factual corrections

1. **Candidate 2's central claim is false.** `no_std_crates()` greps `^#!\[no_std\]`, and the pattern
   includes the `!`. Run it: the pattern matches `mokmok-protocol`, `mokmok-state-machine`,
   `mokmok-scheduler` and `mokmok-capability`, and no others. The rule runs over four crates,
   `dependency-rules.sh check "$PWD"` exits 0, and all six rules of that date print `status=pass`.
   The rule is opt-in with an unmeasured take-up rate, which is what the grounding says. It is not
   vacuous. The non-vacuity floor is still worth adding, but not for that reason.
2. **Candidate 1 cites a mode that does not exist.** `dependency-rules.sh` accepts `list`, `check`
   and `attest` only. The `allowed` lists are attributed to "the measured output of
   `dependency-rules.sh propose` on trunk". The census it also reports reproduces exactly, so the
   lists are sound and only the attribution is wrong.
3. **Candidate 2's egress-purity phase omits an edit it needs.** `mokmok-egress/src/proxy.rs:3`,
   `src/destination.rs:3` and `src/audit.rs:3` import `std::net`, which must become `core::net` under
   `#![no_std]`.
4. **Candidate 2's `uuid` allowlist entry is not the string it claims.** `mokmok-protocol` builds
   `#![no_std]` with `features = ["fast-rng", "serde", "v7"]`, not a bare `default-features = false`.

Claims that held under independent check: `Validated::connect` has no caller outside
`mokmok-egress/tests/enforcement.rs`; `Resolver`'s only implementer and only user is
`mokmok-kernel/src/provider.rs`; `Setting`'s `PathBuf` serde form is a plain string, proven by the
round trip at `mokmok-agent-runtime/src/task.rs:75`, so a `#[serde(transparent)] Path(String)` is
wire-identical; `cargo mutants 27.1.0` does have `--test-workspace`; the kernel's 26 unkilled
mutants are 24 missed plus 2 timeouts. One extra finding: no mutation ledger exists in the tree, so
"eight members with no recorded run" describes operator runs rather than a committed file.

## Recommended base

Candidate 3. It is the only candidate whose every factual claim survives the tree, and its phase list
is the only one fully topological without an unstated prerequisite. It scores highest on R2, R3, R5
and R6 while conceding R1. The choice is the laziness protocol: one crate that owns state and
invariants beats two, and in-place `pub(crate)` free functions with property tests beat a crate per
seam. Candidate 2 loses by the attack-the-premise reading: it wrote a fix for a defect the tree does
not have, which is the failure mode a cross-judge is meant to catch.

Its two real weaknesses are R1 and R4, and both are graftable without touching its shape.

## Grafts

From candidate 2: the explicit non-vacuity floor; a rule closing the reverse direction of the level
check; `mokmok-log` as a later phase after the kernel's hardening, so the folds' survivors have a
scored home; the audit clock as a `Moment` argument, which would make `mokmok-egress` a sixth pure
crate; and `--test-workspace true` recorded as the fallback if the confiner export regresses.

From candidate 1: the `shell` level plus the composition-root allowlist, so a third root cannot
appear silently; `only-allowed-modules-use-std` with liveness; `mokmok_protocol::Path` in place of a
runtime-local one, because the durable task record carries the value; and the disk subtraction of
`mokmok-data/` and the untracked mutants output.

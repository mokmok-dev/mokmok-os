---
name: mutation-test-loop
description: >
  Close the mutation-testing gaps a `mutator` run reports in Rust code. Use when
  the user runs /mutation-test-loop, asks to fix survived mutants, says "cargo
  mutants found survivors", "raise the mutation score", "make the mutation gate
  pass", or when a `mutator --fail-on-survived` run failed in their branch.
---

# Mutation test loop

`mutator` runs `cargo-mutants` on the mutants a diff can affect and reports each as
`KILLED`, `SURVIVED`, `TIMEOUT`, `UNVIABLE`, `ACCEPTED`, or `NOT RUN`. A survivor is a
test gap: the code can change and the suite still passes. This skill closes those gaps.

Work from the report. Never edit source to make a mutant disappear.

## 1. Get the gaps

```sh
mutator --since main --cache --reporter json > /tmp/mutation.json
jq -r '.rows[] | select(.status=="Missed") | [.id, .name] | @tsv' /tmp/mutation.json
```

An empty `rows` array means the diff has no mutants in scope, which is not a pass on
the tests. Confirm with `--list` that the revision is right, or run `--all` to see the
whole project.

`.id` addresses the mutant for `mutator accept`. `.name` is what `cargo-mutants` calls it
(`file:line:col: replace > with >= in clamp`). Read the name to see the change:
`replace X with Y`, `delete - in f`, `replace f -> T with 0`.

If the run is not green, stop and report the failing row instead of looping.

## 2. Fix one gap at a time

For each survivor, in report order:

1. Open the file and line in `.name`, and read the enclosing function named in `.function`.
2. Decide which of the three cases it is. The mutant name tells you:

| shape of the mutant | what the tests miss | the test to write |
|---|---|---|
| `replace < with <=`, `replace > with >=`, `replace == with !=` | a boundary value | a test on the exact boundary, plus one just past it |
| `replace + with -`, `replace * with /` | an arithmetic result never asserted | assert the computed value of a case where the operators differ |
| `replace f -> T with 0`, `with 1`, `with -1`, `with true`, `with ""` | the return value never observed, or only a side effect | assert the returned value directly |
| `delete - in f`, `delete ! in f` | the sign or negation is unobserved | a case where the operand is not zero, and one where it is negative |
| `replace f -> T with Default::default()` | the caller ignores the result | assert the value the caller uses |

3. Write the smallest test that fails against the mutant and passes against the real
   code. Add it beside the existing tests for that function, matching the local style.
4. Re-check. A `--cache` run reruns survivors and reuses kills, so it costs seconds
   after the first cold run:

```sh
mutator --all --cache --reporter text
```

Use `--all` while looping. A diff-scoped run will not see a mutant you just killed in a
function the diff does not touch, so it cannot tell you whether the test worked.

## 3. Accept only what no input can observe

Some mutants are equivalent: no input distinguishes them. Accept one only when you can
name the property that makes it unobservable. Record that property as the reason.

```sh
mutator accept 'src/parse.rs::number::BinaryOperator::+#0' \
  --reason "operands are u8 and the sum saturates, so wrapping never differs"
```

The comparison operators are the common trap. `v > hi` and `v >= hi` in
`if v > hi { hi } else { v }` are equivalent: when `v == hi` both arms return `hi`, so no
input separates them. Write the argument down instead of writing another test. The same
holds for `<=` against `<` when both arms return the same value at equality, and for a
`+` on values that provably cannot overflow.

Good reasons name a type bound, an invariant on the caller, or a value the function
provably never sees. Bad reasons: "hard to test", "defensive code", "unreachable".
If you cannot write the property, write the test instead.

## 4. Stop when the gate passes

```sh
mutator --since main --cache --fail-on-survived
```

Exit 0 means every mutant in scope is killed, accepted, or unviable. Report:

- the gap count before and after,
- each test added and the mutant it kills,
- each accepted id with its reason,
- anything still `NOT RUN`, which the gate treats as a failure.

## Rules

- One mutant per test where possible. A broad test that happens to kill three mutants
  leaves the next reader unable to tell which assertion matters.
- Do not weaken an assertion to make a different test pass, and do not add
  `#[mutants::skip]` to hide a gap. An accepted entry in `mutator.accepted.json` is the
  only place a mutation gap should ever be recorded, and it carries a reason.
- A `TIMEOUT` counts as killed. Look at it only if you suspect the mutant exposed a real
  infinite loop, in which case report it rather than accepting it.
- `UNVIABLE` is outside the score. Nothing to do.
- If a survivor sits in test-only or `#[cfg(test)]` code, check whether that code should
  be mutated at all before writing a test for it.

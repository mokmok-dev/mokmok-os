# The independent read of the brief, and its triage

Provenance: one read-only reviewer subagent (id `01a1334b-c9b4-70e3-8e57-bed8b2beac61`, 193s) was
given the brief, this round's trail rows, the arena archive, and the repository, and asked to flag
what the operator should weigh before saying go. It ran on the parent model rather than a different
family, because this harness reserves model choice for the operator.

It raised 32 flags and confirmed every load-bearing claim about the code it was asked to check
(`Entry`/`Kind`/`Log`, the `Copy`/content-identity rule, the sandbox `Request` and workspace
surface, `plan.rs`'s ABI V3 scope, the authority's grant shape), and found no forbidden dependency
edge. The flags below are paraphrased and grouped by what was done with them; the reviewer's
wording is not reproduced.

## Fixed in the brief

| flag | what was wrong | the fix |
| --- | --- | --- |
| tie rule | the brief required a `Prompt` to have both `ModelRequested` and `ModelAnswered`, but the prompt is committed before the call, so the rule refused every prompt the loop writes | `Prompt` ties to `ModelRequested`, `Answer` to `ModelAnswered`, `Call` to `ToolRequested` |
| unknown outcome | the "do not assume it took effect" message had no home: `Unknown` has no result and no observation record exists | the message is a constant arm of a derived `tool::observation`, so it lands in the next turn's durable `Prompt` |
| halt on unknown | "says it to the model and halts" is impossible in one run, and a halt with no transition out is a dead end | `Halt::UnknownOutcome` deleted; the model is the reconciler and the runner never re-dispatches |
| provider contract | the brief's frozen `Provider` said "parses back the answer text" while the probe had measured structured tool calls, so the one thing the probe settled was absent from the interface | the OpenAI `tools`/`tool_calls` shape is frozen in the brief, with `reasoning` and every vendor id named as discarded |
| `--allow-tool` | "an unlisted program is unreachable because the model never chooses one" is false: authority is consulted for the fixed program, and the `sh -c` string names anything on the granted paths | the brief says plainly that the path grants are the allowlist, and the prompt tells the model the same |
| network scope | `AccessNet` has TCP bind/connect and no UDP or `AF_UNIX`, so "there is no network" promised more than unit 2 delivers | unit 2 claims TCP only; the prompt says "no TCP"; UDP and `setsid` are named residuals |
| egress | who supplies the `PolicyClient`, which `Rules`, where DNS happens, where the audit goes, and what `ExecutionID` the `CallContext` uses were all unstated | all five frozen: `Rules::public_only()`, a host:port policy in `provider.rs`, the existing resolver, the observability audit sink, a derived model-call execution id |
| cost | a live paid run had one bound (`--max-turns`) and a crash mid-call pays twice | `Budget::tokens` and `--token-budget` added; `--max-turns` kept; the double charge is stated in the definition of done |
| data egress | the round sends a checkout's contents to a third-party endpoint and never said so | stated in the definition of done with the local-endpoint alternative named |
| identity | `StepId::derive`'s key was unspecified, though it is what makes an intent replay | frozen as `uuid5(session, "{tag}:{n}")` with the attempt in the `ask-*` tags |
| re-ask | "the prompt bytes are durable, so the re-ask sends identical bytes" was an assumption | `Context` carries no attempt, step, or clock, so it is a consequence |
| cancellation | architecture 5.5's cancellation invariant had no unit, leaving `Draining` dead code | `mokmokd agent cancel` added to unit 4 with a test |
| `AlreadyExists` | `execution::run` calls `completed.cleanup()` before the settlement commits, so the directory is gone and cannot be the second wall | the brief says so and names the `Receipt` rule as the wall that has to hold |
| unit 7 | "iterate the prompt, the clip, and the budget" named no metric and no stopping rule | three recorded tasks, scored as tasks-passing and tokens-spent, stopping after two iterations or at three passing |
| unit 5's dependency | `Cargo.lock` has no TLS crate, and the plan did not say who adds it or whether the network-free gate can build it | unit 5 starts by adding rustls and proving the gate builds it |
| env test | "a test proving an inherited variable does not reach a workload" passes vacuously if the variable is unset | the test spawns a workload from a parent that holds the marker |
| example | the usage granted `/srv/checkout` but passed `CARGO_HOME=/srv/cargo` without granting it | the grant was added |
| draft defects | a dangling "Tradeoffs below" reference, an undefined `Pending`, a call site that would not typecheck, `Resource::Host` described as new when it exists, "db.rs gains no knowledge of the records" overstating it, the example's cargo home | each corrected in place |

## Kept as named limits or open decisions

| flag | why it stands |
| --- | --- |
| one kind instead of five | an unknown record tag stops the read one level finer than an unknown kind; accepted and written down |
| a budget-exhausted run closes `Completed` | `Ending` has two values; adding a third is a state-machine change this round does not make |
| `mokmok-os-architecture.md` is untracked | it is another author's file; committing it is the operator's call and unit 0 says so |
| UDP, `AF_UNIX`, and `setsid` | Landlock's `AccessNet` cannot reach them; a seccomp filter is a later round |
| the live path is outside the gate | the gate has no network; fixtures cover the parse and the harness covers the wire |
| the provider is not under the capability authority | the grant path for `Resource::Host`/`Model` does not exist; a separate round |

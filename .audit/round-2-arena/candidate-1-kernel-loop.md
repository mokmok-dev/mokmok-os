# Candidate 1 — the whole-session loop runs in the kernel process

Mandate: one new `mokmokd` subcommand drives a session to completion in the kernel process; the
model's answer and the tool call it asks for are new append-only `Entry` kinds beside `Session`,
`Turn`, `Result`; the model client is an in-process adapter behind a port. Where that fails is
stated in **Where the mandate fails** below.

## Usage (caller's view)

```console
$ mokmokd --data-dir /var/lib/mokmok \
    --allow-tool /bin/sh --allow-tool /nix/store/.../git \
    --allow-path /nix/store --allow-write /srv/checkout \
    run --session 018f... --idempotency-key 018f... \
        --task "make `cargo test` pass in /srv/checkout" \
        --checkout /srv/checkout \
        --provider ollama --model kimi-k3 --max-turns 24 --context-bytes 65536
session=018f... phase=Closed(Completed)
turn=1 tool=/bin/sh args=["-c","cargo test 2>&1"] execution=... resolution=Some(Finished{code:101})
turn=2 tool=/nix/store/.../git args=["-C","/srv/checkout","diff"] execution=... resolution=Some(Finished{code:0})
turn=3 answer=stop final=artifact:3fa9...
stop=goal turns=3
```

A resumed invocation is the same command line: every turn already in the log replays and is not
re-run, and an unanswered model request stops as `stop=unknown-model` rather than re-asking.

Three Rust call sites:

```rust
// crates/mokmok-kernel/src/bin/mokmokd.rs — the CLI arm.
Command::Run(command) => print_outcome(&agent::run(&config, command, at).await?),

// crates/mokmok-kernel/tests/agent.rs — the whole loop, no network, under `nix flake check`.
let out = run(&["run", "--session", S, "--idempotency-key", K, "--task", "touch /srv/checkout/ok",
    "--checkout", "/srv/checkout", "--provider", "fake", "--script", script, "--max-turns", "8"]);
assert!(out.out.contains("stop=goal"));

// crates/mokmok-agent-runtime/src/agent.rs — the pure driver, unit-tested with no I/O at all.
let next = next(&View { phase: Phase::Accepting, task: None, turns: vec![], max_turns: 24, .. });
assert_eq!(next, Next::Ask(Attempt::new(1)));
```

## Shape — module map

```
mokmok-protocol/            + ArtifactRef (a durable blob reference; execution::Blob becomes an alias)
mokmok-state-machine/       unchanged — Session/Turn stay pure, no_std, and Copy
mokmok-agent-runtime/       the loop's decisions and content types (no I/O, no wire)
  model.rs                  trait Model, Ask/Answer/Finish/Usage/AskError
  log.rs                    Task, ModelResponse, ToolCall — what the kernel's Entry carries
  agent.rs                  View, TurnView, Next, Stop, fn next, fn prompt  (pure)
mokmok-provider/            adapters behind the port: Fake (gate) and Ollama (rustls + egress)
mokmok-kernel/
  db.rs                     + Entry::Task/Model/Tool, Log fields, verify rules, push arms
  agent.rs                  the `while` loop: fold -> next -> I/O -> commit (reuses slice.rs primitives)
  config.rs                 + Command::Run, Run args, Checkout validation
```

`mokmok-kernel -> mokmok-provider -> mokmok-agent-runtime -> mokmok-protocol, mokmok-state-machine`.
`mokmok-provider -> mokmok-egress`. No lower crate imports the kernel; the state machine gains no
dependency. The wire (HTTP, TLS, provider JSON) exists only in `mokmok-provider`.

## Shape — durable kinds

Vocabulary stays append-only: three kinds are added, none renamed or removed.

| `Entry` | kind | payload | committed in the batch with |
| --- | --- | --- | --- |
| `Session(Fact)` | `mokmok.session.fact` | `Fact` | *existing* |
| `Turn(TurnID, TurnFact)` | `mokmok.turn.fact` | `TurnRecord` | *existing* |
| `Result(ExecutionID, ExecutionResult)` | `mokmok.execution.result` | `ResultRecord` | *existing* |
| `Task(Task)` | `mokmok.agent.task` | `Task` | the session's first commit |
| `Model(TurnID, ModelResponse)` | `mokmok.model.response` | `ModelRecord { turn, response }` | `TurnFact::ModelAnswered` |
| `Tool(ExecutionID, ToolCall)` | `mokmok.tool.call` | `ToolCallRecord { execution, call }` | `TurnFact::ModelAnswered` (before dispatch) |

```rust
// mokmok-protocol
pub struct ArtifactRef { pub hash: String, pub bytes: u64, pub truncated: bool }

// mokmok-agent-runtime/src/log.rs
pub struct Task { pub text: String, pub checkout: Option<PathBuf>, pub max_turns: u64,
                  pub context_bytes: u64 }
pub enum Finish { Stop, Length, ToolCall }
pub struct Usage { pub input: u64, pub output: u64 }
pub struct ModelResponse { pub finish: Finish, pub answer: ArtifactRef, pub model: String,
                           pub usage: Option<Usage>, pub provider_request: Option<String> }
pub struct ToolCall { pub program: String, pub args: Vec<String>, pub artifacts: Vec<String>,
                      pub deadline_seconds: u64 }
```

`ModelResponse.answer` is a reference; the assistant text lives in `mokmok-artifact`'s `BlobStore`
and is written before the commit that names it, exactly as `execution.rs::store` already orders a
tool's bytes before `Fact::ExecutionSettled`. The log's growth is a hash and a count.

`db.rs` gains `Log { models: Vec<(TurnID, ModelResponse)>, tools: Vec<(ExecutionID, ToolCall)>, task:
Option<Task> }`, lookups `Log::answer(turn)`, `Log::tool_call(execution)`, and two rules enforced in
`Log::verify` rather than by convention:

- `Entry::Model(turn, ..)` is admitted only when the batch already carries `ModelAnswered` for
  `turn` or the replayed machine holds it — no answer content without an answered turn.
- `Entry::Tool(execution, ..)` is admitted only when a `ToolRequested { execution, .. }` for that
  turn is in the same batch or the history. A content entry never floats free of its machine fact.

Old-log replay: `Db::log` already reads a *superset* of kinds, and a pre-change stream holds only
`Session`, `Turn`, `Result`, so it folds through unchanged (`Session::replay` and `Turn::replay` are
untouched). The converse is not offered: a build that predates the new kinds still stops on
`UnexpectedEntry`, which is the existing deliberate behaviour.

## Shape — the loop

```rust
// mokmok-agent-runtime/src/agent.rs — pure: clock-free, I/O-free, wire-free.
pub enum Next {
    Ask(Attempt),            // record ModelRequested{attempt}, then call the model
    Dispatch(ExecutionID),   // the recorded answer asked for a tool that is not yet resolved
    Stop(Stop),              // end the session
}
pub enum Stop { Goal, Budget, Phase, Truncated, UnknownModel, Refused }

pub fn next(view: &View) -> Next { /* not implemented */ }
pub fn prompt(view: &View) -> String { /* not implemented */ }   // bounded transcript
pub fn execution_of(turn: TurnID, index: u32) -> ExecutionID { /* not implemented */ } // deterministic
```

`next` is a total function of the log fold, so "one invocation owns the whole session" costs no
resumability: the kernel holds control flow, the runtime holds the decision. The rules `next`
enforces, each visible in `View`:

- no task recorded -> `Ask` with attempt 1 (the kernel opens the session and the first turn);
- last turn answered, `Finish::Stop`/`Length` -> `Stop(Goal)`/`Stop(Truncated)` with the recorded
  answer as the deliverable;
- last turn answered, `Finish::ToolCall`, tool unresolved -> `Dispatch` (the recorded call, never a
  re-parse);
- last turn settled -> `Ask` a new attempt for a new turn; `view.turns.len() >= max_turns` ->
  `Stop(Budget)`; `phase != Accepting` -> `Stop(Phase)`;
- last turn requested and *unanswered* -> `Stop(UnknownModel)`: a request seen across a process
  boundary is an external outcome nobody can prove, and the loop refuses to re-ask it.

```rust
// mokmok-kernel/src/agent.rs — the loop. Opens the Db per commit, not for the session's lifetime.
pub async fn run(config: &Config, args: &Run, at: Moment) -> Result<Outcome, Error> {
    // resume + recovery: slice.rs::resume, unchanged.
    // record Entry::Task once, keyed by CommandId::from(sha5(root, "task")).
    loop {
        let view = View::fold(&db.log(session)?, config, args)?; // reads artifact text back
        match agent::next(&view) {
            Next::Ask(attempt) => {
                commit(Entry::Turn(turn, ModelRequested { attempt }));           // durable first
                match model.ask(&Ask { turn, attempt, prompt: agent::prompt(&view) }).await {
                    Ok(answer) => commit([ModelAnswered, Entry::Model(..), Entry::Tool(..)?]),
                    Err(AskError::NotSent)     => continue,        // proven unsent: attempt+1
                    Err(AskError::Ambiguous)   => stop(Stop::UnknownModel), // sent, unproven
                }
            }
            Next::Dispatch(execution) => {
                commit([ExecutionLaunched { execution, Bounded }, ToolRequested { .. }]);
                crate::config::crash(config, CrashPoint::BeforeDispatch);
                let ran = crate::slice::run_tool(config, &call, execution, at).await?;
                commit(settled(turn, execution, ran));                            // existing shape
            }
            Next::Stop(stop) => { commit([TurnSettled { .. }, SessionClosed]); return out(stop); }
        }
    }
}
```

Identity is derived, never minted per process: `TurnID = uuid5(root_command, "turn:{n}")`, each
turn's intent and settle commands are `uuid5(root_command, "turn:{n}:intent|settle")`, and
`ExecutionID = uuid5(turn, "tool:{index}")`. A re-run of the same command line reconstructs the same
ids, so every committed turn is an event-store replay and no tool is dispatched twice; a resumed
dispatch that somehow reached the sandbox meets `Workspace::create`'s `AlreadyExists`, which is the
mechanical second wall. `Turn::answer()` returning `Duplicate`, `Fact::ResultApplied` returning
`Duplicate`, and `Turn::resolve()` returning `Duplicate` remain the third.

## Shape — the port and the provider

```rust
// mokmok-agent-runtime/src/model.rs
pub struct Ask { pub turn: TurnID, pub attempt: Attempt, pub prompt: String }
pub struct Answer { pub finish: Finish, pub text: String, pub call: Option<ToolCall>,
                    pub usage: Option<Usage>, pub provider_request: Option<String> }
pub enum AskError {
    NotSent,     // the request provably never left: a retry is safe, nothing to reconcile
    Ambiguous,   // the provider may have processed it: the loop must not retry
}
pub trait Model { async fn ask(&self, request: &Ask) -> Result<Answer, AskError>; }
```

`mokmok-provider::Ollama` POSTs an OpenAI-compatible body to the configured endpoint, wrapping the
TLS stream over `mokmok_egress::Validated::connect` (validated and audited before any socket). It
**parses** `choices[0].message.content` / `finish_reason` / `usage` / the completion id and the
content as the tool-call JSON; it **discards** HTTP status text and headers, every unknown JSON key,
the request echo, and every provider id except the one kept as `provider_request`. `NotSent` is a
resolve/connect failure before any byte is written; anything after the request is written is
`Ambiguous`. `Answer.text` is never a wire type. `Fake` replays a scripted `[Answer; N]` from
`--script`, so `tests/agent.rs` drives the entire loop — model, artifact store, authority, confined
tool, settlements — with no network under `nix flake check`.

## Shape — the agent's tool surface and workspace

The model is offered exactly two moves. A `run` call is our JSON
`{"tool":"run","program":"/abs/path","args":[...],"deadline_seconds":n}`; any other answer is prose
and ends the session. `program` must be absolute and is admitted only if `Authority::may_invoke`
allows it; an unlisted program becomes `Resolution::Refused` through the existing path — the model
cannot widen its own authority. Output is capped by `Limits::output` (1 MiB) and context is capped
by `Task::context_bytes`; `prompt` elides the oldest turns first and marks each elision.

Each execution still gets a fresh ephemeral `work/`, `stdin` at `/dev/null`, and no network. The
checkout is reached the only way the sandbox allows: the operator names it with `--allow-write`, and
`config.rs` refuses a `--checkout` no write grant covers; the loop passes it as a `Request::exposure`
on every spawn. `prompt` states the three facts the model cannot infer — cwd is a fresh empty
directory, stdin is `/dev/null`, the network is absent — and tells it to address the checkout by
absolute path. Artifacts are workspace-relative, so the loop also declares `work/` outputs it wants
back; edits to the checkout persist in place and are not copied.

## Where the mandate fails

1. **"The loop lives in the kernel" cannot mean "the loop's policy is kernel code."** Architecture
   5.12 makes the kernel a mechanism that "decides nothing," and the section 6 table gives the agent
   runtime the transition functions. So the *sequencing and I/O* live in `mokmok-kernel::agent`, but
   `next`, `prompt`, and every content type live in `mokmok-agent-runtime`. The loop runs in the
   kernel process; taken literally, the mandate asks the composition root to grow an agent-policy
   layer the architecture forbids.
2. **An in-process model call is an unreconcilable external effect.** A tool gets an `ExecutionID`,
   a `Fact::ExecutionSettled`, an artifact, and recovery that says `Unknown`. A model call gets
   none of that: no sandbox record, no durable reference to what was sent, no provider-side
   idempotency unless the wire happens to carry one. The loop's best is to commit
   `ModelRequested{attempt}` before the call and treat an unanswered request on restart as
   `Ambiguous`, stopping the session instead of retrying. It cannot distinguish "never sent" from
   "answered into the void" after a crash; `Attempt` makes a retry *representable*, not *safe*.
3. **The model call bypasses the capability authority and the egress audit unless deliberately
   routed.** `--allow-tool`/`--allow-path` name tools, not the provider, and the credential lives in
   the kernel process's environment, outside every confinement. Routing through `mokmok-egress` buys
   destination validation and an audit row, but the destination is still not something the operator
   granted, and `CallContext` wants an `ExecutionID` the model call does not honestly have.
4. **The wire path cannot be tested where it matters.** The gate has no network and `Cargo.lock` has
   no TLS; `rustls` compiles offline but the adapter's HTTP behaviour is exercised only on the
   operator's machine. Only `Fake` drives the loop under `nix flake check`.

## Problem

The repository has one operator-named tool and a fabricated answer (`slice.rs:8`). What is missing
is the model, and the constraint that makes it hard is that `TurnFact` is `Copy` and `no_std`, so the
model's prose and the tool call's arguments have nowhere to sit inside the pure machines; the log's
kind vocabulary is the only durable home, and `Log::push` stops on a kind it does not know. The
checkout is a second constraint: every execution gets an ephemeral `work/` with `/dev/null` stdin
and no reuse, so a coding agent can only edit a persistent tree through an explicit write exposure.
The shape is non-obvious because the mandate's "loop in the kernel" collides with the architecture's
"the runtime owns the loop and the kernel decides nothing" — the resolution is to split the loop
into a pure decision (`next`) the runtime owns and sequencing the kernel runs.

## Tradeoffs accepted

- We accept a third new kind (`mokmok.agent.task`) beyond the two the mandate names, in exchange for
  a log a resumer can read alone: without it, the goal depends on CLI text re-supplied by the
  operator, which is not a durable record.
- We accept that a crash between `ModelRequested` and an answer kills the session as unknown rather
  than resuming it, in exchange for never double-calling a provider. `Attempt::new(2)` exists and is
  unused across a process boundary on purpose.
- We accept a long-lived `mokmokd run` process, in exchange for one command that drives a session;
  the `Db` is opened per commit, so the relay and reconcile still reach the file between turns.
- We accept an in-process `rustls` + egress dependency in `mokmok-provider`, in exchange for keeping
  every wire type out of `mokmok-protocol` and `mokmok-agent-runtime`.
- We accept one tool per turn, matching the one-model-round-trip-per-turn rule; parallel calls would
  spend several `ExecutionID`s keyed by `index`.

## Alternatives considered

- **Loop in the kernel crate with no runtime split** — smallest file count, but puts agent policy in
  the composition root, contradicting 5.12 and the section 6 table. Lost on fit, not simplicity.
- **A `Journal` port the runtime commits through** (the candidate-2 shape) — richer interface depth,
  but it hides the mandate's deliverable (the kernel's own `Entry` vocabulary) behind a second
  representation and adds a pass-through method. Lost because the durable vocabulary *is* the point.
- **The model call as a confined execution** (the candidate-3 shape) — inherits authority, limits,
  artifact store, and `Resolution::Unknown`, strictly better reconciliation, but it pays a provider
  program, a credential written into a workspace, and a per-call spawn. Lost here because the mandate
  fixes the client as in-process; it is the alternative to graft if failure 2 is judged too costly.

## Open questions and risks

- Does the operator accept that a provider failure stops the session instead of retrying, or is a
  bounded in-process retry on `Ambiguous` (new command id) wanted? These are different contracts.
- Should the `Task` record stay, or should `--task` be re-supplied on resume like `submit --program`?
- Is one `Entry::Tool` per answer enough, or are parallel calls wanted? If so, `execution_of`'s
  `index` becomes load-bearing and `next` must order settlements.
- `mokmok-egress`'s `CallContext` requires an `ExecutionID` the model call does not have. Extend
  `CallContext`, or mint a model-request identity in `mokmok-protocol`?
- The mutation gate can score `next`, `prompt`, `execution_of`, the `verify` rules, and `Log::push`
  arms — all pure. It cannot score `mokmok-provider::Ollama` (network), the crash-resume distinction
  (fault injection, the `--crash-after` pattern), or the model call's non-idempotence.

## Next implementation step

Land `ArtifactRef` in `mokmok-protocol` and the three `Entry` kinds plus `Log` fold/lookups in
`db.rs`, with the two `verify` rules and a replay test over a log that predates them.

## Synthesis decision

*Filled in by the orchestrator.*

# Round 2 frozen interface: the agent runtime as a coding agent

Round 1 froze the crate map and landed the vertical slice for one operator-named tool
(`mokmok-kernel/src/slice.rs`). Round 2 gives that slice a model, so a session is a coding agent:
a real provider, a shell under the existing capability authority, every commitment durable before
its effect, and a crash at any point resumable without blind re-execution.

This file is the brief a delegate builds one unit against and the record a reviewer reads first. It
is not a second architecture document; `mokmok-os-architecture.md` sections 5.5, 6, 8, and 10 are
the architecture, and section 5.5's runtime invariants are this round's acceptance target. That
document is **untracked in this worktree**; unit 0 commits it or the citations here do not resolve
for a reader, and committing another author's file is the operator's call.

Status: revised once after an independent read (flags and their resolutions are in
`.audit/decisions.tsv`). The design came from a three-candidate arena with a cross-judge; the three
candidate packages, the judge's report, and the measured provider responses are in
`.audit/round-2-arena/`. Read the round-1 brief too for the gate conventions and the comment rule;
they carry over unchanged.

## Definition of done

A falsifiable predicate, checked in the gate and on the operator's machine separately.

1. **In the gate, with no network:** a scripted provider drives the whole loop under
   `nix flake check` — model call, artifact store, authority decision, confined tool, settlement,
   close — and a second test kills the run between an intent and its outcome and proves the resume
   neither re-executes the tool nor claims an outcome nobody proved.
2. **On the operator's machine, with the real provider, in a scratch checkout:** one
   `mokmokd agent run` invocation calls the model at least twice (once for a command, once to
   finish), runs that command under the sandbox, and leaves the task's effect on disk.
   `examples/coding-agent/run.sh` asserts the artifact and the loop: `out.txt` holds the values the
   goal named, and `mokmokd status <session>` replays at least two prompts, two answers, and one
   result, so the run cannot have reached the goal in a single ask. The refusal path is proven in
   the gate instead, by a scripted run with no tool granted whose transcript reports the refusal.
3. **On the operator's machine, a crash mid-loop:** the killed run's next invocation settles the
   in-flight tool as `Unknown`, and the next `Prompt` record — read back out of the artifact store
   — contains the warning that the previous command's outcome is unproven and its effect must not
   be assumed. The runner never re-dispatches that command; the model decides what to do next.
4. `mokmokd status <session>` reads a finished session back as its whole transcript: turns, the
   prompt each turn was handed, the answer, the command, the result.

The operator-facing preconditions of 2 and 3, stated so nobody discovers them later: the checkout
is a **scratch** directory granted with `--allow-write`, owned by an unprivileged user, and the run
happens as that user (not as root, not as the `mokmokd` service user). **The checkout's contents,
including every file the agent reads, leave the machine to the configured provider endpoint.** The
default endpoint is `https://ollama.com/v1`; `--endpoint http://127.0.0.1:11434/v1` points the same
adapter at a local server instead, which is the answer for a repository that may not leave the
host. The round has a per-run token ceiling (`--token-budget`) and a turn ceiling (`--max-turns`),
and a crash mid-call pays for that attempt twice.

## Usage (the caller's view)

```
$ mokmokd --data-dir /var/lib/mokmok \
    --allow-tool /bin/sh --allow-path /nix/store --allow-path /srv/cargo \
    --allow-write /srv/checkout --allow-write /srv/cargo \
    --tool-env CARGO_HOME=/srv/cargo \
    agent run --session 018f… --goal "make the failing test in src/parse.rs pass" \
              --checkout /srv/checkout --max-turns 24 --token-budget 400000 \
              --provider ollama --model kimi-k2.7-code --endpoint https://ollama.com/v1 \
              --provider-credential-file /run/secrets/ollama
turn=1 number=1 answer=command command="cd /srv/checkout && cargo test 2>&1 | tail -30"
turn=1 execution=018f… resolution=Finished{code:101} bytes=8123
turn=2 number=2 answer=command command="cd /srv/checkout && sed -n '1,40p' src/parse.rs"
…
turn=6 number=6 answer=final stop=done phase=Closed(Completed)
$ mokmokd --data-dir /var/lib/mokmok … agent run --session 018f… --steps 1
move=act turn=2 execution=018f…        # a crash between these two lines, or before them, resumes here
$ mokmokd --data-dir /var/lib/mokmok … agent cancel --session 018f…
```

`agent run` loops bounded steps until the session closes or yields. `--steps N` bounds how many
steps one invocation takes, which is what the crash harness uses. `agent cancel` writes
`Fact::CancelRequested`, so the next `next` sees `Phase::Draining` and closes the session as
cancelled rather than leaving the machine's cancellation vocabulary dead.

Call sites:

```rust
// mokmok-kernel/src/bin/mokmokd.rs — the CLI arm. The kernel reads no wall clock itself; one read
// in this binary fills the `Moment` every arm takes, and the provider's destination validation is
// the one place outside it that stamps an audit with a clock of its own.
Command::Agent(Agent::Run(run)) => print_agent(&agent::run(&config, run, at).await?),
Command::Agent(Agent::Cancel(session)) => print_cancel(&agent::cancel(&config, session, at)?),

// mokmok-kernel/tests/agent.rs — the whole loop, scripted provider, no network, under the gate.
let mut journal = Journal::open(&config)?;
let progress = advance(&mut journal, &Scripted::of(script), &tools, session, at).await?;
assert!(matches!(progress, Progress::Advanced(Move::Act { .. })));
```

## Shape

### Module map

```
mokmok-agent-runtime        pure: mokmok-protocol, mokmok-state-machine, serde, uuid, tracing
  record.rs   Record, Step, StepId, Receipt, Refusal
  dialogue.rs Transcript, the fold of a session's records
  task.rs     Task, Setting, Budget
  context.rs  Context, ContextTurn, system_prompt, context()
  model.rs    Provider, Answer, Action, Usage
  tool.rs     Tools, ToolCall, ToolReport, Exit, observation()
  advance.rs  Move, next(), advance(), Progress
  error.rs    Error
mokmok-kernel               composition root: the adapters and the CLI
  src/agent.rs    Journal, Tools, Scripted, agent::{run, cancel} over the existing execution::run
  src/provider.rs the OpenAI-compatible adapter (rustls + mokmok-egress validation)
  src/db.rs       + Entry::Agent(Record), one kind, Log fields and lookups
  src/config.rs   + Command::Agent, provider flags, --tool-env, --token-budget, checkout
                  validation, CrashPoints
mokmok-sandbox              + Environment::{Extend, Exact} on Request, and no TCP for a workload
mokmok-protocol             + ArtifactRef (execution::Blob becomes an alias of it)
mokmok-state-machine        unchanged
```

Dependency edges, all top-down and all allowed by the section 6 table:
`mokmok-kernel → mokmok-agent-runtime → {mokmok-protocol, mokmok-state-machine}`. The kernel already
depends on the sandbox, artifact, capability, and event-store crates it adapts. No lower crate
depends on the kernel, and no vendor, HTTP, or sandbox type appears in the runtime.

The credential is **not** a `Config` field: `Config` derives `Debug` and is logged, so unit 5 reads
the file in `provider.rs` at open into a type whose `Debug` prints `REDACTED`, and the path is the
only thing the CLI carries.

### The durable vocabulary: one new kind

| `Entry` | kind | payload | committed with |
| --- | --- | --- | --- |
| `Session(Fact)` | `mokmok.session.fact` | `Fact` | *existing* |
| `Turn(TurnID, TurnFact)` | `mokmok.turn.fact` | `TurnRecord` | *existing* |
| `Result(ExecutionID, ExecutionResult)` | `mokmok.execution.result` | `ResultRecord` | *existing* |
| `Agent(Record)` | `mokmok.agent.record` | `Record` (the runtime's type) | named per variant below |

One kind, not five, because the record vocabulary is the runtime's and belongs in one place: a new
record variant is then added in the crate that owns it, and `mokmok-kernel/src/db.rs` gains one arm.
The kernel does acquire what the records *mean* — `Log::verify` and `Journal::transcript` both run
the runtime's `Record::apply` — and what it does not acquire is a second, parallel definition of
them. The cost, accepted: an unknown **tag** fails the decode of that payload, which stops the read
exactly as an unknown **kind** does today (`Error::Payload` against `Error::UnexpectedEntry`), so a
future record variant is a finer-grained upgrade hazard than a kind per record type would be.

```rust
// mokmok-agent-runtime/src/record.rs
#[serde(tag = "record", rename_all = "snake_case")]
pub enum Record {
    Task(TaskRecord),                     // the goal and the setting: written once, first, idempotent
    Prompt(PromptRecord),                 // turn, attempt, the exact Context bytes the model was handed
    Answer(AnswerRecord),                 // turn, attempt, the parsed action, the raw text, usage
    Call(CallRecord),                     // turn, execution, the command about to be dispatched
}
pub struct TaskRecord { pub task: Task }
pub struct PromptRecord { pub turn: TurnID, pub attempt: Attempt, pub context: ArtifactRef }
pub struct AnswerRecord { pub turn: TurnID, pub attempt: Attempt, pub action: Action,
                          pub text: ArtifactRef, pub model: String, pub usage: Usage }
pub struct CallRecord { pub turn: TurnID, pub execution: ExecutionID, pub command: String }
/// The unit of durable commitment. `StepId` is derived, never minted per invocation, so an
/// invocation that repeats a move replays it instead of writing it twice.
pub struct Step { pub id: StepId, pub session: SessionID, pub records: Vec<Record> }
pub enum Receipt { Written, Replayed }
```

`StepId` is frozen as `uuid5(session, "{tag}:{n}")` where `tag` is `task`, `ask-intent`,
`ask-outcome`, `act-intent`, `act-outcome`, or `close`, `n` is the turn number, and `ask-*` carries
the attempt: `uuid5(session, "ask-intent:{n}:{attempt}")`. Two invocations that reach the same move
on the same session therefore derive the same id and the second append replays.

The tool's **result** rides the agent's own kind, `mokmok.agent.record`, as
`Record::Result { execution, report }`. The existing `mokmok.execution.result` kind stays with the
`submit` slice, so a slice-era log reads unchanged and no old payload shape moves. The report
carries the bounded rendering the model was shown, produced by whoever held the bytes; the bytes
themselves stay in the artifact store behind the two references:

- `Finished{code}` / `Killed{signal}` / `TimedOut` / `Refused` with a result: the exit code and the
  head-and-tail-clipped stdout and stderr, the full bytes staying addressable by hash;
- `Unknown`: the constant warning that the runner could not prove the outcome, that the checkout may
  have been changed, and that the effect must not be assumed. `Resolution::Unknown` never has a
  result (`Fact::ResultApplied` for it is `Rejection::NoResult`), so this arm is a message about a
  fact, not a rendering of bytes. It is what makes DoD 3 falsifiable: the warning is in the next
  turn's durable `Prompt` blob.

`Record::apply` is the fold, and it refuses two things, each with a type rather than a comment:

- a **content conflict**: a second `Prompt` under one `(turn, attempt)`, `Answer` under one
  `(turn, attempt)`, or `Call` under one `execution`, with different bytes;
- a record **floating free of its machine fact**, each tied to the fact that admits it: a `Prompt`
  needs its `ModelRequested`, an `Answer` needs its `ModelAnswered`, and a `Call` needs its
  `ToolRequested`, in the fold or in the same step. The prompt is committed *before* the call it
  precedes, so it is tied to the request and not to the answer.

Identical content under one key is `Ack::Duplicate`, the discipline the machines already hold.

### The ports

```rust
// The runtime's own types on every port: no Entry, Kind, Payload, Version, CommandId, Plan,
// Request, Exposure, or HTTP type crosses this line.
pub trait Journal {
    fn transcript(&self, session: SessionID) -> Result<Transcript, Error>;
    fn bytes(&self, content: &ArtifactRef) -> Result<Vec<u8>, Error>;
    /// The bytes before the reference that names them, always: a crash leaves an unreferenced blob
    /// the collector takes, never a reference to bytes that were never written.
    fn put(&mut self, bytes: &[u8], truncated: bool) -> Result<ArtifactRef, Error>;
    /// One transaction, every record in order or none, with the version the read saw as the
    /// expectation so a concurrent writer is `Conflicted` rather than an overwritten stream.
    fn append(&mut self, step: Step) -> Result<Receipt, Error>;
}
pub trait Provider: Sync {
    /// Encodes the context, calls the vendor, parses back the answer text, the one action, and the
    /// token counts. Everything else is discarded at this boundary.
    fn complete(&self, context: &Context) -> impl Future<Output = Result<Answer, Error>> + Send;
}
pub trait Tools: Sync {
    /// Decides authority, confines, runs, stores the bytes, reports in the runtime's vocabulary.
    /// A caller cannot name an exposure or a program: the operator's grants are the whole set.
    fn invoke(&self, call: &ToolCall) -> impl Future<Output = Result<ToolReport, Error>> + Send;
}
```

### The provider's wire, as measured

`.audit/round-2-arena/probe.md` has the three responses. The adapter sends the OpenAI-compatible
shape the endpoint was measured to accept, and this is frozen:

- request: `{"model":…, "messages":[…], "tools":[{"type":"function","function":{"name":"run_shell",
  "description":…, "parameters":{"type":"object","properties":{"command":{"type":"string"}},
  "required":["command"]}}}], "max_tokens":…, "stream":false}`;
- response parsing: `choices[0].message.tool_calls[0].function.{name,arguments}` where `arguments` is
  a JSON **string** to be decoded, `choices[0].message.content` for the final answer,
  `finish_reason` to pick between them, and `usage.{prompt_tokens,completion_tokens}`;
- discarded at the boundary: HTTP status text and headers, `id`/`object`/`created`/
  `system_fingerprint`, `message.reasoning` (the vendor's extra field, present on every measured
  response), logprobs, and every vendor tool-call wrapper. A malformed `arguments` string is
  `Action::Malformed { reason }`, recorded, never silently retried.

`Answer` carries `Action::{Final, Command, Malformed}`; the command *string* lives only in
`CallRecord`, so the answer record says the model asked for the tool and the call record says what
was dispatched, with no duplicated bytes. `Budget::attempts` (default 3) caps the re-asks of one
turn; the run then closes the turn `Failed` and halts with `Halt::Attempts`.

### The loop

```rust
/// A total decision over the fold. Pure: no clock, no I/O, no wire. Re-folding from scratch is
/// invariant under it, which is the property the tests pin.
pub enum Move {
    Begin { task: Task },
    Ask { turn: TurnID, number: TurnNumber, attempt: Attempt, context: Context },
    Act { turn: TurnID, execution: ExecutionID, call: ToolCall },
    Close { turn: TurnID, outcome: TurnOutcome, ending: Option<Ending> },
    Halt(Halt),
}
pub enum Halt { Closed(Ending), Draining, Budget, Tokens, Attempts }
#[must_use] pub fn next(transcript: &Transcript) -> Move;

pub enum Progress { Advanced(Move), Yielded, Terminal(Ending) }
pub async fn advance(journal: &mut impl Journal, provider: &impl Provider, tools: &impl Tools,
                     session: SessionID, at: Moment) -> Result<Progress, Error>;
```

`Move::Close` is the one way a session ends: `ending` is `Some` only on the move that writes
`Fact::SessionClosed`, so a settled last turn and a closed session are two invocations of the same
move rather than three overlapping variants. `Progress::Terminal` is what `next` says has already
happened, `Advanced` is a move this step performed, `Yielded` is a move whose intent replayed.

Field sketch so a delegate can start:

```rust
pub struct Task { pub goal: String, pub setting: Setting, pub budget: Budget }
pub struct Setting { pub checkout: PathBuf, pub readable: Vec<PathBuf>, pub writable: Vec<PathBuf>,
                     pub program: PathBuf }
pub struct Budget { pub turns: u32, pub attempts: u32, pub tokens: u64, pub observation_bytes: u64,
                    pub context_turns: u32, pub context_bytes: u64 }
pub struct Context { pub system: String, pub turns: Vec<ContextTurn> }
pub struct ContextTurn { pub number: TurnNumber, pub command: String, pub observation: String }
pub enum Action { Final, Command, Malformed { reason: String } }
pub struct Answer { pub action: Action, pub text: ArtifactRef, pub model: String, pub usage: Usage }
pub struct Usage { pub input: u64, pub output: u64 }
pub struct ToolCall { pub execution: ExecutionID, pub command: String, pub deadline: Duration }
pub struct ToolReport { pub exit: Exit, pub attempted: bool, pub stdout: ArtifactRef,
                        pub stderr: ArtifactRef }
pub enum Exit { Finished { code: i32 }, Killed { signal: i32 }, TimedOut, Refused, Unknown }
```

`Context` carries no attempt number, no step id, and no clock, so a re-ask after a crash rebuilds
byte-identical prompt content.

`advance` is one bounded step, and every step obeys one rule: **only the writer of an intent may
perform its effect.** It appends the intent, and dispatches only on `Receipt::Written`; a
`Replayed` intent yields instead, so a second resume cannot execute a tool twice. This is the round's
load-bearing mechanism and it is a return type, not a comment.

Two transactions per effectful move, and each is idempotent by derivation:

| move | intent (before the effect) | effect | outcome |
| --- | --- | --- | --- |
| `Begin` | `Task` | none | — |
| `Ask` | `Prompt` (bytes first), `TurnOpened` if the turn is new, `ModelRequested{attempt}` | the provider call | `ModelAnswered`, `Answer` |
| `Act` | `Call`, `ToolRequested{execution, Bounded}` | the confined shell | `ExecutionSettled`, `ResultApplied` when finished, `Result`, `ToolResolved`, `TurnSettled` |
| `Close` | none | none | `TurnSettled`, `SessionClosed` |

`TurnID` and `ExecutionID` are derived, never minted from the clock: `TurnID = uuid5(session,
"turn:{n}")` and `ExecutionID = uuid5(turn, "tool")`, so a resumed `Act` reconstructs the same
identities and the append replays. `Turn::answer`, `Turn::resolve`, and `Fact::ResultApplied`
returning `Duplicate` are the second wall; `Workspace::create`'s `AlreadyExists` is **not** a wall,
because `execution::run` calls `completed.cleanup()` before the settlement commits
(`mokmok-kernel/src/execution.rs:149`), so a re-dispatch would find the directory gone. The
`Receipt` rule is what has to hold, and the `Replayed` test asserts it directly.

### The model call and the tool call fail differently

This distinction is the durability semantics of the whole round, so it is stated as a rule and not
left to the adapter.

- **A model call is read-only, so an interrupted one is re-asked.** The prompt bytes are durable and
  the context carries no attempt identity, so the re-ask sends identical bytes under a new
  `Attempt`, and the attempt number is recorded. After `Budget::attempts` the turn closes `Failed`
  and the run halts with `Halt::Attempts`. A provider error is never a session failure: it is an
  attempt, recorded.
- **A tool call may have changed the world, so an interrupted one is never re-dispatched.** Recovery
  settles it `Resolution::Unknown` (`recovery::recover` already writes exactly that), and the
  derived observation for that resolution is the warning the next `Prompt` carries. The run does not
  halt on it: the model is the reconciler, it is told the outcome is unproven, and whatever it does
  next is its own recorded decision in a new turn. The runner's only rule is that it never re-runs
  the command itself.

A consequence worth stating: `Provider`'s error carries no `NotSent`/`Ambiguous` split. An
in-process read-only call's ambiguity changes no decision here; the attempt number and the cap are
the whole policy. Adding the split would be a signal with no consumer.

### The agent's surface is one tool, and the grants are the allowlist

`ToolCall { execution, command, deadline }` becomes `argv = [program, "-c", command]` where
`program` comes from `--allow-tool`. The model never chooses a program, so it cannot widen its
authority through `may_invoke`.

**What `--allow-tool` does not mean, stated because the operator's mental model will get it wrong:**
the authority is consulted once per execution for the fixed `program`, and the model's `sh -c`
string then names whatever `PATH` and the granted directories contain. `--allow-tool /bin/sh` is not
a program allowlist. The **path grants are the allowlist**: `--allow-path` (read and execute),
`--allow-write` (read, write, execute), and `--tool-env PATH` decide what the shell can reach. The
system prompt tells the model both facts, and the round's named limits repeat them.

The checkout is reached the only way the sandbox allows: the operator grants it with `--allow-write`,
the kernel refuses a `--checkout` that no write grant covers, and every spawn passes it as an
exposure with a covering capability. The command reaches it by `cd`-ing inside its own shell,
because the sandbox's cwd is a fresh ephemeral `work/` per execution. `stdin` is `/dev/null`, so a
command that reads stdin uses a here-string in its own shell. The system prompt states the facts the
model cannot infer: the cwd is a fresh empty directory, stdin is empty, **TCP is unavailable**, and
the checkout is reached by absolute path.

### What the environment may hold

`Request::env` today is *added* to the inherited environment and nothing clears it
(`mokmok-sandbox/src/sandbox.rs:327`), so a confined workload sees whatever the daemon's process
holds. `Request` gains `environment: Environment` with `Extend` (today's behaviour, the default) and
`Exact`, which clears the environment and sets exactly what it names. The kernel's tool path passes
`Exact`, whose variables are `PATH` taken from the daemon's own environment and every `--tool-env
NAME=VALUE` the operator named, and nothing else. So a model-driven shell sees a deliberate PATH and
the operator's explicit additions.

The provider credential does not depend on this: it is read from a `0600` file named by
`--provider-credential-file`, never from the process environment, so it is in no child's environment
to inherit. Unit 1's test must prove the rule on a process that *does* hold such a variable: it
spawns a confined workload from a parent whose environment carries the marker, and asserts the
marker is absent in the workload and `PATH` is present. A test that only checks an unset variable
proves nothing.

### The network: TCP is closed in unit 2, UDP is a named residual

`mokmok-sandbox`'s ruleset handles `AccessFs::from_all(ABI::V3)`
(`mokmok-sandbox/src/plan.rs:116`), and the `landlock` 0.4.7 crate's TCP rights live in `AccessNet`
at `ABI::V4`. So today **no** part of the sandbox mediates `connect()`: a confined workload may dial
any host, and `mokmok-egress` is not wired into the sandbox path. For a model-driven shell that is
the largest hole in the round's threat model, so unit 2 closes what Landlock can close:
`AccessNet::from_all(ABI::V4)` handled with **no** net rules denies TCP bind and connect to every
workload. `AccessNet` has no UDP right and no `AF_UNIX` right, so UDP to a literal address remains
reachable, and the prompt says "there is no TCP network" rather than "there is no network". Closing
UDP needs a seccomp filter, which is a later round.

The provider is unaffected by that ruleset: it is the kernel's own in-process call, not a workload.

### The provider's destination

The provider is trusted configuration, not a capability. Its destination is validated through
`mokmok-egress` before any socket exists: `Rules::public_only()`, a `PolicyClient` implemented in
`provider.rs` that permits only the configured `host:port`, the existing
`mokmok_egress::Resolver` for the lookup, and an `AuditSink` that writes through
`mokmok-observability`'s audit. The validated `SocketAddr` list is then **pinned** into the HTTP
client (reqwest's `ClientBuilder::resolve`), so the address connected to is one validation accepted,
and the audit row names the decision. `Validated::connect` returns a plain `TcpStream` and is not the
path used, because the wire is TLS.

`CallContext` wants an `ExecutionID` a model call does not have, so the model call mints one for the
audit: `ExecutionID = uuid5(turn, "model:{attempt}")`. It is an audit identity, not a session fact,
and no machine records it.

## The units

Each unit is one PR in a linear stack, each ends in a check, and the sequence is ordered so the
foundations the agent stands on land before the agent. Stacked children are repaired by
`git merge origin/main` after a squash, per the round-1 records.

| # | unit | ends with |
| --- | --- | --- |
| 0 | this brief, the arena archive, and the trail rows, in `.audit/` | a reviewer reads the frozen interface; the architecture doc's tracking status is decided |
| 1 | `Request::environment`, `Extend`/`Exact`, the kernel migrating to `Exact(PATH + --tool-env)` in the same PR | `nix flake check`; the marker test above; the rfc3339 example still prints a timestamp |
| 2 | no TCP for a workload: `AccessNet::from_all(ABI::V4)` handled with no rules, the kernel floor measured first | a confined workload cannot dial TCP, measured with a test helper binary that tries; the sandbox refuses rather than silently downgrading |
| 3 | the runtime's pure core: `Record`, `Transcript`, `apply`, `Task`, `context`, `observation`, `Move`, `next`, `StepId`, `advance` over the ports | property tests (re-fold invariance, no dispatch under `Replayed`), the tie rules, the prompt builder, the `Unknown` warning, and `advance` over an in-crate journal and scripted provider |
| 4 | the kernel adapters and the CLI: `Entry::Agent`, `Log`, `Journal`, `Tools`, `Scripted`, `agent run`, `agent cancel`, `--steps`, the crash points | the whole loop under `nix flake check`, the crash/resume test, and the cancel test |
| 5 | the real provider: rustls + a TLS connector first (proving the new dependency builds under the gate), then the OpenAI-compatible adapter, the egress validation and pin, the credential file, the parse fixtures | the parse fixtures in the gate, and a live run |
| 6 | the operator harness and the deploy note: `examples/coding-agent/run.sh` with its three assertions, the crash variant, `--allow-*` and credential-file guidance for the unit and the module | the predicate on the real artifact, with the real model |
| 7 | grow: three recorded tasks in a fixed scratch checkout, each with its own assertion, scored as tasks-passing and tokens-spent against the round's baseline | a before/after table on the real tasks; stop after two iterations or when all three pass; a second tool only if the table asks for one |

Verification per unit: the gate (`nix flake check`, and `scripts/check.sh` for fast feedback), the
mutation gate on the touched package where it can score it, an independent verifier per PR that did
not write it and posts a verdict on the PR, and for units 4-6 the crash injection and the live
harness. `mokmok-kernel` still cannot be scored by `scripts/mutants.sh` (its tests spawn another
package's binary); unit 3's pure core is the scorable half of this round.

## Named limits and open decisions

- **The provider is trusted configuration, not a capability.** `mokmok-capability` already has
  `Resource::Host` and `Resource::Model`; what is missing is a grant path for them. Putting the
  provider under the same authority as the tools is a separate round.
- **UDP is not closed by unit 2** (see above). Neither is `AF_UNIX`, and the sandbox's own
  documented limit stands: a workload that calls `setsid` itself escapes the process group every
  signal reaches.
- **The `Receipt::Replayed` guard is proven at the port and by the command-id bridge, not end to end
  through the tool path.** The kernel's `Ledger::open` recovers before the first step, so a tool an
  interrupted run left in flight is settled `Unknown` and never re-dispatched by the loop; the guard
  covers a second invocation racing the first, which no deterministic test drives yet.
- **Unit 7, the growing pass, did not happen.** The agent has run one real task, and the recorded
  task set, its pass/fail score, and the token cost per task are still open. The first three things
  it would measure are the context mapping (one user message carrying the goal and the transcript),
  the clip's size against a real failure, and whether the shell alone is enough.
- **The transcript is byte-exact across an upgrade.** The rendering the model was shown is recorded
  with the result, so a changed clip cannot rewrite what a past turn is said to have seen. The full
  bytes stay addressable by hash, and the prompt that was sent is durable. A settlement with no
  recorded report, which is the shape recovery writes, is the one case rendered from its facts.
- **A budget-exhausted run closes the session `Completed`.** `Ending` has two values and neither
  means "gave up", so `Halt::Budget`/`Halt::Tokens` are reported by the CLI and the session closes
  as completed. Adding an ending is a state-machine change this round does not make.
- **The live provider path is not covered by the gate**, which has no network. Its parse function is
  covered by fixtures and its wire behaviour by the operator harness. If that is judged
  insufficient, the round needs a recorded-response replay harness, which is unit 7 work.
- **`mokmok-event-bus` still has no consumer.** The runtime runs as a `mokmokd` invocation against
  the same data directory, the shape `submit` already has. A long-lived agent service consuming
  `session:<id>` is a later round, and the bus's `Subscriber` is what it would use.

## Landed, and where it differs from this sketch

Eight commits on `feat/agent-runtime` off trunk `ce579e7`, in this order.

| commit | what it is |
| --- | --- |
| `ed6fc7b` | this brief, the arena, and the trail |
| `ac3b584` | the daemon recovers the log before the bus accepts a peer |
| `507a0dc` | `Request::environment`, and the tool path on `Exact(PATH + --tool-env)` |
| `c7797e8` | no TCP for a workload |
| `f7e518e` | the runtime's pure loop |
| `c3d69d1` | the kernel's adapters, the CLI, and the transcript |
| `94432c7` | the real provider, its validated destination, and the credential file |
| `b0491b9` | the operator harness, the deploy note, and the doc fixes |

A ninth commit, `372d85b`, closes what a final audit of the round found: the redirect policy, the
credential's mode and its write grant, the default ceilings, the single-tool rule, the scripted
refusal test, and the corrections to this record. It changes nothing outside `mokmok-kernel/` and
this directory.

The definition of done, item by item, and what proves it:

1. The gate runs the whole loop with a scripted provider and no network, and proves the resume
   neither re-executes a tool nor claims an unproven outcome:
   `mokmok-kernel/tests/agent.rs::a_crash_before_dispatch_resumes_without_running_the_command`,
   where the effect is measured by the file the command would have written and the warning is read
   back out of the artifact store by hash.
2. `examples/coding-agent/run.sh` against the live endpoint: the model wrote `fib.sh`, the sandbox
   ran it, `out.txt` holds the ten values the goal named, and the run closed `Completed`. Run on
   2026-10-13 with `kimi-k2.7-code` through `https://ollama.com/v1`.
3. The same test as item 1, second assertion: the next turn's durable prompt carries
   `UNKNOWN_WARNING` and still names the command.
4. `mokmokd status --session` replays the session as its transcript: the task, each prompt with its
   content hash, each answer with its action, each call with its command, each result with its
   exit and the observation the model saw.

Deviations from the sketch above, each with its reason:

- **`Record` is the whole vocabulary, not four records beside the kernel's entries.** One step must
  drive both machines and the result, and the fold's tie rule is "in the fold or in the same step",
  so the runtime's `Record` carries `Session`, `Turn`, and `Result` as well as `Task`, `Prompt`,
  `Answer`, and `Call`. The kind count is what this brief froze: one new kind,
  `mokmok.agent.record`, and `Entry::Result` is still skipped when reading, so a slice-era log reads
  unchanged.
- **The command rides both the answer and the call**, and a `Call` whose command disagrees with the
  answer for its turn is `Refusal::CommandMismatch`. The brief's "the command string lives only in
  `CallRecord`" was wrong: a crash between the answer and the call would lose what to dispatch.
- **`Context` gained `goal`.** Measured: a body whose messages are a single system message is
  answered `finish_reason: "load"` with zero tokens and no tool call, so the adapter now always
  sends a user message carrying the goal, with the settled turns appended to it.
- **The result record carries the bounded rendering, not the bytes** (`ToolReport.observation`).
  `ToolResult` and `Journal::bytes` are gone. The bytes stay in the artifact store behind their
  references, which is what the kernel's own note requires, and the transcript is exact forever.
- **`Setting` gained `deadline`**, wired from `--tool-deadline-seconds` (default 120 s), because the
  task is durable and the deadline is a property of the task.
- **The durability bridge is the command id.** `Journal::append` commits each step under
  `CommandId::from_uuid(step.id.as_uuid())`, so the event store's own dedup answers a repeated step
  with `Outcome::Replayed` and the port answers `Receipt::Replayed`. The brief left this implicit,
  and an independent verifier named it as the one thing that made the durability claim unproven.
- **`Close` derives two ids**, `close:{n}` for the turn's settlement and `close:{n}:session` for the
  session's close, because the brief's one key made the second a replay and the session never closed.
- **The provider flags live on the global `Args` and on `Config`** rather than on `Agent::Run`,
  because the variant crossed clippy's `large_enum_variant` threshold and clap cannot box it. Only
  the credential *path* is carried; the secret is read at open into a type whose `Debug` is
  `REDACTED`.
- **HTTP is allowed only for a loopback host**, which takes the rules that admit
  `AddressClass::Loopback`. The brief promised a local endpoint and did not say how.
- **The audit identity is `uuid5(session, "model")`**, not per turn and attempt: the destination is
  validated and pinned when the provider opens, before any turn exists. `Validated::new` stamps its
  audit record with `SystemTime::now()`, which is the one wall-clock read outside `mokmokd.rs`.
- **The journal and the tool are two adapters**, `Ledger` and `Shell`. `Db` is not `Sync` and the
  runtime's `Tools` port requires it, so one value cannot be both.
- `status` prints a `task` line as well as the four the brief named.


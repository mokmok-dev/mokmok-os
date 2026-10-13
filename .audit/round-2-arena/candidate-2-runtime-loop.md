# `mokmok-agent-runtime` — one bounded step of a coding agent

## Usage (caller's view)

```
$ mokmokd --data-dir /var/lib/mokmok --allow-tool /bin/sh --allow-path /nix/store \
    --allow-write /var/lib/mokmok/checkout agent begin --session 018f… --max-turns 32 \
    --checkout /var/lib/mokmok/checkout --goal "make \`cargo test\` pass"
session=018f… step=begin task=sha256:9c1f… turn=018f…
$ mokmokd … --provider ollama --model kimi-k2.7-code agent step --session 018f…
move=ask turn=018f… number=1 attempt=1 provider=answered bytes=612
$ mokmokd … agent step --session 018f…
move=act turn=018f… execution=018f… command="cd /var/lib/mokmok/checkout && cargo test"
resolution=Finished{code:101} stdout=sha256:44a0… bytes=8123
$ mokmokd … agent step --session 018f…  # killed here; re-running resumes at the same move
move=finish turn=018f… ending=Completed
```

A step never spans a session: `agent run --steps 64` is that loop repeated, and the systemd unit is
`Type=oneshot`, so a restart between any two steps resumes. Two real call sites:

```rust
// 1. kernel/src/bin/mokmokd.rs — the one clock reading in the workspace, and the whole CLI.
Command::Agent(Agent::Step { session, .. }) => {
    let (mut journal, provider, tools) = open_adapters(&config)?;   // Db+BlobStore, Ollama, Confined
    print_progress(&advance(&mut journal, &provider, &tools, session(*session)?, now()).await?);
}

// 2. kernel/tests/agent.rs — a scripted provider runs one step; the process dies between the
//    intent and the outcome; the next invocation must not run the command again.
let mut journal = Journal::open(&config)?;
advance(&mut journal, &Scripted::of(&["shell {\"command\":\"id\"}"]), &tools, s, t).await?;
drop(journal);                                              // the crash: nothing else durable
assert!(matches!(advance(&mut Journal::open(&config)?, &Scripted::of(&[]), &tools, s, t).await?,
                 Progress::Advanced(Move::Unknown { .. })));
```

## Shape — the type sketch

```text
  content.rs Content, Clip          record.rs  Step, StepId, Record, Receipt, the content records
  task.rs    Task, Setting, Budget  dialogue.rs Transcript, Refusal, Record::apply
  context.rs Context, bounds        model.rs   Provider, Answer    tool.rs  Tools, ToolCall, Report
  advance.rs Move, next(), advance()            error.rs   Error
```
Private modules hold `pub` types, not `pub(crate)` (`redundant_pub_crate` is denied); every type below is `Clone + Debug + Eq + PartialEq`, durable ones also `serde`.

```rust
/// Bytes outside the log, named by hash: the same wire shape as the kernel's `Blob`, no dependency
/// on it — the adapter converts.
pub struct Content { pub hash: String, pub bytes: u64, pub truncated: bool }
/// What world the model is told it has: descriptive only, since the grant that enforces it is the
/// operator's and no `Policy`, `Rule`, or capability ever reaches the runtime.
pub struct Task { pub goal: String, pub setting: Setting, pub budget: Budget }
pub struct Setting { pub checkout: PathBuf, pub readable: Vec<PathBuf>, pub writable: Vec<PathBuf>,
                     pub program: PathBuf, pub network: bool }
pub struct Budget { pub turns: u32, pub tokens: u64, pub observation_bytes: u64,
                    pub context_turns: u32, pub context_bytes: u64 }

/// One durable record. Each variant has a kernel `Entry` kind of its own (`mokmok.agent.task`,
/// `.prompt`, `.answer`, `.call`, `.observation`), so an older build stops the read rather than
/// guessing and a newer build folds both.
#[serde(tag = "record", rename_all = "snake_case")]
pub enum Record {
    Session(Fact), Turn(TurnID, TurnFact), Result(ExecutionID, ToolReport),   // the three existing
    Task(TaskRecord), Prompt(PromptRecord), Answer(AnswerRecord),
    Call(CallRecord), Observation(ObservationRecord),
}
pub struct TaskRecord { pub task: Task }
pub struct PromptRecord { pub turn: TurnID, pub attempt: Attempt, pub context: Content }
pub struct AnswerRecord { pub turn: TurnID, pub attempt: Attempt, pub text: Content,
                          pub action: Action, pub usage: Usage }
pub struct CallRecord { pub turn: TurnID, pub execution: ExecutionID, pub command: String }
pub struct ObservationRecord { pub turn: TurnID, pub execution: ExecutionID, pub output: Content }
/// The unit of durable commitment. Its id is derived, never minted per invocation, so an invocation
/// that repeats a step replays it instead of writing it twice.
pub struct Step { pub id: StepId, pub session: SessionID, pub records: Vec<Record>, pub notice: Notice }
pub struct StepId(uuid::Uuid);     // uuid_v5(session, move_tag, key): same fold + same move = same id
impl StepId { pub fn derive(session: SessionID, tag: &'static str, key: impl Hash) -> Self { todo!() } }
/// What the journal did with a step. Only the writer of an intent may perform its effect.
pub enum Receipt { Written, Replayed }

/// The session's whole durable past, folded. The driving path and the replay path run the same
/// fold, so they cannot drift.
pub struct Transcript { pub session: Session, pub turns: BTreeMap<TurnID, Turn>, pub task: Option<Task>,
    pub prompts: BTreeMap<(TurnID, Attempt), Content>, pub answers: BTreeMap<TurnID, AnswerRecord>,
    pub calls: BTreeMap<ExecutionID, CallRecord>, pub runs: BTreeMap<ExecutionID, ToolReport>,
    pub observations: BTreeMap<ExecutionID, Content> }
impl Record {
    /// # Errors
    /// [`Refusal::Redelivered`] for a fact a machine refuses; [`Refusal::ContentConflict`] when a
    /// duplicate key carries different bytes: one prompt per (turn, attempt), one answer per turn,
    /// one observation and one result per execution.
    pub fn apply(&self, into: &mut Transcript) -> Result<Ack, Refusal> { todo!() }
}
/// Every type on these ports is the runtime's own; no `Entry`, `Kind`, `Payload`, `Version`,
/// `CommandId`, `Plan`, `Request`, `Exposure`, or HTTP type crosses this line.
pub trait Journal {
    fn transcript(&self, session: SessionID) -> Result<Transcript, Error>;
    fn bytes(&self, content: &Content) -> Result<Vec<u8>, Error>;
    /// Bytes before the reference that names them, always: a crash leaves an unreferenced blob the
    /// collector takes, never a reference to bytes that were not written.
    fn put(&mut self, bytes: &[u8], truncated: bool) -> Result<Content, Error>;
    /// One transaction, every record in order or none. The version the read saw is the expectation,
    /// so a concurrent writer is `Conflicted` rather than an overwritten stream.
    fn append(&mut self, step: Step) -> Result<Receipt, Error>;
}
pub trait Provider: Sync {
    /// Encodes `context` for the vendor, calls it, parses back. Everything but the answer text, the
    /// one tool call, and the token counts is discarded here.
    fn complete(&self, context: &Context) -> impl Future<Output = Result<Answer, Error>> + Send;
}
pub trait Tools: Sync {
    /// Decides authority, confines, runs, stores the bytes, reports in the runtime's vocabulary.
    /// The caller cannot name an exposure; the operator's grants are the set.
    fn invoke(&self, call: &ToolCall) -> impl Future<Output = Result<ToolReport, Error>> + Send;
}
pub struct Context { pub system: String, pub task: Task, pub turns: Vec<ContextTurn> }
pub struct Answer { pub text: Content, pub action: Action, pub usage: Usage }
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Action { Final { text: Content },                       // the agent is done
                  Command { command: String },                   // the one tool
                  Malformed { reason: String, text: Content } }  // recorded, never retried silently
pub struct Usage { pub input: u64, pub output: u64 }
/// One command as the runtime names it: no exposures, no cwd, no sandbox type, because the checkout
/// is reached by the command itself — the workspace is fresh and ephemeral per execution.
pub struct ToolCall { pub execution: ExecutionID, pub command: String, pub deadline: Duration }
pub struct ToolReport { pub exit: Exit, pub attempted: bool, pub stdout: Content, pub stderr: Content }
pub enum Exit { Finished { code: i32 }, Killed { signal: i32 }, TimedOut, Refused, Unknown }

/// A total decision over the fold. Each variant names the turn and execution it acts on.
pub enum Move { Begin { task: Task },
                Ask { turn: Option<TurnID>, number: TurnNumber, attempt: Attempt, context: Context },
                Act { turn: TurnID, execution: ExecutionID, call: ToolCall },
                Note { turn: TurnID, execution: ExecutionID, output: Content },
                Unknown { turn: TurnID, execution: ExecutionID, output: Content },
                Finish { turn: TurnID }, Halt(Halt) }
pub enum Halt { Closed(Ending), Draining, Budget, Cancelled, NothingToDo }
#[must_use] pub fn next(transcript: &Transcript) -> Move { todo!() }
pub enum Progress { Advanced(Move), Yielded, Halted(Halt), Terminal(Ending) }
/// One bounded step: read, decide, and — if the move has an effect — commit its intent, perform it,
/// commit its outcome. Two transactions at most, each idempotent by derivation.
pub async fn advance(journal: &mut impl Journal, provider: &impl Provider, tools: &impl Tools,
                     session: SessionID, at: Moment) -> Result<Progress, Error> {
    // TODO pseudocode, the whole loop:
    //   let t = journal.transcript(session)?; let move_ = next(&t); let step = plan(&t, &move_)?;
    //   match step.effect {
    //     None => { journal.append(step)?; Ok(Progress::Advanced(move_)) }
    //     Some(effect) => {
    //       if journal.append(step.intent)? == Receipt::Replayed { return Ok(Progress::Yielded); }
    //       let observed = effect(journal, provider, tools, &move_).await?;
    //       journal.append(step.outcome(observed))?; Ok(Progress::Advanced(move_))
    //     }
    //   }
    unimplemented!("the journal, provider, and tool ports do the work")
}
pub enum Error { Journal(#[source] Source), Provider(#[source] Source), Tool(#[source] Source),
                 Refused(Refusal), Rejected(#[from] Rejected) }
pub enum Refusal { ContentConflict { key: String }, Redelivered(Rejection) }
```

### The load-bearing decisions

**A turn is one model round trip, so a step is one effect.** `Turn` already admits many
`ModelRequested{attempt}` and exactly one `ModelAnswered`, refuses a tool before the answer, and
refuses everything after `TurnSettled`. That is a step machine, and the mandate needs no new fact:
`mokmok-state-machine` is **unchanged**. `next()` walks the fold — unopened → `Begin`; newest turn
unanswered → `Ask` (attempt = max recorded + 1, or the existing turn on a retry); answered with a
command and no call → `Act`; answered with a final text → `Finish`; `ToolResolved` with no
observation → `Note`; a call with no resolution → `Unknown`; closed, draining, or budget-spent →
`Halt`. Total, pure, no I/O, no clock.

**Intent before effect, and the intent is what a resumer reads.** `Ask` commits `TurnOpened{number}`
(a duplicate on retry), `ModelRequested{attempt}`, and `Prompt{… context}`; only then is the model
called. `Act` commits `ToolRequested{execution, Bounded}` and a `CallRecord`; only then is the tool
run. The outcome is a *second* transaction under its own derived `StepId`: one command id is one
transaction, and reusing the intent's would make the outcome a replay of the intent. **Here the
mandate strains.** "Resumable at every step boundary" is really resumable at every *durable commit*
boundary: a step is not atomic, and the window between its two commits is the one place a process
holds a session. A step is therefore bounded by the sandbox deadline or the provider timeout, not by
anything the loop controls, and the runtime's persistence port is broader than a journal — it also
reads and writes bytes (`put`/`bytes`), because a resumer must re-send and re-read content by
reference.

**Only the writer of an intent performs its effect.** `append` returns `Receipt::{Written,
Replayed}` and `advance` dispatches only on `Written`. A second process that resumed the same fold
derives the same `StepId`, replays, and yields. That rule makes "a second resume cannot execute a
tool twice" a property of a type rather than a comment. It also means execution identity is derived
from the requesting step: a re-run names the same execution, and an unresolved execution is never
re-dispatched, so the sandbox never meets a reused `ExecutionID` (`Workspace::create` would refuse
`AlreadyExists` anyway). A crashed tool becomes `ExecutionSettled{Unknown}` + `ToolResolved{Unknown}`
+ `TurnSettled{Failed}` plus an observation saying *"the runner could not prove this command's
outcome; do not assume it took effect"*. Re-running it is then the model's explicit, recorded
decision in a new turn, never the runner's blind one — and that is exactly what `recovery::recover`
writes, so the daemon's startup pass and the runtime's `Unknown` move converge on identical facts.

**The provider boundary hides the wire, and the durable prompt is the domain context.** `Context` is
the runtime's canonical bytes; `complete` maps it to the vendor's request and parses back only the
answer text, one tool call, and the token counts. Discarded: HTTP status and headers, chunked/SSE
framing, `id`/`created`/`object`, model metadata, logprobs, reasoning deltas, and every vendor
tool-call envelope. What is recorded is `Context`, never vendor JSON: an append-only log must not
carry a vendor schema forever. A pending call is re-dispatched from the *stored* bytes, so a new
prompt builder cannot change what an in-flight attempt sends.

**Content addressing is the duplicate rule.** `PromptRecord` is keyed `(turn, attempt)`,
`AnswerRecord` by turn, `ObservationRecord` and `Result` by execution. A second record under a key
with identical bytes is an `Ack::Duplicate`; with different bytes it is `ContentConflict` and
nothing is written — the content-keyed discipline the machines already state.

**The tool surface is one tool, and the operator owns it.** The model is offered `shell` and nothing
else: `ToolCall { command }` becomes `argv = [program, "-c", command]` with `program` from
`--allow-tool`. The model never names a program, a privilege, or an exposure; the authority
translation is the untouched `execution::run`. The persistent checkout is a **capability, not a
workspace**: cwd stays the sandbox's fresh ephemeral `work/` and the command reaches the checkout by
`cd`-ing inside a shell the operator granted. `stdin` is `/dev/null`, so a command that reads stdin
does `printf … | cmd` in its own shell.

**Context is bounded and its bounds are in the digest.** The prompt is the setting, the goal, and
the last `budget.context_turns` settled turns, each rendered as the command and its observation
clipped head-and-tail to `budget.observation_bytes` with a deterministic `… N bytes elided …`
marker; total bytes capped at `budget.context_bytes`, oldest turns collapsing to one deterministic
summary line. `Limits::output` bounds captured bytes, the clip bounds what the model sees, and the
full blob stays addressable by hash. Budget is derived from the fold (`turns_in_order().len()`, Σ
`usage`), never a separate counter.

**Interface depth.** Three ports, one function, ~20 types. Hidden: the ledger's commands, versions,
expectations, kinds, and payloads; the artifact store's paths and hashes; the sandbox's plans,
requests, exposures, issuance, and cleanup; the authority's policy and grants; the outbox and bus
topic; the vendor's HTTP client and JSON; the identity derivation; the conflict rule; the
unknown-outcome policy. No pass-through method: `complete` hides encode → dial → parse, `invoke`
hides decide → confine → run → store → translate, `append` hides version check, dedup, outbox, kind.

**Where it lands.** `mokmok-agent-runtime` gains `uuid`'s `v5` feature and nothing else — no
event-store, sandbox, artifact, HTTP, or kernel. `mokmok-kernel` gains `mokmok-agent-runtime`, three
adapters (`journal.rs`, `provider.rs`, `tools.rs`), the `agent` subcommands, four new `CrashPoint`
variants, and one HTTP client (`reqwest` with `rustls-tls`, no default features) for `ollama`.
`mokmok-event-store`, `mokmok-artifact`, `mokmok-protocol`, and `mokmok-state-machine` are
**unchanged**: `Kind::new` accepts any non-empty name, so the five new kinds need no store change.
Old logs replay, and a slice-era session with an unresolved execution reads as `Move::Unknown`.

## Synthesis decision

*(filled by the arena)*

## Problem

`slice.rs` proves the kernel's eight steps for one operator-named tool and fabricates the model: the
"model" is the command line, answering by requesting the tool the operator already named. What must
keep holding: the pure `Copy`-fact machines key every decision on content, the log's kinds are
append-only and stop an unknown read by design, the sandbox has no workspace API and per-execution
ephemerality, the checkout is reachable only through a named exposure, `TurnFact` carries no
content, the gate has no network, and `unwrap`/`todo`/`panic` are denied outside tests. The
non-obvious consequence: a multi-step agent must be many turns, and the model's answer and the
tool's arguments — the only things the loop is *about* — have nowhere to live in the machines, so
they become content records beside them.

## Tradeoffs accepted

- Two transactions per effectful step, in exchange for an intent a resumer can act on.
- Re-folding the whole stream per step (O(n) per step, O(n²) per session), for never having a derived
  cache to invalidate; `Append::state` is the deferred fix.
- A single text-protocol tool call rather than a structured schema, for a provider boundary with no
  vendor shape in it and a fake provider that is exact.
- `SessionClosed{Completed}` for a budget-exhausted or malformed-outcome run, for not adding an
  `Ending::Failed` the machines do not have.
- A new `mokmok-kernel → mokmok-agent-runtime` edge (the port-adapter direction the architecture's
  table allows), for the content vocabulary living in one crate.

## Alternatives considered

- **The loop in the kernel, content as new kernel `Entry` kinds.** Simplest wiring, and the kernel
  already owns the log. It loses on interface depth: the model client, prompt builder, tool-call
  parse, and decision table land in the composition root, which the architecture says decides
  nothing itself, and every caller must import log vocabulary to test its own loop.
- **The model call as a confined execution** (credential in a workspace, answer as an artifact). It
  hides the wire behind the sandbox and inherits durability, but exposes a program-plus-credential
  protocol to the operator and puts a secret in a tree an untrusted process writes. It makes the
  boundary *less* deep: the wire reappears as a file format the operator must author.

## Open questions and risks

- Is one `shell` tool enough, or does reliability need structured `read`/`edit` tools — expressible
  only as a second program under `--allow-tool` plus a path argument, since the workspace is ephemeral?
- Should the provider run in the kernel's process (so it may hold `OLLAMA_API_KEY`) or inside the
  sandbox (so it cannot leak)? Kernel-side is architected-legal — egress mediates *workload* traffic
  — but it puts a credential next to a loop that relays model output.
- The new kinds make an older binary stop the read of a newer log (`UnexpectedEntry`, by design).
  Does the fleet need a version-tolerant read, or an atomic upgrade?
- The live provider is untested by the gate (no network); only its parse function is. Are recorded
  fixtures plus a manual `MOKMOK_LIVE=1` run enough evidence for "safe mid-turn recovery and
  explicit unknown outcomes"?
- `TurnSettled{Failed}` plus `Ending::Completed` is how a dead-end run reads. Does a reader want a
  failed ending the machines do not have, or is a failed turn enough?

## Next implementation step

Write `next`/`advance` and the three ports against an in-crate in-memory `Journal` and a scripted
`Provider`, with the two property tests that make the mandate checkable — `next(fold(records))` is
unchanged by re-folding from scratch, and no `ExecutionID` is ever dispatched under a
`Receipt::Replayed` intent — then wire `kernel/src/{journal,provider,tools}.rs` and `agent`.

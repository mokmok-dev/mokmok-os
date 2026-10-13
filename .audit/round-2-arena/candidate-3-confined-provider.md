# Candidate 3 — the model call is a confined execution

## Problem

The vertical slice records a turn, runs one operator-named tool under Landlock, and settles it (`mokmok-kernel/src/slice.rs`), but its "model" is a doc comment: the answer is fabricated. The kernel already has everything a real provider needs — a durable launch→settle transaction, a capability gate, a per-execution ephemeral workspace, rlimits, a deadline watchdog, an artifact store, and recovery that records `Resolution::Unknown` — and none of it reaches the model. The non-obvious constraint is that the sandbox owns the workspace *and the environment* it spawns: a provider is a program the kernel runs like any tool, and the only place its request body can live is a file inside the ephemeral `work/`, because neither `argv` nor `env` can carry a coding agent's context and `stdin` is `/dev/null`. `Turn` admits one `ModelAnswered` per turn and gates tools on it (`mokmok-state-machine/src/turn.rs`), so a *turn is one model round trip* and an agent is many turns; a `Copy` fact cannot hold the answer, so it rides the existing `Entry::Result` reference.

## Usage (caller's view)

One session runs to completion; a killed run is continued by re-running the same command, and the log decides what happens next.

```console
$ export MOKMOK_PROVIDER_CREDENTIAL_FILE=/run/secrets/ollama   # read once at startup, 0600
$ mokmokd --data-dir /var/lib/mokmok \
    --allow-tool /bin/sh --allow-tool /usr/local/bin/mokmok-provider-ollama \
    --allow-write /srv/checkout --allow-path /nix/store --allow-path /etc/ssl/certs \
    agent --session 018f… --checkout /srv/checkout --max-turns 40 \
          --task "Make the failing test in src/parse.rs pass; touch nothing else." \
          --provider-program /usr/local/bin/mokmok-provider-ollama --provider-model kimi-k2.7-code
turn=1 number=1 model=018f…a1 resolution=Finished { code: 0 } answer=run
turn=1 tool=018f…a2  resolution=Finished { code: 1 } stdout=… stderr=…
turn=6 number=6 model=018f…b1 resolution=Finished { code: 0 } answer=finish
stop=Done turns=6 phase=Closed(Completed)
$ mokmokd … agent --session 018f…     # re-run after kill -9: resumes from the log
halted execution=018f…a2 resolution=Unknown; the checkout may be edited — run `reconcile`
```

Call site 1 — the daemon's handler, `mokmok-kernel/src/bin/mokmokd.rs`:

```rust
Command::Agent(run) => {
    let mut journal = mokmok_kernel::agent::Kernel::open(&config, run, at)?;
    let report = mokmok_agent_runtime::run(&mut journal, run.request(at)?).await?;
    print_report(&report);
},
```

Call site 2 — the whole loop under the gate, no network, a fake provider (`/bin/sh` writing a canned `answer.json`), `mokmok-kernel/tests/agent.rs`:

```rust
let report = agent(&scratch, "--provider-program /bin/sh -c 'cp canned.json answer.json'").await;
assert_eq!(report.turns, 6);
assert_eq!(log.turns_in_order().len(), 6);   // one model round trip per turn
assert_eq!(log.results.len(), 12);           // six model executions, six tool executions
assert_eq!(status(&scratch).phase, Phase::Closed(Ending::Completed));
```

## Shape

### Module map

```
mokmok-agent-runtime   (pure: protocol + state-machine + serde; no I/O, no sandbox)
  brief.rs    Brief — the task and the directory the agent works in
  answer.rs   Answer, ToolCall, Malformed, parse_answer — the provider boundary parser
  context.rs  ModelRequest, Message, system_prompt, context — what the model is offered
  view.rs     View, TurnView, ModelView, ToolView, Halt — the log, projected
  budget.rs   Budget — the turn and context bounds the runtime owns
  step.rs     Step, next — the whole driver decision, pure and total
  journal.rs  Journal port, Ask, Run, Close — durable + execution, implemented by the kernel
  run.rs      run, Request, Report — the async driver
mokmok-kernel  (composition root; the adapter adds no policy)
  src/agent.rs  Kernel: the Journal impl over sandbox + authority + db + artifact store + provider
  src/db.rs     Entry::Task / TaskRecord; Log.task
  src/config.rs Command::Agent, provider flags, role-scoped exposures
mokmok-sandbox  Input (a file written into work/ before the spawn), Request.inputs, Request.environment: Environment { Extend, Exact }
mokmok-state-machine  TurnFact::ModelDispatched, TurnFact::ModelResolved, Turn.model
```

### The provider is a program, not an HTTP client

The runtime never speaks HTTP, TLS, or a vendor dialect. It writes `ModelRequest` into the provider's workspace as `request.json` and reads `answer.json` back as an artifact.

```rust
// mokmok-agent-runtime/src/context.rs
pub struct ModelRequest { pub model: String, pub messages: Vec<Message>, pub max_tokens: u32 }
#[serde(tag = "role", rename_all = "snake_case")]
pub enum Message {
    System { text: String }, User { text: String }, Assistant { text: String },
    Tool { program: String, args: Vec<String>, code: Option<i32>,   // one finished tool execution
           stdout: String, stderr: String, stdout_truncated: bool },
}
pub fn system_prompt(checkout: &Path) -> String { unimplemented!() }  // TODO: one tool `run`, answer shape
/// The bounded window (brief, checkout path, newest tool results, oldest elided past
/// `Budget::context_bytes`), a pure fold over the durable view.
pub fn context(view: &View, budget: &Budget) -> ModelRequest { unimplemented!() }

// mokmok-agent-runtime/src/answer.rs
/// The model's answer, parsed from the text inside the provider's `answer.json` envelope.
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Answer { Run(ToolCall), Finish { summary: String } }
pub struct ToolCall { pub program: PathBuf, pub args: Vec<String>, pub deadline_secs: u64 }
pub enum Malformed { Envelope, ContentMissing, NotJson, UnknownAction, NotAbsolute }
/// Decodes `{"content": "<the model's text>"}`, then the text as `Answer`; discards every other
/// envelope field, usage and finish reason, and the provider's stdout/stderr.
pub fn parse_answer(answer_json: &[u8]) -> Result<Answer, Malformed> { unimplemented!() }
```

Any program that reads `request.json` and writes `answer.json` is a provider, and `/bin/sh -c` is one, so the fake provider drives the whole loop with no network. The vendor boundary lives in the operator-supplied provider program; no vendor or wire type crosses the runtime's or the kernel's surface.

### The view is the loop's only memory

```rust
// mokmok-agent-runtime/src/view.rs
pub struct View { pub brief: Option<Brief>, pub turns: Vec<TurnView>, pub phase: Phase, pub halted: Option<Halt> }
pub struct TurnView  { pub model: Option<ModelView>, pub tool: Option<ToolView>, pub outcome: Option<TurnOutcome> }
pub struct ModelView { pub execution: ExecutionID, pub resolution: Option<Resolution>,
                       pub answer: Option<Result<Answer, Malformed>>, pub raw: Option<String> }
pub struct ToolView  { pub execution: ExecutionID, pub resolution: Option<Resolution>, pub result: Option<ToolResult> }
pub struct Halt { pub execution: ExecutionID }   // a tool recovery left Unknown; the run stops

// mokmok-agent-runtime/src/step.rs
pub enum Step { Ask(Ask), Run(Run), Close(Close), Stop(Stop) }
pub struct Ask { pub number: u64, pub request: ModelRequest, pub deadline: Duration }
pub struct Run { pub call: ToolCall }
pub struct Close { pub outcome: TurnOutcome, pub close: Option<Ending> }
pub enum Stop { Done { summary: String }, Budget, Halted(Halt) }
/// The driver's whole decision, from the durable view alone. Pure and total. TODO: a settled last
/// turn (not a `Finish`) opens the next turn; a parsed `Finish` closes the session; a parsed `Run`
/// with no tool yet runs it; budget exhaustion stops without closing. `Ask` settles its own turn
/// `Failed` when the model execution did not finish or its answer did not parse.
pub fn next(view: &View, budget: &Budget) -> Step { unimplemented!() }

// mokmok-agent-runtime/src/journal.rs
pub trait Journal {   // the kernel implements; every mutator returns the rebuilt view
    fn open(&mut self, session: SessionID) -> Result<View, Error>;
    fn record_brief(&mut self, brief: Brief) -> Result<View, Error>;   // idempotent; first wins
    async fn ask(&mut self, step: &Ask) -> Result<View, Error>;
    async fn invoke(&mut self, step: &Run) -> Result<View, Error>;
    fn close(&mut self, step: &Close) -> Result<View, Error>;
}

// mokmok-agent-runtime/src/run.rs
pub struct Request { pub session: SessionID, pub brief: Brief, pub budget: Budget }
pub struct Report { pub turns: u64, pub stop: Stop }
/// The loop holds no state a crash could lose: the log is its only memory.
pub async fn run(journal: &mut impl Journal, request: Request) -> Result<Report, Error> {
    let mut view = journal.open(request.session)?;
    if view.brief.is_none() { view = journal.record_brief(request.brief)?; }
    let mut turns = 0;
    loop {
        match next(&view, &request.budget) {
            Step::Stop(stop)  => return Ok(Report { turns, stop }),
            Step::Ask(step)   => view = journal.ask(&step).await?,
            Step::Run(step)   => view = journal.invoke(&step).await?,
            Step::Close(step) => { view = journal.close(&step)?; turns += 1; }
        }
    }
}
```

### The kernel adapter: the model call is an execution

```rust
// mokmok-kernel/src/agent.rs — the credential is read from a 0600 file at open; it never enters the
// log, an artifact, or a notice, and is never placed in the daemon's own environment.
pub struct Kernel<'a> { config: &'a Config, db: Db, sandbox: Confined, authority: Authority,
    blobs: BlobStore, log: Log, session: SessionID, provider: Provider,
    answers: BTreeMap<ExecutionID, Cached> }   // parsed answers/results, rebuilt at open

async fn ask(&mut self, step: &Ask) -> Result<View, Error> {
    let at = self.now();
    let (turn, model) = (TurnID::new(timestamp(at)), ExecutionID::new(timestamp(at)));
    let mut intent = vec![
        Entry::Session(Fact::ExecutionLaunched { execution: model, deadline: Deadline::Bounded }),
        Entry::Turn(turn, TurnFact::TurnOpened { number: TurnNumber::new(step.number) }),
        Entry::Turn(turn, TurnFact::ModelRequested { attempt: Attempt::new(1) }),
        Entry::Turn(turn, TurnFact::ModelDispatched { execution: model, deadline: Deadline::Bounded }),
    ];
    if self.log.session.is_empty() { intent.insert(0, Entry::Session(Fact::SessionOpened)); }
    self.log.verify(self.session, &intent)?;
    self.commit(intent, notice(turn, Some(model), None), CommandId::new())?;  // Expected::At(version)
    crash(self.config, CrashPoint::BeforeDispatch);
    let plan = Plan { execution: model, program: self.provider.program.clone(), args: self.provider.args.clone(),
        deadline: step.deadline, artifacts: vec![PathBuf::from(ANSWER)],
        inputs: vec![Input::new(REQUEST, serde_json::to_vec(&step.request)?)],
        exposures: exposures(self.config, Role::Model) };   // never allow_write: no checkout reach
    let ran = execution::run(self.config, &self.sandbox, &self.authority, &plan,
        provider_environment(&self.provider), at).await?;
    // TODO: write ExecutionSettled, ResultApplied when Finished, Entry::Result, ModelResolved,
    // ModelAnswered when parse_answer succeeded, TurnSettled{Failed} when it did not; rebuild.
    self.rebuild()
}
```

`invoke` mirrors it: fresh `CommandId` and `ExecutionID`, intent `[ExecutionLaunched, ToolRequested]`, `BeforeDispatch`, the tool under `Role::Tool` exposures with no inputs, then `[ExecutionSettled, ResultApplied?, Entry::Result, ToolResolved, TurnSettled]`. The tool program is whatever the model requested; an unlisted program is `Resolution::Refused`, a recorded outcome the model reads next turn rather than a crash. `close` writes `TurnSettled{outcome}` and, for a `Finish`, `Fact::SessionClosed`.

### The sandbox grows exactly two things

```rust
// mokmok-sandbox/src/request.rs
pub struct Input { path: PathBuf, bytes: Vec<u8> }   // workspace-relative, written 0600
pub enum Environment { Extend(Vec<(OsString, OsString)>), Exact(Vec<(OsString, OsString)>) }
pub struct Request { … pub inputs: Vec<Input>, pub environment: Environment, … }
```

`Confined::spawn` writes `request.inputs` into `work/` after `prepare_workspace` and before it writes the plan, refusing a path that is not plain-relative or climbs out — the rule `artifact::is_declarable` already applies to outputs. That is the hook the mandate needs and the current API lacks; without it a request larger than `MAX_ARG_STRLEN` (≈128 KiB) cannot reach a confined program at all. `Role::Model` exposures are `allow_path` ∪ the provider paths and never `allow_write`, so the model process holds no capability to the checkout.

### Durable vocabulary (append-only; old logs still replay)

| kind | payload | change |
| --- | --- | --- |
| `mokmok.session.fact` | `Fact` | unchanged |
| `mokmok.turn.fact` | `TurnRecord { turn, fact }` | + `model_dispatched { execution, deadline }`, + `model_resolved { execution, resolution }` |
| `mokmok.execution.result` | `ResultRecord { execution, result }` | unchanged — now also names the model's answer blob |
| `mokmok.agent.task` | `TaskRecord { task, checkout }` | new |

`Log::push` learns the fourth kind; a truly unknown kind still stops the read. An old log holds only the original three kinds and six `TurnFact` tags, so it decodes and replays unchanged: `Turn.model` stays `None` and `answer()`'s added precondition (a resolved `Finished` model) bites only when a `ModelDispatched` is present. One session is still one stream of one aggregate.

### Durability, recovery, idempotence

- **Before dispatch.** For model and tool alike the launch transaction commits before the sandbox creates anything, and `Entry::Result` commits after the bytes exist; the `Notice` rides the same transaction, so it exists only after the history.
- **Recovery.** `recover` splits on the turn's facts: an in-flight execution named by `ModelDispatched` settles `ModelResolved{Unknown}`, any other `ToolResolved{Unknown}`, then the turn `Failed`.
- **A model `Unknown` retries, a tool `Unknown` halts.** A model execution cannot have touched the checkout, so the next call opens a new turn; a tool execution may have, so `rebuild` sets `View::halted` and `next` returns `Stop::Halted`. The distinction is a fact of the log, not a comment.
- **No step twice.** Every append is guarded by `Expected::At(version)` and carries a fresh `CommandId`, so a re-issued identical append is the store's `Replayed` and writes nothing. The loop re-derives the next step from the durable view, and `Turn::apply` refuses any fact after `TurnSettled` (`TurnOver`), so a settled turn is never re-dispatched; each dispatch mints one `ExecutionID`, which settles once (`ResolutionConflict` on a second, different resolution).

### Why this crate owns what

`mokmok-agent-runtime` is pure — protocol types, the machines' vocabulary, serde, the driver, no sandbox, no HTTP, no filesystem — so its loop is a fold over `View` and is unit- and property-testable. `mokmok-kernel` stays the composition root and the only I/O: it implements `Journal`, builds `View` by replaying the log and reading blobs, mints identifiers from the one clock, and maps `Step` onto `mokmok-sandbox` through the same `execution::run` a tool already uses. The architecture's dependency table stays intact (runtime → protocol, state-machine, ports). The public surface a caller touches is `run`, `Request`, `Report`, and the one `Journal` trait; what it hides is the two-phase commit per execution, the workspace seeding, the role-scoped authority, the vendor boundary, and the recovery split.

## Synthesis decision

## Tradeoffs accepted

- **No streaming**, for the model boundary inheriting every sandbox guarantee: `Run::wait` delivers stdout/stderr only after exit and caps each at `Limits::output`, so the runtime sees one finished answer, never tokens; time-to-first-token equals total generation. A live output tap is a new sandbox capability, left as future work.
- **A process per model call** (spawn + confiner + Landlock ruleset build), for reusing the exact execution path a tool uses. Model latency dwarfs it; it is not free.
- **A new sandbox field and `Environment` variant**, for closing a real leak: `configure` today *adds* to the inherited environment and never clears it, so every tool already inherits the daemon's whole environment.
- **Two `TurnFact` variants and a fourth log kind**, for the answer riding the existing `Entry::Result` and the artifact store's write-before-reference order.
- **The credential in the provider's environment**, for never writing it to disc: a `0600` file in `work/` survives a crash, while the environment dies with the process.
- **One tool (`run`)**, for interface depth: the model explores the checkout by running commands, so *all* filesystem reach — reads included — stays under the capability gate.
- **The tool's cwd is its ephemeral `work/`**, so the model addresses the checkout absolutely, for not weakening the sandbox's per-execution workspace.
- **`Unknown` halts the run**, for not blindly re-running a tool that may have edited the checkout; the operator reconciles, and a model `Unknown` retries in a new turn.

## Alternatives considered

- **An in-process model client behind a port** (no sandbox): smallest surface and latency, but it needs an HTTP+TLS dependency `Cargo.lock` does not have and puts the credential and the network in the trusted process, outside every confinement and durability guarantee.
- **The request in `argv` or `env`** instead of a workspace file: no sandbox change, but caps the context at ≈128 KiB, leaks it through `/proc/<pid>/cmdline`, and cannot carry an agent's history.
- **The answer as a new `Entry` content kind** instead of an `ExecutionResult`: it puts model-sized content in the append-only log, duplicating the artifact store's job, and leaves a second home for a large answer.
- **A step-at-a-time CLI** (one `agent` invocation per turn): bounds each process's scope, but the log already gives resumability, and it pushes turn bookkeeping onto the caller.
- **A new `Ending::Exhausted`**: it would make the machine claim an ending the task never reached; leaving the session `Accepting` lets the next invocation continue it, and `Report` names `Stop::Budget`.

## Open questions and risks

- **Is the provider's network reach acceptable?** Landlock here is ABI V3 `AccessFs` only: it mediates the filesystem, **not** `connect()`. Nothing in the sandbox stops the confined provider — or a confined tool — from dialing any host, and `mokmok-egress` is not wired in; only the provider's own behaviour keeps it to its endpoint. Gate the provider on a wired egress proxy (a new dependency plus the `policy_client` port), or accept "provider is trusted config, tools are not"?
- **Credential reachability.** Passed only to the provider via `Request::env`, the key lives in the provider's `environ` for the call; same-uid processes could read `/proc/<pid>/environ`, though the deployed unit's `ProtectProc=invisible` narrows that and a dev run does not. The loop is single-flight, so no agent tool runs concurrently with the provider. Is `Environment::Exact` plus a dedicated `mokmokd` user enough, or should the key ride an inherited fd?
- **Context growth.** `Budget::context_bytes` elides the oldest tool results, and an elided result is gone from the model's window (the ephemeral workspace no longer holds it). Is re-running a command to see it again acceptable, or does this need a bounded summary turn?
- **Retries.** New logs write `Attempt::new(1)` and treat a failed model turn as input to a new turn, leaving `Attempt` vestigial. Should transient model failure retry *within* a turn before it settles?
- **Mutation gate.** The pure runtime (`next`, `parse_answer`, `context`, the new `TurnFact` arms) is scorable at 95%; the adapter's `ask`/`invoke` are scored by the fake-provider integration test, not mutation. A surviving mutant in `exposures(Role::Model)` would be a least-authority hole the gate cannot see — should that list be a pure, unit-tested function?
- **The request is not durable.** The context is derivable from the log, so it is not stored; a reader cannot see the exact bytes the model was handed. Store it as an artifact for forensics, or keep the smaller footprint?

## Next implementation step

Extend `mokmok-sandbox` with `Input`/`Request::inputs` and `Environment::{Extend, Exact}` and land one integration test that spawns a confined `/bin/sh` reading a seeded `request.json` and writing an `answer.json` artifact — the smallest proof a request reaches a confined program and an answer comes back through the artifact store, with no network and no runtime code yet.

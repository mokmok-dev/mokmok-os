//! The kernel's adapters for the agent runtime, and the bounded loop that drives it.
//!
//! The runtime is pure: it reads no clock, no file, and no socket, and everything from the world
//! enters through [`Journal`], [`Provider`], and [`Tools`]. This module is where the world is
//! supplied — the event log and the artifact store as the journal, the scripted file as the
//! provider, and the authority, the sandbox, and the artifact store as the tool. [`run`] is the
//! operator's entry point: it opens a session, seeds its task, and performs at most one bounded
//! step at a time until the session closes or a limit stops it.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use mokmok_agent_runtime::{
    Action, Answer, AnswerRecord, ArtifactRef, Budget, Context, Error as AgentError, Exit, Halt,
    Journal, Move, Progress, Provider, Receipt, Record, Setting, Step, SubagentCall,
    SubagentOutcome, Subagents, Task, ToolCall, ToolReport, Tools, Transcript, Usage, advance,
    advance_watched, child_session, child_task, observation,
};
use mokmok_artifact::{BlobStore, ContentHash};
use mokmok_event_store::{CommandId, Expected, OutboxMessage, Outcome};
use mokmok_protocol::{Moment, SessionID, TurnID};
use mokmok_sandbox::{Confined, Environment};
use mokmok_state_machine::{Attempt, Ending, Fact, Resolution, Turn, TurnNumber};

use crate::authority::Authority;
use crate::config::{Config, ConfigError, CrashPoint, ProviderKind, Run};
use crate::db::{Db, Entry, Log};
use crate::editor::Editor;
use crate::error::Error;
use crate::execution::{self, Blob, Plan, Ran};
use crate::index::Index;
use crate::recovery;
use crate::skill::Library;
use crate::slice::Notice;

/// The model name a scripted answer records, so a transcript says where it came from.
const SCRIPTED_MODEL: &str = "scripted";

impl Run {
    /// The task this invocation seeds the session with.
    ///
    /// Discovery runs here, once per invocation: the recorded task freezes what it found, so a
    /// resumed session is offered exactly the skills it was opened with.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::NoTool`] when no `--allow-tool` names the one program the model's
    /// commands run as, and [`ConfigError::DuplicateSkill`] when two skill roots hold one name.
    pub fn task(
        &self,
        config: &Config,
    ) -> Result<Task, ConfigError> {
        let program = config
            .allow_tool
            .first()
            .cloned()
            .ok_or(ConfigError::NoTool)?;

        Ok(Task {
            goal: self.goal.clone(),
            setting: Setting {
                checkout: self.checkout.clone(),
                readable: config.allow_path.clone(),
                writable: config.allow_write.clone(),
                program,
                deadline: Duration::from_secs(self.tool_deadline_seconds),
                search: config.search_root.clone(),
                skills: crate::skill::discover(&config.skill_root)?,
                delegate: true,
            },
            budget: Budget {
                turns: self.max_turns,
                attempts: Budget::DEFAULT_ATTEMPTS,
                tokens: self.token_budget,
                observation_bytes: self.observation_bytes,
                context_turns: self.context_turns,
                context_bytes: self.context_bytes,
            },
        })
    }
}

/// The event log and the artifact store, as the runtime's journal.
pub struct Ledger {
    /// The task a caller named, handed to the fold before its record commits.
    seed: Option<Task>,
    at: Moment,
    db: Db,
    store: BlobStore,
    log: Log,
}

impl Ledger {
    /// Opens the session's history, recovering what a crash left in flight.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Db::open`], [`Db::log`], the recovery pass behind
    /// [`reconcile`](crate::reconcile), and [`BlobStore::open`].
    pub fn open(
        config: &Config,
        session: SessionID,
        task: Option<Task>,
        at: Moment,
    ) -> Result<Self, Error> {
        let mut db = Db::open(&config.events)?;
        let log = db.log(session)?;
        if !log.session.is_empty() {
            recovery::recover(&mut db, &log, session, at)?;
        }
        let log = db.log(session)?;

        Ok(Self {
            seed: task,
            at,
            db,
            store: BlobStore::open(&config.artifacts)?,
            log,
        })
    }

    /// The outbox notice one committed step carries, taken from the records it wrote.
    fn notice(
        &self,
        session: SessionID,
        command: CommandId,
        records: &[Record],
    ) -> Result<OutboxMessage, Error> {
        let mut turn = None;
        let mut execution = None;
        let mut resolution = None;

        for record in records {
            match record {
                Record::Turn { turn: held, .. } => turn = Some(*held),
                Record::Call(call) => {
                    turn = Some(call.turn);
                    execution = Some(call.execution);
                },
                Record::Session {
                    fact:
                        Fact::ExecutionLaunched {
                            execution: launched,
                            ..
                        },
                } => execution = Some(*launched),
                Record::Session {
                    fact:
                        Fact::ExecutionSettled {
                            execution: settled,
                            resolution: recorded,
                        },
                } => {
                    execution = Some(*settled);
                    resolution = Some(*recorded);
                },
                Record::Result {
                    execution: done,
                    report,
                } => {
                    execution = Some(*done);
                    resolution = Some(report.exit.resolution());
                },
                Record::Speech(speech) => turn = Some(speech.turn),
                Record::Find(find) => turn = Some(find.turn),
                Record::Skill(skill) => turn = Some(skill.turn),
                Record::EditIntent(edit) => {
                    turn = Some(edit.turn);
                    execution = Some(edit.execution);
                },
                Record::EditOutcome(edit) => {
                    turn = Some(edit.turn);
                    execution = Some(edit.execution);
                    resolution = Some(edit.report.outcome.resolution());
                },
                Record::SubagentIntent(subagent) => {
                    turn = Some(subagent.turn);
                    execution = Some(subagent.execution);
                },
                Record::SubagentOutcome(subagent) => {
                    turn = Some(subagent.turn);
                    execution = Some(subagent.execution);
                    resolution = Some(mokmok_state_machine::Resolution::Applied);
                },
                Record::Task(_)
                | Record::Prompt(_)
                | Record::Answer(_)
                | Record::Session { .. } => {},
            }
        }

        Notice {
            session,
            command,
            turn,
            execution,
            resolution,
            at: self.at,
        }
        .outbox()
    }
}

impl Journal for Ledger {
    fn transcript(
        &self,
        session: SessionID,
    ) -> Result<Transcript, AgentError> {
        let mut transcript =
            Transcript::replay(session, &self.log.ordered).map_err(AgentError::Refused)?;
        // The task the caller named seeds the fold before its record commits, which is what makes
        // the session's first move `Begin` rather than a halt with nothing to begin.
        if transcript.task.is_none() {
            transcript.task.clone_from(&self.seed);
        }

        Ok(transcript)
    }

    fn put(
        &mut self,
        bytes: &[u8],
        truncated: bool,
    ) -> Result<ArtifactRef, AgentError> {
        let hash = self
            .store
            .put(bytes)
            .map_err(|error| journal(Error::Artifact(error)))?;

        Ok(ArtifactRef {
            hash: hash.to_string(),
            bytes: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
            truncated,
        })
    }

    fn append(
        &mut self,
        step: Step,
    ) -> Result<Receipt, AgentError> {
        let session = step.session;
        let entries = step
            .records
            .iter()
            .map(|record| entry_of(record.clone()))
            .collect::<Vec<_>>();

        self.log.verify(session, &entries).map_err(journal)?;

        // The command id is the step's own derived id, so a repeated step replays instead of
        // writing a second time, and the store's dedup row is what makes that durable.
        let command = CommandId::from_uuid(step.id.as_uuid());
        let stream = Db::stream(session);
        let expected = Expected::At(self.db.version(stream).map_err(journal)?);
        let notice = self
            .notice(session, command, &step.records)
            .map_err(journal)?;

        match self
            .db
            .append(command, stream, expected, &entries, vec![notice])
            .map_err(journal)?
        {
            Outcome::Committed(_) => {
                self.log.absorb(&entries);

                Ok(Receipt::Written)
            },
            Outcome::Replayed(_) => Ok(Receipt::Replayed),
            Outcome::Conflicted { found, .. } => {
                Err(journal(Error::Conflicted { found: found.get() }))
            },
        }
    }
}

/// The authority, the sandbox, and the artifact store, as the runtime's tool.
///
/// The event log is deliberately absent: [`Db`] is not `Sync`, and the runtime's [`Tools`] port
/// requires it, so the tool and the journal are two values rather than one.
pub struct Shell {
    config: Config,
    program: PathBuf,
    store: BlobStore,
    at: Moment,
}

impl Shell {
    /// Opens the artifact store the report's references will name.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Artifact`] when the store cannot be opened.
    pub fn new(
        config: &Config,
        program: PathBuf,
        at: Moment,
    ) -> Result<Self, Error> {
        Ok(Self {
            store: BlobStore::open(&config.artifacts)?,
            config: config.clone(),
            program,
            at,
        })
    }

    async fn run(
        &self,
        call: &ToolCall,
    ) -> Result<ToolReport, Error> {
        // The intent is durable before this call, so a stop here leaves a request with no outcome
        // for recovery to settle.
        crate::config::crash(&self.config, CrashPoint::BeforeDispatch);

        let plan = Plan {
            execution: call.execution,
            program: self.program.clone(),
            args: vec![OsString::from("-c"), OsString::from(call.command.clone())],
            environment: Environment::Exact(self.config.tool_env.clone()),
            deadline: call.deadline,
            artifacts: Vec::new(),
            exposures: execution::exposures(&self.config),
        };
        let sandbox = Confined::new(mokmok_sandbox::Config {
            root: self.config.sandbox_root.clone(),
            confiner: self.config.confiner.clone(),
            ..mokmok_sandbox::Config::default()
        })?;
        let authority = Authority::new(&self.config, self.at)?;

        match execution::run(&self.config, &sandbox, &authority, &plan, self.at).await? {
            Ran::Settled { resolution, result } => {
                let stdout = self.read(&result.stdout)?;
                let stderr = self.read(&result.stderr)?;

                Ok(ToolReport {
                    exit: exit_of(resolution),
                    attempted: result.attempted,
                    stdout: reference(&result.stdout),
                    stderr: reference(&result.stderr),
                    observation: observation(resolution, &stdout, &stderr),
                })
            },
            Ran::Refused(_) => {
                // A refusal produced no streams, so the report names the empty blob rather than a
                // reference to bytes nobody wrote.
                let empty = self.store.put(&[])?;
                let reference = ArtifactRef {
                    hash: empty.to_string(),
                    bytes: 0,
                    truncated: false,
                };

                Ok(ToolReport {
                    exit: Exit::Refused,
                    attempted: false,
                    stdout: reference.clone(),
                    stderr: reference,
                    observation: observation(Resolution::Refused, &[], &[]),
                })
            },
        }
    }

    fn read(
        &self,
        blob: &Blob,
    ) -> Result<Vec<u8>, Error> {
        let hash = hash_of(&blob.hash)?;

        self.store.read(hash).map_err(Error::Artifact)
    }
}

impl Tools for Shell {
    async fn invoke(
        &self,
        call: &ToolCall,
    ) -> Result<ToolReport, AgentError> {
        self.run(call)
            .await
            .map_err(|error| AgentError::Tool(Box::new(error)))
    }
}

/// The scripted provider: answers replayed from a JSON file, so a loop runs with no network.
pub struct Scripted {
    answers: Mutex<VecDeque<Answer>>,
}

impl Scripted {
    /// Reads the answers from the JSON file `path` names and stores each one's text.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] when the file cannot be read, [`Error::NotJson`] when it is not a list
    /// of answers, and [`Error::Artifact`] when an answer's bytes cannot be stored.
    pub fn open(
        config: &Config,
        path: &Path,
    ) -> Result<Self, Error> {
        let bytes = std::fs::read(path)?;
        let script: Vec<Script> = serde_json::from_slice(&bytes)?;
        let store = BlobStore::open(&config.artifacts)?;
        let mut answers = VecDeque::new();

        for answer in script {
            let prose = answer
                .say()
                .map_or_else(String::new, mokmok_agent_runtime::clip_prose);
            let text = serde_json::to_vec(&answer)?;
            let hash = store.put(&text)?;
            answers.push_back(Answer {
                action: answer.into(),
                prose,
                text: ArtifactRef {
                    hash: hash.to_string(),
                    bytes: u64::try_from(text.len()).unwrap_or(u64::MAX),
                    truncated: false,
                },
                model: SCRIPTED_MODEL.to_owned(),
                usage: Usage {
                    input: 0,
                    output: 0,
                },
            });
        }

        Ok(Self {
            answers: Mutex::new(answers),
        })
    }
}

impl Provider for Scripted {
    fn complete(
        &self,
        _context: &Context,
    ) -> impl Future<Output = Result<Answer, AgentError>> + Send {
        let answer = self
            .answers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pop_front()
            .ok_or_else(|| AgentError::Provider(Box::new(Exhausted)));

        std::future::ready(answer)
    }

    async fn complete_watched<'a>(
        &'a self,
        context: &'a Context,
        watch: &'a mut dyn FnMut(&str),
    ) -> Result<Answer, AgentError> {
        let answer = self.complete(context).await?;
        watch(&answer.prose);
        Ok(answer)
    }
}

/// One scripted answer, as the file holds it.
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
enum Script {
    Command {
        command: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        say: Option<String>,
    },
    Find {
        query: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        say: Option<String>,
    },
    Edit {
        patch: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expect: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        say: Option<String>,
    },
    UseSkill {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        say: Option<String>,
    },
    /// Hand a goal to a child coding agent in its own durable session.
    Subagent {
        goal: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        say: Option<String>,
    },
    /// The model is done.
    Final {
        /// The model's own words, when the script supplies them, so an offline run shows prose.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        say: Option<String>,
    },
    /// The model's arguments could not be decoded.
    Malformed {
        reason: String,
        /// The model's own words, when the script supplies them, so an offline run shows prose.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        say: Option<String>,
    },
}

impl Script {
    fn say(&self) -> Option<&str> {
        match self {
            Self::Command { say, .. }
            | Self::Find { say, .. }
            | Self::Edit { say, .. }
            | Self::UseSkill { say, .. }
            | Self::Subagent { say, .. }
            | Self::Final { say }
            | Self::Malformed { say, .. } => say.as_deref(),
        }
    }
}

impl From<Script> for Action {
    fn from(script: Script) -> Self {
        match script {
            Script::Command { command, .. } => Self::Command { command },
            Script::Find { query, .. } => Self::Find { query },
            Script::Edit { patch, expect, .. } => mokmok_agent_runtime::patch_refusal(&patch)
                .map_or(Self::Edit { patch, expect }, |reason| Self::Malformed {
                    reason,
                }),
            Script::UseSkill { name, .. } => mokmok_agent_runtime::SkillName::new(&name)
                .map_or_else(
                    |refusal| Self::Malformed {
                        reason: format!("the skill name is not valid: {refusal}"),
                    },
                    |name| Self::UseSkill { name },
                ),
            Script::Subagent { goal, .. } => Self::Subagent {
                goal: mokmok_agent_runtime::clip_goal(&goal),
            },
            Script::Final { .. } => Self::Final,
            Script::Malformed { reason, .. } => Self::Malformed { reason },
        }
    }
}

/// The provider one run selected.
enum Model {
    /// The no-network route, reading its answers from the script file.
    Scripted(Scripted),
    /// The real endpoint, over TLS.
    Ollama(crate::provider::Ollama),
}

impl Provider for Model {
    async fn complete(
        &self,
        context: &Context,
    ) -> Result<Answer, AgentError> {
        match self {
            Self::Scripted(provider) => provider.complete(context).await,
            Self::Ollama(provider) => provider.complete(context).await,
        }
    }

    async fn complete_watched<'a>(
        &'a self,
        context: &'a Context,
        watch: &'a mut dyn FnMut(&str),
    ) -> Result<Answer, AgentError> {
        match self {
            Self::Scripted(provider) => provider.complete_watched(context, watch).await,
            Self::Ollama(provider) => provider.complete_watched(context, watch).await,
        }
    }
}

/// Builds the provider the configuration named.
fn model(
    config: &Config,
    session: SessionID,
) -> Result<Model, Error> {
    match config.provider.kind {
        ProviderKind::Scripted => {
            let path = config
                .provider
                .script
                .as_deref()
                .ok_or(ConfigError::NoScript)?;

            Ok(Model::Scripted(Scripted::open(config, path)?))
        },
        ProviderKind::Ollama => {
            let credential = config
                .provider
                .credential
                .clone()
                .ok_or(ConfigError::NoCredential)?;
            let provider = crate::provider::Ollama::open(config, credential, session)
                .map_err(|error| Error::Agent(AgentError::Provider(Box::new(error))))?;

            Ok(Model::Ollama(provider))
        },
    }
}

/// Why a scripted run stopped before it closed.
#[derive(Debug, thiserror::Error)]
#[error("the script holds no answer for this attempt")]
struct Exhausted;

/// Runs a child session for one parent delegation, enforcing the one-level invariant.
struct ChildRunner {
    config: Config,
    parent: Task,
    provider: Arc<Model>,
    at: Moment,
}

impl ChildRunner {
    fn new(
        config: &Config,
        parent: Task,
        provider: Arc<Model>,
        at: Moment,
    ) -> Self {
        Self {
            config: config.clone(),
            parent,
            provider,
            at,
        }
    }
}

impl Subagents for ChildRunner {
    #[allow(refining_impl_trait)]
    fn run(
        &self,
        call: &SubagentCall,
    ) -> Pin<Box<dyn Future<Output = Result<SubagentOutcome, AgentError>> + '_>> {
        let config = self.config.clone();
        let parent = self.parent.clone();
        let provider = Arc::clone(&self.provider);
        let at = self.at;
        let call = call.clone();

        Box::pin(async move {
            let child = child_session(call.execution);
            let child_task = child_task(&parent, &call.goal);

            let run = Run {
                session: child.as_uuid().to_string(),
                goal: call.goal.clone(),
                checkout: child_task.setting.checkout.clone(),
                max_turns: child_task.budget.turns,
                token_budget: child_task.budget.tokens,
                context_bytes: child_task.budget.context_bytes,
                observation_bytes: child_task.budget.observation_bytes,
                context_turns: child_task.budget.context_turns,
                tool_deadline_seconds: child_task.setting.deadline.as_secs(),
                steps: 64,
            };

            let mut driver = Driver::open_with(&config, &run, at, child_task, provider)
                .map_err(|error| AgentError::Subagent(Box::new(error)))?;

            for _ in 0..run.steps {
                match driver.step().await {
                    Ok(
                        Progress::Terminal(_)
                        | Progress::Awaiting
                        | Progress::Advanced(Move::Halt(_)),
                    ) => break,
                    Err(error) => return Err(AgentError::Subagent(Box::new(error))),
                    Ok(_) => {},
                }
            }

            let transcript = driver
                .transcript()
                .map_err(|error| AgentError::Subagent(Box::new(error)))?;
            let reply = extract_child_reply(&transcript);
            let usage = Usage {
                input: transcript
                    .answers
                    .values()
                    .map(|answer| answer.usage.input)
                    .fold(0, u64::saturating_add),
                output: transcript
                    .answers
                    .values()
                    .map(|answer| answer.usage.output)
                    .fold(0, u64::saturating_add),
            };

            Ok(SubagentOutcome { reply, usage })
        })
    }
}

/// The child session's final prose, newest turn first, or a status sentence if none.
fn extract_child_reply(transcript: &Transcript) -> String {
    transcript
        .turns_in_order()
        .into_iter()
        .rev()
        .find_map(|(turn, _)| {
            transcript
                .newest_answer(turn)
                .filter(|answer| answer.action == Action::Final)
                .map(|answer| answer.prose.clone())
        })
        .unwrap_or_else(|| {
            format!(
                "the subagent stopped before producing a final answer ({phase:?})",
                phase = transcript.session.phase()
            )
        })
}

/// What one `agent run` invocation did, step by step.
#[derive(Debug)]
pub struct Report {
    pub session: SessionID,
    /// The ending, once a step closed the session.
    pub ending: Option<Ending>,
    /// Why the loop stopped short of a close, if it did.
    pub halt: Option<Halt>,
    /// One line per bounded step, saying what the step did.
    pub steps: Vec<String>,
}

/// One session's world, assembled the way the kernel assembles it: the event log and the artifact
/// store as the journal, the authority and the sandbox as the tool, and the provider the
/// configuration named.
///
/// It exists so a second front end drives the same loop without assembling a second world or
/// re-deciding which provider a configuration means. A step is one bounded step of the same
/// [`advance`] `agent run` calls.
pub struct Driver {
    ledger: Ledger,
    shell: Shell,
    index: Index,
    skills: Library,
    editor: Editor,
    provider: Arc<Model>,
    delegates: ChildRunner,
    config: Config,
    session: SessionID,
    task: Task,
    at: Moment,
}

impl Driver {
    /// Opens the session's world: its ledger, its tool, its finder, and the provider the
    /// configuration named.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Run::task`], the provider the configuration named, the tool, the
    /// ledger, the editor, and the finder.
    pub fn open(
        config: &Config,
        run: &Run,
        at: Moment,
    ) -> Result<Self, Error> {
        let session = SessionID::from_uuid(crate::config::uuid_of(&run.session, "session")?);
        let task = run.task(config)?;
        let provider = Arc::new(model(config, session)?);
        Self::open_with(config, run, at, task, provider)
    }

    fn open_with(
        config: &Config,
        run: &Run,
        at: Moment,
        task: Task,
        provider: Arc<Model>,
    ) -> Result<Self, Error> {
        let session = SessionID::from_uuid(crate::config::uuid_of(&run.session, "session")?);
        let shell = Shell::new(config, task.setting.program.clone(), at)?;
        let ledger = Ledger::open(config, session, Some(task.clone()), at)?;
        // The task the fold holds, not the one this invocation built: the prompt the model was
        // handed, the tools the provider was given, and the roots the finder opened must all come
        // off the one recorded task, or a resumed session is offered a tool that will not answer
        // and a finder over roots the model was never told about.
        let task = ledger
            .transcript(session)
            .map_err(Error::Agent)?
            .task
            .unwrap_or(task);
        let editor = Editor::new(config, &task.setting.checkout, at)?;
        let index = if task.setting.search.is_empty() {
            Index::none()
        } else {
            Index::open(&task.setting.search, &config.search_index)?
        };
        let skills = Library::open(&task.setting.skills);
        let delegates = ChildRunner::new(config, task.clone(), Arc::clone(&provider), at);

        Ok(Self {
            ledger,
            shell,
            index,
            skills,
            editor,
            provider,
            delegates,
            config: config.clone(),
            session,
            task,
            at,
        })
    }

    /// The session this driver works in.
    #[must_use]
    pub const fn session(&self) -> SessionID {
        self.session
    }

    /// The fold of everything the session has committed.
    ///
    /// # Errors
    ///
    /// Returns the journal's failure when the history cannot be read or does not fold.
    pub fn transcript(&self) -> Result<Transcript, AgentError> {
        self.ledger.transcript(self.session)
    }

    /// Commits one human line as the input of the session's next turn.
    ///
    /// # Errors
    ///
    /// Returns `NotAwaiting` when the session is not waiting for a line, `TooMuch` when the line is
    /// too long, and the journal's own failures.
    pub fn speak(
        &mut self,
        line: &str,
    ) -> Result<Receipt, AgentError> {
        mokmok_agent_runtime::speak(&mut self.ledger, self.session, line)
    }

    /// Performs at most one bounded step of the same [`advance`] `agent run` calls.
    ///
    /// # Errors
    ///
    /// Returns the runtime's own failures.
    pub async fn step(&mut self) -> Result<Progress, AgentError> {
        advance(
            &mut self.ledger,
            &*self.provider,
            &self.shell,
            &self.index,
            &self.editor,
            &self.skills,
            &self.delegates,
            self.session,
            self.at,
        )
        .await
    }

    /// Like [`step`], but forwards every prose fragment the model emits while streaming to `watch`.
    ///
    /// The journal still records one whole answer per attempt; the stream is display-only.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`step`].
    pub async fn step_watched(
        &mut self,
        watch: &mut dyn FnMut(&str),
    ) -> Result<Progress, AgentError> {
        advance_watched(
            &mut self.ledger,
            &*self.provider,
            &self.shell,
            &self.index,
            &self.editor,
            &self.skills,
            &self.delegates,
            self.session,
            self.at,
            watch,
        )
        .await
    }

    /// Re-opens the ledger, running the session's idempotent recovery, so a caller can settle what
    /// a dead process left in flight.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Ledger::open`].
    pub fn reopen(&mut self) -> Result<(), Error> {
        self.ledger = Ledger::open(&self.config, self.session, Some(self.task.clone()), self.at)?;

        Ok(())
    }
}

/// Drives one session for at most `run.steps` bounded steps.
///
/// # Errors
///
/// Returns the errors of opening the log, the script, and the tool, plus the runtime's own.
pub async fn run(
    config: &Config,
    run: &Run,
    at: Moment,
) -> Result<Report, Error> {
    let mut driver = Driver::open(config, run, at)?;
    let mut report = Report {
        session: driver.session(),
        ending: None,
        halt: None,
        steps: Vec::new(),
    };

    for _ in 0..run.steps {
        match driver.step().await? {
            Progress::Terminal(ending) => {
                report.ending = Some(ending);
                report
                    .steps
                    .push(format!("move=terminal phase=Closed({ending:?})"));

                return Ok(report);
            },
            Progress::Awaiting => {
                report.steps.push("awaiting".to_owned());

                return Ok(report);
            },
            Progress::Yielded => report.steps.push("move=yielded".to_owned()),
            Progress::Advanced(mv) => {
                report
                    .steps
                    .push(step_line(&driver.ledger, driver.session(), &mv));
                match mv {
                    Move::Halt(halt) => {
                        report.halt = Some(halt);

                        return Ok(report);
                    },
                    Move::Close {
                        ending: Some(ending),
                        ..
                    } => report.ending = Some(ending),
                    _ => {},
                }
            },
        }
    }

    Ok(report)
}

/// Records a cancel on the session, so the next run closes it cancelled.
///
/// # Errors
///
/// Returns [`Error::UnknownSession`] when the log holds nothing for the session, and the errors of
/// the store.
pub fn cancel(
    config: &Config,
    session: SessionID,
    at: Moment,
) -> Result<(), Error> {
    let mut db = Db::open(&config.events)?;
    let log = db.log(session)?;
    if log.session.is_empty() {
        return Err(Error::UnknownSession(session.as_uuid()));
    }

    let entries = [Entry::Session(Fact::CancelRequested)];
    log.verify(session, &entries)?;

    let command = CommandId::new();
    let notice = Notice {
        session,
        command,
        turn: None,
        execution: None,
        resolution: None,
        at,
    }
    .outbox()?;
    let stream = Db::stream(session);
    let expected = Expected::At(db.version(stream)?);

    match db.append(command, stream, expected, &entries, vec![notice])? {
        Outcome::Committed(_) | Outcome::Replayed(_) => Ok(()),
        Outcome::Conflicted { found, .. } => Err(Error::Conflicted { found: found.get() }),
    }
}

/// The entry one runtime record is logged as.
///
/// The two machine records keep their own kinds so a slice-era log still reads; every other record
/// is the runtime's own vocabulary and rides the one agent kind.
fn entry_of(record: Record) -> Entry {
    match record {
        Record::Session { fact } => Entry::Session(fact),
        Record::Turn { turn, fact } => Entry::Turn(turn, fact),
        other => Entry::Agent(other),
    }
}

/// Erases one kernel failure behind the journal port, which may not name the kernel.
fn journal(error: Error) -> AgentError {
    AgentError::Journal(Box::new(error))
}

/// Reads the hex form of a stored blob's name.
fn hash_of(text: &str) -> Result<ContentHash, Error> {
    text.parse().map_err(|_| Error::NotAName("a content hash"))
}

/// The exit an execution's settlement reads as.
const fn exit_of(resolution: Resolution) -> Exit {
    match resolution {
        Resolution::Finished { code } => Exit::Finished { code },
        // An edit runs no process, so `Applied` borrows `Finished { 0 }` as the sentinel the
        // command vocabulary already has; the edit's own record carries the true outcome.
        Resolution::Applied => Exit::Finished { code: 0 },
        Resolution::Killed { signal } => Exit::Killed { signal },
        Resolution::TimedOut => Exit::TimedOut,
        Resolution::Refused => Exit::Refused,
        Resolution::Cancelled | Resolution::Unknown => Exit::Unknown,
    }
}

/// The reference a stored blob is addressed by.
fn reference(blob: &Blob) -> ArtifactRef {
    ArtifactRef {
        hash: blob.hash.clone(),
        bytes: blob.bytes,
        truncated: blob.truncated,
    }
}

/// One bounded step as the single line the operator reads.
fn step_line(
    ledger: &Ledger,
    session: SessionID,
    next: &Move,
) -> String {
    match next {
        Move::Begin { .. } => "move=begin".to_owned(),
        Move::Ask {
            turn,
            number,
            attempt,
            ..
        } => {
            let answer = ledger
                .transcript(session)
                .ok()
                .and_then(|transcript| transcript.newest_answer(*turn).cloned());

            ask_line(*number, *attempt, answer.as_ref())
        },
        Move::Act {
            turn, execution, ..
        } => format!(
            "move=act turn={} execution={}",
            number_of(ledger, session, *turn),
            execution.as_uuid(),
        ),
        Move::Find { turn, query } => {
            format!(
                "move=find turn={} query={query:?}",
                number_of(ledger, session, *turn)
            )
        },
        Move::UseSkill { turn, name } => {
            format!(
                "move=use-skill turn={} skill={:?}",
                number_of(ledger, session, *turn),
                name.as_str()
            )
        },
        Move::Edit { turn, .. } => {
            format!("move=edit turn={}", number_of(ledger, session, *turn))
        },
        Move::Subagent { turn, call } => {
            format!(
                "move=subagent turn={} child={}",
                number_of(ledger, session, *turn),
                child_session(call.execution).as_uuid(),
            )
        },
        Move::Close {
            turn,
            outcome,
            ending,
        } => format!(
            "move=close turn={} outcome={outcome:?} ending={ending:?}",
            number_of(ledger, session, *turn),
        ),
        Move::Await => "move=await".to_owned(),
        Move::Halt(halt) => format!("move=halt {halt:?}"),
    }
}

/// The line one `Ask` step reads as, naming the answer it left durable.
fn ask_line(
    number: TurnNumber,
    attempt: Attempt,
    answer: Option<&AnswerRecord>,
) -> String {
    let number = number.get();
    let attempt = attempt.get();

    answer.map_or_else(
        || format!("move=ask turn={number} attempt={attempt} answer=none"),
        |record| match &record.action {
            Action::Command { command } => {
                format!(
                    "move=ask turn={number} attempt={attempt} answer=command command={command:?}"
                )
            },
            Action::Final => format!("move=ask turn={number} attempt={attempt} answer=final"),
            Action::Find { query } => {
                format!("move=ask turn={number} attempt={attempt} answer=find query={query:?}")
            },
            Action::UseSkill { name } => {
                format!(
                    "move=ask turn={number} attempt={attempt} answer=use_skill skill={:?}",
                    name.as_str(),
                )
            },
            Action::Edit { patch, .. } => {
                format!("move=ask turn={number} attempt={attempt} answer=edit patch={patch:?}")
            },
            Action::Subagent { goal } => {
                format!("move=ask turn={number} attempt={attempt} answer=subagent goal={goal:?}")
            },
            Action::Malformed { reason } => {
                format!(
                    "move=ask turn={number} attempt={attempt} answer=malformed reason={reason:?}"
                )
            },
        },
    )
}

/// The turn number a turn names in the fold, or zero.
fn number_of(
    ledger: &Ledger,
    session: SessionID,
    turn: TurnID,
) -> u64 {
    ledger
        .transcript(session)
        .ok()
        .and_then(|transcript| transcript.turn(turn).and_then(Turn::number))
        .map_or(0, TurnNumber::get)
}

#[cfg(test)]
mod tests {
    use super::Ledger;
    use crate::config::{Config, ProviderSettings};
    use crate::db::Db;
    use mokmok_agent_runtime::{
        Budget, Journal, Receipt, Record, Setting, Step, StepId, StepKey, Task, TaskRecord,
    };
    use mokmok_protocol::{Moment, SessionID};
    use mokmok_state_machine::Fact;
    use std::path::{Path, PathBuf};
    use std::time::Duration;
    use uuid::Uuid;

    /// A directory this test's own, removed when it ends.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir()
                .join(format!("mokmok-agent-unit-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);

            Self(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn config(root: &Path) -> Config {
        let data = root.join("data");
        let artifacts = data.join("artifacts");
        let sandbox_root = data.join("sandbox");
        std::fs::create_dir_all(&artifacts).unwrap();
        std::fs::create_dir_all(&sandbox_root).unwrap();

        Config {
            data_dir: data.clone(),
            events: data.join("events.sqlite"),
            artifacts,
            sandbox_root,
            allow_tool: Vec::new(),
            allow_path: Vec::new(),
            allow_write: Vec::new(),
            search_root: Vec::new(),
            search_index: data.join("search"),
            skill_root: Vec::new(),
            tool_env: Vec::new(),
            confiner: PathBuf::from("/bin/sh"),
            crash_after: None,
            provider: ProviderSettings::default(),
        }
    }

    fn task() -> Task {
        Task {
            goal: "goal".to_owned(),
            setting: Setting {
                checkout: PathBuf::from("/checkout"),
                readable: Vec::new(),
                writable: Vec::new(),
                program: PathBuf::from("/bin/sh"),
                deadline: Duration::from_secs(1),
                search: Vec::new(),
                skills: Vec::new(),
                delegate: false,
            },
            budget: Budget::default(),
        }
    }

    #[test]
    fn a_step_repeated_under_its_own_command_id_replays_instead_of_writing_twice() {
        let scratch = Scratch::new("command-id");
        let config = config(&scratch.0);
        let session = SessionID::from_uuid(Uuid::from_u128(1));
        let mut ledger = Ledger::open(&config, session, Some(task()), Moment::EPOCH).unwrap();

        let task = task();
        let step = Step::new(
            StepId::derive(session, StepKey::Task),
            session,
            vec![
                Record::Session {
                    fact: Fact::SessionOpened,
                },
                Record::Task(TaskRecord { task }),
            ],
        );

        assert_eq!(ledger.append(step.clone()).unwrap(), Receipt::Written);
        assert_eq!(ledger.append(step).unwrap(), Receipt::Replayed);

        let log = Db::open(&config.events).unwrap().log(session).unwrap();
        assert_eq!(log.session, vec![Fact::SessionOpened]);
        assert_eq!(
            log.ordered
                .iter()
                .filter(|record| matches!(record, Record::Task(_)))
                .count(),
            1,
            "the repeated step wrote nothing a second time:\n{:?}",
            log.ordered,
        );
    }
}

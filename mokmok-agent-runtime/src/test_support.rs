//! Builders the unit tests share, so a hand-written history is written once.

use std::collections::{BTreeSet, VecDeque};
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use mokmok_protocol::{ExecutionID, SessionID, TurnID};
use mokmok_skill::SkillName;
use mokmok_state_machine::{Attempt, Deadline, Ending, Fact, TurnFact, TurnNumber, TurnOutcome};
use uuid::Uuid;

use crate::dialogue::Transcript;
use crate::edit::{EditCall, EditOutcome, EditReport, Editor, Plan, Ready};
use crate::error::Error;
use crate::find::{FindReport, Finder};
use crate::model::{Action, Usage};
use crate::record::{
    AnswerRecord, ArtifactRef, CallRecord, EditIntentRecord, EditOutcomeRecord, FindRecord,
    PromptRecord, Receipt, Record, SkillOutcome, SkillRecord, SpeechRecord, Step, StepId,
    TaskRecord,
};
use crate::skill::{Read, Skills};
use crate::task::{Advert, Budget, Setting, Task};
use crate::tool::{Exit, ToolReport, observation};

/// The session every builder in this module names.
#[must_use]
pub fn session() -> SessionID {
    SessionID::from_uuid(Uuid::from_u128(1))
}

/// The turn a number names on [`session`].
#[must_use]
pub fn turn_id(number: u64) -> TurnID {
    crate::advance::turn_id(session(), number)
}

/// The execution a turn names.
#[must_use]
pub fn execution_id(turn: TurnID) -> ExecutionID {
    crate::advance::execution_id(turn)
}

/// A reference that differs by byte, so two content records can conflict on demand.
#[must_use]
pub fn artifact(byte: u8) -> ArtifactRef {
    ArtifactRef {
        hash: format!("{byte:02x}"),
        bytes: 1,
        truncated: false,
    }
}

/// A task whose ceilings are wide enough for a test.
#[must_use]
pub fn task(turns: u32) -> Task {
    Task {
        goal: "make the failing test pass".to_owned(),
        setting: Setting {
            checkout: "/srv/checkout".into(),
            readable: vec!["/nix/store".into()],
            writable: vec!["/srv/checkout".into()],
            program: "/bin/sh".into(),
            deadline: Duration::from_secs(120),
            search: vec!["/srv/checkout".into()],
            skills: Vec::new(),
        },
        budget: Budget {
            turns,
            attempts: Budget::DEFAULT_ATTEMPTS,
            tokens: 0,
            observation_bytes: 4096,
            context_turns: 8,
            context_bytes: 65_536,
        },
    }
}

/// One advertised skill, as discovery would freeze it.
#[must_use]
pub fn skill_advert(name: &str) -> Advert {
    Advert {
        name: SkillName::new(name).expect("the test names a valid skill"),
        description: format!("{name} description"),
        source: PathBuf::from(format!("/skills/{name}/SKILL.md")),
        digest: format!("{name} digest"),
    }
}

/// The records a `Begin` step carries: the session opens and the task is written.
#[must_use]
pub fn begin_records(task: &Task) -> Vec<Record> {
    vec![
        Record::Session {
            fact: Fact::SessionOpened,
        },
        Record::Task(TaskRecord { task: task.clone() }),
    ]
}

/// The records an `Ask` intent carries: the turn opens, the model is requested, the prompt follows.
#[must_use]
pub fn open_records(
    turn: TurnID,
    number: u64,
    attempt: u32,
) -> Vec<Record> {
    vec![
        Record::Turn {
            turn,
            fact: TurnFact::TurnOpened {
                number: TurnNumber::new(number),
            },
        },
        Record::Turn {
            turn,
            fact: TurnFact::ModelRequested {
                attempt: Attempt::new(attempt),
            },
        },
        Record::Prompt(PromptRecord {
            turn,
            attempt: Attempt::new(attempt),
            context: artifact(u8::try_from(attempt).unwrap_or(u8::MAX)),
        }),
    ]
}

/// The records an `Ask` outcome carries: the model answered with `action`.
#[must_use]
pub fn answer_records(
    turn: TurnID,
    attempt: u32,
    action: Action,
) -> Vec<Record> {
    vec![
        Record::Turn {
            turn,
            fact: TurnFact::ModelAnswered,
        },
        Record::Answer(AnswerRecord {
            turn,
            attempt: Attempt::new(attempt),
            action,
            prose: String::new(),
            text: artifact(0xaa),
            model: "test".to_owned(),
            usage: Usage {
                input: 1,
                output: 1,
            },
        }),
    ]
}

/// The records a human line's commit carries: the turn opens and the line follows.
#[must_use]
pub fn speech_records(
    turn: TurnID,
    number: u64,
    text: &str,
) -> Vec<Record> {
    vec![
        Record::Turn {
            turn,
            fact: TurnFact::TurnOpened {
                number: TurnNumber::new(number),
            },
        },
        Record::Speech(SpeechRecord {
            turn,
            text: text.to_owned(),
        }),
    ]
}

/// The records an `Act` intent carries: the execution launches, the tool is requested, the call
/// follows.
#[must_use]
pub fn act_intent_records(
    turn: TurnID,
    execution: ExecutionID,
    command: &str,
) -> Vec<Record> {
    vec![
        Record::Session {
            fact: Fact::ExecutionLaunched {
                execution,
                deadline: Deadline::Bounded,
            },
        },
        Record::Turn {
            turn,
            fact: TurnFact::ToolRequested {
                execution,
                deadline: Deadline::Bounded,
            },
        },
        Record::Call(CallRecord {
            turn,
            execution,
            command: command.to_owned(),
        }),
    ]
}

/// The records an `Act` outcome carries: the execution settles and the turn completes.
#[must_use]
pub fn act_outcome_records(
    turn: TurnID,
    execution: ExecutionID,
    exit: Exit,
    output: &[u8],
) -> Vec<Record> {
    let resolution = exit.resolution();
    let mut records = vec![Record::Session {
        fact: Fact::ExecutionSettled {
            execution,
            resolution,
        },
    }];
    if matches!(exit, Exit::Finished { .. }) {
        records.push(Record::Session {
            fact: Fact::ResultApplied { execution },
        });
    }
    records.push(Record::Result {
        execution,
        report: ToolReport {
            observation: observation(resolution, output, &[]),
            ..report(exit)
        },
    });
    records.push(Record::Turn {
        turn,
        fact: TurnFact::ToolResolved {
            execution,
            resolution,
        },
    });
    records.push(Record::Turn {
        turn,
        fact: TurnFact::TurnSettled {
            outcome: TurnOutcome::Completed,
        },
    });
    records
}

/// The records an `Edit` intent carries: launch, request, and the intent.
#[must_use]
pub fn edit_intent_records(
    turn: TurnID,
    execution: ExecutionID,
    patch: &str,
    relative: &str,
    before: &ArtifactRef,
    after: &ArtifactRef,
    placement: &str,
) -> Vec<Record> {
    let mut records = edit_requested_records(turn, execution);
    records.push(Record::EditIntent(EditIntentRecord {
        turn,
        execution,
        patch: patch.to_owned(),
        expect: None,
        relative: relative.to_owned(),
        before: before.clone(),
        after: after.clone(),
        placement: placement.to_owned(),
    }));

    records
}

/// The launch and the turn's request for one edit, without the intent record.
#[must_use]
pub fn edit_requested_records(
    turn: TurnID,
    execution: ExecutionID,
) -> Vec<Record> {
    vec![
        Record::Session {
            fact: Fact::ExecutionLaunched {
                execution,
                deadline: Deadline::Unbounded,
            },
        },
        Record::Turn {
            turn,
            fact: TurnFact::ToolRequested {
                execution,
                deadline: Deadline::Unbounded,
            },
        },
    ]
}

/// The records a refusal decided before anything was written carries: the launch, the request, and
/// the outcome, all in the one step a refusal takes.
#[must_use]
pub fn edit_refusal_records(
    turn: TurnID,
    execution: ExecutionID,
    reason: &str,
    observation: &str,
) -> Vec<Record> {
    let mut records = edit_requested_records(turn, execution);
    records.extend(edit_outcome_records(
        turn,
        execution,
        EditOutcome::Refused {
            reason: reason.to_owned(),
        },
        observation,
    ));

    records
}

/// The records an `Edit` outcome carries: settlement and report.
#[must_use]
pub fn edit_outcome_records(
    turn: TurnID,
    execution: ExecutionID,
    outcome: EditOutcome,
    observation: &str,
) -> Vec<Record> {
    let resolution = outcome.resolution();
    let turn_outcome = outcome.outcome();
    let mut records = vec![Record::Session {
        fact: Fact::ExecutionSettled {
            execution,
            resolution,
        },
    }];
    if matches!(outcome, EditOutcome::Applied) {
        records.push(Record::Session {
            fact: Fact::ResultApplied { execution },
        });
    }
    records.push(Record::EditOutcome(EditOutcomeRecord {
        turn,
        execution,
        report: EditReport {
            outcome,
            observation: observation.to_owned(),
        },
    }));
    records.push(Record::Turn {
        turn,
        fact: TurnFact::ToolResolved {
            execution,
            resolution,
        },
    });
    records.push(Record::Turn {
        turn,
        fact: TurnFact::TurnSettled {
            outcome: turn_outcome,
        },
    });
    records
}

#[must_use]
pub fn find_records(
    turn: TurnID,
    query: &str,
    report: &FindReport,
) -> Vec<Record> {
    vec![
        Record::Find(FindRecord {
            turn,
            query: query.to_owned(),
            report: report.clone(),
        }),
        Record::Turn {
            turn,
            fact: TurnFact::TurnSettled {
                outcome: TurnOutcome::Completed,
            },
        },
    ]
}

/// The records one activation takes: the skill record and the turn's settlement.
#[must_use]
pub fn skill_records(
    turn: TurnID,
    name: &str,
    outcome: SkillOutcome,
) -> Vec<Record> {
    let settled = outcome.outcome();
    vec![
        Record::Skill(SkillRecord {
            turn,
            name: SkillName::new(name).expect("the test names a valid skill"),
            outcome,
        }),
        Record::Turn {
            turn,
            fact: TurnFact::TurnSettled { outcome: settled },
        },
    ]
}

/// The records a `Close` step carries.
#[must_use]
pub fn close_records(
    turn: TurnID,
    settled: bool,
    ending: Option<Ending>,
) -> Vec<Record> {
    let mut records = Vec::new();
    if !settled {
        records.push(Record::Turn {
            turn,
            fact: TurnFact::TurnSettled {
                outcome: TurnOutcome::Completed,
            },
        });
    }
    if ending.is_some() {
        records.push(Record::Session {
            fact: Fact::SessionClosed,
        });
    }
    records
}

/// The report a run produces, with references that name its bytes.
#[must_use]
pub fn report(exit: Exit) -> ToolReport {
    ToolReport {
        exit,
        attempted: true,
        stdout: artifact(0x01),
        stderr: artifact(0x02),
        observation: observation(exit.resolution(), &[], &[]),
    }
}

/// Folds a batch of records the builders above produced.
pub fn apply_all(
    transcript: &mut Transcript,
    records: &[Record],
) {
    for record in records {
        transcript
            .apply(record)
            .expect("a builder produced a record the fold admits");
    }
}

/// The count is what proves a search ran again, which is the whole of what an interrupted find
/// leaves to check.
pub struct FakeFinder {
    observation: String,
    queries: Mutex<Vec<String>>,
}

impl FakeFinder {
    #[must_use]
    pub fn of(observation: &str) -> Self {
        Self {
            observation: observation.to_owned(),
            queries: Mutex::new(Vec::new()),
        }
    }

    #[must_use]
    pub fn calls(&self) -> usize {
        self.queries.lock().unwrap().len()
    }

    #[must_use]
    pub fn queries(&self) -> Vec<String> {
        self.queries.lock().unwrap().clone()
    }
}

impl Finder for FakeFinder {
    fn find(
        &self,
        query: &str,
    ) -> Result<FindReport, Error> {
        self.queries.lock().unwrap().push(query.to_owned());
        Ok(FindReport {
            observation: self.observation.clone(),
        })
    }
}

/// One queued answer per read, and the names asked, so a test can tell a load from a refusal that
/// never reached the port.
pub struct FakeSkills {
    reads: Mutex<VecDeque<Result<Read, Error>>>,
    asked: Mutex<Vec<String>>,
}

impl FakeSkills {
    #[must_use]
    pub fn of(reads: impl IntoIterator<Item = Result<Read, Error>>) -> Self {
        Self {
            reads: Mutex::new(reads.into_iter().collect()),
            asked: Mutex::new(Vec::new()),
        }
    }

    #[must_use]
    pub fn calls(&self) -> usize {
        self.asked.lock().unwrap().len()
    }

    #[must_use]
    pub fn asked(&self) -> Vec<String> {
        self.asked.lock().unwrap().clone()
    }
}

impl Skills for FakeSkills {
    fn read(
        &self,
        advert: &Advert,
    ) -> Result<Read, Error> {
        self.asked.lock().unwrap().push(advert.name.to_string());
        self.reads
            .lock()
            .unwrap()
            .pop_front()
            .expect("the fake skills holds a read")
    }
}

/// One queued answer per `plan` and per `commit`, and a count of the plans, so a test can tell a
/// replayed intent from one that planned again.
pub struct FakeEditor {
    plans: Mutex<VecDeque<Result<Plan, Error>>>,
    commits: Mutex<VecDeque<Result<EditReport, Error>>>,
    calls: AtomicUsize,
}

impl FakeEditor {
    #[must_use]
    pub fn of(
        plans: impl IntoIterator<Item = Result<Plan, Error>>,
        commits: impl IntoIterator<Item = Result<EditReport, Error>>,
    ) -> Self {
        Self {
            plans: Mutex::new(plans.into_iter().collect()),
            commits: Mutex::new(commits.into_iter().collect()),
            calls: AtomicUsize::new(0),
        }
    }

    #[must_use]
    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl Editor for FakeEditor {
    fn plan(
        &self,
        _call: &EditCall,
    ) -> Result<Plan, Error> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.plans
            .lock()
            .unwrap()
            .pop_front()
            .expect("the fake editor holds a plan")
    }

    fn commit(
        &self,
        _ready: &Ready,
    ) -> Result<EditReport, Error> {
        self.commits
            .lock()
            .unwrap()
            .pop_front()
            .expect("the fake editor holds a commit")
    }
}

/// A journal over a hand-written history: a test seeds it, drives one move, and folds the result.
pub struct TestJournal {
    session: SessionID,
    task: Task,
    records: Vec<Record>,
    // The journal lives outside the tests that read its write log, so the field crosses the module
    // boundary.
    pub(crate) written: BTreeSet<StepId>,
    puts: usize,
}

impl TestJournal {
    /// A journal for `session` whose transcript names `task` even before its record commits.
    #[must_use]
    pub fn new(
        session: SessionID,
        task: Task,
    ) -> Self {
        Self {
            session,
            task,
            records: Vec::new(),
            written: BTreeSet::new(),
            puts: 0,
        }
    }

    /// Adds `records` to the history the next transcript folds.
    pub fn seed(
        &mut self,
        records: &[Record],
    ) {
        self.records.extend_from_slice(records);
    }

    /// Marks `id` durable without committing its records, so the step that derives it replays.
    pub fn mark(
        &mut self,
        id: StepId,
    ) {
        self.written.insert(id);
    }
}

impl crate::model::Journal for TestJournal {
    fn transcript(
        &self,
        session: SessionID,
    ) -> Result<Transcript, Error> {
        let mut transcript = Transcript::replay(session, &self.records).map_err(Error::Refused)?;
        if transcript.task.is_none() {
            transcript.task = Some(self.task.clone());
        }
        Ok(transcript)
    }

    fn put(
        &mut self,
        bytes: &[u8],
        truncated: bool,
    ) -> Result<ArtifactRef, Error> {
        self.puts += 1;
        let hash = format!("{:016x}", self.puts);
        Ok(ArtifactRef {
            hash,
            bytes: bytes.len() as u64,
            truncated,
        })
    }

    fn append(
        &mut self,
        step: Step,
    ) -> Result<Receipt, Error> {
        if self.written.contains(&step.id) {
            return Ok(Receipt::Replayed);
        }

        let mut transcript =
            Transcript::replay(self.session, &self.records).map_err(Error::Refused)?;
        for record in &step.records {
            transcript.apply(record).map_err(Error::Refused)?;
        }

        self.written.insert(step.id);
        self.records.extend(step.records);
        Ok(Receipt::Written)
    }
}

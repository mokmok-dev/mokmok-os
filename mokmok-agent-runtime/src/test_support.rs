//! Builders the unit tests share, so a hand-written history is written once.

use std::sync::Mutex;
use std::time::Duration;

use mokmok_protocol::{ExecutionID, SessionID, TurnID};
use mokmok_state_machine::{Attempt, Deadline, Ending, Fact, TurnFact, TurnNumber, TurnOutcome};
use uuid::Uuid;

use crate::dialogue::Transcript;
use crate::error::Error;
use crate::find::{FindReport, Finder};
use crate::model::{Action, Usage};
use crate::record::{
    AnswerRecord, ArtifactRef, CallRecord, FindRecord, PromptRecord, Record, SpeechRecord,
    TaskRecord,
};
use crate::task::{Budget, Setting, Task};
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

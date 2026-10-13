//! The total decision over the fold, and the one bounded step that performs it.

use mokmok_protocol::{ExecutionID, Moment, SessionID, TurnID};
use mokmok_state_machine::{
    Attempt, Deadline, Ending, Fact, Phase, Turn, TurnFact, TurnNumber, TurnOutcome,
};

use crate::context::{Context, context};
use crate::dialogue::{Transcript, number_of};
use crate::error::Error;
use crate::model::{Journal, Provider};
use crate::record::{
    AnswerRecord, CallRecord, PromptRecord, Receipt, Record, Step, StepId, StepKey,
};
use crate::task::Task;
use crate::tool::{DEFAULT_DEADLINE, ToolCall, ToolReport, Tools};

/// What the loop decided to do next.
///
/// A total decision over the fold: every state yields exactly one move, and re-folding a history
/// from scratch yields the same move, which is the property the tests pin.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Move {
    /// Write the task, once, and open the session.
    Begin { task: Task },
    /// Call the model with `context` as attempt `attempt` of turn `number`.
    Ask {
        turn: TurnID,
        number: TurnNumber,
        attempt: Attempt,
        context: Context,
    },
    /// Dispatch the one command the recorded call names.
    Act {
        turn: TurnID,
        execution: ExecutionID,
        call: ToolCall,
    },
    /// Settle a turn, and close the session when `ending` is set.
    Close {
        turn: TurnID,
        outcome: TurnOutcome,
        ending: Option<Ending>,
    },
    /// Stop, for a reason no move can carry out.
    Halt(Halt),
}

/// Why the loop stopped short of a close.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Halt {
    /// The session has already closed with this ending.
    Closed(Ending),
    /// A cancelled session holds an execution the runtime cannot settle; recovery must.
    Draining,
    /// The turn ceiling is spent.
    Budget,
    /// The token ceiling is spent.
    Tokens,
    /// A turn's attempt ceiling is spent.
    Attempts,
}

/// What one step did.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Progress {
    /// The step performed this move.
    Advanced(Move),
    /// The move's intent replayed, so no effect was performed.
    Yielded,
    /// The session had already ended when the step began.
    Terminal(Ending),
}

/// The total decision over a folded session.
///
/// Pure: no clock, no I/O, and no wire. It reads only the fold, so the same fold yields the same
/// move on every invocation and on every replay.
#[must_use]
pub fn next(transcript: &Transcript) -> Move {
    match transcript.session.phase() {
        Phase::Closed(ending) => Move::Halt(Halt::Closed(ending)),
        Phase::Draining => draining(transcript),
        Phase::Unopened => transcript
            .task
            .as_ref()
            .map_or(Move::Halt(Halt::Closed(Ending::Completed)), |task| {
                Move::Begin { task: task.clone() }
            }),
        Phase::Accepting => accepting(transcript),
    }
}

/// One bounded step: read the fold, decide, and — when the move has an effect — commit its intent,
/// perform the effect, and commit the outcome.
///
/// Only the writer of an intent performs its effect: the intent is appended first, and the effect
/// runs only on [`Receipt::Written`]. A [`Receipt::Replayed`] intent yields instead, so a second
/// resume cannot execute a tool twice.
///
/// # Errors
///
/// Returns [`Error::Journal`] for a journal failure, [`Error::Provider`] for a provider failure,
/// [`Error::Tool`] for a tool failure, and [`Error::Refused`]/[`Error::Rejected`] when the fold
/// refuses the step the move would write.
pub async fn advance(
    journal: &mut impl Journal,
    provider: &impl Provider,
    tools: &impl Tools,
    session: SessionID,
    at: Moment,
) -> Result<Progress, Error> {
    tracing::debug!(
        session = %session.as_uuid(),
        at = at.as_millis(),
        "agent step",
    );

    let transcript = journal.transcript(session)?;
    let decision = next(&transcript);
    match &decision {
        Move::Halt(Halt::Closed(ending)) => return Ok(Progress::Terminal(*ending)),
        Move::Halt(halt) => {
            if *halt != Halt::Draining {
                journal.append(halt_close(session, &transcript, halt.outcome()))?;
            }
            return Ok(Progress::Advanced(decision));
        },
        Move::Begin { task } => {
            let step = begin_step(session, task);
            if journal.append(step)? == Receipt::Replayed {
                return Ok(Progress::Yielded);
            }
        },
        Move::Ask {
            turn,
            number,
            attempt,
            context,
        } => {
            let intent = ask_intent(
                journal,
                session,
                &transcript,
                *turn,
                *number,
                *attempt,
                context,
            )?;
            if journal.append(intent)? == Receipt::Replayed {
                return Ok(Progress::Yielded);
            }
            let answer = provider.complete(context).await?;
            journal.append(ask_outcome(session, *turn, *number, *attempt, answer))?;
        },
        Move::Act {
            turn,
            execution,
            call,
        } => {
            let intent = act_intent(session, &transcript, *turn, *execution, call);
            if journal.append(intent)? == Receipt::Replayed {
                return Ok(Progress::Yielded);
            }
            let report = tools.invoke(call).await?;
            journal.append(act_outcome(session, &transcript, *turn, *execution, report))?;
        },
        Move::Close {
            turn,
            outcome,
            ending,
        } => {
            let step = close_step(session, &transcript, *turn, *outcome, *ending);
            if journal.append(step)? == Receipt::Replayed {
                return Ok(Progress::Yielded);
            }
        },
    }

    Ok(Progress::Advanced(decision))
}

impl Halt {
    /// How a halt that closes the session settles the turn it names.
    const fn outcome(self) -> TurnOutcome {
        match self {
            Self::Attempts => TurnOutcome::Failed,
            Self::Closed(_) | Self::Draining | Self::Budget | Self::Tokens => {
                TurnOutcome::Completed
            },
        }
    }
}

fn draining(transcript: &Transcript) -> Move {
    let Some((turn, state)) = transcript.newest_turn() else {
        let turn = turn_id(transcript.session.id(), 1);
        return Move::Close {
            turn,
            outcome: TurnOutcome::Cancelled,
            ending: Some(Ending::Cancelled),
        };
    };

    if state.tools().any(|(_, tool)| tool.resolution().is_none()) {
        return Move::Halt(Halt::Draining);
    }

    Move::Close {
        turn,
        outcome: TurnOutcome::Cancelled,
        ending: Some(Ending::Cancelled),
    }
}

fn accepting(transcript: &Transcript) -> Move {
    let session = transcript.session.id();
    let Some(task) = transcript.task.as_ref() else {
        return Move::Halt(Halt::Closed(Ending::Completed));
    };

    if task.budget.tokens != 0 && spent_tokens(transcript) >= task.budget.tokens {
        return Move::Halt(Halt::Tokens);
    }

    let Some((turn, state)) = transcript.newest_turn() else {
        return if task.budget.turns == 0 {
            Move::Halt(Halt::Budget)
        } else {
            ask(session, 1, Attempt::new(1), transcript)
        };
    };

    let number = state.number().map_or(0, TurnNumber::get);

    if state.outcome().is_some() {
        if transcript
            .newest_answer(turn)
            .is_some_and(|answer| answer.action == crate::model::Action::Final)
        {
            return Move::Close {
                turn,
                outcome: TurnOutcome::Completed,
                ending: Some(Ending::Completed),
            };
        }

        let number = number.saturating_add(1);
        return if number > u64::from(task.budget.turns) {
            Move::Halt(Halt::Budget)
        } else {
            ask(session, number, Attempt::new(1), transcript)
        };
    }

    open_turn(turn, state, transcript)
}

fn open_turn(
    turn: TurnID,
    state: &Turn,
    transcript: &Transcript,
) -> Move {
    let number = state.number().map_or(0, TurnNumber::get);
    let attempt = state.attempts().max();

    attempt
        .and_then(|attempt| transcript.answers.get(&(turn, attempt)))
        .map_or_else(
            || reask(turn, number, state, transcript),
            |answer| match &answer.action {
                crate::model::Action::Final => Move::Close {
                    turn,
                    outcome: TurnOutcome::Completed,
                    ending: None,
                },
                crate::model::Action::Command { command } => {
                    let recorded = transcript.call_of(turn);
                    let deadline = transcript
                        .task
                        .as_ref()
                        .map_or(DEFAULT_DEADLINE, |task| task.setting.deadline);
                    let execution =
                        recorded.map_or_else(|| execution_id(turn), |call| call.execution);
                    let command =
                        recorded.map_or_else(|| command.clone(), |call| call.command.clone());
                    Move::Act {
                        turn,
                        execution,
                        call: ToolCall {
                            execution,
                            command,
                            deadline,
                        },
                    }
                },
                crate::model::Action::Malformed { .. } => reask(turn, number, state, transcript),
            },
        )
}

fn reask(
    turn: TurnID,
    number: u64,
    state: &Turn,
    transcript: &Transcript,
) -> Move {
    let asked = state.attempts().max().map_or(0, Attempt::get);
    let ceiling = transcript
        .task
        .as_ref()
        .map_or(0, |task| task.budget.attempts);

    if asked >= ceiling {
        Move::Halt(Halt::Attempts)
    } else {
        Move::Ask {
            turn,
            number: TurnNumber::new(number),
            attempt: Attempt::new(asked.saturating_add(1)),
            context: context(transcript),
        }
    }
}

fn ask(
    session: SessionID,
    number: u64,
    attempt: Attempt,
    transcript: &Transcript,
) -> Move {
    Move::Ask {
        turn: turn_id(session, number),
        number: TurnNumber::new(number),
        attempt,
        context: context(transcript),
    }
}

fn spent_tokens(transcript: &Transcript) -> u64 {
    transcript
        .answers
        .values()
        .map(|answer| answer.usage.input.saturating_add(answer.usage.output))
        .fold(0, u64::saturating_add)
}

/// The identity a turn number always derives.
#[must_use]
pub fn turn_id(
    session: SessionID,
    number: u64,
) -> TurnID {
    TurnID::from_uuid(uuid::Uuid::new_v5(
        &session.as_uuid(),
        format!("turn:{number}").as_bytes(),
    ))
}

/// The identity a turn's one execution always derives.
#[must_use]
pub fn execution_id(turn: TurnID) -> ExecutionID {
    ExecutionID::from_uuid(uuid::Uuid::new_v5(&turn.as_uuid(), b"tool"))
}

fn begin_step(
    session: SessionID,
    task: &Task,
) -> Step {
    let records = vec![
        Record::Session {
            fact: Fact::SessionOpened,
        },
        Record::Task(crate::record::TaskRecord { task: task.clone() }),
    ];
    Step::new(StepId::derive(session, StepKey::Task), session, records)
}

fn ask_intent(
    journal: &mut impl Journal,
    session: SessionID,
    transcript: &Transcript,
    turn: TurnID,
    number: TurnNumber,
    attempt: Attempt,
    context: &Context,
) -> Result<Step, Error> {
    let bytes = serde_json::to_vec(context).map_err(|error| Error::Journal(Box::new(error)))?;
    let reference = journal.put(&bytes, false)?;

    let mut records = Vec::new();
    if transcript.turn(turn).is_none() {
        records.push(Record::Turn {
            turn,
            fact: TurnFact::TurnOpened { number },
        });
    }
    records.push(Record::Turn {
        turn,
        fact: TurnFact::ModelRequested { attempt },
    });
    records.push(Record::Prompt(PromptRecord {
        turn,
        attempt,
        context: reference,
    }));

    let key = StepKey::AskIntent {
        turn: number.get(),
        attempt: attempt.get(),
    };
    Ok(Step::new(StepId::derive(session, key), session, records))
}

fn ask_outcome(
    session: SessionID,
    turn: TurnID,
    number: TurnNumber,
    attempt: Attempt,
    answer: crate::model::Answer,
) -> Step {
    let records = vec![
        Record::Turn {
            turn,
            fact: TurnFact::ModelAnswered,
        },
        Record::Answer(AnswerRecord {
            turn,
            attempt,
            action: answer.action,
            text: answer.text,
            model: answer.model,
            usage: answer.usage,
        }),
    ];
    let key = StepKey::AskOutcome {
        turn: number.get(),
        attempt: attempt.get(),
    };
    Step::new(StepId::derive(session, key), session, records)
}

fn act_intent(
    session: SessionID,
    transcript: &Transcript,
    turn: TurnID,
    execution: ExecutionID,
    call: &ToolCall,
) -> Step {
    let number = number_of(transcript, turn);
    let records = vec![
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
            command: call.command.clone(),
        }),
    ];
    Step::new(
        StepId::derive(session, StepKey::ActIntent { turn: number }),
        session,
        records,
    )
}

fn act_outcome(
    session: SessionID,
    transcript: &Transcript,
    turn: TurnID,
    execution: ExecutionID,
    report: ToolReport,
) -> Step {
    let number = number_of(transcript, turn);
    let resolution = report.exit.resolution();
    let outcome = report.exit.outcome();

    let mut records = vec![Record::Session {
        fact: Fact::ExecutionSettled {
            execution,
            resolution,
        },
    }];
    if matches!(report.exit, crate::tool::Exit::Finished { .. }) {
        records.push(Record::Session {
            fact: Fact::ResultApplied { execution },
        });
    }
    records.push(Record::Result { execution, report });
    records.push(Record::Turn {
        turn,
        fact: TurnFact::ToolResolved {
            execution,
            resolution,
        },
    });
    records.push(Record::Turn {
        turn,
        fact: TurnFact::TurnSettled { outcome },
    });

    Step::new(
        StepId::derive(session, StepKey::ActOutcome { turn: number }),
        session,
        records,
    )
}

fn close_step(
    session: SessionID,
    transcript: &Transcript,
    turn: TurnID,
    outcome: TurnOutcome,
    ending: Option<Ending>,
) -> Step {
    let number = number_of(transcript, turn);
    let mut records = Vec::new();
    if transcript
        .turn(turn)
        .is_some_and(|state| state.outcome().is_none())
    {
        records.push(Record::Turn {
            turn,
            fact: TurnFact::TurnSettled { outcome },
        });
    }
    if ending.is_some() {
        records.push(Record::Session {
            fact: Fact::SessionClosed,
        });
    }
    Step::new(
        StepId::derive(
            session,
            StepKey::Close {
                turn: number,
                session: ending.is_some(),
            },
        ),
        session,
        records,
    )
}

fn halt_close(
    session: SessionID,
    transcript: &Transcript,
    outcome: TurnOutcome,
) -> Step {
    let mut records = Vec::new();
    let number = match transcript.newest_turn() {
        Some((turn, state)) => {
            if state.outcome().is_none() {
                records.push(Record::Turn {
                    turn,
                    fact: TurnFact::TurnSettled { outcome },
                });
            }
            state.number().map_or(0, TurnNumber::get)
        },
        None => 0,
    };
    records.push(Record::Session {
        fact: Fact::SessionClosed,
    });
    Step::new(
        StepId::derive(
            session,
            StepKey::Close {
                turn: number,
                session: true,
            },
        ),
        session,
        records,
    )
}

#[cfg(test)]
mod tests {
    use super::{Halt, Move, Progress, advance, next};
    use crate::context::Context;
    use crate::dialogue::Transcript;
    use crate::error::Error;
    use crate::model::{Action, Answer, Journal, Provider, Usage};
    use crate::record::{ArtifactRef, Receipt, Record, Step, StepId, StepKey};
    use crate::task::Task;
    use crate::test_support::{
        act_intent_records, act_outcome_records, answer_records, apply_all, artifact,
        begin_records, execution_id, open_records, report, session, task, turn_id,
    };
    use crate::tool::{Exit, ToolCall, ToolReport, Tools};
    use mokmok_protocol::{Moment, SessionID};
    use mokmok_state_machine::{Attempt, Ending, Fact, Phase, TurnNumber, TurnOutcome};
    use std::collections::{BTreeSet, VecDeque};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn next_walks_a_transcript_through_begin_ask_act_and_close() {
        let session = session();
        let task = task(4);
        let mut transcript = Transcript::new(session);
        transcript.task = Some(task.clone());

        assert_eq!(next(&transcript), Move::Begin { task: task.clone() },);

        apply_all(&mut transcript, &begin_records(&task));
        let turn = turn_id(1);

        let first = next(&transcript);
        assert_eq!(
            first,
            Move::Ask {
                turn,
                number: TurnNumber::new(1),
                attempt: Attempt::new(1),
                context: crate::context::context(&transcript),
            },
        );

        apply_all(&mut transcript, &open_records(turn, 1, 1));
        apply_all(
            &mut transcript,
            &answer_records(
                turn,
                1,
                Action::Command {
                    command: "ls".to_owned(),
                },
            ),
        );

        let second = next(&transcript);
        let Move::Act {
            turn: acted, call, ..
        } = second
        else {
            panic!("expected an act");
        };
        assert_eq!(acted, turn);
        assert_eq!(call.command, "ls");

        let execution = execution_id(turn);
        apply_all(&mut transcript, &act_intent_records(turn, execution, "ls"));
        apply_all(
            &mut transcript,
            &act_outcome_records(turn, execution, Exit::Finished { code: 0 }, b"ok"),
        );

        let third = next(&transcript);
        assert_eq!(
            third,
            Move::Ask {
                turn: turn_id(2),
                number: TurnNumber::new(2),
                attempt: Attempt::new(1),
                context: crate::context::context(&transcript),
            },
        );
    }

    #[test]
    fn a_final_answer_settles_and_then_closes_the_session() {
        let session = session();
        let task = task(4);
        let mut transcript = Transcript::new(session);
        apply_all(&mut transcript, &begin_records(&task));
        let turn = turn_id(1);
        apply_all(&mut transcript, &open_records(turn, 1, 1));
        apply_all(&mut transcript, &answer_records(turn, 1, Action::Final));

        assert_eq!(
            next(&transcript),
            Move::Close {
                turn,
                outcome: TurnOutcome::Completed,
                ending: None,
            },
        );

        apply_all(
            &mut transcript,
            &crate::test_support::close_records(turn, false, None),
        );
        assert_eq!(
            next(&transcript),
            Move::Close {
                turn,
                outcome: TurnOutcome::Completed,
                ending: Some(Ending::Completed),
            },
        );

        apply_all(
            &mut transcript,
            &crate::test_support::close_records(turn, true, Some(Ending::Completed)),
        );
        assert_eq!(
            next(&transcript),
            Move::Halt(Halt::Closed(Ending::Completed)),
        );
    }

    struct TestJournal {
        session: SessionID,
        task: Task,
        records: Vec<Record>,
        written: BTreeSet<StepId>,
        puts: usize,
    }

    impl TestJournal {
        fn new(
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

        fn seed(
            &mut self,
            records: &[Record],
        ) {
            self.records.extend_from_slice(records);
        }

        fn mark(
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
            let mut transcript =
                Transcript::replay(session, &self.records).map_err(Error::Refused)?;
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

    struct ScriptedProvider {
        answers: Mutex<VecDeque<Answer>>,
        calls: AtomicUsize,
    }

    impl ScriptedProvider {
        fn of(answers: impl IntoIterator<Item = Answer>) -> Self {
            Self {
                answers: Mutex::new(answers.into_iter().collect()),
                calls: AtomicUsize::new(0),
            }
        }
    }

    impl Provider for ScriptedProvider {
        fn complete(
            &self,
            _context: &Context,
        ) -> impl Future<Output = Result<Answer, Error>> + Send {
            self.calls.fetch_add(1, Ordering::SeqCst);
            std::future::ready(Ok(self
                .answers
                .lock()
                .unwrap()
                .pop_front()
                .expect("the script holds an answer")))
        }
    }

    struct FakeTools {
        reports: Mutex<VecDeque<ToolReport>>,
        calls: AtomicUsize,
    }

    impl FakeTools {
        fn of(reports: impl IntoIterator<Item = ToolReport>) -> Self {
            Self {
                reports: Mutex::new(reports.into_iter().collect()),
                calls: AtomicUsize::new(0),
            }
        }
    }

    impl Tools for FakeTools {
        fn invoke(
            &self,
            _call: &ToolCall,
        ) -> impl Future<Output = Result<ToolReport, Error>> + Send {
            self.calls.fetch_add(1, Ordering::SeqCst);
            std::future::ready(Ok(self
                .reports
                .lock()
                .unwrap()
                .pop_front()
                .expect("the script holds a report")))
        }
    }

    fn answer(action: Action) -> Answer {
        Answer {
            action,
            text: artifact(0xbb),
            model: "test".to_owned(),
            usage: Usage {
                input: 1,
                output: 1,
            },
        }
    }

    #[tokio::test]
    async fn the_loop_runs_the_model_then_the_tool_then_closes() {
        let session = session();
        let task = task(4);
        let mut journal = TestJournal::new(session, task.clone());
        let provider = ScriptedProvider::of([
            answer(Action::Command {
                command: "ls".to_owned(),
            }),
            answer(Action::Final),
        ]);
        let tools = FakeTools::of([report(Exit::Finished { code: 0 })]);

        let progress = advance(&mut journal, &provider, &tools, session, Moment::EPOCH)
            .await
            .unwrap();
        assert!(matches!(progress, Progress::Advanced(Move::Begin { .. })));

        let progress = advance(&mut journal, &provider, &tools, session, Moment::EPOCH)
            .await
            .unwrap();
        assert!(matches!(progress, Progress::Advanced(Move::Ask { .. })));

        let progress = advance(&mut journal, &provider, &tools, session, Moment::EPOCH)
            .await
            .unwrap();
        assert!(matches!(progress, Progress::Advanced(Move::Act { .. })));

        let progress = advance(&mut journal, &provider, &tools, session, Moment::EPOCH)
            .await
            .unwrap();
        assert!(matches!(progress, Progress::Advanced(Move::Ask { .. })));

        let progress = advance(&mut journal, &provider, &tools, session, Moment::EPOCH)
            .await
            .unwrap();
        assert!(matches!(
            progress,
            Progress::Advanced(Move::Close { ending: None, .. })
        ));

        let progress = advance(&mut journal, &provider, &tools, session, Moment::EPOCH)
            .await
            .unwrap();
        assert_eq!(
            progress,
            Progress::Advanced(Move::Close {
                turn: turn_id(2),
                outcome: TurnOutcome::Completed,
                ending: Some(Ending::Completed),
            }),
        );

        let progress = advance(&mut journal, &provider, &tools, session, Moment::EPOCH)
            .await
            .unwrap();
        assert_eq!(progress, Progress::Terminal(Ending::Completed));

        assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
        assert_eq!(tools.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_replayed_intent_performs_no_effect() {
        let session = session();
        let task = task(4);
        let mut journal = TestJournal::new(session, task.clone());
        let turn = turn_id(1);
        let execution = execution_id(turn);
        journal.seed(&begin_records(&task));
        journal.seed(&open_records(turn, 1, 1));
        journal.seed(&answer_records(
            turn,
            1,
            Action::Command {
                command: "ls".to_owned(),
            },
        ));
        journal.seed(&act_intent_records(turn, execution, "ls"));
        journal.mark(StepId::derive(session, StepKey::ActIntent { turn: 1 }));

        let provider = ScriptedProvider::of([]);
        let tools = FakeTools::of([report(Exit::Finished { code: 0 })]);

        let progress = advance(&mut journal, &provider, &tools, session, Moment::EPOCH)
            .await
            .unwrap();
        assert_eq!(progress, Progress::Yielded);
        assert_eq!(tools.calls.load(Ordering::SeqCst), 0);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    }

    fn command_transcript(
        task: &Task,
        attempts: u32,
    ) -> Transcript {
        let turn = turn_id(1);
        let mut transcript = Transcript::new(session());
        apply_all(&mut transcript, &begin_records(task));
        for attempt in 1..=attempts {
            apply_all(&mut transcript, &open_records(turn, 1, attempt));
        }
        transcript
    }

    #[test]
    fn the_token_ceiling_halts_before_the_next_call() {
        let mut task = task(4);
        task.budget.tokens = 2;
        let turn = turn_id(1);
        let mut transcript = Transcript::new(session());
        apply_all(&mut transcript, &begin_records(&task));
        apply_all(&mut transcript, &open_records(turn, 1, 1));
        apply_all(
            &mut transcript,
            &answer_records(
                turn,
                1,
                Action::Command {
                    command: "ls".to_owned(),
                },
            ),
        );

        // One answer costs two tokens, so the ceiling of two is exactly spent.
        assert_eq!(next(&transcript), Move::Halt(Halt::Tokens));

        transcript.task.as_mut().unwrap().budget.tokens = 3;
        assert!(matches!(next(&transcript), Move::Act { .. }));
    }

    #[test]
    fn the_turn_ceiling_allows_the_turn_that_ends_at_it() {
        let task = task(2);
        let first = turn_id(1);
        let mut transcript = Transcript::new(session());
        apply_all(&mut transcript, &begin_records(&task));
        apply_all(&mut transcript, &open_records(first, 1, 1));
        apply_all(
            &mut transcript,
            &answer_records(
                first,
                1,
                Action::Command {
                    command: "ls".to_owned(),
                },
            ),
        );
        apply_all(
            &mut transcript,
            &crate::test_support::close_records(first, false, None),
        );

        assert!(
            matches!(next(&transcript), Move::Ask { number, .. } if number == TurnNumber::new(2)),
        );
    }

    #[test]
    fn a_settled_turn_number_past_the_ceiling_halts_the_session() {
        let task = task(2);
        let second = turn_id(2);
        let mut transcript = Transcript::new(session());
        apply_all(&mut transcript, &begin_records(&task));
        apply_all(&mut transcript, &open_records(second, 2, 1));
        apply_all(
            &mut transcript,
            &answer_records(
                second,
                1,
                Action::Command {
                    command: "ls".to_owned(),
                },
            ),
        );
        apply_all(
            &mut transcript,
            &crate::test_support::close_records(second, false, None),
        );

        assert_eq!(next(&transcript), Move::Halt(Halt::Budget));
    }

    #[test]
    fn the_attempt_ceiling_halts_a_turn_that_keeps_being_asked() {
        let transcript = command_transcript(&task(4), 3);

        assert_eq!(next(&transcript), Move::Halt(Halt::Attempts));
    }

    #[test]
    fn a_turn_below_the_attempt_ceiling_asks_again() {
        let transcript = command_transcript(&task(4), 2);

        assert!(
            matches!(next(&transcript), Move::Ask { attempt, .. } if attempt == Attempt::new(3)),
        );
    }

    fn cancelled_transcript(settled: bool) -> Transcript {
        let task = task(4);
        let turn = turn_id(1);
        let execution = execution_id(turn);
        let mut transcript = Transcript::new(session());
        apply_all(&mut transcript, &begin_records(&task));
        apply_all(&mut transcript, &open_records(turn, 1, 1));
        apply_all(
            &mut transcript,
            &answer_records(
                turn,
                1,
                Action::Command {
                    command: "ls".to_owned(),
                },
            ),
        );
        apply_all(&mut transcript, &act_intent_records(turn, execution, "ls"));
        if settled {
            apply_all(
                &mut transcript,
                &act_outcome_records(turn, execution, Exit::Finished { code: 0 }, b"ok"),
            );
        }
        apply_all(
            &mut transcript,
            &[Record::Session {
                fact: Fact::CancelRequested,
            }],
        );
        transcript
    }

    #[test]
    fn draining_halts_while_an_execution_is_unsettled() {
        assert_eq!(
            next(&cancelled_transcript(false)),
            Move::Halt(Halt::Draining),
        );
    }

    #[test]
    fn draining_closes_cancelled_once_every_execution_settled() {
        let turn = turn_id(1);

        assert_eq!(
            next(&cancelled_transcript(true)),
            Move::Close {
                turn,
                outcome: TurnOutcome::Cancelled,
                ending: Some(Ending::Cancelled),
            },
        );
    }

    #[test]
    fn draining_with_no_turn_closes_the_first_turn() {
        let task = task(4);
        let mut transcript = Transcript::new(session());
        apply_all(&mut transcript, &begin_records(&task));
        apply_all(
            &mut transcript,
            &[Record::Session {
                fact: Fact::CancelRequested,
            }],
        );

        assert_eq!(
            next(&transcript),
            Move::Close {
                turn: turn_id(1),
                outcome: TurnOutcome::Cancelled,
                ending: Some(Ending::Cancelled),
            },
        );
    }

    #[tokio::test]
    async fn a_draining_halt_does_not_close_the_session() {
        let session = session();
        let task = task(4);
        let turn = turn_id(1);
        let execution = execution_id(turn);
        let mut journal = TestJournal::new(session, task.clone());
        journal.seed(&begin_records(&task));
        journal.seed(&open_records(turn, 1, 1));
        journal.seed(&answer_records(
            turn,
            1,
            Action::Command {
                command: "ls".to_owned(),
            },
        ));
        journal.seed(&act_intent_records(turn, execution, "ls"));
        journal.seed(&[Record::Session {
            fact: Fact::CancelRequested,
        }]);

        let provider = ScriptedProvider::of([]);
        let tools = FakeTools::of([]);
        let progress = advance(&mut journal, &provider, &tools, session, Moment::EPOCH)
            .await
            .unwrap();

        assert_eq!(progress, Progress::Advanced(Move::Halt(Halt::Draining)));
        assert_eq!(
            journal.transcript(session).unwrap().session.phase(),
            Phase::Draining,
        );
    }

    #[tokio::test]
    async fn a_token_halt_closes_the_session() {
        let session = session();
        let mut task = task(4);
        task.budget.tokens = 2;
        let turn = turn_id(1);
        let mut journal = TestJournal::new(session, task.clone());
        journal.seed(&begin_records(&task));
        journal.seed(&open_records(turn, 1, 1));
        journal.seed(&answer_records(
            turn,
            1,
            Action::Command {
                command: "ls".to_owned(),
            },
        ));

        let provider = ScriptedProvider::of([]);
        let tools = FakeTools::of([]);
        let progress = advance(&mut journal, &provider, &tools, session, Moment::EPOCH)
            .await
            .unwrap();

        assert_eq!(progress, Progress::Advanced(Move::Halt(Halt::Tokens)));
        assert_eq!(
            journal.transcript(session).unwrap().session.phase(),
            Phase::Closed(Ending::Completed),
        );
    }

    #[tokio::test]
    async fn an_attempt_halt_settles_the_turn_failed() {
        let session = session();
        let task = task(4);
        let mut journal = TestJournal::new(session, task.clone());
        journal.seed(&begin_records(&task));
        for attempt in 1..=3 {
            journal.seed(&open_records(turn_id(1), 1, attempt));
        }

        let provider = ScriptedProvider::of([]);
        let tools = FakeTools::of([]);
        let progress = advance(&mut journal, &provider, &tools, session, Moment::EPOCH)
            .await
            .unwrap();

        assert_eq!(progress, Progress::Advanced(Move::Halt(Halt::Attempts)));
        let transcript = journal.transcript(session).unwrap();
        assert_eq!(transcript.session.phase(), Phase::Closed(Ending::Completed));
        let (_, turn) = transcript.newest_turn().unwrap();
        assert_eq!(turn.outcome(), Some(TurnOutcome::Failed));
    }

    #[tokio::test]
    async fn a_final_answer_settles_the_turn_then_closes_the_session() {
        let session = session();
        let task = task(4);
        let turn = turn_id(1);
        let mut journal = TestJournal::new(session, task.clone());
        journal.seed(&begin_records(&task));
        journal.seed(&open_records(turn, 1, 1));
        journal.seed(&answer_records(turn, 1, Action::Final));

        let provider = ScriptedProvider::of([]);
        let tools = FakeTools::of([]);

        let first = advance(&mut journal, &provider, &tools, session, Moment::EPOCH)
            .await
            .unwrap();
        assert_eq!(
            first,
            Progress::Advanced(Move::Close {
                turn,
                outcome: TurnOutcome::Completed,
                ending: None,
            }),
        );
        assert_eq!(
            journal.transcript(session).unwrap().session.phase(),
            Phase::Accepting,
        );

        let second = advance(&mut journal, &provider, &tools, session, Moment::EPOCH)
            .await
            .unwrap();
        assert_eq!(
            second,
            Progress::Advanced(Move::Close {
                turn,
                outcome: TurnOutcome::Completed,
                ending: Some(Ending::Completed),
            }),
        );
        assert_eq!(
            journal.transcript(session).unwrap().session.phase(),
            Phase::Closed(Ending::Completed),
        );
    }
}

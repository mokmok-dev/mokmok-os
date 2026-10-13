//! The fold of a session's records, and the rules that refuse a malformed history.

use std::collections::BTreeMap;

use mokmok_protocol::{ExecutionID, SessionID, TurnID};
use mokmok_state_machine::{
    Ack, Attempt, Rejection, Resolution, Session, State, Turn, TurnFact, TurnNumber,
};

use crate::model::Action;
use crate::record::{
    AnswerRecord, ArtifactRef, CallRecord, PromptRecord, Record, RecordKey, Refusal, TaskRecord,
};
use crate::task::Task;
use crate::tool::{self, ToolReport};

/// The session's whole durable past, folded.
///
/// The driving path and a replay-from-scratch run the same [`Record::apply`], so they cannot
/// drift, and re-folding a history is unchanged by how it was cut into steps.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Transcript {
    /// The session machine's state.
    pub session: Session,
    /// The task the session runs: seeded by the journal before its record commits, then replaced
    /// by the committed one.
    pub task: Option<Task>,
    /// One turn's state per turn, in the order the folds first saw them.
    pub turns: Vec<(TurnID, Turn)>,
    /// The exact context bytes one attempt was handed.
    pub prompts: BTreeMap<(TurnID, Attempt), ArtifactRef>,
    /// The action one attempt answered with.
    pub answers: BTreeMap<(TurnID, Attempt), AnswerRecord>,
    /// The command one execution is about to dispatch.
    pub calls: BTreeMap<ExecutionID, CallRecord>,
    /// The report each result record carried, so a redelivery is compared as the whole record and
    /// the observation it holds is what the model was shown.
    pub runs: BTreeMap<ExecutionID, ToolReport>,
}

impl Transcript {
    /// A fold that holds nothing yet.
    #[must_use]
    pub const fn new(session: SessionID) -> Self {
        Self {
            session: Session::new(session),
            task: None,
            turns: Vec::new(),
            prompts: BTreeMap::new(),
            answers: BTreeMap::new(),
            calls: BTreeMap::new(),
            runs: BTreeMap::new(),
        }
    }

    /// Rebuilds the fold by folding a whole history again.
    ///
    /// # Errors
    ///
    /// Returns the [`Refusal`] of the first record the history refuses.
    pub fn replay(
        session: SessionID,
        records: &[Record],
    ) -> Result<Self, Refusal> {
        let mut transcript = Self::new(session);
        for record in records {
            record.apply(&mut transcript)?;
        }

        Ok(transcript)
    }

    /// Folds one record in.
    ///
    /// # Errors
    ///
    /// Returns the [`Refusal`] [`Record::apply`] reports.
    pub fn apply(
        &mut self,
        record: &Record,
    ) -> Result<Ack, Refusal> {
        record.apply(self)
    }

    /// One turn's state, if the fold holds it.
    #[must_use]
    pub fn turn(
        &self,
        id: TurnID,
    ) -> Option<&Turn> {
        self.turns
            .iter()
            .find(|(turn, _)| *turn == id)
            .map(|(_, turn)| turn)
    }

    /// The turns in the order their numbers name, newest last.
    #[must_use]
    pub fn turns_in_order(&self) -> Vec<(TurnID, &Turn)> {
        let mut ordered: Vec<(TurnID, &Turn)> =
            self.turns.iter().map(|(id, turn)| (*id, turn)).collect();
        ordered.sort_by_key(|(_, turn)| turn.number());
        ordered
    }

    /// The newest turn by number, if the fold holds any.
    #[must_use]
    pub fn newest_turn(&self) -> Option<(TurnID, &Turn)> {
        self.turns
            .iter()
            .max_by_key(|(_, turn)| turn.number())
            .map(|(id, turn)| (*id, turn))
    }

    /// The call one turn recorded, if any.
    #[must_use]
    pub fn call_of(
        &self,
        turn: TurnID,
    ) -> Option<&CallRecord> {
        self.calls.values().find(|call| call.turn == turn)
    }

    /// The newest attempt's answer for one turn, if the fold holds one.
    #[must_use]
    pub fn newest_answer(
        &self,
        turn: TurnID,
    ) -> Option<&AnswerRecord> {
        self.answers
            .iter()
            .filter(|((recorded, _), _)| *recorded == turn)
            .max_by_key(|((_, attempt), _)| *attempt)
            .map(|(_, answer)| answer)
    }

    /// The observation one turn's tool produced, rendered from the fold.
    ///
    /// A recorded report carries the rendering the model was already shown; a settlement with no
    /// recorded report — the shape recovery writes — has no bytes, so it renders from the turn's
    /// settlement facts alone.
    #[must_use]
    pub fn observation(
        &self,
        turn: TurnID,
    ) -> Option<String> {
        let state = self.turn(turn)?;
        let (execution, tool) = state.tools().next()?;
        if let Some(report) = self.runs.get(&execution) {
            return Some(report.observation.clone());
        }

        let resolution = tool.resolution()?;
        Some(tool::observation(resolution, &[], &[]))
    }
}

impl Record {
    /// Folds one record into `into`, or reports why it cannot be folded.
    ///
    /// Two rules are enforced here rather than by convention: a content record whose machine fact
    /// the fold does not hold is refused as floating, and a second record under one key with
    /// different content is refused as a conflict. Identical content under one key is
    /// [`Ack::Duplicate`].
    ///
    /// # Errors
    ///
    /// Returns [`Refusal::Floating`], [`Refusal::Conflict`], [`Refusal::CommandMismatch`], or the
    /// [`Refusal::Rejected`] of the state machine that owns the fact.
    pub fn apply(
        &self,
        into: &mut Transcript,
    ) -> Result<Ack, Refusal> {
        match self {
            Self::Session { fact } => into.session.apply(*fact).map_err(Refusal::Rejected),
            Self::Turn { turn, fact } => apply_turn(into, *turn, *fact),
            Self::Result { execution, report } => apply_result(into, *execution, report),
            Self::Task(record) => apply_task(into, record),
            Self::Prompt(record) => apply_prompt(into, record),
            Self::Answer(record) => apply_answer(into, record),
            Self::Call(record) => apply_call(into, record),
        }
    }
}

fn apply_turn(
    into: &mut Transcript,
    turn: TurnID,
    fact: TurnFact,
) -> Result<Ack, Refusal> {
    if let Some((_, recorded)) = into.turns.iter_mut().find(|(id, _)| *id == turn) {
        return recorded.apply(fact).map_err(Refusal::Rejected);
    }

    if !matches!(fact, TurnFact::TurnOpened { .. }) {
        return Err(Refusal::Rejected(Rejection::NotOpened));
    }

    let mut fresh = Turn::new();
    let ack = fresh.apply(fact).map_err(Refusal::Rejected)?;
    into.turns.push((turn, fresh));
    Ok(ack)
}

fn apply_result(
    into: &mut Transcript,
    execution: ExecutionID,
    report: &ToolReport,
) -> Result<Ack, Refusal> {
    let key = RecordKey::Result(execution);
    if let Some(recorded) = into.runs.get(&execution) {
        return if recorded == report {
            Ok(Ack::Duplicate)
        } else {
            Err(Refusal::Conflict(key))
        };
    }

    if into
        .turns
        .iter()
        .all(|(_, turn)| turn.tool(execution).is_none())
    {
        return Err(Refusal::Floating(key));
    }

    let resolution = report.exit.resolution();
    let settled = match into.session.execution(execution).map(|held| held.state()) {
        Some(State::Settled(recorded)) => recorded == resolution,
        Some(State::Applied { code }) => resolution == Resolution::Finished { code },
        Some(State::InFlight) | None => false,
    };
    if !settled {
        return Err(Refusal::Floating(key));
    }

    into.runs.insert(execution, report.clone());
    Ok(Ack::Applied)
}

fn apply_task(
    into: &mut Transcript,
    record: &TaskRecord,
) -> Result<Ack, Refusal> {
    match &into.task {
        Some(recorded) if recorded == &record.task => Ok(Ack::Duplicate),
        Some(_) => Err(Refusal::Conflict(RecordKey::Task)),
        None => {
            into.task = Some(record.task.clone());
            Ok(Ack::Applied)
        },
    }
}

fn apply_prompt(
    into: &mut Transcript,
    record: &PromptRecord,
) -> Result<Ack, Refusal> {
    let key = RecordKey::Prompt {
        turn: record.turn,
        attempt: record.attempt,
    };
    if let Some(recorded) = into.prompts.get(&(record.turn, record.attempt)) {
        return if recorded == &record.context {
            Ok(Ack::Duplicate)
        } else {
            Err(Refusal::Conflict(key))
        };
    }

    let requested = into
        .turn(record.turn)
        .is_some_and(|turn| turn.attempts().any(|attempt| attempt == record.attempt));
    if !requested {
        return Err(Refusal::Floating(key));
    }

    into.prompts
        .insert((record.turn, record.attempt), record.context.clone());
    Ok(Ack::Applied)
}

fn apply_answer(
    into: &mut Transcript,
    record: &AnswerRecord,
) -> Result<Ack, Refusal> {
    let key = RecordKey::Answer {
        turn: record.turn,
        attempt: record.attempt,
    };
    if let Some(recorded) = into.answers.get(&(record.turn, record.attempt)) {
        return if recorded == record {
            Ok(Ack::Duplicate)
        } else {
            Err(Refusal::Conflict(key))
        };
    }

    if !into.turn(record.turn).is_some_and(Turn::answered) {
        return Err(Refusal::Floating(key));
    }

    into.answers
        .insert((record.turn, record.attempt), record.clone());
    Ok(Ack::Applied)
}

fn apply_call(
    into: &mut Transcript,
    record: &CallRecord,
) -> Result<Ack, Refusal> {
    if let Some(recorded) = into.calls.get(&record.execution) {
        return if recorded == record {
            Ok(Ack::Duplicate)
        } else {
            Err(Refusal::Conflict(RecordKey::Call(record.execution)))
        };
    }

    let requested = into
        .turn(record.turn)
        .is_some_and(|turn| turn.tool(record.execution).is_some());
    if !requested {
        return Err(Refusal::Floating(RecordKey::Call(record.execution)));
    }

    if let Some(Action::Command { command }) =
        into.newest_answer(record.turn).map(|answer| &answer.action)
        && command != &record.command
    {
        return Err(Refusal::CommandMismatch {
            turn: record.turn,
            answer: command.clone(),
            call: record.command.clone(),
        });
    }

    into.calls.insert(record.execution, record.clone());
    Ok(Ack::Applied)
}

/// The turn number a turn names, or zero when the fold does not hold it.
#[must_use]
pub fn number_of(
    transcript: &Transcript,
    turn: TurnID,
) -> u64 {
    transcript
        .turn(turn)
        .and_then(Turn::number)
        .map_or(0, TurnNumber::get)
}

#[cfg(test)]
mod tests {
    use super::{Record, Transcript, number_of};
    use crate::model::{Action, Usage};
    use crate::record::{
        AnswerRecord, ArtifactRef, CallRecord, PromptRecord, RecordKey, Refusal, TaskRecord,
    };
    use crate::task::{Budget, Setting, Task};
    use crate::test_support::{artifact, execution_id, session, task, turn_id};
    use crate::tool::{Exit, ToolReport};
    use mokmok_protocol::{ExecutionID, TurnID};
    use mokmok_state_machine::{
        Ack, Attempt, Deadline, Fact, Resolution, TurnFact, TurnNumber, TurnOutcome,
    };
    use proptest::prelude::*;
    use std::time::Duration;

    fn open(
        transcript: &mut Transcript,
        turn: TurnID,
        number: u64,
    ) {
        transcript
            .apply(&Record::Turn {
                turn,
                fact: TurnFact::TurnOpened {
                    number: TurnNumber::new(number),
                },
            })
            .unwrap();
    }

    fn request(
        transcript: &mut Transcript,
        turn: TurnID,
        attempt: u32,
    ) {
        transcript
            .apply(&Record::Turn {
                turn,
                fact: TurnFact::ModelRequested {
                    attempt: Attempt::new(attempt),
                },
            })
            .unwrap();
    }

    fn prompt(
        turn: TurnID,
        context: ArtifactRef,
    ) -> Record {
        Record::Prompt(PromptRecord {
            turn,
            attempt: Attempt::new(1),
            context,
        })
    }

    fn answer(
        turn: TurnID,
        action: Action,
    ) -> Record {
        Record::Answer(AnswerRecord {
            turn,
            attempt: Attempt::new(1),
            action,
            text: artifact(0xaa),
            model: "test".to_owned(),
            usage: Usage {
                input: 1,
                output: 1,
            },
        })
    }

    fn call(
        turn: TurnID,
        execution: ExecutionID,
    ) -> Record {
        Record::Call(CallRecord {
            turn,
            execution,
            command: "ls".to_owned(),
        })
    }

    fn answered(
        transcript: &mut Transcript,
        turn: TurnID,
    ) {
        transcript
            .apply(&Record::Turn {
                turn,
                fact: TurnFact::ModelAnswered,
            })
            .unwrap();
    }

    fn requested(
        transcript: &mut Transcript,
        turn: TurnID,
        execution: ExecutionID,
    ) {
        transcript
            .apply(&Record::Session {
                fact: Fact::ExecutionLaunched {
                    execution,
                    deadline: Deadline::Bounded,
                },
            })
            .unwrap();
        transcript
            .apply(&Record::Turn {
                turn,
                fact: TurnFact::ToolRequested {
                    execution,
                    deadline: Deadline::Bounded,
                },
            })
            .unwrap();
    }

    fn settle(
        transcript: &mut Transcript,
        execution: ExecutionID,
        resolution: Resolution,
    ) {
        transcript
            .apply(&Record::Session {
                fact: Fact::ExecutionSettled {
                    execution,
                    resolution,
                },
            })
            .unwrap();
    }

    fn result(
        execution: ExecutionID,
        exit: Exit,
        output: &[u8],
    ) -> Record {
        Record::Result {
            execution,
            report: ToolReport {
                exit,
                attempted: true,
                stdout: artifact(0x01),
                stderr: artifact(0x02),
                observation: crate::tool::observation(exit.resolution(), output, &[]),
            },
        }
    }

    fn a_requested_turn(execution: ExecutionID) -> Transcript {
        let turn = turn_id(1);
        let mut transcript = Transcript::new(session());
        transcript
            .apply(&Record::Session {
                fact: Fact::SessionOpened,
            })
            .unwrap();
        open(&mut transcript, turn, 1);
        request(&mut transcript, turn, 1);
        answered(&mut transcript, turn);
        transcript
            .apply(&answer(
                turn,
                Action::Command {
                    command: "ls".to_owned(),
                },
            ))
            .unwrap();
        requested(&mut transcript, turn, execution);
        transcript
    }

    #[test]
    fn a_prompt_without_its_request_is_refused() {
        let turn = turn_id(1);
        let mut transcript = Transcript::new(session());
        open(&mut transcript, turn, 1);

        let refusal = transcript.apply(&prompt(turn, artifact(1))).unwrap_err();
        assert_eq!(
            refusal,
            Refusal::Floating(RecordKey::Prompt {
                turn,
                attempt: Attempt::new(1),
            }),
        );
    }

    #[test]
    fn an_answer_without_its_answer_fact_is_refused() {
        let turn = turn_id(1);
        let mut transcript = Transcript::new(session());
        open(&mut transcript, turn, 1);
        request(&mut transcript, turn, 1);

        let refusal = transcript.apply(&answer(turn, Action::Final)).unwrap_err();
        assert_eq!(
            refusal,
            Refusal::Floating(RecordKey::Answer {
                turn,
                attempt: Attempt::new(1),
            }),
        );
    }

    #[test]
    fn a_call_without_its_request_is_refused() {
        let turn = turn_id(1);
        let execution = execution_id(turn);
        let mut transcript = Transcript::new(session());
        open(&mut transcript, turn, 1);
        request(&mut transcript, turn, 1);

        let refusal = transcript.apply(&call(turn, execution)).unwrap_err();
        assert_eq!(refusal, Refusal::Floating(RecordKey::Call(execution)));
    }

    #[test]
    fn one_key_refuses_different_bytes_and_admits_identical_bytes() {
        let turn = turn_id(1);
        let mut transcript = Transcript::new(session());
        open(&mut transcript, turn, 1);
        request(&mut transcript, turn, 1);

        let record = prompt(turn, artifact(1));
        assert_eq!(transcript.apply(&record), Ok(Ack::Applied));
        assert_eq!(transcript.apply(&record), Ok(Ack::Duplicate));

        let conflicting = prompt(turn, artifact(2));
        assert_eq!(
            transcript.apply(&conflicting),
            Err(Refusal::Conflict(RecordKey::Prompt {
                turn,
                attempt: Attempt::new(1),
            })),
        );
    }

    #[test]
    fn a_task_redelivered_is_a_duplicate_and_a_different_task_is_a_conflict() {
        let mut transcript = Transcript::new(session());
        let record = Record::Task(TaskRecord { task: task(4) });

        assert_eq!(transcript.apply(&record), Ok(Ack::Applied));
        assert_eq!(transcript.apply(&record), Ok(Ack::Duplicate));
        assert_eq!(
            transcript.apply(&Record::Task(TaskRecord { task: task(5) })),
            Err(Refusal::Conflict(RecordKey::Task)),
        );
    }

    #[test]
    fn an_answer_redelivered_is_a_duplicate_and_a_different_answer_is_a_conflict() {
        let turn = turn_id(1);
        let mut transcript = Transcript::new(session());
        open(&mut transcript, turn, 1);
        request(&mut transcript, turn, 1);
        answered(&mut transcript, turn);

        let record = answer(turn, Action::Final);
        assert_eq!(transcript.apply(&record), Ok(Ack::Applied));
        assert_eq!(transcript.apply(&record), Ok(Ack::Duplicate));
        assert_eq!(
            transcript.apply(&answer(
                turn,
                Action::Command {
                    command: "ls".to_owned(),
                },
            )),
            Err(Refusal::Conflict(RecordKey::Answer {
                turn,
                attempt: Attempt::new(1),
            })),
        );
    }

    #[test]
    fn a_call_redelivered_is_a_duplicate_and_a_different_call_is_a_conflict() {
        let turn = turn_id(1);
        let execution = execution_id(turn);
        let mut transcript = a_requested_turn(execution);

        let record = call(turn, execution);
        assert_eq!(transcript.apply(&record), Ok(Ack::Applied));
        assert_eq!(transcript.apply(&record), Ok(Ack::Duplicate));
        assert_eq!(
            transcript.apply(&Record::Call(CallRecord {
                turn,
                execution,
                command: "rm -rf".to_owned(),
            })),
            Err(Refusal::Conflict(RecordKey::Call(execution))),
        );
    }

    #[test]
    fn a_call_whose_command_the_answer_never_named_is_refused() {
        let turn = turn_id(1);
        let execution = execution_id(turn);
        let mut transcript = a_requested_turn(execution);

        assert_eq!(
            transcript.apply(&Record::Call(CallRecord {
                turn,
                execution,
                command: "rm -rf".to_owned(),
            })),
            Err(Refusal::CommandMismatch {
                turn,
                answer: "ls".to_owned(),
                call: "rm -rf".to_owned(),
            }),
        );
    }

    #[test]
    fn a_result_without_its_settlement_is_refused() {
        let execution = execution_id(turn_id(1));
        let mut transcript = a_requested_turn(execution);

        assert_eq!(
            transcript.apply(&result(execution, Exit::Finished { code: 0 }, b"ok")),
            Err(Refusal::Floating(RecordKey::Result(execution))),
        );
    }

    #[test]
    fn a_settled_result_folds_and_its_redelivery_is_a_duplicate() {
        let execution = execution_id(turn_id(1));
        let mut transcript = a_requested_turn(execution);
        settle(&mut transcript, execution, Resolution::Finished { code: 0 });
        transcript
            .apply(&Record::Session {
                fact: Fact::ResultApplied { execution },
            })
            .unwrap();

        let record = result(execution, Exit::Finished { code: 0 }, b"ok");
        assert_eq!(transcript.apply(&record), Ok(Ack::Applied));
        assert_eq!(transcript.apply(&record), Ok(Ack::Duplicate));
    }

    #[test]
    fn a_result_whose_report_or_observation_contradicts_the_recorded_one_is_a_conflict() {
        let execution = execution_id(turn_id(1));
        let mut transcript = a_requested_turn(execution);
        settle(&mut transcript, execution, Resolution::Finished { code: 0 });
        transcript
            .apply(&Record::Session {
                fact: Fact::ResultApplied { execution },
            })
            .unwrap();
        transcript
            .apply(&result(execution, Exit::Finished { code: 0 }, b"ok"))
            .unwrap();

        assert_eq!(
            transcript.apply(&result(execution, Exit::Killed { signal: 9 }, b"ok")),
            Err(Refusal::Conflict(RecordKey::Result(execution))),
        );
        assert_eq!(
            transcript.apply(&result(execution, Exit::Finished { code: 0 }, b"other")),
            Err(Refusal::Conflict(RecordKey::Result(execution))),
        );
    }

    #[test]
    fn a_timed_out_result_folds_on_its_own_settlement() {
        let execution = execution_id(turn_id(1));
        let mut transcript = a_requested_turn(execution);
        settle(&mut transcript, execution, Resolution::TimedOut);

        assert_eq!(
            transcript.apply(&result(execution, Exit::TimedOut, b"")),
            Ok(Ack::Applied),
        );
    }

    #[test]
    fn a_result_whose_exit_disagrees_with_the_settlement_is_refused() {
        let execution = execution_id(turn_id(1));
        let mut transcript = a_requested_turn(execution);
        settle(&mut transcript, execution, Resolution::TimedOut);

        assert_eq!(
            transcript.apply(&result(execution, Exit::Finished { code: 0 }, b"")),
            Err(Refusal::Floating(RecordKey::Result(execution))),
        );
    }

    #[test]
    fn number_of_names_the_turn_number_or_zero() {
        let turn = turn_id(5);
        let mut transcript = Transcript::new(session());
        open(&mut transcript, turn, 5);

        assert_eq!(number_of(&transcript, turn), 5);
        assert_eq!(number_of(&transcript, turn_id(6)), 0);
    }

    #[test]
    fn a_refusal_and_a_key_render_what_they_name() {
        assert_eq!(RecordKey::Task.to_string(), "the task");
        assert_eq!(
            Refusal::Conflict(RecordKey::Task).to_string(),
            "the task already recorded with different content",
        );
        assert_eq!(
            Refusal::CommandMismatch {
                turn: turn_id(1),
                answer: "ls".to_owned(),
                call: "rm".to_owned(),
            }
            .to_string(),
            format!(
                "the call for turn {} names \"rm\" but its answer named \"ls\"",
                turn_id(1).as_uuid(),
            ),
        );
    }

    fn arb_uuid() -> impl Strategy<Value = uuid::Uuid> {
        any::<[u8; 16]>().prop_map(uuid::Uuid::from_bytes)
    }

    fn arb_artifact() -> impl Strategy<Value = ArtifactRef> {
        (any::<u64>(), any::<bool>()).prop_map(|(bytes, truncated)| ArtifactRef {
            hash: format!("{bytes:016x}"),
            bytes,
            truncated,
        })
    }

    fn arb_attempt() -> impl Strategy<Value = Attempt> {
        any::<u32>().prop_map(Attempt::new)
    }

    fn arb_deadline() -> impl Strategy<Value = Deadline> {
        prop_oneof![Just(Deadline::Bounded), Just(Deadline::Unbounded)]
    }

    fn arb_resolution() -> impl Strategy<Value = Resolution> {
        prop_oneof![
            any::<i32>().prop_map(|code| Resolution::Finished { code }),
            any::<i32>().prop_map(|signal| Resolution::Killed { signal }),
            Just(Resolution::TimedOut),
            Just(Resolution::Cancelled),
            Just(Resolution::Refused),
            Just(Resolution::Unknown),
        ]
    }

    fn arb_outcome() -> impl Strategy<Value = TurnOutcome> {
        prop_oneof![
            Just(TurnOutcome::Completed),
            Just(TurnOutcome::Cancelled),
            Just(TurnOutcome::Failed),
        ]
    }

    fn arb_fact() -> impl Strategy<Value = Fact> {
        prop_oneof![
            Just(Fact::SessionOpened),
            (arb_uuid(), arb_deadline()).prop_map(|(execution, deadline)| {
                Fact::ExecutionLaunched {
                    execution: ExecutionID::from_uuid(execution),
                    deadline,
                }
            }),
            (arb_uuid(), arb_resolution()).prop_map(|(execution, resolution)| {
                Fact::ExecutionSettled {
                    execution: ExecutionID::from_uuid(execution),
                    resolution,
                }
            }),
            Just(Fact::CancelRequested),
            arb_uuid().prop_map(|execution| Fact::ResultApplied {
                execution: ExecutionID::from_uuid(execution),
            }),
            Just(Fact::SessionClosed),
        ]
    }

    fn arb_turn_fact() -> impl Strategy<Value = TurnFact> {
        prop_oneof![
            any::<u64>().prop_map(|number| TurnFact::TurnOpened {
                number: TurnNumber::new(number),
            }),
            arb_attempt().prop_map(|attempt| TurnFact::ModelRequested { attempt }),
            Just(TurnFact::ModelAnswered),
            (arb_uuid(), arb_deadline()).prop_map(|(execution, deadline)| {
                TurnFact::ToolRequested {
                    execution: ExecutionID::from_uuid(execution),
                    deadline,
                }
            }),
            (arb_uuid(), arb_resolution()).prop_map(|(execution, resolution)| {
                TurnFact::ToolResolved {
                    execution: ExecutionID::from_uuid(execution),
                    resolution,
                }
            }),
            arb_outcome().prop_map(|outcome| TurnFact::TurnSettled { outcome }),
        ]
    }

    fn arb_action() -> impl Strategy<Value = Action> {
        prop_oneof![
            Just(Action::Final),
            "[a-z ]{0,8}".prop_map(|command| Action::Command { command }),
            "[a-z ]{0,8}".prop_map(|reason| Action::Malformed { reason }),
        ]
    }

    fn arb_exit() -> impl Strategy<Value = Exit> {
        prop_oneof![
            any::<i32>().prop_map(|code| Exit::Finished { code }),
            any::<i32>().prop_map(|signal| Exit::Killed { signal }),
            Just(Exit::TimedOut),
            Just(Exit::Refused),
            Just(Exit::Unknown),
        ]
    }

    fn arb_report() -> impl Strategy<Value = ToolReport> {
        (
            arb_exit(),
            any::<bool>(),
            arb_artifact(),
            arb_artifact(),
            "[a-z ]{0,16}",
        )
            .prop_map(
                |(exit, attempted, stdout, stderr, observation)| ToolReport {
                    exit,
                    attempted,
                    stdout,
                    stderr,
                    observation,
                },
            )
    }

    fn arb_task() -> impl Strategy<Value = Task> {
        any::<u32>().prop_map(|turns| Task {
            goal: "goal".to_owned(),
            setting: Setting {
                checkout: "/checkout".into(),
                readable: Vec::new(),
                writable: Vec::new(),
                program: "/bin/sh".into(),
                deadline: Duration::from_secs(120),
            },
            budget: Budget {
                turns,
                attempts: Budget::DEFAULT_ATTEMPTS,
                tokens: 0,
                observation_bytes: 0,
                context_turns: 0,
                context_bytes: 0,
            },
        })
    }

    fn arb_record() -> impl Strategy<Value = Record> {
        prop_oneof![
            arb_fact().prop_map(|fact| Record::Session { fact }),
            (arb_uuid(), arb_turn_fact()).prop_map(|(turn, fact)| Record::Turn {
                turn: TurnID::from_uuid(turn),
                fact,
            }),
            (arb_uuid(), arb_report()).prop_map(|(execution, report)| Record::Result {
                execution: ExecutionID::from_uuid(execution),
                report,
            }),
            arb_task().prop_map(|task| Record::Task(crate::record::TaskRecord { task })),
            (arb_uuid(), arb_attempt(), arb_artifact()).prop_map(|(turn, attempt, context)| {
                Record::Prompt(PromptRecord {
                    turn: TurnID::from_uuid(turn),
                    attempt,
                    context,
                })
            }),
            (arb_uuid(), arb_attempt(), arb_action(), arb_artifact()).prop_map(
                |(turn, attempt, action, text)| Record::Answer(AnswerRecord {
                    turn: TurnID::from_uuid(turn),
                    attempt,
                    action,
                    text,
                    model: "m".to_owned(),
                    usage: Usage {
                        input: 0,
                        output: 0,
                    },
                })
            ),
            (arb_uuid(), arb_uuid(), "[a-z ]{0,8}").prop_map(|(turn, execution, command)| {
                Record::Call(CallRecord {
                    turn: TurnID::from_uuid(turn),
                    execution: ExecutionID::from_uuid(execution),
                    command,
                })
            }),
        ]
    }

    proptest! {
        #[test]
        fn refolding_a_history_is_invariant(records in prop::collection::vec(arb_record(), 0..24)) {
            let session = session();
            let whole = Transcript::replay(session, &records);

            prop_assert_eq!(&Transcript::replay(session, &records), &whole);

            let split = records.len() / 2;
            let incremental = Transcript::replay(session, &records[..split]).and_then(|mut folded| {
                for record in &records[split..] {
                    folded.apply(record)?;
                }
                Ok(folded)
            });

            prop_assert_eq!(&incremental, &whole);
        }
    }
}

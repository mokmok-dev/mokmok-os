//! The fold of a session's records, and the rules that refuse a malformed history.

use std::collections::BTreeMap;

use mokmok_protocol::{ExecutionID, SessionID, TurnID};
use mokmok_state_machine::{Ack, Attempt, Rejection, Session, State, Turn, TurnFact, TurnNumber};

use crate::edit::{EditOutcome, EditReport};
use crate::model::Action;
use crate::record::{
    AnswerRecord, ArtifactRef, CallRecord, EditIntentRecord, EditOutcomeRecord, FindRecord,
    PromptRecord, Record, RecordKey, Refusal, SpeechRecord, TaskRecord,
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
    /// The line that opened one turn, when a human opened it.
    pub speeches: BTreeMap<TurnID, String>,
    /// The exact context bytes one attempt was handed.
    pub prompts: BTreeMap<(TurnID, Attempt), ArtifactRef>,
    /// The action one attempt answered with.
    pub answers: BTreeMap<(TurnID, Attempt), AnswerRecord>,
    /// The command one execution is about to dispatch.
    pub calls: BTreeMap<ExecutionID, CallRecord>,
    /// The find one turn asked for, when its answer asked for one.
    ///
    /// One turn carries one action, so a turn holds at most one find, and the turn is the key.
    pub finds: BTreeMap<TurnID, FindRecord>,
    /// The report each result record carried, so a redelivery is compared as the whole record and
    /// the observation it holds is what the model was shown.
    pub runs: BTreeMap<ExecutionID, ToolReport>,
    /// The edit one turn asked for, when its answer asked for one.
    pub edit_intents: BTreeMap<ExecutionID, EditIntentRecord>,
    /// The outcome of one edit.
    pub edit_outcomes: BTreeMap<ExecutionID, EditReport>,
}

impl Transcript {
    /// A fold that holds nothing yet.
    #[must_use]
    pub const fn new(session: SessionID) -> Self {
        Self {
            session: Session::new(session),
            task: None,
            turns: Vec::new(),
            speeches: BTreeMap::new(),
            prompts: BTreeMap::new(),
            answers: BTreeMap::new(),
            calls: BTreeMap::new(),
            finds: BTreeMap::new(),
            runs: BTreeMap::new(),
            edit_intents: BTreeMap::new(),
            edit_outcomes: BTreeMap::new(),
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

    /// The line that opened one turn, when a human opened it.
    #[must_use]
    pub fn speech(
        &self,
        turn: TurnID,
    ) -> Option<&str> {
        self.speeches.get(&turn).map(String::as_str)
    }

    /// Whether the session has received a human line, which is what makes it a conversation. A
    /// conversation does not end because the model finished a sentence; a session that has received
    /// no line is a goal run, and a sentence is what it was for.
    #[must_use]
    pub fn holds_speech(&self) -> bool {
        !self.speeches.is_empty()
    }

    /// The call one turn recorded, if any.
    #[must_use]
    pub fn call_of(
        &self,
        turn: TurnID,
    ) -> Option<&CallRecord> {
        self.calls.values().find(|call| call.turn == turn)
    }

    #[must_use]
    pub fn find_of(
        &self,
        turn: TurnID,
    ) -> Option<&FindRecord> {
        self.finds.get(&turn)
    }

    /// The edit intent one turn recorded, if any.
    #[must_use]
    pub fn edit_of(
        &self,
        turn: TurnID,
    ) -> Option<&EditIntentRecord> {
        self.edit_intents
            .values()
            .find(|intent| intent.turn == turn)
    }

    /// The edit outcome for one execution, if any.
    #[must_use]
    pub fn edit_outcome_of(
        &self,
        execution: ExecutionID,
    ) -> Option<&EditReport> {
        self.edit_outcomes.get(&execution)
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
    /// Returns [`Refusal::Floating`], [`Refusal::Late`], [`Refusal::Conflict`],
    /// [`Refusal::CommandMismatch`], [`Refusal::QueryMismatch`], or the [`Refusal::Rejected`] of the
    /// state machine that owns the fact.
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
            Self::Find(record) => apply_find(into, record),
            Self::EditIntent(record) => apply_edit_intent(into, record),
            Self::EditOutcome(record) => apply_edit_outcome(into, record),
            Self::Speech(record) => apply_speech(into, record),
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
        Some(
            State::Settled(recorded)
            | State::Applied {
                resolution: recorded,
            },
        ) => recorded == resolution,
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

fn apply_speech(
    into: &mut Transcript,
    record: &SpeechRecord,
) -> Result<Ack, Refusal> {
    let key = RecordKey::Speech { turn: record.turn };
    if let Some(recorded) = into.speeches.get(&record.turn) {
        return if recorded == &record.text {
            Ok(Ack::Duplicate)
        } else {
            Err(Refusal::Conflict(key))
        };
    }

    let Some(turn) = into.turn(record.turn) else {
        return Err(Refusal::Floating(key));
    };
    // A prompt records the exact bytes an attempt was handed, so a line folded in after an attempt
    // was requested would leave the fold disagreeing with that record.
    if turn.attempts().next().is_some() {
        return Err(Refusal::Late(key));
    }

    into.speeches.insert(record.turn, record.text.clone());
    Ok(Ack::Applied)
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

    match into.newest_answer(record.turn).map(|answer| &answer.action) {
        Some(Action::Command { command }) if command != &record.command => {
            return Err(Refusal::CommandMismatch {
                turn: record.turn,
                answer: command.clone(),
                call: record.command.clone(),
            });
        },
        // A find's query or an edit's patch stands where a call's command would.
        Some(Action::Find { query }) => {
            return Err(Refusal::CommandMismatch {
                turn: record.turn,
                answer: query.clone(),
                call: record.command.clone(),
            });
        },
        Some(Action::Edit { patch, .. }) => {
            return Err(Refusal::CommandMismatch {
                turn: record.turn,
                answer: patch.clone(),
                call: record.command.clone(),
            });
        },
        None | Some(Action::Command { .. } | Action::Final | Action::Malformed { .. }) => {},
    }

    into.calls.insert(record.execution, record.clone());
    Ok(Ack::Applied)
}

fn apply_find(
    into: &mut Transcript,
    record: &FindRecord,
) -> Result<Ack, Refusal> {
    let key = RecordKey::Find(record.turn);
    if let Some(recorded) = into.finds.get(&record.turn) {
        return if recorded == record {
            Ok(Ack::Duplicate)
        } else {
            Err(Refusal::Conflict(key))
        };
    }

    let Some(named) = into.newest_answer(record.turn).map(|answer| &answer.action) else {
        return Err(Refusal::Floating(key));
    };
    let named = match named {
        Action::Find { query } => query,
        Action::Edit { patch, .. } => patch,
        Action::Command { .. } | Action::Final | Action::Malformed { .. } => "",
    };
    if named != record.query {
        return Err(Refusal::QueryMismatch {
            turn: record.turn,
            answer: named.to_owned(),
            find: record.query.clone(),
        });
    }

    into.finds.insert(record.turn, record.clone());
    Ok(Ack::Applied)
}

fn apply_edit_intent(
    into: &mut Transcript,
    record: &EditIntentRecord,
) -> Result<Ack, Refusal> {
    let key = RecordKey::EditIntent(record.execution);
    if let Some(recorded) = into.edit_intents.get(&record.execution) {
        return if recorded == record {
            Ok(Ack::Duplicate)
        } else {
            Err(Refusal::Conflict(key))
        };
    }

    let requested = into
        .turn(record.turn)
        .is_some_and(|turn| turn.tool(record.execution).is_some());
    if !requested {
        return Err(Refusal::Floating(key));
    }

    match into.newest_answer(record.turn).map(|answer| &answer.action) {
        Some(Action::Edit { patch, .. }) if patch != &record.patch => {
            return Err(Refusal::EditMismatch {
                turn: record.turn,
                answer: patch.clone(),
                edit: record.patch.clone(),
            });
        },
        Some(Action::Command { command }) => {
            return Err(Refusal::EditMismatch {
                turn: record.turn,
                answer: command.clone(),
                edit: record.patch.clone(),
            });
        },
        Some(Action::Find { query }) => {
            return Err(Refusal::EditMismatch {
                turn: record.turn,
                answer: query.clone(),
                edit: record.patch.clone(),
            });
        },
        None | Some(Action::Final | Action::Malformed { .. }) => {
            return Err(Refusal::Floating(key));
        },
        Some(Action::Edit { .. }) => {},
    }

    into.edit_intents.insert(record.execution, record.clone());
    Ok(Ack::Applied)
}

fn apply_edit_outcome(
    into: &mut Transcript,
    record: &EditOutcomeRecord,
) -> Result<Ack, Refusal> {
    let key = RecordKey::EditOutcome(record.execution);
    if let Some(recorded) = into.edit_outcomes.get(&record.execution) {
        return if recorded == &record.report {
            Ok(Ack::Duplicate)
        } else {
            Err(Refusal::Conflict(key))
        };
    }

    let Some(turn) = into
        .turns
        .iter()
        .find(|(_, state)| state.tool(record.execution).is_some())
        .map(|(turn, _)| *turn)
    else {
        return Err(Refusal::Floating(key));
    };
    if turn != record.turn {
        return Err(Refusal::Floating(key));
    }

    let resolution = record.report.outcome.resolution();
    let settled = match into
        .session
        .execution(record.execution)
        .map(|held| held.state())
    {
        Some(
            State::Settled(recorded)
            | State::Applied {
                resolution: recorded,
            },
        ) => recorded == resolution,
        Some(State::InFlight) | None => false,
    };
    if !settled {
        return Err(Refusal::Floating(key));
    }

    match record.report.outcome {
        EditOutcome::Applied | EditOutcome::Stale { .. }
            if !into.edit_intents.contains_key(&record.execution) =>
        {
            return Err(Refusal::Unprepared(record.execution));
        },
        EditOutcome::Applied | EditOutcome::Stale { .. } | EditOutcome::Refused { .. } => {},
    }

    into.edit_outcomes
        .insert(record.execution, record.report.clone());
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
    use crate::edit::{EditOutcome, EditReport};
    use crate::find::FindReport;
    use crate::model::{Action, Usage};
    use crate::record::{
        AnswerRecord, ArtifactRef, CallRecord, EditIntentRecord, EditOutcomeRecord, FindRecord,
        PromptRecord, RecordKey, Refusal, SpeechRecord, TaskRecord,
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
            prose: String::new(),
            text: artifact(0xaa),
            model: "test".to_owned(),
            usage: Usage {
                input: 1,
                output: 1,
            },
        })
    }

    fn speech(
        turn: TurnID,
        text: &str,
    ) -> Record {
        Record::Speech(SpeechRecord {
            turn,
            text: text.to_owned(),
        })
    }

    fn find(
        turn: TurnID,
        query: &str,
    ) -> Record {
        Record::Find(FindRecord {
            turn,
            query: query.to_owned(),
            report: FindReport {
                observation: "indexed=831 hits=1".to_owned(),
            },
        })
    }

    fn a_find_answered_turn(query: &str) -> (Transcript, TurnID) {
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
                Action::Find {
                    query: query.to_owned(),
                },
            ))
            .unwrap();
        (transcript, turn)
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
    fn a_speech_whose_turn_the_fold_does_not_hold_is_refused() {
        let turn = turn_id(1);
        let mut transcript = Transcript::new(session());

        assert_eq!(
            transcript.apply(&speech(turn, "hello")),
            Err(Refusal::Floating(RecordKey::Speech { turn })),
        );
        assert!(transcript.speech(turn).is_none());
    }

    #[test]
    fn a_speech_on_a_turn_that_already_asked_the_model_is_refused() {
        let turn = turn_id(1);
        let mut transcript = Transcript::new(session());
        open(&mut transcript, turn, 1);
        request(&mut transcript, turn, 1);

        assert_eq!(
            transcript.apply(&speech(turn, "hello")),
            Err(Refusal::Late(RecordKey::Speech { turn })),
        );
    }

    #[test]
    fn a_speech_redelivered_is_a_duplicate_and_a_different_line_is_a_conflict() {
        let turn = turn_id(1);
        let mut transcript = Transcript::new(session());
        open(&mut transcript, turn, 1);

        assert_eq!(transcript.apply(&speech(turn, "hello")), Ok(Ack::Applied));
        assert_eq!(transcript.speech(turn), Some("hello"));
        assert_eq!(transcript.apply(&speech(turn, "hello")), Ok(Ack::Duplicate));
        assert_eq!(
            transcript.apply(&speech(turn, "goodbye")),
            Err(Refusal::Conflict(RecordKey::Speech { turn })),
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
    fn a_call_after_an_answer_that_asked_the_finder_is_refused() {
        let (mut transcript, turn) = a_find_answered_turn("bm25");
        let execution = execution_id(turn);
        requested(&mut transcript, turn, execution);

        assert_eq!(
            transcript.apply(&call(turn, execution)),
            Err(Refusal::CommandMismatch {
                turn,
                answer: "bm25".to_owned(),
                call: "ls".to_owned(),
            }),
        );
    }

    #[test]
    fn a_find_whose_query_the_answer_never_named_is_refused() {
        let (mut transcript, turn) = a_find_answered_turn("bm25");

        assert_eq!(
            transcript.apply(&find(turn, "tantivy")),
            Err(Refusal::QueryMismatch {
                turn,
                answer: "bm25".to_owned(),
                find: "tantivy".to_owned(),
            }),
        );
    }

    #[test]
    fn a_find_before_the_answer_that_asked_for_it_is_refused() {
        let turn = turn_id(1);
        let mut transcript = Transcript::new(session());
        open(&mut transcript, turn, 1);
        request(&mut transcript, turn, 1);

        assert_eq!(
            transcript.apply(&find(turn, "bm25")),
            Err(Refusal::Floating(RecordKey::Find(turn))),
        );
    }

    #[test]
    fn a_find_redelivered_is_a_duplicate_and_a_different_query_is_a_conflict() {
        let (mut transcript, turn) = a_find_answered_turn("bm25");

        let record = find(turn, "bm25");
        assert_eq!(transcript.apply(&record), Ok(Ack::Applied));
        assert_eq!(
            transcript.find_of(turn).map(|find| find.query.as_str()),
            Some("bm25"),
        );
        assert_eq!(transcript.apply(&record), Ok(Ack::Duplicate));
        assert_eq!(
            transcript.apply(&find(turn, "tantivy")),
            Err(Refusal::Conflict(RecordKey::Find(turn))),
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
            RecordKey::Find(turn_id(1)).to_string(),
            format!("the find for turn {}", turn_id(1).as_uuid()),
        );
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
        assert_eq!(
            Refusal::QueryMismatch {
                turn: turn_id(1),
                answer: "bm25".to_owned(),
                find: "tantivy".to_owned(),
            }
            .to_string(),
            format!(
                "the find for turn {} asks \"tantivy\" but its answer named \"bm25\"",
                turn_id(1).as_uuid(),
            ),
        );
    }

    fn edit_answer(
        turn: TurnID,
        patch: &str,
    ) -> Record {
        Record::Answer(AnswerRecord {
            turn,
            attempt: Attempt::new(1),
            action: Action::Edit {
                patch: patch.to_owned(),
                expect: None,
            },
            prose: String::new(),
            text: artifact(0xaa),
            model: "test".to_owned(),
            usage: Usage {
                input: 1,
                output: 1,
            },
        })
    }

    fn edit_intent(
        turn: TurnID,
        execution: ExecutionID,
        patch: &str,
        relative: &str,
    ) -> Record {
        Record::EditIntent(EditIntentRecord {
            turn,
            execution,
            patch: patch.to_owned(),
            expect: None,
            relative: relative.to_owned(),
            before: artifact(0x01),
            after: artifact(0x02),
            placement: "hunk 1 matched at line 1 (offset 0)".to_owned(),
            create: false,
        })
    }

    fn edit_outcome(
        turn: TurnID,
        execution: ExecutionID,
        outcome: EditOutcome,
    ) -> Record {
        Record::EditOutcome(EditOutcomeRecord {
            turn,
            execution,
            report: EditReport {
                outcome,
                observation: String::new(),
            },
        })
    }

    fn an_edit_answered_turn(patch: &str) -> (Transcript, TurnID, ExecutionID) {
        let turn = turn_id(1);
        let execution = execution_id(turn);
        let mut transcript = Transcript::new(session());
        transcript
            .apply(&Record::Session {
                fact: Fact::SessionOpened,
            })
            .unwrap();
        open(&mut transcript, turn, 1);
        request(&mut transcript, turn, 1);
        answered(&mut transcript, turn);
        transcript.apply(&edit_answer(turn, patch)).unwrap();
        transcript
            .apply(&Record::Session {
                fact: Fact::ExecutionLaunched {
                    execution,
                    deadline: Deadline::Unbounded,
                },
            })
            .unwrap();
        transcript
            .apply(&Record::Turn {
                turn,
                fact: TurnFact::ToolRequested {
                    execution,
                    deadline: Deadline::Unbounded,
                },
            })
            .unwrap();
        (transcript, turn, execution)
    }

    #[test]
    fn an_edit_intent_is_floating_without_its_request() {
        let turn = turn_id(1);
        let execution = execution_id(turn);
        let mut transcript = Transcript::new(session());
        open(&mut transcript, turn, 1);

        assert_eq!(
            transcript.apply(&edit_intent(
                turn,
                execution,
                "--- a\n+++ b\n@@ -1,1 +1,1 @@\n-x\n+y\n",
                "a"
            )),
            Err(Refusal::Floating(RecordKey::EditIntent(execution))),
        );
    }

    #[test]
    fn an_edit_intent_redelivered_is_a_duplicate() {
        let (mut transcript, turn, execution) =
            an_edit_answered_turn("--- a\n+++ b\n@@ -1,1 +1,1 @@\n-x\n+y\n");
        let record = edit_intent(
            turn,
            execution,
            "--- a\n+++ b\n@@ -1,1 +1,1 @@\n-x\n+y\n",
            "a",
        );

        assert_eq!(transcript.apply(&record), Ok(Ack::Applied));
        assert_eq!(transcript.apply(&record), Ok(Ack::Duplicate));
    }

    #[test]
    fn an_edit_intent_refuses_a_different_patch() {
        let (mut transcript, turn, execution) =
            an_edit_answered_turn("--- a\n+++ b\n@@ -1,1 +1,1 @@\n-x\n+y\n");

        assert_eq!(
            transcript.apply(&edit_intent(
                turn,
                execution,
                "--- a\n+++ b\n@@ -1,1 +1,1 @@\n-x\n+z\n",
                "a"
            )),
            Err(Refusal::EditMismatch {
                turn,
                answer: "--- a\n+++ b\n@@ -1,1 +1,1 @@\n-x\n+y\n".to_owned(),
                edit: "--- a\n+++ b\n@@ -1,1 +1,1 @@\n-x\n+z\n".to_owned(),
            }),
        );
    }

    #[test]
    fn an_edit_intent_refuses_a_command_answer() {
        let turn = turn_id(1);
        let execution = execution_id(turn);
        let mut transcript = a_requested_turn(execution);
        transcript
            .apply(&answer(
                turn,
                Action::Command {
                    command: "ls".to_owned(),
                },
            ))
            .unwrap();

        assert_eq!(
            transcript.apply(&edit_intent(turn, execution, "p", "a")),
            Err(Refusal::EditMismatch {
                turn,
                answer: "ls".to_owned(),
                edit: "p".to_owned(),
            }),
        );
    }

    #[test]
    fn an_edit_intent_refuses_a_find_answer() {
        let (mut transcript, turn, execution) = {
            let (mut t, turn) = a_find_answered_turn("bm25");
            let execution = execution_id(turn);
            requested(&mut t, turn, execution);
            (t, turn, execution)
        };

        assert_eq!(
            transcript.apply(&edit_intent(turn, execution, "p", "a")),
            Err(Refusal::EditMismatch {
                turn,
                answer: "bm25".to_owned(),
                edit: "p".to_owned(),
            }),
        );
    }

    #[test]
    fn a_call_after_an_answer_that_asked_for_an_edit_is_refused() {
        let (mut transcript, turn, execution) = an_edit_answered_turn("p");

        assert_eq!(
            transcript.apply(&call(turn, execution)),
            Err(Refusal::CommandMismatch {
                turn,
                answer: "p".to_owned(),
                call: "ls".to_owned(),
            }),
        );
    }

    #[test]
    fn a_find_after_an_answer_that_asked_for_an_edit_is_refused() {
        let (mut transcript, turn, _) = an_edit_answered_turn("p");

        assert_eq!(
            transcript.apply(&find(turn, "bm25")),
            Err(Refusal::QueryMismatch {
                turn,
                answer: "p".to_owned(),
                find: "bm25".to_owned(),
            }),
        );
    }

    #[test]
    fn an_applied_edit_outcome_is_unprepared_without_its_intent() {
        let (mut transcript, turn, execution) = an_edit_answered_turn("p");
        settle(&mut transcript, execution, Resolution::Applied);

        assert_eq!(
            transcript.apply(&edit_outcome(turn, execution, EditOutcome::Applied)),
            Err(Refusal::Unprepared(execution)),
        );
    }

    #[test]
    fn an_edit_outcome_is_refused_when_the_settlement_disagrees() {
        let (mut transcript, turn, execution) = an_edit_answered_turn("p");
        transcript
            .apply(&edit_intent(turn, execution, "p", "a"))
            .unwrap();
        settle(&mut transcript, execution, Resolution::Refused);

        assert_eq!(
            transcript.apply(&edit_outcome(turn, execution, EditOutcome::Applied)),
            Err(Refusal::Floating(RecordKey::EditOutcome(execution))),
        );
    }

    #[test]
    fn an_applied_edit_outcome_folds_when_its_intent_exists() {
        let (mut transcript, turn, execution) = an_edit_answered_turn("p");
        transcript
            .apply(&edit_intent(turn, execution, "p", "a"))
            .unwrap();
        settle(&mut transcript, execution, Resolution::Applied);
        transcript
            .apply(&Record::Session {
                fact: Fact::ResultApplied { execution },
            })
            .unwrap();

        assert_eq!(
            transcript.apply(&edit_outcome(turn, execution, EditOutcome::Applied)),
            Ok(Ack::Applied),
        );
        assert_eq!(
            transcript
                .edit_outcome_of(execution)
                .map(|report| report.outcome.clone()),
            Some(EditOutcome::Applied),
        );
    }

    #[test]
    fn a_refused_edit_outcome_folds_with_no_intent() {
        let (mut transcript, turn, execution) = an_edit_answered_turn("p");
        settle(&mut transcript, execution, Resolution::Refused);

        assert_eq!(
            transcript.apply(&edit_outcome(
                turn,
                execution,
                EditOutcome::Refused {
                    reason: "no file".to_owned(),
                },
            )),
            Ok(Ack::Applied),
        );
    }

    #[test]
    fn an_edit_outcome_redelivered_is_a_duplicate_and_a_different_one_is_a_conflict() {
        let (mut transcript, turn, execution) = an_edit_answered_turn("p");
        transcript
            .apply(&edit_intent(turn, execution, "p", "a"))
            .unwrap();
        settle(&mut transcript, execution, Resolution::Applied);
        transcript
            .apply(&Record::Session {
                fact: Fact::ResultApplied { execution },
            })
            .unwrap();
        let record = edit_outcome(turn, execution, EditOutcome::Applied);
        transcript.apply(&record).unwrap();

        assert_eq!(transcript.apply(&record), Ok(Ack::Duplicate));
        assert_eq!(
            transcript.apply(&edit_outcome(
                turn,
                execution,
                EditOutcome::Refused {
                    reason: "no".to_owned(),
                },
            )),
            Err(Refusal::Conflict(RecordKey::EditOutcome(execution))),
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
            "[a-z ]{0,8}".prop_map(|query| Action::Find { query }),
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
                search: Vec::new(),
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
                    prose: String::new(),
                    text,
                    model: "m".to_owned(),
                    usage: Usage {
                        input: 0,
                        output: 0,
                    },
                })
            ),
            (arb_uuid(), "[a-z ]{0,8}").prop_map(|(turn, text)| {
                Record::Speech(SpeechRecord {
                    turn: TurnID::from_uuid(turn),
                    text,
                })
            }),
            (arb_uuid(), arb_uuid(), "[a-z ]{0,8}").prop_map(|(turn, execution, command)| {
                Record::Call(CallRecord {
                    turn: TurnID::from_uuid(turn),
                    execution: ExecutionID::from_uuid(execution),
                    command,
                })
            }),
            (arb_uuid(), "[a-z ]{0,8}", "[a-z ]{0,16}").prop_map(|(turn, query, observation)| {
                Record::Find(FindRecord {
                    turn: TurnID::from_uuid(turn),
                    query,
                    report: FindReport { observation },
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

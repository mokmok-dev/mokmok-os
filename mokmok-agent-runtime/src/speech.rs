//! Committing one human line as the input of the session's next turn.

use mokmok_protocol::{CommandID, SessionID};
use mokmok_state_machine::{Fact, TurnFact, TurnNumber};

use crate::advance::{Move, next, turn_id};
use crate::error::Error;
use crate::model::Journal;
use crate::record::{
    MAX_SPEECH_BYTES, Receipt, Record, SpeechRecord, Step, StepId, StepKey, TaskRecord,
};
use crate::task::Task;

/// Commits one human line as the input of the session's next turn.
///
/// The line is not the loop's own decision, so it is committed only where the fold says one belongs:
/// an unopened session, which the line opens along with the task and turn 1, and a conversation
/// waiting for its human, where the line opens the next turn. Anywhere else the call is refused,
/// because a line folded into a turn already in flight would hand the model a prompt the fold no
/// longer matches.
///
/// # Errors
///
/// Returns [`Error::NotAwaiting`] when the fold does not wait for a human line, [`Error::TooMuch`]
/// when the line is longer than [`MAX_SPEECH_BYTES`], and the journal's own failures.
pub fn speak(
    journal: &mut impl Journal,
    session: SessionID,
    text: &str,
) -> Result<Receipt, Error> {
    write(journal, session, text, None)
}

/// Commits one human line under a bus request's identity.
///
/// The request derives the step id, so a redelivered command whose write already committed answers
/// [`Receipt::Replayed`] instead of opening the turn after it. The check is against the journal,
/// not a process-local set, so it survives a daemon restart.
///
/// # Errors
///
/// The same errors as [`speak`].
pub fn speak_as(
    journal: &mut impl Journal,
    session: SessionID,
    text: &str,
    request: CommandID,
) -> Result<Receipt, Error> {
    write(journal, session, text, Some(request))
}

fn write(
    journal: &mut impl Journal,
    session: SessionID,
    text: &str,
    request: Option<CommandID>,
) -> Result<Receipt, Error> {
    if text.len() > MAX_SPEECH_BYTES {
        return Err(Error::TooMuch {
            limit: MAX_SPEECH_BYTES,
            found: text.len(),
        });
    }

    let requested = request.map(|request| StepId::derive(session, StepKey::SpeechBy { request }));
    if let Some(id) = requested
        && journal.committed(id)?
    {
        return Ok(Receipt::Replayed);
    }

    let transcript = journal.transcript(session)?;

    let step = match next(&transcript) {
        Move::Begin { task } => open_step(session, &task, text, requested),
        Move::Await => extend_step(session, &transcript, text, requested),
        _ => return Err(Error::NotAwaiting),
    };

    journal.append(step)
}

fn open_step(
    session: SessionID,
    task: &Task,
    text: &str,
    requested: Option<StepId>,
) -> Step {
    let turn = turn_id(session, 1);
    let records = vec![
        Record::Session {
            fact: Fact::SessionOpened,
        },
        Record::Task(TaskRecord { task: task.clone() }),
        Record::Turn {
            turn,
            fact: TurnFact::TurnOpened {
                number: TurnNumber::new(1),
            },
        },
        Record::Speech(SpeechRecord {
            turn,
            text: text.to_owned(),
        }),
    ];

    let id = requested.unwrap_or_else(|| StepId::derive(session, StepKey::Task));

    Step::new(id, session, records)
}

fn extend_step(
    session: SessionID,
    transcript: &crate::dialogue::Transcript,
    text: &str,
    requested: Option<StepId>,
) -> Step {
    let number = transcript
        .newest_turn()
        .and_then(|(_, state)| state.number())
        .map_or(1, |number| number.get().saturating_add(1));
    let turn = turn_id(session, number);
    let records = vec![
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
    ];

    let id = requested.unwrap_or_else(|| StepId::derive(session, StepKey::Speech { turn: number }));

    Step::new(id, session, records)
}

#[cfg(test)]
mod tests {
    use super::{MAX_SPEECH_BYTES, speak, speak_as};
    use crate::error::Error;
    use crate::model::Journal;
    use crate::record::Receipt;
    use crate::test_support::{TestJournal, session, task, turn_id};
    use mokmok_protocol::CommandID;
    use proptest::prelude::*;
    use uuid::Uuid;

    /// Lines spread across the bound and lines exactly on it, so a case lands where an off-by-one shows.
    fn arb_line() -> impl Strategy<Value = String> {
        prop_oneof![
            8 => 0usize..=MAX_SPEECH_BYTES + 1,
            1 => Just(MAX_SPEECH_BYTES - 1),
            1 => Just(MAX_SPEECH_BYTES),
            1 => Just(MAX_SPEECH_BYTES + 1),
        ]
        .prop_map(|len| "x".repeat(len))
    }

    // The accepted line repeats the bound's own arithmetic instead of reading `MAX_SPEECH_BYTES`,
    // so a mutated bound has to refuse a line the log holds.
    #[test]
    fn a_line_the_log_holds_is_spoken_whole_and_one_byte_more_is_refused() {
        let session = session();
        let mut journal = TestJournal::new(session, task(4));
        let line = "x".repeat(4 * 1024);

        assert_eq!(
            speak(&mut journal, session, &line).unwrap(),
            Receipt::Written,
        );
        let opened = journal.transcript(session).unwrap();
        assert_eq!(opened.speech(turn_id(1)), Some(line.as_str()));

        let refused = speak(&mut journal, session, &"x".repeat(MAX_SPEECH_BYTES + 1)).unwrap_err();
        assert!(
            matches!(refused, Error::TooMuch { limit, found }
                if limit == MAX_SPEECH_BYTES && found == MAX_SPEECH_BYTES + 1),
            "{refused}",
        );
    }

    #[test]
    fn a_request_that_already_committed_replays_instead_of_opening_a_second_turn() {
        let session = session();
        let mut journal = TestJournal::new(session, task(4));
        let request = CommandID::from_uuid(Uuid::from_u128(9));

        assert_eq!(
            speak_as(&mut journal, session, "hello", request).unwrap(),
            Receipt::Written,
        );
        assert_eq!(
            speak_as(&mut journal, session, "hello", request).unwrap(),
            Receipt::Replayed,
        );
    }

    proptest! {
        #[test]
        fn speak_accepts_a_line_exactly_when_the_log_can_hold_it(line in arb_line()) {
            let session = session();
            let mut journal = TestJournal::new(session, task(4));

            let accepted = speak(&mut journal, session, &line).is_ok();

            prop_assert_eq!(accepted, line.len() <= MAX_SPEECH_BYTES);
            let transcript = journal.transcript(session).unwrap();
            if accepted {
                prop_assert_eq!(transcript.speech(turn_id(1)), Some(line.as_str()));
            } else {
                prop_assert_eq!(transcript.speech(turn_id(1)), None);
            }
        }
    }
}

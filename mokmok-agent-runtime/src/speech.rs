//! Committing one human line as the input of the session's next turn.

use mokmok_protocol::SessionID;
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
    if text.len() > MAX_SPEECH_BYTES {
        return Err(Error::TooMuch {
            limit: MAX_SPEECH_BYTES,
            found: text.len(),
        });
    }

    let transcript = journal.transcript(session)?;

    let step = match next(&transcript) {
        Move::Begin { task } => open_step(session, &task, text),
        Move::Await => extend_step(session, &transcript, text),
        _ => return Err(Error::NotAwaiting),
    };

    journal.append(step)
}

fn open_step(
    session: SessionID,
    task: &Task,
    text: &str,
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

    Step::new(StepId::derive(session, StepKey::Task), session, records)
}

fn extend_step(
    session: SessionID,
    transcript: &crate::dialogue::Transcript,
    text: &str,
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

    Step::new(
        StepId::derive(session, StepKey::Speech { turn: number }),
        session,
        records,
    )
}

#[cfg(test)]
mod tests {
    use super::{MAX_SPEECH_BYTES, speak};
    use crate::error::Error;
    use crate::model::Journal;
    use crate::record::Receipt;
    use crate::test_support::{TestJournal, session, task, turn_id};
    use proptest::prelude::*;

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

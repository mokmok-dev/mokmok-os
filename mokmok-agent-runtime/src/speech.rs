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

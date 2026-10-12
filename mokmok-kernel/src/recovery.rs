//! Turns the uncertainty a crash leaves behind into a recorded outcome.
//!
//! Recovery runs before the kernel does new work in a session, so a submit never races the
//! execution the last crash left in flight.
//!
//! Nothing in the log says whether a workload whose result was never recorded actually ran: the
//! supervisor that could answer is gone with the process, and its per-execution workspace is gone
//! with it. Assuming success would claim an effect nobody observed; assuming failure would re-run
//! work that may already have happened. So this writes the state that says unknown, and the
//! operator decides what to do with it.

use mokmok_event_store::{CommandId, Expected, Outcome};
use mokmok_protocol::{ExecutionID, Moment, SessionID};
use mokmok_state_machine::{Fact, Resolution, State, TurnFact, TurnOutcome};

use crate::db::{Db, Entry, Log};
use crate::error::Error;
use crate::slice::Notice;

/// Records an unknown outcome for every execution the session left in flight.
///
/// Returns the executions that were recovered, in the order the session recorded them.
///
/// # Errors
///
/// Returns [`Error::Rejected`] when the log does not replay, [`Error::WriteRefused`] when a
/// recovery step is one the machines refuse, and [`Error::Store`] when the append cannot commit.
pub fn recover(
    db: &mut Db,
    log: &Log,
    session: SessionID,
    at: Moment,
) -> Result<Vec<ExecutionID>, Error> {
    let state = log.state(session)?;
    let command = CommandId::new();
    let mut entries = Vec::new();
    let mut recovered = Vec::new();

    for (execution, held) in state.executions() {
        if held.state() != State::InFlight {
            continue;
        }

        entries.push(Entry::Session(Fact::ExecutionSettled {
            execution,
            resolution: Resolution::Unknown,
        }));
        recovered.push(execution);

        let Some(turn) = log.turn_of(execution) else {
            continue;
        };
        entries.push(Entry::Turn(
            turn,
            TurnFact::ToolResolved {
                execution,
                resolution: Resolution::Unknown,
            },
        ));
        if log.turn(turn)?.outcome().is_none() {
            entries.push(Entry::Turn(
                turn,
                TurnFact::TurnSettled {
                    outcome: TurnOutcome::Failed,
                },
            ));
        }
    }

    if entries.is_empty() {
        return Ok(recovered);
    }

    log.verify(session, &entries)?;
    let notices = recovered
        .iter()
        .map(|execution| {
            Notice {
                session,
                command,
                turn: log.turn_of(*execution),
                execution: Some(*execution),
                resolution: Some(Resolution::Unknown),
                at,
            }
            .outbox()
        })
        .collect::<Result<Vec<_>, _>>()?;
    let stream = Db::stream(session);
    let expected = Expected::At(db.version(stream)?);

    match db.append(command, stream, expected, &entries, notices)? {
        Outcome::Committed(_) | Outcome::Replayed(_) => {},
        Outcome::Conflicted { found, .. } => {
            return Err(Error::Conflicted { found: found.get() });
        },
    }

    Ok(recovered)
}

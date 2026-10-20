//! Turns the uncertainty a crash leaves behind into a recorded outcome.
//!
//! Recovery runs before the kernel does new work in a session, so a submit never races the
//! execution the last crash left in flight.
//!
//! Nothing in the log says whether a workload whose result was never recorded actually ran: the
//! supervisor that could answer is gone with the process. Its execution directory under the sandbox
//! root outlives the crash, and nothing here collects it: reclaiming that directory belongs to the
//! sandbox's own unit, not to recovery. Assuming success would claim an effect nobody observed;
//! assuming failure would re-run work that may already have happened. So this writes the state that
//! says unknown, and the operator decides what to do with it.

use mokmok_event_store::{CommandId, Expected, Outcome};
use mokmok_protocol::{ExecutionID, Moment, SessionID};
use mokmok_state_machine::{Fact, Resolution, State, TurnFact, TurnOutcome};

use mokmok_session_wire::SessionCommit;

use crate::db::{Db, Entry, Log};
use crate::error::Error;
use crate::slice::published;

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
    let commits = recovered
        .iter()
        .map(|execution| {
            published(
                session,
                command,
                &entries,
                log.turn_of(*execution),
                Some(*execution),
                Some(Resolution::Unknown),
                at,
            )
        })
        .collect::<Vec<_>>();
    let notices = commits
        .iter()
        .map(SessionCommit::outbox)
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

/// Records the unknown outcome for every execution the log left unsettled, across every session.
///
/// The daemon runs this before it can report readiness, so a dependent that starts after this unit
/// never reads a log a crash left with an execution that has no outcome. It drives the same
/// [`recover`] a caller reaches one session at a time through `reconcile`.
///
/// # Errors
///
/// As [`recover`], plus the errors of [`Db::unsettled_sessions`].
pub fn recover_unsettled(
    db: &mut Db,
    at: Moment,
) -> Result<Vec<(SessionID, ExecutionID)>, Error> {
    let mut recovered = Vec::new();

    for session in db.unsettled_sessions()? {
        let log = db.log(session)?;
        for execution in recover(db, &log, session, at)? {
            recovered.push((session, execution));
        }
    }

    Ok(recovered)
}

#[cfg(test)]
mod tests {
    use super::recover;
    use crate::db::{Db, Entry};
    use mokmok_event_store::{CommandId, Expected};
    use mokmok_protocol::{CommandID, ExecutionID, Moment, SessionID, TurnID};
    use mokmok_session_wire::SessionCommit;
    use mokmok_state_machine::{Attempt, Deadline, Fact, TurnFact, TurnNumber};
    use uuid::Uuid;

    #[test]
    fn a_recovery_notice_names_recoverys_own_command_not_a_submits_key() {
        let directory =
            std::env::temp_dir().join(format!("mokmok-kernel-recovery-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        let session = SessionID::from_uuid(Uuid::from_u128(1));
        let turn = TurnID::from_uuid(Uuid::from_u128(2));
        let execution = ExecutionID::from_uuid(Uuid::from_u128(3));
        let command = CommandId::from_uuid(Uuid::from_u128(4));

        let mut db = Db::open(&directory.join("events.sqlite")).unwrap();
        db.append(
            command,
            Db::stream(session),
            Expected::New,
            &[
                Entry::Session(Fact::SessionOpened),
                Entry::Session(Fact::ExecutionLaunched {
                    execution,
                    deadline: Deadline::Bounded,
                }),
                Entry::Turn(
                    turn,
                    TurnFact::TurnOpened {
                        number: TurnNumber::new(1),
                    },
                ),
                Entry::Turn(
                    turn,
                    TurnFact::ModelRequested {
                        attempt: Attempt::new(1),
                    },
                ),
                Entry::Turn(turn, TurnFact::ModelAnswered),
                Entry::Turn(
                    turn,
                    TurnFact::ToolRequested {
                        execution,
                        deadline: Deadline::Bounded,
                    },
                ),
            ],
            Vec::new(),
        )
        .unwrap();

        let log = db.log(session).unwrap();
        assert_eq!(
            recover(&mut db, &log, session, Moment::from_millis(1)).unwrap(),
            vec![execution],
        );

        let pending = db.pending().unwrap();
        assert_eq!(pending.len(), 1);
        let event = pending[0]
            .payload()
            .decode::<mokmok_session_wire::SessionEvent>()
            .unwrap();
        let mokmok_session_wire::SessionEvent::Commit(commit) = event else {
            panic!("recovery publishes a commit");
        };
        assert_eq!(commit.notice.execution, Some(execution));
        assert_ne!(commit.command, CommandID::from_uuid(command.as_uuid()));

        let _ = std::fs::remove_dir_all(&directory);
    }
}

//! The first vertical slice: one command, one turn, one confined tool, one durable outcome.
//!
//! One `submit` walks the eight steps the architecture names. It resolves the command line into a
//! session and an idempotency identity, commits the transition that records the turn and the tool
//! it is about to run, asks the authority whether the workload may run at all, hands the sandbox
//! the authority it was given, stores what the workload produced, and commits the settlement. A
//! retried command commits nothing the second time and reports what the log recorded.
//!
//! The model in this slice is the operator's own command: it answers by requesting exactly the tool
//! the command named. A real provider replaces that one step and nothing else, which is what makes
//! this a slice of the system rather than of the provider.

use std::ffi::OsString;
use std::time::Duration;

use mokmok_event_bus::Topic;
use mokmok_event_store::{CommandId, Expected, OutboxMessage, Outcome, Payload};
use mokmok_protocol::{ExecutionID, Moment, SessionID, TurnID};
use mokmok_sandbox::Environment;
use mokmok_state_machine::{
    Attempt, Deadline, Fact, Phase, Resolution, State, TurnFact, TurnNumber, TurnOutcome,
};

use crate::authority::Authority;
use crate::config::{Config, CrashPoint, Submit};
use crate::db::{Db, Entry, Log};
use crate::error::Error;
use crate::execution::{self, ExecutionResult, Plan, Ran};
use crate::recovery;

/// What one submit left behind, read back out of the log.
#[derive(Debug)]
pub struct Submitted {
    pub session: SessionID,
    /// Whether this command had already run, so the report is the earlier run's.
    pub replayed: bool,
    pub phase: Phase,
    pub turn: Option<TurnID>,
    pub number: Option<u64>,
    pub outcome: Option<TurnOutcome>,
    pub execution: Option<ExecutionID>,
    pub resolution: Option<Resolution>,
    pub result: Option<ExecutionResult>,
}

/// What the log records for one session.
#[derive(Debug)]
pub struct Report {
    pub session: SessionID,
    pub phase: Phase,
    pub turns: Vec<(TurnID, Option<TurnOutcome>)>,
    pub executions: Vec<(ExecutionID, State)>,
    pub results: Vec<(ExecutionID, ExecutionResult)>,
    /// The agent records the session holds, one line each, in log order.
    pub transcript: Vec<String>,
}

/// A wakeup a subscriber can react to without reading the log.
///
/// The durable fact carries the change in full; this names it. It is written into the same
/// transaction as the change, so a notice exists only after its history committed, and the relay
/// publishes it afterwards.
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub struct Notice {
    pub session: SessionID,
    /// The command whose transaction wrote this notice: a submit's is its own idempotency identity,
    /// while recovery's is the identity it mints for the batch it writes, which no retry reuses.
    pub command: CommandId,
    pub turn: Option<TurnID>,
    pub execution: Option<ExecutionID>,
    pub resolution: Option<Resolution>,
    pub at: Moment,
}

impl Notice {
    /// The outbox row the relay publishes once the transaction that wrote it has committed.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Payload`] when the notice cannot be encoded.
    pub fn outbox(&self) -> Result<OutboxMessage, Error> {
        Ok(OutboxMessage::new(
            Topic::Session(self.session).to_string(),
            Payload::json(self)?,
        ))
    }
}

/// Runs one command: commit the intent, decide, execute, commit the outcome.
///
/// # Errors
///
/// Returns [`Error::Closed`] when the session admits no new turn, [`Error::Divergent`] when the
/// idempotency identity belongs to another session, [`Error::Conflicted`] when another writer moved
/// the stream, and the errors of the store, the authority's names, the artifact store, and the
/// sandbox.
pub async fn submit(
    config: &Config,
    submit: &Submit,
    at: Moment,
) -> Result<Submitted, Error> {
    let session = SessionID::from_uuid(crate::config::uuid_of(&submit.session, "session")?);
    let command = CommandId::from_uuid(crate::config::uuid_of(
        &submit.idempotency_key,
        "idempotency key",
    )?);
    let mut db = Db::open(&config.events)?;
    let log = resume(&mut db, session, at)?;
    let (turn, execution, number) = mint(command, at, &log);
    let mut written = launch(&log, turn, execution, number);
    // A retried command already owns this turn, so the entries above describe a history the log
    // holds. The append below is the store's dedup and writes nothing, and folding them onto that
    // closed turn would refuse a write that never happens.
    if !log.turns_in_order().contains(&turn) {
        log.verify(session, &written)?;
    }

    let launch = notice(session, command, turn, Some(execution), None, at)?;
    match commit(&mut db, command, session, &written, vec![launch])? {
        Outcome::Committed(_) => {},
        Outcome::Replayed(_) => {
            // The command ran before. If this session holds nothing, the identity belongs to another
            // one, and reporting this session's empty log as "replayed" would be a lie.
            if log.session.is_empty() {
                return Err(Error::Divergent);
            }

            return report(&db, session, command, true);
        },
        Outcome::Conflicted { found, .. } => {
            return Err(Error::Conflicted { found: found.get() });
        },
    }
    crate::config::crash(config, CrashPoint::BeforeDispatch);

    let (resolution, result) = run_tool(config, submit, execution, at).await?;

    let settle = settled(turn, execution, resolution, result.as_ref());
    written.extend(settle.iter().cloned());
    log.verify(session, &written)?;

    let notice = notice(
        session,
        command,
        turn,
        Some(execution),
        Some(resolution),
        at,
    )?;
    // The settlement is its own transaction, so it needs its own identity: the store's dedup row
    // is keyed on the command id, and reusing this submit's id would make the second append a
    // replay of the first.
    match commit(&mut db, CommandId::new(), session, &settle, vec![notice])? {
        Outcome::Committed(_) | Outcome::Replayed(_) => {},
        Outcome::Conflicted { found, .. } => {
            return Err(Error::Conflicted { found: found.get() });
        },
    }

    report(&db, session, command, false)
}

/// Recovers whatever the last crash left in flight, then reads the log this submit starts from.
fn resume(
    db: &mut Db,
    session: SessionID,
    at: Moment,
) -> Result<Log, Error> {
    let log = db.log(session)?;
    if !log.session.is_empty() {
        recovery::recover(db, &log, session, at)?;
    }

    let log = db.log(session)?;
    if let Phase::Draining | Phase::Closed(_) = phase_of(&log, session)? {
        return Err(Error::Closed(match phase_of(&log, session)? {
            Phase::Draining => "draining",
            Phase::Closed(_) => "closed",
            _ => "unopened",
        }));
    }

    Ok(log)
}

fn phase_of(
    log: &Log,
    session: SessionID,
) -> Result<Phase, Error> {
    if log.session.is_empty() {
        return Ok(Phase::Unopened);
    }

    Ok(log.state(session)?.phase())
}

/// The identities one attempt runs under, and the turn number it opens.
///
/// A turn takes its identity from the command that opened it, so a retried command derives the turn
/// it already owns instead of minting a second one, and a replay can report that turn without
/// searching the log for the newest. The execution is minted fresh: a new attempt is a new
/// execution even when the command repeats.
///
/// The turn number counts the turns the log already opened, so a retried command that opens no turn
/// cannot advance it, and the number a subscriber sees is the log's own count.
fn mint(
    command: CommandId,
    at: Moment,
    log: &Log,
) -> (TurnID, ExecutionID, TurnNumber) {
    (
        TurnID::from_uuid(command.as_uuid()),
        ExecutionID::new(timestamp(at)),
        TurnNumber::new(
            u64::try_from(log.turns_in_order().len())
                .unwrap_or(u64::MAX)
                .saturating_add(1),
        ),
    )
}

/// The transaction that records the intent: what is about to be run, before anything runs.
fn launch(
    log: &Log,
    turn: TurnID,
    execution: ExecutionID,
    number: TurnNumber,
) -> Vec<Entry> {
    let mut entries = Vec::new();
    if log.session.is_empty() {
        entries.push(Entry::Session(Fact::SessionOpened));
    }

    entries.push(Entry::Session(Fact::ExecutionLaunched {
        execution,
        deadline: Deadline::Bounded,
    }));
    entries.push(Entry::Turn(turn, TurnFact::TurnOpened { number }));
    entries.push(Entry::Turn(
        turn,
        TurnFact::ModelRequested {
            attempt: Attempt::new(1),
        },
    ));
    entries.push(Entry::Turn(turn, TurnFact::ModelAnswered));
    entries.push(Entry::Turn(
        turn,
        TurnFact::ToolRequested {
            execution,
            deadline: Deadline::Bounded,
        },
    ));

    entries
}

/// The transaction that records the outcome, once it is known.
fn settled(
    turn: TurnID,
    execution: ExecutionID,
    resolution: Resolution,
    result: Option<&ExecutionResult>,
) -> Vec<Entry> {
    let mut entries = vec![Entry::Session(Fact::ExecutionSettled {
        execution,
        resolution,
    })];
    // Only a finished workload produced a result to hand over: a refusal, a kill, a deadline, or an
    // outcome nobody could prove has nothing to apply, and the session refuses the claim.
    if matches!(resolution, Resolution::Finished { .. }) {
        entries.push(Entry::Session(Fact::ResultApplied { execution }));
    }
    if let Some(result) = result {
        entries.push(Entry::Result(execution, result.clone()));
    }
    entries.push(Entry::Turn(
        turn,
        TurnFact::ToolResolved {
            execution,
            resolution,
        },
    ));
    entries.push(Entry::Turn(
        turn,
        TurnFact::TurnSettled {
            outcome: outcome_of(resolution),
        },
    ));

    entries
}

fn notice(
    session: SessionID,
    command: CommandId,
    turn: TurnID,
    execution: Option<ExecutionID>,
    resolution: Option<Resolution>,
    at: Moment,
) -> Result<OutboxMessage, Error> {
    Notice {
        session,
        command,
        turn: Some(turn),
        execution,
        resolution,
        at,
    }
    .outbox()
}

/// Decides, confines, runs, and stores what one tool produced.
async fn run_tool(
    config: &Config,
    submit: &Submit,
    execution: ExecutionID,
    at: Moment,
) -> Result<(Resolution, Option<ExecutionResult>), Error> {
    let plan = Plan {
        execution,
        program: submit.program.clone(),
        args: submit.args.iter().map(OsString::from).collect(),
        environment: Environment::Exact(config.tool_env.clone()),
        deadline: Duration::from_secs(submit.deadline_seconds),
        artifacts: submit
            .artifacts
            .iter()
            .map(|path| crate::config::relative_path(path))
            .collect::<Result<Vec<_>, _>>()?,
        exposures: execution::exposures(config),
    };
    let sandbox = mokmok_sandbox::Confined::new(mokmok_sandbox::Config {
        root: config.sandbox_root.clone(),
        confiner: config.confiner.clone(),
        ..mokmok_sandbox::Config::default()
    })?;
    let authority = Authority::new(config, at)?;

    match execution::run(config, &sandbox, &authority, &plan, at).await? {
        Ran::Settled { resolution, result } => Ok((resolution, Some(result))),
        Ran::Refused(_) => Ok((Resolution::Refused, None)),
    }
}

/// Everything the log records for one session.
///
/// # Errors
///
/// Returns [`Error::UnknownSession`] when the log holds nothing for it, and the errors of the
/// replay that reads it.
pub fn status(
    config: &Config,
    session: SessionID,
) -> Result<Report, Error> {
    let db = Db::open(&config.events)?;
    let log = db.log(session)?;
    if log.session.is_empty() {
        return Err(Error::UnknownSession(session.as_uuid()));
    }

    let state = log.state(session)?;
    let turns = log
        .turns_in_order()
        .into_iter()
        .map(|turn| Ok((turn, log.turn(turn)?.outcome())))
        .collect::<Result<Vec<_>, Error>>()?;
    let numbers = log
        .turns_in_order()
        .into_iter()
        .map(|turn| Ok((turn, log.turn(turn)?.number())))
        .collect::<Result<std::collections::BTreeMap<_, _>, Error>>()?;
    let transcript = log
        .ordered
        .iter()
        .filter_map(|record| transcript_line(record, &numbers))
        .collect();

    Ok(Report {
        session,
        phase: state.phase(),
        turns,
        executions: state
            .executions()
            .map(|(execution, held)| (execution, held.state()))
            .collect(),
        results: log
            .results
            .iter()
            .map(|record| (record.execution, record.result.clone()))
            .collect(),
        transcript,
    })
}

/// One agent record as the single line `status` prints for it.
fn transcript_line(
    record: &mokmok_agent_runtime::Record,
    numbers: &std::collections::BTreeMap<TurnID, Option<TurnNumber>>,
) -> Option<String> {
    let number = |turn: TurnID| {
        numbers
            .get(&turn)
            .copied()
            .flatten()
            .map_or(0, TurnNumber::get)
    };

    match record {
        mokmok_agent_runtime::Record::Task(task) => Some(format!("task goal={:?}", task.task.goal)),
        mokmok_agent_runtime::Record::Prompt(prompt) => Some(format!(
            "prompt turn={} number={} attempt={} hash={} bytes={}",
            prompt.turn.as_uuid(),
            number(prompt.turn),
            prompt.attempt.get(),
            prompt.context.hash,
            prompt.context.bytes,
        )),
        mokmok_agent_runtime::Record::Answer(answer) => Some(format!(
            "answer turn={} number={} attempt={} action={:?} model={} say={:?}",
            answer.turn.as_uuid(),
            number(answer.turn),
            answer.attempt.get(),
            answer.action,
            answer.model,
            answer.prose,
        )),
        mokmok_agent_runtime::Record::Call(call) => Some(format!(
            "call turn={} number={} execution={} command={:?}",
            call.turn.as_uuid(),
            number(call.turn),
            call.execution.as_uuid(),
            call.command,
        )),
        mokmok_agent_runtime::Record::Result { execution, report } => Some(format!(
            "result execution={} exit={:?} observation={:?}",
            execution.as_uuid(),
            report.exit,
            report.observation,
        )),
        mokmok_agent_runtime::Record::Speech(speech) => Some(format!(
            "speech turn={} number={} text={:?}",
            speech.turn.as_uuid(),
            number(speech.turn),
            speech.text,
        )),
        mokmok_agent_runtime::Record::Session { .. }
        | mokmok_agent_runtime::Record::Turn { .. } => None,
    }
}

/// Records an unknown outcome for every execution one session left in flight.
///
/// # Errors
///
/// Returns [`Error::UnknownSession`] when the log holds nothing for the session, and otherwise the
/// errors of the recovery pass it drives.
pub fn reconcile(
    config: &Config,
    session: SessionID,
    at: Moment,
) -> Result<Vec<ExecutionID>, Error> {
    let mut db = Db::open(&config.events)?;
    let log = db.log(session)?;
    if log.session.is_empty() {
        return Err(Error::UnknownSession(session.as_uuid()));
    }

    recovery::recover(&mut db, &log, session, at)
}

/// How a tool's end state reads as the turn's outcome.
///
/// A tool that ran to completion is the turn's goal. A refusal, a kill, a deadline, or an outcome
/// nobody could prove is not, and a cancel is the turn's own ending rather than a failure.
#[must_use]
pub const fn outcome_of(resolution: Resolution) -> TurnOutcome {
    match resolution {
        Resolution::Finished { .. } => TurnOutcome::Completed,
        Resolution::Cancelled => TurnOutcome::Cancelled,
        Resolution::Killed { .. }
        | Resolution::TimedOut
        | Resolution::Refused
        | Resolution::Unknown => TurnOutcome::Failed,
    }
}

fn commit(
    db: &mut Db,
    command: CommandId,
    session: SessionID,
    entries: &[Entry],
    notices: Vec<OutboxMessage>,
) -> Result<Outcome, Error> {
    let stream = Db::stream(session);
    let expected = Expected::At(db.version(stream)?);

    db.append(command, stream, expected, entries, notices)
}

/// Reads back what the submitting command did.
///
/// The command names its own turn, so the report describes that command's run: a replay reports the
/// earlier run rather than the session's newest turn.
fn report(
    db: &Db,
    session: SessionID,
    command: CommandId,
    replayed: bool,
) -> Result<Submitted, Error> {
    let log = db.log(session)?;
    let state = log.state(session)?;
    let turn = TurnID::from_uuid(command.as_uuid());
    let held = log.turn(turn)?;
    let number = held.number().map(TurnNumber::get);
    let outcome = held.outcome();
    let (execution, resolution) = match held.tools().last() {
        Some((id, tool)) => (Some(id), tool.resolution()),
        None => (None, None),
    };

    Ok(Submitted {
        session,
        replayed,
        phase: state.phase(),
        turn: Some(turn),
        number,
        outcome,
        execution,
        resolution,
        result: execution.and_then(|execution| log.result(execution).cloned()),
    })
}

/// A [`Moment`] as the version 7 timestamp the identifiers are minted from.
fn timestamp(at: Moment) -> uuid::Timestamp {
    uuid::Timestamp::from_unix_time(
        at.as_millis() / 1_000,
        u32::try_from((at.as_millis() % 1_000) * 1_000_000).unwrap_or(0),
        0,
        0,
    )
}

#[cfg(test)]
mod tests {
    use super::{Notice, outcome_of, timestamp};
    use mokmok_event_store::CommandId;
    use mokmok_protocol::{Moment, SessionID};
    use mokmok_state_machine::{Resolution, TurnOutcome};
    use uuid::Uuid;

    #[test]
    fn a_finished_tool_completes_the_turn_and_everything_else_fails_it() {
        assert_eq!(
            outcome_of(Resolution::Finished { code: 0 }),
            TurnOutcome::Completed,
        );
        assert_eq!(
            outcome_of(Resolution::Finished { code: 101 }),
            TurnOutcome::Completed,
        );
        assert_eq!(outcome_of(Resolution::Refused), TurnOutcome::Failed);
        assert_eq!(outcome_of(Resolution::Unknown), TurnOutcome::Failed);
        assert_eq!(
            outcome_of(Resolution::Killed { signal: 9 }),
            TurnOutcome::Failed,
        );
        assert_eq!(outcome_of(Resolution::TimedOut), TurnOutcome::Failed);
        assert_eq!(outcome_of(Resolution::Cancelled), TurnOutcome::Cancelled);
    }

    #[test]
    fn a_moment_becomes_the_timestamp_the_identifiers_are_minted_from() {
        let at = Moment::from_millis(1_700_000_000_500);
        let stamp = timestamp(at);
        let (seconds, nanos) = stamp.to_unix();

        assert_eq!(seconds, 1_700_000_000);
        assert_eq!(nanos, 500_000_000);
    }

    #[test]
    fn a_notice_names_the_session_topic_and_carries_its_identity() {
        use crate::db::Db;

        let data =
            std::env::temp_dir().join(format!("mokmok-kernel-notice-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&data);
        let mut db = Db::open(&data.join("events.sqlite")).unwrap();
        let session = SessionID::from_uuid(Uuid::from_u128(7));
        let command = CommandId::from_uuid(Uuid::from_u128(8));
        let notice = Notice {
            session,
            command,
            turn: None,
            execution: None,
            resolution: Some(Resolution::Unknown),
            at: Moment::from_millis(1),
        };

        db.append(
            command,
            Db::stream(session),
            mokmok_event_store::Expected::New,
            &[crate::db::Entry::Session(
                mokmok_state_machine::Fact::SessionOpened,
            )],
            vec![notice.outbox().unwrap()],
        )
        .unwrap();

        let pending = db.pending().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].topic(), format!("session:{}", session.as_uuid()));
        assert_eq!(
            pending[0].payload().decode::<Notice>().unwrap().command,
            command,
        );
        let _ = std::fs::remove_dir_all(&data);
    }
}

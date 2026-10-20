//! The one serialized path into the event log, and the fact log it holds.
//!
//! One aggregate is one stream, and one session is one aggregate: its stream carries the session's
//! own facts, its turns' facts, and the reference to what each execution produced, in the order
//! the log recorded them, so everything that must commit together does. Each entry says which
//! machine it belongs to in its event kind, because a stored payload is the only durable form any
//! of those machines has.
//!
//! Every append goes through one `&mut self` here, which is the workspace's whole answer to
//! concurrent writers: the caller serializes on the store, and the dedup row keyed on the command
//! id is what makes a retried command a replay rather than a second execution.

use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroUsize;
use std::path::Path;

use mokmok_agent_runtime::{Record, Refusal, Transcript};
use mokmok_event_store::{
    Append, CommandId, CommitPosition, Expected, Kind, NewEvent, OutboxMessage, Outcome, Payload,
    Store, StreamId, Version,
};
use mokmok_protocol::{ExecutionID, SessionID, TurnID};
use mokmok_state_machine::{Fact, Session, State, Turn, TurnFact};

use crate::error::Error;
use crate::execution::ExecutionResult;

/// How many commits one read of the log asks for.
const PAGE: NonZeroUsize = match NonZeroUsize::new(64) {
    Some(page) => page,
    None => NonZeroUsize::MIN,
};

/// The kind under which a session's own facts are logged.
const SESSION_FACT: &str = "mokmok.session.fact";
/// The kind under which one turn's facts are logged.
const TURN_FACT: &str = "mokmok.turn.fact";
/// The kind under which the reference to one execution's bytes is logged.
const RESULT: &str = "mokmok.execution.result";
/// The kind under which the agent runtime's own records are logged.
const AGENT: &str = "mokmok.agent.record";

/// One durable entry of a session's log.
#[derive(Clone, Debug, PartialEq)]
pub enum Entry {
    /// A fact of the session machine.
    Session(Fact),
    /// A fact of one turn's machine.
    Turn(TurnID, TurnFact),
    /// What one execution produced.
    Result(ExecutionID, ExecutionResult),
    /// One record of the agent runtime's own vocabulary.
    Agent(Record),
}

impl Entry {
    /// The event kind this entry is logged under.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Session(_) => SESSION_FACT,
            Self::Turn(..) => TURN_FACT,
            Self::Result(..) => RESULT,
            Self::Agent(_) => AGENT,
        }
    }

    fn event(&self) -> Result<NewEvent, Error> {
        let kind = Kind::new(self.kind()).map_err(|_| Error::NotAName(self.kind()))?;
        let payload = match self {
            Self::Session(fact) => Payload::json(fact)?,
            Self::Turn(turn, fact) => Payload::json(&TurnRecord {
                turn: *turn,
                fact: *fact,
            })?,
            Self::Result(execution, result) => Payload::json(&ResultRecord {
                execution: *execution,
                result: result.clone(),
            })?,
            Self::Agent(record) => Payload::json(record)?,
        };

        Ok(NewEvent::new(kind, payload))
    }
}

/// One turn's fact as the log holds it: the turn's identity, which the fact itself does not carry.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct TurnRecord {
    pub turn: TurnID,
    pub fact: TurnFact,
}

/// One execution's result as the log holds it: the execution's identity beside the reference.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct ResultRecord {
    pub execution: ExecutionID,
    pub result: ExecutionResult,
}

/// Every entry a session's log holds, in the order it was recorded.
#[derive(Clone, Debug, Default)]
pub struct Log {
    pub session: Vec<Fact>,
    pub turns: Vec<TurnRecord>,
    pub results: Vec<ResultRecord>,
    /// Every record the runtime's fold reads, in log order.
    ///
    /// The fold needs the interleaving of the machines' facts with the runtime's own content
    /// records, which one `Vec` per kind cannot express. A slice-era [`Entry::Result`] is left out:
    /// it names bytes rather than a report, and the fold renders that settlement from its own
    /// facts.
    pub ordered: Vec<Record>,
}

impl Log {
    /// The session state its own facts fold to.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Rejected`] when the history is not one this build's rules admit.
    pub fn state(
        &self,
        session: SessionID,
    ) -> Result<Session, Error> {
        Ok(Session::replay(session, &self.session)?)
    }

    /// One turn's state, folded from the facts logged under its identity.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Rejected`] when the turn's history is not one this build's rules admit.
    pub fn turn(
        &self,
        turn: TurnID,
    ) -> Result<Turn, Error> {
        let facts = self
            .turns
            .iter()
            .filter(|record| record.turn == turn)
            .map(|record| record.fact)
            .collect::<Vec<_>>();

        Ok(Turn::replay(&facts)?)
    }

    /// The turns the log opened, in the order they first appear.
    #[must_use]
    pub fn turns_in_order(&self) -> Vec<TurnID> {
        let mut seen = BTreeSet::new();
        let mut order = Vec::new();
        for record in &self.turns {
            if seen.insert(record.turn) {
                order.push(record.turn);
            }
        }

        order
    }

    /// The last turn the log opened, which is the one the newest command ran under.
    #[must_use]
    pub fn last_turn(&self) -> Option<TurnID> {
        self.turns_in_order().last().copied()
    }

    /// What one execution produced, when its result was recorded.
    #[must_use]
    pub fn result(
        &self,
        execution: ExecutionID,
    ) -> Option<&ExecutionResult> {
        self.results
            .iter()
            .find(|record| record.execution == execution)
            .map(|record| &record.result)
    }

    /// The turn whose log requested `execution`.
    #[must_use]
    pub fn turn_of(
        &self,
        execution: ExecutionID,
    ) -> Option<TurnID> {
        self.turns
            .iter()
            .find(|record| {
                matches!(
                    record.fact,
                    TurnFact::ToolRequested { execution: requested, .. } if requested == execution
                )
            })
            .map(|record| record.turn)
    }

    /// Folds `entries` onto the state the log already holds, and refuses what the machines refuse.
    ///
    /// The kernel is the log's only writer, so this is where a mistake in its own reasoning is
    /// caught before it becomes a history no reader can replay. A step the machines refuse is never
    /// written at all.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Rejected`] when the log itself does not replay, [`Error::Fold`] when the
    /// runtime's fold refuses a record in neither of those ways, and [`Error::WriteRefused`] when
    /// one of `entries` is a step a machine refuses.
    pub fn verify(
        &self,
        session: SessionID,
        entries: &[Entry],
    ) -> Result<(), Error> {
        let mut transcript = Transcript::replay(session, &self.ordered).map_err(Error::Fold)?;

        for entry in entries {
            let record = match entry {
                Entry::Session(fact) => Record::Session { fact: *fact },
                Entry::Turn(turn, fact) => Record::Turn {
                    turn: *turn,
                    fact: *fact,
                },
                Entry::Agent(record) => record.clone(),
                // A slice-era result carries references, not the report the fold checks, and the
                // settlement it names was verified where it entered.
                Entry::Result(..) => continue,
            };
            transcript.apply(&record).map_err(write_refused)?;
        }

        Ok(())
    }

    /// Folds committed entries onto this log without reading them back.
    ///
    /// A writer that just committed a batch keeps its own fold current, so the next
    /// [`verify`](Self::verify) replays the history that includes it.
    pub fn absorb(
        &mut self,
        entries: &[Entry],
    ) {
        for entry in entries {
            match entry {
                Entry::Session(fact) => {
                    self.session.push(*fact);
                    self.ordered.push(Record::Session { fact: *fact });
                },
                Entry::Turn(turn, fact) => {
                    self.turns.push(TurnRecord {
                        turn: *turn,
                        fact: *fact,
                    });
                    self.ordered.push(Record::Turn {
                        turn: *turn,
                        fact: *fact,
                    });
                },
                Entry::Result(execution, result) => self.results.push(ResultRecord {
                    execution: *execution,
                    result: result.clone(),
                }),
                Entry::Agent(record) => self.ordered.push(record.clone()),
            }
        }
    }
}

/// Reads a fold refusal of one of the entries a writer is about to commit.
fn write_refused(refusal: Refusal) -> Error {
    match refusal {
        Refusal::Rejected(rejection) => Error::WriteRefused(rejection),
        other => Error::Fold(other),
    }
}

/// The event log, owned and written by one caller at a time.
pub struct Db {
    store: Store,
}

impl Db {
    /// Opens the log at `path`, creating it when it is not there yet.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when the file cannot be opened, created, or migrated.
    pub fn open(path: &Path) -> Result<Self, Error> {
        Ok(Self {
            store: Store::open(path)?,
        })
    }

    /// The stream one session's aggregate lives in.
    #[must_use]
    pub const fn stream(session: SessionID) -> StreamId {
        StreamId::from_uuid(session.as_uuid())
    }

    /// The stream's current version, which a caller passes back as the expectation of its next
    /// append so that a concurrent writer is detected rather than overwritten.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when the version cannot be read.
    pub fn version(
        &self,
        stream: StreamId,
    ) -> Result<Version, Error> {
        Ok(self
            .store
            .history(stream, Version::INITIAL, PAGE)?
            .version())
    }

    /// Whether the log already holds this command, which makes a redelivered command a replay.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when the dedup row cannot be read.
    pub fn committed(
        &self,
        command: CommandId,
    ) -> Result<bool, Error> {
        self.store.committed(command).map_err(Error::Store)
    }

    /// Appends one step: the entries in order, and the notifications to publish once the
    /// transaction has committed.
    ///
    /// The notifications are taken by value because the store's own type is not cloneable: a
    /// notice is written into the transaction it belongs to and is not something to copy.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when the transaction cannot be committed, [`Error::Empty`] when
    /// there is nothing to append, and [`Error::NotAName`] when an entry's own kind name is not
    /// one.
    pub fn append(
        &mut self,
        command: CommandId,
        stream: StreamId,
        expected: Expected,
        entries: &[Entry],
        notices: Vec<OutboxMessage>,
    ) -> Result<Outcome, Error> {
        let (first, rest) = entries.split_first().ok_or(Error::Empty)?;

        let mut append = Append::new(command, stream, expected, first.event()?);
        for entry in rest {
            append = append.event(entry.event()?);
        }
        for notice in notices {
            append = append.outbox(notice);
        }

        Ok(self.store.append(&append)?)
    }

    /// Everything the session's log holds, in the order the log recorded it.
    ///
    /// An entry of a kind this build does not know stops the read rather than being skipped: a
    /// stream carries one aggregate's facts, so an unknown entry means this build is reading a log
    /// its own writer did not write, and guessing past it would replay the wrong history.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when a commit cannot be read, [`Error::UnexpectedEntry`] for an
    /// entry of another kind, and [`Error::Payload`] for a payload that is not the record its kind
    /// names.
    pub fn log(
        &self,
        session: SessionID,
    ) -> Result<Log, Error> {
        let mut log = Log::default();
        let mut from = Version::INITIAL;

        loop {
            let slice = self.store.history(Self::stream(session), from, PAGE)?;
            let commits = slice.commits();
            if commits.is_empty() {
                return Ok(log);
            }

            for committed in commits {
                for entry in committed.events() {
                    log.push(entry.kind(), entry.payload(), entry.position().get())?;
                }
                from = committed.version();
            }
        }
    }

    /// Every stream the commit log holds, the stream written most recently first.
    ///
    /// The position of a stream's newest commit is the order a chooser wants, and one read over the
    /// commit log is where it comes from: no registry beside the log has to agree with it.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when the log cannot be read.
    pub fn streams(&self) -> Result<Vec<StreamId>, Error> {
        let mut newest: BTreeMap<StreamId, u64> = BTreeMap::new();
        let mut from = CommitPosition::FIRST;

        loop {
            let commits = self.store.log(from, PAGE)?;
            let Some(last) = commits.last() else {
                break;
            };

            for committed in &commits {
                for event in committed.events() {
                    newest.insert(event.stream(), committed.position().get());
                }
            }

            from = last.position().next();
        }

        let mut streams: Vec<(StreamId, u64)> = newest.into_iter().collect();
        streams.sort_by_key(|(_, position)| std::cmp::Reverse(*position));

        Ok(streams.into_iter().map(|(stream, _)| stream).collect())
    }

    /// The sessions whose log still holds an execution with no recorded outcome.
    ///
    /// Every event carries the stream that owns it, and a session's stream is its id, so the commit
    /// log is already the index of sessions: one read over it reaches every one without a registry
    /// to consult. A session with no outcome for an execution folds to [`State::InFlight`], which
    /// is the state recovery exists to settle.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when the log cannot be read, [`Error::Payload`] for a session fact
    /// that is not the record its kind names, and [`Error::Rejected`] when a session's facts do not
    /// replay.
    pub(crate) fn unsettled_sessions(&self) -> Result<Vec<SessionID>, Error> {
        let session_fact = named(SESSION_FACT)?;
        let mut facts: BTreeMap<StreamId, Vec<Fact>> = BTreeMap::new();
        let mut from = CommitPosition::FIRST;

        loop {
            let commits = self.store.log(from, PAGE)?;
            let Some(last) = commits.last() else {
                break;
            };

            for committed in &commits {
                for event in committed.events() {
                    if *event.kind() == session_fact {
                        facts
                            .entry(event.stream())
                            .or_default()
                            .push(event.payload().decode()?);
                    }
                }
            }

            from = last.position().next();
        }

        let mut unsettled = Vec::new();
        for (stream, held) in facts {
            let session = SessionID::from_uuid(stream.as_uuid());
            if Session::replay(session, &held)?
                .executions()
                .any(|(_, execution)| execution.state() == State::InFlight)
            {
                unsettled.push(session);
            }
        }

        Ok(unsettled)
    }

    /// How many notices are waiting for the relay.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when the rows cannot be read.
    pub fn pending(&self) -> Result<Vec<mokmok_event_store::OutboxEntry>, Error> {
        Ok(self.store.pending_outbox(PAGE)?)
    }

    /// Drops every notice up to and including `through`, which the relay calls once the bus has
    /// stored them.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when the rows cannot be deleted.
    pub fn settle_notices(
        &mut self,
        through: mokmok_event_store::OutboxId,
    ) -> Result<u64, Error> {
        Ok(self.store.settle_outbox(through)?)
    }
}

impl Log {
    fn push(
        &mut self,
        kind: &Kind,
        payload: &Payload,
        position: u64,
    ) -> Result<(), Error> {
        if *kind == named(SESSION_FACT)? {
            let fact: Fact = payload.decode()?;
            self.session.push(fact);
            self.ordered.push(Record::Session { fact });
        } else if *kind == named(TURN_FACT)? {
            let record: TurnRecord = payload.decode()?;
            self.turns.push(record);
            self.ordered.push(Record::Turn {
                turn: record.turn,
                fact: record.fact,
            });
        } else if *kind == named(RESULT)? {
            let record: ResultRecord = payload.decode()?;
            self.results.push(record);
        } else if *kind == named(AGENT)? {
            self.ordered.push(payload.decode()?);
        } else {
            return Err(Error::UnexpectedEntry {
                position,
                expected: "session, turn, result, or agent",
            });
        }

        Ok(())
    }
}

fn named(kind: &'static str) -> Result<Kind, Error> {
    Kind::new(kind).map_err(|_| Error::NotAName(kind))
}

#[cfg(test)]
mod tests {
    use super::{Db, Entry};
    use crate::execution::{Blob, ExecutionResult};
    use mokmok_agent_runtime::Record;
    use mokmok_event_store::{CommandId, Expected, Outcome, Payload};
    use mokmok_protocol::{ExecutionID, SessionID, TurnID};
    use mokmok_state_machine::{Fact, Phase, Resolution, TurnFact, TurnNumber, TurnOutcome};

    use std::path::PathBuf;
    use uuid::Uuid;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir()
                .join(format!("mokmok-kernel-db-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);

            Self(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn session() -> SessionID {
        SessionID::from_uuid(Uuid::from_u128(1))
    }

    fn turn() -> TurnID {
        TurnID::from_uuid(Uuid::from_u128(2))
    }

    fn execution() -> ExecutionID {
        ExecutionID::from_uuid(Uuid::from_u128(3))
    }

    fn result() -> ExecutionResult {
        ExecutionResult {
            code: Some(0),
            signal: None,
            attempted: true,
            stdout: Blob {
                hash: "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".to_owned(),
                bytes: 6,
                truncated: false,
            },
            stderr: Blob {
                hash: "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".to_owned(),
                bytes: 0,
                truncated: false,
            },
            artifacts: Vec::new(),
        }
    }

    #[test]
    fn a_log_of_every_kind_replays_after_a_reopen() {
        let scratch = Scratch::new("replay");
        let path = scratch.0.join("events.sqlite");
        let entries = [
            Entry::Session(Fact::SessionOpened),
            Entry::Turn(
                turn(),
                TurnFact::TurnOpened {
                    number: TurnNumber::new(1),
                },
            ),
            Entry::Turn(
                turn(),
                TurnFact::ModelRequested {
                    attempt: mokmok_state_machine::Attempt::new(1),
                },
            ),
            Entry::Turn(turn(), TurnFact::ModelAnswered),
            Entry::Turn(
                turn(),
                TurnFact::ToolRequested {
                    execution: execution(),
                    deadline: mokmok_state_machine::Deadline::Bounded,
                },
            ),
            Entry::Turn(
                turn(),
                TurnFact::ToolResolved {
                    execution: execution(),
                    resolution: Resolution::Finished { code: 0 },
                },
            ),
            Entry::Turn(
                turn(),
                TurnFact::TurnSettled {
                    outcome: TurnOutcome::Completed,
                },
            ),
            Entry::Result(execution(), result()),
        ];

        {
            let mut db = Db::open(&path).unwrap();
            let outcome = db
                .append(
                    CommandId::new(),
                    Db::stream(session()),
                    Expected::New,
                    &entries,
                    Vec::new(),
                )
                .unwrap();
            assert!(matches!(outcome, Outcome::Committed(_)));
        }

        let db = Db::open(&path).unwrap();
        let log = db.log(session()).unwrap();

        assert_eq!(log.session, vec![Fact::SessionOpened]);
        assert_eq!(log.turns.len(), 6);
        assert_eq!(log.results.len(), 1);
        assert_eq!(log.turns_in_order(), vec![turn()]);
        assert_eq!(log.last_turn(), Some(turn()));
        assert_eq!(log.turn_of(execution()), Some(turn()));
        assert_eq!(log.result(execution()).map(|r| r.code), Some(Some(0)));
        assert_eq!(
            log.turn(turn()).unwrap().outcome(),
            Some(TurnOutcome::Completed)
        );
        assert_eq!(log.state(session()).unwrap().phase(), Phase::Accepting);
        // The fold reads the machines' facts in log order, and a slice-era result names bytes
        // rather than a report, so it is left out.
        assert_eq!(
            log.ordered.len(),
            7,
            "the result entry is not a foldable record"
        );
        assert!(matches!(
            log.ordered.first(),
            Some(Record::Session {
                fact: Fact::SessionOpened
            }),
        ));
    }

    #[test]
    fn a_retried_command_replays_without_a_second_write() {
        let scratch = Scratch::new("dedup");
        let mut db = Db::open(&scratch.0.join("events.sqlite")).unwrap();
        let command = CommandId::new();
        let entries = [Entry::Session(Fact::SessionOpened)];

        let first = db
            .append(
                command,
                Db::stream(session()),
                Expected::New,
                &entries,
                Vec::new(),
            )
            .unwrap();
        let second = db
            .append(
                command,
                Db::stream(session()),
                Expected::New,
                &entries,
                Vec::new(),
            )
            .unwrap();

        assert!(matches!(first, Outcome::Committed(_)));
        assert!(matches!(second, Outcome::Replayed(_)));
        assert_eq!(
            db.log(session()).unwrap().session,
            vec![Fact::SessionOpened]
        );
    }

    #[test]
    fn a_step_the_machines_refuse_is_refused_before_it_can_be_written() {
        let scratch = Scratch::new("verify-refusal");
        let mut db = Db::open(&scratch.0.join("events.sqlite")).unwrap();
        db.append(
            CommandId::new(),
            Db::stream(session()),
            Expected::New,
            &[Entry::Session(Fact::SessionOpened)],
            Vec::new(),
        )
        .unwrap();
        let log = db.log(session()).unwrap();

        // A turn nobody opened cannot settle, so a settlement with no turn to settle is the one
        // history the kernel's own reasoning can produce and a reader could not replay.
        let refused = log.verify(
            session(),
            &[Entry::Turn(
                turn(),
                TurnFact::TurnSettled {
                    outcome: TurnOutcome::Completed,
                },
            )],
        );

        assert!(
            matches!(refused, Err(crate::error::Error::WriteRefused(_))),
            "{refused:?}",
        );
        // The control the refusal is read against: the same call with the turn's own opening fact
        // is accepted, so the log refuses the settlement rather than every entry it is handed.
        assert!(
            log.verify(
                session(),
                &[Entry::Turn(
                    turn(),
                    TurnFact::TurnOpened {
                        number: TurnNumber::new(1),
                    },
                )],
            )
            .is_ok(),
        );
    }

    #[test]
    fn an_unexpected_expectation_is_conflicted_and_writes_nothing() {
        let scratch = Scratch::new("conflict");
        let mut db = Db::open(&scratch.0.join("events.sqlite")).unwrap();

        db.append(
            CommandId::new(),
            Db::stream(session()),
            Expected::New,
            &[Entry::Session(Fact::SessionOpened)],
            Vec::new(),
        )
        .unwrap();
        let again = db
            .append(
                CommandId::new(),
                Db::stream(session()),
                Expected::New,
                &[Entry::Session(Fact::SessionClosed)],
                Vec::new(),
            )
            .unwrap();

        assert!(matches!(again, Outcome::Conflicted { .. }));
        assert_eq!(
            db.log(session()).unwrap().session,
            vec![Fact::SessionOpened]
        );
    }

    #[test]
    fn a_log_past_one_page_still_replays_whole() {
        let scratch = Scratch::new("paging");
        let mut db = Db::open(&scratch.0.join("events.sqlite")).unwrap();
        let opened = (0..70)
            .map(|_| Entry::Session(Fact::SessionOpened))
            .collect::<Vec<_>>();

        db.append(
            CommandId::new(),
            Db::stream(session()),
            Expected::New,
            &opened,
            Vec::new(),
        )
        .unwrap();

        assert_eq!(db.log(session()).unwrap().session.len(), 70);
        assert_eq!(db.version(Db::stream(session())).unwrap().get(), 70);
    }

    #[test]
    fn an_entry_of_an_unknown_kind_stops_the_read() {
        let scratch = Scratch::new("other-kind");
        let mut db = Db::open(&scratch.0.join("events.sqlite")).unwrap();
        db.append(
            CommandId::new(),
            Db::stream(session()),
            Expected::New,
            &[Entry::Session(Fact::SessionOpened)],
            Vec::new(),
        )
        .unwrap();

        let stray = mokmok_event_store::Append::new(
            CommandId::new(),
            Db::stream(session()),
            Expected::Any,
            mokmok_event_store::NewEvent::new(
                mokmok_event_store::Kind::new("mokmok.stray.fact").unwrap(),
                Payload::json(&Fact::SessionClosed).unwrap(),
            ),
        )
        .event(mokmok_event_store::NewEvent::new(
            mokmok_event_store::Kind::new("mokmok.session.fact").unwrap(),
            Payload::json(&Fact::SessionClosed).unwrap(),
        ));
        db.store.append(&stray).unwrap();

        assert!(matches!(
            db.log(session()),
            Err(crate::error::Error::UnexpectedEntry { .. }),
        ));
    }

    #[test]
    fn only_a_session_with_an_execution_still_in_flight_is_unsettled() {
        let scratch = Scratch::new("unsettled");
        let mut db = Db::open(&scratch.0.join("events.sqlite")).unwrap();
        let done = SessionID::from_uuid(Uuid::from_u128(0x10));
        let done_turn = TurnID::from_uuid(Uuid::from_u128(0x20));
        let done_execution = ExecutionID::from_uuid(Uuid::from_u128(0x30));

        db.append(
            CommandId::new(),
            Db::stream(session()),
            Expected::New,
            &[
                Entry::Session(Fact::SessionOpened),
                Entry::Session(Fact::ExecutionLaunched {
                    execution: execution(),
                    deadline: mokmok_state_machine::Deadline::Bounded,
                }),
                Entry::Turn(
                    turn(),
                    TurnFact::TurnOpened {
                        number: TurnNumber::new(1),
                    },
                ),
                Entry::Turn(
                    turn(),
                    TurnFact::ModelRequested {
                        attempt: mokmok_state_machine::Attempt::new(1),
                    },
                ),
                Entry::Turn(turn(), TurnFact::ModelAnswered),
                Entry::Turn(
                    turn(),
                    TurnFact::ToolRequested {
                        execution: execution(),
                        deadline: mokmok_state_machine::Deadline::Bounded,
                    },
                ),
            ],
            Vec::new(),
        )
        .unwrap();

        db.append(
            CommandId::new(),
            Db::stream(done),
            Expected::New,
            &[
                Entry::Session(Fact::SessionOpened),
                Entry::Session(Fact::ExecutionLaunched {
                    execution: done_execution,
                    deadline: mokmok_state_machine::Deadline::Bounded,
                }),
                Entry::Session(Fact::ExecutionSettled {
                    execution: done_execution,
                    resolution: Resolution::Unknown,
                }),
                Entry::Turn(
                    done_turn,
                    TurnFact::TurnOpened {
                        number: TurnNumber::new(1),
                    },
                ),
                Entry::Turn(
                    done_turn,
                    TurnFact::ModelRequested {
                        attempt: mokmok_state_machine::Attempt::new(1),
                    },
                ),
                Entry::Turn(done_turn, TurnFact::ModelAnswered),
                Entry::Turn(
                    done_turn,
                    TurnFact::ToolRequested {
                        execution: done_execution,
                        deadline: mokmok_state_machine::Deadline::Bounded,
                    },
                ),
                Entry::Turn(
                    done_turn,
                    TurnFact::ToolResolved {
                        execution: done_execution,
                        resolution: Resolution::Unknown,
                    },
                ),
                Entry::Turn(
                    done_turn,
                    TurnFact::TurnSettled {
                        outcome: TurnOutcome::Failed,
                    },
                ),
            ],
            Vec::new(),
        )
        .unwrap();

        assert_eq!(db.unsettled_sessions().unwrap(), vec![session()]);
    }

    #[test]
    fn streams_answers_every_stream_the_commit_log_holds_newest_first() {
        let scratch = Scratch::new("streams");
        let path = scratch.0.join("events.sqlite");
        let mut db = Db::open(&path).unwrap();

        assert!(
            db.streams().unwrap().is_empty(),
            "a log with nothing committed holds no stream",
        );

        let older = SessionID::from_uuid(Uuid::from_u128(4));
        let newer = SessionID::from_uuid(Uuid::from_u128(5));
        for session in [older, newer] {
            db.append(
                CommandId::new(),
                Db::stream(session),
                Expected::New,
                &[Entry::Session(Fact::SessionOpened)],
                Vec::new(),
            )
            .unwrap();
        }
        // The older stream is written once more, so the order cannot be the order of first writes.
        db.append(
            CommandId::new(),
            Db::stream(older),
            Expected::At(db.version(Db::stream(older)).unwrap()),
            &[Entry::Session(Fact::CancelRequested)],
            Vec::new(),
        )
        .unwrap();

        assert_eq!(
            db.streams().unwrap(),
            vec![Db::stream(older), Db::stream(newer)],
        );
    }
}

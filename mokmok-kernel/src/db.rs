//! The one serialized path into the event log, and the fact log it holds.
//!
//! One aggregate is one stream, and one session is one aggregate: its stream carries the session's
//! own facts and its turns' facts, in the order the log recorded them, so the facts that must
//! commit together do. A [`Fact`] is written as the payload of an event whose kind says which
//! machine it belongs to, because the stored payload is the only durable form either machine has.
//!
//! Every append goes through one `&mut self` here, which is the workspace's whole answer to
//! concurrent writers: the caller serializes on the store, and the dedup row keyed on the command
//! id is what makes a retried command a replay rather than a second execution.

use std::num::NonZeroUsize;
use std::path::Path;

use mokmok_event_store::{
    Append, CommandId, Expected, Kind, NewEvent, OutboxMessage, Outcome, Payload, Store, StreamId,
    Version,
};
use mokmok_protocol::SessionID;
use mokmok_state_machine::{Fact, Session};

use crate::error::Error;

/// How many commits one read of the log asks for.
const PAGE: NonZeroUsize = match NonZeroUsize::new(64) {
    Some(page) => page,
    None => NonZeroUsize::MIN,
};

/// The kind under which a session's own facts are logged.
const SESSION_FACT: &str = "mokmok.session.fact";

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

    /// Appends one step: the stream's facts in order, and the notifications to publish once the
    /// transaction has committed.
    ///
    /// The notifications are taken by value because the store's own type is not cloneable: a
    /// notice is written into the transaction it belongs to and is not something to copy.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when the transaction cannot be committed, [`Error::Empty`] when
    /// there is nothing to append, and [`Error::NotAName`] when the log's own kind name is not one.
    pub fn append(
        &mut self,
        command: CommandId,
        stream: StreamId,
        expected: Expected,
        facts: &[Fact],
        notices: Vec<OutboxMessage>,
    ) -> Result<Outcome, Error> {
        let (first, rest) = facts.split_first().ok_or(Error::Empty)?;

        let mut append = Append::new(command, stream, expected, event(SESSION_FACT, first)?);
        for fact in rest {
            append = append.event(event(SESSION_FACT, fact)?);
        }
        for notice in notices {
            append = append.outbox(notice);
        }

        Ok(self.store.append(&append)?)
    }

    /// Every session fact of `session`, in the order the log recorded them.
    ///
    /// A log entry of any other kind is an error rather than something skipped: a stream carries
    /// the facts of one aggregate, so an entry that is not one means this build is reading a log
    /// its own writer did not write, and guessing past that would replay the wrong history.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when a commit cannot be read, [`Error::UnexpectedEntry`] for an
    /// entry of another kind, and [`Error::Payload`] for a payload that is not a fact.
    pub fn facts(
        &self,
        session: SessionID,
    ) -> Result<Vec<Fact>, Error> {
        let expected_kind = Kind::new(SESSION_FACT).map_err(|_| Error::NotAName(SESSION_FACT))?;
        let mut facts = Vec::new();
        let mut from = Version::INITIAL;

        loop {
            let slice = self.store.history(Self::stream(session), from, PAGE)?;
            let commits = slice.commits();
            if commits.is_empty() {
                return Ok(facts);
            }

            for committed in commits {
                for entry in committed.events() {
                    if *entry.kind() != expected_kind {
                        return Err(Error::UnexpectedEntry {
                            position: entry.position().get(),
                            expected: SESSION_FACT,
                        });
                    }
                    facts.push(entry.payload().decode::<Fact>()?);
                }
                from = committed.version();
            }
        }
    }

    /// The session state its own log replays to.
    ///
    /// # Errors
    ///
    /// As [`Db::facts`], plus [`Error::Rejected`] when the history is not a history this build's
    /// rules admit.
    pub fn session(
        &self,
        session: SessionID,
    ) -> Result<Session, Error> {
        Ok(Session::replay(session, &self.facts(session)?)?)
    }
}

fn event(
    kind: &'static str,
    fact: &Fact,
) -> Result<NewEvent, Error> {
    Ok(NewEvent::new(
        Kind::new(kind).map_err(|_| Error::NotAName(kind))?,
        Payload::json(fact)?,
    ))
}

#[cfg(test)]
mod tests {
    use super::{Db, SESSION_FACT};
    use crate::error::Error;
    use mokmok_event_store::{
        CommandId, Expected, Kind, NewEvent, OutboxMessage, Outcome, Payload, Version,
    };
    use mokmok_protocol::SessionID;
    use mokmok_state_machine::{Fact, Phase};
    use std::num::NonZeroUsize;
    use std::path::PathBuf;

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
        "018f2f1e-0000-7000-8000-000000000000"
            .parse::<uuid::Uuid>()
            .map(SessionID::from_uuid)
            .unwrap()
    }

    #[test]
    fn a_session_replays_out_of_the_log_after_a_reopen() {
        let scratch = Scratch::new("replay");
        let path = scratch.0.join("events.sqlite");

        {
            let mut db = Db::open(&path).unwrap();
            let facts = [Fact::SessionOpened, Fact::SessionClosed];
            let outcome = db
                .append(
                    CommandId::new(),
                    Db::stream(session()),
                    Expected::New,
                    &facts,
                    Vec::new(),
                )
                .unwrap();

            assert!(matches!(outcome, Outcome::Committed(_)));
        }

        let db = Db::open(&path).unwrap();
        let replayed = db.session(session()).unwrap();

        assert_eq!(
            replayed.phase(),
            Phase::Closed(mokmok_state_machine::Ending::Completed)
        );
        assert_eq!(db.facts(session()).unwrap().len(), 2);
    }

    #[test]
    fn a_retried_command_replays_without_a_second_write() {
        let scratch = Scratch::new("dedup");
        let mut db = Db::open(&scratch.0.join("events.sqlite")).unwrap();
        let command = CommandId::new();
        let facts = [Fact::SessionOpened];

        let first = db
            .append(
                command,
                Db::stream(session()),
                Expected::New,
                &facts,
                Vec::new(),
            )
            .unwrap();
        let second = db
            .append(
                command,
                Db::stream(session()),
                Expected::New,
                &facts,
                Vec::new(),
            )
            .unwrap();

        assert!(matches!(first, Outcome::Committed(_)));
        assert!(matches!(second, Outcome::Replayed(_)));
        assert_eq!(db.facts(session()).unwrap(), vec![Fact::SessionOpened]);
    }

    #[test]
    fn an_unexpected_expectation_is_conflicted_and_writes_nothing() {
        let scratch = Scratch::new("conflict");
        let mut db = Db::open(&scratch.0.join("events.sqlite")).unwrap();

        db.append(
            CommandId::new(),
            Db::stream(session()),
            Expected::New,
            &[Fact::SessionOpened],
            Vec::new(),
        )
        .unwrap();
        let again = db
            .append(
                CommandId::new(),
                Db::stream(session()),
                Expected::New,
                &[Fact::SessionClosed],
                Vec::new(),
            )
            .unwrap();

        assert!(matches!(again, Outcome::Conflicted { .. }));
        assert_eq!(db.facts(session()).unwrap(), vec![Fact::SessionOpened]);
    }

    #[test]
    fn a_log_past_one_page_still_replays_whole() {
        let scratch = Scratch::new("paging");
        let mut db = Db::open(&scratch.0.join("events.sqlite")).unwrap();
        let opened = (0..70).map(|_| Fact::SessionOpened).collect::<Vec<_>>();

        db.append(
            CommandId::new(),
            Db::stream(session()),
            Expected::New,
            &opened,
            Vec::new(),
        )
        .unwrap();

        assert_eq!(db.facts(session()).unwrap().len(), 70);
        assert_eq!(db.version(Db::stream(session())).unwrap(), Version::new(70),);
    }

    #[test]
    fn an_entry_of_another_kind_stops_the_replay() {
        let scratch = Scratch::new("other-kind");
        let mut db = Db::open(&scratch.0.join("events.sqlite")).unwrap();
        let mut append = mokmok_event_store::Append::new(
            CommandId::new(),
            Db::stream(session()),
            Expected::New,
            NewEvent::new(
                Kind::new("mokmok.other.fact").unwrap(),
                Payload::json(&Fact::SessionOpened).unwrap(),
            ),
        );
        append = append.event(NewEvent::new(
            Kind::new(SESSION_FACT).unwrap(),
            Payload::json(&Fact::SessionClosed).unwrap(),
        ));
        db.store.append(&append).unwrap();

        assert!(matches!(
            db.facts(session()),
            Err(Error::UnexpectedEntry {
                expected: "mokmok.session.fact",
                ..
            }),
        ));
    }

    #[test]
    fn a_notice_is_pending_until_it_is_settled() {
        let scratch = Scratch::new("outbox");
        let mut db = Db::open(&scratch.0.join("events.sqlite")).unwrap();
        let notice = OutboxMessage::new("session.opened", Payload::json(&"opened").unwrap());

        db.append(
            CommandId::new(),
            Db::stream(session()),
            Expected::New,
            &[Fact::SessionOpened],
            vec![notice],
        )
        .unwrap();

        let pending = db
            .store
            .pending_outbox(NonZeroUsize::new(8).unwrap())
            .unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].topic(), "session.opened");
        assert_eq!(db.store.settle_outbox(pending[0].id()).unwrap(), 1);
        assert!(
            db.store
                .pending_outbox(NonZeroUsize::new(8).unwrap())
                .unwrap()
                .is_empty(),
        );
    }
}

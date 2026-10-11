use mokmok_event_store::{
    Append, CommandId, CommitPosition, Committed, ConsumerId, Error, Event, Expected, Kind,
    NewEvent, Outcome, Payload, Processed, Store, StreamId, Version,
};
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

// The `cfg(test)` module is what tells the `allow-unwrap-in-tests`, `allow-expect-in-tests`, and
// `allow-panic-in-tests` settings in `.clippy.toml` that this harness is test code.
#[cfg(test)]
mod tests {
    use super::*;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);

            Self(std::env::temp_dir().join(format!(
                "mokmok-event-store-{name}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed),
            )))
        }

        fn db(&self) -> PathBuf {
            self.0.join("store.sqlite")
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn max(count: usize) -> NonZeroUsize {
        NonZeroUsize::new(count).unwrap()
    }

    fn number(
        connection: &rusqlite::Connection,
        sql: &str,
    ) -> i64 {
        connection
            .query_row(sql, rusqlite::params![], |row| row.get(0))
            .unwrap()
    }

    fn event(index: usize) -> NewEvent {
        NewEvent::new(
            Kind::new(format!("turn.{index}")).unwrap(),
            Payload::json(&serde_json::json!({"n": index})).unwrap(),
        )
    }

    fn append(
        command: CommandId,
        stream: StreamId,
        expected: Expected,
        count: usize,
    ) -> Append {
        let mut append = Append::new(command, stream, expected, event(0));
        for index in 1..count {
            append = append.event(event(index));
        }

        append
    }

    fn committed(
        store: &mut Store,
        append: &Append,
    ) -> Committed {
        match store.append(append).unwrap() {
            Outcome::Committed(committed) => committed,
            other => panic!("expected a commit, got {other:?}"),
        }
    }

    #[test]
    fn two_stores_race_one_expected_version_and_the_loser_retries() {
        let scratch = Scratch::new("race");
        let stream = StreamId::new();
        let mut first = Store::open(&scratch.db()).unwrap();
        let mut second = Store::open(&scratch.db()).unwrap();

        let seeded = committed(
            &mut first,
            &append(CommandId::new(), stream, Expected::New, 1),
        );

        let winner = first
            .append(&append(
                CommandId::new(),
                stream,
                Expected::At(seeded.version()),
                1,
            ))
            .unwrap();
        let Outcome::Committed(winner) = winner else {
            panic!("the first writer did not commit: {winner:?}");
        };
        assert_eq!(winner.version(), Version::new(2));

        let loser = second
            .append(&append(
                CommandId::new(),
                stream,
                Expected::At(seeded.version()),
                1,
            ))
            .unwrap();
        let Outcome::Conflicted { expected, found } = loser else {
            panic!("the second writer did not conflict: {loser:?}");
        };
        assert_eq!(expected, Expected::At(seeded.version()));
        assert_eq!(found, winner.version());

        let retried = second
            .append(&append(CommandId::new(), stream, Expected::At(found), 1))
            .unwrap();
        let Outcome::Committed(retried) = retried else {
            panic!("the retry did not commit: {retried:?}");
        };
        assert_eq!(retried.version(), Version::new(3));

        let history = second.history(stream, Version::INITIAL, max(8)).unwrap();
        assert_eq!(history.version(), Version::new(3));
        assert_eq!(history.commits().len(), 3);
    }

    #[test]
    fn a_failed_consume_rolls_back_every_write_from_a_second_connection() {
        let scratch = Scratch::new("rollback");
        let stream = StreamId::new();
        let consumer = ConsumerId::new();
        let mut store = Store::open(&scratch.db()).unwrap();
        let commit = committed(
            &mut store,
            &append(CommandId::new(), stream, Expected::New, 1),
        );

        let failed = store.consume(consumer, &commit, |tx| {
            tx.put_projection(
                consumer,
                "turn",
                commit.events()[0].payload(),
                commit.position(),
            )?;
            Err::<(), Error>(Error::Corrupt { what: "deliberate" })
        });
        assert!(failed.is_err(), "the failing closure propagated");

        let mut witness = Store::open(&scratch.db()).unwrap();
        assert_eq!(
            witness.checkpoint(consumer).unwrap(),
            None,
            "the checkpoint rolled back",
        );
        assert_eq!(
            witness
                .history(stream, Version::INITIAL, max(8))
                .unwrap()
                .version(),
            Version::new(1),
            "the stream did not grow",
        );

        let seen = witness
            .consume(consumer, &commit, |tx| tx.projection(consumer, "turn"))
            .unwrap();
        let Processed::Applied(projection) = seen else {
            panic!("the rolled-back consume left an inbox row: {seen:?}");
        };
        assert!(projection.is_none(), "the projection rolled back");
    }

    #[test]
    fn an_append_is_visible_to_a_store_opened_afterwards() {
        let scratch = Scratch::new("visibility");
        let stream = StreamId::new();
        let mut first = Store::open(&scratch.db()).unwrap();
        let commit = committed(
            &mut first,
            &append(CommandId::new(), stream, Expected::New, 2),
        );

        let second = Store::open(&scratch.db()).unwrap();
        let log = second.log(CommitPosition::FIRST, max(8)).unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].position(), commit.position());
        assert_eq!(
            log[0].events().iter().map(Event::id).collect::<Vec<_>>(),
            commit.events().iter().map(Event::id).collect::<Vec<_>>(),
            "the same events with the same ids",
        );
        assert_eq!(
            log[0]
                .events()
                .iter()
                .map(Event::position)
                .collect::<Vec<_>>(),
            commit
                .events()
                .iter()
                .map(Event::position)
                .collect::<Vec<_>>(),
            "the same events at the same positions",
        );
    }

    #[test]
    fn a_dropped_inner_append_error_does_not_leave_a_torn_commit() {
        let scratch = Scratch::new("torn-append");
        let stream = StreamId::new();
        let mut store = Store::open(&scratch.db()).unwrap();
        let seeded = committed(
            &mut store,
            &append(CommandId::new(), stream, Expected::New, 1),
        );
        assert_eq!(seeded.version(), Version::new(1));

        let raw = rusqlite::Connection::open(scratch.db()).unwrap();
        raw.execute_batch(
            "CREATE TRIGGER refuse_second_event BEFORE INSERT ON event
             WHEN NEW.ordinal = 1
             BEGIN SELECT RAISE(ABORT, 'the second event is refused'); END;",
        )
        .unwrap();

        let consumer = ConsumerId::new();
        let inner = append(CommandId::new(), stream, Expected::At(Version::new(1)), 2);
        let applied = store
            .consume(consumer, &seeded, |tx| {
                let _ = tx.append(&inner);
                Ok(())
            })
            .unwrap();
        assert!(matches!(applied, Processed::Applied(())));

        assert_eq!(
            number(&raw, "SELECT COUNT(*) FROM commit_log"),
            1,
            "the failed append left no commit row",
        );
        assert_eq!(
            number(&raw, "SELECT COUNT(*) FROM event"),
            1,
            "the failed append left no event row",
        );
        assert_eq!(
            number(&raw, "SELECT version FROM stream"),
            1,
            "the stream version did not move",
        );
        assert_eq!(
            number(&raw, "SELECT COUNT(*) FROM outbox"),
            0,
            "the failed append left no outbox row",
        );

        let fresh = committed(
            &mut store,
            &append(CommandId::new(), stream, Expected::At(Version::new(1)), 1),
        );
        assert_eq!(fresh.version(), Version::new(2));
    }
}

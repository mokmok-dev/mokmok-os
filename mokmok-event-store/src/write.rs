use core::num::NonZeroUsize;

use rusqlite::Transaction;

use crate::append::{Append, Committed, Outcome};
use crate::error::Error;
use crate::event::Slice;
use crate::model::{CommandId, CommitPosition, ConsumerId, StreamId, Version};
use crate::payload::Payload;

/// The caller's handle on the transaction that `Store::consume` opened.
///
/// Every method runs against that one transaction, so a caller's writes commit exactly when its
/// consume does.
pub struct WriteTx<'a> {
    tx: &'a Transaction<'a>,
}

impl<'a> WriteTx<'a> {
    pub(crate) const fn new(tx: &'a Transaction<'a>) -> Self {
        Self { tx }
    }

    /// The commit this command already produced, or `None` when it has not run.
    ///
    /// # Errors
    ///
    /// Returns an [`Error`] when the lookup fails.
    pub fn recorded(
        &self,
        command: CommandId,
    ) -> Result<Option<Committed>, Error> {
        crate::store::recorded_in(self.tx, command)
    }

    /// Runs the append program inside the caller's transaction.
    ///
    /// # Errors
    ///
    /// Returns an [`Error`] when a statement fails.
    pub fn append(
        &self,
        append: &Append,
    ) -> Result<Outcome, Error> {
        crate::store::append_in(self.tx, append)
    }

    /// At most `max` commits of `stream` whose version range reaches past `from`, plus the
    /// stream's snapshot and version.
    ///
    /// A commit that straddles `from` comes back whole, so a caller filtering by version still
    /// sees every event of the commit it resumed inside.
    ///
    /// # Errors
    ///
    /// Returns an [`Error`] when the rows cannot be read.
    pub fn history(
        &self,
        stream: StreamId,
        from: Version,
        max: NonZeroUsize,
    ) -> Result<Slice, Error> {
        crate::store::history_in(self.tx, stream, from, max)
    }

    /// The projection this consumer holds under `key`, or `None`.
    ///
    /// # Errors
    ///
    /// Returns an [`Error`] when the row cannot be read.
    pub fn projection(
        &self,
        consumer: ConsumerId,
        key: &str,
    ) -> Result<Option<Payload>, Error> {
        crate::store::projection_in(self.tx, consumer, key)
    }

    /// Stores a projection that names the commit that produced it.
    ///
    /// # Errors
    ///
    /// Returns an [`Error`] when the row cannot be written.
    pub fn put_projection(
        &self,
        consumer: ConsumerId,
        key: &str,
        payload: &Payload,
        position: CommitPosition,
    ) -> Result<(), Error> {
        crate::store::put_projection_in(self.tx, consumer, key, payload, position)
    }
}

#[cfg(test)]
mod tests {
    use super::WriteTx;
    use crate::append::{Append, Committed, OutboxMessage, Outcome};
    use crate::error::Error;
    use crate::event::{Kind, NewEvent};
    use crate::model::{CommandId, ConsumerId, Expected, StreamId, Version};
    use crate::payload::Payload;
    use crate::store::{Processed, Store};
    use std::num::NonZeroUsize;
    use std::path::Path;

    fn store() -> Store {
        Store::open(Path::new(":memory:")).unwrap()
    }

    fn max(count: usize) -> NonZeroUsize {
        NonZeroUsize::new(count).unwrap()
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

    fn committed(outcome: Outcome) -> Committed {
        match outcome {
            Outcome::Committed(committed) => committed,
            other => panic!("expected a commit, got {other:?}"),
        }
    }

    #[test]
    fn a_write_tx_appends_and_reads_back_inside_the_consume_transaction() {
        let mut store = store();
        let stream = StreamId::new();
        let outer = committed(
            store
                .append(&append(CommandId::new(), stream, Expected::New, 1))
                .unwrap(),
        );
        let consumer = ConsumerId::new();
        let command = CommandId::new();

        let outcome = store
            .consume(consumer, &outer, |tx: &WriteTx<'_>| {
                assert!(tx.recorded(command)?.is_none());
                let outcome =
                    tx.append(&append(command, stream, Expected::At(Version::new(1)), 2))?;
                assert!(tx.recorded(command)?.is_some());
                assert_eq!(
                    tx.history(stream, Version::INITIAL, max(8))?
                        .events()
                        .count(),
                    3
                );
                Ok(outcome)
            })
            .unwrap();

        let Processed::Applied(Outcome::Committed(inner)) = outcome else {
            panic!("the inner append did not commit");
        };
        assert_eq!(inner.version(), Version::new(3));
        assert_eq!(
            store
                .history(stream, Version::INITIAL, max(8))
                .unwrap()
                .events()
                .count(),
            3
        );
    }

    #[test]
    fn a_write_tx_reads_a_projection_it_wrote() {
        let mut store = store();
        let stream = StreamId::new();
        let outer = committed(
            store
                .append(&append(CommandId::new(), stream, Expected::New, 1))
                .unwrap(),
        );
        let consumer = ConsumerId::new();
        let event = &outer.events()[0];

        let seen = store
            .consume(consumer, &outer, |tx: &WriteTx<'_>| {
                assert!(tx.projection(consumer, "turn")?.is_none());
                tx.put_projection(consumer, "turn", event.payload(), event.position())?;
                tx.projection(consumer, "turn")
            })
            .unwrap();

        let Processed::Applied(Some(projection)) = seen else {
            panic!("the projection just written is missing");
        };
        assert_eq!(projection.as_str(), r#"{"n":0}"#);
    }

    #[test]
    fn a_write_tx_append_that_fails_leaves_the_outer_transaction_unchanged() {
        let mut store = store();
        let stream = StreamId::new();
        let outer = committed(
            store
                .append(&append(CommandId::new(), stream, Expected::New, 1))
                .unwrap(),
        );
        let consumer = ConsumerId::new();
        let failing = append(CommandId::new(), stream, Expected::At(Version::new(9)), 1);

        let failed = store.consume(consumer, &outer, |tx: &WriteTx<'_>| {
            tx.append(&failing)?;
            Err::<(), Error>(Error::Corrupt { what: "deliberate" })
        });

        assert!(failed.is_err());
        assert_eq!(store.checkpoint(consumer).unwrap(), None);
        assert_eq!(
            store
                .history(stream, Version::INITIAL, max(8))
                .unwrap()
                .events()
                .count(),
            1
        );
    }

    #[test]
    fn a_repeated_inner_command_replays_without_a_second_commit() {
        let mut store = store();
        let stream = StreamId::new();
        let outer = committed(
            store
                .append(&append(CommandId::new(), stream, Expected::New, 1))
                .unwrap(),
        );
        let consumer = ConsumerId::new();
        let command = CommandId::new();
        let message = OutboxMessage::new("turn.done", Payload::json(&1).unwrap());

        let Processed::Applied((first, second)) = store
            .consume(consumer, &outer, |tx: &WriteTx<'_>| {
                let first = tx.append(
                    &append(command, stream, Expected::At(Version::new(1)), 1).outbox(message),
                )?;
                let second =
                    tx.append(&append(command, stream, Expected::At(Version::new(1)), 3))?;
                Ok((first, second))
            })
            .unwrap()
        else {
            panic!("the consume did not apply");
        };

        let Outcome::Committed(first) = first else {
            panic!("the first inner append did not commit");
        };
        let Outcome::Replayed(second) = second else {
            panic!("the second inner append did not replay");
        };
        assert_eq!(second.position(), first.position());
        assert_eq!(
            store
                .history(stream, Version::INITIAL, max(8))
                .unwrap()
                .events()
                .count(),
            2
        );
        assert_eq!(store.pending_outbox(max(8)).unwrap().len(), 1);
    }
}

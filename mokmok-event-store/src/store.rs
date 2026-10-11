use core::num::{NonZeroU64, NonZeroUsize};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use mokmok_protocol::EventID;
use rusqlite::{Connection, OptionalExtension, Row, Transaction, TransactionBehavior, params};
use uuid::Uuid;
use uuid::timestamp::{Timestamp, context::NoContext};

use crate::append::{Append, Committed, OutboxEntry, OutboxId, Outcome};
use crate::error::Error;
use crate::event::{Event, Kind, Slice};
use crate::model::{CommandId, CommitPosition, ConsumerId, StreamId, Version};
use crate::payload::Payload;
use crate::write::WriteTx;

const SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS commit_log (
        position  INTEGER PRIMARY KEY,
        stream    BLOB    NOT NULL,
        version   INTEGER NOT NULL,
        count     INTEGER NOT NULL,
        command   BLOB    NOT NULL UNIQUE,
        at_millis INTEGER NOT NULL
    );
    CREATE TABLE IF NOT EXISTS event (
        position INTEGER NOT NULL REFERENCES commit_log(position),
        ordinal  INTEGER NOT NULL,
        stream   BLOB    NOT NULL,
        version  INTEGER NOT NULL,
        id       BLOB    NOT NULL UNIQUE,
        kind     TEXT    NOT NULL,
        payload  TEXT    NOT NULL,
        PRIMARY KEY (position, ordinal),
        UNIQUE (stream, version)
    ) WITHOUT ROWID;
    CREATE TABLE IF NOT EXISTS stream (
        id      BLOB PRIMARY KEY,
        version INTEGER NOT NULL,
        state   TEXT
    ) WITHOUT ROWID;
    CREATE TABLE IF NOT EXISTS projection (
        consumer BLOB    NOT NULL,
        key      TEXT    NOT NULL,
        payload  TEXT    NOT NULL,
        position INTEGER NOT NULL REFERENCES commit_log(position),
        PRIMARY KEY (consumer, key)
    ) WITHOUT ROWID;
    CREATE TABLE IF NOT EXISTS outbox (
        -- `settle_outbox` deletes a prefix, so a plain rowid would reuse an id once it empties.
        id        INTEGER PRIMARY KEY AUTOINCREMENT,
        position  INTEGER NOT NULL REFERENCES commit_log(position),
        topic     TEXT    NOT NULL,
        payload   TEXT    NOT NULL,
        at_millis INTEGER NOT NULL
    );
    CREATE TABLE IF NOT EXISTS inbox (
        consumer  BLOB    NOT NULL,
        position  INTEGER NOT NULL REFERENCES commit_log(position),
        at_millis INTEGER NOT NULL,
        PRIMARY KEY (consumer, position)
    ) WITHOUT ROWID;
    CREATE TABLE IF NOT EXISTS checkpoint (
        consumer BLOB PRIMARY KEY,
        position INTEGER NOT NULL REFERENCES commit_log(position)
    ) WITHOUT ROWID;
";

const EVENT_COLUMNS: &str = "e.position, e.stream, e.version, e.id, e.kind, e.payload, c.at_millis";

/// The result of delivering one commit to a consumer.
#[derive(Debug)]
pub enum Processed<T> {
    Applied(T),
    /// This consumer had already processed this commit; the closure did not run.
    Duplicate,
}

/// One SQLite file holding the global commit log and the cursors over it.
pub struct Store {
    connection: Connection,
}

impl Store {
    /// Opens or creates the store at `path`, applying the schema.
    ///
    /// A file that is not a store yet is adopted when its `user_version` is 0: the tables are added
    /// beside whatever is already in it. Any other version is refused.
    ///
    /// # Errors
    ///
    /// Returns an [`Error`] when the file cannot be created, opened, or read.
    pub fn open(path: &Path) -> Result<Self, Error> {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent).map_err(Error::Io)?;
        }

        let connection = Connection::open(path).map_err(Error::Open)?;
        prepare(&connection)?;

        Ok(Self { connection })
    }

    /// Runs the append program in one `BEGIN IMMEDIATE` transaction.
    ///
    /// A caller that retries after a [`Outcome::Conflicted`] can keep losing to a concurrent
    /// writer, because each attempt compares against a version the last attempt read. Pass
    /// [`Expected::Any`](crate::Expected::Any) when the append does not depend on the version that
    /// was read.
    ///
    /// # Errors
    ///
    /// Returns an [`Error`] when the command cannot be committed. Another process that holds the
    /// write lock past the five-second wait yields [`Error::Query`] over `SQLITE_BUSY`; nothing was
    /// written, so the caller may retry.
    pub fn append(
        &mut self,
        append: &Append,
    ) -> Result<Outcome, Error> {
        let transaction = self.begin()?;
        let outcome = append_in(&transaction, append)?;
        transaction.commit().map_err(Error::Query)?;

        Ok(outcome)
    }

    /// One transaction: inbox dedup, the caller's writes, the inbox row, and the checkpoint.
    ///
    /// `commit` must come from this store's log. A commit from another file whose position happens
    /// to exist here is accepted, and the inbox and checkpoint then name an unrelated commit.
    ///
    /// # Errors
    ///
    /// Returns an [`Error`] when the caller's write or the bookkeeping cannot be committed.
    /// Another process that holds the write lock past the five-second wait yields [`Error::Query`]
    /// over `SQLITE_BUSY`; nothing was written, so the caller may retry.
    pub fn consume<T>(
        &mut self,
        consumer: ConsumerId,
        commit: &Committed,
        apply: impl FnOnce(&WriteTx<'_>) -> Result<T, Error>,
    ) -> Result<Processed<T>, Error> {
        let transaction = self.begin()?;
        if inbox_has(&transaction, consumer, commit.position())? {
            return Ok(Processed::Duplicate);
        }

        let value = apply(&WriteTx::new(&transaction))?;
        insert_inbox(&transaction, consumer, commit.position())?;
        advance_checkpoint(&transaction, consumer, commit.position())?;
        transaction.commit().map_err(Error::Query)?;

        Ok(Processed::Applied(value))
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
        history_in(&self.connection, stream, from, max)
    }

    /// Commits of the whole log at or after `from`, at most `max` commits, in position order.
    ///
    /// # Errors
    ///
    /// Returns an [`Error`] when the rows cannot be read.
    pub fn log(
        &self,
        from: CommitPosition,
        max: NonZeroUsize,
    ) -> Result<Vec<Committed>, Error> {
        log_in(&self.connection, from, max)
    }

    /// The stored checkpoint, or `None` when this consumer has never consumed.
    ///
    /// # Errors
    ///
    /// Returns an [`Error`] when the row cannot be read.
    pub fn checkpoint(
        &self,
        consumer: ConsumerId,
    ) -> Result<Option<CommitPosition>, Error> {
        let position = self
            .connection
            .query_row(
                "SELECT position FROM checkpoint WHERE consumer = ?1",
                params![consumer.as_uuid().as_bytes().as_slice()],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(Error::Query)?;

        position.map(position_from).transpose()
    }

    /// At most `max` outbox rows, in the order they were enqueued.
    ///
    /// # Errors
    ///
    /// Returns an [`Error`] when the rows cannot be read.
    pub fn pending_outbox(
        &self,
        max: NonZeroUsize,
    ) -> Result<Vec<OutboxEntry>, Error> {
        let limit = i64::try_from(max.get()).map_err(|_| Error::Corrupt {
            what: "the outbox limit",
        })?;
        let mut statement = self
            .connection
            .prepare("SELECT id, topic, payload, at_millis FROM outbox ORDER BY id LIMIT ?1")
            .map_err(Error::Query)?;
        let rows = statement
            .query_map(params![limit], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            })
            .map_err(Error::Query)?;

        let mut entries = Vec::new();
        for row in rows {
            let (id, topic, payload, at_millis) = row.map_err(Error::Query)?;
            entries.push(OutboxEntry::new(
                outbox_id_from(id)?,
                topic,
                Payload::from_stored(payload).map_err(|_| Error::Corrupt {
                    what: "an outbox payload",
                })?,
                time_from_millis(at_millis)?,
            ));
        }

        Ok(entries)
    }

    /// Deletes every outbox row whose id is at or below `through`, returning the count removed.
    ///
    /// # Errors
    ///
    /// Returns an [`Error`] when the rows cannot be deleted.
    pub fn settle_outbox(
        &mut self,
        through: OutboxId,
    ) -> Result<u64, Error> {
        let transaction = self.begin()?;
        let removed = transaction
            .execute(
                "DELETE FROM outbox WHERE id <= ?1",
                params![to_i64(through.get())?],
            )
            .map_err(Error::Query)?;
        transaction.commit().map_err(Error::Query)?;

        u64::try_from(removed).map_err(|_| Error::Corrupt {
            what: "the settled outbox count",
        })
    }

    fn begin(&mut self) -> Result<Transaction<'_>, Error> {
        self.connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(Error::Query)
    }
}

fn prepare(connection: &Connection) -> Result<(), Error> {
    // Set before anything that can take a lock, because another process may hold one: a connection
    // without a busy handler answers `SQLITE_BUSY` on the spot rather than waiting for the other
    // process to commit.
    connection
        .execute_batch("PRAGMA busy_timeout = 5000;")
        .map_err(Error::Open)?;

    let found: i64 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(Error::Query)?;
    if found != 0 && found != 1 {
        return Err(Error::Schema { found });
    }

    // SQLite does not run the busy handler for the switch to WAL, so a process that loses the race
    // to create the file is told `SQLITE_BUSY` at once, and the winner settles the mode for every
    // later open. A failed switch is harmless: `synchronous = FULL` is what makes a commit
    // durable, and it holds in either journal mode.
    let _ = connection.execute_batch("PRAGMA journal_mode = WAL;");

    connection
        .execute_batch("PRAGMA synchronous = FULL; PRAGMA foreign_keys = ON;")
        .map_err(Error::Open)?;

    connection.execute_batch(SCHEMA).map_err(Error::Open)?;
    if found == 0 {
        connection
            .execute_batch("PRAGMA user_version = 1;")
            .map_err(Error::Query)?;
    }

    Ok(())
}

/// Runs the append program inside a savepoint, so an error cannot leave a half-written append in
/// whatever transaction the caller owns.
pub fn append_in(
    connection: &Connection,
    append: &Append,
) -> Result<Outcome, Error> {
    connection
        .execute_batch("SAVEPOINT append;")
        .map_err(Error::Query)?;

    match append_program(connection, append) {
        Ok(outcome) => {
            connection
                .execute_batch("RELEASE append;")
                .map_err(Error::Query)?;
            Ok(outcome)
        },
        Err(error) => {
            // A caller that drops this error still commits, so the transaction must not carry a
            // half-written append out of here.
            let _ = connection.execute_batch("ROLLBACK TO append; RELEASE append;");
            Err(error)
        },
    }
}

/// The append program, which only makes sense against a transaction the caller still owns.
fn append_program(
    connection: &Connection,
    append: &Append,
) -> Result<Outcome, Error> {
    if let Some(committed) = recorded_in(connection, append.command)? {
        return Ok(Outcome::Replayed(committed));
    }

    let found = stream_version(connection, append.stream)?;
    if !append.expected.accepts(found) {
        return Ok(Outcome::Conflicted {
            expected: append.expected,
            found,
        });
    }

    let count = u64::try_from(append.rest.len())
        .map_err(|_| Error::Corrupt {
            what: "the commit size",
        })?
        .saturating_add(1);
    let version = found
        .get()
        .checked_add(count)
        .map(Version::new)
        .ok_or(Error::Corrupt {
            what: "the next stream version",
        })?;
    let at = SystemTime::now();
    connection
        .execute(
            "INSERT INTO commit_log (stream, version, count, command, at_millis)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                append.stream.as_uuid().as_bytes().as_slice(),
                to_i64(version.get())?,
                to_i64(count)?,
                append.command.as_uuid().as_bytes().as_slice(),
                to_millis(at),
            ],
        )
        .map_err(Error::Query)?;
    let position = position_from(connection.last_insert_rowid())?;

    insert_events(connection, append, position, found)?;
    upsert_stream(connection, append, version)?;
    insert_outbox(connection, append, position, at)?;

    committed_at(connection, position)?
        .ok_or(Error::Corrupt {
            what: "the appended commit",
        })
        .map(Outcome::Committed)
}

fn insert_events(
    connection: &Connection,
    append: &Append,
    position: CommitPosition,
    found: Version,
) -> Result<(), Error> {
    for (index, event) in core::iter::once(&append.first)
        .chain(&append.rest)
        .enumerate()
    {
        let ordinal = u64::try_from(index).map_err(|_| Error::Corrupt {
            what: "an event ordinal",
        })?;
        let version = found
            .get()
            .checked_add(ordinal)
            .map(Version::new)
            .ok_or(Error::Corrupt {
                what: "an event version",
            })?;
        let id = EventID::new(Timestamp::now(NoContext));
        connection
            .execute(
                "INSERT INTO event (position, ordinal, stream, version, id, kind, payload)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    to_i64(position.get())?,
                    to_i64(ordinal)?,
                    append.stream.as_uuid().as_bytes().as_slice(),
                    to_i64(version.get())?,
                    id.as_uuid().as_bytes().as_slice(),
                    event.kind.as_str(),
                    event.payload.as_str(),
                ],
            )
            .map_err(Error::Query)?;
    }

    Ok(())
}

fn upsert_stream(
    connection: &Connection,
    append: &Append,
    version: Version,
) -> Result<(), Error> {
    connection
        .execute(
            "INSERT INTO stream (id, version, state) VALUES (?1, ?2, ?3)
             ON CONFLICT(id) DO UPDATE SET version = excluded.version,
                                         state = COALESCE(excluded.state, stream.state)",
            params![
                append.stream.as_uuid().as_bytes().as_slice(),
                to_i64(version.get())?,
                append.state.as_ref().map(Payload::as_str),
            ],
        )
        .map_err(Error::Query)?;

    Ok(())
}

fn insert_outbox(
    connection: &Connection,
    append: &Append,
    position: CommitPosition,
    at: SystemTime,
) -> Result<(), Error> {
    for message in &append.outbox {
        connection
            .execute(
                "INSERT INTO outbox (position, topic, payload, at_millis)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    to_i64(position.get())?,
                    message.topic.as_str(),
                    message.payload.as_str(),
                    to_millis(at),
                ],
            )
            .map_err(Error::Query)?;
    }

    Ok(())
}

pub fn recorded_in(
    connection: &Connection,
    command: CommandId,
) -> Result<Option<Committed>, Error> {
    let position = connection
        .query_row(
            "SELECT position FROM commit_log WHERE command = ?1",
            params![command.as_uuid().as_bytes().as_slice()],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(Error::Query)?;

    match position {
        Some(position) => committed_at(connection, position_from(position)?),
        None => Ok(None),
    }
}

fn committed_at(
    connection: &Connection,
    position: CommitPosition,
) -> Result<Option<Committed>, Error> {
    let version = connection
        .query_row(
            "SELECT version FROM commit_log WHERE position = ?1",
            params![to_i64(position.get())?],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(Error::Query)?;
    let Some(version) = version else {
        return Ok(None);
    };

    let events = events_at(connection, position)?;

    Ok(Some(Committed::new(
        position,
        version_from(version)?,
        events,
    )))
}

fn stream_version(
    connection: &Connection,
    stream: StreamId,
) -> Result<Version, Error> {
    let version = connection
        .query_row(
            "SELECT version FROM stream WHERE id = ?1",
            params![stream.as_uuid().as_bytes().as_slice()],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(Error::Query)?;

    version.map_or_else(|| Ok(Version::INITIAL), version_from)
}

pub fn history_in(
    connection: &Connection,
    stream: StreamId,
    from: Version,
    max: NonZeroUsize,
) -> Result<Slice, Error> {
    let row = connection
        .query_row(
            "SELECT version, state FROM stream WHERE id = ?1",
            params![stream.as_uuid().as_bytes().as_slice()],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?)),
        )
        .optional()
        .map_err(Error::Query)?;

    let (version, state) = match row {
        Some((version, state)) => (
            version_from(version)?,
            state
                .map(Payload::from_stored)
                .transpose()
                .map_err(|_| Error::Corrupt {
                    what: "a stream snapshot",
                })?,
        ),
        None => (Version::INITIAL, None),
    };

    let mut commits = Vec::new();
    for row in commit_rows(
        connection,
        "SELECT position, version FROM commit_log
         WHERE stream = ?1 AND version > ?2 ORDER BY position LIMIT ?3",
        params![
            stream.as_uuid().as_bytes().as_slice(),
            to_i64(from.get())?,
            limit_to_i64(max)?,
        ],
    )? {
        commits.push(row_commit(connection, row)?);
    }

    Ok(Slice::new(version, state, commits))
}

fn log_in(
    connection: &Connection,
    from: CommitPosition,
    max: NonZeroUsize,
) -> Result<Vec<Committed>, Error> {
    let mut commits = Vec::new();
    for row in commit_rows(
        connection,
        "SELECT position, version FROM commit_log
         WHERE position >= ?1 ORDER BY position LIMIT ?2",
        params![to_i64(from.get())?, limit_to_i64(max)?],
    )? {
        commits.push(row_commit(connection, row)?);
    }

    Ok(commits)
}

fn commit_rows(
    connection: &Connection,
    sql: &str,
    parameters: impl rusqlite::Params,
) -> Result<Vec<(i64, i64)>, Error> {
    let mut statement = connection.prepare(sql).map_err(Error::Query)?;
    let rows = statement
        .query_map(parameters, |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
        })
        .map_err(Error::Query)?;

    let mut found = Vec::new();
    for row in rows {
        found.push(row.map_err(Error::Query)?);
    }

    Ok(found)
}

fn row_commit(
    connection: &Connection,
    row: (i64, i64),
) -> Result<Committed, Error> {
    let (position, version) = row;
    let position = position_from(position)?;

    Ok(Committed::new(
        position,
        version_from(version)?,
        events_at(connection, position)?,
    ))
}

fn events_at(
    connection: &Connection,
    position: CommitPosition,
) -> Result<Vec<Event>, Error> {
    let sql = format!(
        "SELECT {EVENT_COLUMNS} FROM event e JOIN commit_log c ON c.position = e.position
         WHERE e.position = ?1 ORDER BY e.ordinal"
    );
    let mut statement = connection.prepare(&sql).map_err(Error::Query)?;
    let rows = statement
        .query_map(params![to_i64(position.get())?], StoredEvent::from_row)
        .map_err(Error::Query)?;

    let mut events = Vec::new();
    for row in rows {
        events.push(row.map_err(Error::Query)?.into_event()?);
    }

    Ok(events)
}

fn inbox_has(
    connection: &Connection,
    consumer: ConsumerId,
    position: CommitPosition,
) -> Result<bool, Error> {
    let found = connection
        .query_row(
            "SELECT 1 FROM inbox WHERE consumer = ?1 AND position = ?2",
            params![
                consumer.as_uuid().as_bytes().as_slice(),
                to_i64(position.get())?,
            ],
            |_| Ok(()),
        )
        .optional()
        .map_err(Error::Query)?;

    Ok(found.is_some())
}

fn insert_inbox(
    connection: &Connection,
    consumer: ConsumerId,
    position: CommitPosition,
) -> Result<(), Error> {
    connection
        .execute(
            "INSERT INTO inbox (consumer, position, at_millis) VALUES (?1, ?2, ?3)",
            params![
                consumer.as_uuid().as_bytes().as_slice(),
                to_i64(position.get())?,
                to_millis(SystemTime::now()),
            ],
        )
        .map_err(Error::Query)?;

    Ok(())
}

/// Moves the checkpoint with SQLite's two-argument `max`, so a late delivery never rewinds it.
fn advance_checkpoint(
    connection: &Connection,
    consumer: ConsumerId,
    position: CommitPosition,
) -> Result<(), Error> {
    connection
        .execute(
            "INSERT INTO checkpoint (consumer, position) VALUES (?1, ?2)
             ON CONFLICT(consumer) DO UPDATE SET position = max(position, excluded.position)",
            params![
                consumer.as_uuid().as_bytes().as_slice(),
                to_i64(position.get())?,
            ],
        )
        .map_err(Error::Query)?;

    Ok(())
}

pub fn projection_in(
    connection: &Connection,
    consumer: ConsumerId,
    key: &str,
) -> Result<Option<Payload>, Error> {
    let payload = connection
        .query_row(
            "SELECT payload FROM projection WHERE consumer = ?1 AND key = ?2",
            params![consumer.as_uuid().as_bytes().as_slice(), key],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(Error::Query)?;

    payload
        .map(Payload::from_stored)
        .transpose()
        .map_err(|_| Error::Corrupt {
            what: "a projection payload",
        })
}

pub fn put_projection_in(
    connection: &Connection,
    consumer: ConsumerId,
    key: &str,
    payload: &Payload,
    position: CommitPosition,
) -> Result<(), Error> {
    connection
        .execute(
            "INSERT INTO projection (consumer, key, payload, position) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(consumer, key) DO UPDATE SET payload = excluded.payload,
                                                      position = excluded.position",
            params![
                consumer.as_uuid().as_bytes().as_slice(),
                key,
                payload.as_str(),
                to_i64(position.get())?,
            ],
        )
        .map_err(Error::Query)?;

    Ok(())
}

struct StoredEvent {
    position: i64,
    stream: Vec<u8>,
    version: i64,
    id: Vec<u8>,
    kind: String,
    payload: String,
    at_millis: i64,
}

impl StoredEvent {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            position: row.get(0)?,
            stream: row.get(1)?,
            version: row.get(2)?,
            id: row.get(3)?,
            kind: row.get(4)?,
            payload: row.get(5)?,
            at_millis: row.get(6)?,
        })
    }

    fn into_event(self) -> Result<Event, Error> {
        Ok(Event::new(
            EventID::from_uuid(Uuid::from_slice(&self.id).map_err(|_| Error::Corrupt {
                what: "an event id",
            })?),
            StreamId::from_uuid(Uuid::from_slice(&self.stream).map_err(|_| Error::Corrupt {
                what: "an event stream",
            })?),
            version_from(self.version)?,
            position_from(self.position)?,
            Kind::new(self.kind).map_err(|_| Error::Corrupt {
                what: "an event kind",
            })?,
            Payload::from_stored(self.payload).map_err(|_| Error::Corrupt {
                what: "an event payload",
            })?,
            time_from_millis(self.at_millis)?,
        ))
    }
}

fn version_from(value: i64) -> Result<Version, Error> {
    u64::try_from(value)
        .map(Version::new)
        .map_err(|_| Error::Corrupt {
            what: "a stream version",
        })
}

fn position_from(value: i64) -> Result<CommitPosition, Error> {
    u64::try_from(value)
        .ok()
        .and_then(NonZeroU64::new)
        .map(CommitPosition::from_nonzero)
        .ok_or(Error::Corrupt {
            what: "a commit position",
        })
}

fn outbox_id_from(value: i64) -> Result<OutboxId, Error> {
    u64::try_from(value)
        .ok()
        .and_then(NonZeroU64::new)
        .map(OutboxId::from_nonzero)
        .ok_or(Error::Corrupt {
            what: "an outbox id",
        })
}

fn to_i64(value: u64) -> Result<i64, Error> {
    i64::try_from(value).map_err(|_| Error::Corrupt {
        what: "a value outside SQLite's integer range",
    })
}

fn limit_to_i64(max: NonZeroUsize) -> Result<i64, Error> {
    i64::try_from(max.get()).map_err(|_| Error::Corrupt {
        what: "the read limit",
    })
}

fn to_millis(at: SystemTime) -> i64 {
    let since = at.duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO);

    i64::try_from(since.as_millis()).unwrap_or(i64::MAX)
}

fn time_from_millis(value: i64) -> Result<SystemTime, Error> {
    let millis = u64::try_from(value).map_err(|_| Error::Corrupt {
        what: "a commit time",
    })?;

    UNIX_EPOCH
        .checked_add(Duration::from_millis(millis))
        .ok_or(Error::Corrupt {
            what: "a commit time",
        })
}

#[cfg(test)]
mod tests {
    use super::{Processed, Store};
    use crate::append::{Append, Committed, OutboxEntry, OutboxMessage};
    use crate::error::Error;
    use crate::event::{Event, Kind, NewEvent};
    use crate::model::{CommandId, CommitPosition, ConsumerId, Expected, StreamId, Version};
    use crate::payload::Payload;
    use std::cell::Cell;
    use std::num::NonZeroUsize;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, SystemTime};

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

    fn committed(
        store: &mut Store,
        append: &Append,
    ) -> Committed {
        match store.append(append).unwrap() {
            super::Outcome::Committed(committed) => committed,
            other => panic!("expected a commit, got {other:?}"),
        }
    }

    fn scratch() -> PathBuf {
        static NEXT: AtomicUsize = AtomicUsize::new(0);

        std::env::temp_dir().join(format!(
            "mokmok-event-store-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ))
    }

    #[test]
    fn a_first_append_commits_its_events_and_snapshot() {
        let mut store = store();
        let stream = StreamId::new();
        let first = append(CommandId::new(), stream, Expected::New, 2)
            .state(Payload::json(&serde_json::json!({"open": true})).unwrap());

        let commit = committed(&mut store, &first);

        assert_eq!(commit.position().get(), 1);
        assert_eq!(commit.version(), Version::new(2));
        assert_eq!(
            commit
                .events()
                .iter()
                .map(|event| event.version().get())
                .collect::<Vec<_>>(),
            [0, 1]
        );
        assert_eq!(
            commit
                .events()
                .iter()
                .map(|event| event.position().get())
                .collect::<Vec<_>>(),
            [1, 1]
        );

        let slice = store.history(stream, Version::INITIAL, max(8)).unwrap();
        assert_eq!(slice.version(), Version::new(2));
        assert_eq!(slice.commits().len(), 1);
        let events = slice.events().collect::<Vec<_>>();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].kind().to_string(), "turn.0");
        assert_eq!(events[1].kind().to_string(), "turn.1");
        assert_eq!(slice.state().unwrap().as_str(), r#"{"open":true}"#);
    }

    #[test]
    fn a_second_append_extends_the_stream_to_the_total() {
        let mut store = store();
        let stream = StreamId::new();
        committed(
            &mut store,
            &append(CommandId::new(), stream, Expected::New, 1),
        );

        let loaded = store.history(stream, Version::INITIAL, max(8)).unwrap();
        let commit = committed(
            &mut store,
            &append(CommandId::new(), stream, Expected::At(loaded.version()), 2),
        );

        assert_eq!(commit.version(), Version::new(3));
        let slice = store.history(stream, Version::INITIAL, max(8)).unwrap();
        assert_eq!(slice.version(), Version::new(3));
        assert_eq!(
            slice
                .events()
                .map(|event| event.version().get())
                .collect::<Vec<_>>(),
            [0, 1, 2]
        );
    }

    #[test]
    fn a_stale_expectation_conflicts_and_writes_nothing() {
        let mut store = store();
        let stream = StreamId::new();
        let first = append(CommandId::new(), stream, Expected::New, 2)
            .state(Payload::json(&1).unwrap())
            .outbox(OutboxMessage::new("turn.done", Payload::json(&0).unwrap()));
        committed(&mut store, &first);

        let stale = append(CommandId::new(), stream, Expected::At(Version::INITIAL), 1);
        let outcome = store.append(&stale).unwrap();
        let super::Outcome::Conflicted { expected, found } = outcome else {
            panic!("expected a conflict, got {outcome:?}");
        };

        assert_eq!(expected, Expected::At(Version::INITIAL));
        assert_eq!(found, Version::new(2));
        let slice = store.history(stream, Version::INITIAL, max(8)).unwrap();
        assert_eq!(slice.version(), Version::new(2));
        assert_eq!(slice.events().count(), 2);
        assert_eq!(slice.state().unwrap().as_str(), "1");
        assert_eq!(store.pending_outbox(max(8)).unwrap().len(), 1);
    }

    #[test]
    fn a_repeated_command_replays_without_growing_the_stream() {
        let mut store = store();
        let stream = StreamId::new();
        let command = CommandId::new();
        let first = committed(&mut store, &append(command, stream, Expected::New, 2));

        let outcome = store
            .append(&append(command, stream, Expected::New, 3))
            .unwrap();
        let super::Outcome::Replayed(replayed) = outcome else {
            panic!("expected a replay, got {outcome:?}");
        };

        assert_eq!(replayed.position(), first.position());
        assert_eq!(replayed.version(), Version::new(2));
        assert_eq!(
            replayed.events().iter().map(Event::id).collect::<Vec<_>>(),
            first.events().iter().map(Event::id).collect::<Vec<_>>()
        );
        let slice = store.history(stream, Version::INITIAL, max(8)).unwrap();
        assert_eq!(slice.version(), Version::new(2));
        assert_eq!(slice.events().count(), 2);
    }

    #[test]
    fn a_multi_event_commit_is_delivered_once_and_a_redelivery_is_a_duplicate() {
        let mut store = store();
        let stream = StreamId::new();
        let commit = committed(
            &mut store,
            &append(CommandId::new(), stream, Expected::New, 3),
        );
        assert_eq!(commit.events().len(), 3);
        let consumer = ConsumerId::new();
        let calls = Cell::new(0_usize);

        let first = store
            .consume(consumer, &commit, |tx| {
                calls.set(calls.get() + 1);
                for event in commit.events() {
                    tx.put_projection(
                        consumer,
                        &event.kind().to_string(),
                        event.payload(),
                        commit.position(),
                    )?;
                }
                Ok(())
            })
            .unwrap();
        let second = store
            .consume(consumer, &commit, |_| {
                calls.set(calls.get() + 1);
                Ok(())
            })
            .unwrap();

        assert!(matches!(first, Processed::Applied(())));
        assert!(matches!(second, Processed::Duplicate));
        assert_eq!(calls.get(), 1);
        assert_eq!(store.checkpoint(consumer).unwrap(), Some(commit.position()));
        let inbox: i64 = store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM inbox WHERE consumer = ?1",
                rusqlite::params![consumer.as_uuid().as_bytes().as_slice()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(inbox, 1);
    }

    #[test]
    fn history_returns_whole_commits_and_resumes_from_a_commit_version() {
        let mut store = store();
        let stream = StreamId::new();
        committed(
            &mut store,
            &append(CommandId::new(), stream, Expected::New, 1),
        );
        committed(
            &mut store,
            &append(CommandId::new(), stream, Expected::At(Version::new(1)), 2),
        );
        committed(
            &mut store,
            &append(CommandId::new(), stream, Expected::At(Version::new(3)), 3),
        );

        let slice = store.history(stream, Version::INITIAL, max(8)).unwrap();
        assert_eq!(slice.commits().len(), 3);
        assert_eq!(slice.version(), Version::new(6));
        assert_eq!(slice.events().count(), 6);

        let capped = store.history(stream, Version::INITIAL, max(2)).unwrap();
        assert_eq!(capped.commits().len(), 2);
        assert_eq!(capped.events().count(), 3);
        assert_eq!(
            capped
                .commits()
                .iter()
                .map(|commit| commit.events().len())
                .collect::<Vec<_>>(),
            [1, 2]
        );

        let resumed = store
            .history(stream, capped.commits()[1].version(), max(2))
            .unwrap();
        assert_eq!(resumed.commits().len(), 1);
        assert_eq!(resumed.commits()[0].events().len(), 3);
        assert_eq!(resumed.events().count(), 3);

        let mid = store.history(stream, Version::new(1), max(8)).unwrap();
        assert_eq!(mid.commits().len(), 2);
        assert_eq!(mid.commits()[0].events().len(), 2);
    }

    #[test]
    fn history_returns_a_commit_that_straddles_from_whole() {
        let mut store = store();
        let stream = StreamId::new();
        committed(
            &mut store,
            &append(CommandId::new(), stream, Expected::New, 1),
        );
        committed(
            &mut store,
            &append(CommandId::new(), stream, Expected::At(Version::new(1)), 2),
        );
        committed(
            &mut store,
            &append(CommandId::new(), stream, Expected::At(Version::new(3)), 3),
        );

        let straddled = store.history(stream, Version::new(2), max(8)).unwrap();

        assert_eq!(straddled.commits().len(), 2);
        assert_eq!(
            straddled.commits()[0]
                .events()
                .iter()
                .map(|event| event.version().get())
                .collect::<Vec<_>>(),
            [1, 2],
            "the straddled commit comes back whole",
        );
        assert_eq!(straddled.events().count(), 5);
    }

    #[test]
    fn the_log_reads_commits_in_position_order() {
        let mut store = store();
        let a = StreamId::new();
        let b = StreamId::new();
        committed(&mut store, &append(CommandId::new(), a, Expected::New, 2));
        committed(&mut store, &append(CommandId::new(), b, Expected::New, 1));
        committed(
            &mut store,
            &append(CommandId::new(), a, Expected::At(Version::new(2)), 3),
        );

        let log = store.log(CommitPosition::FIRST, max(8)).unwrap();
        assert_eq!(
            log.iter()
                .map(|commit| commit.position().get())
                .collect::<Vec<_>>(),
            [1, 2, 3]
        );
        assert_eq!(
            log.iter()
                .map(|commit| commit.events().len())
                .collect::<Vec<_>>(),
            [2, 1, 3]
        );

        let tail = store.log(log[1].position(), max(8)).unwrap();
        assert_eq!(
            tail.iter()
                .map(|commit| commit.position().get())
                .collect::<Vec<_>>(),
            [2, 3]
        );

        let capped = store.log(CommitPosition::FIRST, max(1)).unwrap();
        assert_eq!(capped.len(), 1);
        assert_eq!(capped[0].position().get(), 1);
    }

    #[test]
    fn consume_applies_once_and_a_redelivery_is_a_duplicate() {
        let mut store = store();
        let stream = StreamId::new();
        let commit = committed(
            &mut store,
            &append(CommandId::new(), stream, Expected::New, 1),
        );
        let consumer = ConsumerId::new();
        let calls = Cell::new(0_usize);

        let first = store
            .consume(consumer, &commit, |_| {
                calls.set(calls.get() + 1);
                Ok(())
            })
            .unwrap();
        let second = store
            .consume(consumer, &commit, |_| {
                calls.set(calls.get() + 1);
                Ok(())
            })
            .unwrap();

        assert!(matches!(first, Processed::Applied(())));
        assert!(matches!(second, Processed::Duplicate));
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn consume_advances_the_checkpoint_and_keeps_a_projection() {
        let mut store = store();
        let stream = StreamId::new();
        let first_commit = committed(
            &mut store,
            &append(CommandId::new(), stream, Expected::New, 1),
        );
        let second_commit = committed(
            &mut store,
            &append(CommandId::new(), stream, Expected::At(Version::new(1)), 1),
        );
        let consumer = ConsumerId::new();
        let first = &first_commit.events()[0];
        let second = &second_commit.events()[0];
        assert_eq!(first.position().get(), 1);
        assert_eq!(second.position().get(), 2);

        let applied = store
            .consume(consumer, &first_commit, |tx| {
                tx.put_projection(consumer, "turn", first.payload(), first.position())
            })
            .unwrap();
        assert!(matches!(applied, Processed::Applied(())));
        assert_eq!(store.checkpoint(consumer).unwrap(), Some(first.position()));

        let seen = store
            .consume(consumer, &second_commit, |tx| {
                tx.projection(consumer, "turn")
            })
            .unwrap();
        let Processed::Applied(Some(projection)) = seen else {
            panic!("the projection written by the first consume is missing");
        };
        assert_eq!(projection.as_str(), first.payload().as_str());

        let slice = store.history(stream, Version::INITIAL, max(8)).unwrap();
        assert!(
            slice
                .events()
                .any(|event| event.position() == first.position())
        );
        assert_eq!(store.checkpoint(consumer).unwrap(), Some(second.position()));
    }

    #[test]
    fn a_failed_closure_leaves_the_inbox_and_the_checkpoint_untouched() {
        let mut store = store();
        let stream = StreamId::new();
        let commit = committed(
            &mut store,
            &append(CommandId::new(), stream, Expected::New, 1),
        );
        let event = &commit.events()[0];
        let consumer = ConsumerId::new();

        let failed = store.consume(consumer, &commit, |tx| {
            tx.put_projection(consumer, "turn", event.payload(), event.position())?;
            Err::<(), Error>(Error::Corrupt { what: "deliberate" })
        });
        assert!(failed.is_err());

        assert_eq!(store.checkpoint(consumer).unwrap(), None);
        let retried = store
            .consume(consumer, &commit, |tx| tx.projection(consumer, "turn"))
            .unwrap();
        assert!(matches!(retried, Processed::Applied(None)));
        assert_eq!(
            store
                .history(stream, Version::INITIAL, max(8))
                .unwrap()
                .version(),
            Version::new(1)
        );
    }

    #[test]
    fn pending_outbox_lists_in_id_order_and_settle_removes_the_prefix() {
        let mut store = store();
        let stream = StreamId::new();
        let first = append(CommandId::new(), stream, Expected::New, 1)
            .outbox(OutboxMessage::new("a", Payload::json(&1).unwrap()))
            .outbox(OutboxMessage::new("b", Payload::json(&2).unwrap()));
        let second = append(CommandId::new(), stream, Expected::At(Version::new(1)), 1)
            .outbox(OutboxMessage::new("c", Payload::json(&3).unwrap()));
        committed(&mut store, &first);
        committed(&mut store, &second);

        let pending = store.pending_outbox(max(8)).unwrap();
        assert_eq!(
            pending
                .iter()
                .map(|entry| entry.id().get())
                .collect::<Vec<_>>(),
            [1, 2, 3]
        );
        assert_eq!(
            pending.iter().map(OutboxEntry::topic).collect::<Vec<_>>(),
            ["a", "b", "c"]
        );
        assert_eq!(pending[2].payload().as_str(), "3");

        assert_eq!(store.settle_outbox(pending[1].id()).unwrap(), 2);
        let remaining = store.pending_outbox(max(8)).unwrap();
        assert_eq!(
            remaining
                .iter()
                .map(|entry| entry.id().get())
                .collect::<Vec<_>>(),
            [3]
        );
    }

    #[test]
    fn a_checkpoint_never_moves_backwards() {
        let mut store = store();
        let stream = StreamId::new();
        let first = committed(
            &mut store,
            &append(CommandId::new(), stream, Expected::New, 1),
        );
        let second = committed(
            &mut store,
            &append(CommandId::new(), stream, Expected::At(Version::new(1)), 1),
        );
        let consumer = ConsumerId::new();

        store.consume(consumer, &second, |_| Ok(())).unwrap();
        store.consume(consumer, &first, |_| Ok(())).unwrap();

        assert_eq!(store.checkpoint(consumer).unwrap(), Some(second.position()));
    }

    #[test]
    fn an_unknown_schema_version_is_refused() {
        let path = scratch();
        {
            let connection = rusqlite::Connection::open(&path).unwrap();
            connection
                .execute_batch("PRAGMA user_version = 7;")
                .unwrap();
        }

        let Err(error) = Store::open(&path) else {
            panic!("a foreign schema version was accepted");
        };

        assert!(matches!(error, Error::Schema { found: 7 }));
        std::fs::remove_file(&path).unwrap();
    }

    /// The instant a store would keep for `at`, which truncates to whole milliseconds.
    fn floor_to_millis(at: SystemTime) -> SystemTime {
        let millis = at
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_millis();

        SystemTime::UNIX_EPOCH + Duration::from_millis(u64::try_from(millis).unwrap())
    }

    #[test]
    fn an_appended_event_and_its_outbox_carry_the_commit_time() {
        let mut store = store();
        let stream = StreamId::new();
        let append = append(CommandId::new(), stream, Expected::New, 1)
            .outbox(OutboxMessage::new("turn.done", Payload::json(&1).unwrap()));

        let before = SystemTime::now();
        let commit = committed(&mut store, &append);
        let after = SystemTime::now();
        let floor = floor_to_millis(before);

        let mut pending = store.pending_outbox(max(8)).unwrap();
        let entry = pending.remove(0);

        assert!(floor <= commit.events()[0].at());
        assert!(commit.events()[0].at() <= after);
        assert!(floor <= entry.at());
        assert!(entry.at() <= after);
    }

    #[test]
    fn a_fresh_store_stamps_the_file_with_user_version_one() {
        let path = scratch();
        Store::open(&path).unwrap();

        let connection = rusqlite::Connection::open(&path).unwrap();
        let version: i64 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();

        assert_eq!(version, 1);
        std::fs::remove_file(&path).unwrap();
    }
}

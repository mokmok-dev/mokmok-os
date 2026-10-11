use std::num::NonZeroUsize;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use mokmok_protocol::EventID;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;
use uuid::timestamp::{Timestamp, context::NoContext};

use crate::config::Retention;
use crate::event::{Cursor, Event, LogEpoch, Published, Seq, Window};
use crate::payload::Payload;
use crate::subscriber::SubscriberId;
use crate::topic::Topic;

/// How many commands may wait for the owning thread. This is the ceiling on a stalled disk, and
/// that backpressure is intended: a burst of appends queues here rather than in memory.
const CHANNEL: usize = 64;

const SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS topic (
        name        TEXT PRIMARY KEY,
        epoch       BLOB NOT NULL,
        first       INTEGER NOT NULL,
        next        INTEGER NOT NULL,
        bytes       INTEGER NOT NULL,
        last_append INTEGER NOT NULL
    );
    CREATE TABLE IF NOT EXISTS event (
        topic     TEXT    NOT NULL,
        seq       INTEGER NOT NULL,
        id        BLOB    NOT NULL,
        at_secs   INTEGER NOT NULL,
        at_nanos  INTEGER NOT NULL,
        payload   TEXT    NOT NULL,
        PRIMARY KEY (topic, seq)
    ) WITHOUT ROWID;
    CREATE TABLE IF NOT EXISTS checkpoint (
        subscriber BLOB    NOT NULL,
        topic      TEXT    NOT NULL,
        epoch      BLOB    NOT NULL,
        acked_seq  INTEGER NOT NULL,
        PRIMARY KEY (subscriber, topic)
    ) WITHOUT ROWID;
";

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("the event database could not be prepared")]
    Open(#[source] rusqlite::Error),
    #[error("the event database could not be created")]
    Io(#[source] std::io::Error),
    #[error("the event database rejected a statement")]
    Query(#[source] rusqlite::Error),
    #[error("the event database thread is gone")]
    Closed,
    #[error("the event database thread stopped unexpectedly")]
    Thread,
    #[error("the event database returned a value this version cannot read")]
    Corrupt,
}

#[derive(Clone, Copy, Debug)]
pub struct TopicState {
    pub epoch: LogEpoch,
    pub first: Seq,
    pub next: Seq,
    pub bytes: u64,
    pub last_append: SystemTime,
}

/// One SQLite connection, owned by one OS thread and reached over a bounded channel.
///
/// Nothing blocking runs on the async runtime and no `rusqlite` type crosses an async signature.
#[derive(Clone)]
pub struct Store {
    commands: mpsc::Sender<Command>,
    thread: Arc<Mutex<Option<JoinHandle<()>>>>,
}

enum Command {
    OpenTopic {
        topic: Topic,
        reply: Reply<(TopicState, bool)>,
    },
    Append {
        topic: Topic,
        payload: Payload,
        at: SystemTime,
        reply: Reply<(Published, Window)>,
    },
    Read {
        topic: Topic,
        from: Seq,
        max: NonZeroUsize,
        reply: Reply<(TopicState, Vec<Arc<Event>>)>,
    },
    Checkpoint {
        subscriber: SubscriberId,
        topic: Topic,
        reply: Reply<Option<Cursor>>,
    },
    Advance {
        subscriber: SubscriberId,
        topic: Topic,
        through: Cursor,
        reply: Reply<Cursor>,
    },
    Reclaim {
        topic: Topic,
        at: SystemTime,
        reply: Reply<()>,
    },
    Stop {
        reply: oneshot::Sender<()>,
    },
}

type Reply<T> = oneshot::Sender<Result<T, StoreError>>;

impl Store {
    /// Opens or creates the database, applies the schema, and starts the owning thread.
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`] when the database cannot be created, prepared, or owned.
    pub fn open(
        path: &Path,
        retention: Retention,
    ) -> Result<Self, StoreError> {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent).map_err(StoreError::Io)?;
        }

        let connection = Connection::open(path).map_err(StoreError::Open)?;
        prepare(&connection)?;

        let (commands, mut receiver) = mpsc::channel(CHANNEL);

        let thread = std::thread::Builder::new()
            .name("mokmok-bus-store".to_owned())
            .spawn(move || run(&mut receiver, connection, retention))
            .map_err(StoreError::Io)?;

        Ok(Self {
            commands,
            thread: Arc::new(Mutex::new(Some(thread))),
        })
    }

    /// Creates the topic row when it is missing. `true` means a fresh epoch was minted.
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`] when the row cannot be read or inserted.
    pub async fn open_topic(
        &self,
        topic: Topic,
    ) -> Result<(TopicState, bool), StoreError> {
        self.exchange(|reply| Command::OpenTopic { topic, reply })
            .await
    }

    /// One append, one transaction. The reply resolves after `COMMIT`.
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`] when the append or its eviction cannot be committed.
    pub async fn append(
        &self,
        topic: Topic,
        payload: Payload,
        at: SystemTime,
    ) -> Result<(Published, Window), StoreError> {
        self.exchange(|reply| Command::Append {
            topic,
            payload,
            at,
            reply,
        })
        .await
    }

    /// At most `max` retained rows from `from`, plus the row that describes the range.
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`] when the range cannot be read.
    pub async fn read(
        &self,
        topic: Topic,
        from: Seq,
        max: NonZeroUsize,
    ) -> Result<(TopicState, Vec<Arc<Event>>), StoreError> {
        self.exchange(|reply| Command::Read {
            topic,
            from,
            max,
            reply,
        })
        .await
    }

    /// The stored checkpoint, or `None` when this subscriber has never acked this topic.
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`] when the row cannot be read.
    pub async fn checkpoint(
        &self,
        subscriber: SubscriberId,
        topic: Topic,
    ) -> Result<Option<Cursor>, StoreError> {
        self.exchange(|reply| Command::Checkpoint {
            subscriber,
            topic,
            reply,
        })
        .await
    }

    /// Never moves backwards and never past `through`. Answers with the stored cursor. An `epoch`
    /// that is not the topic's current one leaves the checkpoint untouched.
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`] when the checkpoint cannot be written.
    pub async fn advance(
        &self,
        subscriber: SubscriberId,
        topic: Topic,
        through: Cursor,
    ) -> Result<Cursor, StoreError> {
        self.exchange(|reply| Command::Advance {
            subscriber,
            topic,
            through,
            reply,
        })
        .await
    }

    /// Drops the topic's events and keeps its row, the way a reclamation must.
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`] when the events cannot be deleted.
    pub async fn reclaim(
        &self,
        topic: Topic,
        at: SystemTime,
    ) -> Result<(), StoreError> {
        self.exchange(|reply| Command::Reclaim { topic, at, reply })
            .await
    }

    /// Stops the owning thread and joins it, so a later open on the same file sees every commit.
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`] when the thread does not stop cleanly.
    pub async fn shutdown(&self) -> Result<(), StoreError> {
        let (reply, stopped) = oneshot::channel();
        if self.commands.send(Command::Stop { reply }).await.is_ok() {
            let _ = stopped.await;
        }

        let thread = self
            .thread
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();

        thread.map_or_else(
            || Ok(()),
            |thread| thread.join().map_err(|_| StoreError::Thread),
        )
    }

    async fn exchange<T>(
        &self,
        command: impl FnOnce(Reply<T>) -> Command,
    ) -> Result<T, StoreError> {
        let (reply, answer) = oneshot::channel();

        self.commands
            .send(command(reply))
            .await
            .map_err(|_| StoreError::Closed)?;

        answer.await.map_err(|_| StoreError::Closed)?
    }
}

fn run(
    receiver: &mut mpsc::Receiver<Command>,
    mut connection: Connection,
    retention: Retention,
) {
    while let Some(command) = receiver.blocking_recv() {
        match command {
            Command::Stop { reply } => {
                let _ = reply.send(());
                break;
            },
            Command::OpenTopic { topic, reply } => {
                let _ = reply.send(open_topic(&mut connection, topic));
            },
            Command::Append {
                topic,
                payload,
                at,
                reply,
            } => {
                let _ = reply.send(append(&mut connection, retention, topic, &payload, at));
            },
            Command::Read {
                topic,
                from,
                max,
                reply,
            } => {
                let _ = reply.send(read(&connection, topic, from, max));
            },
            Command::Checkpoint {
                subscriber,
                topic,
                reply,
            } => {
                let _ = reply.send(checkpoint(&connection, subscriber, topic));
            },
            Command::Advance {
                subscriber,
                topic,
                through,
                reply,
            } => {
                let _ = reply.send(advance(&connection, subscriber, topic, through));
            },
            Command::Reclaim { topic, at, reply } => {
                let _ = reply.send(reclaim(&mut connection, topic, at));
            },
        }
    }
}

fn prepare(connection: &Connection) -> Result<(), StoreError> {
    connection
        .execute_batch(
            "PRAGMA journal_mode = WAL;\
             PRAGMA synchronous = FULL;\
             PRAGMA busy_timeout = 5000;",
        )
        .map_err(StoreError::Open)?;

    connection.execute_batch(SCHEMA).map_err(StoreError::Open)
}

fn topic_state(
    connection: &Connection,
    topic: Topic,
) -> Result<Option<TopicState>, StoreError> {
    connection
        .query_row(
            "SELECT epoch, first, next, bytes, last_append FROM topic WHERE name = ?1",
            params![topic.to_string()],
            |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            },
        )
        .optional()
        .map_err(StoreError::Query)?
        .map(|(epoch, first, next, bytes, last_append)| {
            Ok(TopicState {
                epoch: LogEpoch::new(Uuid::from_slice(&epoch).map_err(|_| StoreError::Corrupt)?),
                first: seq_from(first)?,
                next: seq_from(next)?,
                bytes: u64::try_from(bytes).map_err(|_| StoreError::Corrupt)?,
                last_append: time_from_millis(last_append)?,
            })
        })
        .transpose()
}

fn open_topic(
    connection: &mut Connection,
    topic: Topic,
) -> Result<(TopicState, bool), StoreError> {
    if let Some(state) = topic_state(connection, topic)? {
        return Ok((state, false));
    }

    let now = SystemTime::now();
    let epoch = LogEpoch::new(Uuid::new_v4());
    connection
        .execute(
            "INSERT INTO topic (name, epoch, first, next, bytes, last_append)
             VALUES (?1, ?2, 0, 0, 0, ?3)",
            params![
                topic.to_string(),
                epoch.as_uuid().as_bytes().as_slice(),
                to_millis(now),
            ],
        )
        .map_err(StoreError::Query)?;

    Ok((
        TopicState {
            epoch,
            first: Seq::new(0),
            next: Seq::new(0),
            bytes: 0,
            last_append: now,
        },
        true,
    ))
}

fn append(
    connection: &mut Connection,
    retention: Retention,
    topic: Topic,
    payload: &Payload,
    at: SystemTime,
) -> Result<(Published, Window), StoreError> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(StoreError::Query)?;

    let name = topic.to_string();
    let state = topic_state(&transaction, topic)?.ok_or(StoreError::Corrupt)?;

    let seq = state.next;
    let id = EventID::new(Timestamp::now(NoContext));
    let (secs, nanos) = split(at);
    transaction
        .execute(
            "INSERT INTO event (topic, seq, id, at_secs, at_nanos, payload)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                name,
                to_i64(seq.get())?,
                id.as_uuid().as_bytes().as_slice(),
                secs,
                nanos,
                payload.as_str(),
            ],
        )
        .map_err(StoreError::Query)?;

    let mut first = state.first.get();
    let next = seq.get() + 1;
    let mut bytes = state
        .bytes
        .checked_add(payload.as_str().len() as u64)
        .ok_or(StoreError::Corrupt)?;

    while next - first > retention.events.get() as u64
        || (next - first > 1 && bytes > retention.bytes.get() as u64)
    {
        let (oldest, held) = oldest_event(&transaction, topic)?;
        transaction
            .execute(
                "DELETE FROM event WHERE topic = ?1 AND seq = ?2",
                params![name, oldest],
            )
            .map_err(StoreError::Query)?;

        bytes = bytes
            .checked_sub(u64::try_from(held).map_err(|_| StoreError::Corrupt)?)
            .ok_or(StoreError::Corrupt)?;
        first += 1;
    }

    let first_value = to_i64(first)?;
    let next_value = to_i64(next)?;
    transaction
        .execute(
            "UPDATE topic SET first = ?1, next = ?2, bytes = ?3, last_append = ?4 WHERE name = ?5",
            params![
                first_value,
                next_value,
                i64::try_from(bytes).map_err(|_| StoreError::Corrupt)?,
                to_millis(at),
                name,
            ],
        )
        .map_err(StoreError::Query)?;

    transaction.commit().map_err(StoreError::Query)?;

    Ok((
        Published::new(id, seq, at),
        Window::new(state.epoch, Seq::new(first), Seq::new(next)),
    ))
}

fn oldest_event(
    connection: &Connection,
    topic: Topic,
) -> Result<(i64, i64), StoreError> {
    connection
        .query_row(
            "SELECT seq, length(CAST(payload AS BLOB)) FROM event
             WHERE topic = ?1 ORDER BY seq LIMIT 1",
            params![topic.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(StoreError::Query)
}

fn read(
    connection: &Connection,
    topic: Topic,
    from: Seq,
    max: NonZeroUsize,
) -> Result<(TopicState, Vec<Arc<Event>>), StoreError> {
    let state = topic_state(connection, topic)?.ok_or(StoreError::Corrupt)?;
    let mut statement = connection
        .prepare(
            "SELECT seq, id, at_secs, at_nanos, payload FROM event
             WHERE topic = ?1 AND seq >= ?2 ORDER BY seq LIMIT ?3",
        )
        .map_err(StoreError::Query)?;

    let rows = statement
        .query_map(
            params![
                topic.to_string(),
                to_i64(from.get())?,
                to_i64(max.get() as u64)?
            ],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Vec<u8>>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                ))
            },
        )
        .map_err(StoreError::Query)?;

    let mut events = Vec::new();
    for row in rows {
        let (seq, id, secs, nanos, payload) = row.map_err(StoreError::Query)?;
        events.push(Arc::new(Event::new(
            EventID::from_uuid(Uuid::from_slice(&id).map_err(|_| StoreError::Corrupt)?),
            topic,
            seq_from(seq)?,
            time_from(secs, nanos)?,
            Payload::from_stored(payload).map_err(|_| StoreError::Corrupt)?,
        )));
    }

    Ok((state, events))
}

fn checkpoint(
    connection: &Connection,
    subscriber: SubscriberId,
    topic: Topic,
) -> Result<Option<Cursor>, StoreError> {
    connection
        .query_row(
            "SELECT epoch, acked_seq FROM checkpoint WHERE subscriber = ?1 AND topic = ?2",
            params![
                subscriber.as_uuid().as_bytes().as_slice(),
                topic.to_string()
            ],
            |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, i64>(1)?)),
        )
        .optional()
        .map_err(StoreError::Query)?
        .map(|(epoch, seq)| {
            Ok(Cursor::new(
                LogEpoch::new(Uuid::from_slice(&epoch).map_err(|_| StoreError::Corrupt)?),
                seq_from(seq)?,
            ))
        })
        .transpose()
}

fn advance(
    connection: &Connection,
    subscriber: SubscriberId,
    topic: Topic,
    through: Cursor,
) -> Result<Cursor, StoreError> {
    let Some(state) = topic_state(connection, topic)? else {
        return Ok(through);
    };

    let stored = checkpoint(connection, subscriber, topic)?;
    if through.epoch() != state.epoch {
        return Ok(stored.unwrap_or(through));
    }

    let held = stored
        .filter(|cursor| cursor.epoch() == state.epoch)
        .map_or(Seq::new(0), Cursor::seq);
    let seq = if through.seq() > held {
        through.seq()
    } else {
        held
    };

    connection
        .execute(
            "INSERT INTO checkpoint (subscriber, topic, epoch, acked_seq) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(subscriber, topic) DO UPDATE SET epoch = excluded.epoch, acked_seq = excluded.acked_seq",
            params![
                subscriber.as_uuid().as_bytes().as_slice(),
                topic.to_string(),
                state.epoch.as_uuid().as_bytes().as_slice(),
                to_i64(seq.get())?,
            ],
        )
        .map_err(StoreError::Query)?;

    Ok(Cursor::new(state.epoch, seq))
}

fn reclaim(
    connection: &mut Connection,
    topic: Topic,
    at: SystemTime,
) -> Result<(), StoreError> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(StoreError::Query)?;

    let name = topic.to_string();
    transaction
        .execute("DELETE FROM event WHERE topic = ?1", params![name])
        .map_err(StoreError::Query)?;
    // `first = next` keeps the sequence the topic had reached, so a returning checkpoint is told
    // what it dropped rather than resolving into a range that starts over.
    transaction
        .execute(
            "UPDATE topic SET first = next, bytes = 0, last_append = ?1 WHERE name = ?2",
            params![to_millis(at), name],
        )
        .map_err(StoreError::Query)?;

    transaction.commit().map_err(StoreError::Query)
}

fn seq_from(value: i64) -> Result<Seq, StoreError> {
    u64::try_from(value)
        .map(Seq::new)
        .map_err(|_| StoreError::Corrupt)
}

fn to_i64(value: u64) -> Result<i64, StoreError> {
    i64::try_from(value).map_err(|_| StoreError::Corrupt)
}

fn split(at: SystemTime) -> (i64, i64) {
    let since = at.duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO);
    let secs = i64::try_from(since.as_secs()).unwrap_or(i64::MAX);

    (secs, i64::from(since.subsec_nanos()))
}

fn to_millis(at: SystemTime) -> i64 {
    let since = at.duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO);

    i64::try_from(since.as_millis()).unwrap_or(i64::MAX)
}

fn time_from_millis(value: i64) -> Result<SystemTime, StoreError> {
    let millis = u64::try_from(value).map_err(|_| StoreError::Corrupt)?;

    UNIX_EPOCH
        .checked_add(Duration::from_millis(millis))
        .ok_or(StoreError::Corrupt)
}

fn time_from(
    secs: i64,
    nanos: i64,
) -> Result<SystemTime, StoreError> {
    let seconds = u64::try_from(secs).map_err(|_| StoreError::Corrupt)?;
    let nanos = u32::try_from(nanos).map_err(|_| StoreError::Corrupt)?;
    if nanos >= 1_000_000_000 {
        return Err(StoreError::Corrupt);
    }

    UNIX_EPOCH
        .checked_add(Duration::new(seconds, nanos))
        .ok_or(StoreError::Corrupt)
}

/// The millisecond clock the reclaim sweep compares against, so the topic row and an entry's
/// cached hint are read as the same unit.
pub fn unix_millis(at: SystemTime) -> u64 {
    let since = at.duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO);

    u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
}

pub struct AppendClock(AtomicU64);

impl AppendClock {
    pub fn new(at: SystemTime) -> Self {
        Self(AtomicU64::new(unix_millis(at)))
    }

    pub fn record(
        &self,
        at: SystemTime,
    ) {
        self.0.store(unix_millis(at), Ordering::Relaxed);
    }

    pub fn idle(
        &self,
        now: SystemTime,
        idle: Duration,
    ) -> bool {
        let last = self.0.load(Ordering::Relaxed);
        let elapsed = unix_millis(now).saturating_sub(last);

        elapsed >= u64::try_from(idle.as_millis()).unwrap_or(u64::MAX)
    }
}

#[cfg(test)]
mod tests {
    use super::{AppendClock, Store, StoreError, TopicState};
    use crate::config::Retention;
    use crate::event::{Cursor, LogEpoch, Seq};
    use crate::payload::Payload;
    use crate::subscriber::SubscriberId;
    use crate::topic::Topic;
    use proptest::prelude::*;
    use std::num::NonZeroUsize;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    use uuid::Uuid;

    static NEXT: AtomicUsize = AtomicUsize::new(0);

    fn scratch(name: &str) -> PathBuf {
        let unique = format!(
            "mokmok-store-{}-{}-{name}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        );

        std::env::temp_dir().join(unique)
    }

    fn retention(
        events: usize,
        bytes: usize,
    ) -> Retention {
        Retention {
            events: NonZeroUsize::new(events).unwrap(),
            bytes: NonZeroUsize::new(bytes).unwrap(),
            idle: Duration::from_secs(600),
        }
    }

    fn max(events: usize) -> NonZeroUsize {
        NonZeroUsize::new(events).unwrap()
    }

    /// A payload whose raw JSON encoding is exactly `bytes` long; a JSON string is its text plus
    /// the two quotes around it.
    fn payload(bytes: usize) -> Payload {
        Payload::json(&"x".repeat(bytes - 2)).unwrap()
    }

    fn store(
        events: usize,
        bytes: usize,
    ) -> Store {
        Store::open(Path::new(":memory:"), retention(events, bytes)).unwrap()
    }

    async fn open(store: &Store) -> TopicState {
        store.open_topic(Topic::Global).await.unwrap().0
    }

    async fn append(
        store: &Store,
        bytes: usize,
    ) -> u64 {
        store
            .append(Topic::Global, payload(bytes), SystemTime::UNIX_EPOCH)
            .await
            .unwrap()
            .0
            .seq()
            .get()
    }

    async fn seqs(
        store: &Store,
        from: Seq,
        count: usize,
    ) -> Vec<u64> {
        store
            .read(Topic::Global, from, max(count))
            .await
            .unwrap()
            .1
            .iter()
            .map(|event| event.seq().get())
            .collect()
    }

    async fn payloads(
        store: &Store,
        from: Seq,
        count: usize,
    ) -> Vec<usize> {
        store
            .read(Topic::Global, from, max(count))
            .await
            .unwrap()
            .1
            .iter()
            .map(|event| event.payload().as_str().len())
            .collect()
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    #[tokio::test]
    async fn beginning_reads_every_retained_event() {
        let store = store(4, 1 << 20);
        open(&store).await;
        append(&store, 3).await;
        append(&store, 3).await;

        let (state, batch) = store
            .read(Topic::Global, Seq::new(0), max(8))
            .await
            .unwrap();

        assert_eq!(batch.len(), 2);
        assert_eq!(
            batch.iter().map(|e| e.seq().get()).collect::<Vec<_>>(),
            [0, 1]
        );
        assert_eq!(state.first.get(), 0);
        assert_eq!(state.next.get(), 2);
    }

    #[tokio::test]
    async fn max_bounds_one_read() {
        let store = store(4, 1 << 20);
        open(&store).await;
        for _ in 0..4 {
            append(&store, 3).await;
        }

        let (state, _) = store
            .read(Topic::Global, Seq::new(0), max(2))
            .await
            .unwrap();

        assert_eq!(seqs(&store, Seq::new(0), 2).await, [0, 1]);
        assert_eq!(state.next.get(), 4);
    }

    #[tokio::test]
    async fn a_read_inside_the_window_starts_at_the_seq() {
        let store = store(4, 1 << 20);
        open(&store).await;
        for _ in 0..4 {
            append(&store, 3).await;
        }

        assert_eq!(seqs(&store, Seq::new(2), 8).await, [2, 3]);
    }

    #[tokio::test]
    async fn a_read_below_the_floor_starts_at_first_and_shows_the_gap() {
        let store = store(2, 1 << 20);
        open(&store).await;
        for _ in 0..5 {
            append(&store, 3).await;
        }

        let (state, batch) = store
            .read(Topic::Global, Seq::new(1), max(8))
            .await
            .unwrap();

        assert_eq!(state.first.get(), 3, "the floor the checkpoint fell behind");
        assert_eq!(
            batch.iter().map(|e| e.seq().get()).collect::<Vec<_>>(),
            [3, 4]
        );
    }

    #[tokio::test]
    async fn sequences_stay_contiguous_across_appends_and_evictions() {
        let store = store(2, 1 << 20);
        open(&store).await;

        for expected in 0..5 {
            assert_eq!(append(&store, 3).await, expected);
        }

        let state = store.open_topic(Topic::Global).await.unwrap().0;
        assert_eq!(state.first.get(), 3);
        assert_eq!(state.next.get(), 5);
        assert_eq!(state.next.get() - state.first.get(), 2);
        assert_eq!(seqs(&store, state.first, 8).await, [3, 4]);
    }

    #[tokio::test]
    async fn byte_retention_keeps_the_newest_event() {
        let store = store(4096, 4);
        open(&store).await;
        append(&store, 3).await;
        append(&store, 3).await;
        append(&store, 5).await;

        let (state, batch) = store
            .read(Topic::Global, Seq::new(0), max(8))
            .await
            .unwrap();

        assert_eq!(batch.iter().map(|e| e.seq().get()).collect::<Vec<_>>(), [2]);
        assert_eq!(state.first.get(), 2);
        assert_eq!(batch[0].payload().as_str().len(), 5);
    }

    #[tokio::test]
    async fn an_empty_topic_reads_no_events() {
        let store = store(4, 1 << 20);
        let state = open(&store).await;

        let (read, batch) = store
            .read(Topic::Global, state.first, max(8))
            .await
            .unwrap();

        assert!(batch.is_empty());
        assert_eq!(read.first, read.next);
    }

    #[tokio::test]
    async fn opening_a_topic_twice_keeps_its_epoch() {
        let store = store(4, 1 << 20);
        let (first, created) = store.open_topic(Topic::Global).await.unwrap();
        let (second, again) = store.open_topic(Topic::Global).await.unwrap();

        assert!(created);
        assert!(!again);
        assert_eq!(first.epoch, second.epoch);
    }

    #[tokio::test]
    async fn a_checkpoint_advances_and_answers_with_the_stored_cursor() {
        let store = store(4, 1 << 20);
        let state = open(&store).await;
        let id = SubscriberId::new();
        append(&store, 3).await;

        let first = store
            .advance(id, Topic::Global, Cursor::new(state.epoch, Seq::new(1)))
            .await
            .unwrap();
        let stored = store.checkpoint(id, Topic::Global).await.unwrap().unwrap();

        assert_eq!(first.seq().get(), 1);
        assert_eq!(stored, first);
    }

    #[tokio::test]
    async fn advance_never_moves_backwards() {
        let store = store(4, 1 << 20);
        let state = open(&store).await;
        let id = SubscriberId::new();

        store
            .advance(id, Topic::Global, Cursor::new(state.epoch, Seq::new(5)))
            .await
            .unwrap();
        let answered = store
            .advance(id, Topic::Global, Cursor::new(state.epoch, Seq::new(2)))
            .await
            .unwrap();

        assert_eq!(answered.seq().get(), 5);
    }

    #[tokio::test]
    async fn advance_with_a_foreign_epoch_is_a_no_op() {
        let store = store(4, 1 << 20);
        open(&store).await;
        let id = SubscriberId::new();
        let foreign = LogEpoch::new(Uuid::new_v4());

        let answered = store
            .advance(id, Topic::Global, Cursor::new(foreign, Seq::new(9)))
            .await
            .unwrap();

        assert_eq!(answered.epoch(), foreign);
        assert!(store.checkpoint(id, Topic::Global).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn reclaim_drops_the_events_and_keeps_the_epoch() {
        let store = store(4, 1 << 20);
        let state = open(&store).await;
        for _ in 0..3 {
            append(&store, 3).await;
        }

        store
            .reclaim(Topic::Global, SystemTime::UNIX_EPOCH)
            .await
            .unwrap();

        let after = store.open_topic(Topic::Global).await.unwrap().0;
        assert_eq!(after.epoch, state.epoch);
        assert_eq!(after.first.get(), 3);
        assert_eq!(after.next.get(), 3);
        assert_eq!(after.bytes, 0);
        assert_eq!(seqs(&store, after.first, 8).await, Vec::<u64>::new());
    }

    #[tokio::test]
    async fn a_checkpoint_behind_the_floor_is_still_stored() {
        let store = store(2, 1 << 20);
        let state = open(&store).await;
        let id = SubscriberId::new();
        store
            .advance(id, Topic::Global, Cursor::new(state.epoch, Seq::new(1)))
            .await
            .unwrap();

        for _ in 0..5 {
            append(&store, 3).await;
        }

        let after = store.open_topic(Topic::Global).await.unwrap().0;
        let stored = store.checkpoint(id, Topic::Global).await.unwrap().unwrap();

        assert_eq!(
            stored.seq().get(),
            1,
            "the checkpoint is never silently moved"
        );
        assert!(
            stored.seq() < after.first,
            "the floor passed the checkpoint"
        );
    }

    #[tokio::test]
    async fn a_store_that_cannot_be_opened_is_reported() {
        let Err(error) = Store::open(
            Path::new("/proc/definitely/not/here/bus.db"),
            retention(4, 64),
        ) else {
            panic!("a database under /proc was opened");
        };

        assert!(
            matches!(error, StoreError::Io(_) | StoreError::Open(_)),
            "expected an open failure",
        );
    }

    #[test]
    fn the_append_clock_reports_idleness() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
        let clock = AppendClock::new(now);

        assert!(!clock.idle(now, Duration::from_secs(600)));
        assert!(clock.idle(now + Duration::from_secs(600), Duration::from_secs(600)));
        assert!(clock.idle(now, Duration::ZERO));
    }

    #[tokio::test]
    async fn shutdown_closes_the_store() {
        let store = store(4, 1 << 20);
        open(&store).await;
        store.shutdown().await.unwrap();

        let appended = store
            .append(Topic::Global, payload(3), SystemTime::UNIX_EPOCH)
            .await;

        assert!(matches!(appended, Err(StoreError::Closed)));
    }

    #[tokio::test]
    async fn open_creates_a_missing_parent_directory() {
        let parent = scratch("parent");
        let path = parent.join("bus.db");
        assert!(!parent.exists(), "the parent starts missing");

        let store = Store::open(&path, retention(4, 64)).unwrap();

        assert!(parent.is_dir(), "the parent was created");
        store.shutdown().await.unwrap();
        std::fs::remove_dir_all(&parent).unwrap();
    }

    #[test]
    fn an_append_resets_the_idle_window() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
        let later = now + Duration::from_secs(3_600);
        let clock = AppendClock::new(now);

        clock.record(later);

        assert!(!clock.idle(later, Duration::from_secs(600)));
    }

    /// The start index the byte bound settles on: pop until the suffix fits, but never evict the
    /// newest event, which the store keeps no matter how large it is.
    fn byte_bound_start(
        sizes: &[usize],
        bytes: usize,
    ) -> usize {
        let mut start = 0;

        while start + 1 < sizes.len() && sizes[start..].iter().sum::<usize>() > bytes {
            start += 1;
        }

        start
    }

    proptest! {
        #[test]
        fn eviction_keeps_exactly_the_count_the_event_retention_allows(
            events in 1_usize..=8,
            sizes in prop::collection::vec(3_usize..=6, 0..=12),
        ) {
            runtime().block_on(async {
                let store = store(events, 1 << 20);
                open(&store).await;

                let mut appended = Vec::new();
                for size in &sizes {
                    appended.push(append(&store, *size).await);
                }

                let expected = sizes.len().min(events);
                let read = seqs(&store, Seq::new(0), 64).await;

                prop_assert_eq!(read.is_empty(), expected == 0);
                prop_assert_eq!(read, appended[appended.len() - expected..].to_vec());

                Ok::<(), TestCaseError>(())
            }).unwrap();
        }

        #[test]
        fn eviction_keeps_the_newest_events_that_fit_the_byte_retention(
            bytes in 4_usize..=12,
            sizes in prop::collection::vec(2_usize..=6, 1..=8),
        ) {
            runtime().block_on(async {
                let store = store(16, bytes);
                open(&store).await;

                let mut appended = Vec::new();
                for size in &sizes {
                    appended.push(append(&store, *size).await);
                }

                let start = byte_bound_start(&sizes, bytes);
                let read = seqs(&store, Seq::new(0), 64).await;

                prop_assert!(!read.is_empty());
                prop_assert_eq!(read, appended[start..].to_vec());

                Ok::<(), TestCaseError>(())
            }).unwrap();
        }

        #[test]
        fn an_event_timestamp_survives_the_round_trip(
            secs in 0_u64..=4_102_444_800,
            nanos in 0_u32..1_000_000_000,
        ) {
            runtime().block_on(async {
                let store = store(4, 1 << 20);
                open(&store).await;
                let at = UNIX_EPOCH + Duration::new(secs, nanos);

                store.append(Topic::Global, payload(3), at).await.unwrap();

                let state = store.open_topic(Topic::Global).await.unwrap().0;
                let (_, events) = store
                    .read(Topic::Global, Seq::new(0), max(1))
                    .await
                    .unwrap();
                let millis =
                    u64::try_from(at.duration_since(UNIX_EPOCH).unwrap().as_millis()).unwrap();
                let truncated = UNIX_EPOCH + Duration::from_millis(millis);

                prop_assert_eq!(events.len(), 1);
                prop_assert_eq!(events[0].at(), at);
                prop_assert_eq!(state.last_append, truncated);

                Ok::<(), TestCaseError>(())
            }).unwrap();
        }

        #[test]
        fn the_store_is_a_log_over_appends_and_reads(
            events in 1_usize..=6,
            bytes in 4_usize..=24,
            ops in prop::collection::vec(
                (2_usize..=6, prop::option::of((0_u64..=12, 1_usize..=6))),
                0..=12,
            ),
        ) {
            runtime().block_on(async {
                let store = store(events, bytes);
                open(&store).await;
                let mut model: Vec<usize> = Vec::new();

                for (size, read) in &ops {
                    append(&store, *size).await;
                    model.push(*size);
                    while model.len() > events
                        || (model.len() > 1 && model.iter().sum::<usize>() > bytes)
                    {
                        model.remove(0);
                    }

                    let state = store.open_topic(Topic::Global).await.unwrap().0;
                    let first = state.first.get();
                    prop_assert_eq!(state.next.get() - first, model.len() as u64);
                    prop_assert_eq!(state.bytes, model.iter().sum::<usize>() as u64);

                    let retained = payloads(&store, state.first, 64).await;
                    prop_assert_eq!(&retained, &model);
                    let from_zero = payloads(&store, Seq::new(0), 64).await;
                    prop_assert_eq!(&from_zero, &model);

                    if let Some((position, width)) = read {
                        let start = (*position).max(first);
                        let skip = usize::try_from(start - first).unwrap();
                        let expected: Vec<usize> =
                            model.iter().skip(skip).take(*width).copied().collect();
                        let got = payloads(&store, Seq::new(*position), *width).await;
                        prop_assert_eq!(&got, &expected);
                    }
                }

                Ok::<(), TestCaseError>(())
            }).unwrap();
        }
    }
}

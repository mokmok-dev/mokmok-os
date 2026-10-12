use mokmok_event_store::{
    Append, CommandId, CommitPosition, ConsumerId, Expected, Kind, NewEvent, Outcome, Payload,
    Processed, Store, StreamId, Version,
};
use rusqlite::Connection;
use std::io::{BufRead, BufReader};
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};
use std::time::Duration;

// The `cfg(test)` module is what tells the `allow-unwrap-in-tests`, `allow-expect-in-tests`, and
// `allow-panic-in-tests` settings in `.clippy.toml` that this harness is test code.
#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    /// Long enough for a helper that has already opened its store to reach its marker, short
    /// enough that a wedged one fails the suite instead of hanging it.
    const BOUND: Duration = Duration::from_secs(10);

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

    /// The killed helper. `Drop` reaps it, so a failing assertion cannot leave a parked process
    /// behind for the next test to trip over.
    struct Crash {
        child: Child,
        lines: Receiver<String>,
        reader: Option<JoinHandle<()>>,
    }

    impl Crash {
        fn spawn(
            point: &str,
            db: &Path,
            stream: StreamId,
            commands: usize,
            appends: usize,
        ) -> Self {
            let mut command = Command::new(env!("CARGO_BIN_EXE_mokmok-event-store-crash"));
            command
                .arg("--db")
                .arg(db)
                .arg("--point")
                .arg(point)
                .arg("--stream")
                .arg(stream.to_string())
                .arg("--commands")
                .arg(commands.to_string())
                .arg("--appends")
                .arg(appends.to_string())
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit());
            let mut child = command.spawn().unwrap();
            let stdout = child.stdout.take().unwrap();
            let (sender, lines) = mpsc::channel();
            let reader = thread::spawn(move || {
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    if sender.send(line).is_err() {
                        break;
                    }
                }
            });

            Self {
                child,
                lines,
                reader: Some(reader),
            }
        }

        fn marker(&self) -> String {
            self.lines
                .recv_timeout(BOUND)
                .unwrap_or_else(|elapsed| panic!("no marker within {BOUND:?}: {elapsed}"))
        }

        /// Waits for a helper that was given a bounded amount of work to finish on its own. A
        /// writer that never finishes fails the test instead of hanging the suite.
        fn finished(&mut self) -> std::process::ExitStatus {
            let start = std::time::Instant::now();
            loop {
                if let Some(status) = self.child.try_wait().unwrap() {
                    return status;
                }
                assert!(
                    start.elapsed() < BOUND,
                    "the helper did not finish within {BOUND:?}",
                );
                thread::sleep(Duration::from_millis(10));
            }
        }
    }

    impl Drop for Crash {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
            if let Some(reader) = self.reader.take() {
                let _ = reader.join();
            }
        }
    }

    fn max(count: usize) -> NonZeroUsize {
        NonZeroUsize::new(count).unwrap()
    }

    fn marker_position(marker: &str) -> u64 {
        let Some(rest) = marker.strip_prefix("COMMITTED ") else {
            panic!("expected a committed marker, got {marker:?}");
        };

        rest.trim().parse::<u64>().unwrap()
    }

    fn new_event() -> NewEvent {
        NewEvent::new(
            Kind::new("turn.finished").unwrap(),
            Payload::json(&serde_json::json!({"retry": true})).unwrap(),
        )
    }

    fn number(
        connection: &Connection,
        sql: &str,
        parameters: impl rusqlite::Params,
    ) -> i64 {
        connection
            .query_row(sql, parameters, |row| row.get(0))
            .unwrap()
    }

    fn numbers(
        connection: &Connection,
        sql: &str,
        parameters: impl rusqlite::Params,
    ) -> Vec<i64> {
        let mut statement = connection.prepare(sql).unwrap();
        let rows = statement
            .query_map(parameters, |row| row.get::<_, i64>(0))
            .unwrap();

        rows.map(|row| row.unwrap()).collect()
    }

    fn blobs(
        connection: &Connection,
        sql: &str,
    ) -> Vec<Vec<u8>> {
        let mut statement = connection.prepare(sql).unwrap();
        let rows = statement
            .query_map([], |row| row.get::<_, Vec<u8>>(0))
            .unwrap();

        rows.map(|row| row.unwrap()).collect()
    }

    fn command_rows(
        connection: &Connection,
        command: CommandId,
    ) -> i64 {
        number(
            connection,
            "SELECT COUNT(*) FROM commit_log WHERE command = ?1",
            rusqlite::params![command.as_uuid().as_bytes()],
        )
    }

    /// Reads every invariant straight from the tables, so the store is never the judge of its own
    /// rows.
    fn sweep(connection: &Connection) {
        let positions = numbers(
            connection,
            "SELECT position FROM commit_log ORDER BY position",
            rusqlite::params![],
        );
        let dense = 1..=i64::try_from(positions.len()).unwrap();
        assert_eq!(
            positions,
            dense.collect::<Vec<_>>(),
            "positions are dense from one"
        );

        for &position in &positions {
            let count = number(
                connection,
                "SELECT count FROM commit_log WHERE position = ?1",
                rusqlite::params![position],
            );
            let ordinals = numbers(
                connection,
                "SELECT ordinal FROM event WHERE position = ?1 ORDER BY ordinal",
                rusqlite::params![position],
            );
            assert_eq!(
                ordinals,
                (0..count).collect::<Vec<_>>(),
                "commit {position} holds ordinals 0..{count}",
            );
        }

        for id in blobs(connection, "SELECT id FROM stream ORDER BY id") {
            let version = number(
                connection,
                "SELECT version FROM stream WHERE id = ?1",
                rusqlite::params![id],
            );
            let versions = numbers(
                connection,
                "SELECT version FROM event WHERE stream = ?1 ORDER BY version",
                rusqlite::params![id],
            );
            assert_eq!(
                versions,
                (0..version).collect::<Vec<_>>(),
                "a stream's versions are exactly 0..version",
            );
        }

        assert_eq!(
            number(
                connection,
                "SELECT COUNT(*) FROM event e
                 LEFT JOIN stream s ON s.id = e.stream WHERE s.id IS NULL",
                rusqlite::params![],
            ),
            0,
            "every event names a stream row",
        );

        let joins = [
            (
                "outbox",
                "SELECT COUNT(*) FROM outbox o
                 LEFT JOIN commit_log c ON c.position = o.position WHERE c.position IS NULL",
            ),
            (
                "inbox",
                "SELECT COUNT(*) FROM inbox i
                 LEFT JOIN commit_log c ON c.position = i.position WHERE c.position IS NULL",
            ),
            (
                "projection",
                "SELECT COUNT(*) FROM projection p
                 LEFT JOIN commit_log c ON c.position = p.position WHERE c.position IS NULL",
            ),
            (
                "checkpoint",
                "SELECT COUNT(*) FROM checkpoint k
                 LEFT JOIN commit_log c ON c.position = k.position WHERE c.position IS NULL",
            ),
        ];
        for (what, sql) in joins {
            assert_eq!(
                number(connection, sql, rusqlite::params![]),
                0,
                "every {what} row names a commit",
            );
        }

        assert_eq!(
            number(
                connection,
                "SELECT COUNT(*) FROM (
                     SELECT command FROM commit_log GROUP BY command HAVING COUNT(*) > 1
                 )",
                rusqlite::params![],
            ),
            0,
            "every command id appears at most once",
        );
    }

    #[test]
    fn before_append_leaves_nothing_and_the_command_still_commits_once() {
        let scratch = Scratch::new("before-append");
        let stream = StreamId::new();
        let command = CommandId::from_uuid(stream.as_uuid());

        let crash = Crash::spawn("before-append", &scratch.db(), stream, 1, 0);
        assert_eq!(
            crash.marker(),
            "READY",
            "the helper reports readiness before writing",
        );
        drop(crash);

        let raw = Connection::open(scratch.db()).unwrap();
        assert_eq!(
            number(&raw, "SELECT COUNT(*) FROM commit_log", rusqlite::params![]),
            0,
            "no commit"
        );
        assert_eq!(
            number(&raw, "SELECT COUNT(*) FROM event", rusqlite::params![]),
            0,
            "no event"
        );
        assert_eq!(
            number(&raw, "SELECT COUNT(*) FROM outbox", rusqlite::params![]),
            0,
            "no outbox row"
        );
        assert_eq!(
            number(&raw, "SELECT COUNT(*) FROM stream", rusqlite::params![]),
            0,
            "no stream row"
        );
        assert_eq!(command_rows(&raw, command), 0, "nothing names the command");
        drop(raw);

        let mut store = Store::open(&scratch.db()).unwrap();
        let outcome = store
            .append(&Append::new(command, stream, Expected::New, new_event()))
            .unwrap();
        let Outcome::Committed(committed) = outcome else {
            panic!("the fresh append did not commit: {outcome:?}");
        };
        assert_eq!(committed.position().get(), 1);
        assert_eq!(committed.version(), Version::new(1));

        let raw = Connection::open(scratch.db()).unwrap();
        assert_eq!(
            command_rows(&raw, command),
            1,
            "the command committed exactly once"
        );
    }

    #[test]
    fn mid_append_rolls_the_inner_append_and_the_outer_consume_back() {
        let scratch = Scratch::new("mid-append");
        let stream = StreamId::new();
        let command = CommandId::from_uuid(stream.as_uuid());

        let crash = Crash::spawn("mid-append", &scratch.db(), stream, 2, 0);
        assert_eq!(
            crash.marker(),
            "IN-TRANSACTION",
            "the helper parks with the consume transaction open",
        );
        drop(crash);

        let raw = Connection::open(scratch.db()).unwrap();
        assert_eq!(
            number(&raw, "SELECT COUNT(*) FROM commit_log", rusqlite::params![]),
            1,
            "only the outer commit is durable"
        );
        assert_eq!(
            number(&raw, "SELECT COUNT(*) FROM event", rusqlite::params![]),
            2,
            "the outer commit's events survived"
        );
        assert_eq!(
            number(&raw, "SELECT COUNT(*) FROM stream", rusqlite::params![]),
            1,
            "the stream row survived"
        );
        assert_eq!(
            command_rows(&raw, command),
            1,
            "the outer command is the only one"
        );
        assert_eq!(
            number(&raw, "SELECT COUNT(*) FROM inbox", rusqlite::params![]),
            0,
            "the killed consume wrote no inbox row"
        );
        assert_eq!(
            number(&raw, "SELECT COUNT(*) FROM checkpoint", rusqlite::params![]),
            0,
            "the killed consume wrote no checkpoint"
        );
        assert_eq!(
            number(&raw, "SELECT COUNT(*) FROM projection", rusqlite::params![]),
            0,
            "the killed consume wrote no projection"
        );
        drop(raw);

        let mut store = Store::open(&scratch.db()).unwrap();
        let log = store.log(CommitPosition::FIRST, max(8)).unwrap();
        assert_eq!(log.len(), 1);
        let outer = &log[0];
        assert_eq!(outer.position().get(), 1);
        assert_eq!(outer.events().len(), 2);
        assert_eq!(
            outer
                .events()
                .iter()
                .map(|event| event.version().get())
                .collect::<Vec<_>>(),
            [0, 1],
        );

        let consumer = ConsumerId::new();
        let calls = Cell::new(0_usize);
        let first = store
            .consume(consumer, outer, |_| {
                calls.set(calls.get() + 1);
                Ok(())
            })
            .unwrap();
        let second = store
            .consume(consumer, outer, |_| {
                calls.set(calls.get() + 1);
                Ok(())
            })
            .unwrap();

        assert!(matches!(first, Processed::Applied(())));
        assert!(matches!(second, Processed::Duplicate));
        assert_eq!(
            calls.get(),
            1,
            "reprocessing the delivery applies exactly once"
        );
        assert_eq!(store.checkpoint(consumer).unwrap(), Some(outer.position()));
    }

    #[test]
    fn after_append_is_durable_and_a_retry_replays_at_the_same_position() {
        let scratch = Scratch::new("after-append");
        let stream = StreamId::new();
        let command = CommandId::from_uuid(stream.as_uuid());

        let crash = Crash::spawn("after-append", &scratch.db(), stream, 2, 0);
        let position = marker_position(&crash.marker());
        assert_eq!(position, 1);
        drop(crash);

        let mut store = Store::open(&scratch.db()).unwrap();
        let log = store.log(CommitPosition::FIRST, max(8)).unwrap();
        assert_eq!(log.len(), 1, "the committed append is present");
        assert_eq!(log[0].position().get(), position);
        assert_eq!(log[0].events().len(), 2);
        assert_eq!(
            log[0]
                .events()
                .iter()
                .map(|event| event.version().get())
                .collect::<Vec<_>>(),
            [0, 1],
        );

        let outcome = store
            .append(&Append::new(command, stream, Expected::Any, new_event()))
            .unwrap();
        let Outcome::Replayed(replayed) = outcome else {
            panic!("the retry did not replay: {outcome:?}");
        };
        assert_eq!(replayed.position().get(), position);
        assert_eq!(
            store.log(CommitPosition::FIRST, max(8)).unwrap().len(),
            1,
            "the replay added no commit",
        );

        let raw = Connection::open(scratch.db()).unwrap();
        assert_eq!(
            number(&raw, "SELECT COUNT(*) FROM event", rusqlite::params![]),
            2,
            "the replay added no event"
        );
        assert_eq!(
            command_rows(&raw, command),
            1,
            "the command is recorded once"
        );
    }

    #[test]
    fn a_killed_loop_leaves_the_log_self_consistent() {
        let scratch = Scratch::new("loop");
        let stream = StreamId::new();

        let crash = Crash::spawn("loop", &scratch.db(), stream, 1, 0);
        let marker = crash.marker();
        assert!(
            marker.starts_with("COMMITTED "),
            "the loop committed before the kill: {marker:?}",
        );
        thread::sleep(kill_delay());
        drop(crash);

        let raw = Connection::open(scratch.db()).unwrap();
        sweep(&raw);
        assert!(
            number(&raw, "SELECT COUNT(*) FROM commit_log", rusqlite::params![]) > 0,
            "the loop committed at least once",
        );
    }

    /// Several writers on one stream at the same time, each with a bounded amount of work. Every
    /// writer finishes, so the test is about what the interleaved commits left behind rather than
    /// about which process wins the single write lock.
    #[test]
    fn concurrent_writers_keep_positions_dense_and_versions_contiguous() {
        const WRITERS: usize = 4;
        const APPENDS: usize = 40;

        let scratch = Scratch::new("contention");
        let stream = StreamId::new();

        let mut writers = (0..WRITERS)
            .map(|_| Crash::spawn("loop", &scratch.db(), stream, 1, APPENDS))
            .collect::<Vec<_>>();
        for writer in &mut writers {
            let status = writer.finished();

            assert!(status.success(), "a writer exited with {status:?}");
        }

        let raw = Connection::open(scratch.db()).unwrap();
        sweep(&raw);
        let commits = u64::try_from(WRITERS * APPENDS).unwrap();
        assert_eq!(
            number(&raw, "SELECT COUNT(*) FROM commit_log", rusqlite::params![]),
            i64::try_from(commits).unwrap(),
            "every writer's appends committed",
        );
        assert_eq!(
            number(&raw, "SELECT version FROM stream", rusqlite::params![]),
            i64::try_from(commits).unwrap(),
            "the stream holds every writer's events",
        );
    }

    /// The kill lands at a seeded moment, so a failure from a timing-dependent write reproduces
    /// when `MOKMOK_CRASH_SEED` is set to the same value.
    fn kill_delay() -> Duration {
        let seed = std::env::var("MOKMOK_CRASH_SEED")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(0x5eed_u64);

        Duration::from_millis(50 + seed % 200)
    }
}

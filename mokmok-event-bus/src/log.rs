use std::collections::VecDeque;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use mokmok_protocol::EventID;
use uuid::Uuid;
use uuid::timestamp::{Timestamp, context::NoContext};

use crate::config::Retention;
use crate::event::{Batch, Event, Lapsed, LogEpoch, Position, Refusal, Seq, Window};
use crate::payload::Payload;
use crate::topic::Topic;

pub struct Log {
    epoch: LogEpoch,
    first: Seq,
    events: VecDeque<Arc<Event>>,
    bytes: usize,
    last_append: Instant,
    retention: Retention,
}

impl Log {
    /// Mints a fresh epoch per log: a reclaimed topic that is reopened is a different log, and
    /// a cursor naming the old incarnation must not resolve against the new one.
    pub fn new(retention: Retention) -> Self {
        Self {
            epoch: LogEpoch::new(Uuid::new_v4()),
            first: Seq::new(0),
            events: VecDeque::new(),
            bytes: 0,
            last_append: Instant::now(),
            retention,
        }
    }

    pub fn window(&self) -> Window {
        Window::new(
            self.epoch,
            self.first,
            Seq::new(self.first.get() + self.events.len() as u64),
        )
    }

    pub fn append(
        &mut self,
        topic: Topic,
        payload: Payload,
        at: SystemTime,
    ) -> (EventID, Seq) {
        let seq = Seq::new(self.first.get() + self.events.len() as u64);
        let id = EventID::new(Timestamp::now(NoContext));
        let bytes = payload.as_str().len();

        self.events
            .push_back(Arc::new(Event::new(id, topic, seq, at, payload)));
        self.bytes += bytes;
        self.last_append = Instant::now();
        self.evict();

        (id, seq)
    }

    fn evict(&mut self) {
        while self.events.len() > self.retention.events.get()
            || (self.events.len() > 1 && self.bytes > self.retention.bytes.get())
        {
            let Some(event) = self.events.pop_front() else {
                break;
            };
            self.bytes -= event.payload().as_str().len();
            self.first = Seq::new(self.first.get() + 1);
        }
    }

    pub fn take(
        &self,
        position: Position,
        max: NonZeroUsize,
    ) -> Result<Batch, Refusal> {
        let window = self.window();
        let (from, lapsed) = match position {
            Position::Beginning => (window.first(), None),
            Position::After(cursor)
                if cursor.epoch() != window.epoch() || cursor.seq() > window.next() =>
            {
                return Err(Refusal::Unresumable { window });
            },
            Position::After(cursor) if cursor.seq() < window.first() => {
                (window.first(), Some(Lapsed::new(cursor, window.first())))
            },
            Position::After(cursor) => (cursor.seq(), None),
        };
        let skip = from
            .offset_from(self.first)
            .ok_or(Refusal::Unresumable { window })?;
        let events = self
            .events
            .iter()
            .skip(skip)
            .take(max.get())
            .cloned()
            .collect();

        Ok(Batch::new(window, lapsed, events))
    }

    pub fn is_idle(
        &self,
        now: Instant,
        idle: Duration,
    ) -> bool {
        now.saturating_duration_since(self.last_append) >= idle
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::Log;
    use crate::config::Retention;
    use crate::event::{Batch, Cursor, LogEpoch, Position, Refusal, Seq};
    use crate::payload::Payload;
    use crate::topic::Topic;
    use std::num::NonZeroUsize;
    use std::time::{Duration, Instant, SystemTime};
    use uuid::Uuid;

    fn retention(
        events: usize,
        bytes: usize,
        idle: Duration,
    ) -> Retention {
        Retention {
            events: NonZeroUsize::new(events).unwrap(),
            bytes: NonZeroUsize::new(bytes).unwrap(),
            idle,
        }
    }

    fn max(events: usize) -> NonZeroUsize {
        NonZeroUsize::new(events).unwrap()
    }

    fn log(
        events: usize,
        bytes: usize,
    ) -> Log {
        Log::new(retention(events, bytes, Duration::from_secs(600)))
    }

    /// A payload whose raw JSON encoding is exactly `bytes` long; a JSON string is its text
    /// plus the two quotes around it.
    fn payload(bytes: usize) -> Payload {
        Payload::json(&"x".repeat(bytes - 2)).unwrap()
    }

    fn append(
        log: &mut Log,
        bytes: usize,
    ) -> u64 {
        let (_, seq) = log.append(Topic::Global, payload(bytes), SystemTime::UNIX_EPOCH);

        seq.get()
    }

    fn seqs(batch: &Batch) -> Vec<u64> {
        batch
            .events()
            .iter()
            .map(|event| event.seq().get())
            .collect()
    }

    #[test]
    fn beginning_reads_every_retained_event() {
        let mut log = log(4, 1 << 20);
        append(&mut log, 3);
        append(&mut log, 3);

        let batch = log.take(Position::Beginning, max(8)).unwrap();

        assert_eq!(seqs(&batch), [0, 1]);
        assert!(batch.lapsed().is_none());
        assert_eq!(batch.window().first().get(), 0);
        assert_eq!(batch.window().next().get(), 2);
        assert_eq!(batch.window().epoch(), log.window().epoch());
    }

    #[test]
    fn max_bounds_one_batch() {
        let mut log = log(4, 1 << 20);
        for _ in 0..4 {
            append(&mut log, 3);
        }

        let batch = log
            .take(Position::Beginning, NonZeroUsize::new(2).unwrap())
            .unwrap();

        assert_eq!(seqs(&batch), [0, 1]);
        assert_eq!(batch.window().next().get(), 4);
    }

    #[test]
    fn a_cursor_inside_the_window_reads_from_the_cursor() {
        let mut log = log(4, 1 << 20);
        for _ in 0..4 {
            append(&mut log, 3);
        }
        let cursor = Cursor::new(log.window().epoch(), Seq::new(2));

        let batch = log.take(Position::After(cursor), max(8)).unwrap();

        assert_eq!(seqs(&batch), [2, 3]);
        assert!(batch.lapsed().is_none());
        assert_eq!(
            batch.after(),
            Position::After(Cursor::new(batch.window().epoch(), Seq::new(4)))
        );
    }

    #[test]
    fn a_cursor_behind_the_window_reports_the_hole_it_skipped() {
        let mut log = log(2, 1 << 20);
        for _ in 0..5 {
            append(&mut log, 3);
        }
        let cursor = Cursor::new(log.window().epoch(), Seq::new(1));

        let batch = log.take(Position::After(cursor), max(8)).unwrap();

        let lapsed = batch.lapsed().unwrap();
        assert_eq!(lapsed.held().seq().get(), 1);
        assert_eq!(lapsed.from().get(), 3);
        assert_eq!(lapsed.missed(), 2);
        assert_eq!(seqs(&batch), [3, 4]);
    }

    #[test]
    fn a_cursor_from_a_foreign_epoch_is_unresumable() {
        let mut log = log(4, 1 << 20);
        append(&mut log, 3);
        let cursor = Cursor::new(LogEpoch::new(Uuid::new_v4()), Seq::new(0));

        let refusal = log.take(Position::After(cursor), max(8)).unwrap_err();

        assert_eq!(
            refusal,
            Refusal::Unresumable {
                window: log.window()
            }
        );
    }

    #[test]
    fn a_cursor_ahead_of_the_log_is_unresumable() {
        let mut log = log(4, 1 << 20);
        append(&mut log, 3);
        let cursor = Cursor::new(log.window().epoch(), Seq::new(2));

        let refusal = log.take(Position::After(cursor), max(8)).unwrap_err();

        assert_eq!(
            refusal,
            Refusal::Unresumable {
                window: log.window()
            }
        );
    }

    #[test]
    fn sequences_stay_contiguous_across_appends_and_evictions() {
        let mut log = log(2, 1 << 20);

        for expected in 0..5 {
            assert_eq!(append(&mut log, 3), expected);
        }

        assert_eq!(log.window().first().get(), 3);
        assert_eq!(log.window().next().get(), 5);
        assert_eq!(
            log.window().next().get() - log.window().first().get(),
            2,
            "the window counts the retained events",
        );
        assert_eq!(
            seqs(&log.take(Position::Beginning, max(8)).unwrap()),
            [3, 4]
        );
    }

    #[test]
    fn byte_retention_keeps_the_newest_event() {
        let mut log = log(4096, 4);
        append(&mut log, 3);
        append(&mut log, 3);
        append(&mut log, 5);

        let batch = log.take(Position::Beginning, max(8)).unwrap();

        assert_eq!(seqs(&batch), [2]);
        assert_eq!(log.window().first().get(), 2);
        assert_eq!(batch.events()[0].payload().as_str().len(), 5);
    }

    #[test]
    fn an_empty_log_reads_an_empty_batch() {
        let log = log(4, 1 << 20);

        let batch = log.take(Position::Beginning, max(8)).unwrap();

        assert!(batch.is_empty());
        assert!(batch.lapsed().is_none());
        assert_eq!(batch.window().first(), batch.window().next());
        assert_eq!(
            batch.after(),
            Position::After(Cursor::new(batch.window().epoch(), Seq::new(0))),
        );
        assert!(log.is_empty());
    }

    #[test]
    fn is_empty_tracks_the_retained_events() {
        let mut log = log(1, 1 << 20);
        assert!(log.is_empty());

        append(&mut log, 3);

        assert!(!log.is_empty());
    }

    #[test]
    fn is_idle_waits_out_the_idle_window() {
        let mut log = log(4, 1 << 20);
        append(&mut log, 3);
        let appended = Instant::now();

        assert!(!log.is_idle(appended, Duration::from_secs(600)));
        assert!(log.is_idle(
            appended + Duration::from_secs(600),
            Duration::from_secs(600)
        ));
        assert!(log.is_idle(appended, Duration::ZERO));
    }
}

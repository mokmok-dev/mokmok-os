//! The ports the outside world implements for an event-sourced store.

use std::fmt;
use std::hash::Hash;

use crate::cloud_event::CloudEvent;
use crate::stream::Version;

/// A stable identity for one stored event stream.
///
/// [`as_str`](Self::as_str) must be a non-empty, single path component of
/// characters that are safe on every supported filesystem, so an adapter uses it as
/// a file stem without escaping. [`parse_key`](Self::parse_key) is the inverse, and
/// an adapter that lists its streams needs it to recognise the stems it finds.
pub trait StreamKey: Clone + Eq + Hash + Send + Sync + 'static {
    fn as_str(&self) -> &str;

    /// Rebuilds a key from the text [`as_str`](Self::as_str) returned.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Invalid`] when `raw` is not the form this key writes.
    fn parse_key(raw: &str) -> Result<Self, StoreError>;
}

/// A durable, append-only log of one event stream per key.
///
/// Both adapters persist the same [`CloudEvent`] records; only the place they
/// keep them differs.
pub trait EventStore<K: StreamKey, E>: Send + Sync {
    /// The actor runtime calls this while it holds the stream's replay lock, so an
    /// implementation must not send a command back to that same stream's actor.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Io`] or [`StoreError::Corrupt`] when the log cannot be
    /// read or holds a record the store cannot trust.
    fn load(
        &self,
        key: &K,
    ) -> Result<Vec<CloudEvent<E>>, StoreError>;

    /// Commits the whole slice or nothing, returning the last committed version.
    ///
    /// `events` must be the records `expected.next()..` of `key`'s stream, which
    /// [`check_append`] verifies.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Conflict`] when the stream's version is not `expected`,
    /// and [`StoreError::Invalid`] when a record names another stream or skips a
    /// position.
    fn append(
        &self,
        key: &K,
        expected: Version,
        events: &[CloudEvent<E>],
    ) -> Result<Version, StoreError>;
}

/// An [`EventStore`] that can name every stream it holds.
///
/// A restart has to find the work an earlier process committed, and only the store
/// knows which streams its log contains.
pub trait EnumerableStore<K: StreamKey, E>: EventStore<K, E> {
    /// Returns every stream key, sorted by [`StreamKey::as_str`].
    ///
    /// An adapter that recognises its files by name skips an entry whose name is not a
    /// key, so one stray file cannot hide the healthy streams.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Io`] when the store cannot be listed.
    fn keys(&self) -> Result<Vec<K>, StoreError>;
}

/// Checks that `events` continue `expected` in `subject`, returning the last version.
///
/// Both adapters call this before persisting, so a caller cannot write a record that
/// belongs to another stream or leaves a gap in the sequence.
///
/// # Errors
///
/// Returns [`StoreError::Invalid`] when a record names another subject or does not
/// follow the previous version.
pub fn check_append<E>(
    subject: &str,
    expected: Version,
    events: &[CloudEvent<E>],
) -> Result<Version, StoreError> {
    let mut version = expected;
    for event in events {
        let next = version.next();
        if event.subject != subject {
            return Err(StoreError::Invalid(format!(
                "record for {:?} cannot enter stream {subject:?}",
                event.subject
            )));
        }
        if event.version != next {
            return Err(StoreError::Invalid(format!(
                "expected version {} but the record is {}",
                next.get(),
                event.version.get()
            )));
        }
        version = next;
    }
    Ok(version)
}

/// Checks a whole stored stream: one subject, versions `1..=n`, returning the last.
///
/// A replay uses this to reject a log a writer would never have produced. An empty
/// stream is well formed and stands at [`Version::ZERO`].
///
/// # Errors
///
/// Returns [`StoreError::Invalid`] when a record names another subject or the
/// versions are not contiguous from one.
pub fn validate_history<E>(
    subject: &str,
    events: &[CloudEvent<E>],
) -> Result<Version, StoreError> {
    check_append(subject, Version::ZERO, events)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StoreError {
    Conflict { expected: Version, actual: Version },
    Invalid(String),
    Io(String),
    Corrupt(String),
}

impl fmt::Display for StoreError {
    fn fmt(
        &self,
        formatter: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        match self {
            Self::Conflict { expected, actual } => write!(
                formatter,
                "append expected version {} but the stream is at {}",
                expected.get(),
                actual.get()
            ),
            Self::Invalid(message) => write!(formatter, "store refused: {message}"),
            Self::Io(message) => write!(formatter, "store i/o: {message}"),
            Self::Corrupt(message) => write!(formatter, "store is corrupt: {message}"),
        }
    }
}

impl std::error::Error for StoreError {}

#[cfg(test)]
mod tests {
    use crate::cloud_event::EventType;

    use super::*;

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct Tick(u64);

    impl EventType for Tick {
        fn event_type(&self) -> &'static str {
            "dev.mokmok.tick"
        }
    }

    fn event(
        subject: &str,
        version: u64,
    ) -> CloudEvent<Tick> {
        CloudEvent::new(subject, Version::new(version), Tick(version))
    }

    #[test]
    fn check_append_accepts_a_contiguous_slice() {
        let events = [event("s", 1), event("s", 2)];
        assert_eq!(
            check_append("s", Version::ZERO, &events),
            Ok(Version::new(2))
        );
    }

    #[test]
    fn check_append_rejects_another_subject() {
        let events = [event("other", 1)];
        assert!(matches!(
            check_append("s", Version::ZERO, &events),
            Err(StoreError::Invalid(_))
        ));
    }

    #[test]
    fn check_append_rejects_a_skipped_version() {
        let events = [event("s", 2)];
        assert!(matches!(
            check_append("s", Version::ZERO, &events),
            Err(StoreError::Invalid(_))
        ));
    }

    #[test]
    fn validate_history_of_an_empty_stream_is_zero() {
        assert_eq!(validate_history::<Tick>("s", &[]), Ok(Version::ZERO));
    }

    #[test]
    fn validate_history_accepts_a_contiguous_stream() {
        let events = [event("s", 1), event("s", 2), event("s", 3)];
        assert_eq!(validate_history("s", &events), Ok(Version::new(3)));
    }

    #[test]
    fn validate_history_rejects_another_subject() {
        let events = [event("s", 1), event("other", 2)];
        assert!(matches!(
            validate_history("s", &events),
            Err(StoreError::Invalid(_))
        ));
    }

    #[test]
    fn validate_history_rejects_a_gap() {
        let events = [event("s", 1), event("s", 3)];
        assert!(matches!(
            validate_history("s", &events),
            Err(StoreError::Invalid(_))
        ));
    }

    #[test]
    fn every_store_error_has_a_message() {
        let cases = [
            (
                StoreError::Conflict {
                    expected: Version::new(1),
                    actual: Version::new(3),
                },
                "expected version 1",
            ),
            (StoreError::Invalid("gap".to_owned()), "gap"),
            (StoreError::Io("disk".to_owned()), "disk"),
            (StoreError::Corrupt("line".to_owned()), "line"),
        ];
        for (error, expected) in cases {
            assert!(error.to_string().contains(expected));
        }
    }
}

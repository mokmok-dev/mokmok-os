//! The `JSONL` record format every adapter of [`EventStore`](crate::EventStore) shares.
//!
//! One stream is one object whose bytes are [`CloudEvent`] records, one JSON object per
//! line, with no header and no trailer. The functions here are the single home of that
//! format: an adapter supplies the bytes and the place it keeps them, and never its own
//! parser.

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::cloud_event::{CloudEvent, EventType};
use crate::port::{StoreError, StreamKey};
use crate::stream::Version;

/// The suffix an adapter appends to [`StreamKey::as_str`] to name a stream's object.
pub const OBJECT_SUFFIX: &str = ".jsonl";

/// The name of `key`'s stream object within an adapter's root.
#[must_use]
pub fn object_name(key: &str) -> String {
    format!("{key}{OBJECT_SUFFIX}")
}

/// The keys named by `names`, sorted by [`StreamKey::as_str`].
///
/// A name that does not carry [`OBJECT_SUFFIX`], does not parse as a key, or does not
/// spell that key canonically is skipped. A stray object therefore cannot hide the
/// streams an adapter does hold.
#[must_use]
pub fn canonical_keys<K: StreamKey>(names: impl IntoIterator<Item = impl AsRef<str>>) -> Vec<K> {
    let mut keys: Vec<K> = names
        .into_iter()
        .filter_map(|name| {
            let name = name.as_ref();
            let stem = name.strip_suffix(OBJECT_SUFFIX)?;
            let key = K::parse_key(stem).ok()?;
            (key.as_str() == stem).then_some(key)
        })
        .collect();
    keys.sort_by(|left, right| left.as_str().cmp(right.as_str()));
    keys
}

/// The bytes up to and including the last newline, which is the committed prefix.
///
/// A writer that died mid-record leaves bytes past that newline. They are not a record
/// any writer committed, so a reader ignores them and the next append drops them.
#[must_use]
pub fn complete_prefix(bytes: &[u8]) -> &[u8] {
    match bytes.iter().rposition(|byte| *byte == b'\n') {
        Some(index) => &bytes[..=index],
        None => &[],
    }
}

/// Appends `events` to `out` as one JSON line each.
///
/// # Errors
///
/// Returns [`StoreError::Io`] when a record cannot be encoded. A [`CloudEvent`] holds
/// only strings, byte vectors, and domain data that implements [`Serialize`], so this
/// names a bug rather than a caller mistake.
pub fn encode_records<E: Serialize>(
    events: &[CloudEvent<E>],
    out: &mut Vec<u8>,
) -> Result<(), StoreError> {
    for event in events {
        serde_json::to_writer(&mut *out, event)
            .map_err(|error| StoreError::Io(error.to_string()))?;
        out.push(b'\n');
    }
    Ok(())
}

/// Decodes a whole committed stream, checking every cross-field invariant.
///
/// `label` names the object in every error, so a reader sees which stream is damaged.
/// An empty stream is well formed and stands at [`Version::ZERO`].
///
/// # Errors
///
/// Returns [`StoreError::Corrupt`] when the bytes are not UTF-8, a line is not a
/// [`CloudEvent`], a record names another subject, a version is not contiguous from
/// one, a record's `type` disagrees with its data, or its `id` is not derived from the
/// subject and version. Records that a writer would not have produced are refused here
/// rather than replayed.
pub fn parse_records<E: EventType + DeserializeOwned>(
    label: &str,
    subject: &str,
    bytes: &[u8],
) -> Result<(Version, Vec<CloudEvent<E>>), StoreError> {
    if bytes.is_empty() {
        return Ok((Version::ZERO, Vec::new()));
    }
    let text = std::str::from_utf8(bytes)
        .map_err(|error| StoreError::Corrupt(format!("{label}: {error}")))?;
    let mut version = Version::ZERO;
    let mut events = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let event: CloudEvent<E> = serde_json::from_str(line)
            .map_err(|error| StoreError::Corrupt(format!("{label}: {error}")))?;
        let position = index + 1;
        let expected = version.next();
        if event.subject != subject {
            return Err(StoreError::Corrupt(format!(
                "{label}:{position}: record names stream {} instead of {subject}",
                event.subject
            )));
        }
        if event.version != expected {
            return Err(StoreError::Corrupt(format!(
                "{label}:{position}: expected version {}",
                expected.get()
            )));
        }
        // `CloudEvent::new` keeps the envelope and the data in step, so a line where
        // they disagree, or whose id is not the derived one, was not written by a store.
        if event.event_type != event.data.event_type() {
            return Err(StoreError::Corrupt(format!(
                "{label}:{position}: type {:?} does not match the record",
                event.event_type
            )));
        }
        let expected_id = format!("{subject}:{}", event.version.get());
        if event.id != expected_id {
            return Err(StoreError::Corrupt(format!(
                "{label}:{position}: id {:?} is not {expected_id:?}",
                event.id
            )));
        }
        version = event.version;
        events.push(event);
    }
    Ok((version, events))
}

#[cfg(test)]
mod tests {
    use serde::{Deserialize, Serialize};

    use super::*;

    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct Tick(u64);

    impl EventType for Tick {
        fn event_type(&self) -> &'static str {
            "dev.mokmok.tick"
        }
    }

    #[derive(Clone, Debug, Eq, Hash, PartialEq)]
    struct Id(String);

    impl StreamKey for Id {
        fn as_str(&self) -> &str {
            self.0.as_str()
        }

        fn parse_key(raw: &str) -> Result<Self, StoreError> {
            let canonical = raw.to_lowercase();
            if canonical != raw || raw.is_empty() {
                return Err(StoreError::Invalid(raw.to_owned()));
            }
            Ok(Self(raw.to_owned()))
        }
    }

    #[test]
    fn an_empty_stream_is_version_zero() {
        assert_eq!(
            parse_records::<Tick>("s.jsonl", "s", &[]),
            Ok((Version::ZERO, Vec::new()))
        );
    }

    #[test]
    fn bytes_that_are_not_utf8_are_corrupt() {
        assert!(matches!(
            parse_records::<Tick>("s.jsonl", "s", &[0xff]),
            Err(StoreError::Corrupt(_))
        ));
    }

    #[test]
    fn a_record_round_trips_through_encode_and_parse() {
        let events = [
            CloudEvent::new("s", Version::new(1), Tick(1)),
            CloudEvent::new("s", Version::new(2), Tick(2)),
        ];
        let mut bytes = Vec::new();
        encode_records(&events, &mut bytes).expect("records encode");
        assert_eq!(
            parse_records::<Tick>("s.jsonl", "s", &bytes),
            Ok((Version::new(2), events.to_vec()))
        );
    }

    #[test]
    fn an_object_name_carries_the_suffix() {
        assert_eq!(object_name("s"), "s.jsonl");
    }

    #[test]
    fn a_torn_tail_is_not_part_of_the_complete_prefix() {
        assert_eq!(complete_prefix(b"one\ntwo"), b"one\n");
        assert_eq!(complete_prefix(b"one\n"), b"one\n");
        assert_eq!(complete_prefix(b"one"), b"");
    }

    #[test]
    fn canonical_keys_skips_stray_and_non_canonical_names_and_sorts() {
        let names = ["b.jsonl", "not-a-key.txt", "A.jsonl", "a.jsonl", "x"];
        assert_eq!(
            canonical_keys::<Id>(names),
            vec![Id("a".to_owned()), Id("b".to_owned())]
        );
    }
}

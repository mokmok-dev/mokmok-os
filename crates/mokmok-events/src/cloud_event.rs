//! The `CloudEvents` record every adapter persists.
//!
//! The record uses the `CloudEvents` 1.0 JSON shape in structured mode. The mapping
//! from the store's terms is:
//!
//! - `subject` is the stream key.
//! - `sequence` is the one-based stream position, with `sequencetype` `Integer`.
//! - `data` is the domain event.
//! - `type` is the domain event's [`EventType`].
//! - `id` is `<subject>:<sequence>`, unique within [`SOURCE`].
//!
//! `time` is optional and left absent: the core reads no clock, and the adapters
//! add no data of their own.

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::stream::Version;

/// The `CloudEvents` `source` shared by every record this store produces.
pub const SOURCE: &str = "mokmok://eventstore";

/// The `CloudEvents` `specversion` of a stored record.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SpecVersion {
    #[default]
    #[serde(rename = "1.0")]
    V1_0,
}

/// The `CloudEvents` `sequencetype` of a stored record.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SequenceType {
    #[default]
    Integer,
}

/// One committed event, shaped as a `CloudEvents` 1.0 JSON record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloudEvent<E> {
    pub specversion: SpecVersion,
    pub id: String,
    pub source: String,
    #[serde(rename = "type")]
    pub event_type: String,
    pub subject: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<String>,
    #[serde(
        rename = "sequence",
        serialize_with = "serialize_sequence",
        deserialize_with = "deserialize_sequence"
    )]
    pub version: Version,
    #[serde(rename = "sequencetype")]
    pub sequencetype: SequenceType,
    pub data: E,
}

/// The `CloudEvents` `type` of one committed event.
pub trait EventType {
    /// A stable, reverse-DNS-prefixed name such as `dev.mokmok.job.submitted`.
    fn event_type(&self) -> &'static str;
}

impl<E: EventType> CloudEvent<E> {
    /// Builds the record for `data` at `version` in `subject`'s stream.
    #[must_use]
    pub fn new(
        subject: &str,
        version: Version,
        data: E,
    ) -> Self {
        Self {
            specversion: SpecVersion::V1_0,
            id: format!("{subject}:{}", version.get()),
            source: SOURCE.to_owned(),
            event_type: data.event_type().to_owned(),
            subject: subject.to_owned(),
            time: None,
            version,
            sequencetype: SequenceType::Integer,
            data,
        }
    }
}

// serde calls a `serialize_with` hook with a reference even for a `Copy` value.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn serialize_sequence<S>(
    version: &Version,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.serialize_str(&version.get().to_string())
}

fn deserialize_sequence<'de, D>(deserializer: D) -> Result<Version, D::Error>
where
    D: Deserializer<'de>,
{
    let raw = String::deserialize(deserializer)?;
    raw.parse::<u64>()
        .map(Version::new)
        .map_err(|_| D::Error::custom(format!("sequence {raw:?} is not an unsigned integer")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct Tick {
        n: u64,
    }

    impl EventType for Tick {
        fn event_type(&self) -> &'static str {
            "dev.mokmok.tick"
        }
    }

    #[test]
    fn a_record_carries_the_required_cloud_events_attributes() {
        let record = CloudEvent::new("stream-1", Version::new(3), Tick { n: 7 });
        let json = serde_json::to_value(&record).expect("record encodes");
        assert_eq!(json["specversion"], "1.0");
        assert_eq!(json["id"], "stream-1:3");
        assert_eq!(json["source"], SOURCE);
        assert_eq!(json["type"], "dev.mokmok.tick");
        assert_eq!(json["subject"], "stream-1");
        assert_eq!(json["sequence"], "3");
        assert_eq!(json["sequencetype"], "Integer");
        assert_eq!(json["data"]["n"], 7);
        assert!(json.get("time").is_none());
    }

    #[test]
    fn a_record_round_trips_through_json() {
        let record = CloudEvent::new("stream-1", Version::new(2), Tick { n: 5 });
        let json = serde_json::to_string(&record).expect("record encodes");
        let decoded: CloudEvent<Tick> = serde_json::from_str(&json).expect("record decodes");
        assert_eq!(decoded, record);
    }

    #[test]
    fn a_foreign_specversion_is_refused() {
        let json = r#"{"specversion":"0.3","id":"x","source":"s","type":"t","subject":"u","sequence":"1","sequencetype":"Integer","data":{"n":1}}"#;
        assert!(serde_json::from_str::<CloudEvent<Tick>>(json).is_err());
    }

    #[test]
    fn a_non_integer_sequence_is_refused() {
        let json = r#"{"specversion":"1.0","id":"x","source":"s","type":"t","subject":"u","sequence":"first","sequencetype":"Integer","data":{"n":1}}"#;
        assert!(serde_json::from_str::<CloudEvent<Tick>>(json).is_err());
    }

    #[test]
    fn another_sequence_type_is_refused() {
        let json = r#"{"specversion":"1.0","id":"x","source":"s","type":"t","subject":"u","sequence":"1","sequencetype":"String","data":{"n":1}}"#;
        assert!(serde_json::from_str::<CloudEvent<Tick>>(json).is_err());
    }
}

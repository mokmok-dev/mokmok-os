//! The envelope every message travels in, and the values it is built from.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use uuid::Uuid;

use crate::error::ProtocolError;
use crate::id::{
    AgentID, ArtifactID, CorrelationID, EventID, ExecutionID, MessageID, PrincipalID, SessionID,
    SpanID, TraceID, TurnID, WorkerID,
};
use crate::message::MessageKind;
use crate::moment::Moment;
use crate::version::{Compatibility, SchemaVersion};

/// The longest a [`Source`] may be, in bytes, so a log line can hold it without escaping or
/// truncation.
const MAX_SOURCE_BYTES: usize = 253;

/// The only `traceparent` version this crate reads.
const TRACEPARENT_VERSION: &str = "00";

/// The flag bit a `traceparent` uses to say the trace is sampled.
const SAMPLED_FLAG: u8 = 0x01;

/// Who produced a message.
///
/// The value has no whitespace and at most 253 bytes, so a log line can hold it and two producers
/// cannot differ only by how a space was written.
#[derive(Clone, Debug, Eq, Hash, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(transparent)]
pub struct Source(String);

impl Source {
    /// # Errors
    ///
    /// Returns [`ProtocolError::InvalidSource`] when `value` is empty, contains whitespace, or is
    /// longer than 253 bytes.
    pub fn new(value: impl Into<String>) -> Result<Self, ProtocolError> {
        let value = value.into();
        ensure_source(&value)?;
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Source {
    fn fmt(
        &self,
        formatter: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// The source rule [`Source::new`] and the decode path share, so a decoded source cannot carry what
/// a constructed one refuses.
fn ensure_source(value: &str) -> Result<(), ProtocolError> {
    if value.is_empty() || value.len() > MAX_SOURCE_BYTES || value.chars().any(char::is_whitespace)
    {
        Err(ProtocolError::InvalidSource)
    } else {
        Ok(())
    }
}

/// The media type a [`Data`] payload declares.
///
/// Only `application/json` exists this round, which is what lets [`Data::from_json`] promise its
/// bytes are UTF-8 and [`Data::to_json`] promise to read them.
#[derive(Clone, Debug, Eq, Hash, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(transparent)]
pub struct MediaType(String);

impl MediaType {
    /// The only media type this round defines.
    pub const JSON: &str = "application/json";

    /// # Errors
    ///
    /// Returns [`ProtocolError::InvalidMediaType`] for anything but [`MediaType::JSON`].
    pub fn new(value: impl Into<String>) -> Result<Self, ProtocolError> {
        let value = value.into();
        if value == Self::JSON {
            Ok(Self(value))
        } else {
            Err(ProtocolError::InvalidMediaType)
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn json() -> Self {
        Self(String::from(Self::JSON))
    }
}

impl fmt::Display for MediaType {
    fn fmt(
        &self,
        formatter: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// The bytes of a message and the media type that says how to read them.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct Data {
    media_type: MediaType,
    bytes: Vec<u8>,
}

impl Data {
    /// Encodes `value` as the payload, with the media type [`MediaType::JSON`].
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::Malformed`] when `value` cannot be encoded as JSON.
    pub fn json<T: serde::Serialize + ?Sized>(value: &T) -> Result<Self, ProtocolError> {
        serde_json::to_vec(value)
            .map(|bytes| Self {
                media_type: MediaType::json(),
                bytes,
            })
            .map_err(ProtocolError::Malformed)
    }

    /// Wraps bytes already known to be JSON.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::EmptyPayload`] for no bytes, and [`ProtocolError::InvalidData`] for
    /// bytes that are not UTF-8.
    pub fn from_json(bytes: impl Into<Vec<u8>>) -> Result<Self, ProtocolError> {
        let bytes = bytes.into();
        if bytes.is_empty() {
            return Err(ProtocolError::EmptyPayload);
        }
        if core::str::from_utf8(&bytes).is_err() {
            return Err(ProtocolError::InvalidData);
        }
        Ok(Self {
            media_type: MediaType::json(),
            bytes,
        })
    }

    #[must_use]
    pub const fn media_type(&self) -> &MediaType {
        &self.media_type
    }

    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Reads the payload back as `T`.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::Malformed`] when the payload is not JSON that `T` can read.
    pub fn to_json<T: serde::de::DeserializeOwned>(&self) -> Result<T, ProtocolError> {
        serde_json::from_slice(&self.bytes).map_err(ProtocolError::Malformed)
    }
}

/// The W3C `traceparent` a message carries, so one trace spans process boundaries.
///
/// Correlation ties the messages of one request and causation names the event that caused this
/// one; a trace context is neither, and is the only one of the three that a peer process forwards
/// unchanged.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct TraceContext {
    trace: TraceID,
    span: SpanID,
    sampled: bool,
}

impl TraceContext {
    /// Parses the W3C `traceparent` header `00-<32 hex>-<16 hex>-<2 hex>`.
    ///
    /// Only version `00` exists. The flags field's low bit is [`TraceContext::sampled`]; every
    /// other bit is ignored.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::InvalidData`] for a version other than `00`, a field count other
    /// than four, a field whose hex length is wrong, a non-hex character, or an all-zero trace id.
    pub fn parse(header: &str) -> Result<Self, ProtocolError> {
        let mut fields = header.split('-');
        let (Some(version), Some(trace), Some(span), Some(flags)) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            return Err(ProtocolError::InvalidData);
        };
        if fields.next().is_some() || version != TRACEPARENT_VERSION {
            return Err(ProtocolError::InvalidData);
        }
        let trace = parse_hex::<16>(trace)?;
        if trace == [0_u8; 16] {
            return Err(ProtocolError::InvalidData);
        }
        let span = parse_hex::<8>(span)?;
        let [flag] = parse_hex::<1>(flags)?;

        Ok(Self {
            trace: TraceID::from_uuid(Uuid::from_bytes(trace)),
            span: SpanID::from_uuid(span_uuid(span)),
            sampled: flag & SAMPLED_FLAG != 0,
        })
    }

    #[must_use]
    pub const fn trace(&self) -> TraceID {
        self.trace
    }

    #[must_use]
    pub const fn span(&self) -> SpanID {
        self.span
    }

    #[must_use]
    pub const fn sampled(&self) -> bool {
        self.sampled
    }
}

impl fmt::Display for TraceContext {
    fn fmt(
        &self,
        formatter: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        formatter.write_str(TRACEPARENT_VERSION)?;
        formatter.write_str("-")?;
        write_hex(formatter, self.trace.as_uuid().as_bytes())?;
        formatter.write_str("-")?;
        write_hex(formatter, &self.span.as_uuid().as_bytes()[8..])?;
        write!(formatter, "-{:02x}", u8::from(self.sampled))
    }
}

/// Reads exactly `2 * N` hex digits into `N` bytes, refusing any other length or digit.
fn parse_hex<const N: usize>(field: &str) -> Result<[u8; N], ProtocolError> {
    if field.len() != N * 2 {
        return Err(ProtocolError::InvalidData);
    }

    let mut bytes = [0_u8; N];
    for (index, [high, low]) in field.as_bytes().as_chunks::<2>().0.iter().enumerate() {
        let high = hex_nibble(*high).ok_or(ProtocolError::InvalidData)?;
        let low = hex_nibble(*low).ok_or(ProtocolError::InvalidData)?;
        bytes[index] = (high << 4) | low;
    }

    Ok(bytes)
}

/// The value of one lowercase hex digit, or `None` for anything else.
///
/// A `traceparent` is defined in lowercase; accepting the uppercase spelling too would give one id
/// two encodings, so only the defined one is read.
const fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 0x0a),
        _ => None,
    }
}

/// Packs an 8-byte W3C parent id into the low half of a [`SpanID`], whose other half stays zero.
fn span_uuid(span: [u8; 8]) -> Uuid {
    let mut bytes = [0_u8; 16];
    bytes[8..].copy_from_slice(&span);
    Uuid::from_bytes(bytes)
}

/// Writes `bytes` as lowercase hex, the form a `traceparent` field uses.
fn write_hex(
    formatter: &mut fmt::Formatter<'_>,
    bytes: &[u8],
) -> fmt::Result {
    for byte in bytes {
        write!(formatter, "{byte:02x}")?;
    }

    Ok(())
}

/// What a message is about.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, serde::Deserialize, serde::Serialize)]
pub enum Subject {
    Agent(AgentID),
    Session(SessionID),
    Turn(TurnID),
    Execution(ExecutionID),
    Artifact(ArtifactID),
    Principal(PrincipalID),
    Worker(WorkerID),
}

impl Subject {
    #[must_use]
    pub const fn as_uuid(&self) -> Uuid {
        match self {
            Self::Agent(id) => id.as_uuid(),
            Self::Session(id) => id.as_uuid(),
            Self::Turn(id) => id.as_uuid(),
            Self::Execution(id) => id.as_uuid(),
            Self::Artifact(id) => id.as_uuid(),
            Self::Principal(id) => id.as_uuid(),
            Self::Worker(id) => id.as_uuid(),
        }
    }
}

/// One message on the wire, whatever its [`MessageKind`].
///
/// An envelope carries the schema it was written at, so a reader can decide whether it may read
/// it, and the three optional chains stay distinct: `correlation` ties the peers of one request,
/// `causation` names the event that caused this one, and `trace` carries the distributed trace.
///
/// # Examples
///
/// ```
/// use mokmok_protocol::{
///     Data, Envelope, MessageID, MessageKind, Moment, PrincipalID, Source, Subject,
/// };
/// use uuid::Uuid;
///
/// # fn main() -> Result<(), mokmok_protocol::ProtocolError> {
/// let envelope = Envelope::new(
///     MessageID::from_uuid(Uuid::from_u128(1)),
///     Source::new("agent://planner")?,
///     MessageKind::Event,
///     Subject::Principal(PrincipalID::from_uuid(Uuid::from_u128(2))),
///     Moment::from_millis(1_700_000_000_000),
///     Data::json(&"turn opened")?,
/// );
///
/// let decoded = Envelope::from_json(&envelope.to_json()?)?;
/// assert_eq!(decoded.kind, MessageKind::Event);
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct Envelope {
    pub version: SchemaVersion,
    pub id: MessageID,
    pub source: Source,
    pub kind: MessageKind,
    pub subject: Subject,
    pub at: Moment,
    pub correlation: Option<CorrelationID>,
    pub causation: Option<EventID>,
    pub trace: Option<TraceContext>,
    pub data: Data,
}

impl Envelope {
    #[must_use]
    pub const fn new(
        id: MessageID,
        source: Source,
        kind: MessageKind,
        subject: Subject,
        at: Moment,
        data: Data,
    ) -> Self {
        Self {
            version: SchemaVersion::CURRENT,
            id,
            source,
            kind,
            subject,
            at,
            correlation: None,
            causation: None,
            trace: None,
            data,
        }
    }

    /// Ties this message to the peers of one logical request without claiming causation.
    #[must_use]
    pub const fn with_correlation(
        mut self,
        correlation: CorrelationID,
    ) -> Self {
        self.correlation = Some(correlation);
        self
    }

    /// Names the event that caused this message.
    #[must_use]
    pub const fn with_causation(
        mut self,
        causation: EventID,
    ) -> Self {
        self.causation = Some(causation);
        self
    }

    #[must_use]
    pub const fn with_trace(
        mut self,
        trace: TraceContext,
    ) -> Self {
        self.trace = Some(trace);
        self
    }

    /// # Errors
    ///
    /// Returns [`ProtocolError::Malformed`] when the envelope cannot be encoded as JSON.
    pub fn to_json(&self) -> Result<Vec<u8>, ProtocolError> {
        serde_json::to_vec(self).map_err(ProtocolError::Malformed)
    }

    /// Parses `bytes` with `serde` and then validates the result before returning it.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::Malformed`] when `bytes` is not an envelope, and the field errors
    /// [`ProtocolError::UnsupportedVersion`], [`ProtocolError::InvalidSource`],
    /// [`ProtocolError::InvalidMediaType`], and [`ProtocolError::EmptyPayload`] for a parsed
    /// envelope that breaks a rule.
    pub fn from_json(bytes: &[u8]) -> Result<Self, ProtocolError> {
        let envelope: Self = serde_json::from_slice(bytes).map_err(ProtocolError::Malformed)?;
        validate(&envelope)?;
        Ok(envelope)
    }
}

/// Checks a parsed envelope against the rules a producer's bytes cannot be trusted to hold.
///
/// Crate-internal: [`Envelope::from_json`] and [`crate::Command`] both route decoded bytes through
/// it, so neither path can skip a rule the other enforces.
///
/// # Errors
///
/// Returns the first rule the envelope breaks, in the order a caller would want to hear about it:
/// the version, then the source, the media type, and the payload.
pub fn validate(envelope: &Envelope) -> Result<(), ProtocolError> {
    if SchemaVersion::CURRENT.compatibility(envelope.version) == Compatibility::Unsupported {
        return Err(ProtocolError::UnsupportedVersion {
            seen: envelope.version,
            supported: SchemaVersion::CURRENT,
        });
    }
    ensure_source(envelope.source.as_str())?;
    if envelope.data.media_type.as_str() != MediaType::JSON {
        return Err(ProtocolError::InvalidMediaType);
    }
    if envelope.data.bytes.is_empty() {
        return Err(ProtocolError::EmptyPayload);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use alloc::format;
    use alloc::string::ToString;
    use alloc::vec;
    use alloc::vec::Vec;

    use proptest::prelude::*;
    use serde_json::json;
    use uuid::Uuid;

    use super::{Data, Envelope, MediaType, Source, Subject, TRACEPARENT_VERSION, TraceContext};
    use crate::error::ProtocolError;
    use crate::id::{
        AgentID, ArtifactID, CorrelationID, EventID, ExecutionID, MessageID, PrincipalID,
        SessionID, TurnID, WorkerID,
    };
    use crate::message::MessageKind;
    use crate::moment::Moment;
    use crate::version::SchemaVersion;

    const TRACE: &str = "4bf92f3577b34da6a3ce929d0e0e4736";
    const SPAN: &str = "00f067aa0ba902b7";

    fn envelope() -> Envelope {
        Envelope::new(
            MessageID::from_uuid(Uuid::from_u128(11)),
            Source::new("agent://planner").unwrap(),
            MessageKind::Event,
            Subject::Session(SessionID::from_uuid(Uuid::from_u128(12))),
            Moment::from_millis(1_700_000_000_000),
            Data::json(&"a payload").unwrap(),
        )
    }

    fn encoded(value: &serde_json::Value) -> Vec<u8> {
        serde_json::to_vec(value).unwrap()
    }

    #[test]
    fn a_source_is_non_empty_free_of_whitespace_and_at_most_253_bytes() {
        assert_eq!(Source::new(""), Err(ProtocolError::InvalidSource));
        assert_eq!(
            Source::new("agent planner"),
            Err(ProtocolError::InvalidSource),
        );
        assert_eq!(
            Source::new("agent\tplanner"),
            Err(ProtocolError::InvalidSource),
        );
        assert_eq!(
            Source::new("a".repeat(254)),
            Err(ProtocolError::InvalidSource),
        );
        assert_eq!(Source::new("a".repeat(253)).unwrap().as_str().len(), 253);
        assert_eq!(
            Source::new("agent://planner").unwrap().as_str(),
            "agent://planner",
        );
    }

    #[derive(Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
    struct Turn {
        number: u32,
        open: bool,
    }

    #[test]
    fn data_round_trips_a_literal_struct() {
        let turn = Turn {
            number: 7,
            open: true,
        };
        let data = Data::json(&turn).unwrap();

        assert_eq!(data.media_type().as_str(), "application/json");
        assert_eq!(data.bytes(), br#"{"number":7,"open":true}"#);
        assert_eq!(data.to_json::<Turn>().unwrap(), turn);
    }

    #[test]
    fn data_from_json_refuses_an_empty_payload_and_non_utf8_bytes() {
        assert_eq!(Data::from_json(""), Err(ProtocolError::EmptyPayload),);
        assert_eq!(
            Data::from_json(vec![0xff_u8, 0xfe_u8]),
            Err(ProtocolError::InvalidData),
        );
        assert_eq!(Data::from_json("{}").unwrap().bytes(), b"{}");
    }

    #[test]
    fn only_the_json_media_type_is_accepted() {
        assert_eq!(
            MediaType::new("text/plain"),
            Err(ProtocolError::InvalidMediaType),
        );
        assert_eq!(
            MediaType::new("application/json").unwrap().as_str(),
            "application/json",
        );
    }

    #[test]
    fn a_source_renders_as_its_value() {
        assert_eq!(
            Source::new("agent://planner").unwrap().to_string(),
            "agent://planner",
        );
    }

    #[test]
    fn the_json_media_type_renders_as_its_value() {
        assert_eq!(
            MediaType::new("application/json").unwrap().to_string(),
            "application/json",
        );
    }

    #[test]
    fn a_newer_minor_envelope_is_refused_with_both_versions() {
        let mut value = serde_json::to_value(envelope()).unwrap();
        value["version"]["minor"] = json!(1);

        assert_eq!(
            Envelope::from_json(&encoded(&value)),
            Err(ProtocolError::UnsupportedVersion {
                seen: SchemaVersion::new(1, 1),
                supported: SchemaVersion::CURRENT,
            }),
        );
    }

    #[test]
    fn another_major_envelope_is_refused_with_both_versions() {
        let mut value = serde_json::to_value(envelope()).unwrap();
        value["version"]["major"] = json!(2);

        assert_eq!(
            Envelope::from_json(&encoded(&value)),
            Err(ProtocolError::UnsupportedVersion {
                seen: SchemaVersion::new(2, 0),
                supported: SchemaVersion::CURRENT,
            }),
        );
    }

    #[test]
    fn a_decoded_envelope_with_an_invalid_source_is_refused() {
        let mut value = serde_json::to_value(envelope()).unwrap();
        value["source"] = json!("");

        assert_eq!(
            Envelope::from_json(&encoded(&value)),
            Err(ProtocolError::InvalidSource),
        );
    }

    #[test]
    fn a_decoded_envelope_with_another_media_type_is_refused() {
        let mut value = serde_json::to_value(envelope()).unwrap();
        value["data"]["media_type"] = json!("text/plain");

        assert_eq!(
            Envelope::from_json(&encoded(&value)),
            Err(ProtocolError::InvalidMediaType),
        );
    }

    #[test]
    fn a_decoded_envelope_with_an_empty_payload_is_refused() {
        let mut value = serde_json::to_value(envelope()).unwrap();
        value["data"]["bytes"] = json!([]);

        assert_eq!(
            Envelope::from_json(&encoded(&value)),
            Err(ProtocolError::EmptyPayload),
        );
    }

    #[test]
    fn bytes_that_are_not_json_are_malformed() {
        assert!(matches!(
            Envelope::from_json(b"{"),
            Err(ProtocolError::Malformed(_)),
        ));
    }

    #[test]
    fn a_traceparent_round_trips_through_parse_and_display() {
        let header = format!("00-{TRACE}-{SPAN}-01");
        let context = TraceContext::parse(&header).unwrap();

        assert_eq!(context.to_string(), header);
        assert!(context.sampled());
        assert_eq!(context, TraceContext::parse(&context.to_string()).unwrap());
        assert_eq!(
            format!("{:032x}", context.trace().as_uuid().as_u128()),
            TRACE,
        );
        let span = format!("{:016x}", context.span().as_uuid().as_u128());
        assert_eq!(span, SPAN);
    }

    #[test]
    fn a_traceparent_version_other_than_zero_zero_is_refused() {
        assert_eq!(
            TraceContext::parse(&format!("01-{TRACE}-{SPAN}-01")),
            Err(ProtocolError::InvalidData),
        );
    }

    #[test]
    fn a_traceparent_with_a_wrong_field_count_is_refused() {
        assert_eq!(TraceContext::parse(""), Err(ProtocolError::InvalidData));
        assert_eq!(
            TraceContext::parse(&format!("00-{TRACE}-{SPAN}")),
            Err(ProtocolError::InvalidData),
        );
        assert_eq!(
            TraceContext::parse(&format!("00-{TRACE}-{SPAN}-01-extra")),
            Err(ProtocolError::InvalidData),
        );
    }

    #[test]
    fn a_traceparent_field_of_the_wrong_hex_length_is_refused() {
        assert_eq!(
            TraceContext::parse(&format!("{TRACEPARENT_VERSION}-{}-{SPAN}-01", &TRACE[..30])),
            Err(ProtocolError::InvalidData),
        );
        assert_eq!(
            TraceContext::parse(&format!("00-{TRACE}-{}-01", &SPAN[..14])),
            Err(ProtocolError::InvalidData),
        );
        assert_eq!(
            TraceContext::parse(&format!("00-{TRACE}-{SPAN}-1")),
            Err(ProtocolError::InvalidData),
        );
    }

    #[test]
    fn a_traceparent_field_with_a_non_hex_character_is_refused() {
        assert_eq!(
            TraceContext::parse(&format!("00-{}g-{SPAN}-01", &TRACE[..31])),
            Err(ProtocolError::InvalidData),
        );
        assert_eq!(
            TraceContext::parse(&format!("00-{TRACE}-{SPAN}-0g")),
            Err(ProtocolError::InvalidData),
        );
    }

    #[test]
    fn an_all_zero_trace_id_is_refused() {
        assert_eq!(
            TraceContext::parse(&format!("00-{}-{SPAN}-01", "0".repeat(32))),
            Err(ProtocolError::InvalidData),
        );
    }

    #[test]
    fn only_the_low_flag_bit_is_sampled() {
        let context = TraceContext::parse(&format!("00-{TRACE}-{SPAN}-ff")).unwrap();

        assert!(context.sampled());
        assert_eq!(context.to_string(), format!("00-{TRACE}-{SPAN}-01"));
    }

    fn uuid_strategy() -> impl Strategy<Value = Uuid> {
        proptest::array::uniform16(any::<u8>()).prop_map(Uuid::from_bytes)
    }

    fn source_strategy() -> impl Strategy<Value = Source> {
        proptest::string::string_regex("[!-~]{1,200}")
            .expect("the source regex is valid")
            .prop_map(|value| Source::new(value).expect("printable ascii is a valid source"))
    }

    fn data_strategy() -> impl Strategy<Value = Data> {
        "[!-~]{1,120}".prop_map(|value| Data::from_json(value).expect("printable ascii is utf-8"))
    }

    fn kind_strategy() -> impl Strategy<Value = MessageKind> {
        prop_oneof![
            Just(MessageKind::Command),
            Just(MessageKind::Event),
            Just(MessageKind::Query),
            Just(MessageKind::Control),
            Just(MessageKind::Telemetry),
        ]
    }

    fn subject_strategy() -> impl Strategy<Value = Subject> {
        prop_oneof![
            uuid_strategy().prop_map(|id| Subject::Agent(AgentID::from_uuid(id))),
            uuid_strategy().prop_map(|id| Subject::Session(SessionID::from_uuid(id))),
            uuid_strategy().prop_map(|id| Subject::Turn(TurnID::from_uuid(id))),
            uuid_strategy().prop_map(|id| Subject::Execution(ExecutionID::from_uuid(id))),
            uuid_strategy().prop_map(|id| Subject::Artifact(ArtifactID::from_uuid(id))),
            uuid_strategy().prop_map(|id| Subject::Principal(PrincipalID::from_uuid(id))),
            uuid_strategy().prop_map(|id| Subject::Worker(WorkerID::from_uuid(id))),
        ]
    }

    fn trace_strategy() -> impl Strategy<Value = TraceContext> {
        (
            any::<u128>().prop_map(|raw| raw | 1),
            any::<u64>(),
            any::<u8>(),
        )
            .prop_map(|(trace, span, flags)| {
                let header = format!("00-{trace:032x}-{span:016x}-{flags:02x}");
                TraceContext::parse(&header).expect("the generated traceparent is well formed")
            })
    }

    fn envelope_strategy() -> impl Strategy<Value = Envelope> {
        (
            uuid_strategy(),
            source_strategy(),
            kind_strategy(),
            subject_strategy(),
            any::<u64>(),
            proptest::option::of(trace_strategy()),
            (
                proptest::option::of(uuid_strategy()),
                proptest::option::of(uuid_strategy()),
            ),
            data_strategy(),
        )
            .prop_map(
                |(id, source, kind, subject, at, trace, (correlation, causation), data)| {
                    let mut envelope = Envelope::new(
                        MessageID::from_uuid(id),
                        source,
                        kind,
                        subject,
                        Moment::from_millis(at),
                        data,
                    );
                    if let Some(correlation) = correlation {
                        envelope = envelope.with_correlation(CorrelationID::from_uuid(correlation));
                    }
                    if let Some(causation) = causation {
                        envelope = envelope.with_causation(EventID::from_uuid(causation));
                    }
                    if let Some(trace) = trace {
                        envelope = envelope.with_trace(trace);
                    }
                    envelope
                },
            )
    }

    proptest! {
        #[test]
        fn an_envelope_round_trips_through_json(envelope in envelope_strategy()) {
            let decoded = Envelope::from_json(&envelope.to_json().unwrap()).unwrap();

            prop_assert_eq!(decoded, envelope);
        }

        #[test]
        fn a_generated_traceparent_round_trips_through_parse_and_display(
            trace in any::<u128>().prop_map(|raw| raw | 1),
            span in any::<u64>(),
            flags in any::<u8>(),
        ) {
            let header = format!("00-{trace:032x}-{span:016x}-{flags:02x}");
            let context = TraceContext::parse(&header).unwrap();

            prop_assert_eq!(TraceContext::parse(&context.to_string()).unwrap(), context);
            prop_assert_eq!(context.sampled(), flags & 1 == 1);
        }
    }
}

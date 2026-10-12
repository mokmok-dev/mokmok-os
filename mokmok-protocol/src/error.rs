//! The typed failures a contract check can report.

use alloc::string::ToString;
use core::fmt;

use crate::version::SchemaVersion;

/// Why a message contract could not be built or decoded.
///
/// Every decode path returns one of these instead of panicking. The variant is the caller's
/// decision point: [`ProtocolError::UnsupportedVersion`] is a version negotiation, and the rest are
/// refusals.
#[derive(Debug)]
pub enum ProtocolError {
    /// A producer wrote a schema outside this reader's [`SchemaVersion::accepts`] policy.
    UnsupportedVersion {
        /// The version the producer's bytes declared.
        seen: SchemaVersion,
        /// The version this build reads.
        supported: SchemaVersion,
    },
    /// The bytes were not the JSON the contract describes.
    Malformed(serde_json::Error),
    /// A source is empty, contains whitespace, or is longer than 253 bytes.
    InvalidSource,
    /// The media type is not `application/json`.
    InvalidMediaType,
    /// The payload is not the shape the contract names, or a `traceparent` is malformed.
    InvalidData,
    /// The payload carries no bytes.
    EmptyPayload,
}

impl fmt::Display for ProtocolError {
    fn fmt(
        &self,
        formatter: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        match self {
            Self::UnsupportedVersion { seen, supported } => write!(
                formatter,
                "a producer at {}.{} is not supported by this reader at {}.{}",
                seen.major(),
                seen.minor(),
                supported.major(),
                supported.minor(),
            ),
            Self::Malformed(error) => write!(formatter, "not the message's JSON: {error}"),
            Self::InvalidSource => {
                formatter.write_str("a source is empty, contains whitespace, or exceeds 253 bytes")
            },
            Self::InvalidMediaType => formatter.write_str("only application/json is supported"),
            Self::InvalidData => {
                formatter.write_str("the data is not the shape the contract names")
            },
            Self::EmptyPayload => formatter.write_str("the payload is empty"),
        }
    }
}

impl core::error::Error for ProtocolError {}

/// Equality that a test can pin: `serde_json::Error` is neither `Clone` nor `PartialEq`, so the
/// [`ProtocolError::Malformed`] arm compares the rendered message instead.
impl PartialEq for ProtocolError {
    fn eq(
        &self,
        other: &Self,
    ) -> bool {
        match (self, other) {
            (
                Self::UnsupportedVersion { seen, supported },
                Self::UnsupportedVersion {
                    seen: other_seen,
                    supported: other_supported,
                },
            ) => seen == other_seen && supported == other_supported,
            (Self::Malformed(error), Self::Malformed(other_error)) => {
                error.to_string() == other_error.to_string()
            },
            (Self::InvalidSource, Self::InvalidSource)
            | (Self::InvalidMediaType, Self::InvalidMediaType)
            | (Self::InvalidData, Self::InvalidData)
            | (Self::EmptyPayload, Self::EmptyPayload) => true,
            _ => false,
        }
    }
}

impl Eq for ProtocolError {}

#[cfg(test)]
mod tests {
    use alloc::string::ToString;

    use super::ProtocolError;
    use crate::version::SchemaVersion;

    #[test]
    fn an_unsupported_version_error_names_both_versions() {
        let error = ProtocolError::UnsupportedVersion {
            seen: SchemaVersion::new(2, 0),
            supported: SchemaVersion::new(1, 3),
        };

        assert_eq!(
            error.to_string(),
            "a producer at 2.0 is not supported by this reader at 1.3",
        );
        assert_eq!(
            error,
            ProtocolError::UnsupportedVersion {
                seen: SchemaVersion::new(2, 0),
                supported: SchemaVersion::new(1, 3),
            },
        );
    }

    #[test]
    fn two_different_variants_are_not_equal() {
        assert_ne!(ProtocolError::InvalidSource, ProtocolError::EmptyPayload);
    }
}

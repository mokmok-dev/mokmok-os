//! A job's identity and its stream position.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::job::JobError;
use crate::port::StreamKey;

/// The stable identity of one durable job.
///
/// The id is a `UUIDv7`. Generate one per job and reuse it on retries: the id is
/// the idempotency key. A business key belongs in the submitted input.
///
/// [`from_u128`](Self::from_u128) builds an id from a fixed number and does not
/// promise a v7 timestamp.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct JobId(String);

impl JobId {
    /// # Errors
    ///
    /// Returns [`JobError::InvalidJobId`] when the raw value is not a UUID.
    pub fn parse(raw: impl Into<String>) -> Result<Self, JobError> {
        let raw = raw.into();
        Uuid::parse_str(&raw).map_or(Err(JobError::InvalidJobId(raw)), |uuid| {
            Ok(Self::from_uuid(uuid))
        })
    }

    #[must_use]
    pub fn from_uuid(uuid: Uuid) -> Self {
        Self(uuid.hyphenated().to_string())
    }

    /// Builds an id from a fixed number, for tests and externally allocated ids.
    ///
    /// The result carries no v7 timestamp.
    #[must_use]
    pub fn from_u128(value: u128) -> Self {
        Self::from_uuid(Uuid::from_u128(value))
    }

    #[must_use]
    pub const fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl TryFrom<String> for JobId {
    type Error = JobError;

    fn try_from(raw: String) -> Result<Self, Self::Error> {
        Self::parse(raw)
    }
}

impl From<JobId> for String {
    fn from(id: JobId) -> Self {
        id.0
    }
}

impl StreamKey for JobId {
    fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

/// A one-based position in one job's event stream.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct Version(u64);

impl Version {
    pub const ZERO: Self = Self(0);

    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0 + 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const V7: &str = "01890b0c-7f3a-7c2e-9f4a-2b1c3d4e5f60";

    #[test]
    fn job_id_parses_a_uuid() {
        let id = JobId::parse(V7).expect("literal is a uuid");
        assert_eq!(id.as_str(), V7);
    }

    #[test]
    fn job_id_canonicalizes_uppercase_and_compact_forms() {
        for raw in [
            V7.to_uppercase(),
            "01890b0c7f3a7c2e9f4a2b1c3d4e5f60".to_owned(),
            format!("urn:uuid:{V7}"),
        ] {
            let id = JobId::parse(raw).expect("the form is a uuid");
            assert_eq!(id.as_str(), V7);
        }
    }

    #[test]
    fn job_id_rejects_non_uuid_input() {
        for raw in ["bad id".to_owned(), ":".repeat(128), String::new()] {
            assert!(matches!(JobId::parse(raw), Err(JobError::InvalidJobId(_))));
        }
    }

    #[test]
    fn from_u128_is_a_fixed_id() {
        assert_eq!(
            JobId::from_u128(1).as_str(),
            "00000000-0000-0000-0000-000000000001"
        );
    }

    #[test]
    fn job_id_string_conversion_validates() {
        let id = JobId::try_from(V7.to_owned()).expect("literal is a uuid");
        assert_eq!(String::from(id), V7);
        assert!(matches!(
            JobId::try_from(String::new()),
            Err(JobError::InvalidJobId(_))
        ));
    }

    #[test]
    fn stream_key_exposes_the_job_id_text() {
        let id = JobId::from_u128(1);
        assert_eq!(
            StreamKey::as_str(&id),
            "00000000-0000-0000-0000-000000000001"
        );
    }

    #[test]
    fn version_counts_forward_from_zero() {
        assert_eq!(Version::ZERO.get(), 0);
        assert_eq!(Version::new(7).next().get(), 8);
        assert!(Version::ZERO < Version::new(1));
    }
}

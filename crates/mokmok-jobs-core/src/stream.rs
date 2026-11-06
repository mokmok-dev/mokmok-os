//! A job's identity and its stream position.

use serde::{Deserialize, Serialize};

use crate::job::JobError;

const MAX_JOB_ID_LEN: usize = 128;

/// The stable identity of one durable job.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct JobId(String);

impl JobId {
    /// # Errors
    ///
    /// Returns [`JobError::InvalidJobId`] when the raw value is empty, longer than
    /// 128 bytes, or holds a character outside ASCII alphanumerics and `- _ . :`.
    pub fn parse(raw: impl Into<String>) -> Result<Self, JobError> {
        let raw = raw.into();
        let allowed = raw
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'));
        if raw.is_empty() || raw.len() > MAX_JOB_ID_LEN || !allowed {
            return Err(JobError::InvalidJobId(raw));
        }
        Ok(Self(raw))
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

    #[test]
    fn job_id_accepts_the_allowed_alphabet() {
        for raw in ["a", "job-1", "a_b.c:d", &"x".repeat(MAX_JOB_ID_LEN)] {
            let id = JobId::parse(raw).expect("id is valid");
            assert_eq!(id.as_str(), raw);
        }
    }

    #[test]
    fn job_id_rejects_empty_long_and_foreign_input() {
        for raw in [
            String::new(),
            "x".repeat(MAX_JOB_ID_LEN + 1),
            "has space".to_owned(),
            "slash/here".to_owned(),
            "héllo".to_owned(),
        ] {
            assert!(matches!(JobId::parse(raw), Err(JobError::InvalidJobId(_))));
        }
    }

    #[test]
    fn job_id_string_conversion_validates() {
        let id = JobId::try_from("round-trip".to_owned()).expect("id is valid");
        assert_eq!(String::from(id), "round-trip");
        assert!(matches!(
            JobId::try_from(String::new()),
            Err(JobError::InvalidJobId(_))
        ));
    }

    #[test]
    fn version_counts_forward_from_zero() {
        assert_eq!(Version::ZERO.get(), 0);
        assert_eq!(Version::new(7).next().get(), 8);
        assert!(Version::ZERO < Version::new(1));
    }
}

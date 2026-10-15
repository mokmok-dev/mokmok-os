//! The second tool the model may reach for: a search over what the operator granted.

use crate::error::Error;

/// What one find answered, as the model reads it.
///
/// Unlike a tool's report there are no captured bytes and no references: the rendering is the whole
/// answer, so the observation is the only thing the record has to keep.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct FindReport {
    pub observation: String,
}

/// What a session that was granted no root answers with.
pub const NO_SEARCH_ROOT: &str = "no search root was granted, so no file can be found";

/// The second tool the model may reach for: reads, changes nothing.
///
/// Nothing here is reconciled after a crash: a find writes no intent and no resolution, because
/// running one again changes nothing.
pub trait Finder: Sync {
    /// Answers one question about the files the operator's granted roots hold.
    ///
    /// # Errors
    ///
    /// Returns the adapter's failure when no answer can be produced.
    fn find(
        &self,
        query: &str,
    ) -> Result<FindReport, Error>;
}

//! The edit tool: one patch, one file, and the two hashes that make a crash decidable.

use mokmok_protocol::ExecutionID;
use mokmok_state_machine::{Resolution, TurnOutcome};

use crate::error::Error;
use crate::record::{ArtifactRef, EditIntentRecord};

/// What one edit call carries.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EditCall {
    pub execution: ExecutionID,
    pub patch: String,
    pub expect: Option<String>,
}

/// What planning an edit produced.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Plan {
    /// The edit is ready to commit.
    Ready(Ready),
    /// The edit was refused before anything was written.
    Refused(String),
}

/// The prepared edit, holding the bytes it will write.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Ready {
    /// The file, as the patch's own labels name it.
    pub relative: String,
    /// The bytes the file holds.
    pub before: ArtifactRef,
    /// The bytes the patch produces.
    pub after: ArtifactRef,
    /// The bytes to write.
    pub bytes: Vec<u8>,
    /// Where each hunk landed.
    pub placement: String,
    /// The plan read no file because the patch creates it, so a commit that finds none writes one
    /// rather than treating the absence as a file that moved.
    pub creates: bool,
}

/// How one edit ended.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub enum EditOutcome {
    /// The bytes were written and renamed into place.
    Applied,
    /// The file held different bytes when commit read it.
    Stale { found: ArtifactRef },
    /// The edit was refused, and nothing was renamed onto the file it names.
    Refused { reason: String },
}

impl EditOutcome {
    /// The settlement this outcome records.
    #[must_use]
    pub const fn resolution(&self) -> Resolution {
        match self {
            Self::Applied => Resolution::Applied,
            Self::Stale { .. } | Self::Refused { .. } => Resolution::Refused,
        }
    }

    /// How this outcome reads as the turn's outcome.
    #[must_use]
    pub const fn outcome(&self) -> TurnOutcome {
        match self {
            Self::Applied => TurnOutcome::Completed,
            Self::Stale { .. } | Self::Refused { .. } => TurnOutcome::Failed,
        }
    }
}

/// The durable report of one edit, with the observation the model sees.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct EditReport {
    pub outcome: EditOutcome,
    pub observation: String,
}

/// The port the runtime drives an edit through.
pub trait Editor: Sync {
    /// Plans an edit: parse, check authority, apply in memory, and store the two images.
    ///
    /// # Errors
    ///
    /// Returns the adapter's own failure when the plan cannot be produced.
    fn plan(
        &self,
        call: &EditCall,
    ) -> Result<Plan, Error>;

    /// Commits a planned edit to the file it names.
    ///
    /// # Errors
    ///
    /// Returns the adapter's own failure when the outcome cannot be recorded.
    fn commit(
        &self,
        ready: &Ready,
    ) -> Result<EditReport, Error>;
}

/// Renders the observation the outcome a commit reached becomes.
#[must_use]
pub fn observation(
    relative: &str,
    before: &ArtifactRef,
    after: &ArtifactRef,
    outcome: &EditOutcome,
    placement: &str,
) -> String {
    match outcome {
        EditOutcome::Applied => format!(
            "edit applied to {relative}\n{} bytes (sha256 {}) became {} bytes (sha256 {})\n{placement}",
            before.bytes, before.hash, after.bytes, after.hash,
        ),
        EditOutcome::Stale { found } => format!(
            "the edit of {relative} was refused: the file changed since it was read, so nothing was written; it is {} bytes (sha256 {})",
            found.bytes, found.hash,
        ),
        EditOutcome::Refused { reason } => format!("the edit of {relative} was refused: {reason}"),
    }
}

/// Renders the observation a refusal decided before any byte was read becomes.
///
/// The reason is the adapter's own sentence, and it names the file whenever the patch got far
/// enough to name one.
#[must_use]
pub fn refused(reason: &str) -> String {
    format!("the edit was refused: {reason}")
}

/// Renders the warning for an edit whose outcome never committed.
#[must_use]
pub fn unproven(intent: &EditIntentRecord) -> String {
    format!(
        "the edit of {} was interrupted; the daemon does not know whether it was applied\n\
         the file held {} bytes (sha256 {}) before the edit and would hold {} bytes (sha256 {}) after it\n\
         check the file before editing it again",
        intent.relative,
        intent.before.bytes,
        intent.before.hash,
        intent.after.bytes,
        intent.after.hash,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::ArtifactRef;
    use mokmok_state_machine::{Resolution, TurnOutcome};

    fn artifact(byte: u8) -> ArtifactRef {
        ArtifactRef {
            hash: format!("{byte:02x}"),
            bytes: 1,
            truncated: false,
        }
    }

    #[test]
    fn applied_resolves_and_completes() {
        assert_eq!(EditOutcome::Applied.resolution(), Resolution::Applied,);
        assert_eq!(EditOutcome::Applied.outcome(), TurnOutcome::Completed,);
    }

    #[test]
    fn stale_and_refused_resolve_and_fail() {
        assert_eq!(
            EditOutcome::Stale { found: artifact(1) }.resolution(),
            Resolution::Refused,
        );
        assert_eq!(
            EditOutcome::Stale { found: artifact(1) }.outcome(),
            TurnOutcome::Failed,
        );
        assert_eq!(
            EditOutcome::Refused {
                reason: String::new()
            }
            .resolution(),
            Resolution::Refused,
        );
        assert_eq!(
            EditOutcome::Refused {
                reason: String::new()
            }
            .outcome(),
            TurnOutcome::Failed,
        );
    }

    #[test]
    fn observation_renders_the_applied_case() {
        let before = artifact(1);
        let after = artifact(2);
        let text = observation(
            "src/x.rs",
            &before,
            &after,
            &EditOutcome::Applied,
            "hunk 1 matched at line 3 (offset 0)",
        );

        assert!(text.contains("edit applied to src/x.rs"), "{text}");
        assert!(text.contains("1 bytes (sha256 01)"), "{text}");
        assert!(text.contains("1 bytes (sha256 02)"), "{text}");
        assert!(
            text.contains("hunk 1 matched at line 3 (offset 0)"),
            "{text}"
        );
    }

    #[test]
    fn observation_renders_the_stale_case() {
        let found = artifact(3);
        let text = observation(
            "src/x.rs",
            &artifact(1),
            &artifact(2),
            &EditOutcome::Stale {
                found: found.clone(),
            },
            "",
        );

        assert!(text.contains("the edit of src/x.rs was refused"), "{text}");
        assert!(text.contains(&format!("sha256 {}", found.hash)), "{text}");
    }

    #[test]
    fn observation_renders_the_refused_case() {
        let text = observation(
            "src/x.rs",
            &artifact(1),
            &artifact(2),
            &EditOutcome::Refused {
                reason: "no hunk matched".to_owned(),
            },
            "",
        );

        assert!(
            text.contains("the edit of src/x.rs was refused: no hunk matched"),
            "{text}"
        );
    }

    #[test]
    fn refused_renders_the_exact_sentence() {
        let text = refused("the patch named two files");

        assert_eq!(
            text,
            "the edit was refused: the patch named two files".to_owned(),
        );
    }

    #[test]
    fn unproven_names_both_hashes() {
        let intent = EditIntentRecord {
            turn: mokmok_protocol::TurnID::from_uuid(uuid::Uuid::nil()),
            execution: mokmok_protocol::ExecutionID::from_uuid(uuid::Uuid::nil()),
            patch: String::new(),
            expect: None,
            relative: "src/x.rs".to_owned(),
            before: artifact(1),
            after: artifact(2),
            placement: String::new(),
        };
        let text = unproven(&intent);

        assert!(
            text.contains("the edit of src/x.rs was interrupted"),
            "{text}"
        );
        assert!(text.contains("sha256 01"), "{text}");
        assert!(text.contains("sha256 02"), "{text}");
        assert!(
            text.contains("check the file before editing it again"),
            "{text}"
        );
    }
}

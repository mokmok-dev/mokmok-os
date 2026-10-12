//! The schema compatibility policy every contract in the workspace shares.

/// The schema a message was written at.
///
/// A new major is a breaking change; a new minor only adds. A reader compares a producer against
/// [`SchemaVersion::CURRENT`] through [`SchemaVersion::compatibility`] before it trusts any other
/// field of a decoded message.
#[derive(
    Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, serde::Deserialize, serde::Serialize,
)]
pub struct SchemaVersion {
    major: u16,
    minor: u16,
}

impl SchemaVersion {
    /// The schema this build writes.
    pub const CURRENT: Self = Self::new(1, 0);

    #[must_use]
    pub const fn new(
        major: u16,
        minor: u16,
    ) -> Self {
        Self { major, minor }
    }

    #[must_use]
    pub const fn major(self) -> u16 {
        self.major
    }

    #[must_use]
    pub const fn minor(self) -> u16 {
        self.minor
    }

    /// How this reader relates to a producer, decided in one place so no caller can invent a
    /// second policy.
    ///
    /// Any other major is [`Compatibility::Unsupported`], and so is a producer at a newer minor:
    /// this reader cannot know what a field it has never seen means.
    #[must_use]
    pub const fn compatibility(
        self,
        producer: Self,
    ) -> Compatibility {
        if self.major != producer.major || producer.minor > self.minor {
            Compatibility::Unsupported
        } else if producer.minor == self.minor {
            Compatibility::Current
        } else {
            Compatibility::Older
        }
    }

    /// Whether this reader may decode a producer's message at all.
    #[must_use]
    pub const fn accepts(
        self,
        producer: Self,
    ) -> bool {
        matches!(
            self.compatibility(producer),
            Compatibility::Current | Compatibility::Older
        )
    }
}

/// How a producer's [`SchemaVersion`] relates to a reader's.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Compatibility {
    /// Same major and minor.
    Current,
    /// Same major, older minor: every field the producer wrote is still defined.
    Older,
    /// A different major, or a newer minor.
    Unsupported,
}

#[cfg(test)]
mod tests {
    use super::{Compatibility, SchemaVersion};

    #[test]
    fn a_reader_accepts_its_own_version() {
        assert_eq!(
            SchemaVersion::new(1, 4).compatibility(SchemaVersion::new(1, 4)),
            Compatibility::Current,
        );
        assert!(SchemaVersion::new(1, 4).accepts(SchemaVersion::new(1, 4)));
    }

    #[test]
    fn a_newer_minor_is_refused_and_an_older_one_is_accepted() {
        let reader = SchemaVersion::new(1, 4);

        assert_eq!(
            reader.compatibility(SchemaVersion::new(1, 3)),
            Compatibility::Older,
        );
        assert!(reader.accepts(SchemaVersion::new(1, 3)));
        assert_eq!(
            reader.compatibility(SchemaVersion::new(1, 5)),
            Compatibility::Unsupported,
        );
        assert!(!reader.accepts(SchemaVersion::new(1, 5)));
    }

    #[test]
    fn any_other_major_is_unsupported_in_both_directions() {
        let reader = SchemaVersion::new(2, 3);

        assert_eq!(
            reader.compatibility(SchemaVersion::new(1, 0)),
            Compatibility::Unsupported,
        );
        assert_eq!(
            reader.compatibility(SchemaVersion::new(3, 0)),
            Compatibility::Unsupported,
        );
    }

    #[test]
    fn current_reports_the_major_the_workspace_writes() {
        assert_eq!(SchemaVersion::CURRENT.major(), 1);
        assert_eq!(SchemaVersion::CURRENT.minor(), 0);
    }
}

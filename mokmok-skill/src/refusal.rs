//! Why a file is not a skill: one variant per rule section 2 names.

use alloc::string::String;

use thiserror::Error;

/// Why a file is not a skill.
///
/// Shape variants name the line the scanner could not read. Field variants name the field and the
/// offending text or size: a name rule cannot name a line, because [`SkillName::new`] sees the text
/// alone. `Display` renders one sentence.
///
/// [`SkillName::new`]: crate::SkillName::new
#[derive(Debug, Error, Eq, PartialEq)]
pub enum Refusal {
    /// The file does not open with a `---` line.
    #[error("line {line}: the file does not open with a `---` fence")]
    NoFrontmatter {
        /// The 1-based line the fence should have started on.
        line: usize,
    },
    /// The file has no closing `---` line.
    #[error("the frontmatter has no closing `---` fence")]
    UnterminatedFrontmatter,
    /// The frontmatter region is larger than [`MAX_FRONTMATTER_BYTES`].
    ///
    /// [`MAX_FRONTMATTER_BYTES`]: crate::MAX_FRONTMATTER_BYTES
    #[error("frontmatter is {bytes} bytes, larger than the cap")]
    FrontmatterTooLarge {
        /// The size of the frontmatter region, in bytes.
        bytes: usize,
    },
    /// A continuation line starts with a tab.
    #[error("line {line}: a tab-indented continuation is outside the value subset")]
    TabIndented {
        /// The 1-based line of the tab.
        line: usize,
    },
    /// A top-level line is indented with no block scalar open.
    #[error("line {line}: an indented top-level line is outside the value subset")]
    IndentedLine {
        /// The 1-based line of the indentation.
        line: usize,
    },
    /// A top-level line is not a `key: value` pair.
    #[error("line {line}: `{text}` is not a `key: value` line")]
    NotAKey {
        /// The 1-based line of the offending text.
        line: usize,
        /// The line as it was read.
        text: String,
    },
    /// A value is outside the accepted subset.
    #[error("line {line}: `{field}` has a value outside the value subset: `{text}`")]
    InvalidValue {
        /// The field whose value could not be read.
        field: String,
        /// The 1-based line of the value.
        line: usize,
        /// The value as it was written.
        text: String,
    },
    /// A key naming a known field appears twice.
    #[error("line {line}: `{key}` appears twice")]
    DuplicateKey {
        /// The key that repeated.
        key: String,
        /// The 1-based line of the second occurrence.
        line: usize,
    },
    /// The `metadata` block nests deeper than one level.
    #[error("line {line}: metadata nests deeper than one level")]
    NestedMetadata {
        /// The 1-based line of the nested entry.
        line: usize,
    },
    /// A line inside the `metadata` block is not a `key: value` pair.
    #[error("line {line}: `{text}` is not a metadata entry")]
    InvalidMetadata {
        /// The 1-based line of the offending text.
        line: usize,
        /// The line as it was read.
        text: String,
    },
    /// A `metadata` entry has no value.
    #[error("line {line}: metadata `{field}` has no value")]
    EmptyMetadataValue {
        /// The metadata key with no value.
        field: String,
        /// The 1-based line of the key.
        line: usize,
    },
    /// A required field is absent.
    #[error("`{field}` is required")]
    MissingField {
        /// The absent field.
        field: &'static str,
    },
    /// A name is outside 1..=64 characters.
    #[error("name `{name}` is {chars} characters, outside 1..=64")]
    NameLength {
        /// The offending name.
        name: String,
        /// Its length in characters.
        chars: usize,
    },
    /// A name carries a character outside `[a-z0-9-]`.
    #[error("name `{name}` has a character outside `[a-z0-9-]`")]
    NameCharset {
        /// The offending name.
        name: String,
    },
    /// A name starts or ends with a hyphen.
    #[error("name `{name}` starts or ends with a hyphen")]
    NameEdgeHyphen {
        /// The offending name.
        name: String,
    },
    /// A name contains `--`.
    #[error("name `{name}` contains `--`")]
    NameDoubleHyphen {
        /// The offending name.
        name: String,
    },
    /// A name differs from the directory it was read from.
    #[error("name `{name}` differs from its directory `{directory}`")]
    NameDirectoryMismatch {
        /// The frontmatter name.
        name: String,
        /// The directory the file was read from.
        directory: String,
    },
    /// A description is empty.
    #[error("description is empty")]
    DescriptionEmpty,
    /// A description is longer than 1024 characters.
    #[error("description is {chars} characters, longer than 1024")]
    DescriptionLength {
        /// The description length in characters.
        chars: usize,
    },
    /// A license is empty.
    #[error("license is empty")]
    LicenseEmpty,
    /// A compatibility is outside 1..=500 characters.
    #[error("compatibility is {chars} characters, outside 1..=500")]
    CompatibilityLength {
        /// The compatibility length in characters.
        chars: usize,
    },
    /// The body is larger than [`MAX_INSTRUCTIONS_BYTES`].
    ///
    /// [`MAX_INSTRUCTIONS_BYTES`]: crate::MAX_INSTRUCTIONS_BYTES
    #[error("body is {bytes} bytes, larger than the cap")]
    InstructionsTooLarge {
        /// The size of the body, in bytes.
        bytes: usize,
    },
}

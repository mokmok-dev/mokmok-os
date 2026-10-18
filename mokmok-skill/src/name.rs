//! The skill-name rule, owned here so every entry point shares one answer.

use alloc::string::{String, ToString};
use core::fmt::{self, Display};

use crate::refusal::Refusal;

/// The largest name accepted, in characters.
const MAX_NAME_CHARS: usize = 64;

/// A validated skill name: the frontmatter `name` and its directory must both be this.
///
/// 1..=64 characters, every character in `[a-z0-9-]`, no leading or trailing hyphen, and no `--`.
/// `Deserialize` re-validates through `TryFrom`, so a name entering the runtime from any direction
/// is checked once.
#[derive(Clone, Debug, Eq, Hash, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(try_from = "String")]
pub struct SkillName(String);

impl SkillName {
    /// Validates `text` as a skill name.
    ///
    /// # Errors
    ///
    /// Returns a [`Refusal`] naming the offending text when `text` is empty, longer than 64
    /// characters, carries a character outside `[a-z0-9-]`, starts or ends with `-`, or holds `--`.
    pub fn new(text: &str) -> Result<Self, Refusal> {
        let chars = text.chars().count();
        if chars == 0 || chars > MAX_NAME_CHARS {
            return Err(Refusal::NameLength {
                name: text.to_string(),
                chars,
            });
        }
        if !text.chars().all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-'
        }) {
            return Err(Refusal::NameCharset {
                name: text.to_string(),
            });
        }
        if text.starts_with('-') || text.ends_with('-') {
            return Err(Refusal::NameEdgeHyphen {
                name: text.to_string(),
            });
        }
        if text.contains("--") {
            return Err(Refusal::NameDoubleHyphen {
                name: text.to_string(),
            });
        }
        Ok(Self(text.to_string()))
    }

    /// The name as it was written.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for SkillName {
    fn fmt(
        &self,
        formatter: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl TryFrom<String> for SkillName {
    type Error = Refusal;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(&value)
    }
}

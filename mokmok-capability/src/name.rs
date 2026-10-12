use alloc::string::String;
use core::fmt;

/// The longest name accepted, in bytes: the longest one label of a DNS name can be.
const MAX_BYTES: usize = 253;

/// A single-token name for a tool, a model, or a principal.
///
/// [`Resource::Path`](crate::Resource::Path) and [`Resource::Host`](crate::Resource::Host) carry a
/// plain `String` because the layer that enforces a decision resolves those names itself, while a
/// tool or a model is compared exactly as written. A name that is not one canonical token would
/// let a rule and the request it matches disagree about what the name says.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Name(String);

impl Name {
    /// # Errors
    ///
    /// Returns [`InvalidName`] when the name is empty, holds whitespace, or is longer than 253
    /// bytes.
    pub fn new(name: impl Into<String>) -> Result<Self, InvalidName> {
        let name = name.into();

        if name.is_empty() {
            return Err(InvalidName::Empty);
        }

        if name.chars().any(char::is_whitespace) {
            return Err(InvalidName::Whitespace);
        }

        if name.len() > MAX_BYTES {
            return Err(InvalidName::TooLong);
        }

        Ok(Self(name))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Name {
    fn fmt(
        &self,
        formatter: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Why a [`Name`] is refused.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvalidName {
    Empty,
    Whitespace,
    TooLong,
}

impl fmt::Display for InvalidName {
    fn fmt(
        &self,
        formatter: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        formatter.write_str(match self {
            Self::Empty => "a name cannot be empty",
            Self::Whitespace => "a name cannot hold whitespace",
            Self::TooLong => "a name cannot be longer than 253 bytes",
        })
    }
}

impl core::error::Error for InvalidName {}

impl serde::Serialize for Name {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

// Parsing runs the same check as construction: a name that arrived empty or with whitespace would
// otherwise be a value no rule could have named, which is a request that matches nothing by
// accident rather than by policy.
impl<'de> serde::Deserialize<'de> for Name {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::{InvalidName, MAX_BYTES, Name};
    use alloc::string::String;

    #[test]
    fn an_empty_name_is_refused() {
        assert_eq!(Name::new(""), Err(InvalidName::Empty));
    }

    #[test]
    fn a_name_holding_whitespace_is_refused() {
        assert_eq!(Name::new("write file"), Err(InvalidName::Whitespace));
    }

    #[test]
    fn a_name_longer_than_a_dns_label_is_refused() {
        let longest: String = core::iter::repeat_n('a', MAX_BYTES).collect();
        let overlong: String = core::iter::repeat_n('a', MAX_BYTES + 1).collect();

        assert_eq!(Name::new(overlong), Err(InvalidName::TooLong));
        assert_eq!(
            Name::new(longest).map(|name| name.as_str().len()),
            Ok(MAX_BYTES)
        );
    }
}

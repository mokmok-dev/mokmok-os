//! Removes a protected value from a line while leaving the identifiers that correlate it.

use std::borrow::Cow;

/// What a [`Rule`] writes where it removed a value.
const PLACEHOLDER: &str = "[redacted]";

/// One literal value a [`Redaction`] removes, and what it writes in its place.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct Rule {
    pattern: String,
    replacement: String,
}

impl Rule {
    /// A rule that replaces every occurrence of `pattern` with `"[redacted]"`.
    #[must_use]
    pub fn new(pattern: impl Into<String>) -> Self {
        Self::replacing(pattern, PLACEHOLDER)
    }

    /// A rule that replaces every occurrence of `pattern` with `replacement`.
    #[must_use]
    pub fn replacing(
        pattern: impl Into<String>,
        replacement: impl Into<String>,
    ) -> Self {
        Self {
            pattern: pattern.into(),
            replacement: replacement.into(),
        }
    }

    /// The literal value this rule removes.
    #[must_use]
    pub fn pattern(&self) -> &str {
        &self.pattern
    }

    /// What this rule writes in place of the value it removed.
    #[must_use]
    pub fn replacement(&self) -> &str {
        &self.replacement
    }
}

/// A rule that names no value, returned with the position of the offending rule.
///
/// An empty pattern cannot remove anything and would make every line look rewritten, so
/// [`Redaction::from_rules`] refuses it rather than accepting a rule that only spends work.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("redaction rule {index} names an empty value")]
pub struct InvalidRule {
    index: usize,
}

impl InvalidRule {
    /// The position in the input of the rule that named no value.
    #[must_use]
    pub const fn index(self) -> usize {
        self.index
    }
}

/// An ordered set of [`Rule`]s that removes protected values from a line.
///
/// `#[derive(Default)]` gives a redaction that removes nothing.
///
/// The rules apply in the order given, and each one rewrites the output of the rule before it.
/// Order therefore decides the result when two patterns overlap: an `"abc"` rule listed before an
/// `"abcd"` rule removes the prefix and leaves the longer pattern nothing to find, so a caller
/// that protects both must list the longer pattern first. The rest of the line, including the
/// trace and correlation identifiers, is untouched, which is what keeps a redacted line worth
/// keeping.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Redaction {
    rules: Vec<Rule>,
}

impl Redaction {
    /// A redaction that removes nothing, the default for a caller that has no policy yet.
    #[must_use]
    pub const fn none() -> Self {
        Self { rules: Vec::new() }
    }

    /// Builds a redaction that applies `rules` in the order given.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidRule`] naming the first rule whose pattern is empty.
    pub fn from_rules(rules: Vec<Rule>) -> Result<Self, InvalidRule> {
        rules
            .iter()
            .position(|rule| rule.pattern.is_empty())
            .map_or(Ok(Self { rules }), |index| Err(InvalidRule { index }))
    }

    /// The rules, in the order they apply.
    #[must_use]
    pub fn rules(&self) -> &[Rule] {
        &self.rules
    }

    /// `value` with every rule applied.
    ///
    /// Returns the input borrowed, without allocating, when no rule matches.
    #[must_use]
    pub fn apply<'a>(
        &self,
        value: &'a str,
    ) -> Cow<'a, str> {
        let mut redacted: Option<String> = None;
        for rule in &self.rules {
            let line = redacted.as_deref().unwrap_or(value);
            if !line.contains(rule.pattern.as_str()) {
                continue;
            }
            redacted = Some(line.replace(rule.pattern.as_str(), rule.replacement.as_str()));
        }
        redacted.map_or(Cow::Borrowed(value), Cow::Owned)
    }
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use super::{InvalidRule, Redaction, Rule};

    #[test]
    fn a_literal_secret_leaves_the_line_while_its_trace_id_stays() {
        let redaction = Redaction::from_rules(vec![Rule::new("hunter2")]).unwrap();

        assert_eq!(
            redaction
                .apply("principal=alice trace=4bf92f3577b34da6a3ce929d0e0e4736 secret=hunter2")
                .as_ref(),
            "principal=alice trace=4bf92f3577b34da6a3ce929d0e0e4736 secret=[redacted]",
        );
    }

    #[test]
    fn a_rule_that_matches_nothing_returns_the_line_unchanged() {
        let redaction = Redaction::from_rules(vec![Rule::new("hunter2")]).unwrap();

        let line = "principal=alice trace=4bf92f3577b34da6a3ce929d0e0e4736 secret=swordfish";

        assert!(matches!(redaction.apply(line), Cow::Borrowed(_)));
        assert_eq!(redaction.apply(line).as_ref(), line);
    }

    #[test]
    fn an_earlier_shorter_pattern_pre_empts_a_later_longer_one() {
        let redaction = Redaction::from_rules(vec![Rule::new("abc"), Rule::new("abcd")]).unwrap();

        assert_eq!(redaction.apply("abcd").as_ref(), "[redacted]d");
    }

    #[test]
    fn a_rule_that_names_no_value_is_refused_with_its_index() {
        let rules = vec![Rule::new("hunter2"), Rule::new("")];

        assert_eq!(Redaction::from_rules(rules), Err(InvalidRule { index: 1 }));
        assert_eq!(
            Redaction::from_rules(vec![Rule::new("")])
                .unwrap_err()
                .index(),
            0,
        );
    }
}

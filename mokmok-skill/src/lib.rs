//! Reads one `SKILL.md` into a validated skill, with no file opened.
//!
//! The crate owns one answer: what a `SKILL.md` may contain, and which files are not one. Two
//! stages divide that answer. `frontmatter::read` checks the file's shape — the `---` fences, the
//! value subset, and one level of `metadata` nesting. [`parse`] applies the field rules section 2
//! names. A key the spec does not name is skipped, value and all: nothing reads it.

#![no_std]

extern crate alloc;

#[cfg(test)]
extern crate std;

mod frontmatter;
mod name;
mod refusal;

pub use name::SkillName;
pub use refusal::Refusal;

/// Runs `work` on another thread and hands back its answer, failing when no answer arrives.
///
/// The scanner advances its line cursor only when a state consumes the line, so a state that
/// declines every line spins without ever returning and no assertion in a case that calls `parse`
/// would run. A deadline is what turns that failure to terminate into a failure. The flag makes
/// every later call fail at once, so a proptest shrinking search costs one deadline, not one per
/// attempt.
#[cfg(test)]
pub(crate) fn answered<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;
    use std::time::Duration;

    static STUCK: AtomicBool = AtomicBool::new(false);

    assert!(
        !STUCK.load(Ordering::Relaxed),
        "an earlier call never answered"
    );
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(work());
    });
    let Ok(answer) = receiver.recv_timeout(Duration::from_secs(2)) else {
        STUCK.store(true, Ordering::Relaxed);
        panic!("no answer within two seconds: the parse does not terminate");
    };
    answer
}

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};

/// The largest frontmatter region accepted, in bytes, between the two `---` fences.
pub const MAX_FRONTMATTER_BYTES: usize = 16 * 1024;

/// The largest body accepted, in bytes, after the closing `---`.
///
/// An oversized body is refused, never clipped: a clipped instruction is a different instruction.
pub const MAX_INSTRUCTIONS_BYTES: usize = 32 * 1024;

/// The largest `description` accepted, counted in Unicode characters after folding.
const MAX_DESCRIPTION_CHARS: usize = 1024;

/// The largest `compatibility` accepted, counted in Unicode characters.
const MAX_COMPATIBILITY_CHARS: usize = 500;

/// What the prompt advertises: the pair the model chooses a skill by.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct SkillSummary {
    /// The validated name, equal to the skill's directory.
    pub name: SkillName,
    /// The description, after a folded block has been joined.
    pub description: String,
}

/// One `SKILL.md`, parsed and validated against section 2.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct Skill {
    /// The name and description the prompt advertises.
    pub summary: SkillSummary,
    /// The optional license scalar.
    pub license: Option<String>,
    /// The optional compatibility scalar.
    pub compatibility: Option<String>,
    /// The `metadata` block, flattened to a string map.
    pub metadata: BTreeMap<String, String>,
    /// The optional `allowed-tools` scalar; shape-checked only, never enforced.
    pub allowed_tools: Option<String>,
    /// The body: everything after the closing `---` and its newline, verbatim.
    pub instructions: String,
}

/// Parses one `SKILL.md`, read from a directory whose name its `name` must equal.
///
/// Only the file's shape is checked by `frontmatter::read`; this function applies the field rules.
/// A key the spec does not name never reaches here — the reader skipped it, value and all — and a
/// duplicate key naming a known field was refused while reading.
///
/// # Errors
///
/// Returns a [`Refusal`] naming the field and the offending text or size when `name` is absent,
/// malformed, or differs from `directory`; when `description` is absent, empty, or over 1024
/// characters; when `compatibility` is over 500 characters; when `license` is empty; when the body
/// is over [`MAX_INSTRUCTIONS_BYTES`]; or, from `frontmatter::read`, when the file's shape is not
/// a skill.
pub fn parse(
    directory: &str,
    text: &str,
) -> Result<Skill, Refusal> {
    let raw = frontmatter::read(text)?;

    let name_value = field(&raw, "name").ok_or(Refusal::MissingField { field: "name" })?;
    let name = SkillName::new(&text_of(name_value))?;
    if name.as_str() != directory {
        return Err(Refusal::NameDirectoryMismatch {
            name: name.as_str().to_string(),
            directory: directory.to_string(),
        });
    }

    let description_value = field(&raw, "description").ok_or(Refusal::MissingField {
        field: "description",
    })?;
    let description = text_of(description_value);
    if description.is_empty() {
        return Err(Refusal::DescriptionEmpty);
    }
    let description_chars = description.chars().count();
    if description_chars > MAX_DESCRIPTION_CHARS {
        return Err(Refusal::DescriptionLength {
            chars: description_chars,
        });
    }

    let license = match field(&raw, "license") {
        None => None,
        Some(value) => {
            let text = text_of(value);
            if text.is_empty() {
                return Err(Refusal::LicenseEmpty);
            }
            Some(text)
        },
    };

    let compatibility = match field(&raw, "compatibility") {
        None => None,
        Some(value) => {
            let text = text_of(value);
            let chars = text.chars().count();
            if chars == 0 || chars > MAX_COMPATIBILITY_CHARS {
                return Err(Refusal::CompatibilityLength { chars });
            }
            Some(text)
        },
    };

    let allowed_tools = field(&raw, "allowed-tools").map(text_of);
    let metadata = raw.metadata.iter().cloned().collect();

    if raw.body.len() > MAX_INSTRUCTIONS_BYTES {
        return Err(Refusal::InstructionsTooLarge {
            bytes: raw.body.len(),
        });
    }

    Ok(Skill {
        summary: SkillSummary { name, description },
        license,
        compatibility,
        metadata,
        allowed_tools,
        instructions: raw.body,
    })
}

/// The field named `key`, if any. A known key cannot repeat, so a caller asking for one is handed
/// the only one; only an unknown key may appear more than once, and no caller asks for those.
fn field<'a>(
    raw: &'a frontmatter::Raw,
    key: &str,
) -> Option<&'a frontmatter::Value> {
    raw.fields
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value)
}

/// A field's text, whichever value form it holds.
fn text_of(value: &frontmatter::Value) -> String {
    match value {
        frontmatter::Value::Scalar(text) | frontmatter::Value::Block(text) => text.clone(),
    }
}

#[cfg(test)]
mod tests {
    use alloc::format;

    use super::*;

    /// [`crate::parse`], bounded. The shadowed name keeps every case below on the invariant the
    /// wrapper pins: a parse always answers.
    fn parse(
        directory: &str,
        text: &str,
    ) -> Result<Skill, Refusal> {
        let directory = directory.to_string();
        let text = text.to_string();
        crate::answered(move || crate::parse(&directory, &text))
    }

    fn skill(
        name: &str,
        description: &str,
    ) -> String {
        format!("---\nname: {name}\ndescription: {description}\n---\nbody\n")
    }

    #[test]
    fn a_parse_answers_for_every_shape_a_scan_can_enter() {
        // Each text drives the scanner into one of the states whose answer about consuming the
        // line decides the cursor's next step: a blank line, a skipped vendor value, a block
        // scalar ending on the key after it, and a metadata block ending on a top-level key. A
        // scanner that stopped advancing would never return from one of them, and the bounded
        // `parse` above is what turns that from a hanging run into this failure.
        for text in [
            "---\n\nname: s\ndescription: d\n---\nbody\n",
            "---\nname: s\ndescription: d\nvendor: [a]\n---\nbody\n",
            "---\nname: s\ndescription: >-\n  one\n  two\nlicense: MIT\n---\nbody\n",
            "---\nmetadata:\n  author: org\nname: s\ndescription: d\n---\nbody\n",
        ] {
            assert!(parse("s", text).is_ok(), "{text}");
        }
    }

    #[test]
    fn a_valid_skill_parses() {
        let text = "---\nname: pdf-processing\ndescription: Extract PDF text.\nlicense: Apache-2.0\ncompatibility: Requires uv\nmetadata:\n  author: example-org\nallowed-tools: Bash(git:*) Read\n---\nDo the thing.\n";
        let skill = parse("pdf-processing", text).unwrap();
        assert_eq!(skill.summary.name.as_str(), "pdf-processing");
        assert_eq!(skill.summary.description, "Extract PDF text.");
        assert_eq!(skill.license.as_deref(), Some("Apache-2.0"));
        assert_eq!(skill.compatibility.as_deref(), Some("Requires uv"));
        assert_eq!(
            skill.metadata.get("author").map(String::as_str),
            Some("example-org")
        );
        assert_eq!(skill.allowed_tools.as_deref(), Some("Bash(git:*) Read"));
        assert_eq!(skill.instructions, "Do the thing.\n");
    }

    #[test]
    fn a_name_is_required() {
        assert_eq!(
            parse("x", "---\ndescription: d\n---\nbody\n"),
            Err(Refusal::MissingField { field: "name" })
        );
    }

    #[test]
    fn a_description_is_required() {
        assert_eq!(
            parse("x", "---\nname: x\n---\nbody\n"),
            Err(Refusal::MissingField {
                field: "description"
            })
        );
    }

    #[test]
    fn a_name_outside_one_to_64_characters_is_refused() {
        let name = "a".repeat(65);
        assert_eq!(
            parse(&name, &skill(&name, "d")),
            Err(Refusal::NameLength { name, chars: 65 })
        );
    }

    #[test]
    fn a_name_with_a_character_outside_the_charset_is_refused() {
        assert_eq!(
            parse("Bad-Name", &skill("Bad-Name", "d")),
            Err(Refusal::NameCharset {
                name: "Bad-Name".to_string(),
            })
        );
    }

    #[test]
    fn a_name_with_a_leading_hyphen_is_refused() {
        assert_eq!(
            parse("-x", &skill("-x", "d")),
            Err(Refusal::NameEdgeHyphen {
                name: "-x".to_string(),
            })
        );
    }

    #[test]
    fn a_name_with_a_double_hyphen_is_refused() {
        assert_eq!(
            parse("a--b", &skill("a--b", "d")),
            Err(Refusal::NameDoubleHyphen {
                name: "a--b".to_string(),
            })
        );
    }

    #[test]
    fn a_name_that_differs_from_its_directory_is_refused() {
        assert_eq!(
            parse("directory-name", &skill("other-name", "d")),
            Err(Refusal::NameDirectoryMismatch {
                name: "other-name".to_string(),
                directory: "directory-name".to_string(),
            })
        );
    }

    #[test]
    fn a_display_name_with_uppercase_and_a_space_is_refused() {
        assert!(parse("poteto-mode", &skill("Poteto Mode", "d")).is_err());
    }

    #[test]
    fn an_empty_description_is_refused() {
        assert_eq!(
            parse("s", "---\nname: s\ndescription: \"\"\n---\nbody\n"),
            Err(Refusal::DescriptionEmpty)
        );
    }

    #[test]
    fn a_description_over_1024_characters_is_refused() {
        let description = "x".repeat(1025);
        assert_eq!(
            parse("s", &skill("s", &description)),
            Err(Refusal::DescriptionLength { chars: 1025 })
        );
    }

    #[test]
    fn a_description_of_exactly_1024_characters_is_accepted() {
        let description = "x".repeat(1024);
        let skill = parse("s", &skill("s", &description)).unwrap();
        assert_eq!(skill.summary.description.chars().count(), 1024);
    }

    #[test]
    fn an_empty_license_is_refused() {
        assert_eq!(
            parse(
                "s",
                "---\nname: s\ndescription: d\nlicense: \"\"\n---\nbody\n"
            ),
            Err(Refusal::LicenseEmpty)
        );
    }

    #[test]
    fn a_compatibility_over_500_characters_is_refused() {
        let compatibility = "x".repeat(501);
        let text =
            format!("---\nname: s\ndescription: d\ncompatibility: {compatibility}\n---\nbody\n");
        assert_eq!(
            parse("s", &text),
            Err(Refusal::CompatibilityLength { chars: 501 })
        );
    }

    #[test]
    fn an_oversize_body_is_refused_and_not_truncated() {
        let body = "x".repeat(MAX_INSTRUCTIONS_BYTES + 1);
        let text = format!("---\nname: s\ndescription: d\n---\n{body}");
        assert_eq!(
            parse("s", &text),
            Err(Refusal::InstructionsTooLarge {
                bytes: MAX_INSTRUCTIONS_BYTES + 1,
            })
        );
    }

    #[test]
    fn a_vendor_key_is_ignored() {
        let text = "---\nname: s\ndescription: d\ndisable-model-invocation: true\nicon: star\ncolor: blue\npaths: src\n---\nbody\n";
        let skill = parse("s", text).unwrap();
        assert_eq!(skill.summary.name.as_str(), "s");
        assert_eq!(skill.instructions, "body\n");
    }

    #[test]
    fn a_folded_description_is_joined_with_a_space() {
        let text = "---\nname: s\ndescription: >-\n  Extract PDF text,\n  fill forms, merge files.\n---\nbody\n";
        let skill = parse("s", text).unwrap();
        assert_eq!(
            skill.summary.description,
            "Extract PDF text, fill forms, merge files."
        );
    }

    #[test]
    fn a_folded_description_between_other_keys_still_parses() {
        // The shape real skills use: a folded description, then a scalar, then a folded
        // compatibility. A scanner that does not leave the block state when its last continuation
        // ends reads the key after it forever.
        let text = "---\nname: s\ndescription: >-\n  Extract PDF text,\n  fill forms, merge files.\nlicense: MIT\ncompatibility: >-\n  Requires uv.\n---\nbody\n";
        let skill = parse("s", text).unwrap();
        assert_eq!(
            skill.summary.description,
            "Extract PDF text, fill forms, merge files."
        );
        assert_eq!(skill.license.as_deref(), Some("MIT"));
        assert_eq!(skill.compatibility.as_deref(), Some("Requires uv."));
    }

    #[test]
    fn a_folded_compatibility_is_joined_with_a_space() {
        let text = "---\nname: s\ndescription: d\ncompatibility: >-\n  Requires uv\n  and Python.\n---\nbody\n";
        let skill = parse("s", text).unwrap();
        assert_eq!(
            skill.compatibility.as_deref(),
            Some("Requires uv and Python.")
        );
    }

    #[test]
    fn a_literal_block_keeps_its_newlines() {
        let text = "---\nname: s\ndescription: |-\n  one\n  two\n---\nbody\n";
        let skill = parse("s", text).unwrap();
        assert_eq!(skill.summary.description, "one\ntwo");
    }

    #[test]
    fn the_clip_chomp_keeps_one_trailing_newline() {
        let text = "---\nname: s\ndescription: |\n  one\n---\nbody\n";
        let skill = parse("s", text).unwrap();
        assert_eq!(skill.summary.description, "one\n");
    }

    #[test]
    fn a_quoted_description_keeps_an_escaped_quote() {
        let text = "---\nname: s\ndescription: \"say \\\"hi\\\"\"\n---\nbody\n";
        let skill = parse("s", text).unwrap();
        assert_eq!(skill.summary.description, "say \"hi\"");
    }

    #[test]
    fn a_single_quoted_description_keeps_a_doubled_quote() {
        let text = "---\nname: s\ndescription: 'it''s fine'\n---\nbody\n";
        let skill = parse("s", text).unwrap();
        assert_eq!(skill.summary.description, "it's fine");
    }

    #[test]
    fn metadata_entries_are_gathered_into_a_string_map() {
        let text = "---\nname: s\ndescription: d\nmetadata:\n  author: example-org\n  version: \"2\"\n---\nbody\n";
        let skill = parse("s", text).unwrap();
        assert_eq!(skill.metadata.len(), 2);
        assert_eq!(
            skill.metadata.get("author").map(String::as_str),
            Some("example-org")
        );
        assert_eq!(skill.metadata.get("version").map(String::as_str), Some("2"));
    }

    #[test]
    fn a_vendor_block_scalar_is_dropped() {
        let text = "---\nname: s\ndescription: d\nvendor: >-\n  some words\n---\nbody\n";
        let skill = parse("s", text).unwrap();
        assert_eq!(skill.summary.description, "d");
    }

    #[test]
    fn an_unknown_key_is_skipped_with_its_value() {
        let skill = parse(
            "s",
            "---\nname: s\ndescription: d\npaths: [\"**/*.ts\", \"**/*.tsx\"]\n---\nbody\n",
        )
        .unwrap();

        assert_eq!(skill.summary.description, "d");
        assert_eq!(skill.instructions, "body\n");
    }

    #[test]
    fn a_name_entering_a_decode_revalidates() {
        let decoded = serde_json::from_str::<SkillName>("\"good-name\"");
        assert_eq!(decoded.unwrap().as_str(), "good-name");

        let refused = serde_json::from_str::<SkillName>("\"Bad Name\"");
        assert!(refused.is_err());
    }

    #[test]
    fn a_name_displays_as_itself() {
        let name = SkillName::new("pdf-processing").unwrap();
        assert_eq!(name.to_string(), "pdf-processing");
    }

    #[test]
    fn a_compatibility_of_exactly_500_characters_is_accepted() {
        let compatibility = "x".repeat(500);
        let text =
            format!("---\nname: s\ndescription: d\ncompatibility: {compatibility}\n---\nbody\n");
        let skill = parse("s", &text).unwrap();
        assert_eq!(skill.compatibility.as_deref(), Some(compatibility.as_str()));
    }

    #[test]
    fn a_body_of_exactly_the_cap_is_accepted() {
        let body = "x".repeat(32 * 1024);
        let text = format!("---\nname: s\ndescription: d\n---\n{body}");
        let skill = parse("s", &text).unwrap();
        assert_eq!(skill.instructions.len(), 32 * 1024);
    }

    #[test]
    fn a_literal_block_keeps_every_trailing_newline() {
        // The `+` chomp keeps the trailing blank line, where `|` would strip it.
        let text = "---\nname: s\ndescription: |+\n  one\n\n---\nbody\n";
        let skill = parse("s", text).unwrap();
        assert_eq!(skill.summary.description, "one\n\n");
    }

    #[test]
    fn a_clip_chomp_strips_trailing_blank_lines() {
        let text = "---\nname: s\ndescription: |\n  one\n\n---\nbody\n";
        let skill = parse("s", text).unwrap();
        assert_eq!(skill.summary.description, "one\n");
    }

    #[test]
    fn an_all_blank_block_is_empty_plus_the_clip_newline() {
        let text = "---\nname: s\ndescription: |\n\n---\nbody\n";
        let skill = parse("s", text).unwrap();
        assert_eq!(skill.summary.description, "\n");
    }

    #[test]
    fn a_folded_block_joins_its_lines_with_a_space() {
        let text = "---\nname: s\ndescription: >\n  one\n  two\n---\nbody\n";
        let skill = parse("s", text).unwrap();
        assert_eq!(skill.summary.description, "one two\n");
    }

    #[test]
    fn a_folded_block_that_keeps_newlines_still_folds() {
        let text = "---\nname: s\ndescription: >+\n  one\n  two\n---\nbody\n";
        let skill = parse("s", text).unwrap();
        assert_eq!(skill.summary.description, "one two\n");
    }

    #[test]
    fn a_quoted_scalar_unescapes_a_backslash() {
        let text = "---\nname: s\ndescription: \"back\\\\slash\"\n---\nbody\n";
        let skill = parse("s", text).unwrap();
        assert_eq!(skill.summary.description, "back\\slash");
    }

    #[test]
    fn a_quoted_scalar_unescapes_a_newline() {
        let text = "---\nname: s\ndescription: \"two\\nlines\"\n---\nbody\n";
        let skill = parse("s", text).unwrap();
        assert_eq!(skill.summary.description, "two\nlines");
    }

    use proptest::prelude::*;

    proptest! {
        #[test]
        fn parse_never_panics_on_arbitrary_bytes(directory in "(?s).*", text in "(?s).*") {
            let _ = parse(&directory, &text);
        }

        #[test]
        fn a_parsed_name_always_equals_its_directory(directory in "(?s).*", text in "(?s).*") {
            if let Ok(skill) = parse(&directory, &text) {
                prop_assert_eq!(skill.summary.name.as_str(), directory.as_str());
            }
        }

        #[test]
        fn a_refused_oversize_body_is_never_truncated(extra in 1usize..2048) {
            let body = "x".repeat(MAX_INSTRUCTIONS_BYTES + extra);
            let text = format!("---\nname: s\ndescription: d\n---\n{body}");
            match parse("s", &text) {
                Err(Refusal::InstructionsTooLarge { bytes }) => prop_assert_eq!(bytes, body.len()),
                other => prop_assert!(false, "expected InstructionsTooLarge, got {:?}", other),
            }
        }

        #[test]
        fn a_decoded_name_revalidates(name in "(?s).*") {
            let encoded = serde_json::to_string(&name).unwrap();
            let decoded = serde_json::from_str::<SkillName>(&encoded);
            prop_assert_eq!(decoded.is_ok(), SkillName::new(&name).is_ok());
        }
    }
}

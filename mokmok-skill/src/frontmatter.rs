//! The `SKILL.md` frontmatter scanner: text in, shape-checked values out.
//!
//! This module owns the file's shape and nothing else. It checks the `---` fences, reads the value
//! subset section 2 names, gathers the one level of `metadata` entries, and refuses a duplicate key
//! naming a known field. It does not decide what a valid `name` or `description` is; [`read`]
//! returns a [`Raw`] and `crate::parse` applies the field rules.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::MAX_FRONTMATTER_BYTES;
use crate::refusal::Refusal;

/// A frontmatter value: a scalar written on one line, or a `|`/`>` block's joined text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Value {
    /// A scalar, possibly quoted, written on the key's own line.
    Scalar(String),
    /// A block scalar, joined and chomped.
    Block(String),
}

/// The shape of one file's frontmatter, before any field rule is applied.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Raw {
    /// Every field the spec names, in file order. A key the spec does not name is skipped.
    pub fields: Vec<(String, Value)>,
    /// The entries gathered from the one nested `metadata` block, in file order.
    pub metadata: Vec<(String, String)>,
    /// Everything after the closing `---` line and its newline, verbatim.
    pub body: String,
}

/// The keys section 2 names. Only these are refused for a duplicate.
const KNOWN_KEYS: [&str; 6] = [
    "name",
    "description",
    "license",
    "compatibility",
    "metadata",
    "allowed-tools",
];

/// Which line of the file the scanner is reading, and what that line is read for.
#[derive(Debug, Default, Eq, PartialEq)]
enum State {
    /// Before the opening `---`.
    #[default]
    Outside,
    /// After the closing `---`: the body.
    Body,
    /// A top-level field, or a blank line between fields.
    TopLevelKey,
    /// The continuation lines of a `|` or `>` block scalar, and the block being gathered.
    Block(Block),
    /// A `key: value` entry inside the `metadata` block.
    MetadataEntry,
    /// The line that must decide an empty `metadata` value: nothing, or one level too deep.
    MetadataValue {
        /// The key the empty entry named.
        field: String,
        /// The line the empty entry was written on.
        line: usize,
    },
    /// The lines under a key the spec does not name, consumed without being read.
    SkipValue,
}

/// Where a finished block scalar's value belongs.
///
/// Not the state to return to: a [`State`] that reads a block would send the line that ended this
/// block back to a reader with no block to read, which consumes nothing and leaves the state, so
/// the scan would never finish.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Home {
    /// A top-level field.
    Fields,
    /// An entry of the `metadata` block.
    Metadata,
}

/// How a block scalar treats its trailing newline.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Chomp {
    /// `-`: strip the trailing newline.
    Strip,
    /// No indicator: keep one trailing newline.
    Clip,
    /// `+`: keep every trailing newline.
    Keep,
}

/// A block scalar being gathered.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Block {
    key: String,
    folded: bool,
    chomp: Chomp,
    indent: Option<usize>,
    lines: Vec<String>,
    home: Home,
}

/// Reads one file's frontmatter and body into a shape-checked [`Raw`].
///
/// # Errors
///
/// Returns [`Refusal::NoFrontmatter`] if the file does not open with `---`,
/// [`Refusal::UnterminatedFrontmatter`] if it has no closing `---`,
/// [`Refusal::FrontmatterTooLarge`] if the region between the fences is over the cap, or a shape
/// refusal naming the line for a value outside the value subset, a duplicate known key, a nested
/// `metadata` map, or a tab-indented continuation.
pub fn read(text: &str) -> Result<Raw, Refusal> {
    let lines = split_lines(text);
    let mut scanner = Scanner::default();
    let mut frontmatter_start = 0usize;
    let mut body = String::new();
    let mut index = 0;

    while index < lines.len() {
        let (line, offset) = lines[index];
        let number = index + 1;

        if scanner.state == State::Body {
            body = text[offset..].to_string();
            break;
        }

        if line == "---" {
            if scanner.state == State::Outside {
                frontmatter_start = offset + line.len() + 1;
                scanner.state = State::TopLevelKey;
                index += 1;
                continue;
            }
            if matches!(scanner.state, State::Block(_)) {
                scanner.finish_block();
            }
            let bytes = offset.saturating_sub(frontmatter_start);
            if bytes > MAX_FRONTMATTER_BYTES {
                return Err(Refusal::FrontmatterTooLarge { bytes });
            }
            if let State::MetadataValue { field, line } = core::mem::take(&mut scanner.state) {
                return Err(Refusal::EmptyMetadataValue { field, line });
            }
            scanner.state = State::Body;
            index += 1;
            continue;
        }

        if scanner.state == State::Outside {
            return Err(Refusal::NoFrontmatter { line: number });
        }

        // A step that answers `false` left the line for the state it moved to, so the same line is
        // read again there rather than skipped. A step that answers `true` consumed it.
        if scanner.step(line, number)? {
            index += 1;
        }
    }

    match scanner.state {
        State::Body => {},
        State::Outside => return Err(Refusal::NoFrontmatter { line: 1 }),
        _ => return Err(Refusal::UnterminatedFrontmatter),
    }

    Ok(Raw {
        fields: scanner.fields,
        metadata: scanner.metadata,
        body,
    })
}

/// The scanner's mutable state, one state at a time.
#[derive(Debug, Default)]
struct Scanner {
    state: State,
    fields: Vec<(String, Value)>,
    metadata: Vec<(String, String)>,
    metadata_seen: bool,
}

impl Scanner {
    /// Reads one line in the current state. Returns whether the line was consumed.
    ///
    /// The state is taken for the step, so a state that carries a block or a pending key hands it to
    /// its reader rather than leaving a copy behind, and every reader writes the state the line
    /// after it is read in.
    fn step(
        &mut self,
        line: &str,
        number: usize,
    ) -> Result<bool, Refusal> {
        match core::mem::take(&mut self.state) {
            State::TopLevelKey => self.top_level(line, number),
            State::MetadataEntry => self.metadata_entry(line, number),
            State::Block(block) => self.block_scalar(block, line, number),
            State::MetadataValue { field, line: at } => {
                if line.trim().is_empty() {
                    self.state = State::MetadataValue { field, line: at };
                    return Ok(true);
                }
                if line.starts_with(' ') {
                    return Err(Refusal::NestedMetadata { line: number });
                }
                Err(Refusal::EmptyMetadataValue { field, line: at })
            },
            State::SkipValue => {
                if line.starts_with(' ') || line.starts_with('\t') {
                    self.state = State::SkipValue;
                    return Ok(true);
                }
                self.state = State::TopLevelKey;
                Ok(false)
            },
            state @ (State::Outside | State::Body) => {
                self.state = state;
                Ok(true)
            },
        }
    }

    /// One top-level `key: value` line, a blank line, or the start of a block scalar.
    fn top_level(
        &mut self,
        line: &str,
        number: usize,
    ) -> Result<bool, Refusal> {
        if line.trim().is_empty() {
            self.state = State::TopLevelKey;
            return Ok(true);
        }
        if line.starts_with('\t') {
            return Err(Refusal::TabIndented { line: number });
        }
        if line.starts_with(' ') {
            return Err(Refusal::IndentedLine { line: number });
        }

        let Some(colon) = line.find(':') else {
            return Err(Refusal::NotAKey {
                line: number,
                text: line.to_string(),
            });
        };
        let key = line[..colon].trim();
        if key.is_empty() || key.contains(char::is_whitespace) {
            return Err(Refusal::NotAKey {
                line: number,
                text: line.to_string(),
            });
        }
        let value = line[colon + 1..].trim();

        // A key the spec does not name is skipped, value and all: nothing reads it, so its shape is
        // the vendor's business, and refusing a skill over a field no reader consults would drop the
        // operator's skill for a reason the skill cannot fix.
        if !KNOWN_KEYS.contains(&key) {
            self.state = State::SkipValue;
            return Ok(true);
        }

        let duplicate = self.fields.iter().any(|(seen, _)| seen == key)
            || (key == "metadata" && self.metadata_seen);
        if duplicate {
            return Err(Refusal::DuplicateKey {
                key: key.to_string(),
                line: number,
            });
        }

        if key == "metadata" {
            self.metadata_seen = true;
            if value.is_empty() {
                self.state = State::MetadataEntry;
                return Ok(true);
            }
            return Err(Refusal::InvalidValue {
                field: key.to_string(),
                line: number,
                text: value.to_string(),
            });
        }

        if let Some((folded, chomp)) = block_indicator(value) {
            self.state = State::Block(Block {
                key: key.to_string(),
                folded,
                chomp,
                indent: None,
                lines: Vec::new(),
                home: Home::Fields,
            });
            return Ok(true);
        }

        let scalar = parse_scalar(value).ok_or_else(|| Refusal::InvalidValue {
            field: key.to_string(),
            line: number,
            text: value.to_string(),
        })?;
        self.fields.push((key.to_string(), Value::Scalar(scalar)));
        self.state = State::TopLevelKey;
        Ok(true)
    }

    /// One continuation line of a block scalar, or the line that ends it.
    fn block_scalar(
        &mut self,
        mut block: Block,
        line: &str,
        number: usize,
    ) -> Result<bool, Refusal> {
        if line.is_empty() {
            block.lines.push(String::new());
            self.state = State::Block(block);
            return Ok(true);
        }
        if line.starts_with('\t') {
            return Err(Refusal::TabIndented { line: number });
        }

        let indent = line.len() - line.trim_start_matches(' ').len();
        let indented = line.starts_with(' ');
        let ends = !indented || block.indent.is_some_and(|base| indent < base);
        if ends {
            self.place_block(block);
            return Ok(false);
        }

        // The first continuation line fixes the block's indentation, so a later line indented
        // less than it ends the block instead of being read as part of the value.
        let base = *block.indent.get_or_insert(indent);
        block.lines.push(line[base..].to_string());
        self.state = State::Block(block);
        Ok(true)
    }

    /// One `key: value` line inside the `metadata` block, or the line that ends the block.
    fn metadata_entry(
        &mut self,
        line: &str,
        number: usize,
    ) -> Result<bool, Refusal> {
        if line.trim().is_empty() {
            self.state = State::MetadataEntry;
            return Ok(true);
        }
        if line.starts_with('\t') {
            return Err(Refusal::TabIndented { line: number });
        }
        if !line.starts_with(' ') {
            self.state = State::TopLevelKey;
            return Ok(false);
        }

        let entry = line.trim_start_matches(' ');
        let Some(colon) = entry.find(':') else {
            return Err(Refusal::InvalidMetadata {
                line: number,
                text: line.to_string(),
            });
        };
        let key = entry[..colon].trim();
        if key.is_empty() || key.contains(char::is_whitespace) {
            return Err(Refusal::InvalidMetadata {
                line: number,
                text: line.to_string(),
            });
        }

        // Only a pushed entry can repeat here: an entry written with no value moves to
        // `MetadataValue`, which refuses the next indented line rather than reading another entry,
        // so `pending_empty` is always `None` by the time a second entry is read.
        let repeated = self.metadata.iter().any(|(seen, _)| seen == key);
        if repeated {
            return Err(Refusal::DuplicateKey {
                key: key.to_string(),
                line: number,
            });
        }

        let value = entry[colon + 1..].trim();
        if value.is_empty() {
            self.state = State::MetadataValue {
                field: key.to_string(),
                line: number,
            };
            return Ok(true);
        }

        if let Some((folded, chomp)) = block_indicator(value) {
            self.state = State::Block(Block {
                key: key.to_string(),
                folded,
                chomp,
                indent: None,
                lines: Vec::new(),
                home: Home::Metadata,
            });
            return Ok(true);
        }

        let scalar = parse_scalar(value).ok_or_else(|| Refusal::InvalidMetadata {
            line: number,
            text: line.to_string(),
        })?;
        self.metadata.push((key.to_string(), scalar));
        self.state = State::MetadataEntry;
        Ok(true)
    }

    /// Puts a finished block scalar's value where its key belongs.
    fn place_block(
        &mut self,
        block: Block,
    ) {
        let rendered = render_block(&block);
        match block.home {
            Home::Fields => {
                self.fields.push((block.key, Value::Block(rendered)));
                self.state = State::TopLevelKey;
            },
            Home::Metadata => {
                self.metadata.push((block.key, rendered));
                self.state = State::MetadataEntry;
            },
        }
    }

    /// Puts the open block scalar's value where its key belongs, and leaves no block open.
    ///
    /// The caller that ends a block on a line it did not consume re-reads that line in the state
    /// this answers. Leaving a block open would dispatch that line to `block_scalar` again with
    /// nothing to read, so the scan would never finish.
    fn finish_block(&mut self) {
        if let State::Block(block) = core::mem::take(&mut self.state) {
            self.place_block(block);
        }
    }
}

/// The `|`/`>` indicator, if `value` is one: folded or literal, and how it chomps.
fn block_indicator(value: &str) -> Option<(bool, Chomp)> {
    match value {
        "|" => Some((false, Chomp::Clip)),
        "|-" => Some((false, Chomp::Strip)),
        "|+" => Some((false, Chomp::Keep)),
        ">" => Some((true, Chomp::Clip)),
        ">-" => Some((true, Chomp::Strip)),
        ">+" => Some((true, Chomp::Keep)),
        _ => None,
    }
}

/// Joins and chomps a gathered block scalar.
///
/// A folded block joins its lines with a space rather than a newline; only the `|` form keeps the
/// line breaks the file wrote. `-` drops the trailing newline, and no indicator or `+` keeps it.
fn render_block(block: &Block) -> String {
    let separator = if block.folded { " " } else { "\n" };
    let mut end = block.lines.len();
    if block.chomp != Chomp::Keep {
        while end > 0 && block.lines[end - 1].is_empty() {
            end -= 1;
        }
    }
    let mut text = block.lines[..end].join(separator);
    if block.chomp != Chomp::Strip {
        text.push('\n');
    }
    text
}

/// Reads the value subset: a plain, single-quoted, or double-quoted scalar.
fn parse_scalar(raw: &str) -> Option<String> {
    if raw.is_empty() {
        return None;
    }
    if let Some(inner) = raw.strip_prefix('\'') {
        let inner = inner.strip_suffix('\'')?;
        return Some(inner.replace("''", "'"));
    }
    if let Some(inner) = raw.strip_prefix('"') {
        let inner = inner.strip_suffix('"')?;
        return unescape(inner);
    }
    if matches!(
        raw.as_bytes()[0],
        b'[' | b'{' | b'&' | b'*' | b'!' | b'|' | b'>'
    ) {
        return None;
    }
    Some(raw.to_string())
}

/// The escapes the value subset allows inside a double-quoted scalar.
fn unescape(inner: &str) -> Option<String> {
    let mut out = String::with_capacity(inner.len());
    let mut characters = inner.chars();
    while let Some(character) = characters.next() {
        if character == '\\' {
            match characters.next()? {
                '"' => out.push('"'),
                '\\' => out.push('\\'),
                'n' => out.push('\n'),
                _ => return None,
            }
        } else if character == '"' {
            return None;
        } else {
            out.push(character);
        }
    }
    Some(out)
}

/// Every line and the byte offset it starts at. A trailing newline adds no empty final line.
fn split_lines(text: &str) -> Vec<(&str, usize)> {
    let mut lines = Vec::new();
    let mut offset = 0;
    for chunk in text.split_inclusive('\n') {
        let line = chunk.strip_suffix('\n').unwrap_or(chunk);
        lines.push((line, offset));
        offset += chunk.len();
    }
    lines
}

#[cfg(test)]
mod tests {
    use alloc::format;
    use alloc::vec;

    use super::*;

    /// [`read`], bounded, so a scanner that stops advancing its cursor fails a case here instead
    /// of hanging the run.
    fn read(text: &str) -> Result<Raw, Refusal> {
        let text = text.to_string();
        crate::answered(move || super::read(&text))
    }

    #[test]
    fn a_file_that_does_not_open_with_a_fence_is_refused() {
        assert_eq!(read("name: x\n"), Err(Refusal::NoFrontmatter { line: 1 }));
    }

    #[test]
    fn frontmatter_with_no_closing_fence_is_refused() {
        assert_eq!(
            read("---\nname: x\n"),
            Err(Refusal::UnterminatedFrontmatter)
        );
    }

    #[test]
    fn frontmatter_over_the_cap_is_refused() {
        let text = format!(
            "---\nbig: {}\n---\nbody\n",
            "x".repeat(MAX_FRONTMATTER_BYTES)
        );
        assert!(matches!(
            read(&text),
            Err(Refusal::FrontmatterTooLarge { .. })
        ));
    }

    #[test]
    fn a_tab_indented_continuation_is_refused() {
        let text = "---\ndescription: >-\n\thello\n---\nbody\n";
        assert_eq!(read(text), Err(Refusal::TabIndented { line: 3 }));
    }

    #[test]
    fn an_indented_top_level_line_is_refused() {
        assert_eq!(
            read("---\n  name: x\n---\nbody\n"),
            Err(Refusal::IndentedLine { line: 2 })
        );
    }

    #[test]
    fn a_line_that_is_not_a_key_is_refused() {
        assert_eq!(
            read("---\njust words\n---\nbody\n"),
            Err(Refusal::NotAKey {
                line: 2,
                text: "just words".to_string(),
            })
        );
    }

    #[test]
    fn a_value_outside_the_subset_is_refused() {
        assert_eq!(
            read("---\nname: [a, b]\n---\nbody\n"),
            Err(Refusal::InvalidValue {
                field: "name".to_string(),
                line: 2,
                text: "[a, b]".to_string(),
            })
        );
    }

    #[test]
    fn an_unknown_key_is_skipped_with_its_value() {
        let raw = read("---\nvendor: [a]\nname: x\n---\nbody\n").unwrap();

        assert_eq!(
            raw.fields,
            vec![("name".to_string(), Value::Scalar("x".to_string()))],
        );
    }

    #[test]
    fn an_unknown_key_s_value_swallows_its_indented_lines() {
        let raw = read("---\nvendor:\n  a: 1\n  b: [2, 3]\nname: x\n---\nbody\n").unwrap();

        assert_eq!(
            raw.fields,
            vec![("name".to_string(), Value::Scalar("x".to_string()))],
        );
    }

    #[test]
    fn a_duplicate_known_key_is_refused() {
        assert_eq!(
            read("---\nname: a\nname: b\n---\nbody\n"),
            Err(Refusal::DuplicateKey {
                key: "name".to_string(),
                line: 3,
            })
        );
    }

    #[test]
    fn a_duplicate_unknown_key_is_tolerated() {
        let raw = read("---\nvendor: a\nvendor: b\n---\nbody\n");
        assert!(raw.is_ok());
    }

    #[test]
    fn a_nested_metadata_map_is_refused() {
        assert_eq!(
            read("---\nmetadata:\n  outer:\n    inner: x\n---\nbody\n"),
            Err(Refusal::NestedMetadata { line: 4 })
        );
    }

    #[test]
    fn a_metadata_line_that_is_not_an_entry_is_refused() {
        assert_eq!(
            read("---\nmetadata:\n  no-colon-here\n---\nbody\n"),
            Err(Refusal::InvalidMetadata {
                line: 3,
                text: "  no-colon-here".to_string(),
            })
        );
    }

    #[test]
    fn an_empty_metadata_value_is_refused() {
        assert_eq!(
            read("---\nmetadata:\n  author:\nname: x\n---\nbody\n"),
            Err(Refusal::EmptyMetadataValue {
                field: "author".to_string(),
                line: 3,
            })
        );
    }

    #[test]
    fn an_empty_metadata_value_at_the_closing_fence_is_refused() {
        assert_eq!(
            read("---\nmetadata:\n  author:\n---\nbody\n"),
            Err(Refusal::EmptyMetadataValue {
                field: "author".to_string(),
                line: 3,
            })
        );
    }

    #[test]
    fn a_blank_line_ends_nothing() {
        let raw = read("---\n\nname: x\n\n---\nbody\n").unwrap();
        assert_eq!(raw.fields.len(), 1);
        assert_eq!(raw.body, "body\n");
    }

    #[test]
    fn an_empty_frontmatter_is_shaped_not_refused() {
        let raw = read("---\n---\nbody\n").unwrap();
        assert_eq!(raw.fields.len(), 0);
        assert_eq!(raw.body, "body\n");
    }

    #[test]
    fn the_body_is_everything_after_the_closing_fence() {
        let raw = read("---\nname: x\n---\n\nsecond line\n").unwrap();
        assert_eq!(raw.body, "\nsecond line\n");
    }

    #[test]
    fn a_block_scalar_ends_at_the_next_top_level_key() {
        let raw =
            read("---\nname: s\ndescription: >-\n  one\n  two\nlicense: MIT\n---\nbody\n").unwrap();

        assert_eq!(
            raw.fields,
            vec![
                ("name".to_string(), Value::Scalar("s".to_string())),
                (
                    "description".to_string(),
                    Value::Block("one two".to_string())
                ),
                ("license".to_string(), Value::Scalar("MIT".to_string())),
            ],
        );
    }

    #[test]
    fn a_block_scalar_ends_at_the_next_metadata_entry() {
        let raw = read("---\nmetadata:\n  note: >-\n    one\n    two\n  author: org\n---\nbody\n")
            .unwrap();

        assert_eq!(
            raw.metadata,
            vec![
                ("note".to_string(), "one two".to_string()),
                ("author".to_string(), "org".to_string()),
            ],
        );
    }

    #[test]
    fn a_frontmatter_of_exactly_the_cap_is_accepted() {
        let filled = "x".repeat(MAX_FRONTMATTER_BYTES - "big: \n".len());
        let text = format!("---\nbig: {filled}\n---\nbody\n");
        let raw = read(&text).unwrap();

        assert_eq!(raw.body, "body\n");
    }

    #[test]
    fn the_frontmatter_refusal_names_the_region_size() {
        let filled = "x".repeat(MAX_FRONTMATTER_BYTES);
        let text = format!("---\nbig: {filled}\n---\nbody\n");

        assert_eq!(
            read(&text),
            Err(Refusal::FrontmatterTooLarge {
                bytes: MAX_FRONTMATTER_BYTES + "big: \n".len(),
            })
        );
    }

    #[test]
    fn an_empty_file_has_no_fence() {
        assert_eq!(read(""), Err(Refusal::NoFrontmatter { line: 1 }));
    }

    #[test]
    fn a_top_level_key_with_no_name_is_refused() {
        assert_eq!(
            read("---\n: value\n---\nbody\n"),
            Err(Refusal::NotAKey {
                line: 2,
                text: ": value".to_string(),
            })
        );
    }

    #[test]
    fn a_metadata_entry_with_no_name_is_refused() {
        assert_eq!(
            read("---\nmetadata:\n  : v\n---\nbody\n"),
            Err(Refusal::InvalidMetadata {
                line: 3,
                text: "  : v".to_string(),
            })
        );
    }

    #[test]
    fn a_duplicate_metadata_key_is_refused() {
        assert_eq!(
            read("---\nmetadata:\n  author: a\n  author: b\n---\nbody\n"),
            Err(Refusal::DuplicateKey {
                key: "author".to_string(),
                line: 4,
            })
        );
    }
}

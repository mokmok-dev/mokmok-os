//! The `SKILL.md` frontmatter scanner: text in, shape-checked values out.
//!
//! This module owns the file's shape and nothing else. It checks the `---` fences, reads the value
//! subset section 2 names, gathers the one level of `metadata` entries, and refuses a duplicate key
//! naming a known field. It does not decide what a valid `name` or `description` is; [`read`]
//! returns a [`Raw`] and `crate::parse` applies the field rules.

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
    /// Every top-level field, in file order, unknown keys included.
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

/// Which line of the file the scanner is reading.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum State {
    /// Before the opening `---`.
    #[default]
    Outside,
    /// After the closing `---`: the body.
    Body,
    /// A top-level field, or a blank line between fields.
    TopLevelKey,
    /// The continuation lines of a `|` or `>` block scalar.
    BlockScalar,
    /// A `key: value` entry inside the `metadata` block.
    MetadataEntry,
    /// The line that must decide an empty `metadata` value: nothing, or one level too deep.
    MetadataValue,
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
    after: State,
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
            if scanner.state == State::BlockScalar {
                scanner.finish_block();
            }
            let bytes = offset.saturating_sub(frontmatter_start);
            if bytes > MAX_FRONTMATTER_BYTES {
                return Err(Refusal::FrontmatterTooLarge { bytes });
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

    if let Some((field, line)) = scanner.pending_empty.take() {
        return Err(Refusal::EmptyMetadataValue { field, line });
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
    block: Option<Block>,
    pending_empty: Option<(String, usize)>,
    metadata_seen: bool,
}

impl Scanner {
    /// Reads one line in the current state. Returns whether the line was consumed.
    fn step(
        &mut self,
        line: &str,
        number: usize,
    ) -> Result<bool, Refusal> {
        match self.state {
            State::TopLevelKey => self.top_level(line, number),
            State::BlockScalar => self.block_scalar(line, number),
            State::MetadataEntry => self.metadata_entry(line, number),
            State::MetadataValue => self.metadata_value(line, number),
            State::Outside | State::Body => Ok(true),
        }
    }

    /// One top-level `key: value` line, a blank line, or the start of a block scalar.
    fn top_level(
        &mut self,
        line: &str,
        number: usize,
    ) -> Result<bool, Refusal> {
        if line.trim().is_empty() {
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

        let duplicate = self.fields.iter().any(|(seen, _)| seen == key)
            || (key == "metadata" && self.metadata_seen);
        if is_known(key) && duplicate {
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
            self.block = Some(Block {
                key: key.to_string(),
                folded,
                chomp,
                indent: None,
                lines: Vec::new(),
                after: State::TopLevelKey,
            });
            self.state = State::BlockScalar;
            return Ok(true);
        }

        let scalar = parse_scalar(value).ok_or_else(|| Refusal::InvalidValue {
            field: key.to_string(),
            line: number,
            text: value.to_string(),
        })?;
        self.fields.push((key.to_string(), Value::Scalar(scalar)));
        Ok(true)
    }

    /// One continuation line of a block scalar, or the line that ends it.
    fn block_scalar(
        &mut self,
        line: &str,
        number: usize,
    ) -> Result<bool, Refusal> {
        if line.is_empty() {
            if let Some(active) = self.block.as_mut() {
                active.lines.push(String::new());
            }
            return Ok(true);
        }
        if line.starts_with('\t') {
            return Err(Refusal::TabIndented { line: number });
        }

        let indent = line.len() - line.trim_start_matches(' ').len();
        let indented = line.starts_with(' ');
        let ends = !indented
            || self
                .block
                .as_ref()
                .and_then(|block| block.indent)
                .is_some_and(|base| indent < base);
        if ends {
            self.finish_block();
            return Ok(false);
        }

        if let Some(active) = self.block.as_mut() {
            let base = active.indent.unwrap_or(indent);
            active.lines.push(line[base..].to_string());
        }
        Ok(true)
    }

    /// One `key: value` line inside the `metadata` block, or the line that ends the block.
    fn metadata_entry(
        &mut self,
        line: &str,
        number: usize,
    ) -> Result<bool, Refusal> {
        if line.trim().is_empty() {
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

        let repeated = self.metadata.iter().any(|(seen, _)| seen == key)
            || self
                .pending_empty
                .as_ref()
                .is_some_and(|(seen, _)| seen == key);
        if repeated {
            return Err(Refusal::DuplicateKey {
                key: key.to_string(),
                line: number,
            });
        }

        let value = entry[colon + 1..].trim();
        if value.is_empty() {
            self.pending_empty = Some((key.to_string(), number));
            self.state = State::MetadataValue;
            return Ok(true);
        }

        if let Some((folded, chomp)) = block_indicator(value) {
            self.block = Some(Block {
                key: key.to_string(),
                folded,
                chomp,
                indent: None,
                lines: Vec::new(),
                after: State::MetadataEntry,
            });
            self.state = State::BlockScalar;
            return Ok(true);
        }

        let scalar = parse_scalar(value).ok_or_else(|| Refusal::InvalidMetadata {
            line: number,
            text: line.to_string(),
        })?;
        self.metadata.push((key.to_string(), scalar));
        Ok(true)
    }

    /// The line after an empty `metadata` value: deeper means a nested map, flatter means no value.
    fn metadata_value(
        &mut self,
        line: &str,
        number: usize,
    ) -> Result<bool, Refusal> {
        if line.trim().is_empty() {
            return Ok(true);
        }
        if line.starts_with(' ') {
            return Err(Refusal::NestedMetadata { line: number });
        }
        let (field, line) = self
            .pending_empty
            .take()
            .unwrap_or_else(|| (String::new(), number));
        Err(Refusal::EmptyMetadataValue { field, line })
    }

    /// Puts the open block scalar's value where its key belongs.
    fn finish_block(&mut self) {
        if let Some(block) = self.block.take() {
            let rendered = render_block(&block);
            if block.after == State::MetadataEntry {
                self.metadata.push((block.key, rendered));
            } else {
                self.fields.push((block.key, Value::Block(rendered)));
            }
        }
    }
}

/// The keys section 2 names.
fn is_known(key: &str) -> bool {
    KNOWN_KEYS.contains(&key)
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
    use super::*;

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
    fn an_unknown_key_is_shape_checked() {
        assert_eq!(
            read("---\nvendor: [a]\n---\nbody\n"),
            Err(Refusal::InvalidValue {
                field: "vendor".to_string(),
                line: 2,
                text: "[a]".to_string(),
            })
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
    fn a_blank_line_ends_nothing() {
        let raw = read("---\n\nname: x\n\n---\nbody\n").unwrap();
        assert_eq!(raw.fields.len(), 1);
        assert_eq!(raw.body, "body\n");
    }

    #[test]
    fn an_empty_frontmatter_is_shaped_not_refused() {
        let raw = read("---\n---\nbody\n").unwrap();
        assert!(raw.fields.is_empty());
        assert_eq!(raw.body, "body\n");
    }

    #[test]
    fn the_body_is_everything_after_the_closing_fence() {
        let raw = read("---\nname: x\n---\n\nsecond line\n").unwrap();
        assert_eq!(raw.body, "\nsecond line\n");
    }
}

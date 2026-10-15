//! Reads a unified diff and applies it, with no repository behind it.

use std::fmt::Write as _;
use std::str;
use thiserror::Error;

/// One patch: the file sections a diff names, in order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Patch {
    /// The sections, in the order they appeared in the diff.
    pub files: Vec<FilePatch>,
}

/// One file's section of a patch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FilePatch {
    /// The path named on the `---` side.
    pub source: Label,
    /// The path named on the `+++` side.
    pub target: Label,
    /// The hunks belonging to this file, in order.
    pub hunks: Vec<Hunk>,
}

/// Which file a side of a section names. `Absent` is `/dev/null`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Label {
    /// The side names `/dev/null`.
    Absent,
    /// The side names a concrete path.
    Path(String),
}

/// One hunk: the counts the diff stated, and the body.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Hunk {
    /// The numbers from the `@@` header.
    pub stated: Header,
    /// The body lines, in order.
    pub lines: Vec<Line>,
}

/// The numbers one `@@` header states, as bytes in the diff.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Header {
    /// The 1-based starting line on the old side.
    pub old_start: u64,
    /// The number of old-side lines the diff claimed.
    pub old_len: u64,
    /// The 1-based starting line on the new side.
    pub new_start: u64,
    /// The number of new-side lines the diff claimed.
    pub new_len: u64,
}

/// One body line.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Line {
    /// What the line does in the hunk.
    pub kind: Kind,
    /// The line text, without the leading ` `, `-`, or `+`.
    pub text: String,
    /// Whether the source line ended with a newline.
    pub newline: bool,
}

/// What a body line does.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Kind {
    /// A line present on both sides.
    Context,
    /// A line present only on the old side.
    Removed,
    /// A line present only on the new side.
    Added,
}

/// A file with every hunk applied.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Applied {
    /// The bytes of the patched file.
    pub bytes: Vec<u8>,
    /// Where each hunk was placed.
    pub hunks: Vec<Placed>,
}

/// Where one hunk's body went.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Placed {
    /// A hunk with an old side matched that side here, this far from the line its header named.
    Matched {
        /// The 1-based line where the old side matched.
        line: u64,
        /// The signed distance from `Header::old_start`.
        offset: i64,
    },
    /// A hunk with no old side put its new lines here, after this many lines of the file.
    ///
    /// A start past the end of the file clamps to the number of lines the file has.
    Inserted {
        /// How many lines of the file precede the insertion.
        after: u64,
    },
}

/// Ways a diff can fail to parse.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum ParseError {
    #[error("no file section opened")]
    NoFileSection,
    #[error("file section had no hunks")]
    NoHunks,
    #[error("invalid hunk line: {0}")]
    InvalidHunkLine(String),
    #[error("invalid hunk header: {0}")]
    InvalidHeader(String),
    #[error("invalid path: {0}")]
    InvalidPath(String),
}

/// Ways applying a section can fail.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum ApplyError {
    #[error("file is not valid UTF-8")]
    NotText,
    #[error("hunk {hunk} does not match the file from its stated line forward")]
    NotFound { hunk: usize },
    #[error("the source is absent, so a hunk with an old side has nothing to match")]
    NothingToApply,
    #[error("hunk {hunk} starts at 0 in a file that exists")]
    ZeroStart { hunk: usize },
}

impl Patch {
    /// Reads one unified diff. Every header number is kept as stated; nothing is applied.
    ///
    /// # Errors
    ///
    /// Returns `ParseError::NoFileSection` if no `---`/`+++` pair opens a section,
    /// `ParseError::NoHunks` if a section has no hunks, `ParseError::InvalidHunkLine` if a body
    /// line starts with an unexpected character, `ParseError::InvalidHeader` if an `@@` header
    /// does not contain four readable numbers, or `ParseError::InvalidPath` if a path is
    /// absolute, contains `..`, or the two section labels name different paths.
    pub fn parse(source: &str) -> Result<Self, ParseError> {
        let lines: Vec<&str> = source.lines().collect();
        let mut files = Vec::new();
        let mut rest = lines.as_slice();

        while let Some((line, tail)) = rest.split_first() {
            if is_ignored_preamble(line) {
                rest = tail;
                continue;
            }

            if let Some(source_rest) = line.strip_prefix("--- ") {
                let source_label = parse_label(source_rest)?;
                let (target_line, after_target) =
                    tail.split_first().ok_or(ParseError::NoFileSection)?;
                let target_rest = target_line
                    .strip_prefix("+++ ")
                    .ok_or(ParseError::NoFileSection)?;
                let target_label = parse_label(target_rest)?;
                validate_labels(&source_label, &target_label)?;

                let mut hunks = Vec::new();
                let mut hunk_rest = after_target;
                while let Some((hunk_line, _)) = hunk_rest.split_first() {
                    if !hunk_line.starts_with("@@") {
                        break;
                    }
                    let start = lines.len() - hunk_rest.len();
                    let (hunk, next_i) = parse_hunk(&lines, start)?;
                    hunks.push(hunk);
                    hunk_rest = &lines[next_i..];
                }

                if hunks.is_empty() {
                    return Err(ParseError::NoHunks);
                }

                files.push(FilePatch {
                    source: source_label,
                    target: target_label,
                    hunks,
                });
                rest = hunk_rest;
                continue;
            }

            // A model that wraps its answer in a sentence still has a patch inside it, so anything
            // before the first section is skipped rather than refused. Inside a section nothing is
            // skipped: a body line we guessed at is a body line we could apply wrongly.
            rest = tail;
        }

        if files.is_empty() {
            return Err(ParseError::NoFileSection);
        }

        Ok(Self { files })
    }

    /// Recomputed counts for every hunk, the starts left as stated.
    #[must_use]
    pub fn recounted(&self) -> Self {
        let mut files = self.files.clone();
        for file in &mut files {
            for hunk in &mut file.hunks {
                hunk.stated.old_len = hunk
                    .lines
                    .iter()
                    .filter(|line| matches!(line.kind, Kind::Context | Kind::Removed))
                    .count() as u64;
                hunk.stated.new_len = hunk
                    .lines
                    .iter()
                    .filter(|line| matches!(line.kind, Kind::Context | Kind::Added))
                    .count() as u64;
            }
        }
        Self { files }
    }

    /// The patch as a unified diff, one section per file.
    #[must_use]
    pub fn to_unified(&self) -> String {
        let mut out = String::new();
        for file in &self.files {
            write_file_patch(&mut out, file);
        }
        out
    }
}

impl FilePatch {
    /// Applies every hunk to `old`. Nothing is written and no file is opened.
    ///
    /// A hunk whose body has no old-side line inserts rather than replaces, so a file the diff
    /// creates is applied to empty bytes even when its header does not name `/dev/null`.
    ///
    /// # Errors
    ///
    /// Returns `ApplyError::NotText` if `old` is not valid UTF-8, `ApplyError::NothingToApply` if the
    /// source is absent but a hunk has an old side to match, `ApplyError::ZeroStart` if such a hunk
    /// starts at line `0`, or `ApplyError::NotFound` if its old side cannot be found from its header
    /// forward to the end of the file.
    pub fn apply(
        &self,
        old: &[u8],
    ) -> Result<Applied, ApplyError> {
        let source_absent = matches!(self.source, Label::Absent);
        let mut file_lines: Vec<FileLine> = if source_absent {
            Vec::new()
        } else {
            let text = str::from_utf8(old).map_err(|_| ApplyError::NotText)?;
            split_text(text)
        };

        let mut placed = Vec::new();
        let mut delta: i64 = 0;

        for (hunk_index, hunk) in self.hunks.iter().enumerate() {
            let old_side: Vec<usize> = hunk
                .lines
                .iter()
                .enumerate()
                .filter(|(_, line)| matches!(line.kind, Kind::Context | Kind::Removed))
                .map(|(index, _)| index)
                .collect();
            let new_side: Vec<usize> = hunk
                .lines
                .iter()
                .enumerate()
                .filter(|(_, line)| matches!(line.kind, Kind::Context | Kind::Added))
                .map(|(index, _)| index)
                .collect();

            let n = file_lines.len();
            let side_len = old_side.len();

            // A hunk with no old-side line inserts between lines rather than replacing them, so no
            // source is needed and the label need not say `/dev/null` for a file the diff creates.
            let (placement, insert_index) = if old_side.is_empty() {
                let raw = usize::try_from(hunk.stated.old_start).unwrap_or(usize::MAX);
                let index = raw.min(n);

                (
                    Placed::Inserted {
                        after: u64::try_from(index).unwrap_or(u64::MAX),
                    },
                    index,
                )
            } else {
                if source_absent {
                    return Err(ApplyError::NothingToApply);
                }

                if hunk.stated.old_start == 0 {
                    return Err(ApplyError::ZeroStart { hunk: hunk_index });
                }

                if side_len > n {
                    return Err(ApplyError::NotFound { hunk: hunk_index });
                }

                let raw_start = usize::try_from(hunk.stated.old_start).unwrap_or(usize::MAX);
                let shifted = if delta >= 0 {
                    raw_start.saturating_add(usize::try_from(delta).unwrap_or(usize::MAX))
                } else {
                    raw_start
                        .saturating_sub(usize::try_from(delta.unsigned_abs()).unwrap_or(usize::MAX))
                };
                let max_idx = n - side_len;
                let idx = shifted.saturating_sub(1).clamp(0, max_idx);
                let found_idx = (idx..=max_idx)
                    .find(|&candidate| {
                        old_side
                            .iter()
                            .enumerate()
                            .all(|(offset_in_side, &line_index)| {
                                file_lines[candidate + offset_in_side]
                                    .matches(&hunk.lines[line_index])
                            })
                    })
                    .ok_or(ApplyError::NotFound { hunk: hunk_index })?;
                let line = u64::try_from(found_idx + 1).unwrap_or(u64::MAX);
                let offset = i64::try_from(found_idx + 1).unwrap_or(i64::MAX)
                    - i64::try_from(hunk.stated.old_start).unwrap_or(i64::MAX);

                (Placed::Matched { line, offset }, found_idx)
            };

            let replacements: Vec<FileLine> = new_side
                .iter()
                .map(|&line_index| FileLine::from_hunk_line(&hunk.lines[line_index]))
                .collect();
            file_lines.splice(insert_index..insert_index + old_side.len(), replacements);
            delta += i64::try_from(new_side.len()).unwrap_or(i64::MAX)
                - i64::try_from(old_side.len()).unwrap_or(i64::MAX);

            placed.push(placement);
        }

        Ok(Applied {
            bytes: encode_lines(&file_lines),
            hunks: placed,
        })
    }
}

fn is_ignored_preamble(line: &str) -> bool {
    line.starts_with("diff ")
        || line.starts_with("index ")
        || line.starts_with("new file mode")
        || line.starts_with("old mode")
        || line.starts_with("deleted file mode")
        || line.starts_with("similarity index ")
}

fn parse_label(raw: &str) -> Result<Label, ParseError> {
    let tab = raw.find('\t');
    let path_part = tab.map_or(raw, |index| &raw[..index]);

    if path_part == "/dev/null" {
        return Ok(Label::Absent);
    }

    let path = path_part
        .strip_prefix("a/")
        .or_else(|| path_part.strip_prefix("b/"))
        .unwrap_or(path_part);

    if path.is_empty()
        || path.starts_with('/')
        || path.split('/').any(|component| component == "..")
    {
        return Err(ParseError::InvalidPath(path.to_string()));
    }

    Ok(Label::Path(path.to_string()))
}

fn validate_labels(
    source: &Label,
    target: &Label,
) -> Result<(), ParseError> {
    match (source, target) {
        (Label::Absent, Label::Absent) => Err(ParseError::InvalidPath(
            "both sides name /dev/null".to_string(),
        )),
        (Label::Path(s), Label::Path(t)) if s != t => Err(ParseError::InvalidPath(format!(
            "source {s:?} differs from target {t:?}"
        ))),
        _ => Ok(()),
    }
}

fn parse_hunk(
    lines: &[&str],
    start: usize,
) -> Result<(Hunk, usize), ParseError> {
    let header = parse_header(lines[start])?;
    let mut body: Vec<Line> = Vec::new();
    let mut rest = &lines[start + 1..];

    while let Some((line, tail)) = rest.split_first() {
        if line.starts_with("@@") || line.starts_with("--- ") || line.starts_with("diff ") {
            break;
        }

        if *line == "\\ No newline at end of file" {
            if let Some(last) = body.last_mut() {
                last.newline = false;
            }
            rest = tail;
            continue;
        }

        if line.is_empty() {
            body.push(Line {
                kind: Kind::Context,
                text: String::new(),
                newline: true,
            });
            rest = tail;
            continue;
        }

        let kind = match line.as_bytes()[0] {
            b' ' => Kind::Context,
            b'-' => Kind::Removed,
            b'+' => Kind::Added,
            _ => return Err(ParseError::InvalidHunkLine(line.to_string())),
        };

        body.push(Line {
            kind,
            text: line[1..].to_string(),
            newline: true,
        });
        rest = tail;
    }

    Ok((
        Hunk {
            stated: header,
            lines: body,
        },
        lines.len() - rest.len(),
    ))
}

fn parse_header(line: &str) -> Result<Header, ParseError> {
    if !line.starts_with("@@") {
        return Err(ParseError::InvalidHeader(line.to_string()));
    }

    let rest = &line[2..];
    let marker = rest
        .find("@@")
        .ok_or_else(|| ParseError::InvalidHeader(line.to_string()))?;
    let inner = rest[..marker].trim();
    let tokens: Vec<&str> = inner.split_whitespace().collect();

    if tokens.len() != 2 {
        return Err(ParseError::InvalidHeader(line.to_string()));
    }

    let (old_start, old_len) = parse_range(tokens[0])?;
    let (new_start, new_len) = parse_range(tokens[1])?;

    Ok(Header {
        old_start,
        old_len,
        new_start,
        new_len,
    })
}

fn parse_range(token: &str) -> Result<(u64, u64), ParseError> {
    let bytes = token.as_bytes();
    if bytes.len() < 2 {
        return Err(ParseError::InvalidHeader(token.to_string()));
    }

    let rest = &token[1..];
    let (start_s, len_s) = rest
        .find(',')
        .map_or((rest, "1"), |index| (&rest[..index], &rest[index + 1..]));

    let start = start_s
        .parse::<u64>()
        .map_err(|_| ParseError::InvalidHeader(token.to_string()))?;
    let len = len_s
        .parse::<u64>()
        .map_err(|_| ParseError::InvalidHeader(token.to_string()))?;

    Ok((start, len))
}

fn write_file_patch(
    out: &mut String,
    file: &FilePatch,
) {
    out.push_str(&label_line('-', &file.source));
    out.push('\n');
    out.push_str(&label_line('+', &file.target));
    out.push('\n');

    for hunk in &file.hunks {
        write_hunk(out, hunk);
    }
}

/// One section header line.
///
/// The paths render as they were read: the `a/` and `b/` prefixes a diff may carry are stripped at
/// parse, and no `diff --git` or `index` line is invented, so the rendered patch is the patch that
/// was read rather than the shape a version control tool would have written.
fn label_line(
    side: char,
    label: &Label,
) -> String {
    match label {
        Label::Absent => format!("{side}{side}{side} /dev/null"),
        Label::Path(p) => format!("{side}{side}{side} {p}"),
    }
}

fn write_hunk(
    out: &mut String,
    hunk: &Hunk,
) {
    let old_len = hunk
        .lines
        .iter()
        .filter(|line| matches!(line.kind, Kind::Context | Kind::Removed))
        .count() as u64;
    let new_len = hunk
        .lines
        .iter()
        .filter(|line| matches!(line.kind, Kind::Context | Kind::Added))
        .count() as u64;

    let _ = writeln!(
        out,
        "@@ -{},{} +{},{} @@",
        hunk.stated.old_start, old_len, hunk.stated.new_start, new_len
    );

    for line in &hunk.lines {
        let marker = match line.kind {
            Kind::Context => ' ',
            Kind::Removed => '-',
            Kind::Added => '+',
        };
        out.push(marker);
        out.push_str(&line.text);
        if line.newline {
            out.push('\n');
        } else {
            out.push_str("\n\\ No newline at end of file\n");
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct FileLine {
    text: String,
    newline: bool,
}

impl FileLine {
    fn matches(
        &self,
        hunk_line: &Line,
    ) -> bool {
        matches!(hunk_line.kind, Kind::Context | Kind::Removed)
            && self.text == hunk_line.text
            && self.newline == hunk_line.newline
    }

    fn from_hunk_line(line: &Line) -> Self {
        Self {
            text: line.text.clone(),
            newline: line.newline,
        }
    }
}

fn split_text(text: &str) -> Vec<FileLine> {
    if text.is_empty() {
        return Vec::new();
    }

    let mut lines = Vec::new();
    let mut start = 0;
    for (index, character) in text.char_indices() {
        if character == '\n' {
            lines.push(FileLine {
                text: text[start..index].to_string(),
                newline: true,
            });
            start = index + 1;
        }
    }

    if start < text.len() {
        lines.push(FileLine {
            text: text[start..].to_string(),
            newline: false,
        });
    }

    lines
}

fn encode_lines(lines: &[FileLine]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for line in lines {
        bytes.extend_from_slice(line.text.as_bytes());
        if line.newline {
            bytes.push(b'\n');
        }
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn parse_real_two_file_diff() {
        let diff = [
            "diff --git a/Cargo.toml b/Cargo.toml",
            "index 1234567..abcdefg 100644",
            "--- a/Cargo.toml\t2024-01-01 00:00:00.000000000 +0000",
            "+++ b/Cargo.toml\t2024-01-01 00:00:00.000000000 +0000",
            "@@ -1,2 +1,3 @@",
            " one",
            "-two",
            "+twox",
            "diff --git a/src/lib.rs b/src/lib.rs",
            "--- a/src/lib.rs",
            "+++ b/src/lib.rs",
            "@@ -10,1 +10,1 @@",
            " old",
            "-new",
            "+new2",
            "",
        ]
        .join("\n");

        let patch = Patch::parse(&diff).unwrap();
        assert_eq!(patch.files.len(), 2);
        assert_eq!(patch.files[0].source, Label::Path("Cargo.toml".to_string()));
        assert_eq!(patch.files[0].target, Label::Path("Cargo.toml".to_string()));
        assert_eq!(patch.files[1].source, Label::Path("src/lib.rs".to_string()));
        assert_eq!(patch.files[1].target, Label::Path("src/lib.rs".to_string()));

        assert_eq!(patch.files[0].hunks.len(), 1);
        let hunk0 = &patch.files[0].hunks[0];
        assert_eq!(
            hunk0.stated,
            Header {
                old_start: 1,
                old_len: 2,
                new_start: 1,
                new_len: 3,
            }
        );
        assert_eq!(hunk0.lines.len(), 3);
        assert_eq!(
            hunk0.lines[0],
            Line {
                kind: Kind::Context,
                text: "one".to_string(),
                newline: true,
            }
        );
        assert_eq!(
            hunk0.lines[1],
            Line {
                kind: Kind::Removed,
                text: "two".to_string(),
                newline: true,
            }
        );
        assert_eq!(
            hunk0.lines[2],
            Line {
                kind: Kind::Added,
                text: "twox".to_string(),
                newline: true,
            }
        );

        let hunk1 = &patch.files[1].hunks[0];
        assert_eq!(hunk1.lines.len(), 3);
        assert_eq!(hunk1.lines[0].kind, Kind::Context);
        assert_eq!(hunk1.lines[0].text, "old");
        assert_eq!(hunk1.lines[1].kind, Kind::Removed);
        assert_eq!(hunk1.lines[1].text, "new");
        assert_eq!(hunk1.lines[2].kind, Kind::Added);
        assert_eq!(hunk1.lines[2].text, "new2");
    }

    #[test]
    fn header_with_wrong_counts_is_recounted() {
        let diff = "--- a/f\n+++ b/f\n@@ -1,5 +1,5 @@\n one\n two\n+three\n four\n";
        let patch = Patch::parse(diff).unwrap();
        let recounted = patch.recounted();
        assert_eq!(
            recounted.files[0].hunks[0].stated,
            Header {
                old_start: 1,
                old_len: 3,
                new_start: 1,
                new_len: 4,
            }
        );
    }

    #[test]
    fn to_unified_round_trips() {
        let diff = [
            "diff --git a/f b/f",
            "index 0000000..0000000",
            "--- a/f",
            "+++ b/f",
            "@@ -1,3 +1,3 @@",
            " keep",
            "-drop",
            "+add",
            " tail",
            "@@ -5,1 +6,2 @@",
            " five",
            "+six",
            "",
        ]
        .join("\n");
        let patch = Patch::parse(&diff).unwrap();
        let rendered = patch.to_unified();
        let again = Patch::parse(&rendered).unwrap();
        assert_eq!(again, patch);
    }

    #[test]
    fn apply_at_exact_offset() {
        let file = FilePatch {
            source: Label::Path("f".to_string()),
            target: Label::Path("f".to_string()),
            hunks: vec![Hunk {
                stated: Header {
                    old_start: 2,
                    old_len: 1,
                    new_start: 2,
                    new_len: 1,
                },
                lines: vec![
                    Line {
                        kind: Kind::Removed,
                        text: "two".to_string(),
                        newline: true,
                    },
                    Line {
                        kind: Kind::Added,
                        text: "TWO".to_string(),
                        newline: true,
                    },
                ],
            }],
        };

        let applied = file.apply(b"one\ntwo\nthree\n").unwrap();
        assert_eq!(applied.bytes, b"one\nTWO\nthree\n");
        assert_eq!(applied.hunks, vec![Placed::Matched { line: 2, offset: 0 }]);
    }

    #[test]
    fn apply_shifted_forward_by_two() {
        let file = FilePatch {
            source: Label::Path("f".to_string()),
            target: Label::Path("f".to_string()),
            hunks: vec![Hunk {
                stated: Header {
                    old_start: 1,
                    old_len: 3,
                    new_start: 1,
                    new_len: 3,
                },
                lines: vec![
                    Line {
                        kind: Kind::Context,
                        text: "three".to_string(),
                        newline: true,
                    },
                    Line {
                        kind: Kind::Removed,
                        text: "four".to_string(),
                        newline: true,
                    },
                    Line {
                        kind: Kind::Added,
                        text: "FOUR".to_string(),
                        newline: true,
                    },
                    Line {
                        kind: Kind::Context,
                        text: "five".to_string(),
                        newline: true,
                    },
                ],
            }],
        };

        let applied = file.apply(b"one\ntwo\nthree\nfour\nfive\n").unwrap();
        assert_eq!(applied.bytes, b"one\ntwo\nthree\nFOUR\nfive\n");
        assert_eq!(applied.hunks, vec![Placed::Matched { line: 3, offset: 2 }]);
    }

    #[test]
    fn apply_not_found() {
        let file = FilePatch {
            source: Label::Path("f".to_string()),
            target: Label::Path("f".to_string()),
            hunks: vec![Hunk {
                stated: Header {
                    old_start: 1,
                    old_len: 1,
                    new_start: 1,
                    new_len: 1,
                },
                lines: vec![
                    Line {
                        kind: Kind::Context,
                        text: "missing".to_string(),
                        newline: true,
                    },
                    Line {
                        kind: Kind::Added,
                        text: "extra".to_string(),
                        newline: true,
                    },
                ],
            }],
        };

        assert_eq!(
            file.apply(b"alpha\nbeta\n"),
            Err(ApplyError::NotFound { hunk: 0 })
        );
    }

    #[test]
    fn apply_creates_file() {
        let file = FilePatch {
            source: Label::Absent,
            target: Label::Path("f".to_string()),
            hunks: vec![Hunk {
                stated: Header {
                    old_start: 0,
                    old_len: 0,
                    new_start: 1,
                    new_len: 2,
                },
                lines: vec![
                    Line {
                        kind: Kind::Added,
                        text: "one".to_string(),
                        newline: true,
                    },
                    Line {
                        kind: Kind::Added,
                        text: "two".to_string(),
                        newline: true,
                    },
                ],
            }],
        };

        let applied = file.apply(b"").unwrap();
        assert_eq!(applied.bytes, b"one\ntwo\n");
        assert_eq!(applied.hunks, vec![Placed::Inserted { after: 0 }]);
    }

    #[test]
    fn apply_deletes_file() {
        let file = FilePatch {
            source: Label::Path("f".to_string()),
            target: Label::Absent,
            hunks: vec![Hunk {
                stated: Header {
                    old_start: 1,
                    old_len: 2,
                    new_start: 0,
                    new_len: 0,
                },
                lines: vec![
                    Line {
                        kind: Kind::Removed,
                        text: "one".to_string(),
                        newline: true,
                    },
                    Line {
                        kind: Kind::Removed,
                        text: "two".to_string(),
                        newline: true,
                    },
                ],
            }],
        };

        let applied = file.apply(b"one\ntwo\n").unwrap();
        assert_eq!(applied.bytes, Vec::<u8>::new());
        assert_eq!(applied.hunks, vec![Placed::Matched { line: 1, offset: 0 }]);
    }

    #[test]
    fn no_newline_at_end_round_trips() {
        let create = [
            "--- /dev/null",
            "+++ b/f",
            "@@ -0,0 +1,1 @@",
            "+last",
            "\\ No newline at end of file",
            "",
        ]
        .join("\n");
        let patch = Patch::parse(&create).unwrap();
        assert!(!patch.files[0].hunks[0].lines[0].newline);
        let applied = patch.files[0].apply(b"").unwrap();
        assert_eq!(applied.bytes, b"last");
        let rendered = patch.to_unified();
        let again = Patch::parse(&rendered).unwrap();
        assert_eq!(
            again.files[0].hunks[0].lines[0].newline,
            patch.files[0].hunks[0].lines[0].newline
        );

        let delete = [
            "--- a/f",
            "+++ /dev/null",
            "@@ -1,1 +0,0 @@",
            "-only",
            "\\ No newline at end of file",
            "",
        ]
        .join("\n");
        let patch = Patch::parse(&delete).unwrap();
        assert!(!patch.files[0].hunks[0].lines[0].newline);
        let applied = patch.files[0].apply(b"only").unwrap();
        assert_eq!(applied.bytes, Vec::<u8>::new());
    }

    #[test]
    fn binary_file_is_not_text() {
        let file = FilePatch {
            source: Label::Path("f".to_string()),
            target: Label::Path("f".to_string()),
            hunks: vec![Hunk {
                stated: Header {
                    old_start: 1,
                    old_len: 1,
                    new_start: 1,
                    new_len: 1,
                },
                lines: vec![
                    Line {
                        kind: Kind::Removed,
                        text: String::new(),
                        newline: true,
                    },
                    Line {
                        kind: Kind::Added,
                        text: String::new(),
                        newline: true,
                    },
                ],
            }],
        };

        assert_eq!(file.apply(b"\xff\xfe"), Err(ApplyError::NotText));
    }

    #[test]
    fn dotdot_path_is_rejected() {
        let diff = "--- a/../etc\n+++ b/../etc\n@@ -1,1 +1,1 @@\n x\n";
        assert!(Patch::parse(diff).is_err());
    }

    #[test]
    fn absolute_path_is_rejected() {
        let diff = "--- a//etc/passwd\n+++ b//etc/passwd\n@@ -1,1 +1,1 @@\n x\n";
        assert!(Patch::parse(diff).is_err());
    }

    #[test]
    fn a_new_file_whose_source_is_not_dev_null_is_still_created() {
        let diff = "--- a/new.txt\n+++ b/new.txt\n@@ -0,0 +1,2 @@\n+one\n+two\n";
        let patch = Patch::parse(diff).unwrap();

        let applied = patch.files[0].apply(&[]).unwrap();

        assert_eq!(applied.bytes, b"one\ntwo\n".to_vec());
        assert_eq!(applied.hunks, vec![Placed::Inserted { after: 0 }]);
    }

    #[test]
    fn an_insertion_with_no_old_side_lands_after_the_line_its_header_names() {
        let diff = "--- a/f\n+++ b/f\n@@ -2,0 +3,1 @@\n+new\n";
        let patch = Patch::parse(diff).unwrap();

        let applied = patch.files[0].apply(b"one\ntwo\nthree\n").unwrap();

        assert_eq!(applied.bytes, b"one\ntwo\nnew\nthree\n".to_vec());
        assert_eq!(applied.hunks, vec![Placed::Inserted { after: 2 }]);
    }

    #[test]
    fn prose_before_the_first_section_is_skipped() {
        let diff = "Here is the change:\n\n--- a/x.rs\n+++ b/x.rs\n@@ -1,1 +1,1 @@\n-old\n+new\n";
        let patch = Patch::parse(diff).unwrap();

        assert_eq!(patch.files.len(), 1);
        assert_eq!(patch.files[0].target, Label::Path("x.rs".to_string()));
    }

    #[test]
    fn prose_after_the_last_hunk_names_the_line_it_could_not_read() {
        let diff = "--- a/x.rs\n+++ b/x.rs\n@@ -1,1 +1,1 @@\n-old\n+new\nThat is all.\n";

        assert_eq!(
            Patch::parse(diff),
            Err(ParseError::InvalidHunkLine("That is all.".to_string())),
        );
    }

    #[test]
    fn the_rendered_patch_invents_no_version_control_preamble() {
        let diff = "diff --git a/src/x.rs b/src/x.rs\n--- a/src/x.rs\n+++ b/src/x.rs\n@@ -1,1 +1,1 @@\n-old\n+new\n";
        let patch = Patch::parse(diff).unwrap();

        assert_eq!(
            patch.to_unified(),
            "--- src/x.rs\n+++ src/x.rs\n@@ -1,1 +1,1 @@\n-old\n+new\n",
        );
    }

    fn line_text() -> impl Strategy<Value = String> {
        proptest::string::string_regex("[^\n\r]{0,10}").unwrap()
    }

    fn lines() -> impl Strategy<Value = Vec<String>> {
        prop::collection::vec(line_text(), 0..16)
    }

    fn old_and_patch() -> impl Strategy<Value = (Vec<String>, FilePatch)> {
        (
            lines(),
            any::<usize>(),
            any::<usize>(),
            prop::collection::vec(line_text(), 0..4),
        )
            .prop_map(|(old, start_raw, insert_raw, added)| {
                let len = old.len();
                let start = start_raw % (len + 1);
                let max_end_len = len.saturating_sub(start) + 1;
                let end = start + (insert_raw % max_end_len);
                let insert = start + (insert_raw % (end - start + 1));
                let added_len = added.len();

                let mut hunk_lines = Vec::new();
                for line in old.iter().skip(start).take(insert - start) {
                    hunk_lines.push(Line {
                        kind: Kind::Context,
                        text: line.clone(),
                        newline: true,
                    });
                }
                for line in added {
                    hunk_lines.push(Line {
                        kind: Kind::Added,
                        text: line,
                        newline: true,
                    });
                }
                for line in old.iter().skip(insert).take(end - insert) {
                    hunk_lines.push(Line {
                        kind: Kind::Context,
                        text: line.clone(),
                        newline: true,
                    });
                }

                let selected = end - start;
                let patch = FilePatch {
                    source: Label::Path("f".to_string()),
                    target: Label::Path("f".to_string()),
                    hunks: vec![Hunk {
                        stated: Header {
                            old_start: start as u64 + 1,
                            old_len: selected as u64,
                            new_start: start as u64 + 1,
                            new_len: (selected + added_len) as u64,
                        },
                        lines: hunk_lines,
                    }],
                };
                (old, patch)
            })
    }

    fn lines_to_bytes(lines: &[String]) -> Vec<u8> {
        if lines.is_empty() {
            return Vec::new();
        }
        let mut text = lines.join("\n");
        text.push('\n');
        text.into_bytes()
    }

    proptest! {
        #[test]
        fn rendered_patch_applies_idempotently_to_original((old, hunk) in old_and_patch()) {
            let old_bytes = lines_to_bytes(&old);
            let first = hunk.apply(&old_bytes).unwrap();

            let rendered = Patch { files: vec![hunk.clone()] }.to_unified();
            let reparsed = Patch::parse(&rendered).unwrap();
            let second = reparsed.files[0].apply(&old_bytes).unwrap();

            let expected = if hunk.hunks[0]
                .lines
                .iter()
                .any(|line| matches!(line.kind, Kind::Context | Kind::Removed))
            {
                Placed::Matched {
                    line: hunk.hunks[0].stated.old_start,
                    offset: 0,
                }
            } else {
                Placed::Inserted {
                    after: hunk.hunks[0]
                        .stated
                        .old_start
                        .min(u64::try_from(old.len()).unwrap()),
                }
            };

            prop_assert_eq!(second.bytes, first.bytes);
            prop_assert_eq!(second.hunks, vec![expected]);
        }

        #[test]
        fn mutated_header_count_does_not_change_apply((old, mut patch) in old_and_patch()) {
            let old_bytes = lines_to_bytes(&old);
            let expected = patch.apply(&old_bytes).unwrap().bytes;

            patch.hunks[0].stated.old_len = 0;
            patch.hunks[0].stated.new_len = 99;
            let got = patch.apply(&old_bytes).unwrap().bytes;

            prop_assert_eq!(got, expected);
        }
    }
}

//! A small markdown subset for agent replies.

use crate::port::Tone;

/// One drawn line of agent prose, and whether it came from a fenced block.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Prose {
    pub spans: Vec<(String, Tone)>,
    pub code: bool,
}

/// Statefully turns one logical line of agent text into styled spans.
///
/// A fenced block spans lines, so the state lives across the reply's lines; a fence line itself
/// draws nothing.
#[derive(Debug, Default)]
pub struct Markdown {
    in_block: bool,
}

impl Markdown {
    pub const fn new() -> Self {
        Self { in_block: false }
    }

    /// The styled spans for one line, or `None` when the line is a fence.
    pub fn line(
        &mut self,
        text: &str,
    ) -> Option<Prose> {
        let trimmed = text.trim_start();
        if trimmed.starts_with("```") {
            self.in_block = !self.in_block;
            return None;
        }
        if self.in_block {
            return Some(Prose {
                spans: vec![(escaped(text), Tone::Code)],
                code: true,
            });
        }
        if let Some(body) = header_body(text) {
            return Some(Prose {
                spans: vec![(escaped(body), Tone::Strong)],
                code: false,
            });
        }

        Some(Prose {
            spans: inline(text),
            code: false,
        })
    }
}

fn header_body(text: &str) -> Option<&str> {
    let hashes = text
        .chars()
        .take_while(|character| *character == '#')
        .count();

    if hashes == 0 || hashes > 6 {
        return None;
    }

    text[hashes..].strip_prefix(' ').map(str::trim_start)
}

fn inline(text: &str) -> Vec<(String, Tone)> {
    let mut spans = Vec::new();
    let mut rest = text;

    while let Some((tone, before, inner, after)) = first_marker(rest) {
        if !before.is_empty() {
            spans.push((escaped(before), Tone::Agent));
        }
        spans.push((escaped(inner), tone));

        // A malformed marker that does not consume its own text would spin this loop.
        if after.len() >= rest.len() {
            break;
        }
        rest = after;
    }

    if !rest.is_empty() {
        spans.push((escaped(rest), Tone::Agent));
    }

    spans
}

fn first_marker(text: &str) -> Option<(Tone, &str, &str, &str)> {
    let strong = find_pair(text, "**");
    let code = find_pair(text, "`");

    match (strong, code) {
        (Some((s_pos, s_before, s_inner, s_after)), Some((c_pos, c_before, c_inner, c_after))) => {
            if s_pos <= c_pos {
                Some((Tone::Strong, s_before, s_inner, s_after))
            } else {
                Some((Tone::Code, c_before, c_inner, c_after))
            }
        },
        (Some((_, before, inner, after)), None) => Some((Tone::Strong, before, inner, after)),
        (None, Some((_, before, inner, after))) => Some((Tone::Code, before, inner, after)),
        (None, None) => None,
    }
}

fn find_pair<'a>(
    text: &'a str,
    marker: &str,
) -> Option<(usize, &'a str, &'a str, &'a str)> {
    let marker_len = marker.len();
    let mut start = 0;

    while let Some(at) = text[start..].find(marker) {
        let absolute = start + at;
        if is_escaped(text, absolute) {
            start = absolute + marker_len;
            continue;
        }

        let after_open = absolute + marker_len;
        if let Some(close) = text[after_open..].find(marker) {
            let close_absolute = after_open + close;
            if is_escaped(text, close_absolute) {
                start = absolute + marker_len;
                continue;
            }
            let before = &text[..absolute];
            let inner = &text[after_open..close_absolute];
            let after = &text[close_absolute + marker_len..];
            return Some((absolute, before, inner, after));
        }
        start = absolute + marker_len;
    }

    None
}

fn is_escaped(
    text: &str,
    position: usize,
) -> bool {
    let prefix = &text[..position];
    prefix.ends_with('\\') && !prefix.ends_with("\\\\")
}

fn escaped(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());

    for character in text.chars() {
        match character {
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            control if control.is_control() => escaped.push('?'),
            other => escaped.push(other),
        }
    }

    escaped
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spans(text: &str) -> Vec<(String, Tone)> {
        let mut md = Markdown::new();

        md.line(text).expect("a normal line").spans
    }

    #[test]
    fn a_plain_line_is_agent() {
        assert_eq!(spans("hello"), vec![("hello".to_owned(), Tone::Agent)]);
    }

    #[test]
    fn bold_wraps_strong_around_the_marker_content() {
        assert_eq!(
            spans("say **this** now"),
            vec![
                ("say ".to_owned(), Tone::Agent),
                ("this".to_owned(), Tone::Strong),
                (" now".to_owned(), Tone::Agent),
            ],
        );
    }

    #[test]
    fn code_wraps_code_around_the_marker_content() {
        assert_eq!(
            spans("use `x` here"),
            vec![
                ("use ".to_owned(), Tone::Agent),
                ("x".to_owned(), Tone::Code),
                (" here".to_owned(), Tone::Agent),
            ],
        );
    }

    #[test]
    fn unclosed_markers_stay_literal() {
        assert_eq!(
            spans("**not closed and `not closed"),
            vec![("**not closed and `not closed".to_owned(), Tone::Agent)],
        );
    }

    #[test]
    fn a_header_line_becomes_strong() {
        assert_eq!(spans("# title"), vec![("title".to_owned(), Tone::Strong)]);
        assert_eq!(
            spans("## section"),
            vec![("section".to_owned(), Tone::Strong)],
        );
        assert_eq!(
            spans("###### six"),
            vec![("six".to_owned(), Tone::Strong)],
            "six hashes are the deepest heading",
        );
        assert_eq!(
            spans("####### seven"),
            vec![("####### seven".to_owned(), Tone::Agent)],
            "seven hashes are not a heading",
        );
        assert_eq!(
            spans("#no space"),
            vec![("#no space".to_owned(), Tone::Agent)],
            "a heading marker needs the space after it",
        );
    }

    #[test]
    fn a_fenced_block_toggles_and_skips_the_fence() {
        let mut md = Markdown::new();
        assert!(md.line("```").is_none());
        assert_eq!(
            md.line("code here").map(|line| (line.spans, line.code)),
            Some((vec![("code here".to_owned(), Tone::Code)], true)),
        );
        assert!(md.line("```").is_none());
        assert_eq!(
            md.line("plain").map(|line| (line.spans, line.code)),
            Some((vec![("plain".to_owned(), Tone::Agent)], false)),
        );
    }
}

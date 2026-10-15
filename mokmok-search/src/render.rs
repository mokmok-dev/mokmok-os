use std::fmt::Write;

use crate::query::Response;

/// One response as the text the model reads.
///
/// The header names how many documents the index holds and how many hits follow it. Each hit is
/// one line of `{score:.3}  {path}`, with `  {snippet}` appended when a snippet was found. A
/// hitless response is its header and nothing else: no query is no hits, never every document.
#[must_use]
pub fn render(response: &Response) -> String {
    let mut text = format!(
        "indexed={} hits={}\n",
        response.indexed,
        response.hits.len()
    );

    for hit in &response.hits {
        let _ = write!(text, "{:.3}  {}", hit.score, hit.path.display());
        if !hit.snippet.is_empty() {
            let _ = write!(text, "  {}", hit.snippet);
        }
        text.push('\n');
    }

    text
}

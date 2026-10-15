//! The boolean query, and what one answer holds.

use std::path::PathBuf;

use tantivy::Term;
use tantivy::query::{
    BooleanQuery, BoostQuery, FuzzyTermQuery, Occur, PhrasePrefixQuery, Query, TermQuery,
};
use tantivy::schema::IndexRecordOption;

use crate::corpus::Fields;

/// The weight of an exact name term against an exact body term: a name the model asked for outranks
/// a body that merely mentions it.
const NAME_WEIGHT: f32 = 3.0;
/// The weight of a one-edit term against an exact one.
const NEAR_WEIGHT: f32 = 0.5;
/// The longest a snippet reaches, in bytes.
const SNIPPET_BYTES: usize = 160;

/// One document one query returned.
#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    /// The path the index holds the document under.
    pub path: PathBuf,
    /// Tantivy's BM25 score, which is what orders the hits.
    pub score: f32,
    /// The first line of the indexed bytes carrying one of the query's words, or nothing when no
    /// line does. A document reached by its name alone carries no snippet.
    pub snippet: String,
}

/// What one query answered.
#[derive(Clone, Debug, PartialEq)]
pub struct Response {
    /// The documents the index held when the query ran.
    pub indexed: u64,
    /// The highest-scoring documents, at most `limit` of them, best first.
    pub hits: Vec<Hit>,
}

/// The boolean query over `words`: every word contributes its exact body and name terms, its
/// one-edit transpose-aware terms on both fields, and the last word a name prefix as well.
#[must_use]
pub fn boolean(
    fields: &Fields,
    words: &[String],
) -> Box<dyn Query> {
    let mut clauses: Vec<(Occur, Box<dyn Query>)> = Vec::new();

    for word in words {
        let body = Term::from_field_text(fields.body, word);
        let name = Term::from_field_text(fields.name, word);

        clauses.push((
            Occur::Should,
            Box::new(TermQuery::new(body.clone(), IndexRecordOption::WithFreqs)),
        ));
        clauses.push((
            Occur::Should,
            Box::new(BoostQuery::new(
                Box::new(TermQuery::new(name.clone(), IndexRecordOption::WithFreqs)),
                NAME_WEIGHT,
            )),
        ));
        clauses.push((
            Occur::Should,
            Box::new(BoostQuery::new(
                Box::new(FuzzyTermQuery::new(body, 1, true)),
                NEAR_WEIGHT,
            )),
        ));
        clauses.push((
            Occur::Should,
            Box::new(BoostQuery::new(
                Box::new(FuzzyTermQuery::new(name, 1, true)),
                NEAR_WEIGHT,
            )),
        ));
    }

    if let Some(last) = words.last() {
        clauses.push((
            Occur::Should,
            Box::new(PhrasePrefixQuery::new(vec![Term::from_field_text(
                fields.name,
                last,
            )])),
        ));
    }

    Box::new(BooleanQuery::new(clauses))
}

/// The first line of `body` carrying one of `words`, cut to a readable length.
///
/// A word of the query is the word of a line when the line, lowercased, contains it: the query was
/// tokenized the same way the field was, so a match on `verify_token` is found by the word
/// `verify`. A line the query reached only fuzzily carries none of its words and yields nothing.
#[must_use]
pub fn snippet(
    body: &str,
    words: &[String],
) -> String {
    for line in body.lines() {
        let lowered = line.to_lowercase();
        if words.iter().any(|word| lowered.contains(word.as_str())) {
            return clipped(line.trim());
        }
    }

    String::new()
}

/// `text` cut to [`SNIPPET_BYTES`] on a character boundary.
fn clipped(text: &str) -> String {
    let end = text.len().min(SNIPPET_BYTES);

    text[..text.floor_char_boundary(end)].trim_end().to_owned()
}

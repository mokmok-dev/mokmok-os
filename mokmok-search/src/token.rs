//! The code tokenizer, one owner for index time and query time.
//!
//! A word is a run of alphanumeric characters, cut again wherever a lowercase or numeric
//! character meets an uppercase one, and lowercased. The cut is what lets a query for
//! `agent runtime` reach `agentRuntime.rs`; the lowercase is what lets `verifyToken`,
//! `verify_token`, and `verify-token` reach the same two words.

use tantivy::tokenizer::{Token, TokenStream, Tokenizer};

fn spans(text: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut open: Option<usize> = None;
    let mut lower = false;

    for (index, character) in text.char_indices() {
        if (!character.is_alphanumeric() || (lower && character.is_uppercase()))
            && let Some(from) = open.take()
        {
            spans.push((from, index));
        }

        if character.is_alphanumeric() {
            open = open.or(Some(index));
            lower = character.is_lowercase() || character.is_numeric();
        } else {
            lower = false;
        }
    }

    if let Some(from) = open {
        spans.push((from, text.len()));
    }

    spans
}

#[must_use]
pub fn tokens(text: &str) -> Vec<String> {
    spans(text)
        .into_iter()
        .map(|(from, to)| text[from..to].to_lowercase())
        .collect()
}

/// Tantivy's view of [`tokens`], so a field is analyzed exactly as a query is tokenized.
#[derive(Clone, Copy, Debug, Default)]
pub struct CodeTokenizer;

impl Tokenizer for CodeTokenizer {
    type TokenStream<'a> = CodeTokenStream<'a>;

    fn token_stream<'a>(
        &'a mut self,
        text: &'a str,
    ) -> Self::TokenStream<'a> {
        CodeTokenStream {
            text,
            spans: spans(text).into_iter(),
            position: 0,
            token: Token::default(),
        }
    }
}

#[derive(Debug)]
pub struct CodeTokenStream<'a> {
    text: &'a str,
    spans: std::vec::IntoIter<(usize, usize)>,
    position: usize,
    token: Token,
}

impl TokenStream for CodeTokenStream<'_> {
    fn advance(&mut self) -> bool {
        let Some((from, to)) = self.spans.next() else {
            return false;
        };

        self.token.text = self.text[from..to].to_lowercase();
        self.token.offset_from = from;
        self.token.offset_to = to;
        self.token.position = self.position;
        self.position += 1;

        true
    }

    fn token(&self) -> &Token {
        &self.token
    }

    fn token_mut(&mut self) -> &mut Token {
        &mut self.token
    }
}

#[cfg(test)]
mod tests {
    use super::{CodeTokenizer, tokens};
    use tantivy::tokenizer::{AsciiFoldingFilter, TextAnalyzer, TokenStream};

    fn analyzed(text: &str) -> Vec<String> {
        let mut analyzer = TextAnalyzer::from(CodeTokenizer);
        let mut stream = analyzer.token_stream(text);
        let mut words = Vec::new();

        // A hang scores as a timeout rather than a catch, so the loop is bounded.
        while words.len() <= text.len() {
            let Some(token) = stream.next() else {
                return words;
            };
            words.push(token.text.clone());
        }

        panic!("{text:?} yielded more words than it has bytes");
    }

    fn step(stream: &mut dyn TokenStream) -> Option<(String, usize, usize, usize)> {
        stream.next().map(|token| {
            (
                token.text.clone(),
                token.offset_from,
                token.offset_to,
                token.position,
            )
        })
    }

    #[test]
    fn the_three_spellings_of_one_identifier_reach_the_same_two_words() {
        assert_eq!(tokens("verify_token"), ["verify", "token"]);
        assert_eq!(tokens("verifyToken"), ["verify", "token"]);
        assert_eq!(tokens("verify-token"), ["verify", "token"]);
    }

    #[test]
    fn a_path_reaches_one_word_per_component() {
        assert_eq!(
            tokens("/srv/checkout/mokmok-search/src/lib.rs"),
            ["srv", "checkout", "mokmok", "search", "src", "lib", "rs"],
        );
    }

    #[test]
    fn an_analyzed_field_and_a_query_are_tokenized_by_the_same_rule() {
        for text in [
            "agentRuntime.rs",
            "agent_runtime.rs",
            "verifyToken",
            "The Name of the Wind",
            "",
            "...",
        ] {
            assert_eq!(analyzed(text), tokens(text), "{text:?}");
        }
        assert_eq!(analyzed("agentRuntime.rs"), ["agent", "runtime", "rs"]);
    }

    #[test]
    fn the_stream_carries_one_word_per_span_at_its_own_position() {
        let mut analyzer = TextAnalyzer::from(CodeTokenizer);
        let mut stream = analyzer.token_stream("agentRuntime.rs");

        assert_eq!(step(&mut stream), Some(("agent".to_owned(), 0, 5, 0)));
        assert_eq!(step(&mut stream), Some(("runtime".to_owned(), 5, 12, 1)));
        assert_eq!(step(&mut stream), Some(("rs".to_owned(), 13, 15, 2)));
        assert_eq!(step(&mut stream), None);
    }

    #[test]
    fn a_filter_stacked_on_the_tokenizer_rewrites_its_words() {
        let mut analyzer = TextAnalyzer::builder(CodeTokenizer)
            .filter(AsciiFoldingFilter)
            .build();
        let mut stream = analyzer.token_stream("Café");

        assert_eq!(step(&mut stream), Some(("cafe".to_owned(), 0, 5, 0)));
        assert_eq!(step(&mut stream), None);
    }
}

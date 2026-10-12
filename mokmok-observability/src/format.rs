//! Renders a log event as one JSON line with every protected value already removed.

use std::fmt;

use serde_json::{Map, Value};
use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::{FmtContext, FormatEvent, FormatFields};
use tracing_subscriber::registry::LookupSpan;

use crate::redaction::Redaction;

/// A [`FormatEvent`] that applies a [`Redaction`] to every value before it reaches the writer.
///
/// The line is JSON so that a value keeps its name, which is the correlation a reader needs once
/// the value itself is gone.
pub struct RedactingFormat {
    redaction: Redaction,
}

impl RedactingFormat {
    /// Renders every event's fields through `redaction`.
    #[must_use]
    pub const fn new(redaction: Redaction) -> Self {
        Self { redaction }
    }
}

/// The fields of one event, redacted as they are recorded.
struct Values<'a> {
    redaction: &'a Redaction,
    fields: Vec<(String, Value)>,
}

impl Values<'_> {
    fn push(
        &mut self,
        field: &Field,
        text: String,
    ) {
        self.fields
            .push((field.name().to_owned(), Value::String(text)));
    }
}

impl Visit for Values<'_> {
    fn record_str(
        &mut self,
        field: &Field,
        value: &str,
    ) {
        let redacted = self.redaction.apply(value).into_owned();
        self.push(field, redacted);
    }

    fn record_debug(
        &mut self,
        field: &Field,
        value: &dyn fmt::Debug,
    ) {
        // A field written as `?value` renders through `Debug` and never reaches `record_str`, so
        // the rendered text is what has to be redacted; leaving it alone would let a caller
        // bypass the policy by choosing the sigil.
        let rendered = self.redaction.apply(&format!("{value:?}")).into_owned();
        self.push(field, rendered);
    }
}

impl<S, N> FormatEvent<S, N> for RedactingFormat
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        _ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> fmt::Result {
        let mut values = Values {
            redaction: &self.redaction,
            fields: Vec::new(),
        };
        event.record(&mut values);

        let mut line = Map::new();
        line.insert(
            "level".to_owned(),
            Value::String(event.metadata().level().to_string()),
        );
        line.insert(
            "target".to_owned(),
            Value::String(event.metadata().target().to_owned()),
        );
        line.extend(values.fields);

        writer.write_fmt(format_args!("{}\n", Value::Object(line)))
    }
}

#[cfg(test)]
mod tests {
    use tracing_subscriber::fmt;
    use tracing_subscriber::prelude::*;

    use super::RedactingFormat;
    use crate::redaction::{Redaction, Rule};
    use crate::support::Capture;

    #[test]
    fn a_secret_rendered_in_a_log_line_is_removed_while_the_trace_id_stays() {
        let capture = Capture::new();
        let redaction = Redaction::from_rules(vec![Rule::new("hunter2")]).unwrap();
        let subscriber = tracing_subscriber::registry().with(
            fmt::layer()
                .with_writer(capture.clone())
                .event_format(RedactingFormat::new(redaction)),
        );

        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(
                trace = "4bf92f3577b34da6a3ce929d0e0e4736",
                presented = "hunter2",
                "login",
            );
        });

        let line = String::from_utf8(capture.bytes()).unwrap();

        assert!(line.contains(r#""trace":"4bf92f3577b34da6a3ce929d0e0e4736""#));
        assert!(line.contains(r#""presented":"[redacted]""#));
        assert!(!line.contains("hunter2"));
    }

    #[test]
    fn a_value_written_through_the_debug_sigil_is_redacted_before_it_is_written() {
        let capture = Capture::new();
        let redaction = Redaction::from_rules(vec![Rule::new("hunter2")]).unwrap();
        let subscriber = tracing_subscriber::registry().with(
            fmt::layer()
                .with_writer(capture.clone())
                .event_format(RedactingFormat::new(redaction)),
        );

        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(presented = ?Presented, "login");
        });

        let line = String::from_utf8(capture.bytes()).unwrap();

        assert!(line.contains(r#""presented":"[redacted]""#));
        assert!(!line.contains("hunter2"));
    }

    struct Presented;

    impl std::fmt::Debug for Presented {
        fn fmt(
            &self,
            formatter: &mut std::fmt::Formatter<'_>,
        ) -> std::fmt::Result {
            formatter.write_str("hunter2")
        }
    }
}

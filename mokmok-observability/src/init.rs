//! Installing the subscriber that filters, redacts, and renders this process's logs.

use tracing_subscriber::Registry;
use tracing_subscriber::filter::EnvFilter;
use tracing_subscriber::prelude::*;
use tracing_subscriber::reload;

use crate::error::Error;
use crate::format::RedactingFormat;
use crate::redaction::Redaction;

/// What [`init`] installs.
///
/// [`Config::default`] renders `info` and higher with nothing redacted.
#[derive(Clone, Debug)]
pub struct Config {
    /// The `EnvFilter` directives that decide which events are rendered.
    pub filter: String,
    /// The rules every rendered field passes through before it is written.
    pub redaction: Redaction,
}

impl Config {
    /// A configuration that renders the events `filter` selects, with `redaction` applied.
    #[must_use]
    pub fn new(
        filter: impl Into<String>,
        redaction: Redaction,
    ) -> Self {
        Self {
            filter: filter.into(),
            redaction,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self::new("info", Redaction::none())
    }
}

/// The handle to an installed subscriber.
///
/// It exists to retune the installed filter in place: [`init`] can only install once per process,
/// so without this handle a level change would need a restart.
pub struct Guard {
    filter: reload::Handle<EnvFilter, Registry>,
}

impl Guard {
    /// Replaces the installed filter directives with `directives`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Filter`] when `directives` do not parse, leaving the installed filter as
    /// it was, and [`Error::Reload`] when the subscriber is gone.
    pub fn reconfigure(
        &self,
        directives: &str,
    ) -> Result<(), Error> {
        let filter = EnvFilter::try_new(directives).map_err(Error::Filter)?;
        self.filter.reload(filter).map_err(Error::Reload)
    }
}

/// Installs the global tracing subscriber: the environment filter, the redaction, and the JSON
/// renderer.
///
/// # Errors
///
/// Returns [`Error::Filter`] when [`Config::filter`] does not parse, and [`Error::Install`] when
/// this process already has a global subscriber.
pub fn init(config: &Config) -> Result<Guard, Error> {
    let (filter, handle) =
        reload::Layer::new(EnvFilter::try_new(&config.filter).map_err(Error::Filter)?);
    let subscriber = tracing_subscriber::registry().with(filter).with(
        tracing_subscriber::fmt::layer()
            // Log lines go to stderr, the stream a caller does not parse; stdout stays the
            // program's own output. systemd captures both into the journal.
            .with_writer(std::io::stderr)
            .event_format(RedactingFormat::new(config.redaction.clone())),
    );

    tracing::subscriber::set_global_default(subscriber).map_err(Error::Install)?;

    Ok(Guard { filter: handle })
}

#[cfg(test)]
mod tests {
    use super::{Config, Guard, init};
    use crate::error::Error;

    #[test]
    fn installing_a_second_global_subscriber_reports_the_failure() {
        let config = Config::default();
        let guard = init(&config).unwrap();

        assert!(guard.reconfigure("mokmok_observability=debug").is_ok());
        assert!(matches!(init(&config), Err(Error::Install(_))));
    }

    #[test]
    fn directives_that_do_not_parse_are_reported_rather_than_reloaded() {
        let (_layer, handle) = tracing_subscriber::reload::Layer::new(
            tracing_subscriber::filter::EnvFilter::new("info"),
        );
        let guard = Guard { filter: handle };

        assert!(matches!(
            guard.reconfigure("mokmok_observability=nonsense"),
            Err(Error::Filter(_))
        ));
    }
}

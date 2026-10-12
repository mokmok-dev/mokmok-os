//! The failures this crate can report, one per boundary it crosses.

/// A failure to install instrumentation, record an audit entry, or deliver an export.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The audit journal could not be opened, written, or read.
    #[error("the audit journal could not be reached")]
    Journal(#[source] std::io::Error),
    /// An entry could not be rendered as a journal line.
    #[error("the audit entry could not be encoded")]
    Encode(#[source] serde_json::Error),
    /// A line in the journal is not an entry this build can read.
    #[error("the audit journal holds a record this version cannot read")]
    Decode(#[source] serde_json::Error),
    /// The exporter refused or could not reach its destination.
    #[error("the exporter could not deliver the snapshot")]
    Export(#[source] Box<dyn std::error::Error + Send + Sync>),
    /// The log filter directives could not be parsed.
    #[error("the log filter directives could not be parsed")]
    Filter(#[source] tracing_subscriber::filter::ParseError),
    /// The installed log filter could not be replaced.
    #[error("the log filter could not be replaced")]
    Reload(#[source] tracing_subscriber::reload::Error),
    /// A global tracing subscriber was already installed, so this one was not.
    #[error("a global tracing subscriber is already installed")]
    Install(#[source] tracing::subscriber::SetGlobalDefaultError),
}

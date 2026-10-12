#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("the event store could not be opened")]
    Open(#[source] rusqlite::Error),
    #[error("the event store file could not be created")]
    Io(#[source] std::io::Error),
    #[error("the event store rejected a statement")]
    Query(#[source] rusqlite::Error),
    #[error("the event store holds schema version {found}, which this build cannot read")]
    Schema { found: i64 },
    #[error("the event store holds a value this version cannot read: {what}")]
    Corrupt { what: &'static str },
    #[error("the database ended the transaction while the event store was working on it")]
    Aborted,
}

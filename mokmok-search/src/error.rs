use std::path::PathBuf;

/// A failure the index itself reported, erased so that no tantivy type crosses this crate's
/// boundary and a caller cannot grow a dependency on the index implementation.
pub type Source = Box<dyn std::error::Error + Send + Sync>;

/// Why a corpus could not be opened, refreshed, or queried.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("the search index could not reach the filesystem")]
    Io(#[source] std::io::Error),
    #[error("the search index refused the operation")]
    Index(#[source] Source),
    #[error("the search root {path:?} cannot be read")]
    Root {
        /// The root that could not be listed.
        path: PathBuf,
    },
}

//! The failures this crate can report, one per boundary it crosses.

use std::path::PathBuf;

/// A failure the index itself reported, erased so that no tantivy type crosses this crate's
/// boundary and a caller cannot grow a dependency on the index implementation.
pub type Source = Box<dyn std::error::Error + Send + Sync>;

/// Why a corpus could not be opened, refreshed, or queried.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The filesystem refused an operation on the tree, the index, or the manifest.
    #[error("the search index could not reach the filesystem")]
    Io(#[source] std::io::Error),
    /// The index refused to be opened, written, or read.
    #[error("the search index refused the operation")]
    Index(#[source] Source),
    /// The root the corpus is bound to cannot be listed.
    #[error("the search root {path:?} cannot be read")]
    Root {
        /// The root that could not be listed.
        path: PathBuf,
    },
}

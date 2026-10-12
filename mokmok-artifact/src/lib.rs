//! Persists large results and generated files outside the event log.
//!
//! An artifact's bytes live in a [`BlobStore`], named by their SHA-256; the event log holds only
//! the reference that names them. Two orders make that safe:
//!
//! - [`BlobStore::put`] writes a staging file, syncs it, and renames it into place. A reader sees
//!   either no file or the whole blob, and a write interrupted before the rename leaves a file no
//!   hash names, which [`BlobStore::read`] cannot return and the [`Collector`] reclaims.
//! - Publication is write-then-reference: [`Reference::new`] refuses a hash whose blob the store
//!   does not hold whole, so a durable reference cannot outlive an interrupted write.
//!
//! A content hash establishes identity, not the right to read. [`Catalog::read`] checks an
//! [`Authorizer`] before it touches the store; [`BlobStore::read`] assumes its caller already holds
//! that right.
//!
//! ```rust
//! use mokmok_artifact::{
//!     Authorizer, BlobStore, Catalog, ContentHash, Error, Reference, ReferenceStore,
//! };
//! use mokmok_protocol::{ArtifactID, Moment, PrincipalID};
//! use uuid::timestamp::{Timestamp, context::NoContext};
//!
//! struct Registry(Vec<Reference>);
//!
//! impl ReferenceStore for Registry {
//!     fn referenced(&self, hash: ContentHash) -> Result<bool, Error> {
//!         Ok(self.0.iter().any(|reference| reference.hash() == hash))
//!     }
//!
//!     fn list(&self) -> Result<Vec<Reference>, Error> {
//!         Ok(self.0.clone())
//!     }
//! }
//!
//! struct Unrestricted;
//!
//! impl Authorizer for Unrestricted {
//!     fn may_read(&self, _principal: PrincipalID, _artifact: ArtifactID) -> bool {
//!         true
//!     }
//! }
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let root = std::env::temp_dir().join(format!("mokmok-artifact-doctest-{}", std::process::id()));
//! let store = BlobStore::open(&root)?;
//! let hash = store.put(b"a large model response")?;
//! let artifact = ArtifactID::new(Timestamp::now(NoContext));
//! let reference = Reference::new(&store, artifact, hash, Moment::EPOCH)?;
//! let catalog = Catalog::new(store, Registry(vec![reference]));
//!
//! let bytes = catalog.read(&Unrestricted, PrincipalID::new(Timestamp::now(NoContext)), artifact)?;
//! assert_eq!(bytes, b"a large model response");
//! std::fs::remove_dir_all(&root)?;
//! # Ok(())
//! # }
//! ```

use mokmok_protocol::ArtifactID;

mod blob;
mod content_hash;
mod filesystem;
mod gc;
mod metadata;
mod retention;

pub use blob::BlobStore;
pub use content_hash::{ContentHash, HexError};
pub use gc::{Collector, Report};
pub use metadata::{Authorizer, Catalog, Reference, ReferenceStore};
pub use retention::Retention;

/// What went wrong while storing, naming, or reading an artifact.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The filesystem refused an operation on the store.
    #[error("the artifact store could not reach the filesystem")]
    Io(#[source] std::io::Error),
    /// No complete blob is stored for the hash.
    #[error("no complete blob is stored for {hash}")]
    NotFound {
        /// The hash that names nothing.
        hash: ContentHash,
    },
    /// The reference registry could not answer.
    #[error("the reference registry could not answer")]
    References(#[source] Box<dyn std::error::Error + Send + Sync>),
    /// No reference names the artifact.
    #[error("nothing references artifact {artifact:?}")]
    Unreferenced {
        /// The artifact that no reference names.
        artifact: ArtifactID,
    },
    /// The authorizer refused the read.
    #[error("the principal may not read artifact {artifact:?}")]
    Denied {
        /// The artifact the principal asked for.
        artifact: ArtifactID,
    },
}

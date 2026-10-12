//! Artifact references and the authority gate over reading them.

use mokmok_protocol::{ArtifactID, Moment, PrincipalID};

use crate::Error;
use crate::blob::BlobStore;
use crate::content_hash::ContentHash;

/// The registry of references the catalog reads and the collector consults.
///
/// A durable adapter answers from storage the collector and the catalog share, so a reference
/// published between two calls is one the next call sees.
pub trait ReferenceStore {
    /// Whether any reference currently names `hash`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::References`] or [`Error::Io`] when the registry cannot answer.
    fn referenced(
        &self,
        hash: ContentHash,
    ) -> Result<bool, Error>;

    /// Every reference the registry holds.
    ///
    /// # Errors
    ///
    /// Returns [`Error::References`] or [`Error::Io`] when the registry cannot be read.
    fn list(&self) -> Result<Vec<Reference>, Error>;
}

/// A durable statement that an artifact's bytes are `hash`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Reference {
    artifact: ArtifactID,
    hash: ContentHash,
    at: Moment,
    size: u64,
}

impl Reference {
    /// Names a blob this store already holds whole.
    ///
    /// The lookup is the write-then-reference order in code: a hash the store never finished
    /// writing names an interrupted `put`, and a reference to it would promise bytes that are not
    /// there. The fields stay private so this is the only way a reference comes to be.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotFound`] when the store holds no complete blob for `hash`, and
    /// [`Error::Io`] when the store cannot be inspected.
    pub fn new(
        store: &BlobStore,
        artifact: ArtifactID,
        hash: ContentHash,
        at: Moment,
    ) -> Result<Self, Error> {
        let size = store.len(hash)?;

        Ok(Self {
            artifact,
            hash,
            at,
            size,
        })
    }

    /// The artifact this reference names.
    #[must_use]
    pub const fn artifact(&self) -> ArtifactID {
        self.artifact
    }

    /// The blob this reference points at.
    #[must_use]
    pub const fn hash(&self) -> ContentHash {
        self.hash
    }

    /// When the reference was published.
    #[must_use]
    pub const fn at(&self) -> Moment {
        self.at
    }

    /// The blob's length, recorded when the reference was published.
    #[must_use]
    pub const fn size(&self) -> u64 {
        self.size
    }
}

/// The authorization port a caller supplies to read through the catalog.
pub trait Authorizer {
    /// Whether `principal` may read `artifact`.
    fn may_read(
        &self,
        principal: PrincipalID,
        artifact: ArtifactID,
    ) -> bool;
}

/// Blobs beside the references that name them, with reads gated on authority.
pub struct Catalog<S: ReferenceStore> {
    store: BlobStore,
    references: S,
}

impl<S: ReferenceStore> Catalog<S> {
    /// Pairs a blob store with its reference registry.
    #[must_use]
    pub const fn new(
        store: BlobStore,
        references: S,
    ) -> Self {
        Self { store, references }
    }

    /// The blob store this catalog reads through.
    #[must_use]
    pub const fn store(&self) -> &BlobStore {
        &self.store
    }

    /// Reads `artifact`'s bytes for `principal`.
    ///
    /// Authority is checked before the registry is touched, so a refused read answers the same
    /// whether or not the artifact exists.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Denied`] when the authorizer refuses, [`Error::Unreferenced`] when no
    /// reference names `artifact`, [`Error::References`] or [`Error::Io`] when the registry cannot
    /// answer, and [`Error::NotFound`] when the referenced blob is gone.
    pub fn read(
        &self,
        authorizer: &impl Authorizer,
        principal: PrincipalID,
        artifact: ArtifactID,
    ) -> Result<Vec<u8>, Error> {
        if !authorizer.may_read(principal, artifact) {
            return Err(Error::Denied { artifact });
        }

        let hash = self
            .references
            .list()?
            .into_iter()
            .find(|reference| reference.artifact() == artifact)
            .map(|reference| reference.hash())
            .ok_or(Error::Unreferenced { artifact })?;

        self.store.read(hash)
    }
}

//! The content-addressed store for blob bytes.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::Error;
use crate::content_hash::ContentHash;
use crate::filesystem;

/// A directory of blobs, each named by its content hash.
///
/// The store owns its root: nothing else may write there, because the collector treats every file
/// whose name is not a hash as a torn write and reclaims it.
pub struct BlobStore {
    root: PathBuf,
}

impl BlobStore {
    /// Opens or creates the store rooted at `root`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] when `root` cannot be created.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, Error> {
        let root = root.into();
        std::fs::create_dir_all(&root).map_err(Error::Io)?;

        Ok(Self { root })
    }

    /// The directory this store owns.
    #[must_use]
    pub fn root(&self) -> &Path {
        self.root.as_path()
    }

    /// Where the blob for `hash` lives, whether or not it is there.
    #[must_use]
    pub fn path(
        &self,
        hash: ContentHash,
    ) -> PathBuf {
        let name = hash.to_string();
        let (prefix, _) = name.split_at(2);

        self.root.join(prefix).join(name)
    }

    /// Stores `bytes` and returns the hash that names them.
    ///
    /// The bytes go to a staging file in the blob's own directory, are synced, and are renamed
    /// into place, so a concurrent reader sees either no blob or all of it. An interrupted write
    /// leaves only the staging file, which no hash names.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] when the blob cannot be written.
    pub fn put(
        &self,
        bytes: &[u8],
    ) -> Result<ContentHash, Error> {
        let hash = ContentHash::of(bytes);
        let target = self.path(hash);

        // A file only ever appears at its hash by the rename below, so one that is there is whole.
        // Writing it again could only race a reader that already opened it.
        if target.is_file() {
            return Ok(hash);
        }

        if let Some(directory) = target.parent() {
            std::fs::create_dir_all(directory).map_err(Error::Io)?;
        }

        filesystem::write_atomic(&target, &staging(&target), bytes)?;

        Ok(hash)
    }

    /// The length of the stored blob, or [`Error::NotFound`] when there is none.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotFound`] when no complete blob is stored for `hash`, and [`Error::Io`]
    /// when the file cannot be inspected.
    pub fn len(
        &self,
        hash: ContentHash,
    ) -> Result<u64, Error> {
        std::fs::metadata(self.path(hash))
            .map(|metadata| metadata.len())
            .map_err(|error| absent(error, hash))
    }

    /// The stored bytes, or [`Error::NotFound`] when there are none.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotFound`] when no complete blob is stored for `hash`, and [`Error::Io`]
    /// when the file cannot be read.
    pub fn read(
        &self,
        hash: ContentHash,
    ) -> Result<Vec<u8>, Error> {
        std::fs::read(self.path(hash)).map_err(|error| absent(error, hash))
    }

    /// Removes the blob, reporting whether it was there.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] when the blob exists and cannot be removed.
    pub fn remove(
        &self,
        hash: ContentHash,
    ) -> Result<bool, Error> {
        filesystem::remove(&self.path(hash))
    }
}

fn absent(
    error: std::io::Error,
    hash: ContentHash,
) -> Error {
    if error.kind() == ErrorKind::NotFound {
        Error::NotFound { hash }
    } else {
        Error::Io(error)
    }
}

fn staging(target: &Path) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);

    let name = format!(
        ".tmp-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed),
    );

    match target.parent() {
        Some(directory) => directory.join(name),
        None => PathBuf::from(name),
    }
}

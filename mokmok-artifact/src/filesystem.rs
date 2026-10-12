//! The filesystem primitives the blob store and the collector are built from.

use std::fs::{File, read_dir};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use mokmok_protocol::Moment;

use crate::Error;
use crate::content_hash::ContentHash;

/// One file found under the store root.
pub struct Entry {
    /// Where the file is.
    pub path: PathBuf,
    /// The hash the file is named by, or `None` when the name is not one.
    ///
    /// A staging file written by an interrupted [`crate::BlobStore::put`] is named by no hash, so
    /// this is how the collector tells a blob from a torn write.
    pub hash: Option<ContentHash>,
    /// The file's length in bytes.
    pub bytes: u64,
    /// The file's last modification time, the age the retention contract reads.
    pub modified: Moment,
}

/// Every file under `root`, in no particular order.
///
/// Directories are descended; a symlink is reported as the file it is, never followed.
///
/// # Errors
///
/// Returns [`Error::Io`] when a directory or a file entry cannot be read. A `root` that does not
/// exist yields no entries, because a store that was never written to has nothing to sweep.
pub fn scan(root: &Path) -> Result<Vec<Entry>, Error> {
    let mut entries = Vec::new();
    descend(root, &mut entries)?;

    Ok(entries)
}

/// Writes `bytes` to `target` by way of `staging`.
///
/// `staging` must share a directory with `target`: the two are one rename apart, which is atomic
/// only within a filesystem, and a reader that never sees `staging`'s half-written contents
/// depends on that. The staging file is synced before the rename and the directory after it, so
/// the name `target` either points at the whole blob or does not exist.
///
/// # Errors
///
/// Returns [`Error::Io`] when the staging file cannot be created, written, synced, or renamed.
pub fn write_atomic(
    target: &Path,
    staging: &Path,
    bytes: &[u8],
) -> Result<(), Error> {
    let mut file = File::create(staging).map_err(Error::Io)?;
    file.write_all(bytes).map_err(Error::Io)?;
    file.sync_all().map_err(Error::Io)?;
    drop(file);

    std::fs::rename(staging, target).map_err(Error::Io)?;

    target.parent().map_or_else(|| Ok(()), sync_directory)
}

/// Removes the file at `path`, reporting whether it existed.
///
/// # Errors
///
/// Returns [`Error::Io`] when the file exists and cannot be removed.
pub fn remove(path: &Path) -> Result<bool, Error> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(Error::Io(error)),
    }
}

/// Reads a filesystem time as the instant the retention contract compares.
///
/// A time before the epoch, which a hostile clock can produce, reads as the epoch rather than
/// failing: an age is then the whole span since the epoch, which keeps the blob.
#[must_use]
pub fn as_moment(time: SystemTime) -> Moment {
    let since = time.duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO);

    Moment::from_millis(u64::try_from(since.as_millis()).unwrap_or(u64::MAX))
}

fn descend(
    directory: &Path,
    entries: &mut Vec<Entry>,
) -> Result<(), Error> {
    let reader = match read_dir(directory) {
        Ok(reader) => reader,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(Error::Io(error)),
    };

    for item in reader {
        let item = item.map_err(Error::Io)?;
        let path = item.path();

        if item.file_type().map_err(Error::Io)?.is_dir() {
            descend(&path, entries)?;
            continue;
        }

        let metadata = item.metadata().map_err(Error::Io)?;
        let hash = item
            .file_name()
            .to_str()
            .and_then(|name| ContentHash::from_str(name).ok());

        entries.push(Entry {
            path,
            hash,
            bytes: metadata.len(),
            modified: as_moment(metadata.modified().map_err(Error::Io)?),
        });
    }

    Ok(())
}

fn sync_directory(directory: &Path) -> Result<(), Error> {
    File::open(directory)
        .and_then(|handle| handle.sync_all())
        .map_err(Error::Io)
}

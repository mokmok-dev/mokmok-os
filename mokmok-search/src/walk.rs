//! The tree walk and the limits it holds.

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use crate::corpus::Limits;
use crate::error::Error;

/// The directories the walk never enters, besides any whose name begins with a dot: build output
/// is not source, and it dwarfs the tree it sits in.
const PRUNED: [&str; 2] = ["target", "node_modules"];

/// One file the walk offers the index, with the stamp a pass compares against the manifest.
#[derive(Clone, Debug)]
pub struct Candidate {
    /// Where the file is.
    pub path: PathBuf,
    /// The path as the index stores it, which the walk has proven is UTF-8.
    pub key: String,
    /// The base name the `name` field is written from.
    pub name: String,
    /// The file's length.
    pub size: u64,
    /// The file's modification time in milliseconds since the epoch, when the platform has one.
    pub modified: Option<u64>,
}

/// What one walk found.
#[derive(Debug, Default)]
pub struct Walk {
    /// The files the walk offers, in the order the directories handed them over.
    pub files: Vec<Candidate>,
    /// The entries the walk refused or stopped on: past a limit, unreadable, or not a plain file.
    pub skipped: u64,
}

impl Walk {
    /// Counts one entry the walk refused, wherever the refusal was decided.
    const fn skip(&mut self) {
        self.skipped += 1;
    }
}

/// Walks `root`, refusing every entry the limits do not let through.
///
/// A symlink is never followed, a directory whose name begins with a dot is never entered, and
/// the store, when it lies inside the root, is skipped by resolved path so that the index never
/// indexes itself. A file past `file_bytes`, a tree past `files`, an unreadable directory, and an
/// entry that is not a plain file are all skipped and counted rather than fatal.
///
/// # Errors
///
/// Returns [`Error::Root`] when `root` itself cannot be listed. An index that quietly covers less
/// than the operator named is worse than no index, so that is the one fatal refusal.
pub fn walk(
    root: &Path,
    store: &Path,
    limits: &Limits,
) -> Result<Walk, Error> {
    let mut walk = Walk::default();
    let mut directories = vec![root.to_path_buf()];
    let store = std::fs::canonicalize(store).ok();

    while let Some(directory) = directories.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            if directory.as_path() == root {
                return Err(Error::Root {
                    path: root.to_path_buf(),
                });
            }
            walk.skip();
            continue;
        };

        for entry in entries {
            let Ok(entry) = entry else {
                walk.skip();
                continue;
            };
            let path = entry.path();
            let Ok(kind) = entry.file_type() else {
                walk.skip();
                continue;
            };

            if kind.is_symlink() {
                walk.skip();
                continue;
            }

            if kind.is_dir() {
                if pruned(&path, store.as_deref()) {
                    continue;
                }
                directories.push(path);
                continue;
            }

            if !kind.is_file() {
                walk.skip();
                continue;
            }

            if walk.files.len() >= limits.files {
                walk.skip();
                return Ok(walk);
            }

            let Ok(metadata) = entry.metadata() else {
                walk.skip();
                continue;
            };
            if metadata.len() > limits.file_bytes {
                walk.skip();
                continue;
            }

            let (Some(key), Some(name)) = (
                path.to_str(),
                path.file_name().and_then(|name| name.to_str()),
            ) else {
                walk.skip();
                continue;
            };

            let candidate = Candidate {
                key: key.to_owned(),
                name: name.to_owned(),
                path,
                size: metadata.len(),
                modified: modified(&metadata),
            };
            walk.files.push(candidate);
        }
    }

    Ok(walk)
}

/// Whether the walk leaves the directory at `path` alone: a name it prunes, or the index's own
/// store, resolved so that a link inside the root cannot alias it.
fn pruned(
    path: &Path,
    store: Option<&Path>,
) -> bool {
    let name = path.file_name().and_then(|name| name.to_str());
    let named = name.is_some_and(|name| name.starts_with('.') || PRUNED.contains(&name));
    let own = store.is_some_and(|store| {
        std::fs::canonicalize(path).is_ok_and(|resolved| resolved.as_path() == store)
    });

    named || own
}

/// The file's modification time in milliseconds since the epoch, when the platform reports one.
fn modified(metadata: &std::fs::Metadata) -> Option<u64> {
    let elapsed = metadata.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;

    u64::try_from(elapsed.as_millis()).ok()
}

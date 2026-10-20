//! The materialized view behind one scope: where it lives, how it was made, and the projection
//! that turns a host path into the path the workload sees.

use std::ffi::OsStr;
use std::io;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};

use crate::plan::open_resolved_dir;
use crate::scope::{Backing, OpenSpec, ScopeError};
use crate::workspace::remove_directory;

/// The filesystem object the workload sees as its world.
///
/// The root is canonical and opens without following a symlink, so every rule the plan later
/// records against it names the object that was validated here.
pub struct View {
    root: PathBuf,
    checkout: PathBuf,
    backing: Backing,
}

impl View {
    /// Materializes the view the spec describes, or reuses the one already there.
    ///
    /// # Errors
    ///
    /// Returns [`ScopeError`] when the name is not one plain component, either root cannot be
    /// prepared, the view would sit inside its own source, or the backing cannot be
    /// materialized or validated. Whatever this call created is removed before returning.
    pub fn open(spec: &OpenSpec) -> Result<Self, ScopeError> {
        let name = &spec.name;

        if name.is_empty() || name.contains('/') || name == "." || name == ".." {
            return Err(ScopeError::InvalidName { name: name.clone() });
        }

        make_directory(&spec.root).map_err(|source| ScopeError::Setup {
            path: spec.root.clone(),
            source,
        })?;
        let base = std::fs::canonicalize(&spec.root).map_err(|source| ScopeError::Setup {
            path: spec.root.clone(),
            source,
        })?;
        let checkout =
            std::fs::canonicalize(&spec.checkout).map_err(|source| ScopeError::Setup {
                path: spec.checkout.clone(),
                source,
            })?;
        if !checkout.is_dir() {
            return Err(ScopeError::Setup {
                path: spec.checkout.clone(),
                source: io::Error::other("the checkout is not a directory"),
            });
        }
        let root = base.join(name);

        // A copy or a worktree inside its own source aliases itself: its `.git` would name the
        // copy it sits in, and the projection could not tell the two apart.
        if root.starts_with(&checkout) && !(spec.backing == Backing::Existing && root == checkout) {
            return Err(ScopeError::Setup {
                path: root.clone(),
                source: io::Error::other(format!("{} sits inside its own source", root.display())),
            });
        }

        let created = match spec.backing {
            Backing::Existing => false,
            Backing::Copy => {
                materialize_copy(&checkout, &root, &base.join(format!(".{name}.copying")))?
            },
            Backing::Worktree => materialize_worktree(&checkout, &root)?,
        };

        let canonical = match std::fs::canonicalize(&root) {
            Ok(canonical) => canonical,
            Err(source) => {
                undo_created(root.as_path(), &checkout, spec.backing, created);
                return Err(ScopeError::Setup { path: root, source });
            },
        };
        if let Err(source) = open_resolved_dir(&canonical) {
            undo_created(root.as_path(), &checkout, spec.backing, created);
            return Err(ScopeError::Setup { path: root, source });
        }

        Ok(Self {
            root: canonical,
            checkout,
            backing: spec.backing,
        })
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn remove(&self) -> Result<(), ScopeError> {
        remove_view(&self.root, &self.checkout, self.backing)
    }

    /// The path the workload sees a resolved host path at.
    ///
    /// A path already inside the view passes through; one under the checkout presents as its
    /// view path, and the checkout itself as the view root. A path the checkout sits under would
    /// hide the checkout, and a checkout path the view does not hold is refused.
    ///
    /// # Errors
    ///
    /// Returns [`ScopeError::Hidden`] and [`ScopeError::Absent`] in those two cases.
    pub fn project(
        &self,
        host: &Path,
    ) -> Result<PathBuf, ScopeError> {
        if host.starts_with(&self.root) {
            return Ok(host.to_path_buf());
        }

        if let Ok(relative) = host.strip_prefix(&self.checkout) {
            // The checkout itself projects onto the view root; `join` would add a separator.
            let view_path = if relative.as_os_str().is_empty() {
                self.root.clone()
            } else {
                self.root.join(relative)
            };

            if view_path.exists() {
                return Ok(view_path);
            }
            return Err(ScopeError::Absent {
                path: host.to_path_buf(),
            });
        }

        if self.checkout.starts_with(host) {
            return Err(ScopeError::Hidden {
                path: host.to_path_buf(),
            });
        }

        Ok(host.to_path_buf())
    }
}

/// Tears a materialized view down, idempotently: what is already gone is success.
///
/// An existing view was never this crate's to remove, so its arm is a no-op. A running workload
/// keeps its plan either way; this only decides what the next open finds.
fn remove_view(
    root: &Path,
    checkout: &Path,
    backing: Backing,
) -> Result<(), ScopeError> {
    match backing {
        Backing::Existing => Ok(()),
        Backing::Copy => remove_directory(root).map_err(|source| ScopeError::Setup {
            path: root.to_path_buf(),
            source,
        }),
        Backing::Worktree => {
            // `git worktree remove` refuses what git no longer tracks; whatever it left, the
            // removal below still clears, and the prune drops the registration either way.
            let _ = std::process::Command::new("git")
                .arg("-C")
                .arg(checkout)
                .args(["worktree", "remove", "--force"])
                .arg(root)
                .output();

            remove_directory(root).map_err(|source| ScopeError::Setup {
                path: root.to_path_buf(),
                source,
            })?;

            // A checkout that is gone has no registration left to prune.
            if !checkout.is_dir() {
                return Ok(());
            }

            let pruned =
                git(checkout, &["worktree", "prune"]).map_err(|source| ScopeError::Setup {
                    path: root.to_path_buf(),
                    source,
                })?;
            if !pruned.status.success() {
                return Err(ScopeError::Setup {
                    path: root.to_path_buf(),
                    source: io::Error::other(stderr_of(&pruned)),
                });
            }

            Ok(())
        },
    }
}

/// Removes back what one open created, and nothing before it.
///
/// A reused view belongs to the open that made it, so only a view this call materialized is
/// removed when the checks after it fail.
fn undo_created(
    root: &Path,
    checkout: &Path,
    backing: Backing,
    created: bool,
) {
    if created {
        let _ = remove_view(root, checkout, backing);
    }
}

/// A nonempty directory is a view an earlier open left behind; an empty one or none gets the
/// checkout copied anew.
fn materialize_copy(
    checkout: &Path,
    root: &Path,
    staging: &Path,
) -> Result<bool, ScopeError> {
    match std::fs::symlink_metadata(root) {
        Ok(metadata) if metadata.is_dir() => {
            if has_entries(root).map_err(|source| ScopeError::Materialize {
                path: root.to_path_buf(),
                source,
            })? {
                return Ok(false);
            }
        },
        Ok(_) => {
            return Err(ScopeError::Materialize {
                path: root.to_path_buf(),
                source: io::Error::other(format!("{} is not a directory", root.display())),
            });
        },
        Err(source) if source.kind() == io::ErrorKind::NotFound => {},
        Err(source) => {
            return Err(ScopeError::Materialize {
                path: root.to_path_buf(),
                source,
            });
        },
    }

    let _ = remove_directory(staging);
    let copied = make_directory(staging).and_then(|()| copy_tree(checkout, staging));

    if let Err(source) = copied {
        let _ = remove_directory(staging);
        return Err(ScopeError::Materialize {
            path: root.to_path_buf(),
            source,
        });
    }

    // The copy lands at the view path with one rename, so an interrupted open leaves a staging
    // directory a later open removes, not half a view that looks reused.
    if let Err(source) = std::fs::rename(staging, root) {
        let _ = remove_directory(staging);
        return Err(ScopeError::Materialize {
            path: root.to_path_buf(),
            source,
        });
    }

    Ok(true)
}

/// Adds the checkout as a detached worktree, or reuses one already registered there.
fn materialize_worktree(
    checkout: &Path,
    root: &Path,
) -> Result<bool, ScopeError> {
    let registered = registered_worktrees(checkout).map_err(|source| ScopeError::Materialize {
        path: root.to_path_buf(),
        source,
    })?;

    let registered_here = registered.iter().any(|listed| {
        *listed == root || std::fs::canonicalize(listed).is_ok_and(|resolved| resolved == root)
    });

    if registered_here {
        if root.is_dir() {
            return Ok(false);
        }

        // A registration whose directory vanished is what a crash between `remove` and `prune`
        // leaves. Pruning first is what lets `add` reuse the name.
        let _ = git(checkout, &["worktree", "prune"]);
    }

    // A directory git does not know about is not a worktree: what is there is replaced.
    if let Err(source) = remove_directory(root) {
        return Err(ScopeError::Materialize {
            path: root.to_path_buf(),
            source,
        });
    }

    let added = std::process::Command::new("git")
        .arg("-C")
        .arg(checkout)
        .args(["worktree", "add", "--detach"])
        .arg(root)
        .arg("HEAD")
        .output();

    match added {
        Ok(output) if output.status.success() => Ok(true),
        Ok(output) => {
            undo_created(root, checkout, Backing::Worktree, true);
            Err(ScopeError::Materialize {
                path: root.to_path_buf(),
                source: io::Error::other(stderr_of(&output)),
            })
        },
        Err(source) => {
            undo_created(root, checkout, Backing::Worktree, true);
            Err(ScopeError::Materialize {
                path: root.to_path_buf(),
                source,
            })
        },
    }
}

fn registered_worktrees(checkout: &Path) -> io::Result<Vec<PathBuf>> {
    let listed = git(checkout, &["worktree", "list", "--porcelain"])?;

    if !listed.status.success() {
        return Err(io::Error::other(stderr_of(&listed)));
    }

    Ok(listed
        .stdout
        .split(|byte| *byte == b'\n')
        .filter_map(|line| {
            line.strip_prefix(b"worktree ".as_slice())
                .map(|rest| PathBuf::from(OsStr::from_bytes(rest)))
        })
        .collect())
}

fn git(
    checkout: &Path,
    arguments: &[&str],
) -> io::Result<std::process::Output> {
    std::process::Command::new("git")
        .arg("-C")
        .arg(checkout)
        .args(arguments)
        .output()
}

fn stderr_of(output: &std::process::Output) -> String {
    let text = String::from_utf8_lossy(&output.stderr)
        .trim_end()
        .to_owned();

    if text.is_empty() {
        format!("git ended with {}", output.status)
    } else {
        text
    }
}

/// Recursively copies directories, regular files, and symlinks, the symlinks as themselves and
/// never followed; any other file type stops the copy.
fn copy_tree(
    source: &Path,
    destination: &Path,
) -> io::Result<()> {
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let from = entry.path();
        let to = destination.join(entry.file_name());
        let file_type = std::fs::symlink_metadata(&from)?.file_type();

        if file_type.is_symlink() {
            std::os::unix::fs::symlink(&std::fs::read_link(&from)?, &to)?;
        } else if file_type.is_dir() {
            copy_directory(&from, &to)?;
        } else if file_type.is_file() {
            std::fs::copy(&from, &to)?;
        } else {
            return Err(io::Error::other(format!(
                "{} is not a directory, file, or symlink",
                from.display()
            )));
        }
    }

    Ok(())
}

fn copy_directory(
    source: &Path,
    destination: &Path,
) -> io::Result<()> {
    // The view is a private working tree: every directory it creates is owner-writable, so a
    // read-only directory in the source neither blocks the copy into it nor blocks teardown.
    std::fs::DirBuilder::new().mode(0o700).create(destination)?;
    copy_tree(source, destination)
}

fn has_entries(directory: &Path) -> io::Result<bool> {
    Ok(std::fs::read_dir(directory)?.next().is_some())
}

/// The shared-root shape the execution workspace uses: recursive, mode 0700 on what it creates.
fn make_directory(path: &Path) -> io::Result<()> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
}

#[cfg(test)]
mod tests {
    use super::{materialize_copy, undo_created};
    use crate::scope::{Backing, ScopeError};

    #[test]
    fn undo_created_removes_only_what_this_open_created() {
        let base = std::env::temp_dir().join(format!("mokmok-view-undo-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let root = base.join("view");
        let checkout = base.join("checkout");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&checkout).unwrap();

        undo_created(&root, &checkout, Backing::Copy, true);
        assert!(!root.exists(), "a created view survived its failed open");

        std::fs::create_dir_all(&root).unwrap();
        undo_created(&root, &checkout, Backing::Copy, false);
        assert!(root.exists(), "an open removed a view it did not create");

        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn an_unreadable_view_path_is_reported_before_the_copy_runs() {
        let base =
            std::env::temp_dir().join(format!("mokmok-view-metadata-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let checkout = base.join("checkout");
        let staging = base.join("staging");
        let blocked = base.join("blocked");
        std::fs::create_dir_all(&checkout).unwrap();
        std::fs::write(&blocked, b"not a directory").unwrap();
        // A socket is neither a file, a directory, nor a symlink, so a copy that runs over it
        // fails with a message the metadata failure does not carry.
        let _listener = std::os::unix::net::UnixListener::bind(checkout.join("socket")).unwrap();
        let root = blocked.join("view");

        let error = materialize_copy(&checkout, &root, &staging).unwrap_err();

        match error {
            ScopeError::Materialize { source, .. } => assert!(
                !source
                    .to_string()
                    .contains("is not a directory, file, or symlink"),
                "the copy ran after an unreadable view path: {source}",
            ),
            other => panic!("the metadata failure was reported as another kind: {other:?}"),
        }
        std::fs::remove_dir_all(&base).unwrap();
    }
}

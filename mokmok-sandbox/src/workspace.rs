use std::io;
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};

use mokmok_protocol::ExecutionID;

/// The directory tree one execution owns: the workload's own scope, and the control plane beside
/// it.
pub struct Workspace {
    directory: PathBuf,
    work: PathBuf,
    ctl: PathBuf,
}

impl Workspace {
    /// Where the execution directory sits before anything is created there.
    #[must_use]
    pub fn directory_path(
        root: &Path,
        execution: ExecutionID,
    ) -> PathBuf {
        root.join(execution.as_uuid().to_string())
    }

    pub fn create(
        root: &Path,
        execution: ExecutionID,
    ) -> io::Result<Self> {
        let directory = Self::directory_path(root, execution);
        let work = directory.join("work");
        let ctl = directory.join("ctl");

        // The root is shared and idempotent; the execution directory is this execution's alone, so
        // an id already in use fails with `AlreadyExists` instead of silently mixing two runs.
        shared(root)?;
        private(&directory)?;

        let created = private(&work).and_then(|()| private(&ctl));

        if let Err(error) = created {
            let _ = std::fs::remove_dir_all(&directory);

            return Err(error);
        }

        Ok(Self {
            directory,
            work,
            ctl,
        })
    }

    #[must_use]
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    #[must_use]
    pub fn work(&self) -> &Path {
        &self.work
    }

    #[must_use]
    pub fn plan(&self) -> PathBuf {
        self.ctl.join("plan.json")
    }

    /// The confiner writes its refusals here, and a workload cannot: `ctl/` is outside every rule
    /// the plan grants.
    #[must_use]
    pub fn status(&self) -> PathBuf {
        self.ctl.join("status.json")
    }

    pub fn remove(&self) -> io::Result<()> {
        remove_directory(&self.directory)
    }
}

/// The execution tree belongs to its owner alone: the fallback root sits under `/tmp`, which every
/// local user can search, and the workload's files, the plan, and the status file are nobody
/// else's business.
fn private(path: &Path) -> io::Result<()> {
    std::fs::DirBuilder::new().mode(0o700).create(path)
}

/// The shared root is idempotent: every execution creates it, and only the execution directory
/// beneath it has to be exclusive.
fn shared(path: &Path) -> io::Result<()> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
}

/// Idempotent: a missing directory is the state removal aims at.
pub fn remove_directory(directory: &Path) -> io::Result<()> {
    match std::fs::remove_dir_all(directory) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

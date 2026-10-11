use std::path::{Component, Path, PathBuf};

use mokmok_protocol::ArtifactID;
use uuid::timestamp::{Timestamp, context::NoContext};

/// A file the workload declared and that is a regular file in the workspace after the run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Artifact {
    id: ArtifactID,
    path: PathBuf,
    bytes: u64,
}

impl Artifact {
    #[must_use]
    pub const fn new(
        id: ArtifactID,
        path: PathBuf,
        bytes: u64,
    ) -> Self {
        Self { id, path, bytes }
    }

    #[must_use]
    pub const fn id(&self) -> ArtifactID {
        self.id
    }

    /// The path the caller declared, relative to the workspace.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub const fn bytes(&self) -> u64 {
        self.bytes
    }
}

#[must_use]
pub fn is_declarable(path: &Path) -> bool {
    path.is_relative()
        && !path.as_os_str().is_empty()
        && !path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
}

pub fn register(
    workspace: &Path,
    declared: &Path,
) -> Option<Artifact> {
    let metadata = std::fs::symlink_metadata(workspace.join(declared)).ok()?;

    // A symlink is not a file under the workspace, and its target's length is not this
    // execution's output.
    if !metadata.file_type().is_file() {
        return None;
    }

    Some(Artifact::new(
        ArtifactID::new(Timestamp::now(NoContext)),
        declared.to_path_buf(),
        metadata.len(),
    ))
}

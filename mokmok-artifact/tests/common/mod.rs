// The `cfg(test)` module is what tells the `allow-unwrap-in-tests`, `allow-expect-in-tests`, and
// `allow-panic-in-tests` settings in `.clippy.toml` that this harness is test code. A helper only
// some integration test targets reach would otherwise be dead code in the others.
#[cfg(test)]
#[allow(dead_code)]
pub mod tests {
    use mokmok_artifact::{Authorizer, ContentHash, Error, Reference, ReferenceStore};
    use mokmok_protocol::{ArtifactID, Moment, PrincipalID};
    use std::path::{Path, PathBuf};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    use uuid::timestamp::{Timestamp, context::NoContext};

    /// A directory of this test's own, removed when the test ends.
    pub struct Scratch {
        path: PathBuf,
    }

    impl Scratch {
        pub fn new(name: &str) -> Self {
            let pid = std::process::id();
            let path = std::env::temp_dir().join(format!("mokmok-artifact-{pid}-{name}"));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();

            Self { path }
        }

        pub fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    pub fn artifact() -> ArtifactID {
        ArtifactID::new(Timestamp::now(NoContext))
    }

    pub fn principal() -> PrincipalID {
        PrincipalID::new(Timestamp::now(NoContext))
    }

    pub fn now() -> Moment {
        let since = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();

        Moment::from_millis(u64::try_from(since.as_millis()).unwrap())
    }

    pub fn after(seconds: u64) -> Moment {
        now().saturating_add(Duration::from_secs(seconds))
    }

    /// A registry over a fixed list of references.
    pub struct MemoryReferences {
        references: Vec<Reference>,
    }

    impl MemoryReferences {
        pub fn holding(references: Vec<Reference>) -> Self {
            Self { references }
        }
    }

    impl ReferenceStore for MemoryReferences {
        fn referenced(
            &self,
            hash: ContentHash,
        ) -> Result<bool, Error> {
            Ok(self
                .references
                .iter()
                .any(|reference| reference.hash() == hash))
        }

        fn list(&self) -> Result<Vec<Reference>, Error> {
            Ok(self.references.clone())
        }
    }

    pub struct Allow;

    impl Authorizer for Allow {
        fn may_read(
            &self,
            _principal: PrincipalID,
            _artifact: ArtifactID,
        ) -> bool {
            true
        }
    }

    pub struct Deny;

    impl Authorizer for Deny {
        fn may_read(
            &self,
            _principal: PrincipalID,
            _artifact: ArtifactID,
        ) -> bool {
            false
        }
    }
}

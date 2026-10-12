use std::io;
use std::path::{Path, PathBuf};

/// The access a capability or an exposure names, as a set of three component rights.
///
/// A set rather than an enum because the gate composes rights component-wise: a read-write
/// capability covers a read exposure and a write exposure, and neither alone.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Access(u8);

impl Access {
    /// Open a file and read it, and list and traverse a directory.
    pub const READ: Self = Self(0b001);
    /// Create, write, rename, and remove entries, and truncate a file.
    pub const WRITE: Self = Self(0b010);
    /// Replace the process image with a file.
    pub const EXECUTE: Self = Self(0b100);
    pub const READ_WRITE: Self = Self(0b011);
    pub const READ_EXECUTE: Self = Self(0b101);
    pub const READ_WRITE_EXECUTE: Self = Self(0b111);

    #[must_use]
    pub const fn covers(
        self,
        needed: Self,
    ) -> bool {
        self.0 & needed.0 == needed.0
    }

    /// The rights in this set together with the rights in `other`.
    ///
    /// The caller that decides authority one right at a time needs to name what it decided as one
    /// set, and the rights are a set: combining two of them is the operation its own gate is
    /// written against.
    #[must_use]
    pub const fn union(
        self,
        other: Self,
    ) -> Self {
        Self(self.0 | other.0)
    }
}

// The wire form is the bit set. A plan that names no rights, or a right this version does not
// know, would let a forged plan widen a grant, so both are refused where the plan is parsed.
impl serde::Serialize for Access {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_u8(self.0)
    }
}

impl<'de> serde::Deserialize<'de> for Access {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let bits = u8::deserialize(deserializer)?;

        if bits == 0 || bits & !Self::READ_WRITE_EXECUTE.0 != 0 {
            return Err(serde::de::Error::custom(
                "access is a nonempty subset of read, write, and execute",
            ));
        }

        Ok(Self(bits))
    }
}

/// A path a caller asks the sandbox to expose, with the access it wants there.
///
/// The path is a name for an object, not the object itself. Both backends resolve it before they
/// gate, and one that cannot be resolved is refused rather than dropped: an exposure is a grant,
/// and a name that reaches nothing is not a grant.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Exposure {
    path: PathBuf,
    access: Access,
}

impl Exposure {
    #[must_use]
    pub fn read(path: impl Into<PathBuf>) -> Self {
        Self::path(path, Access::READ)
    }

    #[must_use]
    pub fn read_write(path: impl Into<PathBuf>) -> Self {
        Self::path(path, Access::READ_WRITE)
    }

    #[must_use]
    pub fn read_execute(path: impl Into<PathBuf>) -> Self {
        Self::path(path, Access::READ_EXECUTE)
    }

    #[must_use]
    pub fn path(
        path: impl Into<PathBuf>,
        access: Access,
    ) -> Self {
        Self {
            path: path.into(),
            access,
        }
    }

    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub const fn access(&self) -> Access {
        self.access
    }
}

/// Authority for one path, issued by whoever holds it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Capability {
    path: PathBuf,
    access: Access,
}

impl Capability {
    #[must_use]
    pub fn issued(
        path: impl Into<PathBuf>,
        access: Access,
    ) -> Self {
        Self {
            path: path.into(),
            access,
        }
    }

    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub const fn access(&self) -> Access {
        self.access
    }
}

/// The mechanical gate: the first exposure the issued set does not cover.
///
/// A capability at `p` with access `a` covers an exposure at `q` when `a.covers(needed)` and
/// either `p == q`, or `p` is a proper ancestor of `q` compared by path components and `p` is an
/// existing directory. A capability naming a file therefore does not cover paths beneath its
/// name, and `/a/b` does not cover `/a/bc`. Paths are never compared as strings. Refusing when
/// the filesystem cannot confirm `p` is a directory is the fail-closed direction.
#[must_use]
pub fn uncovered<'a>(
    exposures: &'a [Exposure],
    issued: &[Capability],
) -> Option<&'a Exposure> {
    uncovered_index(exposures, issued).map(|index| &exposures[index])
}

/// The position of the first exposure the issued set does not cover.
///
/// The gate refuses with both the requested exposure and the resolved path it did not cover, and
/// those are two lists held side by side; the position is what pairs them without a second
/// comparison.
#[must_use]
pub fn uncovered_index(
    exposures: &[Exposure],
    issued: &[Capability],
) -> Option<usize> {
    exposures
        .iter()
        .position(|exposure| !issued.iter().any(|capability| covers(capability, exposure)))
}

/// The one resolution the gate and the plan share.
///
/// `canonicalize` follows every symlink, so two names for one object resolve to the same path and
/// the gate compares resolved objects rather than the names a caller wrote.
///
/// # Errors
///
/// Returns the `canonicalize` error: `NotFound` for a name that is not there, `ELOOP` for a
/// symlink loop, and the permissions error for a component that cannot be searched.
pub fn resolve(path: &Path) -> io::Result<PathBuf> {
    std::fs::canonicalize(path)
}

fn covers(
    capability: &Capability,
    exposure: &Exposure,
) -> bool {
    if !capability.access.covers(exposure.access) {
        return false;
    }

    // `Path::starts_with` compares whole components, so `/a/b` is not a prefix of `/a/bc`.
    capability.path == exposure.path
        || (exposure.path.starts_with(&capability.path) && capability.path.is_dir())
}

#[cfg(test)]
mod tests {
    use super::{Access, Capability, Exposure, uncovered};
    use std::path::PathBuf;

    #[test]
    fn covers_is_component_wise_over_the_three_rights() {
        assert!(Access::READ_WRITE_EXECUTE.covers(Access::READ_WRITE_EXECUTE));
        assert!(Access::READ_WRITE.covers(Access::READ));
        assert!(Access::READ_WRITE.covers(Access::WRITE));
        assert!(!Access::READ.covers(Access::READ_WRITE));
        assert!(!Access::READ_WRITE.covers(Access::READ_EXECUTE));
        assert!(!Access::READ.covers(Access::READ_EXECUTE));
    }

    #[test]
    fn a_union_holds_exactly_the_rights_of_both() {
        let combined = Access::READ_EXECUTE.union(Access::WRITE);

        assert!(combined.covers(Access::READ_WRITE_EXECUTE));
        assert_eq!(combined.union(Access::READ_EXECUTE), combined);
        assert!(!Access::READ.union(Access::READ).covers(Access::WRITE));
    }

    #[test]
    fn a_capability_for_a_file_covers_only_that_name() {
        let file = std::env::temp_dir().join(format!("mokmok-gate-{}", std::process::id()));
        std::fs::write(&file, b"x").unwrap();
        let inside = file.join("child");
        let exposures = [Exposure::read(&inside)];
        let issued = [Capability::issued(&file, Access::READ)];

        let declined = uncovered(&exposures, &issued).unwrap();

        assert_eq!(declined, &exposures[0]);
        std::fs::remove_file(&file).unwrap();
    }

    #[test]
    fn an_untouched_exposure_has_no_cover() {
        let exposures = [Exposure::read(PathBuf::from("/a/b"))];
        let issued = [Capability::issued("/a", Access::READ)];

        // `/a` cannot be confirmed a directory, so the ancestor rule does not apply.
        assert!(uncovered(&exposures, &issued).is_some());
    }
}

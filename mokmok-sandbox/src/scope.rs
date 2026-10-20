//! A grant ledger over an isolated view of one checkout.
//!
//! The sandbox never widens, so the ledger holds only what an issuer handed over: [`Scope::record`]
//! takes an already-constructed [`Capability`] and never mints one from a path and an access set.
//! A snapshot fixes one spawn's world, and a [`Retired`] reports what revoking a grant left
//! behind.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use crate::capability::{self, Access, Capability, Exposure};
use crate::request::Request;
use crate::run::Run;
use crate::sandbox::{Refusal, Sandbox};
use crate::view::View;

/// One recorded grant's id, stable for the scope that issued it.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct GrantID(u64);

/// How the view is created from its checkout.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Backing {
    /// A detached git worktree of the checkout, which must be a repository.
    ///
    /// The repository's object directory stays outside the view's exposures, so a workload that
    /// runs git inside the view cannot reach it; [`Backing::Copy`] carries a self-contained
    /// `.git` instead.
    Worktree,
    /// A recursive copy of the checkout, uncommitted state included.
    Copy,
    /// An existing directory used as it stands, created and removed by nobody here.
    Existing,
}

impl Backing {
    /// The natural backing for a checkout: a git repository materializes as a worktree, and
    /// anything else as a copy.
    #[must_use]
    pub fn for_checkout(checkout: &Path) -> Self {
        if checkout.join(".git").exists() {
            Self::Worktree
        } else {
            Self::Copy
        }
    }
}

/// What the view is made from.
#[derive(Clone, Debug)]
pub struct OpenSpec {
    /// The directory the view is created under, outside the checkout.
    pub root: PathBuf,
    /// The view's own name: one plain path component, so the view lives at `root/<name>`.
    pub name: String,
    /// The source the view materializes.
    pub checkout: PathBuf,
    /// How the view is materialized.
    pub backing: Backing,
}

/// One recorded grant: the host path its capability named, the view path it presents as, and the
/// access the issuer handed over.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Grant {
    id: GrantID,
    host: PathBuf,
    path: PathBuf,
    access: Access,
}

impl Grant {
    /// The id that names this grant to [`Scope::revoke`].
    #[must_use]
    pub const fn id(&self) -> GrantID {
        self.id
    }

    /// The host path the capability named.
    #[must_use]
    pub fn host(&self) -> &Path {
        &self.host
    }

    /// The path the workload sees this grant at.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub const fn access(&self) -> Access {
        self.access
    }
}

/// The ledger at one moment, held for one spawn.
#[derive(Clone, Debug)]
pub struct Snapshot {
    generation: u64,
    rows: Vec<Grant>,
}

impl Snapshot {
    /// The generation the snapshot was taken at.
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// The recorded grants, in the order they were recorded.
    #[must_use]
    pub fn rows(&self) -> &[Grant] {
        &self.rows
    }

    /// The exposures one spawn takes: the rows' paths with the union of the rights recorded at
    /// each, in `BTreeMap` order.
    #[must_use]
    pub fn exposures(&self) -> Vec<Exposure> {
        self.by_path()
            .into_iter()
            .map(|(path, access)| Exposure::path(path, access))
            .collect()
    }

    /// One issued capability per exposure, in the same order and at the same paths.
    #[must_use]
    pub fn issued(&self) -> Vec<Capability> {
        self.by_path()
            .into_iter()
            .map(|(path, access)| Capability::issued(path, access))
            .collect()
    }

    fn by_path(&self) -> BTreeMap<PathBuf, Access> {
        let mut by_path = BTreeMap::new();

        for row in &self.rows {
            let access = by_path.entry(row.path.clone()).or_insert(row.access);
            *access = access.union(row.access);
        }

        by_path
    }
}

/// What remains of a revoked grant.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Retired {
    /// No remaining grant reaches the revoked path with the rights it had.
    Gone,
    /// The revoked rights are still covered by the remaining grants.
    ///
    /// Landlock unions rules that overlap, so a workload this scope spawns later loses nothing
    /// until the covering grants go too. `by` names the deepest of them and `access` is their
    /// union, which may take more than one grant to cover what was revoked.
    Covered {
        /// The host, or view, path of the deeper grant that still covers.
        by: PathBuf,
        /// The rights that still hold at the revoked path.
        access: Access,
    },
}

/// Why a scope could not be opened, recorded against, or closed.
#[derive(Debug, thiserror::Error)]
pub enum ScopeError {
    /// A directory the view needs could not be prepared or validated.
    #[error("{} could not be prepared: {source}", path.display())]
    Setup { path: PathBuf, source: io::Error },
    /// The view's contents could not be materialized.
    #[error("the view at {} could not be materialized: {source}", path.display())]
    Materialize { path: PathBuf, source: io::Error },
    /// The recorded path resolves to nothing.
    #[error("the path {} does not resolve: {source}", path.display())]
    Unresolved { path: PathBuf, source: io::Error },
    /// The checkout sits under the recorded path: presenting it would hide the checkout.
    #[error("the checkout sits beneath {}, so the view cannot present it", path.display())]
    Hidden { path: PathBuf },
    /// The recorded path exists at the checkout but not in the view.
    #[error("{} is not in the view", path.display())]
    Absent { path: PathBuf },
    /// The view's name is not one plain path component.
    #[error("{name:?} is not one plain path component")]
    InvalidName { name: String },
}

/// A filesystem view of one checkout with a ledger of what its workload may reach.
///
/// The view is materialized at [`Scope::open`] from the [`Backing`] the spec names, and the
/// ledger starts empty: the caller records the capabilities it was issued. What a workload sees
/// is the snapshot a spawn fixed, not the ledger's current state.
pub struct Scope {
    view: View,
    entries: Vec<Grant>,
    next_id: u64,
    generation: u64,
}

impl Scope {
    /// Opens a view from the spec and an empty ledger.
    ///
    /// # Errors
    ///
    /// Returns [`ScopeError::InvalidName`] when the name is not one plain component, and the
    /// remaining variants when the view cannot be materialized or validated.
    pub fn open(spec: &OpenSpec) -> Result<Self, ScopeError> {
        let view = View::open(spec)?;

        Ok(Self {
            view,
            entries: Vec::new(),
            next_id: 1,
            generation: 0,
        })
    }

    /// Records one capability the issuer already handed over.
    ///
    /// The ledger never mints authority from a path and an access set: a capability arrives
    /// constructed, and a grant it holds is whatever its access says. The view projects the
    /// capability's path, and the grant is recorded under the id this call returns.
    ///
    /// Recording the same path twice is allowed; the snapshot unions the rights.
    ///
    /// # Errors
    ///
    /// Returns [`ScopeError::Unresolved`] when the capability's path resolves to nothing, and
    /// [`ScopeError::Hidden`] or [`ScopeError::Absent`] when the view cannot present it.
    pub fn record(
        &mut self,
        capability: &Capability,
    ) -> Result<GrantID, ScopeError> {
        let host =
            capability::resolve(capability.as_path()).map_err(|source| ScopeError::Unresolved {
                path: capability.as_path().to_path_buf(),
                source,
            })?;
        let path = self.view.project(&host)?;

        let id = GrantID(self.next_id);
        self.next_id += 1;
        self.generation += 1;
        self.entries.push(Grant {
            id,
            host,
            path,
            access: capability.access(),
        });

        Ok(id)
    }

    /// Ends one grant.
    ///
    /// Landlock rules are fixed for the life of a process, so a workload that is already running
    /// keeps the plan it was spawned under; a revocation reaches the next spawn, not the one in
    /// flight.
    ///
    /// A missing or already-revoked id is `None`, which makes revoking twice a no-op.
    ///
    /// Landlock unions rules that overlap, so a revoked grant the remaining grants still cover
    /// changes nothing for the workload: [`Retired::Covered`] names the deeper path whose rights
    /// still hold what the revoked grant had.
    pub fn revoke(
        &mut self,
        id: GrantID,
    ) -> Option<Retired> {
        let index = self.entries.iter().position(|entry| entry.id == id)?;
        let removed = self.entries.remove(index);
        self.generation += 1;

        // Coverage is decided in view coordinates: the snapshot unions rules by that path, so a
        // remaining rule reaches the revoked object exactly when its view path is an ancestor.
        // Reporting the host path keeps the answer useful to the caller that recorded it.
        let covering: Vec<&Grant> = self
            .entries
            .iter()
            .filter(|entry| removed.path.starts_with(&entry.path))
            .collect();
        let access = union(covering.iter().map(|entry| entry.access));
        let deepest = covering
            .iter()
            .max_by_key(|entry| entry.path.components().count())
            .map(|entry| entry.host.clone());

        Some(match (deepest, access) {
            (Some(by), Some(access)) if access.covers(removed.access) => {
                Retired::Covered { by, access }
            },
            _ => Retired::Gone,
        })
    }

    /// The ledger and its generation, as one value.
    #[must_use]
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            generation: self.generation,
            rows: self.entries.clone(),
        }
    }

    /// The rights that hold at a host or view path, from the ledger as it stands now.
    ///
    /// `None` when no entry is an ancestor of, or equals, the query.
    #[must_use]
    pub fn effective(
        &self,
        path: &Path,
    ) -> Option<Access> {
        let query = capability::resolve(path).unwrap_or_else(|_| path.to_path_buf());

        union(
            self.entries
                .iter()
                .filter(|entry| query.starts_with(&entry.host) || query.starts_with(&entry.path))
                .map(|entry| entry.access),
        )
    }

    /// The path the workload lives at.
    #[must_use]
    pub fn root(&self) -> &Path {
        self.view.root()
    }

    /// How many grants were recorded or revoked since `open`.
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// Starts one workload whose world is the view.
    ///
    /// The request's exposures are replaced with the snapshot's and its working directory with
    /// the view root; what the caller set for either is discarded. It is the snapshot, not the
    /// ledger's moment of spawning, that the sandbox gates against.
    ///
    /// # Errors
    ///
    /// Returns [`Refusal`] when the sandbox declines the request, including when the snapshot's
    /// exposures do not cover the view root.
    pub fn spawn(
        &self,
        sandbox: &impl Sandbox,
        mut request: Request,
    ) -> Result<Run, Refusal> {
        let snapshot = self.snapshot();

        request.exposures = snapshot.exposures();
        request.workdir = Some(self.view.root().to_path_buf());
        sandbox.spawn(&request, &snapshot.issued())
    }

    /// Tears the view down.
    ///
    /// A worktree is unregistered and removed, a copy removed, an existing view left alone;
    /// everything already gone, including a view removed by hand, is success. There is no `Drop`:
    /// a scope dropped without `close` leaves its view for the next `open` to reuse or replace.
    /// Closing while a run is still live removes that run's working tree, so stop or wait first.
    ///
    /// # Errors
    ///
    /// Returns [`ScopeError::Setup`] when what this call must remove cannot be removed.
    pub fn close(self) -> Result<(), ScopeError> {
        self.view.remove()
    }
}

/// The union of some accesses, or `None` when there are none to union.
fn union(mut accesses: impl Iterator<Item = Access>) -> Option<Access> {
    let first = accesses.next()?;

    Some(accesses.fold(first, Access::union))
}

use core::fmt;

use mokmok_protocol::{Moment, PrincipalID};

use crate::decision::Request;
use crate::permission::{ActionSet, Permission};

/// Identifies one grant an authority issued.
#[derive(
    Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, serde::Deserialize, serde::Serialize,
)]
#[serde(transparent)]
pub struct GrantId(u64);

impl GrantId {
    #[must_use]
    pub const fn from_value(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn value(self) -> u64 {
        self.0
    }
}

/// One permission an authority issued to one principal.
///
/// A grant has no public constructor and no `Deserialize`. The only expression that builds one is
/// an [`Authority`](crate::Authority) issuing it against a grant the caller already holds, so a
/// permission cannot be widened by writing one down, and every grant names the parent it was
/// attenuated from.
// Serialize only: a `Deserialize` impl would be a constructor, and a constructor is exactly what
// lets a caller write down authority nobody issued.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct Grant {
    id: GrantId,
    principal: PrincipalID,
    permission: Permission,
    not_after: Option<Moment>,
    parent: Option<GrantId>,
}

impl Grant {
    #[must_use]
    pub const fn id(&self) -> GrantId {
        self.id
    }

    #[must_use]
    pub const fn principal(&self) -> PrincipalID {
        self.principal
    }

    #[must_use]
    pub const fn permission(&self) -> &Permission {
        &self.permission
    }

    /// The last instant at which the grant authorizes, or `None` when it never lapses.
    #[must_use]
    pub const fn not_after(&self) -> Option<Moment> {
        self.not_after
    }

    /// The grant this one was attenuated from, or `None` for a root.
    #[must_use]
    pub const fn parent(&self) -> Option<GrantId> {
        self.parent
    }
}

/// Builds a grant for an authority to hold.
///
/// A free function rather than an associated one: [`Grant`] is re-exported, so a `pub` inherent
/// constructor would join the crate's public surface, and that is the surface attenuation depends
/// on staying shut.
#[must_use]
pub const fn minted(
    id: GrantId,
    principal: PrincipalID,
    permission: Permission,
    not_after: Option<Moment>,
    parent: Option<GrantId>,
) -> Grant {
    Grant {
        id,
        principal,
        permission,
        not_after,
        parent,
    }
}

/// Whether a child expiry would outlive its parent's.
///
/// An unbounded child under a bounded parent outlives it; nothing outlives an unbounded parent.
/// The parent's instant is inclusive, so a child may expire exactly with it.
#[must_use]
pub fn outlives(
    not_after: Option<Moment>,
    parent: Option<Moment>,
) -> bool {
    parent.is_some_and(|bound| not_after.is_none_or(|child| child > bound))
}

/// Whether the grant names the principal, the action, and the resource the request asks for,
/// ignoring lapse, revocation, and any approval binding.
#[must_use]
pub fn mentions(
    grant: &Grant,
    request: &Request,
) -> bool {
    grant.principal == request.principal
        && grant.permission.resource() == &request.resource
        && grant
            .permission
            .actions()
            .covers(ActionSet::of(request.action))
}

/// Why the only path that mints a grant refused to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Attenuation {
    /// The requested permission is not a subset of the parent grant's.
    NotASubset,
    /// The requested expiry outlives the parent grant's.
    OutlivesParent,
    /// The requested expiry has already passed.
    AlreadyExpired,
    /// The parent grant was not issued by this authority.
    UnknownParent,
}

impl fmt::Display for Attenuation {
    fn fmt(
        &self,
        formatter: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        formatter.write_str(match self {
            Self::NotASubset => "the requested permission is not a subset of the parent grant's",
            Self::OutlivesParent => "the requested expiry outlives the parent grant's",
            Self::AlreadyExpired => "the requested expiry is not in the future",
            Self::UnknownParent => "the parent grant was not issued by this authority",
        })
    }
}

impl core::error::Error for Attenuation {}

use alloc::collections::BTreeMap;

use mokmok_protocol::{Moment, PrincipalID};

use crate::grants::{Grant, GrantId};

/// The revocations one authority recorded, each with the instant the caller supplied.
///
/// A revocation is kept rather than applied by deleting the grant, because a decision that names
/// the instant is the record of why authority was withdrawn; deleting the grant would leave the
/// same denial with nothing behind it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Revocations {
    grants: BTreeMap<GrantId, Moment>,
    principals: BTreeMap<PrincipalID, Moment>,
}

impl Revocations {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            grants: BTreeMap::new(),
            principals: BTreeMap::new(),
        }
    }

    /// Records a grant revocation, reporting whether this call was the one that recorded it.
    pub fn grant(
        &mut self,
        id: GrantId,
        at: Moment,
    ) -> bool {
        self.grants.insert(id, at).is_none()
    }

    /// Records a principal revocation. Every grant the principal holds, and everything issued
    /// from those grants, is denied from `at` on.
    pub fn principal(
        &mut self,
        id: PrincipalID,
        at: Moment,
    ) {
        self.principals.insert(id, at);
    }

    /// The instant a revocation blocked this grant, whether it names the grant or its principal.
    #[must_use]
    pub fn blocked(
        &self,
        grant: &Grant,
    ) -> Option<Moment> {
        self.grants
            .get(&grant.id())
            .or_else(|| self.principals.get(&grant.principal()))
            .copied()
    }
}

impl Default for Revocations {
    fn default() -> Self {
        Self::new()
    }
}

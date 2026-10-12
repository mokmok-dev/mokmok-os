use alloc::collections::BTreeMap;

use mokmok_protocol::{Moment, PrincipalID};

use crate::approvals::{ApprovalError, Requirement, authorized_operation, requirement_for};
use crate::decision::{Decision, Denial, DenialReason, Request, severity};
use crate::grants::{Attenuation, Grant, GrantId, mentions, minted, outlives};
use crate::permission::{ActionSet, Permission};
use crate::policy::{Policy, Verdict};
use crate::principals::Principal;
use crate::revocations::Revocations;

/// One grant an authority holds, with the instant it was issued and the requirement it answers.
pub struct Held {
    grant: Grant,
    at: Moment,
    binding: Option<Requirement>,
}

/// The authority a decision is made against.
///
/// The value holds the policy, the grants issued so far, and the revocations recorded against
/// them. It reads no clock: `now` is a parameter of every call that needs one, so the same call
/// sequence reaches the same decision wherever it runs, and a test can pin the boundary instant
/// exactly.
pub struct Authority {
    policy: Policy,
    since: Moment,
    principals: BTreeMap<PrincipalID, Principal>,
    grants: BTreeMap<GrantId, Held>,
    revocations: Revocations,
    next: u64,
}

impl Authority {
    /// An authority holding no grants, adopting `policy` at `now`.
    #[must_use]
    pub const fn new(
        policy: Policy,
        now: Moment,
    ) -> Self {
        Self {
            policy,
            since: now,
            principals: BTreeMap::new(),
            grants: BTreeMap::new(),
            revocations: Revocations::new(),
            next: 0,
        }
    }

    /// The instant this authority's record begins.
    #[must_use]
    pub const fn since(&self) -> Moment {
        self.since
    }

    /// The principal this authority admitted, or `None` when it never admitted that id.
    ///
    /// The name and kind are kept beside the id rather than only inside the grant, so a durable
    /// record of a decision can say what it decided about and not just which id it decided about.
    #[must_use]
    pub fn principal(
        &self,
        principal: PrincipalID,
    ) -> Option<&Principal> {
        self.principals.get(&principal)
    }

    /// The instant a grant was issued, or `None` when this authority never issued it.
    #[must_use]
    pub fn issued_at(
        &self,
        grant: GrantId,
    ) -> Option<Moment> {
        self.grants.get(&grant).map(|held| held.at)
    }

    /// Issues the root grant for a principal: the first authority it holds, and the one every
    /// grant it later delegates descends from.
    ///
    /// A root never lapses on its own. Only [`Authority::revoke`] and
    /// [`Authority::revoke_principal`] end it, which is why setup is not a decision.
    #[must_use]
    pub fn root(
        &mut self,
        principal: Principal,
        permission: Permission,
        now: Moment,
    ) -> Grant {
        let id = principal.id();
        self.principals.insert(id, principal);

        self.mint(id, permission, None, None, None, now)
    }

    /// Issues a grant attenuated from `parent`, the only path that mints one.
    ///
    /// The requested permission must be a subset of the parent's and must not outlive it, so a
    /// caller cannot issue authority it does not already hold. A parent that has already lapsed
    /// or been revoked still issues here: the decision consults the whole chain, so the child is
    /// dead on arrival rather than a widening.
    ///
    /// # Errors
    ///
    /// Returns [`Attenuation::UnknownParent`] when this authority never issued `parent`,
    /// [`Attenuation::NotASubset`] when `permission` admits anything the parent's does not,
    /// [`Attenuation::OutlivesParent`] when `not_after` is later than the parent's, and
    /// [`Attenuation::AlreadyExpired`] when `not_after` is not in the future at `now`.
    pub fn issue(
        &mut self,
        parent: &Grant,
        principal: PrincipalID,
        permission: Permission,
        not_after: Option<Moment>,
        now: Moment,
    ) -> Result<Grant, Attenuation> {
        let (parent_id, binding) = match self.grants.get(&parent.id()) {
            Some(held) => {
                if !held.grant.permission().covers(&permission) {
                    return Err(Attenuation::NotASubset);
                }

                if outlives(not_after, held.grant.not_after()) {
                    return Err(Attenuation::OutlivesParent);
                }

                (held.grant.id(), held.binding.clone())
            },
            None => return Err(Attenuation::UnknownParent),
        };

        if not_after.is_some_and(|limit| limit <= now) {
            return Err(Attenuation::AlreadyExpired);
        }

        // The child of an approved grant keeps the approval's binding: dropping it here would let
        // an attenuation widen the context the approval was earned for.
        Ok(self.mint(
            principal,
            permission,
            not_after,
            Some(parent_id),
            binding,
            now,
        ))
    }

    /// Decides one request at `now`.
    ///
    /// The policy speaks first. A request no rule matches is [`DenialReason::NoMatchingRule`], and
    /// a rule that allows an operation permits it only when a live grant covers the request as
    /// well. An approval rule offers a [`Requirement`] until the request is covered, at which
    /// point the covering grant is what the decision names.
    #[must_use]
    pub fn decide(
        &self,
        request: &Request,
        now: Moment,
    ) -> Decision {
        let Some(verdict) = self.policy.resolve(request) else {
            return Decision::Deny(Denial::new(DenialReason::NoMatchingRule));
        };

        match verdict {
            Verdict::Deny { rule } => {
                Decision::Deny(Denial::new(DenialReason::DeniedByRule { rule }))
            },
            Verdict::Allow => self.authorize(request, now).map_or_else(
                |reason| Decision::Deny(Denial::new(reason)),
                Decision::Allow,
            ),
            Verdict::Approve { not_after } => self.authorize(request, now).map_or_else(
                |_| Decision::Approve(requirement_for(request, not_after)),
                Decision::Allow,
            ),
        }
    }

    /// Mints the authority a requirement asks for, naming the request it was earned for.
    ///
    /// Approving is a second principal's act, and the minted grant remembers the requirement, so
    /// the approval authorizes exactly the request that was decided on and no other. The approver
    /// must be a principal this authority admitted through [`Authority::root`], so an id that
    /// never entered here cannot sign off on anything it holds no standing for.
    ///
    /// # Errors
    ///
    /// Returns [`ApprovalError::SelfApproved`] when `by` is the principal the approval would
    /// authorize, [`ApprovalError::UnknownApprover`] when this authority never admitted `by`, and
    /// [`ApprovalError::RequirementExpired`] when the requirement's own window closed at or
    /// before `now`.
    pub fn approve(
        &mut self,
        requirement: &Requirement,
        by: PrincipalID,
        now: Moment,
    ) -> Result<Grant, ApprovalError> {
        let (principal, action, resource) = authorized_operation(requirement);

        if by == principal {
            return Err(ApprovalError::SelfApproved);
        }

        if !self.principals.contains_key(&by) {
            return Err(ApprovalError::UnknownApprover);
        }

        if requirement.not_after().is_some_and(|limit| limit <= now) {
            return Err(ApprovalError::RequirementExpired);
        }

        let permission = Permission::new(resource.clone(), ActionSet::of(action));

        Ok(self.mint(
            principal,
            permission,
            requirement.not_after(),
            None,
            Some(requirement.clone()),
            now,
        ))
    }

    /// Revokes one grant, reporting whether this call was the one that revoked a grant this
    /// authority holds. Everything issued from the grant is denied with it.
    pub fn revoke(
        &mut self,
        grant: GrantId,
        now: Moment,
    ) -> bool {
        self.grants.contains_key(&grant) && self.revocations.grant(grant, now)
    }

    /// Revokes a principal, denying every grant it holds and everything issued from them.
    pub fn revoke_principal(
        &mut self,
        principal: PrincipalID,
        now: Moment,
    ) {
        self.revocations.principal(principal, now);
    }

    fn mint(
        &mut self,
        principal: PrincipalID,
        permission: Permission,
        not_after: Option<Moment>,
        parent: Option<GrantId>,
        binding: Option<Requirement>,
        now: Moment,
    ) -> Grant {
        let id = GrantId::from_value(self.next);
        self.next += 1;

        let grant = minted(id, principal, permission, not_after, parent);
        self.grants.insert(
            id,
            Held {
                grant: grant.clone(),
                at: now,
                binding,
            },
        );

        grant
    }

    /// The earliest live grant that covers the request, or the refusal that explains why none
    /// does.
    fn authorize(
        &self,
        request: &Request,
        now: Moment,
    ) -> Result<Grant, DenialReason> {
        let mut refusal: Option<DenialReason> = None;

        for held in self.grants.values() {
            if !mentions(&held.grant, request) {
                continue;
            }

            match self.refusal(held, request, now) {
                None => return Ok(held.grant.clone()),
                Some(reason) => {
                    if refusal
                        .as_ref()
                        .is_none_or(|worst| severity(&reason) > severity(worst))
                    {
                        refusal = Some(reason);
                    }
                },
            }
        }

        Err(refusal.unwrap_or(DenialReason::NotGranted))
    }

    /// Why a grant the request mentions cannot speak for it, or `None` when it can.
    ///
    /// The whole chain is checked, not just the grant: a grant issued from one that was later
    /// revoked or lapsed would otherwise launder the authority it descended from.
    fn refusal(
        &self,
        held: &Held,
        request: &Request,
        now: Moment,
    ) -> Option<DenialReason> {
        if held
            .binding
            .as_ref()
            .is_some_and(|binding| !binding.binds(request))
        {
            return Some(DenialReason::NotGranted);
        }

        let mut current = &held.grant;

        loop {
            if let Some(at) = self.revocations.blocked(current) {
                return Some(DenialReason::Revoked { at });
            }

            // The named instant is the first at which the grant no longer authorizes, so the
            // comparison includes it.
            if let Some(at) = current.not_after().filter(|limit| *limit <= now) {
                return Some(DenialReason::Expired { at });
            }

            let parent = current.parent()?;
            match self.grants.get(&parent) {
                // A chain this authority can no longer follow is refused rather than trusted: the
                // fail-closed direction.
                Some(ancestor) => current = &ancestor.grant,
                None => return Some(DenialReason::NotGranted),
            }
        }
    }
}

use alloc::vec::Vec;

use mokmok_protocol::{Moment, PrincipalID};

use crate::decision::{Context, Request};
use crate::permission::{Action, ActionSet};
use crate::resource::Resource;

/// A rule's answer for a request it matches.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Effect {
    /// The operation may happen, if a live grant covers the request.
    Allow,
    /// The operation may not happen.
    Deny,
    /// The operation needs an approval that names the request.
    RequiresApproval,
}

impl Effect {
    /// How restrictive the effect is, so that the most restrictive rule wins at equal specificity.
    const fn rank(self) -> u8 {
        match self {
            Self::Allow => 0,
            Self::RequiresApproval => 1,
            Self::Deny => 2,
        }
    }

    const fn verdict(
        self,
        rule: usize,
        not_after: Option<Moment>,
    ) -> Verdict {
        match self {
            Self::Allow => Verdict::Allow,
            Self::RequiresApproval => Verdict::Approve { not_after },
            Self::Deny => Verdict::Deny { rule },
        }
    }
}

/// The answer of the policy, before any grant is consulted.
pub enum Verdict {
    Deny { rule: usize },
    Allow,
    Approve { not_after: Option<Moment> },
}

/// One rule of a [`Policy`].
///
/// A rule matches a request on the axes it constrains and ignores the ones it leaves open, so its
/// specificity is the number of constrained axes. A builder that sets an axis twice keeps the
/// last value, which is what makes a rule readable as a chain of sentences.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Rule {
    effect: Effect,
    principal: Option<PrincipalID>,
    actions: Option<ActionSet>,
    resource: Option<Resource>,
    context: Option<Context>,
    not_after: Option<Moment>,
}

impl Rule {
    #[must_use]
    pub const fn allow() -> Self {
        Self::with_effect(Effect::Allow)
    }

    #[must_use]
    pub const fn deny() -> Self {
        Self::with_effect(Effect::Deny)
    }

    #[must_use]
    pub const fn requires_approval() -> Self {
        Self::with_effect(Effect::RequiresApproval)
    }

    const fn with_effect(effect: Effect) -> Self {
        Self {
            effect,
            principal: None,
            actions: None,
            resource: None,
            context: None,
            not_after: None,
        }
    }

    #[must_use]
    pub const fn for_principal(
        mut self,
        principal: PrincipalID,
    ) -> Self {
        self.principal = Some(principal);
        self
    }

    #[must_use]
    pub const fn for_action(
        mut self,
        action: Action,
    ) -> Self {
        self.actions = Some(ActionSet::of(action));
        self
    }

    #[must_use]
    pub const fn for_actions(
        mut self,
        actions: ActionSet,
    ) -> Self {
        self.actions = Some(actions);
        self
    }

    #[must_use]
    pub fn for_resource(
        mut self,
        resource: Resource,
    ) -> Self {
        self.resource = Some(resource);
        self
    }

    #[must_use]
    pub const fn in_context(
        mut self,
        context: Context,
    ) -> Self {
        self.context = Some(context);
        self
    }

    /// Bounds the approval this rule demands. Only an approval rule reads it; an allow or a deny
    /// leaves it unused, because a grant's own expiry is what bounds a grant.
    #[must_use]
    pub const fn not_after(
        mut self,
        not_after: Moment,
    ) -> Self {
        self.not_after = Some(not_after);
        self
    }

    fn matches(
        &self,
        request: &Request,
    ) -> bool {
        self.principal
            .is_none_or(|principal| principal == request.principal)
            && self
                .actions
                .is_none_or(|actions| actions.covers(ActionSet::of(request.action)))
            && self
                .resource
                .as_ref()
                .is_none_or(|resource| *resource == request.resource)
            && self
                .context
                .as_ref()
                .is_none_or(|context| *context == request.context)
    }

    const fn specificity(&self) -> u8 {
        let mut constrained = 0;

        if self.principal.is_some() {
            constrained += 1;
        }
        if self.actions.is_some() {
            constrained += 1;
        }
        if self.resource.is_some() {
            constrained += 1;
        }
        if self.context.is_some() {
            constrained += 1;
        }

        constrained
    }
}

/// The ordered rules a decision resolves against.
///
/// Resolution takes the matching rule with the greatest specificity, and at equal specificity the
/// most restrictive effect, so a deny wins a tie. Only at equal specificity *and* effect does the
/// earlier rule win, which is the one thing the caller's order decides.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Policy {
    rules: Vec<Rule>,
}

impl Policy {
    /// A policy with no rules, under which no request is allowed.
    #[must_use]
    pub const fn new() -> Self {
        Self { rules: Vec::new() }
    }

    #[must_use]
    pub fn with(
        mut self,
        rule: Rule,
    ) -> Self {
        self.rules.push(rule);
        self
    }

    /// The answer of the most specific matching rule, or `None` when no rule matches.
    #[must_use]
    pub fn resolve(
        &self,
        request: &Request,
    ) -> Option<Verdict> {
        let mut best: Option<(u8, u8, usize, &Rule)> = None;

        for (index, rule) in self.rules.iter().enumerate() {
            if !rule.matches(request) {
                continue;
            }

            let key = (rule.specificity(), rule.effect.rank());
            if best.is_none_or(|(specificity, rank, _, _)| key > (specificity, rank)) {
                best = Some((key.0, key.1, index, rule));
            }
        }

        best.map(|(_, _, index, rule)| rule.effect.verdict(index, rule.not_after))
    }
}

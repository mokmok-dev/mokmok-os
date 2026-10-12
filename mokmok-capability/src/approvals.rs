use core::fmt;

use mokmok_protocol::{Moment, PrincipalID};

use crate::decision::{Context, Request};
use crate::permission::Action;
use crate::resource::Resource;

/// The request fields a requirement binds, held whole rather than hashed.
///
/// The operation digest holds the principal, the action, and the resource, and no context, so it
/// constrains exactly those three. The context digest holds all four, so it adds the context the
/// approval was earned in. The binding is then a field-by-field comparison of what the request
/// asks for against what the approval named, with no digest to collide and no field to forget.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Digest {
    principal: PrincipalID,
    action: Action,
    resource: Resource,
    context: Option<Context>,
}

impl Digest {
    fn operation(request: &Request) -> Self {
        Self {
            principal: request.principal,
            action: request.action,
            resource: request.resource.clone(),
            context: None,
        }
    }

    fn context(request: &Request) -> Self {
        Self {
            principal: request.principal,
            action: request.action,
            resource: request.resource.clone(),
            context: Some(request.context.clone()),
        }
    }

    fn names(
        &self,
        request: &Request,
    ) -> bool {
        self.principal == request.principal
            && self.action == request.action
            && self.resource == request.resource
            && self
                .context
                .as_ref()
                .is_none_or(|context| *context == request.context)
    }
}

/// An approval a principal must obtain before an authority will issue a grant.
///
/// The value names one request: the principal, the action, the resource, and the context. It has
/// no public constructor and no `Deserialize`, so the only way to hold one is to have received it
/// from [`Authority::decide`](crate::Authority::decide), and the only way to spend it is
/// [`Authority::approve`](crate::Authority::approve), which mints authority for exactly the
/// request named.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Requirement {
    operation: Digest,
    context: Digest,
    not_after: Option<Moment>,
}

impl Requirement {
    /// Whether this requirement was earned for exactly this request.
    #[must_use]
    pub fn binds(
        &self,
        request: &Request,
    ) -> bool {
        self.operation.names(request) && self.context.names(request)
    }

    /// The instant after which the approval must be given, or `None` when it does not lapse.
    #[must_use]
    pub const fn not_after(&self) -> Option<Moment> {
        self.not_after
    }
}

impl fmt::Display for Requirement {
    fn fmt(
        &self,
        formatter: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        write!(
            formatter,
            "{} may {} {}",
            self.operation.principal.as_uuid(),
            self.operation.action,
            self.operation.resource,
        )?;

        self.not_after.map_or(Ok(()), |at| {
            write!(formatter, " until {} ms", at.as_millis())
        })
    }
}
/// Builds the requirement a decision hands to an approver.
///
/// A free function rather than an associated one: [`Requirement`] is re-exported, so a `pub`
/// inherent constructor would join the crate's public surface, while this module stays private.
#[must_use]
pub fn requirement_for(
    request: &Request,
    not_after: Option<Moment>,
) -> Requirement {
    Requirement {
        operation: Digest::operation(request),
        context: Digest::context(request),
        not_after,
    }
}

/// The principal, action, and resource a requirement authorizes.
#[must_use]
pub const fn authorized_operation(requirement: &Requirement) -> (PrincipalID, Action, &Resource) {
    (
        requirement.operation.principal,
        requirement.operation.action,
        &requirement.operation.resource,
    )
}

/// Why an approval was refused.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApprovalError {
    /// The principal that would receive the authority is the one approving it.
    SelfApproved,
    /// The requirement's own window closed, so no approval can satisfy it.
    RequirementExpired,
}

impl fmt::Display for ApprovalError {
    fn fmt(
        &self,
        formatter: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        formatter.write_str(match self {
            Self::SelfApproved => "a principal cannot approve the authority it requests",
            Self::RequirementExpired => "the approval window has already closed",
        })
    }
}

impl core::error::Error for ApprovalError {}

use core::fmt;

use mokmok_protocol::{ExecutionID, Moment, PrincipalID, SessionID, TurnID};

use crate::approvals::Requirement;
use crate::grants::Grant;
use crate::permission::Action;
use crate::resource::Resource;

/// The execution context one request runs in.
///
/// The context is compared whole, field by field, and never parsed. A request that names the same
/// principal, action, and resource in a different session is a different request, which is what
/// keeps an approval earned in one session from being spent in another.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct Context {
    pub session: Option<SessionID>,
    pub execution: Option<ExecutionID>,
    pub turn: Option<TurnID>,
}

impl Context {
    /// A request outside any session, execution, and turn.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            session: None,
            execution: None,
            turn: None,
        }
    }
}

/// One authorization question.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct Request {
    pub principal: PrincipalID,
    pub action: Action,
    pub resource: Resource,
    pub context: Context,
}

/// What an authority decided about a request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Decision {
    /// This grant authorizes the request, and the authority holds every grant in its chain.
    Allow(Grant),
    /// No live authority covers the request, for this reason.
    Deny(Denial),
    /// The operation is behind an approval, so the authority asks for one that names this request.
    Approve(Requirement),
}

/// Why a request was denied.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Denial {
    reason: DenialReason,
}

impl Denial {
    #[must_use]
    pub const fn new(reason: DenialReason) -> Self {
        Self { reason }
    }

    #[must_use]
    pub const fn reason(&self) -> &DenialReason {
        &self.reason
    }
}

impl fmt::Display for Denial {
    fn fmt(
        &self,
        formatter: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        fmt::Display::fmt(&self.reason, formatter)
    }
}

/// The rule a denial rests on.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DenialReason {
    /// No rule of the policy matches the request.
    NoMatchingRule,
    /// The policy rule at this index denies the request.
    DeniedByRule { rule: usize },
    /// The policy allows the operation, but no live grant this authority issued covers the request.
    NotGranted,
    /// The grant expired at this instant: it authorizes before it and denies at it.
    Expired { at: Moment },
    /// The grant, or one it descends from, was revoked at this instant.
    Revoked { at: Moment },
}

impl fmt::Display for DenialReason {
    fn fmt(
        &self,
        formatter: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        match self {
            Self::NoMatchingRule => formatter.write_str("no policy rule matches the request"),
            Self::DeniedByRule { rule } => {
                write!(formatter, "policy rule {rule} denies the request")
            },
            Self::NotGranted => {
                formatter.write_str("no live grant from this authority covers the request")
            },
            Self::Expired { at } => write!(formatter, "the grant expired at {} ms", at.as_millis()),
            Self::Revoked { at } => {
                write!(formatter, "the grant was revoked at {} ms", at.as_millis())
            },
        }
    }
}

impl core::error::Error for DenialReason {}

/// How strong a refusal is, so an authority reports the strongest one it found.
///
/// A free function rather than an associated one: [`DenialReason`] is re-exported, so a `pub`
/// inherent method would join the crate's public surface, while this module stays private. A
/// revocation outranks a lapse because it is an explicit act of authority, and both outrank the
/// absence of any grant, which explains the least.
#[must_use]
pub const fn severity(reason: &DenialReason) -> u8 {
    match reason {
        DenialReason::Revoked { .. } => 3,
        DenialReason::Expired { .. } => 2,
        DenialReason::NotGranted => 1,
        DenialReason::NoMatchingRule | DenialReason::DeniedByRule { .. } => 0,
    }
}

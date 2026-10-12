//! Decides which principal may perform an operation on a resource in one execution context.
//!
//! The crate is pure and deterministic. It reads no clock, no randomness, no filesystem, and no
//! socket, it opens no network path, and it controls no process: every instant enters as a
//! [`Moment`](mokmok_protocol::Moment) the caller supplies, so one call sequence reaches the same
//! decision in a test, in a replay, and in production.
//!
//! Three rules carry the contract, and the tests pin each of them:
//!
//! - **A grant cannot be written down.** [`Grant`] has no public constructor, and
//!   [`Authority::issue`] is the only expression that mints one beyond a root. It takes a grant the
//!   caller already holds, and what it issues must be a subset of that grant's permission and must
//!   not outlive it, so a principal's own claim cannot widen what it holds: widening is not a check
//!   that can be forgotten, it is not expressible.
//! - **Nothing is allowed by default.** A request no policy rule matches is [`Decision::Deny`]
//!   with [`DenialReason::NoMatchingRule`]. A rule that allows an operation permits it only when a
//!   live [`Grant`] covers the request as well: the policy says what may happen, the grants say
//!   who may do it.
//! - **An approval is bound to the request it authorized.** The [`Requirement`] a decision hands
//!   back holds the principal, the action, the resource, and the context it was earned for.
//!   Approving it mints authority for exactly that request and no other, so an approval cannot be
//!   spent on an operation it never named.
//!
//! Two boundaries are exact. A grant whose `not_after` equals `now` has expired: the instant named
//! is the first instant at which the grant no longer authorizes. And a revocation reaches the
//! whole chain: revoking a grant, or a principal, denies every grant descending from it, because
//! otherwise a grant issued before the revocation would carry revoked authority forward.
//!
//! This crate decides; it does not enforce. Whether a decision is consulted before an operation
//! happens, and whether the approver is entitled to approve, belong to the caller.
//!
//! # Examples
//!
//! ```
//! use mokmok_capability::{
//!     Action, ActionSet, Authority, Context, Decision, Name, Permission, Policy, Principal,
//!     PrincipalKind, Request, Resource, Rule,
//! };
//! use mokmok_protocol::{Moment, PrincipalID};
//! use uuid::Uuid;
//!
//! let now = Moment::from_millis(1_700_000_000_000);
//! let alice = PrincipalID::from_uuid(Uuid::from_u128(1));
//! let bob = PrincipalID::from_uuid(Uuid::from_u128(2));
//! let data = Resource::Path(String::from("/data"));
//!
//! // The policy says a read may happen at all; the grants say who may read what.
//! let mut authority = Authority::new(Policy::new().with(Rule::allow().for_action(Action::Read)), now);
//! let root = authority.root(
//!     Principal::new(alice, PrincipalKind::Agent, Name::new("analyst")?),
//!     Permission::new(data.clone(), ActionSet::READ),
//!     now,
//! );
//!
//! // Alice's authority is delegated to Bob, attenuated to a read.
//! let read = authority.issue(&root, bob, Permission::new(data.clone(), ActionSet::READ), None, now)?;
//!
//! let request = Request {
//!     principal: bob,
//!     action: Action::Read,
//!     resource: data,
//!     context: Context::none(),
//! };
//! assert_eq!(authority.decide(&request, now), Decision::Allow(read));
//! # Ok::<(), Box<dyn core::error::Error>>(())
//! ```

#![no_std]

extern crate alloc;

mod approvals;
mod authority;
mod decision;
mod grants;
mod name;
mod permission;
mod policy;
mod principals;
mod resource;
mod revocations;

pub use approvals::{ApprovalError, Requirement};
pub use authority::Authority;
pub use decision::{Context, Decision, Denial, DenialReason, Request};
pub use grants::{Attenuation, Grant, GrantId};
pub use name::{InvalidName, Name};
pub use permission::{Action, ActionSet, Permission};
pub use policy::{Policy, Rule};
pub use principals::{Principal, PrincipalKind};
pub use resource::Resource;

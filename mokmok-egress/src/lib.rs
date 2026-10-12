//! Mediates sandbox network traffic and enforces destination decisions.
//!
//! A caller parses the destination it wants, resolves it, and hands both to [`Validated`].
//! Validation is a decision over that list alone: every address is classified, the policy port is
//! consulted, and a destination whose addresses all fall in a refused class is rejected. Nothing
//! has been connected at that point, and no socket exists.
//!
//! [`Validated`] keeps the exact [`std::net::SocketAddr`] values it accepted, and
//! [`Validated::connect`] takes no resolver, so the address connected to is one of the addresses
//! validated. A name that answers differently on a second lookup cannot redirect the connection,
//! and an alternate address the rules refuse was dropped before any connection was attempted.
//!
//! Validation and the connection each record what they decided to the [`AuditSink`] they are
//! given, so the trail is emitted by the work rather than assembled by a caller.
//!
//! ```rust
//! use std::net::{IpAddr, Ipv4Addr, SocketAddr};
//!
//! use mokmok_egress::{
//!     AuditRecord, AuditSink, CallContext, Destination, PolicyClient, Refusal, Rules, Validated,
//!     parse_connect,
//! };
//! use mokmok_protocol::{ExecutionID, PrincipalID};
//! use uuid::Uuid;
//!
//! struct Permit;
//!
//! impl PolicyClient for Permit {
//!     fn check(&self, _: &Destination, _: &CallContext) -> Result<(), Refusal> {
//!         Ok(())
//!     }
//! }
//!
//! struct Discard;
//!
//! impl AuditSink for Discard {
//!     fn record(&self, _: AuditRecord) {}
//! }
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let destination = parse_connect("CONNECT example.com:443 HTTP/1.1")?;
//! let addresses = vec![SocketAddr::new(IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34)), 443)];
//! let context = CallContext {
//!     principal: PrincipalID::from_uuid(Uuid::from_u128(1)),
//!     execution: ExecutionID::from_uuid(Uuid::from_u128(2)),
//! };
//!
//! let validated = Validated::new(
//!     destination,
//!     addresses,
//!     &Rules::public_only(),
//!     &Permit,
//!     &context,
//!     &Discard,
//! )?;
//!
//! assert_eq!(validated.addresses().len(), 1);
//! # Ok(())
//! # }
//! ```

// Internal types are `pub`, not `pub(crate)`: the workspace denies
// `clippy::redundant_pub_crate`, and a private module already keeps them crate-local.
mod audit;
mod connect;
mod destination;
mod dns;
mod policy_client;
mod proxy;
mod rate_limit;

pub use audit::{AuditRecord, AuditSink, DecisionName};
pub use connect::{Error, Validated};
pub use destination::{AddressClass, Destination, Host, Port, Rules, Scheme, classify};
pub use dns::Resolver;
pub use policy_client::{CallContext, PolicyClient, Refusal};
pub use proxy::{ParseError, parse_authority, parse_connect};
pub use rate_limit::RateLimit;

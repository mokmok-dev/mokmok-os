//! The port through which a destination's authorization is decided.

use mokmok_protocol::{ExecutionID, PrincipalID};

use crate::destination::{AddressClass, Destination};

/// Who is asking, and in whose name the connection will be made.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CallContext {
    /// The principal the connection is made for.
    pub principal: PrincipalID,
    /// The execution the connection belongs to.
    pub execution: ExecutionID,
}

/// Why a destination was not accepted.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum Refusal {
    /// The policy port declined the destination.
    #[error("the destination policy refused the destination")]
    Denied,
    /// Every address the destination resolved to falls in a class the rules refuse.
    #[error("every address is in a forbidden class")]
    Forbidden(AddressClass),
    /// The destination resolved to no address at all.
    #[error("the destination resolved to no address")]
    Unresolved,
}

/// Decides whether a destination may be reached at all.
///
/// This is a port so the authority that owns the decision stays outside the proxy; the proxy
/// only enforces the answer.
pub trait PolicyClient {
    /// Whether `destination` may be reached for `context`.
    ///
    /// # Errors
    ///
    /// Returns [`Refusal`] when the destination is not authorized.
    fn check(
        &self,
        destination: &Destination,
        context: &CallContext,
    ) -> Result<(), Refusal>;
}

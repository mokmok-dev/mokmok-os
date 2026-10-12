//! The validated destination, and the one connection it can make.

use std::io;
use std::net::SocketAddr;

use tokio::net::TcpStream;

use crate::destination::{Destination, Rules, classify};
use crate::policy_client::{CallContext, PolicyClient, Refusal};

/// A destination that passed the rules and the policy port, with the addresses it passed with.
///
/// The address list is the whole authority to connect: it holds the [`SocketAddr`] values
/// validation accepted, and there is no resolver anywhere on this type, so a name that answers
/// differently on a second lookup cannot redirect the connection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Validated {
    destination: Destination,
    addresses: Vec<SocketAddr>,
}

impl Validated {
    /// Validates `destination` against `rules` and `policy`, before any socket exists.
    ///
    /// The returned value keeps only the addresses whose class the rules admit. When none of them
    /// is admitted, or the policy declines, nothing is returned and no socket was opened.
    ///
    /// # Errors
    ///
    /// Returns [`Refusal::Denied`] when the policy port declines the destination,
    /// [`Refusal::Forbidden`] when every address falls in a refused class, and
    /// [`Refusal::Unresolved`] when there is no address to validate.
    pub fn new(
        destination: Destination,
        addresses: Vec<SocketAddr>,
        rules: &Rules,
        policy: &impl PolicyClient,
        context: &CallContext,
    ) -> Result<Self, Refusal> {
        policy.check(&destination, context)?;

        let mut permitted = Vec::new();
        let mut refused = None;
        for address in addresses {
            let class = classify(address.ip());
            if rules.allows(class) {
                permitted.push(address);
            } else if refused.is_none() {
                refused = Some(class);
            }
        }

        if permitted.is_empty() {
            return Err(refused.map_or(Refusal::Unresolved, Refusal::Forbidden));
        }

        Ok(Self {
            destination,
            addresses: permitted,
        })
    }

    /// The destination that was validated.
    #[must_use]
    pub const fn destination(&self) -> &Destination {
        &self.destination
    }

    /// The addresses that passed validation, in the order they were supplied.
    #[must_use]
    pub fn addresses(&self) -> &[SocketAddr] {
        &self.addresses
    }

    /// Connects to the first validated address that answers.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Unreachable`] when no validated address could be reached.
    pub async fn connect(&self) -> Result<TcpStream, Error> {
        // Every candidate comes from the validated list, so a fallback after a refused address is
        // still a validated address.
        let mut failure = None;
        for address in &self.addresses {
            match TcpStream::connect(address).await {
                Ok(stream) => return Ok(stream),
                Err(error) => failure = Some(error),
            }
        }

        Err(Error::Unreachable(failure.unwrap_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "the destination has no validated address",
            )
        })))
    }
}

/// A validated destination that could not be reached.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// No validated address answered.
    #[error("no validated address of the destination could be reached")]
    Unreachable(#[source] io::Error),
}

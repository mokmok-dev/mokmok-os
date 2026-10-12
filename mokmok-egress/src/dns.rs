//! Name resolution, as a port the caller supplies.

use std::io;
use std::net::SocketAddr;

/// Turns a host name into the addresses it currently names.
///
/// Resolution is deliberately outside [`crate::Validated`]: it happens once, before validation,
/// and its answer is what validation accepts or refuses.
pub trait Resolver {
    /// The addresses `host` currently names at `port`.
    ///
    /// # Errors
    ///
    /// Returns the resolver's own error when the name cannot be resolved.
    fn resolve(
        &self,
        host: &str,
        port: u16,
    ) -> Result<Vec<SocketAddr>, io::Error>;
}

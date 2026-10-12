//! The port a metrics snapshot leaves through.

use crate::error::Error;
use crate::metrics::Snapshot;

/// A destination for a metrics [`Snapshot`].
///
/// This is a port, so the caller owns the concrete exporter: a collector endpoint, a file, a
/// buffer in another process. Nothing in this crate calls one on its own, which is what keeps a
/// security fact from depending on a telemetry destination being reachable. Implementations
/// report failure as [`Error::Export`] and must neither panic nor drop a snapshot silently.
pub trait Exporter {
    /// Delivers `snapshot` to the destination, or reports why it could not.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Export`] when the destination refused the snapshot or could not be
    /// reached.
    fn export(
        &self,
        snapshot: &Snapshot,
    ) -> Result<(), Error>;
}

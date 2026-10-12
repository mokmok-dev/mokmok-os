//! Exports traces, metrics, logs, and durable audit records for the rest of the system.
//!
//! This crate owns no authoritative state. A [`Redaction`] removes the protected value from a
//! line while leaving the identifiers that let a reader correlate it, and a [`Metrics`] registry
//! holds the counters and gauges a caller hands to an [`Exporter`]. The exporter is a port the
//! caller implements; its failure is a typed [`Error`] the caller reports, and it never redefines
//! a decision that already landed.
//!
//! ```rust
//! use mokmok_observability::{InvalidRule, Redaction, Rule};
//!
//! # fn main() -> Result<(), InvalidRule> {
//! let redaction = Redaction::from_rules(vec![Rule::new("hunter2")])?;
//!
//! assert_eq!(
//!     redaction
//!         .apply("principal=alice trace=4bf92f3577b34da6a3ce929d0e0e4736 secret=hunter2")
//!         .as_ref(),
//!     "principal=alice trace=4bf92f3577b34da6a3ce929d0e0e4736 secret=[redacted]",
//! );
//! # Ok(())
//! # }
//! ```

// Internal types are `pub`, not `pub(crate)`: the workspace denies
// `clippy::redundant_pub_crate`, and a private module already keeps them crate-local.
mod error;
mod exporter;
mod metrics;
mod redaction;

pub use error::Error;
pub use exporter::Exporter;
pub use metrics::{Metrics, Snapshot};
pub use redaction::{InvalidRule, Redaction, Rule};

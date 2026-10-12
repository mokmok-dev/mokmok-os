//! The record of a network decision, and where it goes.

use std::net::SocketAddr;

use mokmok_protocol::{ExecutionID, Moment, PrincipalID};

use crate::destination::Destination;

/// What was decided about one address.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DecisionName {
    /// The connection was made.
    Allowed,
    /// The connection was refused.
    Refused,
}

/// One entry of the network audit trail.
///
/// The record carries the execution and principal it was made for, so a connection can be
/// attributed after the fact without consulting any other log.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuditRecord {
    /// The instant the decision was made.
    pub at: Moment,
    /// The execution the connection belongs to.
    pub execution: ExecutionID,
    /// The principal the connection was made for.
    pub principal: PrincipalID,
    /// The destination that was decided about.
    pub destination: Destination,
    /// The address the decision is about; for an allowed connection, the one it was made to.
    pub address: SocketAddr,
    /// What was decided.
    pub decision: DecisionName,
}

/// Somewhere an audit record goes. The record itself is owned by this crate; the sink decides
/// where it is kept.
pub trait AuditSink {
    /// Appends `record`.
    fn record(
        &self,
        record: AuditRecord,
    );
}

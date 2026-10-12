//! The record of a network decision, and where it goes.

use std::net::SocketAddr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use mokmok_protocol::{ExecutionID, Moment, PrincipalID};

use crate::destination::Destination;
use crate::policy_client::Refusal;

/// What was decided about one destination or the address it was reached at.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DecisionName {
    /// The destination was admitted, or the connection it authorized was made.
    Allowed,
    /// The decision refused, carrying the reason the validation returned.
    Refused(Refusal),
    /// No validated address answered the connection attempt.
    Unreachable,
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
    /// The address the decision names, when it names one: validation admits a destination before
    /// an address is chosen, and a refused connection never reached one.
    pub address: Option<SocketAddr>,
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

/// The instant this crate stamps a record with.
///
/// Read here rather than taken from the caller, so a caller being audited cannot date the record
/// of its own decision to an instant of its choosing.
#[must_use]
pub fn now() -> Moment {
    let since = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO);

    Moment::from_millis(u64::try_from(since.as_millis()).unwrap_or(u64::MAX))
}

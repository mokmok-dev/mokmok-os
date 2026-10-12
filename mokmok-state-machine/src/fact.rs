use mokmok_protocol::ExecutionID;

/// One recorded fact of a session's history.
///
/// A fact's own content is its identity. The event bus is at-least-once, a replay carries the same
/// [`EventID`](mokmok_protocol::EventID), and a publisher retry after `Error::Indeterminate`
/// commits a new event for the same logical fact, so no event id can separate a redelivery from a
/// first delivery. This crate therefore keys every idempotence decision on the content here.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(tag = "fact", rename_all = "snake_case")]
pub enum Fact {
    SessionOpened,
    ExecutionLaunched {
        execution: ExecutionID,
        deadline: Deadline,
    },
    ExecutionSettled {
        execution: ExecutionID,
        resolution: Resolution,
    },
    CancelRequested,
    ResultApplied {
        execution: ExecutionID,
    },
    SessionClosed,
}

/// Whether the execution declared a deadline. The instant lives with whoever owns a clock; this
/// crate records only that one applies, so [`Resolution::TimedOut`] can be refused for an execution
/// that declared none.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Deadline {
    Bounded,
    Unbounded,
}

/// How one execution attempt ended.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Resolution {
    /// The tool ran to completion with this exit code.
    Finished { code: i32 },
    /// The workload died of this signal number without a cancel naming it.
    Killed { signal: i32 },
    /// The execution's own deadline passed while it was still running.
    TimedOut,
    /// The execution was stopped because the session was cancelled.
    Cancelled,
    /// The workload never ran: the sandbox refused it, or confined it away before exec.
    Refused,
}

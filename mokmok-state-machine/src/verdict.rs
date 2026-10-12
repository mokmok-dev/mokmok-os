use core::fmt;

use mokmok_protocol::ExecutionID;

/// What [`Session::apply`](crate::Session::apply) did with a fact.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Ack {
    /// The fact was new and moved the state.
    Applied,
    /// The state already held the fact's effect, so nothing changed.
    Duplicate,
}

/// Why one fact is refused.
///
/// The message is lowercase and names its subject, because it is the text a caller shows and the
/// text a test pins literally. An [`ExecutionID`] renders as the UUID it wraps.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Rejection {
    NotOpened,
    NotAccepting,
    SessionOver,
    Unsettled,
    UnknownExecution(ExecutionID),
    DeadlineConflict(ExecutionID),
    ResolutionConflict(ExecutionID),
    UnrequestedCancel(ExecutionID),
    NoDeadline(ExecutionID),
    NoResult(ExecutionID),
}

impl fmt::Display for Rejection {
    fn fmt(
        &self,
        formatter: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        match self {
            Self::NotOpened => formatter.write_str("the session has not been opened"),
            Self::NotAccepting => {
                formatter.write_str("the session is cancelled and accepting no new work")
            },
            Self::SessionOver => formatter.write_str("the session is over"),
            Self::Unsettled => formatter.write_str("an execution is still in flight"),
            Self::UnknownExecution(execution) => write!(
                formatter,
                "no execution {} in this session",
                execution.as_uuid(),
            ),
            Self::DeadlineConflict(execution) => write!(
                formatter,
                "execution {} was launched with a different deadline",
                execution.as_uuid(),
            ),
            Self::ResolutionConflict(execution) => write!(
                formatter,
                "execution {} already settled another way",
                execution.as_uuid(),
            ),
            Self::UnrequestedCancel(execution) => write!(
                formatter,
                "execution {} cannot be cancelled while the session is not cancelling",
                execution.as_uuid(),
            ),
            Self::NoDeadline(execution) => write!(
                formatter,
                "execution {} declared no deadline, so it cannot time out",
                execution.as_uuid(),
            ),
            Self::NoResult(execution) => write!(
                formatter,
                "execution {} has no unconsumed result",
                execution.as_uuid(),
            ),
        }
    }
}

impl core::error::Error for Rejection {}

/// A refusal together with where in the log it happened.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Rejected {
    offset: usize,
    rejection: Rejection,
}

impl Rejected {
    /// The index into the slice the caller passed to
    /// [`Session::replay`](crate::Session::replay).
    #[must_use]
    pub const fn offset(&self) -> usize {
        self.offset
    }

    #[must_use]
    pub const fn rejection(&self) -> Rejection {
        self.rejection
    }
}

impl fmt::Display for Rejected {
    fn fmt(
        &self,
        formatter: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        write!(
            formatter,
            "fact {} of the log: {}",
            self.offset, self.rejection,
        )
    }
}

impl core::error::Error for Rejected {}

/// Builds a [`Rejected`].
///
/// A `pub` free function rather than an associated one: [`Rejected`] is re-exported, so a `pub`
/// inherent method would join the crate's public surface, while this module stays private.
#[must_use]
pub const fn rejected(
    offset: usize,
    rejection: Rejection,
) -> Rejected {
    Rejected { offset, rejection }
}

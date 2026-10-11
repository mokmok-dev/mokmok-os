use alloc::collections::BTreeMap;

use mokmok_protocol::{ExecutionID, SessionID};

use crate::fact::{Deadline, Fact, Resolution};
use crate::verdict::{Ack, Rejected, Rejection, rejected};

/// The lifecycle of one agent session, folded from its [`Fact`]s.
///
/// A session starts [`Phase::Unopened`] and remembers one [`Execution`] per launched
/// [`ExecutionID`]. The value holds no handle, no clock, and no serialization, so the log is the
/// only durable form and [`Session::replay`] is the only way back to it.
///
/// Re-applying a fact the state already holds never fails and never changes the state: every fact
/// of a valid history re-applied in order yields [`Ack::Duplicate`] and an equal session, which is
/// what makes a redelivered batch safe to re-fold.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Session {
    id: SessionID,
    phase: Phase,
    executions: BTreeMap<ExecutionID, Execution>,
}

/// Where a session is in its lifecycle.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Phase {
    /// The state a fold starts from, before [`Fact::SessionOpened`].
    Unopened,
    Accepting,
    /// A cancel is in force: no new execution may launch, and the ending is decided.
    Draining,
    Closed(Ending),
}

/// How a closed session ended.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Ending {
    Completed,
    Cancelled,
}

/// One execution launched under the session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Execution {
    deadline: Deadline,
    state: State,
}

/// Where one execution is in its lifecycle.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum State {
    InFlight,
    /// `Resolution` is written once and never changes.
    Settled(Resolution),
    /// The [`Resolution::Finished`] result was handed to its consumer exactly once.
    ///
    /// The hand-off is a state marker, so it lives here and not in a recorded [`Fact`]: a field
    /// inside `Finished` could disagree with the history it was folded from. Re-applying the
    /// settle fact after the result was consumed therefore still names the same settlement and is
    /// an [`Ack::Duplicate`], which is the idempotence the bus's at-least-once delivery needs.
    Applied {
        code: i32,
    },
}

impl State {
    /// The settlement this state records, or `None` while the execution is still in flight.
    ///
    /// An [`State::Applied`] execution settled as `Finished { code }`, so the settle fact that
    /// recorded it matches the state even after the result was handed over.
    #[must_use]
    const fn settlement(self) -> Option<Resolution> {
        match self {
            Self::Settled(resolution) => Some(resolution),
            Self::Applied { code } => Some(Resolution::Finished { code }),
            Self::InFlight => None,
        }
    }

    /// Whether the result was already handed to its consumer.
    #[must_use]
    const fn is_applied(self) -> bool {
        matches!(self, Self::Applied { .. })
    }

    /// The exit code of a [`Resolution::Finished`] result still waiting to be applied.
    ///
    /// One home for the rule, so the arms of [`Session::consume`] branch on it.
    #[must_use]
    const fn waiting_result(self) -> Option<i32> {
        match self {
            Self::Settled(Resolution::Finished { code }) => Some(code),
            _ => None,
        }
    }
}

impl Session {
    /// A session that has recorded nothing yet.
    #[must_use]
    pub const fn new(id: SessionID) -> Self {
        Self {
            id,
            phase: Phase::Unopened,
            executions: BTreeMap::new(),
        }
    }

    /// Rebuilds the state a driving path held by folding its whole log again.
    ///
    /// This is `facts.iter().try_fold(Session::new(id), Session::apply)`: the driving path and the
    /// replay path run the same function, so they cannot drift.
    ///
    /// # Errors
    ///
    /// Returns [`Rejected`] at the first fact the history refuses, naming the offset into `facts`.
    pub fn replay(
        id: SessionID,
        facts: &[Fact],
    ) -> Result<Self, Rejected> {
        let mut session = Self::new(id);
        for (offset, fact) in facts.iter().enumerate() {
            session
                .apply(*fact)
                .map_err(|rejection| rejected(offset, rejection))?;
        }

        Ok(session)
    }

    /// Records one fact, or reports why it cannot be recorded.
    ///
    /// Whether the fact's effect is already recorded is decided before any phase precondition, so
    /// a redelivered fact is [`Ack::Duplicate`] rather than an error even after the session closed.
    ///
    /// # Errors
    ///
    /// Returns [`Rejection::NotOpened`] before [`Fact::SessionOpened`], the phase refusals
    /// [`Rejection::NotAccepting`], [`Rejection::SessionOver`], and [`Rejection::Unsettled`] for a
    /// fact the phase does not admit, and the per-fact refusals
    /// [`Rejection::UnknownExecution`], [`Rejection::DeadlineConflict`],
    /// [`Rejection::ResolutionConflict`], [`Rejection::UnrequestedCancel`],
    /// [`Rejection::NoDeadline`], and [`Rejection::NoResult`]. A refusal leaves the session as it
    /// found it.
    pub fn apply(
        &mut self,
        fact: Fact,
    ) -> Result<Ack, Rejection> {
        if self.phase == Phase::Unopened && fact != Fact::SessionOpened {
            return Err(Rejection::NotOpened);
        }

        match fact {
            Fact::SessionOpened => {
                if self.phase == Phase::Unopened {
                    self.phase = Phase::Accepting;
                    Ok(Ack::Applied)
                } else {
                    Ok(Ack::Duplicate)
                }
            },
            Fact::ExecutionLaunched {
                execution,
                deadline,
            } => self.launch(execution, deadline),
            Fact::ExecutionSettled {
                execution,
                resolution,
            } => self.settle(execution, resolution),
            Fact::CancelRequested => {
                if self.phase == Phase::Accepting {
                    self.phase = Phase::Draining;
                    Ok(Ack::Applied)
                } else if self.phase == Phase::Draining
                    || self.phase == Phase::Closed(Ending::Cancelled)
                {
                    Ok(Ack::Duplicate)
                } else {
                    Err(Rejection::SessionOver)
                }
            },
            Fact::ResultApplied { execution } => self.consume(execution),
            Fact::SessionClosed => {
                if self.phase == Phase::Accepting || self.phase == Phase::Draining {
                    if self.all_settled() {
                        self.phase = if self.phase == Phase::Draining {
                            Phase::Closed(Ending::Cancelled)
                        } else {
                            Phase::Closed(Ending::Completed)
                        };
                        Ok(Ack::Applied)
                    } else {
                        Err(Rejection::Unsettled)
                    }
                } else {
                    Ok(Ack::Duplicate)
                }
            },
        }
    }

    fn launch(
        &mut self,
        execution: ExecutionID,
        deadline: Deadline,
    ) -> Result<Ack, Rejection> {
        if let Some(recorded) = self.executions.get(&execution) {
            if recorded.deadline == deadline {
                Ok(Ack::Duplicate)
            } else {
                Err(Rejection::DeadlineConflict(execution))
            }
        } else if self.phase == Phase::Accepting {
            self.executions.insert(
                execution,
                Execution {
                    deadline,
                    state: State::InFlight,
                },
            );
            Ok(Ack::Applied)
        } else if self.phase == Phase::Draining {
            Err(Rejection::NotAccepting)
        } else {
            Err(Rejection::SessionOver)
        }
    }

    fn all_settled(&self) -> bool {
        self.executions
            .values()
            .all(|execution| execution.state != State::InFlight)
    }

    fn settle(
        &mut self,
        execution: ExecutionID,
        resolution: Resolution,
    ) -> Result<Ack, Rejection> {
        let Some(recorded) = self.executions.get_mut(&execution) else {
            return Err(Rejection::UnknownExecution(execution));
        };
        let deadline = recorded.deadline;
        if let Some(current) = recorded.state.settlement() {
            if current == resolution {
                Ok(Ack::Duplicate)
            } else {
                Err(Rejection::ResolutionConflict(execution))
            }
        } else if resolution == Resolution::Cancelled && self.phase != Phase::Draining {
            Err(Rejection::UnrequestedCancel(execution))
        } else if resolution == Resolution::TimedOut && deadline == Deadline::Unbounded {
            Err(Rejection::NoDeadline(execution))
        } else {
            recorded.state = State::Settled(resolution);
            Ok(Ack::Applied)
        }
    }

    fn consume(
        &mut self,
        execution: ExecutionID,
    ) -> Result<Ack, Rejection> {
        let Some(recorded) = self.executions.get_mut(&execution) else {
            return Err(Rejection::UnknownExecution(execution));
        };
        if recorded.state.is_applied() {
            Ok(Ack::Duplicate)
        } else if let Some(code) = recorded.state.waiting_result() {
            if self.phase == Phase::Accepting || self.phase == Phase::Draining {
                recorded.state = State::Applied { code };
                Ok(Ack::Applied)
            } else {
                Err(Rejection::SessionOver)
            }
        } else {
            Err(Rejection::NoResult(execution))
        }
    }

    #[must_use]
    pub const fn id(&self) -> SessionID {
        self.id
    }

    #[must_use]
    pub const fn phase(&self) -> Phase {
        self.phase
    }

    #[must_use]
    pub fn execution(
        &self,
        execution: ExecutionID,
    ) -> Option<Execution> {
        self.executions.get(&execution).copied()
    }

    pub fn executions(&self) -> impl Iterator<Item = (ExecutionID, Execution)> + '_ {
        self.executions
            .iter()
            .map(|(id, execution)| (*id, *execution))
    }
}

impl Execution {
    #[must_use]
    pub const fn deadline(&self) -> Deadline {
        self.deadline
    }

    #[must_use]
    pub const fn state(&self) -> State {
        self.state
    }
}

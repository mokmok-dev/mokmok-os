//! The per-turn machine, folded from a [`TurnFact`] history under the same shape as [`Session`].

use alloc::collections::{BTreeMap, BTreeSet};

use mokmok_protocol::ExecutionID;

use crate::fact::{Deadline, Resolution};
use crate::verdict::{Ack, Rejected, Rejection, rejected};

/// The position of a turn in its session's history.
#[derive(
    Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, serde::Deserialize, serde::Serialize,
)]
#[serde(transparent)]
pub struct TurnNumber(u64);

impl TurnNumber {
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Which model call this turn is on.
///
/// The first request is attempt one, and a retry carries the next value. The number is the fact's
/// identity, so a redelivered request reads as the same round while a retry is a new fact.
#[derive(
    Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, serde::Deserialize, serde::Serialize,
)]
#[serde(transparent)]
pub struct Attempt(u32);

impl Attempt {
    #[must_use]
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// How a turn ended.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnOutcome {
    /// The turn reached its goal.
    Completed,
    /// The turn was stopped by a cancel.
    Cancelled,
    /// The turn gave up: the model failed, or a tool did.
    Failed,
}

/// One tool execution requested under a turn.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Tool {
    deadline: Deadline,
    resolution: Option<Resolution>,
}

/// One recorded fact of a turn's history.
///
/// A fact's own content is its identity, like [`Fact`](crate::Fact): the bus redelivers without
/// distinguishing a first delivery from a retry, so every idempotence decision here is keyed on
/// the content.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(tag = "fact", rename_all = "snake_case")]
pub enum TurnFact {
    TurnOpened {
        number: TurnNumber,
    },
    ModelRequested {
        attempt: Attempt,
    },
    ModelAnswered,
    ToolRequested {
        execution: ExecutionID,
        deadline: Deadline,
    },
    ToolResolved {
        execution: ExecutionID,
        resolution: Resolution,
    },
    TurnSettled {
        outcome: TurnOutcome,
    },
}

/// The lifecycle of one turn, folded from its [`TurnFact`]s.
///
/// A turn starts unopened and, once opened, remembers one [`Tool`] per requested
/// [`ExecutionID`]. The value holds no handle, no clock, and no serialization, so the log is the
/// only durable form and [`Turn::replay`] is the only way back to it.
///
/// Re-applying a fact the state already holds never fails and never changes the state: every fact
/// of a valid history re-applied in order yields [`Ack::Duplicate`] and an equal turn.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Turn {
    number: Option<TurnNumber>,
    attempts: BTreeSet<Attempt>,
    answered: bool,
    tools: BTreeMap<ExecutionID, Tool>,
    outcome: Option<TurnOutcome>,
}

impl Turn {
    /// A turn that has recorded nothing yet.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            number: None,
            attempts: BTreeSet::new(),
            answered: false,
            tools: BTreeMap::new(),
            outcome: None,
        }
    }

    /// Rebuilds the state a driving path held by folding its whole log again.
    ///
    /// This is `facts.iter().try_fold(Turn::new(), Turn::apply)`: the driving path and the replay
    /// path run the same function, so they cannot drift.
    ///
    /// # Errors
    ///
    /// Returns [`Rejected`] at the first fact the history refuses, naming the offset into `facts`.
    pub fn replay(facts: &[TurnFact]) -> Result<Self, Rejected> {
        let mut turn = Self::new();
        for (offset, fact) in facts.iter().enumerate() {
            turn.apply(*fact)
                .map_err(|rejection| rejected(offset, rejection))?;
        }

        Ok(turn)
    }

    /// Records one fact, or reports why it cannot be recorded.
    ///
    /// Whether the fact's effect is already recorded is decided before any state precondition, so
    /// a redelivered fact is [`Ack::Duplicate`] rather than an error even after the turn settled.
    ///
    /// # Errors
    ///
    /// Returns [`Rejection::NotOpened`] before [`TurnFact::TurnOpened`], [`Rejection::TurnOver`]
    /// for a new fact after the turn settled, [`Rejection::ModelNotAnswered`] for a tool requested
    /// before the model answered, [`Rejection::UnrequestedAnswer`] for an answer
    /// with no request outstanding, and the per-fact refusals
    /// [`Rejection::UnknownExecution`], [`Rejection::DeadlineConflict`],
    /// [`Rejection::ResolutionConflict`], and [`Rejection::OutcomeConflict`]. A refusal leaves the
    /// turn as it found it.
    pub fn apply(
        &mut self,
        fact: TurnFact,
    ) -> Result<Ack, Rejection> {
        if self.number.is_none() && !matches!(fact, TurnFact::TurnOpened { .. }) {
            return Err(Rejection::NotOpened);
        }

        match fact {
            TurnFact::TurnOpened { number } => {
                if self.number.is_some() {
                    Ok(Ack::Duplicate)
                } else {
                    self.number = Some(number);
                    Ok(Ack::Applied)
                }
            },
            TurnFact::ModelRequested { attempt } => self.request(attempt),
            TurnFact::ModelAnswered => self.answer(),
            TurnFact::ToolRequested {
                execution,
                deadline,
            } => self.launch(execution, deadline),
            TurnFact::ToolResolved {
                execution,
                resolution,
            } => self.resolve(execution, resolution),
            TurnFact::TurnSettled { outcome } => self.settle(outcome),
        }
    }

    fn request(
        &mut self,
        attempt: Attempt,
    ) -> Result<Ack, Rejection> {
        if self.attempts.contains(&attempt) {
            return Ok(Ack::Duplicate);
        }
        if self.outcome.is_some() {
            return Err(Rejection::TurnOver);
        }
        self.attempts.insert(attempt);
        Ok(Ack::Applied)
    }

    fn answer(&mut self) -> Result<Ack, Rejection> {
        if self.answered {
            return Ok(Ack::Duplicate);
        }
        if self.outcome.is_some() {
            return Err(Rejection::TurnOver);
        }
        if self.attempts.is_empty() {
            return Err(Rejection::UnrequestedAnswer);
        }
        self.answered = true;
        Ok(Ack::Applied)
    }

    fn launch(
        &mut self,
        execution: ExecutionID,
        deadline: Deadline,
    ) -> Result<Ack, Rejection> {
        if let Some(recorded) = self.tools.get(&execution) {
            if recorded.deadline == deadline {
                Ok(Ack::Duplicate)
            } else {
                Err(Rejection::DeadlineConflict(execution))
            }
        } else if self.outcome.is_some() {
            Err(Rejection::TurnOver)
        } else if !self.answered {
            Err(Rejection::ModelNotAnswered)
        } else {
            self.tools.insert(
                execution,
                Tool {
                    deadline,
                    resolution: None,
                },
            );
            Ok(Ack::Applied)
        }
    }

    fn resolve(
        &mut self,
        execution: ExecutionID,
        resolution: Resolution,
    ) -> Result<Ack, Rejection> {
        let settled = self.outcome.is_some();
        let Some(recorded) = self.tools.get_mut(&execution) else {
            return Err(Rejection::UnknownExecution(execution));
        };
        match recorded.resolution {
            Some(current) if current == resolution => Ok(Ack::Duplicate),
            Some(_) => Err(Rejection::ResolutionConflict(execution)),
            None if settled => Err(Rejection::TurnOver),
            None => {
                recorded.resolution = Some(resolution);
                Ok(Ack::Applied)
            },
        }
    }

    fn settle(
        &mut self,
        outcome: TurnOutcome,
    ) -> Result<Ack, Rejection> {
        if let Some(recorded) = self.outcome {
            if recorded == outcome {
                Ok(Ack::Duplicate)
            } else {
                Err(Rejection::OutcomeConflict)
            }
        } else {
            self.outcome = Some(outcome);
            Ok(Ack::Applied)
        }
    }

    #[must_use]
    pub const fn number(&self) -> Option<TurnNumber> {
        self.number
    }

    #[must_use]
    pub const fn outcome(&self) -> Option<TurnOutcome> {
        self.outcome
    }

    /// Whether the turn holds a model answer, so a tool may be requested.
    #[must_use]
    pub const fn answered(&self) -> bool {
        self.answered
    }

    #[must_use]
    pub fn tool(
        &self,
        execution: ExecutionID,
    ) -> Option<Tool> {
        self.tools.get(&execution).copied()
    }

    pub fn tools(&self) -> impl Iterator<Item = (ExecutionID, Tool)> + '_ {
        self.tools.iter().map(|(id, tool)| (*id, *tool))
    }

    pub fn attempts(&self) -> impl Iterator<Item = Attempt> + '_ {
        self.attempts.iter().copied()
    }
}

impl Default for Turn {
    fn default() -> Self {
        Self::new()
    }
}

impl Tool {
    #[must_use]
    pub const fn deadline(&self) -> Deadline {
        self.deadline
    }

    /// The resolution this execution settled with, or `None` while it is unresolved.
    #[must_use]
    pub const fn resolution(&self) -> Option<Resolution> {
        self.resolution
    }
}

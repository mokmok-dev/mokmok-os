//! Validates the recorded history of an agent session and of one turn within it, folding each back
//! into its state.
//!
//! The crate is pure and deterministic. It reads no clock, no randomness, no filesystem, and no
//! socket, and neither [`Session`] nor [`Turn`] carries serialization: the log of [`Fact`]s or
//! [`TurnFact`]s is the only durable form and [`Session::replay`] or [`Turn::replay`] is the only
//! way back to the state a driving path held.
//!
//! Six properties hold, each enforced by a rule in [`Session::apply`]:
//!
//! - **A terminal session admits no fact that extends it.** Once the phase is [`Phase::Closed`], a
//!   new [`Fact::ExecutionLaunched`] is a [`Rejection::SessionOver`], a [`Fact::CancelRequested`]
//!   after [`Ending::Completed`] is too, and the only fact admitted is a redelivery of one the
//!   history already holds.
//! - **A resolution is recorded once and applied once.** [`Fact::ExecutionSettled`] matches a
//!   recorded execution by content: the same resolution is an [`Ack::Duplicate`], another is a
//!   [`Rejection::ResolutionConflict`], and neither writes. [`Fact::ResultApplied`] is an
//!   [`Ack::Duplicate`] once the result was handed over and a [`Rejection::NoResult`] for a
//!   resolution that never produced one, so a crash between the record and the effect cannot hand
//!   the result to the consumer twice. The order is settle, then apply, then close: a
//!   [`Fact::ResultApplied`] for a closed session is a [`Rejection::SessionOver`], and that result
//!   stays undelivered.
//! - **A refusal is typed and leaves no trace.** Every arm validates the fact completely before it
//!   mutates; the writes sit only on the `Applied` path, so a refused fact returns the state it
//!   found.
//! - **Cancel and timeout stay distinct.** [`Fact::CancelRequested`] is the session-scoped request
//!   that moves [`Phase::Accepting`] to [`Phase::Draining`]; `Draining` refuses a new
//!   [`Fact::ExecutionLaunched`] with [`Rejection::NotAccepting`] and decides the ending as
//!   [`Ending::Cancelled`]; a [`Resolution::Cancelled`] is refused outside `Draining` with
//!   [`Rejection::UnrequestedCancel`]. A [`Resolution::TimedOut`] needs [`Deadline::Bounded`], and
//!   an execution that declared [`Deadline::Unbounded`] is refused with [`Rejection::NoDeadline`].
//! - **Replay reconstructs the driven state.** [`Session::replay`] runs the same [`Session::apply`]
//!   the driving path runs, fact by fact, and stops at the first refusal naming the offset into the
//!   log, so the two paths cannot drift.
//! - **An unknown outcome settles without a result.** [`Resolution::Unknown`] records that the
//!   executor could not prove what it did. It leaves the execution like any other settlement, so a
//!   session holding one reaches [`Ending::Completed`] under the ordinary close rule, but it never
//!   becomes [`State::Applied`]: a [`Fact::ResultApplied`] for it is [`Rejection::NoResult`], so no
//!   caller can claim an outcome that was never proven. Reconciliation is the caller's duty, and
//!   the unknown stays visible in the session's state until it happens.
//!
//! One turn folds under the same [`Ack`] and [`Rejection`] shape, with rules of its own:
//!
//! - **A tool needs a model answer.** [`TurnFact::ToolRequested`] is
//!   [`Rejection::ModelNotAnswered`] until the turn holds a [`TurnFact::ModelAnswered`], and a
//!   [`TurnFact::ModelAnswered`] with no [`TurnFact::ModelRequested`] outstanding is
//!   [`Rejection::UnrequestedAnswer`].
//! - **A turn settles once, then admits no new fact.** [`TurnFact::TurnSettled`] with the same
//!   outcome is an [`Ack::Duplicate`], another outcome is [`Rejection::OutcomeConflict`], and any
//!   other fact the turn does not already hold is [`Rejection::TurnOver`].
//! - **A resolution names a requested execution.** A [`TurnFact::ToolResolved`] for an
//!   [`ExecutionID`](mokmok_protocol::ExecutionID) the turn never requested is
//!   [`Rejection::UnknownExecution`].
//! - **An execution is resolved at most once.** The same resolution again is an [`Ack::Duplicate`]
//!   and a different one is [`Rejection::ResolutionConflict`], so a redelivered outcome cannot
//!   rewrite a recorded settlement.
//! - **Replay reconstructs the driven state.** [`Turn::replay`] runs the same [`Turn::apply`] and
//!   stops at the first refusal naming the offset into the log.
//!
//! # Examples
//!
//! ```
//! use mokmok_protocol::SessionID;
//! use mokmok_state_machine::{Ack, Ending, Fact, Phase, Session};
//! use uuid::Uuid;
//!
//! let id = SessionID::from_uuid(Uuid::nil());
//! let mut session = Session::new(id);
//!
//! assert_eq!(session.apply(Fact::SessionOpened), Ok(Ack::Applied));
//! assert_eq!(session.apply(Fact::SessionClosed), Ok(Ack::Applied));
//! assert_eq!(session.phase(), Phase::Closed(Ending::Completed));
//!
//! assert_eq!(
//!     Session::replay(id, &[Fact::SessionOpened, Fact::SessionClosed]),
//!     Ok(session),
//! );
//! ```
//!
//! A turn opens, gates its tools on the model, and settles once:
//!
//! ```
//! use mokmok_protocol::ExecutionID;
//! use mokmok_state_machine::{
//!     Ack, Attempt, Deadline, Rejection, Turn, TurnFact, TurnNumber, TurnOutcome,
//! };
//! use uuid::Uuid;
//!
//! let mut turn = Turn::new();
//! let execution = ExecutionID::from_uuid(Uuid::from_u128(7));
//!
//! assert_eq!(turn.apply(TurnFact::TurnOpened { number: TurnNumber::new(1) }), Ok(Ack::Applied));
//! assert_eq!(
//!     turn.apply(TurnFact::ToolRequested { execution, deadline: Deadline::Bounded }),
//!     Err(Rejection::ModelNotAnswered),
//! );
//! assert_eq!(turn.apply(TurnFact::ModelRequested { attempt: Attempt::new(1) }), Ok(Ack::Applied));
//! assert_eq!(turn.apply(TurnFact::ModelAnswered), Ok(Ack::Applied));
//! assert_eq!(
//!     turn.apply(TurnFact::ToolRequested { execution, deadline: Deadline::Bounded }),
//!     Ok(Ack::Applied),
//! );
//! assert_eq!(
//!     turn.apply(TurnFact::TurnSettled { outcome: TurnOutcome::Completed }),
//!     Ok(Ack::Applied),
//! );
//! assert_eq!(
//!     turn.apply(TurnFact::ToolRequested { execution, deadline: Deadline::Bounded }),
//!     Ok(Ack::Duplicate),
//! );
//! ```

#![no_std]

extern crate alloc;

mod fact;
mod session;
mod turn;
mod verdict;

pub use fact::{Deadline, Fact, Resolution};
pub use session::{Ending, Execution, Phase, Session, State};
pub use turn::{Attempt, Tool, Turn, TurnFact, TurnNumber, TurnOutcome};
pub use verdict::{Ack, Rejected, Rejection};

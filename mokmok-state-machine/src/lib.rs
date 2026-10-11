//! Validates the recorded history of one agent session and folds it back into [`Session`] state.
//!
//! The crate is pure and deterministic. It reads no clock, no randomness, no filesystem, and no
//! socket, and [`Session`] carries no serialization: the log of [`Fact`]s is the only durable form
//! and [`Session::replay`] is the only way back to the state a driving path held.
//!
//! Five properties hold, each enforced by a rule in [`Session::apply`]:
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

#![no_std]

extern crate alloc;

mod fact;
mod session;
mod verdict;

pub use fact::{Deadline, Fact, Resolution};
pub use session::{Ending, Execution, Phase, Session, State};
pub use verdict::{Ack, Rejected, Rejection};

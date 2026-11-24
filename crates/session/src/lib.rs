//! A session lifecycle small enough to model check exhaustively.
//!
//! A session moves through [`SessionPhase`]s under [`SessionEvent`]s. [`transition`] is the one
//! place that decides which move is legal, [`Session::apply`] is the only way to take one, and
//! [`SessionModel`] hands the same table to stateright.

mod model;
mod session;

pub use model::SessionModel;
pub use session::{Session, SessionEvent, SessionPhase, transition};

//! The stateright model whose state space is every session a caller can build.

use stateright::{Model, Property};

use crate::session::{Session, SessionEvent, SessionPhase};

/// Exhaustive model of the session state space.
///
/// The state is a [`Session`], the actions are the six [`SessionEvent`]s, and the successor
/// relation is [`Session::apply`]. Because a session also remembers whether it was expired, the
/// checker reaches eleven states, which is what the tests pin.
pub struct SessionModel;

impl Model for SessionModel {
    type State = Session;
    type Action = SessionEvent;

    fn init_states(&self) -> Vec<Self::State> {
        vec![Session::pending()]
    }

    fn actions(&self, _state: &Self::State, actions: &mut Vec<Self::Action>) {
        actions.extend(SessionEvent::ALL);
    }

    fn next_state(&self, last_state: &Self::State, action: Self::Action) -> Option<Self::State> {
        last_state.apply(action)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        vec![
            Property::always(
                "a session that finished stays finished",
                |_: &Self, session: &Session| {
                    !session.ever_terminal() || session.phase().is_terminal()
                },
            ),
            Property::sometimes("a session can expire", |_: &Self, session: &Session| {
                session.phase() == SessionPhase::Expired
            }),
            Property::sometimes("a session can succeed", |_: &Self, session: &Session| {
                session.phase() == SessionPhase::Succeed
            }),
            Property::sometimes("a session can fail", |_: &Self, session: &Session| {
                session.phase() == SessionPhase::Failed
            }),
            Property::sometimes(
                "an expired session can return to pending",
                |_: &Self, session: &Session| {
                    session.was_expired() && session.phase() == SessionPhase::Pending
                },
            ),
        ]
    }
}

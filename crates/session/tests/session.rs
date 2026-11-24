use proptest::prelude::*;
use session::{Session, SessionEvent, SessionModel, SessionPhase, transition};
use stateright::{Checker, Model, Path};

/// The transition table written out independently of the implementation.
const EXPECTED: [[Option<SessionPhase>; 6]; 6] = {
    use SessionPhase as P;
    [
        [Some(P::Claimed), None, None, None, None, None],
        [None, Some(P::Running), Some(P::Expired), None, None, None],
        [
            None,
            None,
            Some(P::Expired),
            None,
            Some(P::Succeed),
            Some(P::Failed),
        ],
        [None, None, None, Some(P::Pending), None, None],
        [None, None, None, None, None, None],
        [None, None, None, None, None, None],
    ]
};

/// Folds `events` onto a fresh session, stopping at the first event with no edge.
fn advance(events: &[SessionEvent]) -> Result<Session, &'static str> {
    events
        .iter()
        .try_fold(Session::pending(), |session, event| {
            session
                .apply(*event)
                .ok_or("event has no edge from the current phase")
        })
}

#[test]
fn transition_matches_the_table() {
    for (phase_index, phase) in SessionPhase::ALL.iter().enumerate() {
        for (event_index, event) in SessionEvent::ALL.iter().enumerate() {
            assert_eq!(
                transition(*phase, *event),
                EXPECTED[phase_index][event_index],
                "{phase:?} + {event:?}"
            );
        }
    }
}

#[test]
fn terminal_phases_absorb_every_event() -> Result<(), &'static str> {
    let claimed = advance(&[SessionEvent::Claim])?;
    assert_eq!(
        Session::pending().apply(SessionEvent::Claim),
        Some(claimed),
        "a pending session accepts a claim"
    );

    let succeed = advance(&[
        SessionEvent::Claim,
        SessionEvent::Start,
        SessionEvent::Succeed,
    ])?;
    let failed = advance(&[SessionEvent::Claim, SessionEvent::Start, SessionEvent::Fail])?;
    for session in [succeed, failed] {
        let phase = session.phase();
        for event in SessionEvent::ALL {
            assert_eq!(session.apply(event), None, "{phase:?} + {event:?}");
        }
    }
    Ok(())
}

#[test]
fn entering_terminal_records_it() -> Result<(), &'static str> {
    let failed = advance(&[SessionEvent::Claim, SessionEvent::Start, SessionEvent::Fail])?;
    assert_eq!(failed.phase(), SessionPhase::Failed);
    assert!(failed.ever_terminal());

    let succeed = advance(&[
        SessionEvent::Claim,
        SessionEvent::Start,
        SessionEvent::Succeed,
    ])?;
    assert_eq!(succeed.phase(), SessionPhase::Succeed);
    assert!(succeed.ever_terminal());

    let running = advance(&[SessionEvent::Claim, SessionEvent::Start])?;
    assert!(!running.ever_terminal());
    Ok(())
}

#[test]
fn retry_returns_expired_to_pending() {
    let end = Path::from_actions(
        &SessionModel,
        Session::pending(),
        &[
            SessionEvent::Claim,
            SessionEvent::Expire,
            SessionEvent::Retry,
        ],
    )
    .map(|path| path.last_state().phase());
    assert_eq!(end, Some(SessionPhase::Pending));
}

#[test]
fn checker_explores_every_phase() {
    let checker = SessionModel.checker().spawn_bfs().join();
    assert!(checker.is_done());
    let states = checker.unique_state_count();
    println!("unique states: {states}");
    checker.assert_properties();
    assert_eq!(states, 6, "every session phase is reachable exactly once");
    checker.assert_discovery(
        "an expired session can return to pending",
        vec![
            SessionEvent::Claim,
            SessionEvent::Expire,
            SessionEvent::Retry,
        ],
    );
}

proptest! {
    #[test]
    fn terminal_flag_tracks_the_phase(
        events in proptest::collection::vec(
            proptest::sample::select(&SessionEvent::ALL[..]),
            0..8,
        ),
    ) {
        let mut session = Session::pending();
        prop_assert_eq!(session.phase().is_terminal(), session.ever_terminal());
        for event in events {
            let Some(next) = session.apply(event) else {
                break;
            };
            session = next;
            prop_assert_eq!(session.phase().is_terminal(), session.ever_terminal());
        }
    }
}

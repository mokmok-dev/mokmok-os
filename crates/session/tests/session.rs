use proptest::prelude::*;
use session::{Session, SessionEvent, SessionModel, SessionPhase, transition};
use stateright::{Checker, Model, Path};

/// The legal edges, written out independently of `transition`.
const LEGAL: [(SessionPhase, SessionEvent, SessionPhase); 7] = {
    use SessionEvent as E;
    use SessionPhase as P;
    [
        (P::Pending, E::Claim, P::Claimed),
        (P::Claimed, E::Start, P::Running),
        (P::Claimed, E::Expire, P::Expired),
        (P::Running, E::Expire, P::Expired),
        (P::Expired, E::Retry, P::Pending),
        (P::Running, E::Succeed, P::Succeed),
        (P::Running, E::Fail, P::Failed),
    ]
};

/// Folds `events` onto a fresh session, returning an error at the first event with no edge.
fn advance(events: &[SessionEvent]) -> Result<Session, &'static str> {
    events
        .iter()
        .try_fold(Session::pending(), |session, event| {
            session
                .apply(*event)
                .ok_or("event has no edge from the current phase")
        })
}

/// Walks `events` through [`transition`] alone, returning the phase it lands in and whether it
/// ever entered `Expired`.
fn expected_history(events: &[SessionEvent]) -> (SessionPhase, bool) {
    let mut phase = SessionPhase::Pending;
    let mut was_expired = false;
    for event in events {
        let Some(next) = transition(phase, *event) else {
            break;
        };
        phase = next;
        was_expired |= phase == SessionPhase::Expired;
    }
    (phase, was_expired)
}

#[test]
fn transition_matches_the_table() {
    for phase in SessionPhase::ALL {
        for event in SessionEvent::ALL {
            let expected = LEGAL
                .iter()
                .find(|(from, on, _)| *from == phase && *on == event)
                .map(|(_, _, next)| *next);
            assert_eq!(transition(phase, event), expected, "{phase:?} + {event:?}");
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
    checker.assert_properties();
    assert_eq!(
        states, 11,
        "five phases are reachable with and without a prior expiry, plus Expired, which \
         exists only after one"
    );
    checker.assert_discovery(
        "an expired session can return to pending",
        vec![
            SessionEvent::Claim,
            SessionEvent::Expire,
            SessionEvent::Retry,
        ],
    );
}

/// A retry property that the initial `Pending` state satisfies is found on a zero-action path
/// and proves nothing. Reject that state and assert the three-action path that remains.
#[test]
fn retry_property_needs_a_real_retry() {
    let property = SessionModel.property("an expired session can return to pending");
    assert!(
        !(property.condition)(&SessionModel, &Session::pending()),
        "the initial state satisfies the retry property, so its discovery proves nothing"
    );

    let checker = SessionModel.checker().spawn_bfs().join();
    assert!(checker.is_done());
    let discovery = checker
        .discovery("an expired session can return to pending")
        .map(Path::into_actions);
    assert_eq!(
        discovery,
        Some(vec![
            SessionEvent::Claim,
            SessionEvent::Expire,
            SessionEvent::Retry,
        ])
    );
}

proptest! {
    #[test]
    fn history_flags_track_the_events(
        events in proptest::collection::vec(
            proptest::sample::select(&SessionEvent::ALL[..]),
            0..8,
        ),
    ) {
        let mut session = Session::pending();
        prop_assert!(!session.was_expired());
        prop_assert_eq!(session.phase().is_terminal(), session.ever_terminal());
        for event in &events {
            let Some(next) = session.apply(*event) else {
                break;
            };
            session = next;
            prop_assert_eq!(session.phase().is_terminal(), session.ever_terminal());
        }
        let (phase, was_expired) = expected_history(&events);
        prop_assert_eq!(session.phase(), phase);
        prop_assert_eq!(session.was_expired(), was_expired);
    }
}

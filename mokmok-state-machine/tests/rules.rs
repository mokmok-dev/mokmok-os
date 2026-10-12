use mokmok_protocol::{ExecutionID, SessionID};
use mokmok_state_machine::{
    Ack, Deadline, Ending, Fact, Phase, Rejection, Resolution, Session, State,
};
use uuid::Uuid;

const fn execution(n: u128) -> ExecutionID {
    ExecutionID::from_uuid(Uuid::from_u128(n))
}

const fn session_id() -> SessionID {
    SessionID::from_uuid(Uuid::from_u128(0))
}

const fn e() -> ExecutionID {
    execution(1)
}

const fn e2() -> ExecutionID {
    execution(2)
}

const fn finished(code: i32) -> Resolution {
    Resolution::Finished { code }
}

fn opened() -> Session {
    let mut session = Session::new(session_id());
    assert_eq!(session.apply(Fact::SessionOpened), Ok(Ack::Applied));
    session
}

fn launched(deadline: Deadline) -> Session {
    let mut session = opened();
    assert_eq!(
        session.apply(Fact::ExecutionLaunched {
            execution: e(),
            deadline,
        }),
        Ok(Ack::Applied),
    );
    session
}

fn settled(resolution: Resolution) -> Session {
    let mut session = launched(Deadline::Bounded);
    assert_eq!(
        session.apply(Fact::ExecutionSettled {
            execution: e(),
            resolution,
        }),
        Ok(Ack::Applied),
    );
    session
}

fn applied() -> Session {
    let mut session = settled(finished(0));
    assert_eq!(
        session.apply(Fact::ResultApplied { execution: e() }),
        Ok(Ack::Applied),
    );
    session
}

fn draining() -> Session {
    let mut session = opened();
    assert_eq!(session.apply(Fact::CancelRequested), Ok(Ack::Applied));
    session
}

fn draining_launched(deadline: Deadline) -> Session {
    let mut session = launched(deadline);
    assert_eq!(session.apply(Fact::CancelRequested), Ok(Ack::Applied));
    session
}

fn draining_settled(resolution: Resolution) -> Session {
    let mut session = draining_launched(Deadline::Bounded);
    assert_eq!(
        session.apply(Fact::ExecutionSettled {
            execution: e(),
            resolution,
        }),
        Ok(Ack::Applied),
    );
    session
}

fn draining_applied() -> Session {
    let mut session = draining_settled(finished(0));
    assert_eq!(
        session.apply(Fact::ResultApplied { execution: e() }),
        Ok(Ack::Applied),
    );
    session
}

fn completed() -> Session {
    let mut session = opened();
    assert_eq!(session.apply(Fact::SessionClosed), Ok(Ack::Applied));
    session
}

fn cancelled() -> Session {
    let mut session = draining();
    assert_eq!(session.apply(Fact::SessionClosed), Ok(Ack::Applied));
    session
}

fn completed_settled(resolution: Resolution) -> Session {
    let mut session = settled(resolution);
    assert_eq!(session.apply(Fact::SessionClosed), Ok(Ack::Applied));
    session
}

fn completed_applied() -> Session {
    let mut session = applied();
    assert_eq!(session.apply(Fact::SessionClosed), Ok(Ack::Applied));
    session
}

fn cancelled_settled(resolution: Resolution) -> Session {
    let mut session = draining_settled(resolution);
    assert_eq!(session.apply(Fact::SessionClosed), Ok(Ack::Applied));
    session
}

struct Row {
    start: Session,
    fact: Fact,
    expected: Result<Ack, Rejection>,
    phase_after: Option<Phase>,
}

impl Row {
    const fn applied(
        start: Session,
        fact: Fact,
        phase: Phase,
    ) -> Self {
        Self {
            start,
            fact,
            expected: Ok(Ack::Applied),
            phase_after: Some(phase),
        }
    }

    const fn other(
        start: Session,
        fact: Fact,
        expected: Result<Ack, Rejection>,
    ) -> Self {
        Self {
            start,
            fact,
            expected,
            phase_after: None,
        }
    }
}

fn unopened_rows() -> Vec<Row> {
    vec![
        Row::applied(
            Session::new(session_id()),
            Fact::SessionOpened,
            Phase::Accepting,
        ),
        Row::other(
            Session::new(session_id()),
            Fact::ExecutionLaunched {
                execution: e(),
                deadline: Deadline::Bounded,
            },
            Err(Rejection::NotOpened),
        ),
        Row::other(
            Session::new(session_id()),
            Fact::ExecutionSettled {
                execution: e(),
                resolution: Resolution::Refused,
            },
            Err(Rejection::NotOpened),
        ),
        Row::other(
            Session::new(session_id()),
            Fact::CancelRequested,
            Err(Rejection::NotOpened),
        ),
        Row::other(
            Session::new(session_id()),
            Fact::ResultApplied { execution: e() },
            Err(Rejection::NotOpened),
        ),
        Row::other(
            Session::new(session_id()),
            Fact::SessionClosed,
            Err(Rejection::NotOpened),
        ),
    ]
}

fn accepting_launch_rows() -> Vec<Row> {
    vec![
        Row::other(opened(), Fact::SessionOpened, Ok(Ack::Duplicate)),
        Row::applied(
            opened(),
            Fact::ExecutionLaunched {
                execution: e(),
                deadline: Deadline::Bounded,
            },
            Phase::Accepting,
        ),
        Row::other(
            launched(Deadline::Bounded),
            Fact::ExecutionLaunched {
                execution: e(),
                deadline: Deadline::Bounded,
            },
            Ok(Ack::Duplicate),
        ),
        Row::other(
            launched(Deadline::Bounded),
            Fact::ExecutionLaunched {
                execution: e(),
                deadline: Deadline::Unbounded,
            },
            Err(Rejection::DeadlineConflict(e())),
        ),
        Row::other(
            settled(finished(0)),
            Fact::ExecutionLaunched {
                execution: e(),
                deadline: Deadline::Bounded,
            },
            Ok(Ack::Duplicate),
        ),
        Row::other(
            opened(),
            Fact::ExecutionSettled {
                execution: e(),
                resolution: finished(0),
            },
            Err(Rejection::UnknownExecution(e())),
        ),
        Row::applied(
            launched(Deadline::Bounded),
            Fact::ExecutionSettled {
                execution: e(),
                resolution: finished(0),
            },
            Phase::Accepting,
        ),
        Row::applied(
            launched(Deadline::Bounded),
            Fact::ExecutionSettled {
                execution: e(),
                resolution: Resolution::TimedOut,
            },
            Phase::Accepting,
        ),
        Row::other(
            launched(Deadline::Unbounded),
            Fact::ExecutionSettled {
                execution: e(),
                resolution: Resolution::TimedOut,
            },
            Err(Rejection::NoDeadline(e())),
        ),
        Row::other(
            launched(Deadline::Bounded),
            Fact::ExecutionSettled {
                execution: e(),
                resolution: Resolution::Cancelled,
            },
            Err(Rejection::UnrequestedCancel(e())),
        ),
        Row::other(
            settled(finished(0)),
            Fact::ExecutionSettled {
                execution: e(),
                resolution: finished(0),
            },
            Ok(Ack::Duplicate),
        ),
        Row::other(
            settled(finished(0)),
            Fact::ExecutionSettled {
                execution: e(),
                resolution: finished(1),
            },
            Err(Rejection::ResolutionConflict(e())),
        ),
        Row::other(
            settled(Resolution::TimedOut),
            Fact::ExecutionSettled {
                execution: e(),
                resolution: Resolution::Cancelled,
            },
            Err(Rejection::ResolutionConflict(e())),
        ),
        Row::applied(opened(), Fact::CancelRequested, Phase::Draining),
    ]
}

fn accepting_result_rows() -> Vec<Row> {
    vec![
        Row::other(
            opened(),
            Fact::ResultApplied { execution: e() },
            Err(Rejection::UnknownExecution(e())),
        ),
        Row::other(
            launched(Deadline::Bounded),
            Fact::ResultApplied { execution: e() },
            Err(Rejection::NoResult(e())),
        ),
        Row::applied(
            settled(finished(0)),
            Fact::ResultApplied { execution: e() },
            Phase::Accepting,
        ),
        Row::other(
            applied(),
            Fact::ResultApplied { execution: e() },
            Ok(Ack::Duplicate),
        ),
        Row::other(
            settled(Resolution::Killed { signal: 9 }),
            Fact::ResultApplied { execution: e() },
            Err(Rejection::NoResult(e())),
        ),
        Row::other(
            settled(Resolution::TimedOut),
            Fact::ResultApplied { execution: e() },
            Err(Rejection::NoResult(e())),
        ),
        Row::other(
            settled(Resolution::Refused),
            Fact::ResultApplied { execution: e() },
            Err(Rejection::NoResult(e())),
        ),
        Row::other(
            settled(Resolution::Unknown),
            Fact::ResultApplied { execution: e() },
            Err(Rejection::NoResult(e())),
        ),
        Row::applied(
            opened(),
            Fact::SessionClosed,
            Phase::Closed(Ending::Completed),
        ),
        Row::other(
            launched(Deadline::Bounded),
            Fact::SessionClosed,
            Err(Rejection::Unsettled),
        ),
        Row::applied(
            settled(Resolution::TimedOut),
            Fact::SessionClosed,
            Phase::Closed(Ending::Completed),
        ),
        Row::applied(
            settled(Resolution::Unknown),
            Fact::SessionClosed,
            Phase::Closed(Ending::Completed),
        ),
    ]
}

fn draining_rows() -> Vec<Row> {
    vec![
        Row::other(draining(), Fact::SessionOpened, Ok(Ack::Duplicate)),
        Row::other(
            draining(),
            Fact::ExecutionLaunched {
                execution: e(),
                deadline: Deadline::Bounded,
            },
            Err(Rejection::NotAccepting),
        ),
        Row::other(
            draining_launched(Deadline::Bounded),
            Fact::ExecutionLaunched {
                execution: e(),
                deadline: Deadline::Bounded,
            },
            Ok(Ack::Duplicate),
        ),
        Row::other(
            draining_launched(Deadline::Bounded),
            Fact::ExecutionLaunched {
                execution: e(),
                deadline: Deadline::Unbounded,
            },
            Err(Rejection::DeadlineConflict(e())),
        ),
        Row::applied(
            draining_launched(Deadline::Bounded),
            Fact::ExecutionSettled {
                execution: e(),
                resolution: Resolution::Cancelled,
            },
            Phase::Draining,
        ),
        Row::other(
            draining_launched(Deadline::Unbounded),
            Fact::ExecutionSettled {
                execution: e(),
                resolution: Resolution::TimedOut,
            },
            Err(Rejection::NoDeadline(e())),
        ),
        Row::other(
            draining_settled(Resolution::Cancelled),
            Fact::ExecutionSettled {
                execution: e(),
                resolution: Resolution::Cancelled,
            },
            Ok(Ack::Duplicate),
        ),
        Row::other(
            draining_settled(Resolution::Cancelled),
            Fact::ExecutionSettled {
                execution: e(),
                resolution: finished(0),
            },
            Err(Rejection::ResolutionConflict(e())),
        ),
        Row::other(draining(), Fact::CancelRequested, Ok(Ack::Duplicate)),
        Row::other(
            draining_applied(),
            Fact::ResultApplied { execution: e() },
            Ok(Ack::Duplicate),
        ),
        Row::other(
            draining_settled(Resolution::Cancelled),
            Fact::ResultApplied { execution: e() },
            Err(Rejection::NoResult(e())),
        ),
        Row::applied(
            draining_settled(finished(0)),
            Fact::ResultApplied { execution: e() },
            Phase::Draining,
        ),
        Row::other(
            draining_launched(Deadline::Bounded),
            Fact::SessionClosed,
            Err(Rejection::Unsettled),
        ),
        Row::applied(
            draining_settled(Resolution::Cancelled),
            Fact::SessionClosed,
            Phase::Closed(Ending::Cancelled),
        ),
    ]
}

fn unknown_rows() -> Vec<Row> {
    vec![
        Row::applied(
            launched(Deadline::Bounded),
            Fact::ExecutionSettled {
                execution: e(),
                resolution: Resolution::Unknown,
            },
            Phase::Accepting,
        ),
        Row::other(
            settled(Resolution::Unknown),
            Fact::ExecutionSettled {
                execution: e(),
                resolution: Resolution::Unknown,
            },
            Ok(Ack::Duplicate),
        ),
        Row::other(
            settled(Resolution::Unknown),
            Fact::ExecutionSettled {
                execution: e(),
                resolution: finished(0),
            },
            Err(Rejection::ResolutionConflict(e())),
        ),
    ]
}

fn closed_rows() -> Vec<Row> {
    vec![
        Row::other(cancelled(), Fact::SessionOpened, Ok(Ack::Duplicate)),
        Row::other(cancelled(), Fact::CancelRequested, Ok(Ack::Duplicate)),
        Row::other(
            completed(),
            Fact::CancelRequested,
            Err(Rejection::SessionOver),
        ),
        Row::other(
            completed(),
            Fact::ExecutionLaunched {
                execution: e2(),
                deadline: Deadline::Bounded,
            },
            Err(Rejection::SessionOver),
        ),
        Row::other(
            cancelled(),
            Fact::ExecutionLaunched {
                execution: e2(),
                deadline: Deadline::Bounded,
            },
            Err(Rejection::SessionOver),
        ),
        Row::other(
            cancelled_settled(Resolution::Cancelled),
            Fact::ExecutionSettled {
                execution: e(),
                resolution: Resolution::Cancelled,
            },
            Ok(Ack::Duplicate),
        ),
        Row::other(
            cancelled_settled(Resolution::Cancelled),
            Fact::ExecutionSettled {
                execution: e(),
                resolution: finished(0),
            },
            Err(Rejection::ResolutionConflict(e())),
        ),
        Row::other(
            completed_applied(),
            Fact::ResultApplied { execution: e() },
            Ok(Ack::Duplicate),
        ),
        Row::other(
            completed_settled(finished(0)),
            Fact::ResultApplied { execution: e() },
            Err(Rejection::SessionOver),
        ),
        Row::other(completed(), Fact::SessionClosed, Ok(Ack::Duplicate)),
        Row::other(cancelled(), Fact::SessionClosed, Ok(Ack::Duplicate)),
        Row::other(
            applied(),
            Fact::ExecutionSettled {
                execution: e(),
                resolution: finished(0),
            },
            Ok(Ack::Duplicate),
        ),
    ]
}

#[test]
fn the_verdict_matrix_holds_cell_by_cell() {
    let rows: Vec<Row> = unopened_rows()
        .into_iter()
        .chain(accepting_launch_rows())
        .chain(accepting_result_rows())
        .chain(draining_rows())
        .chain(unknown_rows())
        .chain(closed_rows())
        .collect();

    assert_eq!(rows.len(), 61);

    for (index, row) in rows.into_iter().enumerate() {
        let row_number = index + 1;
        let mut session = row.start;
        let before = session.clone();
        let actual = session.apply(row.fact);

        assert_eq!(actual, row.expected, "row {row_number}");

        match row.expected {
            Ok(Ack::Applied) => assert_eq!(
                session.phase(),
                row.phase_after.expect("an applied row names its phase"),
                "row {row_number}",
            ),
            Ok(Ack::Duplicate) | Err(_) => {
                assert_eq!(session, before, "row {row_number}");
            },
        }
    }
}

#[test]
fn replay_reports_the_offset_and_the_rejection() {
    let facts = [
        Fact::SessionOpened,
        Fact::CancelRequested,
        Fact::ExecutionLaunched {
            execution: e(),
            deadline: Deadline::Bounded,
        },
    ];

    let rejected = Session::replay(session_id(), &facts).unwrap_err();

    assert_eq!(rejected.offset(), 2);
    assert_eq!(rejected.rejection(), Rejection::NotAccepting);
    assert_eq!(
        rejected.to_string(),
        "fact 2 of the log: the session is cancelled and accepting no new work",
    );
}

#[test]
fn a_settle_redelivery_after_the_result_is_applied_is_a_duplicate() {
    let mut session = applied();
    assert_eq!(
        session.apply(Fact::ExecutionSettled {
            execution: e(),
            resolution: finished(0),
        }),
        Ok(Ack::Duplicate),
    );

    let mut killed = settled(Resolution::Killed { signal: 9 });
    assert_eq!(
        killed.apply(Fact::ExecutionSettled {
            execution: e(),
            resolution: Resolution::Killed { signal: 9 },
        }),
        Ok(Ack::Duplicate),
    );
    assert_eq!(
        killed.apply(Fact::ExecutionSettled {
            execution: e(),
            resolution: Resolution::Killed { signal: 15 },
        }),
        Err(Rejection::ResolutionConflict(e())),
    );

    let mut timed_out = settled(Resolution::TimedOut);
    assert_eq!(
        timed_out.apply(Fact::ExecutionSettled {
            execution: e(),
            resolution: Resolution::Refused,
        }),
        Err(Rejection::ResolutionConflict(e())),
    );
}

#[test]
fn a_result_after_an_unknown_settle_is_no_result() {
    let mut session = settled(Resolution::Unknown);

    assert_eq!(
        session.apply(Fact::ResultApplied { execution: e() }),
        Err(Rejection::NoResult(e())),
    );
    assert_eq!(
        session.execution(e()).unwrap().state(),
        State::Settled(Resolution::Unknown),
    );
}

#[test]
fn replay_of_a_history_holding_an_unknown_reconstructs_the_driven_state() {
    let facts = [
        Fact::SessionOpened,
        Fact::ExecutionLaunched {
            execution: e(),
            deadline: Deadline::Bounded,
        },
        Fact::ExecutionSettled {
            execution: e(),
            resolution: Resolution::Unknown,
        },
        Fact::SessionClosed,
    ];

    let mut driven = Session::new(session_id());
    for fact in facts {
        assert_eq!(driven.apply(fact), Ok(Ack::Applied));
    }

    assert_eq!(Session::replay(session_id(), &facts), Ok(driven));
}

#[test]
fn a_session_holding_an_unknown_still_reaches_completed() {
    let mut session = settled(Resolution::Unknown);

    assert_eq!(session.apply(Fact::SessionClosed), Ok(Ack::Applied));
    assert_eq!(session.phase(), Phase::Closed(Ending::Completed));
    assert_eq!(
        session.execution(e()).unwrap().state(),
        State::Settled(Resolution::Unknown),
    );
}

#[test]
fn display_of_not_opened_is_literal() {
    assert_eq!(
        Rejection::NotOpened.to_string(),
        "the session has not been opened",
    );
}

#[test]
fn display_of_not_accepting_is_literal() {
    assert_eq!(
        Rejection::NotAccepting.to_string(),
        "the session is cancelled and accepting no new work",
    );
}

#[test]
fn display_of_session_over_is_literal() {
    assert_eq!(Rejection::SessionOver.to_string(), "the session is over");
}

#[test]
fn display_of_unsettled_is_literal() {
    assert_eq!(
        Rejection::Unsettled.to_string(),
        "an execution is still in flight",
    );
}

#[test]
fn display_of_unknown_execution_is_literal() {
    assert_eq!(
        Rejection::UnknownExecution(e()).to_string(),
        "no execution 00000000-0000-0000-0000-000000000001 in this session",
    );
}

#[test]
fn display_of_deadline_conflict_is_literal() {
    assert_eq!(
        Rejection::DeadlineConflict(e()).to_string(),
        "execution 00000000-0000-0000-0000-000000000001 was launched with a different deadline",
    );
}

#[test]
fn display_of_resolution_conflict_is_literal() {
    assert_eq!(
        Rejection::ResolutionConflict(e()).to_string(),
        "execution 00000000-0000-0000-0000-000000000001 already settled another way",
    );
}

#[test]
fn display_of_unrequested_cancel_is_literal() {
    assert_eq!(
        Rejection::UnrequestedCancel(e()).to_string(),
        "execution 00000000-0000-0000-0000-000000000001 cannot be cancelled while the session is not cancelling",
    );
}

#[test]
fn display_of_no_deadline_is_literal() {
    assert_eq!(
        Rejection::NoDeadline(e()).to_string(),
        "execution 00000000-0000-0000-0000-000000000001 declared no deadline, so it cannot time out",
    );
}

#[test]
fn display_of_no_result_is_literal() {
    assert_eq!(
        Rejection::NoResult(e()).to_string(),
        "execution 00000000-0000-0000-0000-000000000001 has no unconsumed result",
    );
}

#[test]
fn the_fact_vocabulary_serde_as_the_contract_says() {
    assert_eq!(
        serde_json::to_string(&Fact::SessionOpened).unwrap(),
        r#"{"fact":"session_opened"}"#,
    );
    assert_eq!(
        serde_json::to_string(&Deadline::Bounded).unwrap(),
        r#""bounded""#,
    );
    assert_eq!(
        serde_json::to_string(&Resolution::TimedOut).unwrap(),
        r#""timed_out""#,
    );
    assert_eq!(
        serde_json::to_string(&Resolution::Unknown).unwrap(),
        r#""unknown""#,
    );
    assert_eq!(
        serde_json::to_string(&Resolution::Finished { code: 0 }).unwrap(),
        r#"{"finished":{"code":0}}"#,
    );
    assert_eq!(
        serde_json::to_string(&Resolution::Killed { signal: 9 }).unwrap(),
        r#"{"killed":{"signal":9}}"#,
    );

    let fact = Fact::ExecutionLaunched {
        execution: e(),
        deadline: Deadline::Unbounded,
    };
    let encoded = serde_json::to_string(&fact).unwrap();

    assert_eq!(serde_json::from_str::<Fact>(&encoded).unwrap(), fact);
}

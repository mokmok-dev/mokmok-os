use std::collections::BTreeMap;

use mokmok_protocol::{ExecutionID, SessionID};
use mokmok_state_machine::{
    Ack, Deadline, Ending, Fact, Phase, Rejection, Resolution, Session, State,
};
use proptest::prelude::*;
use proptest::strategy::BoxedStrategy;
use proptest_state_machine::{ReferenceStateMachine, StateMachineTest, prop_state_machine};
use uuid::Uuid;

const fn execution(n: u128) -> ExecutionID {
    ExecutionID::from_uuid(Uuid::from_u128(n))
}

const fn session_id() -> SessionID {
    SessionID::from_uuid(Uuid::from_u128(0))
}

/// The settlement a state records, re-derived from the public [`State`] rather than read back from
/// the machine. An [`State::Applied`] execution settled as `Finished { code }`.
const fn settlement_of(state: State) -> Option<Resolution> {
    match state {
        State::Settled(resolution) => Some(resolution),
        State::Applied { code } => Some(Resolution::Finished { code }),
        State::InFlight => None,
    }
}

fn settlement_verdict(
    current: Resolution,
    resolution: Resolution,
    execution: ExecutionID,
) -> Verdict {
    if current == resolution {
        Verdict::Duplicate
    } else {
        Verdict::Refuse(Rejection::ResolutionConflict(execution))
    }
}

/// A second, independent derivation of the session rules from the recorded history.
///
/// The verdict is derived from the facts the history holds rather than by calling
/// [`Session::apply`], so a rule that drifts in either implementation shows up as a disagreement.
#[derive(Clone, Debug)]
struct Model {
    history: Vec<Fact>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Verdict {
    Apply,
    Duplicate,
    Refuse(Rejection),
}

impl Model {
    const fn new() -> Self {
        Self {
            history: Vec::new(),
        }
    }

    fn phase(&self) -> Phase {
        if !self
            .history
            .iter()
            .any(|fact| matches!(fact, Fact::SessionOpened))
        {
            return Phase::Unopened;
        }
        if self
            .history
            .iter()
            .any(|fact| matches!(fact, Fact::SessionClosed))
        {
            let ending = if self.cancelled() {
                Ending::Cancelled
            } else {
                Ending::Completed
            };

            return Phase::Closed(ending);
        }
        if self.cancelled() {
            Phase::Draining
        } else {
            Phase::Accepting
        }
    }

    fn cancelled(&self) -> bool {
        self.history
            .iter()
            .any(|fact| matches!(fact, Fact::CancelRequested))
    }

    fn executions(&self) -> BTreeMap<ExecutionID, (Deadline, State)> {
        let mut table: BTreeMap<ExecutionID, (Deadline, State)> = BTreeMap::new();
        for fact in &self.history {
            match fact {
                Fact::ExecutionLaunched {
                    execution,
                    deadline,
                } => {
                    table
                        .entry(*execution)
                        .or_insert((*deadline, State::InFlight));
                },
                Fact::ExecutionSettled {
                    execution,
                    resolution,
                } => {
                    if let Some((_, state)) = table.get_mut(execution)
                        && *state == State::InFlight
                    {
                        *state = State::Settled(*resolution);
                    }
                },
                Fact::ResultApplied { execution } => {
                    if let Some((_, state)) = table.get_mut(execution)
                        && let State::Settled(Resolution::Finished { code }) = *state
                    {
                        *state = State::Applied { code };
                    }
                },
                Fact::SessionOpened | Fact::CancelRequested | Fact::SessionClosed => {},
            }
        }
        table
    }

    fn before_last(&self) -> Self {
        let mut history = self.history.clone();
        history.pop();
        Self { history }
    }

    fn verdict(
        &self,
        fact: Fact,
    ) -> Verdict {
        let phase = self.phase();
        if phase == Phase::Unopened {
            return if matches!(fact, Fact::SessionOpened) {
                Verdict::Apply
            } else {
                Verdict::Refuse(Rejection::NotOpened)
            };
        }

        let executions = self.executions();
        match fact {
            Fact::SessionOpened => Verdict::Duplicate,
            Fact::ExecutionLaunched {
                execution,
                deadline,
            } => match executions.get(&execution) {
                Some((recorded, _)) if *recorded == deadline => Verdict::Duplicate,
                Some(_) => Verdict::Refuse(Rejection::DeadlineConflict(execution)),
                None => match phase {
                    Phase::Accepting => Verdict::Apply,
                    Phase::Draining => Verdict::Refuse(Rejection::NotAccepting),
                    Phase::Closed(_) => Verdict::Refuse(Rejection::SessionOver),
                    Phase::Unopened => Verdict::Refuse(Rejection::NotOpened),
                },
            },
            Fact::ExecutionSettled {
                execution,
                resolution,
            } => match executions.get(&execution) {
                None => Verdict::Refuse(Rejection::UnknownExecution(execution)),
                Some((deadline, state)) => match *state {
                    State::InFlight => {
                        if resolution == Resolution::Cancelled && phase != Phase::Draining {
                            Verdict::Refuse(Rejection::UnrequestedCancel(execution))
                        } else if resolution == Resolution::TimedOut
                            && *deadline == Deadline::Unbounded
                        {
                            Verdict::Refuse(Rejection::NoDeadline(execution))
                        } else {
                            Verdict::Apply
                        }
                    },
                    State::Settled(current) => settlement_verdict(current, resolution, execution),
                    State::Applied { code } => {
                        settlement_verdict(Resolution::Finished { code }, resolution, execution)
                    },
                },
            },
            Fact::CancelRequested => match phase {
                Phase::Accepting => Verdict::Apply,
                Phase::Draining | Phase::Closed(Ending::Cancelled) => Verdict::Duplicate,
                Phase::Closed(Ending::Completed) => Verdict::Refuse(Rejection::SessionOver),
                Phase::Unopened => Verdict::Refuse(Rejection::NotOpened),
            },
            Fact::ResultApplied { execution } => match executions.get(&execution) {
                None => Verdict::Refuse(Rejection::UnknownExecution(execution)),
                Some((_, state)) => {
                    if matches!(state, State::Applied { .. }) {
                        Verdict::Duplicate
                    } else if matches!(state, State::Settled(Resolution::Finished { .. })) {
                        match phase {
                            Phase::Closed(_) => Verdict::Refuse(Rejection::SessionOver),
                            Phase::Accepting | Phase::Draining => Verdict::Apply,
                            Phase::Unopened => Verdict::Refuse(Rejection::NotOpened),
                        }
                    } else {
                        Verdict::Refuse(Rejection::NoResult(execution))
                    }
                },
            },
            Fact::SessionClosed => match phase {
                Phase::Closed(_) => Verdict::Duplicate,
                Phase::Accepting | Phase::Draining => {
                    if executions
                        .values()
                        .all(|(_, state)| settlement_of(*state).is_some())
                    {
                        Verdict::Apply
                    } else {
                        Verdict::Refuse(Rejection::Unsettled)
                    }
                },
                Phase::Unopened => Verdict::Refuse(Rejection::NotOpened),
            },
        }
    }
}

/// The state the model derives, in the shape the system under test reports it.
fn snapshot(session: &Session) -> BTreeMap<ExecutionID, (Deadline, State)> {
    session
        .executions()
        .map(|(execution, value)| (execution, (value.deadline(), value.state())))
        .collect()
}

/// The system under test, plus the settlement it has recorded for each execution.
struct System {
    session: Session,
    settled: BTreeMap<ExecutionID, Resolution>,
}

struct Machine;

impl ReferenceStateMachine for Model {
    type State = Self;
    type Transition = Fact;

    fn init_state() -> BoxedStrategy<Self::State> {
        Just(Self::new()).boxed()
    }

    fn transitions(state: &Self::State) -> BoxedStrategy<Self::Transition> {
        let candidates: Vec<Fact> = candidate_facts()
            .into_iter()
            .filter(|fact| !matches!(state.verdict(*fact), Verdict::Refuse(_)))
            .collect();

        proptest::sample::select(candidates).boxed()
    }

    fn apply(
        mut state: Self::State,
        transition: &Self::Transition,
    ) -> Self::State {
        state.history.push(*transition);
        state
    }

    fn preconditions(
        state: &Self::State,
        transition: &Self::Transition,
    ) -> bool {
        !matches!(state.verdict(*transition), Verdict::Refuse(_))
    }
}

impl StateMachineTest for Machine {
    type SystemUnderTest = System;
    type Reference = Model;

    fn init_test(_ref_state: &Model) -> System {
        System {
            session: Session::new(session_id()),
            settled: BTreeMap::new(),
        }
    }

    fn apply(
        mut state: System,
        ref_state: &Model,
        transition: Fact,
    ) -> System {
        let expected = ref_state.before_last().verdict(transition);
        let actual = state.session.apply(transition);

        match expected {
            Verdict::Apply => assert_eq!(actual, Ok(Ack::Applied), "{transition:?}"),
            Verdict::Duplicate => assert_eq!(actual, Ok(Ack::Duplicate), "{transition:?}"),
            Verdict::Refuse(rejection) => assert_eq!(actual, Err(rejection), "{transition:?}"),
        }

        for (execution, previous) in state.settled.clone() {
            let current = state
                .session
                .execution(execution)
                .and_then(|value| settlement_of(value.state()));
            assert_eq!(
                current,
                Some(previous),
                "resolution of {execution:?} changed or returned to flight",
            );
        }
        for (execution, value) in state.session.executions() {
            if let Some(settlement) = settlement_of(value.state()) {
                state.settled.insert(execution, settlement);
            }
        }

        state
    }

    fn check_invariants(
        state: &System,
        ref_state: &Model,
    ) {
        assert_eq!(state.session.phase(), ref_state.phase());
        assert_eq!(snapshot(&state.session), ref_state.executions());

        if let Phase::Closed(_) = state.session.phase() {
            assert!(
                state
                    .session
                    .executions()
                    .all(|(_, value)| settlement_of(value.state()).is_some()),
            );
        }

        assert_eq!(
            matches!(
                state.session.phase(),
                Phase::Draining | Phase::Closed(Ending::Cancelled),
            ),
            ref_state.cancelled(),
        );

        for execution in state.settled.keys() {
            assert!(
                state
                    .session
                    .execution(*execution)
                    .and_then(|value| settlement_of(value.state()))
                    .is_some(),
                "execution {execution:?} returned to flight after settling",
            );
        }
    }
}

fn candidate_facts() -> Vec<Fact> {
    let mut facts = vec![
        Fact::SessionOpened,
        Fact::CancelRequested,
        Fact::SessionClosed,
    ];
    for execution in [execution(1), execution(2)] {
        facts.extend([
            Fact::ExecutionLaunched {
                execution,
                deadline: Deadline::Bounded,
            },
            Fact::ExecutionLaunched {
                execution,
                deadline: Deadline::Unbounded,
            },
            Fact::ExecutionSettled {
                execution,
                resolution: Resolution::Finished { code: 0 },
            },
            Fact::ExecutionSettled {
                execution,
                resolution: Resolution::Finished { code: 1 },
            },
            Fact::ExecutionSettled {
                execution,
                resolution: Resolution::Killed { signal: 9 },
            },
            Fact::ExecutionSettled {
                execution,
                resolution: Resolution::TimedOut,
            },
            Fact::ExecutionSettled {
                execution,
                resolution: Resolution::Cancelled,
            },
            Fact::ExecutionSettled {
                execution,
                resolution: Resolution::Refused,
            },
            Fact::ResultApplied { execution },
        ]);
    }
    facts
}

fn arbitrary_fact(ids: [ExecutionID; 4]) -> impl Strategy<Value = Fact> {
    let execution = (0..4_usize, 0..9_usize).prop_map(move |(index, kind)| {
        let execution = ids[index];
        match kind {
            0 => Fact::ExecutionLaunched {
                execution,
                deadline: Deadline::Bounded,
            },
            1 => Fact::ExecutionLaunched {
                execution,
                deadline: Deadline::Unbounded,
            },
            2 => Fact::ExecutionSettled {
                execution,
                resolution: Resolution::Finished { code: 0 },
            },
            3 => Fact::ExecutionSettled {
                execution,
                resolution: Resolution::Finished { code: 1 },
            },
            4 => Fact::ExecutionSettled {
                execution,
                resolution: Resolution::Killed { signal: 9 },
            },
            5 => Fact::ExecutionSettled {
                execution,
                resolution: Resolution::TimedOut,
            },
            6 => Fact::ExecutionSettled {
                execution,
                resolution: Resolution::Cancelled,
            },
            7 => Fact::ExecutionSettled {
                execution,
                resolution: Resolution::Refused,
            },
            _ => Fact::ResultApplied { execution },
        }
    });

    prop_oneof![
        2 => Just(Fact::SessionOpened),
        1 => Just(Fact::CancelRequested),
        1 => Just(Fact::SessionClosed),
        8 => execution,
    ]
}

fn arbitrary_history() -> impl Strategy<Value = Vec<Fact>> {
    proptest::collection::vec(
        arbitrary_fact([execution(1), execution(2), execution(3), execution(4)]),
        1..=12,
    )
    .prop_map(|mut facts| {
        facts[0] = Fact::SessionOpened;
        facts
    })
}

/// Folds `facts`, keeping the facts the machine applied rather than repeated.
fn fold(facts: Vec<Fact>) -> (Session, Vec<Fact>) {
    let mut session = Session::new(session_id());
    let mut accepted = Vec::new();
    for fact in facts {
        if session.apply(fact) == Ok(Ack::Applied) {
            accepted.push(fact);
        }
    }
    (session, accepted)
}

prop_state_machine! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn the_machine_agrees_with_the_model_over_every_valid_history(sequential 1..20 => Machine);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn the_machine_and_model_agree_on_arbitrary_facts(facts in arbitrary_history()) {
        let mut session = Session::new(session_id());
        let mut model = Model::new();

        for fact in facts {
            let expected = model.verdict(fact);
            let actual = session.apply(fact);

            match expected {
                Verdict::Apply => prop_assert_eq!(actual, Ok(Ack::Applied)),
                Verdict::Duplicate => prop_assert_eq!(actual, Ok(Ack::Duplicate)),
                Verdict::Refuse(rejection) => prop_assert_eq!(actual, Err(rejection)),
            }

            if !matches!(expected, Verdict::Refuse(_)) {
                model.history.push(fact);
            }

            prop_assert_eq!(session.phase(), model.phase());
            prop_assert_eq!(snapshot(&session), model.executions());
        }
    }

    #[test]
    fn reapplying_the_accepted_facts_is_all_duplicates_and_a_fixpoint(facts in arbitrary_history()) {
        let (mut session, accepted) = fold(facts);
        prop_assert_eq!(accepted.first(), Some(&Fact::SessionOpened));
        let driven = session.clone();

        for fact in &accepted {
            prop_assert_eq!(session.apply(*fact), Ok(Ack::Duplicate));
            prop_assert_eq!(&session, &driven);
        }
    }

    #[test]
    fn replay_of_the_accepted_facts_equals_the_driven_state(facts in arbitrary_history()) {
        let (session, accepted) = fold(facts);
        prop_assert_eq!(accepted.first(), Some(&Fact::SessionOpened));
        prop_assert_ne!(session.phase(), Phase::Unopened);

        prop_assert_eq!(Session::replay(session_id(), &accepted).unwrap(), session);
    }

    #[test]
    fn a_prefix_then_the_rest_equals_the_whole(facts in arbitrary_history()) {
        let (session, accepted) = fold(facts);
        prop_assert_eq!(accepted.first(), Some(&Fact::SessionOpened));
        prop_assert_ne!(session.phase(), Phase::Unopened);

        for split in 0..=accepted.len() {
            let mut rebuilt = Session::replay(session_id(), &accepted[..split]).unwrap();
            for fact in &accepted[split..] {
                rebuilt.apply(*fact).unwrap();
            }
            prop_assert_eq!(&rebuilt, &session);
        }
    }

    #[test]
    fn a_refusal_leaves_the_state_untouched(facts in arbitrary_history()) {
        let (mut session, accepted) = fold(facts);
        prop_assert_eq!(accepted.first(), Some(&Fact::SessionOpened));

        for fact in candidate_facts() {
            let before = session.clone();
            if session.apply(fact) == Ok(Ack::Applied) {
                prop_assert_ne!(&session, &before);
            } else {
                prop_assert_eq!(&session, &before);
            }
        }
    }
}

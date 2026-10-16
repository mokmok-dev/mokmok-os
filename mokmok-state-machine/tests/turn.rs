use std::collections::{BTreeMap, BTreeSet};

use mokmok_protocol::ExecutionID;
use mokmok_state_machine::{
    Ack, Attempt, Deadline, Rejection, Resolution, Turn, TurnFact, TurnNumber, TurnOutcome,
};
use proptest::prelude::*;
use uuid::Uuid;

const fn execution(n: u128) -> ExecutionID {
    ExecutionID::from_uuid(Uuid::from_u128(n))
}

const fn e() -> ExecutionID {
    execution(1)
}

const fn e2() -> ExecutionID {
    execution(2)
}

const fn opening() -> TurnFact {
    TurnFact::TurnOpened {
        number: TurnNumber::new(1),
    }
}

fn opened() -> Turn {
    let mut turn = Turn::new();
    assert_eq!(turn.apply(opening()), Ok(Ack::Applied));
    turn
}

fn answered() -> Turn {
    let mut turn = opened();
    assert_eq!(
        turn.apply(TurnFact::ModelRequested {
            attempt: Attempt::new(1),
        }),
        Ok(Ack::Applied),
    );
    assert_eq!(turn.apply(TurnFact::ModelAnswered), Ok(Ack::Applied));
    turn
}

fn requested() -> Turn {
    let mut turn = answered();
    assert_eq!(
        turn.apply(TurnFact::ToolRequested {
            execution: e(),
            deadline: Deadline::Bounded,
        }),
        Ok(Ack::Applied),
    );
    turn
}

fn settled(outcome: TurnOutcome) -> Turn {
    let mut turn = requested();
    assert_eq!(
        turn.apply(TurnFact::TurnSettled { outcome }),
        Ok(Ack::Applied)
    );
    turn
}

#[test]
fn a_number_and_an_attempt_report_the_value_they_carry() {
    assert_eq!(TurnNumber::new(7).get(), 7);
    assert_eq!(Attempt::new(3).get(), 3);
}

#[test]
fn a_tool_before_a_model_answer_is_refused() {
    let mut turn = opened();

    assert_eq!(
        turn.apply(TurnFact::ToolRequested {
            execution: e(),
            deadline: Deadline::Bounded,
        }),
        Err(Rejection::ModelNotAnswered),
    );
    assert_eq!(turn.tool(e()), None);

    assert_eq!(
        turn.apply(TurnFact::ModelRequested {
            attempt: Attempt::new(1),
        }),
        Ok(Ack::Applied),
    );
    assert_eq!(
        turn.apply(TurnFact::ToolRequested {
            execution: e(),
            deadline: Deadline::Bounded,
        }),
        Err(Rejection::ModelNotAnswered),
    );

    assert_eq!(turn.apply(TurnFact::ModelAnswered), Ok(Ack::Applied));
    assert_eq!(
        turn.apply(TurnFact::ToolRequested {
            execution: e(),
            deadline: Deadline::Bounded,
        }),
        Ok(Ack::Applied),
    );
}

#[test]
fn a_retry_is_a_new_request_while_a_redelivery_is_a_duplicate() {
    let mut turn = answered();

    assert_eq!(
        turn.apply(TurnFact::ModelRequested {
            attempt: Attempt::new(2),
        }),
        Ok(Ack::Applied),
    );
    assert_eq!(
        turn.apply(TurnFact::ModelRequested {
            attempt: Attempt::new(2),
        }),
        Ok(Ack::Duplicate),
    );
    assert_eq!(
        turn.apply(TurnFact::ModelRequested {
            attempt: Attempt::new(1),
        }),
        Ok(Ack::Duplicate),
    );
}

#[test]
fn an_answer_without_a_request_is_refused() {
    let mut turn = opened();

    assert_eq!(
        turn.apply(TurnFact::ModelAnswered),
        Err(Rejection::UnrequestedAnswer),
    );
}

#[test]
fn a_turn_settles_once() {
    let mut turn = settled(TurnOutcome::Completed);

    assert_eq!(
        turn.apply(TurnFact::TurnSettled {
            outcome: TurnOutcome::Completed,
        }),
        Ok(Ack::Duplicate),
    );
    assert_eq!(
        turn.apply(TurnFact::TurnSettled {
            outcome: TurnOutcome::Failed,
        }),
        Err(Rejection::OutcomeConflict),
    );
    assert_eq!(turn.outcome(), Some(TurnOutcome::Completed));
}

#[test]
fn a_settled_turn_admits_no_new_fact() {
    let mut turn = opened();
    assert_eq!(
        turn.apply(TurnFact::ModelRequested {
            attempt: Attempt::new(1),
        }),
        Ok(Ack::Applied),
    );
    assert_eq!(
        turn.apply(TurnFact::TurnSettled {
            outcome: TurnOutcome::Cancelled,
        }),
        Ok(Ack::Applied),
    );

    assert_eq!(
        turn.apply(TurnFact::ModelAnswered),
        Err(Rejection::TurnOver),
    );
    assert_eq!(
        turn.apply(TurnFact::ToolRequested {
            execution: e(),
            deadline: Deadline::Bounded,
        }),
        Err(Rejection::TurnOver),
    );
    assert_eq!(
        turn.apply(TurnFact::TurnSettled {
            outcome: TurnOutcome::Failed,
        }),
        Err(Rejection::OutcomeConflict),
    );

    assert_eq!(turn.apply(opening()), Ok(Ack::Duplicate));
    assert_eq!(
        turn.apply(TurnFact::ModelRequested {
            attempt: Attempt::new(1),
        }),
        Ok(Ack::Duplicate),
    );
}

#[test]
fn a_resolution_for_an_unrequested_execution_is_refused() {
    let mut turn = answered();

    assert_eq!(
        turn.apply(TurnFact::ToolResolved {
            execution: e(),
            resolution: Resolution::Finished { code: 0 },
        }),
        Err(Rejection::UnknownExecution(e())),
    );
}

#[test]
fn an_execution_is_resolved_at_most_once() {
    let mut turn = requested();

    assert_eq!(
        turn.apply(TurnFact::ToolResolved {
            execution: e(),
            resolution: Resolution::Finished { code: 0 },
        }),
        Ok(Ack::Applied),
    );
    assert_eq!(
        turn.apply(TurnFact::ToolResolved {
            execution: e(),
            resolution: Resolution::Finished { code: 0 },
        }),
        Ok(Ack::Duplicate),
    );
    assert_eq!(
        turn.apply(TurnFact::ToolResolved {
            execution: e(),
            resolution: Resolution::Finished { code: 1 },
        }),
        Err(Rejection::ResolutionConflict(e())),
    );
    assert_eq!(
        turn.tool(e()).unwrap().resolution(),
        Some(Resolution::Finished { code: 0 }),
    );
}

#[test]
fn a_resolution_after_the_turn_settled_is_refused() {
    let mut turn = settled(TurnOutcome::Cancelled);

    assert_eq!(
        turn.apply(TurnFact::ToolResolved {
            execution: e(),
            resolution: Resolution::Finished { code: 0 },
        }),
        Err(Rejection::TurnOver),
    );
    assert_eq!(turn.tool(e()).unwrap().resolution(), None);
}

#[test]
fn a_redelivered_tool_request_must_name_the_same_deadline() {
    let mut turn = requested();

    assert_eq!(
        turn.apply(TurnFact::ToolRequested {
            execution: e(),
            deadline: Deadline::Bounded,
        }),
        Ok(Ack::Duplicate),
    );
    assert_eq!(
        turn.apply(TurnFact::ToolRequested {
            execution: e(),
            deadline: Deadline::Unbounded,
        }),
        Err(Rejection::DeadlineConflict(e())),
    );
}

#[test]
fn replay_stops_at_the_first_refusal_naming_the_offset() {
    let facts = [
        opening(),
        TurnFact::ModelRequested {
            attempt: Attempt::new(1),
        },
        TurnFact::ToolRequested {
            execution: e(),
            deadline: Deadline::Bounded,
        },
        TurnFact::ModelAnswered,
        TurnFact::ToolRequested {
            execution: e2(),
            deadline: Deadline::Bounded,
        },
    ];

    let rejected = Turn::replay(&facts).unwrap_err();

    assert_eq!(rejected.offset(), 2);
    assert_eq!(rejected.rejection(), Rejection::ModelNotAnswered);
    assert_eq!(
        rejected.to_string(),
        "fact 2 of the log: the turn requested a tool before the model answered",
    );
}

#[test]
fn replay_reconstructs_the_driven_state() {
    let facts = [
        opening(),
        TurnFact::ModelRequested {
            attempt: Attempt::new(1),
        },
        TurnFact::ModelAnswered,
        TurnFact::ToolRequested {
            execution: e(),
            deadline: Deadline::Bounded,
        },
        TurnFact::ToolResolved {
            execution: e(),
            resolution: Resolution::Unknown,
        },
        TurnFact::TurnSettled {
            outcome: TurnOutcome::Completed,
        },
    ];

    let mut driven = Turn::new();
    for fact in facts {
        assert_eq!(driven.apply(fact), Ok(Ack::Applied));
    }

    assert_eq!(driven.number(), Some(TurnNumber::new(1)));
    assert_eq!(driven.outcome(), Some(TurnOutcome::Completed));
    assert_eq!(Turn::replay(&facts), Ok(driven));
}

#[test]
fn display_of_model_not_answered_is_literal() {
    assert_eq!(
        Rejection::ModelNotAnswered.to_string(),
        "the turn requested a tool before the model answered",
    );
}

#[test]
fn display_of_unrequested_answer_is_literal() {
    assert_eq!(
        Rejection::UnrequestedAnswer.to_string(),
        "the turn has no model request for an answer",
    );
}

#[test]
fn display_of_turn_over_is_literal() {
    assert_eq!(Rejection::TurnOver.to_string(), "the turn is settled");
}

#[test]
fn display_of_outcome_conflict_is_literal() {
    assert_eq!(
        Rejection::OutcomeConflict.to_string(),
        "the turn already settled with a different outcome",
    );
}

#[test]
fn the_turn_vocabulary_serde_as_the_contract_says() {
    assert_eq!(
        serde_json::to_string(&opening()).unwrap(),
        r#"{"fact":"turn_opened","number":1}"#,
    );
    assert_eq!(
        serde_json::to_string(&TurnFact::ModelAnswered).unwrap(),
        r#"{"fact":"model_answered"}"#,
    );
    assert_eq!(
        serde_json::to_string(&TurnFact::ToolRequested {
            execution: e(),
            deadline: Deadline::Unbounded,
        })
        .unwrap(),
        r#"{"fact":"tool_requested","execution":"00000000-0000-0000-0000-000000000001","deadline":"unbounded"}"#,
    );
    assert_eq!(
        serde_json::to_string(&TurnOutcome::Cancelled).unwrap(),
        r#""cancelled""#,
    );

    let fact = TurnFact::ToolResolved {
        execution: e(),
        resolution: Resolution::Unknown,
    };
    let encoded = serde_json::to_string(&fact).unwrap();

    assert_eq!(serde_json::from_str::<TurnFact>(&encoded).unwrap(), fact);
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Verdict {
    Apply,
    Duplicate,
    Refuse(Rejection),
}

/// A second, independent derivation of the turn rules from the applied history.
///
/// The verdict is derived from the facts the history holds rather than by calling [`Turn::apply`],
/// so a rule that drifts in either implementation shows up as a disagreement. Only applied facts
/// are recorded, so the derived sets are the machine's sets.
#[derive(Clone, Debug)]
struct Model {
    history: Vec<TurnFact>,
}

impl Model {
    const fn new() -> Self {
        Self {
            history: Vec::new(),
        }
    }

    fn number(&self) -> Option<TurnNumber> {
        self.history.iter().find_map(|fact| match fact {
            TurnFact::TurnOpened { number } => Some(*number),
            _ => None,
        })
    }

    fn attempts(&self) -> BTreeSet<Attempt> {
        self.history
            .iter()
            .filter_map(|fact| match fact {
                TurnFact::ModelRequested { attempt } => Some(*attempt),
                _ => None,
            })
            .collect()
    }

    fn answered(&self) -> bool {
        self.history
            .iter()
            .any(|fact| matches!(fact, TurnFact::ModelAnswered))
    }

    fn outcome(&self) -> Option<TurnOutcome> {
        self.history.iter().find_map(|fact| match fact {
            TurnFact::TurnSettled { outcome } => Some(*outcome),
            _ => None,
        })
    }

    fn tools(&self) -> BTreeMap<ExecutionID, (Deadline, Option<Resolution>)> {
        let mut tools = BTreeMap::new();
        for fact in &self.history {
            match fact {
                TurnFact::ToolRequested {
                    execution,
                    deadline,
                } => {
                    tools.entry(*execution).or_insert((*deadline, None));
                },
                TurnFact::ToolResolved {
                    execution,
                    resolution,
                } => {
                    if let Some((_, recorded)) = tools.get_mut(execution)
                        && recorded.is_none()
                    {
                        *recorded = Some(*resolution);
                    }
                },
                _ => {},
            }
        }
        tools
    }

    fn verdict(
        &self,
        fact: TurnFact,
    ) -> Verdict {
        if self.number().is_none() {
            return if matches!(fact, TurnFact::TurnOpened { .. }) {
                Verdict::Apply
            } else {
                Verdict::Refuse(Rejection::NotOpened)
            };
        }

        match fact {
            TurnFact::TurnOpened { .. } => Verdict::Duplicate,
            TurnFact::ModelRequested { attempt } => {
                if self.attempts().contains(&attempt) {
                    Verdict::Duplicate
                } else if self.outcome().is_some() {
                    Verdict::Refuse(Rejection::TurnOver)
                } else {
                    Verdict::Apply
                }
            },
            TurnFact::ModelAnswered => {
                if self.answered() {
                    Verdict::Duplicate
                } else if self.outcome().is_some() {
                    Verdict::Refuse(Rejection::TurnOver)
                } else if self.attempts().is_empty() {
                    Verdict::Refuse(Rejection::UnrequestedAnswer)
                } else {
                    Verdict::Apply
                }
            },
            TurnFact::ToolRequested {
                execution,
                deadline,
            } => match self.tools().get(&execution) {
                Some((recorded, _)) if *recorded == deadline => Verdict::Duplicate,
                Some(_) => Verdict::Refuse(Rejection::DeadlineConflict(execution)),
                None => {
                    if self.outcome().is_some() {
                        Verdict::Refuse(Rejection::TurnOver)
                    } else if self.answered() {
                        Verdict::Apply
                    } else {
                        Verdict::Refuse(Rejection::ModelNotAnswered)
                    }
                },
            },
            TurnFact::ToolResolved {
                execution,
                resolution,
            } => match self.tools().get(&execution) {
                None => Verdict::Refuse(Rejection::UnknownExecution(execution)),
                Some((_, Some(current))) => {
                    if *current == resolution {
                        Verdict::Duplicate
                    } else {
                        Verdict::Refuse(Rejection::ResolutionConflict(execution))
                    }
                },
                Some((_, None)) => {
                    if self.outcome().is_some() {
                        Verdict::Refuse(Rejection::TurnOver)
                    } else {
                        Verdict::Apply
                    }
                },
            },
            TurnFact::TurnSettled { outcome } => match self.outcome() {
                Some(current) if current == outcome => Verdict::Duplicate,
                Some(_) => Verdict::Refuse(Rejection::OutcomeConflict),
                None => Verdict::Apply,
            },
        }
    }
}

/// The tool table the model derives, in the shape the machine reports it.
fn snapshot(turn: &Turn) -> BTreeMap<ExecutionID, (Deadline, Option<Resolution>)> {
    turn.tools()
        .map(|(execution, tool)| (execution, (tool.deadline(), tool.resolution())))
        .collect()
}

fn arbitrary_resolution() -> impl Strategy<Value = Resolution> {
    prop_oneof![
        Just(Resolution::Finished { code: 0 }),
        Just(Resolution::Killed { signal: 9 }),
        Just(Resolution::TimedOut),
        Just(Resolution::Cancelled),
        Just(Resolution::Refused),
        Just(Resolution::Unknown),
    ]
}

fn arbitrary_turn_fact(ids: [ExecutionID; 4]) -> impl Strategy<Value = TurnFact> {
    let tool = (0..4_usize).prop_map(move |index| ids[index]);
    let deadline = prop_oneof![Just(Deadline::Bounded), Just(Deadline::Unbounded)];

    prop_oneof![
        2 => (0..2_u64).prop_map(|number| TurnFact::TurnOpened { number: TurnNumber::new(number) }),
        2 => (0..3_u32).prop_map(|attempt| TurnFact::ModelRequested { attempt: Attempt::new(attempt) }),
        3 => Just(TurnFact::ModelAnswered),
        4 => (tool.clone(), deadline).prop_map(|(execution, deadline)| {
            TurnFact::ToolRequested { execution, deadline }
        }),
        8 => (tool, arbitrary_resolution()).prop_map(|(execution, resolution)| {
            TurnFact::ToolResolved { execution, resolution }
        }),
        2 => prop_oneof![
            Just(TurnOutcome::Completed),
            Just(TurnOutcome::Cancelled),
            Just(TurnOutcome::Failed),
        ]
        .prop_map(|outcome| TurnFact::TurnSettled { outcome }),
    ]
}

fn arbitrary_turn_history() -> impl Strategy<Value = Vec<TurnFact>> {
    proptest::collection::vec(
        arbitrary_turn_fact([execution(1), execution(2), execution(3), execution(4)]),
        1..=12,
    )
    .prop_map(|mut facts| {
        facts[0] = opening();
        facts
    })
}

/// Folds `facts`, keeping the facts the machine applied rather than repeated.
fn fold(facts: Vec<TurnFact>) -> (Turn, Vec<TurnFact>) {
    let mut turn = Turn::new();
    let mut accepted = Vec::new();
    for fact in facts {
        if turn.apply(fact) == Ok(Ack::Applied) {
            accepted.push(fact);
        }
    }
    (turn, accepted)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn the_turn_agrees_with_the_reference_fold(facts in arbitrary_turn_history()) {
        let start = facts[0];
        let mut turn = Turn::new();
        let mut model = Model::new();

        for fact in facts {
            let expected = model.verdict(fact);
            let actual = turn.apply(fact);

            match expected {
                Verdict::Apply => prop_assert_eq!(actual, Ok(Ack::Applied)),
                Verdict::Duplicate => prop_assert_eq!(actual, Ok(Ack::Duplicate)),
                Verdict::Refuse(rejection) => prop_assert_eq!(actual, Err(rejection)),
            }

            if actual == Ok(Ack::Applied) {
                model.history.push(fact);
            }

            prop_assert_eq!(turn.number(), model.number());
            prop_assert_eq!(turn.outcome(), model.outcome());
            prop_assert_eq!(turn.answered(), model.answered());
            prop_assert_eq!(
                turn.attempts().collect::<Vec<_>>(),
                model.attempts().into_iter().collect::<Vec<_>>(),
            );
            prop_assert_eq!(snapshot(&turn), model.tools());
        }

        prop_assert_eq!(turn.number(), Some(TurnNumber::new(1)));
        prop_assert_eq!(model.history.first(), Some(&start));
    }

    #[test]
    fn replay_of_the_applied_turn_facts_equals_the_driven_state(facts in arbitrary_turn_history()) {
        let (turn, accepted) = fold(facts);

        prop_assert_eq!(accepted.first(), Some(&opening()));
        prop_assert_eq!(Turn::replay(&accepted).unwrap(), turn);
    }

    #[test]
    fn reapplying_the_applied_turn_facts_is_all_duplicates_and_a_fixpoint(facts in arbitrary_turn_history()) {
        let (mut turn, accepted) = fold(facts);
        prop_assert_eq!(accepted.first(), Some(&opening()));
        let driven = turn.clone();

        for fact in &accepted {
            prop_assert_eq!(turn.apply(*fact), Ok(Ack::Duplicate));
            prop_assert_eq!(&turn, &driven);
        }
    }

    #[test]
    fn a_prefix_then_the_rest_equals_the_whole_turn(facts in arbitrary_turn_history()) {
        let (turn, accepted) = fold(facts);
        prop_assert_eq!(accepted.first(), Some(&opening()));

        for split in 0..=accepted.len() {
            let mut rebuilt = Turn::replay(&accepted[..split]).unwrap();
            for fact in &accepted[split..] {
                rebuilt.apply(*fact).unwrap();
            }
            prop_assert_eq!(&rebuilt, &turn);
        }
    }
}

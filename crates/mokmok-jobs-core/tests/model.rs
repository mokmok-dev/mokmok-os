//! Exhaustive model check of the job state machine with Stateright.
//!
//! The model explores every command the aggregate accepts, so the invariants here
//! are checked over all reachable states within the bounded attempt policy.

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use mokmok_jobs_core::{
        CloudEvent, JobCommand, JobEvent, JobId, JobState, Version, decide, evolve,
    };
    use stateright::{Checker, Model, Property};

    /// The longest history `max_attempts = 3` allows: submit, then a start and a
    /// retry for each attempt but the last, then a terminal event.
    ///
    /// The log is part of the state, so without this cap a state machine that
    /// fails to advance grows the log forever and BFS never terminates.
    const MAX_LOG: usize = 7;

    #[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
    enum Action {
        Submit(u32),
        Start,
        Succeed,
        Fail { retryable: bool },
        Cancel,
    }

    #[derive(Clone, Debug, Eq, Hash, PartialEq)]
    struct ModelState {
        state: JobState,
        log: Vec<JobEvent>,
    }

    struct JobModel {
        id: JobId,
    }

    impl JobModel {
        fn new() -> Self {
            Self {
                id: JobId::from_u128(1),
            }
        }

        fn command(action: Action) -> JobCommand {
            match action {
                Action::Submit(max_attempts) => JobCommand::Submit {
                    input: b"input".to_vec(),
                    max_attempts: NonZeroU32::new(max_attempts).expect("actions stay above zero"),
                },
                Action::Start => JobCommand::Start,
                Action::Succeed => JobCommand::Succeed {
                    output: b"output".to_vec(),
                },
                Action::Fail { retryable } => JobCommand::Fail {
                    reason: "failed".to_owned(),
                    retryable,
                },
                Action::Cancel => JobCommand::Cancel {
                    reason: "cancelled".to_owned(),
                },
            }
        }
    }

    impl Model for JobModel {
        type State = ModelState;
        type Action = Action;

        fn init_states(&self) -> Vec<Self::State> {
            vec![ModelState {
                state: JobState::New,
                log: Vec::new(),
            }]
        }

        fn actions(
            &self,
            _state: &Self::State,
            actions: &mut Vec<Self::Action>,
        ) {
            for max_attempts in 1..=3 {
                actions.push(Action::Submit(max_attempts));
            }
            actions.push(Action::Start);
            actions.push(Action::Succeed);
            actions.push(Action::Fail { retryable: true });
            actions.push(Action::Fail { retryable: false });
            actions.push(Action::Cancel);
        }

        fn next_state(
            &self,
            last: &Self::State,
            action: Action,
        ) -> Option<Self::State> {
            let events = decide(&last.state, Self::command(action)).ok()?;
            let mut state = last.state.clone();
            let mut log = last.log.clone();
            for event in events {
                evolve(&mut state, &event).expect("decide produced a legal event");
                log.push(event);
            }
            if log.len() > MAX_LOG {
                return None;
            }
            Some(ModelState { state, log })
        }

        fn properties(&self) -> Vec<Property<Self>> {
            vec![
                Property::always(
                    "attempts stay within the policy",
                    |_: &Self, state: &ModelState| match &state.state {
                        JobState::Queued {
                            attempt,
                            max_attempts,
                            ..
                        }
                        | JobState::Running {
                            attempt,
                            max_attempts,
                            ..
                        } => *attempt <= max_attempts.get(),
                        _ => true,
                    },
                ),
                Property::always(
                    "replay rebuilds the same state",
                    |model: &Self, state: &ModelState| {
                        let records: Vec<CloudEvent<JobEvent>> = state
                            .log
                            .iter()
                            .enumerate()
                            .map(|(index, event)| {
                                CloudEvent::new(
                                    model.id.as_str(),
                                    Version::new(
                                        u64::try_from(index).expect("log index fits in u64") + 1,
                                    ),
                                    event.clone(),
                                )
                            })
                            .collect();
                        JobState::replay(&records).is_ok_and(|replayed| replayed == state.state)
                    },
                ),
                Property::always(
                    "terminal states have no successor",
                    |model: &Self, state: &ModelState| {
                        let terminal = matches!(
                            state.state,
                            JobState::Succeeded { .. }
                                | JobState::Failed { .. }
                                | JobState::Cancelled { .. }
                        );
                        !terminal || model.next_states(state).is_empty()
                    },
                ),
                Property::sometimes("a retry is reachable", |_: &Self, state: &ModelState| {
                    matches!(state.log.last(), Some(JobEvent::RetryScheduled { .. }))
                }),
            ]
        }
    }

    #[test]
    fn every_reachable_state_keeps_the_job_invariants() {
        let checker = JobModel::new().checker().spawn_bfs().join();
        checker.assert_properties();
        assert!(
            checker.unique_state_count() > 1,
            "the model explored no transitions"
        );
    }
}

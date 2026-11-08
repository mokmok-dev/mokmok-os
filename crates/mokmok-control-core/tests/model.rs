//! Exhaustive model check of the task state machine with Stateright.
//!
//! The model explores every command the aggregate accepts, so the invariants here are
//! checked over all reachable states within the bounded log.

#[cfg(test)]
mod tests {
    use mokmok_control_core::{
        AgentUpdate, CloudEvent, ControlCommand, ControlEvent, MAX_UPDATES, TaskId, TaskSpec,
        TaskState, TaskStatus, Version, decide, evolve,
    };
    use stateright::{Checker, Model, Property};

    /// The longest history the model explores.
    ///
    /// The log is part of the state, so without this cap a state machine that fails to
    /// advance grows the log forever and BFS never terminates.
    const MAX_LOG: usize = 8;

    #[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
    enum Action {
        Submit,
        Start,
        OpenSession,
        Message,
        Thought,
        ToolCall,
        Complete,
        Fail,
        Cancel,
    }

    #[derive(Clone, Debug, Eq, Hash, PartialEq)]
    struct ModelState {
        state: TaskState,
        log: Vec<ControlEvent>,
    }

    struct TaskModel {
        spec: TaskSpec,
    }

    impl TaskModel {
        fn new() -> Self {
            Self {
                spec: TaskSpec {
                    task_id: TaskId::from_u128(1),
                    title: "Fix the build".to_owned(),
                    instruction: "Make the tests pass.".to_owned(),
                    agent: "scripted".to_owned(),
                    host: None,
                    workspace: Some("/work/mokmok".to_owned()),
                },
            }
        }

        fn command(
            &self,
            action: Action,
        ) -> ControlCommand {
            match action {
                Action::Submit => ControlCommand::Submit(self.spec.clone()),
                Action::Start => ControlCommand::Start,
                Action::OpenSession => ControlCommand::OpenSession {
                    session_id: "session-1".to_owned(),
                },
                Action::Message => ControlCommand::Update {
                    update: AgentUpdate::Message {
                        text: "looking".to_owned(),
                    },
                },
                Action::Thought => ControlCommand::Update {
                    update: AgentUpdate::Thought {
                        text: "thinking".to_owned(),
                    },
                },
                Action::ToolCall => ControlCommand::Update {
                    update: AgentUpdate::ToolCall {
                        title: "read".to_owned(),
                        status: "ok".to_owned(),
                    },
                },
                Action::Complete => ControlCommand::Complete {
                    summary: "done".to_owned(),
                },
                Action::Fail => ControlCommand::Fail {
                    reason: "broken".to_owned(),
                },
                Action::Cancel => ControlCommand::Cancel {
                    reason: "stop".to_owned(),
                },
            }
        }

        fn started(state: &ModelState) -> bool {
            state
                .log
                .iter()
                .any(|event| matches!(event, ControlEvent::Started))
        }
    }

    impl Model for TaskModel {
        type State = ModelState;
        type Action = Action;

        fn init_states(&self) -> Vec<Self::State> {
            vec![ModelState {
                state: TaskState::default(),
                log: Vec::new(),
            }]
        }

        fn actions(
            &self,
            _state: &Self::State,
            actions: &mut Vec<Self::Action>,
        ) {
            for action in [
                Action::Submit,
                Action::Start,
                Action::OpenSession,
                Action::Message,
                Action::Thought,
                Action::ToolCall,
                Action::Complete,
                Action::Fail,
                Action::Cancel,
            ] {
                actions.push(action);
            }
        }

        fn next_state(
            &self,
            last: &Self::State,
            action: Action,
        ) -> Option<Self::State> {
            let events = decide(&last.state, self.command(action)).ok()?;
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
                    "no task runs without a start",
                    |_: &Self, state: &ModelState| {
                        Self::started(state) || !matches!(state.state.status, TaskStatus::Running)
                    },
                ),
                Property::always(
                    "nothing is recorded before a start",
                    |_: &Self, state: &ModelState| {
                        Self::started(state)
                            || (state.state.session.is_none() && state.state.updates.is_empty())
                    },
                ),
                Property::always("at most one session id", |_: &Self, state: &ModelState| {
                    let opened = state
                        .log
                        .iter()
                        .filter(|event| matches!(event, ControlEvent::SessionOpened { .. }))
                        .count();
                    opened <= 1 && state.state.session.is_some() == (opened == 1)
                }),
                // `MAX_LOG` is 8, far below `MAX_UPDATES` (256), so the eviction branch is
                // unreachable here; the unit test `evolve_caps_the_updates_and_counts_every_one`
                // covers it. This property checks the count and the (vacuous) cap.
                Property::always(
                    "update_count matches the recorded updates and the state keeps at most MAX_UPDATES",
                    |_: &Self, state: &ModelState| {
                        let recorded = state
                            .log
                            .iter()
                            .filter(|event| matches!(event, ControlEvent::UpdateRecorded { .. }))
                            .count();
                        state.state.updates.len() <= MAX_UPDATES
                            && state.state.update_count
                                == u64::try_from(recorded).expect("log length fits in u64")
                            && state.state.updates.len() == recorded.min(MAX_UPDATES)
                    },
                ),
                Property::always(
                    "replay rebuilds the same state",
                    |model: &Self, state: &ModelState| {
                        let records: Vec<CloudEvent<ControlEvent>> = state
                            .log
                            .iter()
                            .enumerate()
                            .map(|(index, event)| {
                                let position = u64::try_from(index).expect("log index fits in u64");
                                CloudEvent::new(
                                    model.spec.task_id.as_str(),
                                    Version::new(position + 1),
                                    event.clone(),
                                )
                            })
                            .collect();
                        TaskState::replay(&records).is_ok_and(|replayed| replayed == state.state)
                    },
                ),
                Property::always(
                    "terminal states have no successor",
                    |model: &Self, state: &ModelState| {
                        let terminal = matches!(
                            state.state.status,
                            TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Cancelled
                        );
                        !terminal || model.next_states(state).is_empty()
                    },
                ),
                Property::sometimes(
                    "a completed task is reachable",
                    |_: &Self, state: &ModelState| {
                        matches!(state.state.status, TaskStatus::Completed)
                    },
                ),
            ]
        }
    }

    #[test]
    fn every_reachable_state_keeps_the_task_invariants() {
        let checker = TaskModel::new().checker().spawn_bfs().join();
        checker.assert_properties();
        assert!(
            checker.unique_state_count() > 1,
            "the model explored no transitions"
        );
    }
}

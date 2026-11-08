//! Property tests over the task domain and the prompt composer.

#[cfg(test)]
mod tests {
    use mokmok_control_core::{
        AgentUpdate, CloudEvent, ControlCommand, ControlError, ControlEvent, MAX_PROMPT_BYTES,
        MAX_UPDATES, PromptInput, TaskId, TaskSpec, TaskState, TaskStatus, Version, compose_prompt,
        decide, evolve,
    };
    use proptest::prelude::*;

    fn text() -> impl Strategy<Value = String> {
        prop::collection::vec(any::<char>(), 0..24).prop_map(String::from_iter)
    }

    fn spec() -> impl Strategy<Value = TaskSpec> {
        (
            "[ -~]{0,40}",
            "[ -~]{0,200}",
            "[a-z0-9][a-z0-9-]{0,31}",
            prop::option::of("[a-z0-9][a-z0-9.-]{0,63}"),
            prop::option::of("/[ -~]{0,40}"),
        )
            .prop_map(|(title, instruction, agent, host, workspace)| TaskSpec {
                task_id: TaskId::from_u128(1),
                title,
                instruction,
                agent,
                host,
                workspace,
            })
    }

    fn update() -> impl Strategy<Value = AgentUpdate> {
        prop_oneof![
            text().prop_map(|text| AgentUpdate::Message { text }),
            text().prop_map(|text| AgentUpdate::Thought { text }),
            (text(), text()).prop_map(|(title, status)| AgentUpdate::ToolCall { title, status }),
        ]
    }

    fn command() -> impl Strategy<Value = ControlCommand> {
        prop_oneof![
            3 => spec().prop_map(ControlCommand::Submit),
            3 => Just(ControlCommand::Start),
            2 => text().prop_map(|session_id| ControlCommand::OpenSession { session_id }),
            4 => update().prop_map(|update| ControlCommand::Update { update }),
            2 => text().prop_map(|summary| ControlCommand::Complete { summary }),
            2 => text().prop_map(|reason| ControlCommand::Fail { reason }),
            2 => text().prop_map(|reason| ControlCommand::Cancel { reason }),
        ]
    }

    fn every_command() -> Vec<ControlCommand> {
        vec![
            ControlCommand::Start,
            ControlCommand::OpenSession {
                session_id: "session-1".to_owned(),
            },
            ControlCommand::Update {
                update: AgentUpdate::Message {
                    text: "looking".to_owned(),
                },
            },
            ControlCommand::Complete {
                summary: "done".to_owned(),
            },
            ControlCommand::Fail {
                reason: "broken".to_owned(),
            },
            ControlCommand::Cancel {
                reason: "stop".to_owned(),
            },
        ]
    }

    proptest! {
        #[test]
        fn every_accepted_decision_evolves_and_replays(
            commands in prop::collection::vec(command(), 0..32),
        ) {
            let subject = TaskId::from_u128(1);
            let mut state = TaskState::default();
            let mut log: Vec<CloudEvent<ControlEvent>> = Vec::new();
            for command in commands {
                let Ok(events) = decide(&state, command) else {
                    continue;
                };
                for event in events {
                    let position = u64::try_from(log.len()).expect("log length fits in u64");
                    let record =
                        CloudEvent::new(subject.as_str(), Version::new(position + 1), event.clone());
                    evolve(&mut state, &event).expect("decide produced a legal event");
                    log.push(record);
                    prop_assert!(state.updates.len() <= MAX_UPDATES);
                    if matches!(
                        state.status,
                        TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Cancelled
                    ) {
                        for command in every_command() {
                            prop_assert_eq!(decide(&state, command), Err(ControlError::Terminal));
                        }
                    }
                }
            }
            let replayed = TaskState::replay(&log).expect("decide built a well formed log");
            prop_assert_eq!(&replayed, &state);
        }

        #[test]
        fn validate_never_panics_on_arbitrary_bytes(
            bytes in prop::collection::vec(any::<u8>(), 0..96),
            field in 0usize..5,
        ) {
            let value = String::from_utf8_lossy(&bytes).into_owned();
            let mut spec = TaskSpec {
                task_id: TaskId::from_u128(1),
                title: "t".to_owned(),
                instruction: "i".to_owned(),
                agent: "a".to_owned(),
                host: None,
                workspace: None,
            };
            match field {
                0 => spec.title = value,
                1 => spec.instruction = value,
                2 => spec.agent = value,
                3 => spec.host = Some(value),
                _ => spec.workspace = Some(value),
            }
            if let Err(error) = spec.validate() {
                let rejected_as_invalid = matches!(error, ControlError::InvalidSpec { .. });
                prop_assert!(rejected_as_invalid, "unexpected error: {}", error);
            }
        }

        #[test]
        fn task_id_parsing_round_trips_every_accepted_id(raw in text()) {
            if let Ok(id) = TaskId::parse(raw) {
                let canonical = id.as_str().to_owned();
                prop_assert_eq!(TaskId::parse(canonical), Ok(id));
            }
        }

        #[test]
        fn compose_prompt_is_bounded_and_deterministic(
            title in text(),
            instruction in text(),
            workspace in prop::option::of(text()),
            max_bytes in 0usize..MAX_PROMPT_BYTES + 1,
        ) {
            let input = PromptInput {
                title: &title,
                instruction: &instruction,
                workspace: workspace.as_deref(),
            };
            let first = compose_prompt(&input, max_bytes);
            let second = compose_prompt(&input, max_bytes);
            prop_assert!(first.len() <= max_bytes);
            prop_assert_eq!(first, second);
        }

        #[test]
        fn a_generous_budget_keeps_the_title_and_the_instruction(
            title in "[ -~]{1,40}",
            instruction in "[ -~]{0,2000}",
            workspace in prop::option::of("[ -~]{1,40}"),
        ) {
            let input = PromptInput {
                title: &title,
                instruction: &instruction,
                workspace: workspace.as_deref(),
            };
            let prompt = compose_prompt(&input, MAX_PROMPT_BYTES);
            prop_assert!(prompt.contains(&title));
            prop_assert!(prompt.contains(&instruction));
            prop_assert!(prompt.len() <= MAX_PROMPT_BYTES);
        }
    }
}

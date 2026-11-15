#[cfg(test)]
mod tests {
    use mokmok_control_core::{
        ControlCommand, TaskId, TaskSpec, TaskState, TaskStatus, decide, evolve,
    };

    const DOC: &str = include_str!("../../../ARCHITECTURE.md");

    const STATUSES: [TaskStatus; 6] = [
        TaskStatus::New,
        TaskStatus::Submitted,
        TaskStatus::Running,
        TaskStatus::Completed,
        TaskStatus::Failed,
        TaskStatus::Cancelled,
    ];

    fn spec() -> TaskSpec {
        TaskSpec {
            task_id: TaskId::from_u128(1),
            title: "Fix the build".to_owned(),
            instruction: "Make the tests pass.".to_owned(),
            agent: "scripted".to_owned(),
            host: None,
            workspace: Some("/work/mokmok".to_owned()),
        }
    }

    fn commands() -> [ControlCommand; 6] {
        [
            ControlCommand::Submit(spec()),
            ControlCommand::Start,
            ControlCommand::OpenSession {
                session_id: "session-1".to_owned(),
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

    const fn command_name(command: &ControlCommand) -> &'static str {
        match command {
            ControlCommand::Submit(_) => "Submit",
            ControlCommand::Start => "Start",
            ControlCommand::OpenSession { .. } => "OpenSession",
            ControlCommand::Complete { .. } => "Complete",
            ControlCommand::Fail { .. } => "Fail",
            ControlCommand::Cancel { .. } => "Cancel",
        }
    }

    const fn status_name(status: TaskStatus) -> &'static str {
        match status {
            TaskStatus::New => "New",
            TaskStatus::Submitted => "Submitted",
            TaskStatus::Running => "Running",
            TaskStatus::Completed => "Completed",
            TaskStatus::Failed => "Failed",
            TaskStatus::Cancelled => "Cancelled",
        }
    }

    fn walk(
        state: &TaskState,
        command: ControlCommand,
        status: TaskStatus,
    ) -> TaskState {
        let events = decide(state, command).expect("the walk command is legal here");
        let mut next = state.clone();
        for event in &events {
            evolve(&mut next, event).expect("a decided event applies to the state");
        }
        assert_eq!(
            next.status,
            status,
            "the walk should reach {}",
            status_name(status)
        );
        next
    }

    fn states() -> Vec<TaskState> {
        let new = TaskState::default();
        let submitted = walk(&new, ControlCommand::Submit(spec()), TaskStatus::Submitted);
        let running = walk(&submitted, ControlCommand::Start, TaskStatus::Running);
        let completed = walk(
            &running,
            ControlCommand::Complete {
                summary: "done".to_owned(),
            },
            TaskStatus::Completed,
        );
        let failed = walk(
            &running,
            ControlCommand::Fail {
                reason: "broken".to_owned(),
            },
            TaskStatus::Failed,
        );
        let cancelled = walk(
            &submitted,
            ControlCommand::Cancel {
                reason: "stop".to_owned(),
            },
            TaskStatus::Cancelled,
        );
        vec![new, submitted, running, completed, failed, cancelled]
    }

    fn machine_edges() -> Vec<(String, String, String)> {
        let mut edges = Vec::new();
        for state in states() {
            for command in commands() {
                let name = command_name(&command);
                let Ok(events) = decide(&state, command) else {
                    continue;
                };
                let mut next = state.clone();
                for event in &events {
                    evolve(&mut next, event).expect("a decided event applies to the state");
                }
                edges.push((
                    status_name(state.status).to_owned(),
                    status_name(next.status).to_owned(),
                    name.to_owned(),
                ));
            }
        }
        edges.sort();
        edges.dedup();
        edges
    }

    fn task_diagram() -> Vec<&'static str> {
        let mut lines = DOC.lines().skip_while(|line| line.trim() != "```mermaid");
        assert_eq!(
            lines.next().map(str::trim),
            Some("```mermaid"),
            "the doc has no mermaid block"
        );
        lines.take_while(|line| line.trim() != "```").collect()
    }

    fn diagram_edges() -> (Vec<(String, String, String)>, Vec<String>) {
        let mut edges = Vec::new();
        let mut initial = Vec::new();
        let mut headings = 0_usize;
        for raw in task_diagram() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with("direction ") {
                continue;
            }
            if line == "stateDiagram-v2" {
                headings += 1;
                continue;
            }
            let Some((from, rest)) = line.split_once(" --> ") else {
                panic!("the diagram line is not an edge: {line}");
            };
            let (to, label) = match rest.split_once(" : ") {
                Some((to, label)) => (to.trim(), Some(label.trim())),
                None => (rest.trim(), None),
            };
            assert_ne!(
                to, "[*]",
                "the diagram must not draw into a terminal marker"
            );
            if from == "[*]" {
                initial.push(to.to_owned());
                continue;
            }
            let Some(label) = label else {
                panic!("the edge {from} --> {to} names no command");
            };
            let command = label.split(',').next().unwrap_or(label).trim();
            edges.push((from.to_owned(), to.to_owned(), command.to_owned()));
        }
        assert_eq!(headings, 1, "the diagram holds one stateDiagram-v2");
        edges.sort();
        (edges, initial)
    }

    #[test]
    fn the_diagram_matches_every_command_the_machine_accepts() {
        let (diagram, initial) = diagram_edges();
        assert_eq!(
            initial,
            vec!["New".to_owned()],
            "the diagram has one initial edge, and it enters New"
        );
        assert_eq!(
            diagram,
            machine_edges(),
            "the diagram and the machine disagree; the left side is the doc"
        );
    }

    #[test]
    fn every_status_appears_in_the_diagram() {
        let (diagram, initial) = diagram_edges();
        for status in STATUSES {
            let name = status_name(status);
            let drawn = initial.iter().any(|target| target == name)
                || diagram
                    .iter()
                    .any(|(from, to, _)| from == name || to == name);
            assert!(drawn, "{name} appears in no edge of the diagram");
        }
    }

    #[test]
    fn every_diagram_label_names_a_real_command() {
        let (diagram, _) = diagram_edges();
        let known: Vec<&str> = commands().iter().map(command_name).collect();
        for (from, to, command) in &diagram {
            assert!(
                known.contains(&command.as_str()),
                "{from} --> {to} names {command}, which no command produces"
            );
        }
    }
}

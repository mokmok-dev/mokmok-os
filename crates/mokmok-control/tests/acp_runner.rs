//! The ACP runner against the scripted agent binary.

mod common;

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::Duration;

    use mokmok_control::{
        AcpRunner, AgentConfig, AgentTable, AgentTransport, ControlEvent, ControlSystem,
        EventStore, Journal, JournalEvent, NewTask, NoopPublisher, Reporter, RunOutcome, Runner,
        Supervisor, TaskId, TaskSpec, TaskStatus,
    };
    use mokmok_jobs_memory::MemoryEventStore;
    use mokmok_journal::MemoryJournal;

    use crate::common;

    const AGENT: &str = "fake";
    const SESSION: &str = "fake-session";
    const TIMEOUT: Duration = Duration::from_secs(30);

    fn program() -> PathBuf {
        PathBuf::from(env!("CARGO_BIN_EXE_mokmok-control-test-agent"))
    }

    fn table(args: Vec<String>) -> AgentTable {
        AgentTable {
            agents: [(
                AGENT.to_owned(),
                AgentConfig {
                    transport: AgentTransport::Stdio {
                        program: program(),
                        args,
                    },
                },
            )]
            .into_iter()
            .collect(),
            hosts: BTreeMap::new(),
            default: Some(AGENT.to_owned()),
        }
    }

    fn new_task(
        workspace: &Path,
        agent: &str,
    ) -> NewTask {
        NewTask {
            title: "Say hello".to_owned(),
            instruction: "Greet the user.".to_owned(),
            agent: agent.to_owned(),
            host: None,
            workspace: Some(workspace.display().to_string()),
        }
    }

    type Store = Arc<MemoryEventStore<TaskId, ControlEvent>>;
    type JournalHandle = Arc<MemoryJournal>;

    fn spec(workspace: &Path) -> TaskSpec {
        TaskSpec {
            task_id: TaskId::from_u128(1),
            title: "Say hello".to_owned(),
            instruction: "Greet the user.".to_owned(),
            agent: AGENT.to_owned(),
            host: None,
            workspace: Some(workspace.display().to_string()),
        }
    }

    fn run_task(
        args: Vec<String>,
        task: NewTask,
    ) -> Result<(Store, ControlSystem, JournalHandle, TaskId), Box<dyn std::error::Error>> {
        run_task_with_timeout(args, task, TIMEOUT)
    }

    fn run_task_with_timeout(
        args: Vec<String>,
        task: NewTask,
        timeout: Duration,
    ) -> Result<(Store, ControlSystem, JournalHandle, TaskId), Box<dyn std::error::Error>> {
        let store: Store = Arc::new(MemoryEventStore::new());
        let system = ControlSystem::without_idle_timeout(store.clone(), Arc::new(NoopPublisher));
        let runner = Arc::new(AcpRunner::new(table(args), timeout));
        let journal: JournalHandle = Arc::new(MemoryJournal::new());
        let supervisor = Supervisor::new(system.clone(), runner, journal.clone(), 1);
        let id = supervisor.submit(task)?;
        assert_eq!(supervisor.run_pending()?, 1);
        Ok((store, system, journal, id))
    }

    fn events(
        store: &MemoryEventStore<TaskId, ControlEvent>,
        id: &TaskId,
    ) -> Result<Vec<ControlEvent>, Box<dyn std::error::Error>> {
        Ok(store
            .load(id)?
            .into_iter()
            .map(|record| record.data)
            .collect())
    }

    fn record_path(workspace: &Path) -> PathBuf {
        workspace.join("agent.txt")
    }

    fn recorded(path: &Path) -> String {
        fs::read_to_string(path).unwrap_or_default()
    }

    fn record_arg(path: &Path) -> String {
        format!("--record={}", path.display())
    }

    fn message(text: &str) -> JournalEvent {
        JournalEvent::Message {
            text: text.to_owned(),
        }
    }

    fn permission(
        title: &str,
        status: &str,
    ) -> JournalEvent {
        JournalEvent::Permission {
            title: title.to_owned(),
            status: status.to_owned(),
        }
    }

    fn journal_events(
        journal: &MemoryJournal,
        id: &TaskId,
    ) -> Vec<JournalEvent> {
        journal
            .tail(id.as_str(), 10_000)
            .expect("the journal reads")
            .entries
            .into_iter()
            .map(|entry| entry.event)
            .collect()
    }

    #[test]
    fn a_delegated_task_records_the_transcript() -> Result<(), Box<dyn std::error::Error>> {
        let workspace = tempfile::tempdir()?;
        let record = record_path(workspace.path());
        let (store, system, journal, id) = run_task(
            vec!["--tool-kind=read".to_owned(), record_arg(&record)],
            new_task(workspace.path(), AGENT),
        )?;

        let text = recorded(&record);
        let state = system.state(&id)?;
        assert_eq!(state.status, TaskStatus::Completed);
        assert_eq!(state.session.as_deref(), Some(SESSION));
        assert_eq!(state.updates, [] as [mokmok_control::AgentUpdate; 0]);
        assert_eq!(state.update_count, 0);
        assert_eq!(state.outcome.as_deref(), Some("working"));
        assert_eq!(
            journal_events(&journal, &id),
            vec![message("working"), permission("tool call", "allowed"),]
        );

        let spec = state.spec.expect("the task carries its spec");
        assert_eq!(
            events(&store, &id)?,
            vec![
                ControlEvent::Submitted { spec },
                ControlEvent::Started,
                ControlEvent::SessionOpened {
                    session_id: SESSION.to_owned()
                },
                ControlEvent::Completed {
                    summary: "working".to_owned()
                },
            ]
        );

        assert!(text.contains("session fake-session"), "{text}");
        assert!(text.contains("permission allow"), "{text}");
        assert!(
            text.contains("Say hello"),
            "the composed prompt must carry the title: {text}"
        );
        Ok(())
    }

    #[test]
    fn a_failing_agent_fails_the_task() -> Result<(), Box<dyn std::error::Error>> {
        let workspace = tempfile::tempdir()?;
        let (store, system, _, id) =
            run_task(vec!["--fail".to_owned()], new_task(workspace.path(), AGENT))?;

        let state = system.state(&id)?;
        assert_eq!(state.status, TaskStatus::Failed);
        let outcome = state.outcome.unwrap_or_default();
        assert!(
            outcome.contains("the agent failed on purpose"),
            "the agent's error must reach the outcome: {outcome}"
        );
        assert!(matches!(
            events(&store, &id)?.last(),
            Some(ControlEvent::Failed { .. })
        ));
        Ok(())
    }

    #[test]
    fn a_silent_agent_times_out() -> Result<(), Box<dyn std::error::Error>> {
        let workspace = tempfile::tempdir()?;
        let (store, system, _, id) = run_task_with_timeout(
            vec!["--silent".to_owned()],
            new_task(workspace.path(), AGENT),
            Duration::from_millis(500),
        )?;

        let state = system.state(&id)?;
        assert_eq!(state.status, TaskStatus::Failed);
        let outcome = state.outcome.unwrap_or_default();
        assert!(
            outcome.contains("did not answer in time"),
            "a quiet agent must be reported as a timeout: {outcome}"
        );
        assert!(matches!(
            events(&store, &id)?.last(),
            Some(ControlEvent::Failed { .. })
        ));
        Ok(())
    }

    #[test]
    fn a_read_tool_call_is_allowed() -> Result<(), Box<dyn std::error::Error>> {
        let workspace = tempfile::tempdir()?;
        let record = record_path(workspace.path());
        let (_, system, _, id) = run_task(
            vec!["--tool-kind=read".to_owned(), record_arg(&record)],
            new_task(workspace.path(), AGENT),
        )?;

        assert_eq!(system.state(&id)?.status, TaskStatus::Completed);
        assert!(
            recorded(&record).contains("permission allow"),
            "a read tool call must be allowed: {}",
            recorded(&record)
        );
        Ok(())
    }

    #[test]
    fn a_write_tool_call_is_refused() -> Result<(), Box<dyn std::error::Error>> {
        let workspace = tempfile::tempdir()?;
        let record = record_path(workspace.path());
        let (_, system, _, id) = run_task(
            vec!["--tool-kind=edit".to_owned(), record_arg(&record)],
            new_task(workspace.path(), AGENT),
        )?;

        assert_eq!(system.state(&id)?.status, TaskStatus::Completed);
        assert!(
            recorded(&record).contains("permission cancelled"),
            "an edit must not be approved automatically: {}",
            recorded(&record)
        );
        Ok(())
    }

    #[test]
    fn a_tool_call_without_a_kind_is_refused() -> Result<(), Box<dyn std::error::Error>> {
        let workspace = tempfile::tempdir()?;
        let record = record_path(workspace.path());
        let (_, system, _, id) =
            run_task(vec![record_arg(&record)], new_task(workspace.path(), AGENT))?;

        assert_eq!(system.state(&id)?.status, TaskStatus::Completed);
        assert!(
            recorded(&record).contains("permission cancelled"),
            "an absent tool kind must not be approved automatically: {}",
            recorded(&record)
        );
        Ok(())
    }

    #[test]
    fn an_unknown_agent_fails_without_starting_one() -> Result<(), Box<dyn std::error::Error>> {
        let workspace = tempfile::tempdir()?;
        let record = record_path(workspace.path());
        let (_, system, _, id) = run_task(
            vec![record_arg(&record)],
            new_task(workspace.path(), "missing"),
        )?;

        let state = system.state(&id)?;
        assert_eq!(state.status, TaskStatus::Failed);
        let outcome = state.outcome.unwrap_or_default();
        assert!(
            outcome.contains("missing"),
            "the outcome must name the unknown agent: {outcome}"
        );
        assert!(!record.exists(), "no agent was started");
        Ok(())
    }

    #[test]
    fn a_cancelled_task_never_starts_the_agent() {
        struct Cancelled;

        impl Reporter for Cancelled {
            fn started(
                &self,
                _session: &str,
            ) -> Result<(), String> {
                Ok(())
            }

            fn update(
                &self,
                _event: JournalEvent,
            ) -> Result<(), String> {
                Ok(())
            }

            fn cancelled(&self) -> bool {
                true
            }
        }

        let workspace = tempfile::tempdir().expect("the temp dir exists");
        let record = record_path(workspace.path());
        let runner = AcpRunner::new(table(vec![record_arg(&record)]), TIMEOUT);
        assert_eq!(
            runner.run(&spec(workspace.path()), &Cancelled),
            RunOutcome::Cancelled
        );
        assert!(!record.exists(), "no agent was started");
    }

    #[test]
    fn a_refused_session_is_never_prompted() {
        struct RefusesSession;

        impl Reporter for RefusesSession {
            fn started(
                &self,
                _session: &str,
            ) -> Result<(), String> {
                Err("the task is already terminal".to_owned())
            }

            fn update(
                &self,
                _event: JournalEvent,
            ) -> Result<(), String> {
                Ok(())
            }

            fn cancelled(&self) -> bool {
                false
            }
        }

        let workspace = tempfile::tempdir().expect("the temp dir exists");
        let record = record_path(workspace.path());
        let runner = AcpRunner::new(table(vec![record_arg(&record)]), TIMEOUT);

        assert_eq!(
            runner.run(&spec(workspace.path()), &RefusesSession),
            RunOutcome::Failed {
                reason: "the task is already terminal".to_owned(),
            }
        );
        assert!(
            !recorded(&record)
                .lines()
                .any(|line| line.starts_with("prompt ")),
            "no prompt reached the agent: {}",
            recorded(&record)
        );
    }

    #[test]
    fn an_allow_always_only_option_is_accepted() -> Result<(), Box<dyn std::error::Error>> {
        let workspace = tempfile::tempdir()?;
        let record = record_path(workspace.path());
        let (_, system, _, id) = run_task(
            vec![
                "--tool-kind=read".to_owned(),
                "--allow-always-only".to_owned(),
                record_arg(&record),
            ],
            new_task(workspace.path(), AGENT),
        )?;

        assert_eq!(system.state(&id)?.status, TaskStatus::Completed);
        assert!(
            recorded(&record).contains("permission allow"),
            "an allow_always option must be selected when it is the only one: {}",
            recorded(&record)
        );
        Ok(())
    }

    #[test]
    fn a_fetch_tool_call_is_refused() -> Result<(), Box<dyn std::error::Error>> {
        let workspace = tempfile::tempdir()?;
        let record = record_path(workspace.path());
        let (_, system, _, id) = run_task(
            vec!["--tool-kind=fetch".to_owned(), record_arg(&record)],
            new_task(workspace.path(), AGENT),
        )?;

        assert_eq!(system.state(&id)?.status, TaskStatus::Completed);
        assert!(
            recorded(&record).contains("permission cancelled"),
            "network egress must not be approved automatically: {}",
            recorded(&record)
        );
        Ok(())
    }

    #[test]
    fn a_rich_agent_records_thoughts_and_tool_calls() -> Result<(), Box<dyn std::error::Error>> {
        let workspace = tempfile::tempdir()?;
        let (_, system, journal, id) = run_task(
            vec!["--rich".to_owned(), "--tool-kind=read".to_owned()],
            new_task(workspace.path(), AGENT),
        )?;

        let state = system.state(&id)?;
        assert_eq!(state.status, TaskStatus::Completed);
        assert_eq!(
            journal_events(&journal, &id),
            vec![
                message("working"),
                JournalEvent::Thought {
                    text: "considering".to_owned()
                },
                JournalEvent::ToolCall {
                    title: "tool pending".to_owned(),
                    status: "pending".to_owned()
                },
                JournalEvent::ToolCall {
                    title: "tool in_progress".to_owned(),
                    status: "in_progress".to_owned()
                },
                JournalEvent::ToolCall {
                    title: "tool completed".to_owned(),
                    status: "completed".to_owned()
                },
                JournalEvent::ToolCall {
                    title: "tool failed".to_owned(),
                    status: "failed".to_owned()
                },
                JournalEvent::ToolCall {
                    title: "updated title".to_owned(),
                    status: "completed".to_owned()
                },
                JournalEvent::ToolCall {
                    title: "tool call".to_owned(),
                    status: "failed".to_owned()
                },
                JournalEvent::ToolCall {
                    title: "title only".to_owned(),
                    status: "pending".to_owned()
                },
                permission("tool call", "allowed"),
            ]
        );
        Ok(())
    }

    fn message_text(events: &[JournalEvent]) -> String {
        events
            .iter()
            .filter_map(|event| match event {
                JournalEvent::Message { text } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_streamed_turn_coalesces_its_chunks() -> Result<(), Box<dyn std::error::Error>> {
        let workspace = tempfile::tempdir()?;
        let (store, system, journal, id) = run_task(
            vec!["--stream".to_owned()],
            new_task(workspace.path(), AGENT),
        )?;

        let recorded = journal_events(&journal, &id).len();
        assert!(
            recorded < 20,
            "200 tiny chunks must coalesce, but {recorded} entries were recorded"
        );

        let records = events(&store, &id)?;
        assert_eq!(records.len(), 4, "the domain log stays bounded");
        assert!(matches!(&records[0], ControlEvent::Submitted { .. }));
        assert!(matches!(&records[1], ControlEvent::Started));
        assert!(matches!(&records[2], ControlEvent::SessionOpened { .. }));
        assert!(matches!(&records[3], ControlEvent::Completed { .. }));

        let state = system.state(&id)?;
        assert_eq!(state.status, TaskStatus::Completed);
        assert_eq!(
            message_text(&journal_events(&journal, &id)),
            common::STREAM_SENTENCE
        );
        assert_eq!(state.outcome.as_deref(), Some(common::STREAM_SENTENCE));
        Ok(())
    }

    #[test]
    fn a_streamed_thought_and_tool_call_keep_their_order() -> Result<(), Box<dyn std::error::Error>>
    {
        let workspace = tempfile::tempdir()?;
        let (_, system, journal, id) = run_task(
            vec!["--stream-thoughts".to_owned()],
            new_task(workspace.path(), AGENT),
        )?;

        let recorded = journal_events(&journal, &id).len();
        assert!(
            recorded < 20,
            "streamed thoughts and tool calls must coalesce, but {recorded} were recorded"
        );

        let state = system.state(&id)?;
        assert_eq!(
            journal_events(&journal, &id),
            vec![
                message(common::STREAM_SENTENCE),
                JournalEvent::Thought {
                    text: common::STREAM_THOUGHT.to_owned()
                },
                JournalEvent::ToolCall {
                    title: "stream tool".to_owned(),
                    status: "pending".to_owned()
                },
                permission("tool call", "refused"),
            ]
        );
        // The turn ended after the thought, so the last completed run is the thought.
        assert_eq!(state.outcome.as_deref(), Some(common::STREAM_THOUGHT));
        Ok(())
    }

    struct NullReporter;

    impl Reporter for NullReporter {
        fn started(
            &self,
            _session: &str,
        ) -> Result<(), String> {
            Ok(())
        }

        fn update(
            &self,
            _event: JournalEvent,
        ) -> Result<(), String> {
            Ok(())
        }

        fn cancelled(&self) -> bool {
            false
        }
    }

    fn spec_in(workspace: Option<&str>) -> TaskSpec {
        TaskSpec {
            task_id: TaskId::from_u128(2),
            title: "Say hello".to_owned(),
            instruction: "Greet the user.".to_owned(),
            agent: AGENT.to_owned(),
            host: None,
            workspace: workspace.map(str::to_owned),
        }
    }

    fn failure(
        runner: &AcpRunner,
        spec: &TaskSpec,
    ) -> String {
        match runner.run(spec, &NullReporter) {
            RunOutcome::Failed { reason } => reason,
            other => panic!("expected a failure, got {other:?}"),
        }
    }

    #[test]
    fn a_task_without_a_workspace_fails() {
        let runner = AcpRunner::new(table(Vec::new()), TIMEOUT);
        let reason = failure(&runner, &spec_in(None));
        assert!(reason.contains("no workspace"), "{reason}");
    }

    #[test]
    fn a_relative_workspace_fails() {
        let runner = AcpRunner::new(table(Vec::new()), TIMEOUT);
        let reason = failure(&runner, &spec_in(Some("relative/path")));
        assert!(reason.contains("absolute"), "{reason}");
    }

    #[test]
    fn a_missing_workspace_fails() {
        let directory = tempfile::tempdir().expect("a temp dir");
        let missing = directory.path().join("missing");
        let runner = AcpRunner::new(table(Vec::new()), TIMEOUT);
        let reason = failure(&runner, &spec_in(Some(&missing.display().to_string())));
        assert!(reason.contains("does not exist"), "{reason}");
    }

    #[test]
    fn a_workspace_outside_the_root_fails() {
        let root = tempfile::tempdir().expect("a root");
        let outside = tempfile::tempdir().expect("another tree");
        let runner = AcpRunner::new(table(Vec::new()), TIMEOUT).with_workspace_root(root.path());
        let reason = failure(
            &runner,
            &spec_in(Some(&outside.path().display().to_string())),
        );
        assert!(reason.contains("outside"), "{reason}");
    }

    #[test]
    fn a_workspace_inside_the_root_runs() -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let workspace = root.path().join("project");
        fs::create_dir(&workspace)?;
        let record = record_path(&workspace);
        let store: Store = Arc::new(MemoryEventStore::new());
        let system = ControlSystem::without_idle_timeout(store, Arc::new(NoopPublisher));
        let runner = Arc::new(
            AcpRunner::new(table(vec![record_arg(&record)]), TIMEOUT)
                .with_workspace_root(root.path()),
        );
        let supervisor = Supervisor::new(system.clone(), runner, Arc::new(MemoryJournal::new()), 1);
        let id = supervisor.submit(new_task(&workspace, AGENT))?;
        assert_eq!(supervisor.run_pending()?, 1);
        assert_eq!(system.state(&id)?.status, TaskStatus::Completed);
        assert!(recorded(&record).contains("session fake-session"));
        Ok(())
    }

    #[test]
    fn a_host_selects_its_transport_override() -> Result<(), Box<dyn std::error::Error>> {
        let workspace = tempfile::tempdir()?;
        let base = workspace.path().join("base.txt");
        let override_record = workspace.path().join("override.txt");
        let mut agents_table = table(vec![record_arg(&base)]);
        agents_table.hosts.insert(
            "remote".to_owned(),
            AgentTransport::Stdio {
                program: program(),
                args: vec![record_arg(&override_record)],
            },
        );
        let runner = Arc::new(AcpRunner::new(agents_table, TIMEOUT));
        let mut spec = spec(workspace.path());
        spec.host = Some("remote".to_owned());
        assert!(matches!(
            runner.run(&spec, &NullReporter),
            RunOutcome::Completed { .. }
        ));
        assert!(recorded(&override_record).contains("session fake-session"));
        assert!(!base.exists(), "the agent's own transport was not used");
        Ok(())
    }

    #[test]
    fn an_unknown_host_fails() -> Result<(), Box<dyn std::error::Error>> {
        let workspace = tempfile::tempdir()?;
        let runner = AcpRunner::new(table(Vec::new()), TIMEOUT);
        let mut spec = spec(workspace.path());
        spec.host = Some("missing".to_owned());
        let reason = failure(&runner, &spec);
        assert!(reason.contains("missing"), "{reason}");
        Ok(())
    }

    struct CancelAfter {
        cancelled: Arc<std::sync::atomic::AtomicBool>,
    }

    impl Reporter for CancelAfter {
        fn started(
            &self,
            _session: &str,
        ) -> Result<(), String> {
            Ok(())
        }

        fn update(
            &self,
            _event: JournalEvent,
        ) -> Result<(), String> {
            Ok(())
        }

        fn cancelled(&self) -> bool {
            self.cancelled.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    #[test]
    fn a_silent_agent_is_cancelled_by_the_watcher() -> Result<(), Box<dyn std::error::Error>> {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::time::Instant;

        let workspace = tempfile::tempdir()?;
        let cancelled = Arc::new(AtomicBool::new(false));
        let signal = Arc::clone(&cancelled);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            signal.store(true, Ordering::SeqCst);
        });
        let runner = AcpRunner::new(table(vec!["--silent".to_owned()]), TIMEOUT);
        let started = Instant::now();
        let outcome = runner.run(
            &spec(workspace.path()),
            &CancelAfter {
                cancelled: Arc::clone(&cancelled),
            },
        );
        assert_eq!(outcome, RunOutcome::Cancelled);
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "the silent agent was not cancelled in time: {:?}",
            started.elapsed()
        );
        Ok(())
    }
}

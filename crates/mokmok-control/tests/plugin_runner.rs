//! The plugin runner against real TypeScript plugins.

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::thread;
    use std::time::{Duration, Instant};

    use mokmok_control::{
        AgentConfig, AgentTable, AgentTransport, ControlEvent, ControlSystem, Journal,
        JournalEvent, NewTask, NoopPublisher, PluginHost, PluginRunner, Supervisor, TaskId,
        TaskStatus,
    };
    use mokmok_jobs_memory::MemoryEventStore;
    use mokmok_journal::MemoryJournal;

    const TIMEOUT: Duration = Duration::from_secs(30);

    fn node() -> PathBuf {
        // The dev shell puts node on PATH, and the daemon resolves it the same way.
        PathBuf::from("node")
    }

    fn fixture_dir() -> PathBuf {
        // The fixtures live with the SDK package, which is where a plugin author would
        // write one, so the runner tests drive the same files the host tests do.
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages/plugin-sdk/test/fixtures")
    }

    fn entry(
        dir: &Path,
        fixture: &str,
    ) -> AgentTransport {
        AgentTransport::Plugin {
            entry: dir.join(format!("{fixture}.ts")),
            timeout_secs: None,
        }
    }

    fn table(transport: AgentTransport) -> AgentTable {
        AgentTable {
            agents: [("plugin".to_owned(), AgentConfig { transport })]
                .into_iter()
                .collect(),
            hosts: BTreeMap::new(),
            default: Some("plugin".to_owned()),
        }
    }

    fn new_task(workspace: &Path) -> NewTask {
        NewTask {
            title: "Say hello".to_owned(),
            instruction: "Greet the user.".to_owned(),
            agent: "plugin".to_owned(),
            host: None,
            workspace: Some(workspace.display().to_string()),
        }
    }

    fn system() -> ControlSystem {
        ControlSystem::without_idle_timeout(
            Arc::new(MemoryEventStore::<TaskId, ControlEvent>::new()),
            Arc::new(NoopPublisher),
        )
    }

    struct Ran {
        system: ControlSystem,
        journal: Arc<MemoryJournal>,
        id: TaskId,
    }

    fn run_with(
        transport: AgentTransport,
        host: PluginHost,
    ) -> Result<Ran, Box<dyn std::error::Error>> {
        let workspace = tempfile::tempdir()?;
        let system = system();
        let runner = PluginRunner::new(table(transport), host);
        let journal: Arc<MemoryJournal> = Arc::new(MemoryJournal::new());
        let supervisor = Supervisor::new(system.clone(), Arc::new(runner), journal.clone(), 1);
        let id = supervisor.submit(new_task(workspace.path()))?;
        assert_eq!(supervisor.run_pending()?, 1);
        Ok(Ran {
            system,
            journal,
            id,
        })
    }

    fn run(
        fixture: &str,
        timeout: Duration,
    ) -> Result<Ran, Box<dyn std::error::Error>> {
        run_with(
            entry(&fixture_dir(), fixture),
            PluginHost::new(node()).with_timeout(timeout),
        )
    }

    fn wait_for(
        system: &ControlSystem,
        id: &TaskId,
        status: TaskStatus,
    ) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !system.state(id).is_ok_and(|state| state.status == status) {
            assert!(
                Instant::now() < deadline,
                "the task never became {status:?}"
            );
            thread::sleep(Duration::from_millis(5));
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

    fn message(text: &str) -> JournalEvent {
        JournalEvent::Message {
            text: text.to_owned(),
        }
    }

    fn outcome(
        system: &ControlSystem,
        id: &TaskId,
    ) -> String {
        system
            .state(id)
            .expect("the task reads")
            .outcome
            .expect("the task carries an outcome")
    }

    #[test]
    fn a_plugin_run_records_its_transcript_and_completes() -> Result<(), Box<dyn std::error::Error>>
    {
        let ran = run("completes", TIMEOUT)?;
        let state = ran.system.state(&ran.id)?;

        assert_eq!(state.status, TaskStatus::Completed);
        // The plugin echoes the task id the daemon handed it, which is the id the
        // supervisor generated rather than a fixture constant.
        let expected = format!("finished {}", ran.id.as_str());
        assert_eq!(state.outcome.as_deref(), Some(expected.as_str()));
        assert!(
            state.session.is_some(),
            "the host's session id is recorded once"
        );
        assert_eq!(
            journal_events(&ran.journal, &ran.id),
            vec![
                message("running Say hello"),
                JournalEvent::Thought {
                    text: "the workspace looks fine".to_owned(),
                },
                JournalEvent::ToolCall {
                    title: "read the workspace".to_owned(),
                    status: "completed".to_owned(),
                },
                JournalEvent::Permission {
                    title: "write the report".to_owned(),
                    status: "allowed".to_owned(),
                },
            ]
        );
        Ok(())
    }

    #[test]
    fn every_journal_event_a_plugin_emits_reaches_the_journal()
    -> Result<(), Box<dyn std::error::Error>> {
        // The UI renders exactly these four kinds, so a plugin that could only emit a
        // subset would be half-visible.
        let ran = run("completes", TIMEOUT)?;
        let kinds: Vec<&'static str> = journal_events(&ran.journal, &ran.id)
            .iter()
            .map(|event| match event {
                JournalEvent::Message { .. } => "message",
                JournalEvent::Thought { .. } => "thought",
                JournalEvent::ToolCall { .. } => "tool_call",
                JournalEvent::Permission { .. } => "permission",
            })
            .collect();
        assert_eq!(kinds, vec!["message", "thought", "tool_call", "permission"]);
        Ok(())
    }

    #[test]
    fn a_thrown_error_fails_the_task_with_its_stack() -> Result<(), Box<dyn std::error::Error>> {
        let ran = run("throws", TIMEOUT)?;
        let state = ran.system.state(&ran.id)?;

        assert_eq!(state.status, TaskStatus::Failed);
        let reason = state.outcome.expect("a failed task carries its reason");
        assert!(reason.contains("the plugin exploded"), "{reason}");
        assert_eq!(
            journal_events(&ran.journal, &ran.id),
            vec![message("about to fail")]
        );
        Ok(())
    }

    #[test]
    fn an_entry_with_no_default_export_fails_the_task() -> Result<(), Box<dyn std::error::Error>> {
        let ran = run("no_default", TIMEOUT)?;
        assert_eq!(ran.system.state(&ran.id)?.status, TaskStatus::Failed);
        assert!(
            outcome(&ran.system, &ran.id).contains("has no default export function"),
            "{}",
            outcome(&ran.system, &ran.id)
        );
        Ok(())
    }

    #[test]
    fn a_missing_entry_fails_before_the_host_starts() -> Result<(), Box<dyn std::error::Error>> {
        let ran = run_with(
            entry(&fixture_dir(), "does_not_exist"),
            PluginHost::new(node()),
        )?;

        assert_eq!(ran.system.state(&ran.id)?.status, TaskStatus::Failed);
        assert!(outcome(&ran.system, &ran.id).contains("the plugin entry does not exist"));
        assert_eq!(journal_events(&ran.journal, &ran.id), []);
        Ok(())
    }

    #[test]
    fn a_node_that_cannot_start_fails_the_task() -> Result<(), Box<dyn std::error::Error>> {
        let ran = run_with(
            entry(&fixture_dir(), "completes"),
            PluginHost::new("/nonexistent/node"),
        )?;

        assert_eq!(ran.system.state(&ran.id)?.status, TaskStatus::Failed);
        assert!(outcome(&ran.system, &ran.id).contains("the plugin host could not be started"));
        Ok(())
    }

    #[test]
    fn a_missing_workspace_fails_before_the_host_starts() -> Result<(), Box<dyn std::error::Error>>
    {
        let system = system();
        let runner = PluginRunner::new(
            table(entry(&fixture_dir(), "completes")),
            PluginHost::new(node()),
        );
        let supervisor = Supervisor::new(
            system.clone(),
            Arc::new(runner),
            Arc::new(MemoryJournal::new()),
            1,
        );
        let id = supervisor.submit(NewTask {
            workspace: Some("/definitely/not/a/directory".to_owned()),
            ..new_task(Path::new("/work"))
        })?;
        assert_eq!(supervisor.run_pending()?, 1);

        assert_eq!(system.state(&id)?.status, TaskStatus::Failed);
        assert!(outcome(&system, &id).contains("the task workspace does not exist"));
        Ok(())
    }

    #[test]
    fn a_plugin_that_never_settles_fails_at_its_timeout() -> Result<(), Box<dyn std::error::Error>>
    {
        let started = Instant::now();
        let ran = run("hangs", Duration::from_secs(1))?;

        assert_eq!(ran.system.state(&ran.id)?.status, TaskStatus::Failed);
        assert!(
            outcome(&ran.system, &ran.id).contains("the plugin exceeded its 1 second timeout"),
            "{}",
            outcome(&ran.system, &ran.id)
        );
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "the run must be bounded, took {:?}",
            started.elapsed()
        );
        Ok(())
    }

    #[test]
    fn a_plugin_cancelled_by_a_human_ends_cancelled() -> Result<(), Box<dyn std::error::Error>> {
        let workspace = tempfile::tempdir()?;
        let system = system();
        let runner = PluginRunner::new(
            table(entry(&fixture_dir(), "cancelled")),
            PluginHost::new(node()).with_timeout(TIMEOUT),
        );
        let journal: Arc<MemoryJournal> = Arc::new(MemoryJournal::new());
        let supervisor = Supervisor::new(system.clone(), Arc::new(runner), journal.clone(), 1);
        let id = supervisor.submit(new_task(workspace.path()))?;
        supervisor.start();
        wait_for(&system, &id, TaskStatus::Running);
        // The plugin subscribes to the signal before this returns, so the cancel reaches
        // it rather than arriving before it started waiting.
        thread::sleep(Duration::from_millis(300));
        supervisor.cancel(&id, "the human stopped it")?;
        wait_for(&system, &id, TaskStatus::Cancelled);
        supervisor.shutdown();

        assert_eq!(
            journal_events(&journal, &id),
            vec![message("waiting to be stopped")]
        );
        Ok(())
    }

    #[test]
    fn a_plugin_that_ignores_the_cancel_still_ends_bounded()
    -> Result<(), Box<dyn std::error::Error>> {
        // The host settles a cancel itself, so this covers the plugin that never returns:
        // the host's own cancel wins and the run ends without waiting out the timeout.
        let started = Instant::now();
        let workspace = tempfile::tempdir()?;
        let system = system();
        let runner = PluginRunner::new(
            table(entry(&fixture_dir(), "ignores_cancel")),
            // A timeout far past the cancel, so only the cancel can end this run.
            PluginHost::new(node()).with_timeout(Duration::from_secs(300)),
        );
        let supervisor = Supervisor::new(
            system.clone(),
            Arc::new(runner),
            Arc::new(MemoryJournal::new()),
            1,
        );
        let id = supervisor.submit(new_task(workspace.path()))?;
        supervisor.start();
        wait_for(&system, &id, TaskStatus::Running);
        thread::sleep(Duration::from_millis(300));
        supervisor.cancel(&id, "the human stopped it")?;
        wait_for(&system, &id, TaskStatus::Cancelled);
        supervisor.shutdown();

        assert!(
            started.elapsed() < Duration::from_secs(60),
            "a cancel must not wait out the 300 second timeout, took {:?}",
            started.elapsed()
        );
        Ok(())
    }

    #[test]
    fn a_plugin_runs_in_its_workspace() -> Result<(), Box<dyn std::error::Error>> {
        // The plugin runs with the workspace as its cwd, so a relative write lands there.
        let workspace = tempfile::tempdir()?;
        let written = workspace.path().join("writer.ts");
        fs::write(
            &written,
            "import { writeFileSync } from 'node:fs';\n\
             export default async function () {\n\
               writeFileSync('written.txt', 'hello from the plugin');\n\
               return { kind: 'completed', summary: 'wrote a file' };\n\
             }\n",
        )?;
        let system = system();
        let runner = PluginRunner::new(
            table(AgentTransport::Plugin {
                entry: written,
                timeout_secs: None,
            }),
            PluginHost::new(node()).with_timeout(TIMEOUT),
        );
        let supervisor = Supervisor::new(
            system.clone(),
            Arc::new(runner),
            Arc::new(MemoryJournal::new()),
            1,
        );
        let id = supervisor.submit(new_task(workspace.path()))?;
        assert_eq!(supervisor.run_pending()?, 1);

        assert_eq!(system.state(&id)?.status, TaskStatus::Completed);
        assert_eq!(
            fs::read_to_string(workspace.path().join("written.txt"))?,
            "hello from the plugin"
        );
        Ok(())
    }
}

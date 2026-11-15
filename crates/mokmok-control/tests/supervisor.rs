//! The supervisor: queueing, the start fence, cancellation, and settling.

#[cfg(test)]
mod tests {
    use std::fs;
    use std::fs::OpenOptions;
    use std::io::Write;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    use mokmok_control::{
        CloudEvent, ControlCommand, ControlError, ControlEvent, ControlSystem, EnumerableStore,
        Error, EventStore, FailingRunner, INTERRUPTED_REASON, Journal, JournalEvent, NewTask,
        NoopPublisher, Recovery, Reporter, RunOutcome, Runner, ScriptedRunner, StoreError,
        Supervisor, TaskId, TaskSpec, TaskStatus, Version,
    };
    use mokmok_jobs_jsonl::JsonlEventStore;
    use mokmok_jobs_memory::MemoryEventStore;
    use mokmok_journal::MemoryJournal;

    fn message(text: &str) -> JournalEvent {
        JournalEvent::Message {
            text: text.to_owned(),
        }
    }

    fn empty_journal() -> Arc<MemoryJournal> {
        Arc::new(MemoryJournal::new())
    }

    fn new_task(agent: &str) -> NewTask {
        NewTask {
            title: "Fix the build".to_owned(),
            instruction: "Make the tests pass.".to_owned(),
            agent: agent.to_owned(),
            host: None,
            workspace: Some("/work/mokmok".to_owned()),
            request_id: None,
        }
    }

    fn system() -> ControlSystem {
        ControlSystem::without_idle_timeout(
            Arc::new(MemoryEventStore::<TaskId, ControlEvent>::new()),
            Arc::new(NoopPublisher),
        )
    }

    fn wait_for(
        system: &ControlSystem,
        id: &TaskId,
        status: TaskStatus,
    ) {
        // A busy host stretches a run well past 10 s; the bound stays under cargo-mutants'
        // 60 s, so a real hang still fails here.
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if system.state(id).is_ok_and(|state| state.status == status) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "the task never reached {status:?}"
            );
            thread::sleep(Duration::from_millis(2));
        }
    }

    struct PollingRunner {
        starts: Arc<AtomicUsize>,
        observed: Arc<AtomicBool>,
    }

    impl Runner for PollingRunner {
        fn run(
            &self,
            _spec: &TaskSpec,
            reporter: &dyn Reporter,
        ) -> RunOutcome {
            self.starts.fetch_add(1, Ordering::SeqCst);
            let deadline = Instant::now() + Duration::from_secs(10);
            while !reporter.cancelled() {
                if Instant::now() >= deadline {
                    return RunOutcome::Failed {
                        reason: "the cancel never arrived".to_owned(),
                    };
                }
                thread::sleep(Duration::from_millis(2));
            }
            self.observed.store(true, Ordering::SeqCst);
            RunOutcome::Cancelled
        }
    }

    struct SlowRunner {
        starts: Arc<AtomicUsize>,
        hold: Duration,
    }

    impl Runner for SlowRunner {
        fn run(
            &self,
            spec: &TaskSpec,
            reporter: &dyn Reporter,
        ) -> RunOutcome {
            self.starts.fetch_add(1, Ordering::SeqCst);
            thread::sleep(self.hold);
            let _ = reporter.started("slow-session");
            RunOutcome::Completed {
                summary: format!("ran {}", spec.title),
            }
        }
    }

    #[test]
    fn a_scripted_run_completes_the_task() -> Result<(), Box<dyn std::error::Error>> {
        let system = system();
        let journal = empty_journal();
        let runner = Arc::new(ScriptedRunner::new(
            vec![message("one"), message("two")],
            "all done",
        ));
        let supervisor = Supervisor::new(system.clone(), runner, journal.clone(), 1);

        let id = supervisor.submit(new_task("scripted"))?.id;
        assert_eq!(supervisor.run_pending()?, 1);

        let state = system.state(&id)?;
        assert_eq!(state.status, TaskStatus::Completed);
        assert_eq!(state.session.as_deref(), Some("scripted-session"));
        assert_eq!(state.updates, [] as [mokmok_control::AgentUpdate; 0]);
        assert_eq!(state.update_count, 0);
        assert_eq!(state.outcome.as_deref(), Some("all done"));
        assert_eq!(
            journal
                .tail(id.as_str(), 200)?
                .entries
                .into_iter()
                .map(|entry| entry.event)
                .collect::<Vec<_>>(),
            vec![message("one"), message("two")]
        );
        assert_eq!(supervisor.run_pending()?, 0);
        Ok(())
    }

    #[test]
    fn a_failing_run_fails_the_task() -> Result<(), Box<dyn std::error::Error>> {
        let system = system();
        let supervisor = Supervisor::new(
            system.clone(),
            Arc::new(FailingRunner::new("boom")),
            empty_journal(),
            1,
        );

        let id = supervisor.submit(new_task("scripted"))?.id;
        assert_eq!(supervisor.run_pending()?, 1);

        let state = system.state(&id)?;
        assert_eq!(state.status, TaskStatus::Failed);
        assert_eq!(state.outcome.as_deref(), Some("boom"));
        Ok(())
    }

    #[test]
    fn a_cancel_before_the_run_stops_the_task() -> Result<(), Box<dyn std::error::Error>> {
        let system = system();
        let starts = Arc::new(AtomicUsize::new(0));
        let runner = Arc::new(SlowRunner {
            starts: Arc::clone(&starts),
            hold: Duration::from_millis(5),
        });
        let supervisor = Supervisor::new(system.clone(), runner, empty_journal(), 1);

        let id = supervisor.submit(new_task("scripted"))?.id;
        supervisor.cancel(&id, "stop")?;
        assert_eq!(supervisor.run_pending()?, 0);

        let state = system.state(&id)?;
        assert_eq!(state.status, TaskStatus::Cancelled);
        assert_eq!(state.outcome.as_deref(), Some("stop"));
        assert_eq!(starts.load(Ordering::SeqCst), 0, "nothing ran");
        Ok(())
    }

    #[test]
    fn a_cancel_during_the_run_reaches_the_runner() -> Result<(), Box<dyn std::error::Error>> {
        let system = system();
        let starts = Arc::new(AtomicUsize::new(0));
        let observed = Arc::new(AtomicBool::new(false));
        let runner = Arc::new(PollingRunner {
            starts: Arc::clone(&starts),
            observed: Arc::clone(&observed),
        });
        let supervisor = Supervisor::new(system.clone(), runner, empty_journal(), 1);

        let id = supervisor.submit(new_task("scripted"))?.id;
        supervisor.start();
        wait_for(&system, &id, TaskStatus::Running);
        supervisor.cancel(&id, "stop")?;
        wait_for(&system, &id, TaskStatus::Cancelled);

        let deadline = Instant::now() + Duration::from_secs(10);
        while !observed.load(Ordering::SeqCst) {
            assert!(Instant::now() < deadline, "the runner never saw the cancel");
            thread::sleep(Duration::from_millis(2));
        }
        // Join only after the runner is known to have returned, so a stuck worker can
        // fail the observation loop instead of blocking the join.
        supervisor.shutdown();
        assert!(observed.load(Ordering::SeqCst), "the runner saw the cancel");
        assert_eq!(starts.load(Ordering::SeqCst), 1);
        let state = system.state(&id)?;
        assert_eq!(state.status, TaskStatus::Cancelled);
        assert_eq!(state.outcome.as_deref(), Some("stop"));
        Ok(())
    }

    #[test]
    fn shutdown_reaches_a_running_runner_as_a_cancel() -> Result<(), Box<dyn std::error::Error>> {
        let system = system();
        let starts = Arc::new(AtomicUsize::new(0));
        let observed = Arc::new(AtomicBool::new(false));
        let supervisor = Supervisor::new(
            system.clone(),
            Arc::new(PollingRunner {
                starts: Arc::clone(&starts),
                observed: Arc::clone(&observed),
            }),
            empty_journal(),
            1,
        );
        let id = supervisor.submit(new_task("scripted"))?.id;
        supervisor.start();
        wait_for(&system, &id, TaskStatus::Running);

        let started = Instant::now();
        supervisor.shutdown();
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "shutdown waited out its grace instead of joining the worker"
        );
        assert!(
            observed.load(Ordering::SeqCst),
            "the runner saw the shutdown"
        );
        assert_eq!(system.state(&id)?.status, TaskStatus::Cancelled);
        assert_eq!(
            system.state(&id)?.outcome.as_deref(),
            Some("the control plane shut down while this task was running"),
            "a shutdown is not a human cancel, and the reason says which it was"
        );
        Ok(())
    }

    #[test]
    fn shutdown_waits_for_the_run_and_stops_the_worker() -> Result<(), Box<dyn std::error::Error>> {
        let system = system();
        let starts = Arc::new(AtomicUsize::new(0));
        let supervisor = Supervisor::new(
            system.clone(),
            Arc::new(SlowRunner {
                starts: Arc::clone(&starts),
                hold: Duration::from_millis(200),
            }),
            empty_journal(),
            1,
        );
        let id = supervisor.submit(new_task("scripted"))?.id;
        supervisor.start();
        wait_for(&system, &id, TaskStatus::Running);
        supervisor.shutdown();
        assert_eq!(
            system.state(&id)?.status,
            TaskStatus::Completed,
            "shutdown joins the running worker"
        );
        Ok(())
    }

    #[test]
    fn a_second_completion_is_rejected() -> Result<(), Box<dyn std::error::Error>> {
        let system = system();
        let supervisor = Supervisor::new(
            system.clone(),
            Arc::new(ScriptedRunner::new(Vec::new(), "done")),
            empty_journal(),
            1,
        );
        let id = supervisor.submit(new_task("scripted"))?.id;
        assert_eq!(supervisor.run_pending()?, 1);

        let late = system.ask(
            &id,
            ControlCommand::Complete {
                summary: "again".to_owned(),
            },
        );
        assert!(
            matches!(late, Err(Error::Control(ControlError::Terminal))),
            "a completed task rejects another completion: {late:?}"
        );
        assert_eq!(system.state(&id)?.outcome.as_deref(), Some("done"));
        assert!(
            supervisor.cancel(&id, "stop").is_err(),
            "a terminal task cannot be cancelled"
        );
        Ok(())
    }

    #[test]
    fn two_supervisors_start_a_task_once() -> Result<(), Box<dyn std::error::Error>> {
        let system = system();
        let starts = Arc::new(AtomicUsize::new(0));
        let runner = Arc::new(SlowRunner {
            starts: Arc::clone(&starts),
            hold: Duration::from_millis(50),
        });
        let first = Supervisor::new(system.clone(), runner.clone(), empty_journal(), 1);
        let second = Supervisor::new(system.clone(), runner, empty_journal(), 1);
        let id = first.submit(new_task("scripted"))?.id;

        let (one, two) = thread::scope(|scope| {
            let one = scope.spawn(|| first.run_pending());
            let two = scope.spawn(|| second.run_pending());
            (
                one.join().expect("the first supervisor did not panic"),
                two.join().expect("the second supervisor did not panic"),
            )
        });

        assert_eq!(
            one.expect("the first supervisor reports") + two.expect("the second reports"),
            1,
            "exactly one supervisor may start the task"
        );
        assert_eq!(starts.load(Ordering::SeqCst), 1);
        assert_eq!(system.state(&id)?.status, TaskStatus::Completed);
        Ok(())
    }

    #[test]
    fn starting_twice_does_not_double_the_workers() -> Result<(), Box<dyn std::error::Error>> {
        let system = system();
        let starts = Arc::new(AtomicUsize::new(0));
        let supervisor = Supervisor::new(
            system.clone(),
            Arc::new(SlowRunner {
                starts: Arc::clone(&starts),
                hold: Duration::from_millis(10),
            }),
            empty_journal(),
            2,
        );
        let ids: Vec<TaskId> = (0..3)
            .map(|_| {
                supervisor
                    .submit(new_task("scripted"))
                    .map(|submitted| submitted.id)
            })
            .collect::<Result<_, _>>()?;

        supervisor.start();
        supervisor.start();
        for id in &ids {
            wait_for(&system, id, TaskStatus::Completed);
        }
        supervisor.shutdown();

        assert_eq!(starts.load(Ordering::SeqCst), ids.len());
        Ok(())
    }

    #[test]
    fn a_restarted_supervisor_runs_new_tasks() -> Result<(), Box<dyn std::error::Error>> {
        let system = system();
        let supervisor = Supervisor::new(
            system.clone(),
            Arc::new(ScriptedRunner::new(Vec::new(), "done")),
            empty_journal(),
            1,
        );
        let first = supervisor.submit(new_task("scripted"))?.id;
        supervisor.start();
        wait_for(&system, &first, TaskStatus::Completed);
        supervisor.shutdown();

        let second = supervisor.submit(new_task("scripted"))?.id;
        supervisor.start();
        wait_for(&system, &second, TaskStatus::Completed);
        supervisor.shutdown();
        assert_eq!(system.state(&second)?.outcome.as_deref(), Some("done"));
        Ok(())
    }

    struct UncancelledRunner;

    impl Runner for UncancelledRunner {
        fn run(
            &self,
            _spec: &TaskSpec,
            reporter: &dyn Reporter,
        ) -> RunOutcome {
            if reporter.cancelled() {
                return RunOutcome::Failed {
                    reason: "the run started cancelled".to_owned(),
                };
            }
            RunOutcome::Completed {
                summary: "ran".to_owned(),
            }
        }
    }

    #[test]
    fn a_restart_after_shutdown_runs_without_a_cancel_mark()
    -> Result<(), Box<dyn std::error::Error>> {
        let system = system();
        let supervisor = Supervisor::new(
            system.clone(),
            Arc::new(UncancelledRunner),
            empty_journal(),
            1,
        );

        let first = supervisor.submit(new_task("scripted"))?.id;
        supervisor.start();
        wait_for(&system, &first, TaskStatus::Completed);
        assert_eq!(system.state(&first)?.outcome.as_deref(), Some("ran"));
        supervisor.shutdown();

        let second = supervisor.submit(new_task("scripted"))?.id;
        supervisor.start();
        wait_for(&system, &second, TaskStatus::Completed);
        supervisor.shutdown();
        assert_eq!(system.state(&second)?.outcome.as_deref(), Some("ran"));
        Ok(())
    }

    #[test]
    fn a_restart_runs_a_task_left_submitted() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let root = directory.path().join("tasks");
        let left = {
            let store: Arc<JsonlEventStore<TaskId, ControlEvent>> =
                Arc::new(JsonlEventStore::open(&root)?);
            let system = ControlSystem::without_idle_timeout(store, Arc::new(NoopPublisher));
            let supervisor = Supervisor::new(
                system,
                Arc::new(ScriptedRunner::new(Vec::new(), "done")),
                empty_journal(),
                1,
            );
            // Submitting without starting leaves the durable `Submitted` record behind.
            supervisor.submit(new_task("scripted"))?.id
        };

        let store: Arc<JsonlEventStore<TaskId, ControlEvent>> =
            Arc::new(JsonlEventStore::open(&root)?);
        let system = ControlSystem::without_idle_timeout(store, Arc::new(NoopPublisher));
        let supervisor = Supervisor::new(
            system.clone(),
            Arc::new(ScriptedRunner::new(Vec::new(), "done")),
            empty_journal(),
            1,
        );
        assert_eq!(system.state(&left)?.status, TaskStatus::Submitted);
        supervisor.start();
        wait_for(&system, &left, TaskStatus::Completed);
        supervisor.shutdown();
        assert_eq!(system.state(&left)?.outcome.as_deref(), Some("done"));
        Ok(())
    }

    #[test]
    fn a_restart_skips_a_corrupt_stream_and_runs_the_healthy_task()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let root = directory.path().join("tasks");
        let (left, finished) = {
            let store: Arc<JsonlEventStore<TaskId, ControlEvent>> =
                Arc::new(JsonlEventStore::open(&root)?);
            let system = ControlSystem::without_idle_timeout(store, Arc::new(NoopPublisher));
            let supervisor = Supervisor::new(
                system,
                Arc::new(ScriptedRunner::new(Vec::new(), "done")),
                empty_journal(),
                1,
            );
            let finished = supervisor.submit(new_task("scripted"))?.id;
            assert_eq!(supervisor.run_pending()?, 1);
            let left = supervisor.submit(new_task("scripted"))?.id;
            (left, finished)
        };
        let corrupt = TaskId::from_u128(99);
        fs::write(
            root.join(format!("{}.jsonl", corrupt.as_str())),
            b"not json\n",
        )?;

        let store: Arc<JsonlEventStore<TaskId, ControlEvent>> =
            Arc::new(JsonlEventStore::open(&root)?);
        let system = ControlSystem::without_idle_timeout(store, Arc::new(NoopPublisher));
        assert_eq!(
            system.recover()?,
            Recovery {
                interrupted: Vec::new(),
                submitted: vec![left.clone()],
            },
            "only the submitted task is queued; the corrupt stream is skipped"
        );
        let supervisor = Supervisor::new(
            system.clone(),
            Arc::new(ScriptedRunner::new(Vec::new(), "done")),
            empty_journal(),
            1,
        );
        supervisor.start();
        wait_for(&system, &left, TaskStatus::Completed);
        supervisor.shutdown();
        assert_eq!(system.state(&left)?.outcome.as_deref(), Some("done"));
        assert_eq!(system.state(&finished)?.status, TaskStatus::Completed);
        Ok(())
    }

    // Fails the append that carries a terminal event, so a run reaches `settle` and the
    // store refuses its outcome. Every other append lands, which is what lets the task
    // start at all.
    struct RefusingOutcomeStore {
        inner: MemoryEventStore<TaskId, ControlEvent>,
        failure: StoreError,
        remaining: AtomicUsize,
    }

    impl RefusingOutcomeStore {
        fn new(
            failure: StoreError,
            failures: usize,
        ) -> Arc<Self> {
            Arc::new(Self {
                inner: MemoryEventStore::new(),
                failure,
                remaining: AtomicUsize::new(failures),
            })
        }
    }

    fn terminal(events: &[CloudEvent<ControlEvent>]) -> bool {
        events.iter().any(|event| {
            matches!(
                event.data,
                ControlEvent::Completed { .. }
                    | ControlEvent::Failed { .. }
                    | ControlEvent::Cancelled { .. }
                    | ControlEvent::Interrupted { .. }
            )
        })
    }

    impl EventStore<TaskId, ControlEvent> for RefusingOutcomeStore {
        fn load(
            &self,
            id: &TaskId,
        ) -> Result<Vec<CloudEvent<ControlEvent>>, StoreError> {
            self.inner.load(id)
        }

        fn append(
            &self,
            id: &TaskId,
            expected: Version,
            events: &[CloudEvent<ControlEvent>],
        ) -> Result<Version, StoreError> {
            if terminal(events) && self.remaining.load(Ordering::SeqCst) > 0 {
                self.remaining.fetch_sub(1, Ordering::SeqCst);
                return Err(self.failure.clone());
            }
            self.inner.append(id, expected, events)
        }
    }

    impl EnumerableStore<TaskId, ControlEvent> for RefusingOutcomeStore {
        fn keys(&self) -> Result<Vec<TaskId>, StoreError> {
            self.inner.keys()
        }
    }

    #[test]
    fn a_leftover_run_is_interrupted_once_and_its_runner_never_starts()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let root = directory.path().join("tasks");
        let left = {
            let store: Arc<JsonlEventStore<TaskId, ControlEvent>> =
                Arc::new(JsonlEventStore::open(&root)?);
            let system = ControlSystem::without_idle_timeout(store, Arc::new(NoopPublisher));
            // Three leftovers, so a sweep that closes only the first is caught.
            let mut ids = Vec::new();
            for _ in 0..3 {
                let id = system.submit(new_task("scripted"))?.id;
                system.ask(&id, ControlCommand::Start)?;
                ids.push(id);
            }
            ids
        };

        let store: Arc<JsonlEventStore<TaskId, ControlEvent>> =
            Arc::new(JsonlEventStore::open(&root)?);
        let system = ControlSystem::without_idle_timeout(store.clone(), Arc::new(NoopPublisher));
        let starts = Arc::new(AtomicUsize::new(0));
        let supervisor = Supervisor::new(
            system.clone(),
            Arc::new(SlowRunner {
                starts: Arc::clone(&starts),
                hold: Duration::from_millis(5),
            }),
            empty_journal(),
            1,
        );
        for id in &left {
            assert_eq!(system.state(id)?.status, TaskStatus::Running);
        }

        supervisor.start();
        for id in &left {
            let state = system.state(id)?;
            assert_eq!(state.status, TaskStatus::Interrupted, "{id:?}");
            assert_eq!(state.outcome.as_deref(), Some(INTERRUPTED_REASON));
            assert_eq!(store.load(id)?.len(), 3, "{id:?}");
        }
        supervisor.shutdown();
        assert_eq!(starts.load(Ordering::SeqCst), 0, "the sweep is not a run");

        let settled: Vec<usize> = left
            .iter()
            .map(|id| store.load(id).map(|records| records.len()))
            .collect::<Result<_, _>>()?;
        supervisor.start();
        let after: Vec<usize> = left
            .iter()
            .map(|id| store.load(id).map(|records| records.len()))
            .collect::<Result<_, _>>()?;
        assert_eq!(
            after, settled,
            "a second start finds a terminal state and appends nothing"
        );
        supervisor.shutdown();
        Ok(())
    }

    #[test]
    fn settle_retries_after_one_conflict() -> Result<(), Box<dyn std::error::Error>> {
        let conflicted = |failures| {
            RefusingOutcomeStore::new(
                StoreError::Conflict {
                    expected: Version::new(2),
                    actual: Version::new(3),
                },
                failures,
            )
        };
        for failures in [1, 2] {
            let store = conflicted(failures);
            let system =
                ControlSystem::without_idle_timeout(store.clone(), Arc::new(NoopPublisher));
            let supervisor = Supervisor::new(
                system.clone(),
                Arc::new(ScriptedRunner::new(Vec::new(), "done")),
                empty_journal(),
                1,
            );
            let id = supervisor.submit(new_task("scripted"))?.id;

            assert_eq!(supervisor.run_pending()?, 1, "{failures} conflicts");
            let state = system.state(&id)?;
            assert_eq!(state.status, TaskStatus::Completed, "{failures} conflicts");
            assert_eq!(state.outcome.as_deref(), Some("done"));
            assert_eq!(
                store.load(&id)?.len(),
                4,
                "submitted, started, the session, and the settled outcome"
            );
        }
        Ok(())
    }

    // Cancels its own task through the system before it reports an outcome, which is how a
    // human cancel that lands mid-run looks to `settle`: its own `Complete` meets a
    // terminal task.
    struct SelfCancellingRunner {
        system: ControlSystem,
    }

    impl Runner for SelfCancellingRunner {
        fn run(
            &self,
            spec: &TaskSpec,
            _reporter: &dyn Reporter,
        ) -> RunOutcome {
            // The spec comes from the task's own stream, so its id is the stream to cancel.
            let _ = self.system.ask(
                &spec.task_id,
                ControlCommand::Cancel {
                    reason: "stop".to_owned(),
                },
            );
            RunOutcome::Completed {
                summary: "done".to_owned(),
            }
        }
    }

    #[test]
    fn settle_counts_a_terminal_refusal_as_success() -> Result<(), Box<dyn std::error::Error>> {
        let system = system();
        let supervisor = Supervisor::new(
            system.clone(),
            Arc::new(SelfCancellingRunner {
                system: system.clone(),
            }),
            empty_journal(),
            1,
        );
        let id = supervisor.submit(new_task("scripted"))?.id;

        let ran = supervisor.run_pending();
        assert_eq!(
            ran?, 1,
            "a terminal refusal from settle is a success, not an error"
        );
        let state = system.state(&id)?;
        assert_eq!(state.status, TaskStatus::Cancelled);
        assert_eq!(state.outcome.as_deref(), Some("stop"));
        Ok(())
    }

    struct OrderRunner {
        seen: Arc<Mutex<Vec<String>>>,
    }

    impl Runner for OrderRunner {
        fn run(
            &self,
            spec: &TaskSpec,
            _reporter: &dyn Reporter,
        ) -> RunOutcome {
            self.seen
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(spec.title.clone());
            RunOutcome::Completed {
                summary: "done".to_owned(),
            }
        }
    }

    #[test]
    fn run_pending_runs_a_same_second_burst_oldest_first() -> Result<(), Box<dyn std::error::Error>>
    {
        // Every stamp is one second, so only the id decides. The ids ascend with the
        // submission order, so the oldest task has the lowest id and a reversal of the
        // newest-first listing would run this backwards.
        let store = Arc::new(MemoryEventStore::<TaskId, ControlEvent>::new());
        let system = ControlSystem::without_idle_timeout(store.clone(), Arc::new(NoopPublisher));
        for (id, title) in [
            (TaskId::from_u128(1), "first"),
            (TaskId::from_u128(2), "second"),
            (TaskId::from_u128(3), "third"),
        ] {
            let spec = TaskSpec {
                task_id: id.clone(),
                title: title.to_owned(),
                instruction: "Run it.".to_owned(),
                agent: "scripted".to_owned(),
                host: None,
                workspace: None,
            };
            store.append(
                &id,
                Version::ZERO,
                &[CloudEvent::new(
                    id.as_str(),
                    Version::new(1),
                    ControlEvent::Submitted { spec },
                )
                .with_time("2026-11-01T00:00:00.000Z")],
            )?;
        }

        let seen = Arc::new(Mutex::new(Vec::new()));
        let supervisor = Supervisor::new(
            system,
            Arc::new(OrderRunner {
                seen: Arc::clone(&seen),
            }),
            empty_journal(),
            1,
        );
        assert_eq!(supervisor.run_pending()?, 3);

        let order = seen
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        assert_eq!(
            order,
            vec!["first", "second", "third"],
            "the lowest id is the oldest when the stamps tie"
        );
        Ok(())
    }

    #[test]
    fn a_repeated_request_id_names_one_task() -> Result<(), Box<dyn std::error::Error>> {
        // The queue side of this is pinned in `supervisor.rs`, where the queue is visible;
        // `run_pending` walks the store, so it cannot see a duplicate queue entry.
        let system = system();
        let supervisor = Supervisor::new(
            system.clone(),
            Arc::new(ScriptedRunner::new(Vec::new(), "done")),
            empty_journal(),
            1,
        );
        let mut new = new_task("scripted");
        new.request_id = Some("01890b0c-7f3a-7c2e-9f4a-2b1c3d4e5f60".to_owned());
        let first = supervisor.submit(new.clone())?;
        let repeated = supervisor.submit(new)?;

        assert!(first.created);
        assert!(!repeated.created);
        assert_eq!(first.id, repeated.id);
        assert_eq!(
            supervisor.run_pending()?,
            1,
            "one task, one run, however often it was submitted"
        );
        assert_eq!(system.state(&first.id)?.status, TaskStatus::Completed);
        Ok(())
    }

    #[test]
    fn a_store_that_always_refuses_the_outcome_leaves_the_task_running()
    -> Result<(), Box<dyn std::error::Error>> {
        let store = RefusingOutcomeStore::new(StoreError::Io("the disk is full".to_owned()), 99);
        let system = ControlSystem::without_idle_timeout(store, Arc::new(NoopPublisher));
        let supervisor = Supervisor::new(
            system.clone(),
            Arc::new(ScriptedRunner::new(Vec::new(), "done")),
            empty_journal(),
            1,
        );
        let id = supervisor.submit(new_task("scripted"))?.id;

        let ran = supervisor.run_pending();
        assert!(
            matches!(ran, Err(Error::Store(StoreError::Io(_)))),
            "the failed outcome must surface: {ran:?}"
        );
        assert_eq!(system.state(&id)?.status, TaskStatus::Running);
        Ok(())
    }

    #[cfg(unix)]
    #[derive(Clone, Copy)]
    enum Fault {
        // The record reaches the file and the sync then fails, so the bytes are durable and
        // both the adapter's cache and the actor's version still name the old version.
        LandedThenSyncFailed,
        // Only part of the record reaches the file, and the sync fails, so the stream ends on
        // a line no writer committed.
        Torn,
    }

    // Drives one terminal append through the real adapter with an injected fault, then undoes
    // the fault so the same store instance keeps its cache across the retry.
    #[cfg(unix)]
    struct FaultOutcomeStore {
        inner: JsonlEventStore<TaskId, ControlEvent>,
        root: PathBuf,
        fault: Fault,
        fired: AtomicBool,
    }

    #[cfg(unix)]
    impl FaultOutcomeStore {
        fn open(
            root: &Path,
            fault: Fault,
        ) -> Result<Arc<Self>, StoreError> {
            Ok(Arc::new(Self {
                inner: JsonlEventStore::open(root)?,
                root: root.to_path_buf(),
                fault,
                fired: AtomicBool::new(false),
            }))
        }

        // Moves the committed file aside, damages it the way the fault describes, and puts
        // `/dev/null` in its place for one append, where `write_all` succeeds and `sync_data`
        // fails with `EINVAL`. Returns the held file so the caller can restore it.
        fn redirect(
            &self,
            id: &TaskId,
            events: &[CloudEvent<ControlEvent>],
        ) -> Result<PathBuf, StoreError> {
            let path = self.root.join(format!("{}.jsonl", id.as_str()));
            let held = self.root.join(format!("{}.held", id.as_str()));
            fs::rename(&path, &held).map_err(|error| io_error(&path, &error))?;
            let mut file = OpenOptions::new()
                .append(true)
                .open(&held)
                .map_err(|error| io_error(&held, &error))?;
            // One record per line, which is the format `encode_records` writes.
            let mut buffer = Vec::new();
            for event in events {
                let line = serde_json::to_string(event)
                    .map_err(|error| StoreError::Io(error.to_string()))?;
                buffer.extend_from_slice(line.as_bytes());
                buffer.push(b'\n');
            }
            match self.fault {
                Fault::LandedThenSyncFailed => {
                    file.write_all(&buffer)
                        .map_err(|error| io_error(&held, &error))?;
                },
                Fault::Torn => {
                    let torn = buffer.len().saturating_sub(1);
                    file.write_all(&buffer[..torn])
                        .map_err(|error| io_error(&held, &error))?;
                },
            }
            std::os::unix::fs::symlink("/dev/null", &path)
                .map_err(|error| io_error(&path, &error))?;
            Ok(held)
        }

        fn restore(
            &self,
            id: &TaskId,
            held: &Path,
        ) {
            let path = self.root.join(format!("{}.jsonl", id.as_str()));
            let _ = fs::remove_file(&path);
            let _ = fs::rename(held, &path);
        }
    }

    #[cfg(unix)]
    fn io_error(
        path: &Path,
        error: &std::io::Error,
    ) -> StoreError {
        StoreError::Io(format!("{}: {error}", path.display()))
    }

    #[cfg(unix)]
    impl EventStore<TaskId, ControlEvent> for FaultOutcomeStore {
        fn load(
            &self,
            id: &TaskId,
        ) -> Result<Vec<CloudEvent<ControlEvent>>, StoreError> {
            self.inner.load(id)
        }

        fn append(
            &self,
            id: &TaskId,
            expected: Version,
            events: &[CloudEvent<ControlEvent>],
        ) -> Result<Version, StoreError> {
            if !terminal(events) || self.fired.swap(true, Ordering::SeqCst) {
                return self.inner.append(id, expected, events);
            }
            let held = self.redirect(id, events)?;
            let appended = self.inner.append(id, expected, events);
            self.restore(id, &held);
            appended
        }
    }

    #[cfg(unix)]
    impl EnumerableStore<TaskId, ControlEvent> for FaultOutcomeStore {
        fn keys(&self) -> Result<Vec<TaskId>, StoreError> {
            self.inner.keys()
        }
    }

    #[cfg(unix)]
    fn settle_after_a_fault(fault: Fault) -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let root = directory.path().join("tasks");
        let store = FaultOutcomeStore::open(&root, fault)?;
        let system = ControlSystem::without_idle_timeout(store.clone(), Arc::new(NoopPublisher));
        let supervisor = Supervisor::new(
            system.clone(),
            Arc::new(ScriptedRunner::new(Vec::new(), "done")),
            empty_journal(),
            1,
        );
        let id = supervisor.submit(new_task("scripted"))?.id;

        // The retry is what heals this: one attempt sees the I/O failure, the next reaches a
        // store that dropped its cached version and re-read the file.
        assert_eq!(
            supervisor.run_pending()?,
            1,
            "a retried outcome write finishes the run"
        );
        let state = system.state(&id)?;
        assert_eq!(state.status, TaskStatus::Completed);
        assert_eq!(state.outcome.as_deref(), Some("done"));
        assert_eq!(
            system.list()?.len(),
            1,
            "the stream must replay, whatever the fault left behind"
        );
        let records = store.load(&id)?;
        assert_eq!(
            records
                .iter()
                .filter(|record| terminal(std::slice::from_ref(record)))
                .count(),
            1,
            "exactly one terminal record, written once"
        );
        let mut version = Version::ZERO;
        for record in &records {
            version = version.next();
            assert_eq!(record.version, version, "the stream stays contiguous");
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn a_landed_record_whose_sync_failed_is_retried_once() -> Result<(), Box<dyn std::error::Error>>
    {
        settle_after_a_fault(Fault::LandedThenSyncFailed)
    }

    #[cfg(unix)]
    #[test]
    fn a_torn_outcome_write_is_rewritten_by_the_retry() -> Result<(), Box<dyn std::error::Error>> {
        settle_after_a_fault(Fault::Torn)
    }
}

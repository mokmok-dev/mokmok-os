//! The supervisor: queueing, the start fence, cancellation, and settling.

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::thread;
    use std::time::{Duration, Instant};

    use mokmok_control::{
        AgentUpdate, ControlCommand, ControlError, ControlEvent, ControlSystem, Error,
        FailingRunner, NewTask, NoopPublisher, Reporter, RunOutcome, Runner, ScriptedRunner,
        Supervisor, TaskId, TaskSpec, TaskStatus,
    };
    use mokmok_jobs_jsonl::JsonlEventStore;
    use mokmok_jobs_memory::MemoryEventStore;

    fn message(text: &str) -> AgentUpdate {
        AgentUpdate::Message {
            text: text.to_owned(),
        }
    }

    fn new_task(agent: &str) -> NewTask {
        NewTask {
            title: "Fix the build".to_owned(),
            instruction: "Make the tests pass.".to_owned(),
            agent: agent.to_owned(),
            host: None,
            workspace: Some("/work/mokmok".to_owned()),
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
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if system.state(id).is_ok_and(|state| state.status == status) {
                return;
            }
            assert!(Instant::now() < deadline, "{id:?} never reached {status:?}");
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
        let runner = Arc::new(ScriptedRunner::new(
            vec![message("one"), message("two")],
            "all done",
        ));
        let supervisor = Supervisor::new(system.clone(), runner, 1);

        let id = supervisor.submit(new_task("scripted"))?;
        assert_eq!(supervisor.run_pending()?, 1);

        let state = system.state(&id)?;
        assert_eq!(state.status, TaskStatus::Completed);
        assert_eq!(state.session.as_deref(), Some("scripted-session"));
        assert_eq!(state.updates, vec![message("one"), message("two")]);
        assert_eq!(state.outcome.as_deref(), Some("all done"));
        assert_eq!(supervisor.run_pending()?, 0);
        Ok(())
    }

    #[test]
    fn a_failing_run_fails_the_task() -> Result<(), Box<dyn std::error::Error>> {
        let system = system();
        let supervisor = Supervisor::new(system.clone(), Arc::new(FailingRunner::new("boom")), 1);

        let id = supervisor.submit(new_task("scripted"))?;
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
        let supervisor = Supervisor::new(system.clone(), runner, 1);

        let id = supervisor.submit(new_task("scripted"))?;
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
        let supervisor = Supervisor::new(system.clone(), runner, 1);

        let id = supervisor.submit(new_task("scripted"))?;
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
    fn shutdown_waits_for_the_run_and_stops_the_worker() -> Result<(), Box<dyn std::error::Error>> {
        let system = system();
        let starts = Arc::new(AtomicUsize::new(0));
        let supervisor = Supervisor::new(
            system.clone(),
            Arc::new(SlowRunner {
                starts: Arc::clone(&starts),
                hold: Duration::from_millis(200),
            }),
            1,
        );
        let id = supervisor.submit(new_task("scripted"))?;
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
            1,
        );
        let id = supervisor.submit(new_task("scripted"))?;
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
        let first = Supervisor::new(system.clone(), runner.clone(), 1);
        let second = Supervisor::new(system.clone(), runner, 1);
        let id = first.submit(new_task("scripted"))?;

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
            2,
        );
        let ids: Vec<TaskId> = (0..3)
            .map(|_| supervisor.submit(new_task("scripted")))
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
            1,
        );
        let first = supervisor.submit(new_task("scripted"))?;
        supervisor.start();
        wait_for(&system, &first, TaskStatus::Completed);
        supervisor.shutdown();

        let second = supervisor.submit(new_task("scripted"))?;
        supervisor.start();
        wait_for(&system, &second, TaskStatus::Completed);
        supervisor.shutdown();
        assert_eq!(system.state(&second)?.outcome.as_deref(), Some("done"));
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
            let supervisor =
                Supervisor::new(system, Arc::new(ScriptedRunner::new(Vec::new(), "done")), 1);
            // Submitting without starting leaves the durable `Submitted` record behind.
            supervisor.submit(new_task("scripted"))?
        };

        let store: Arc<JsonlEventStore<TaskId, ControlEvent>> =
            Arc::new(JsonlEventStore::open(&root)?);
        let system = ControlSystem::without_idle_timeout(store, Arc::new(NoopPublisher));
        let supervisor = Supervisor::new(
            system.clone(),
            Arc::new(ScriptedRunner::new(Vec::new(), "done")),
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
            let supervisor =
                Supervisor::new(system, Arc::new(ScriptedRunner::new(Vec::new(), "done")), 1);
            let finished = supervisor.submit(new_task("scripted"))?;
            assert_eq!(supervisor.run_pending()?, 1);
            let left = supervisor.submit(new_task("scripted"))?;
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
            system.submitted()?,
            vec![left.clone()],
            "only the submitted task is queued; the corrupt stream is skipped"
        );
        let supervisor = Supervisor::new(
            system.clone(),
            Arc::new(ScriptedRunner::new(Vec::new(), "done")),
            1,
        );
        supervisor.start();
        wait_for(&system, &left, TaskStatus::Completed);
        supervisor.shutdown();
        assert_eq!(system.state(&left)?.outcome.as_deref(), Some("done"));
        assert_eq!(system.state(&finished)?.status, TaskStatus::Completed);
        Ok(())
    }
}

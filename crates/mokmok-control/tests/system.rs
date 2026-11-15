//! The actor runtime: durability, serialization, listing, and retirement.

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex, PoisonError};
    use std::thread;
    use std::time::{Duration, Instant};

    use mokmok_control::{
        CloudEvent, ControlCommand, ControlError, ControlEvent, ControlPublisher, ControlSystem,
        EnumerableStore, Error, EventStore, NewTask, NoopPublisher, StoreError, Submission, TaskId,
        TaskSpec, TaskStatus, Version,
    };
    use mokmok_jobs_jsonl::JsonlEventStore;
    use mokmok_jobs_memory::MemoryEventStore;

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

    fn spec(id: &TaskId) -> TaskSpec {
        TaskSpec {
            task_id: id.clone(),
            title: "Fix the build".to_owned(),
            instruction: "Make the tests pass.".to_owned(),
            agent: "scripted".to_owned(),
            host: None,
            workspace: Some("/work/mokmok".to_owned()),
        }
    }

    fn memory() -> Arc<MemoryEventStore<TaskId, ControlEvent>> {
        Arc::new(MemoryEventStore::new())
    }

    #[derive(Default)]
    struct CountingStore {
        inner: MemoryEventStore<TaskId, ControlEvent>,
        loads: AtomicUsize,
    }

    impl CountingStore {
        fn loads(&self) -> usize {
            self.loads.load(Ordering::SeqCst)
        }
    }

    impl EventStore<TaskId, ControlEvent> for CountingStore {
        fn load(
            &self,
            id: &TaskId,
        ) -> Result<Vec<CloudEvent<ControlEvent>>, StoreError> {
            self.loads.fetch_add(1, Ordering::SeqCst);
            self.inner.load(id)
        }

        fn append(
            &self,
            id: &TaskId,
            expected: Version,
            events: &[CloudEvent<ControlEvent>],
        ) -> Result<Version, StoreError> {
            self.inner.append(id, expected, events)
        }
    }

    impl EnumerableStore<TaskId, ControlEvent> for CountingStore {
        fn keys(&self) -> Result<Vec<TaskId>, StoreError> {
            self.inner.keys()
        }
    }

    struct ConflictingStore {
        inner: MemoryEventStore<TaskId, ControlEvent>,
        failed: AtomicBool,
    }

    impl EventStore<TaskId, ControlEvent> for ConflictingStore {
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
            if !self.failed.swap(true, Ordering::SeqCst) {
                return Err(StoreError::Conflict {
                    expected,
                    actual: expected.next(),
                });
            }
            self.inner.append(id, expected, events)
        }
    }

    impl EnumerableStore<TaskId, ControlEvent> for ConflictingStore {
        fn keys(&self) -> Result<Vec<TaskId>, StoreError> {
            self.inner.keys()
        }
    }

    struct IllegalStore {
        id: TaskId,
        records: Vec<CloudEvent<ControlEvent>>,
    }

    impl EventStore<TaskId, ControlEvent> for IllegalStore {
        fn load(
            &self,
            _id: &TaskId,
        ) -> Result<Vec<CloudEvent<ControlEvent>>, StoreError> {
            Ok(self.records.clone())
        }

        fn append(
            &self,
            _id: &TaskId,
            _expected: Version,
            _events: &[CloudEvent<ControlEvent>],
        ) -> Result<Version, StoreError> {
            Err(StoreError::Io("the illegal store is read-only".to_owned()))
        }
    }

    impl EnumerableStore<TaskId, ControlEvent> for IllegalStore {
        fn keys(&self) -> Result<Vec<TaskId>, StoreError> {
            Ok(vec![self.id.clone()])
        }
    }

    #[derive(Default)]
    struct RecordingPublisher {
        seen: Mutex<Vec<ControlEvent>>,
    }

    impl RecordingPublisher {
        fn seen(&self) -> Vec<ControlEvent> {
            self.seen
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
        }
    }

    impl ControlPublisher for RecordingPublisher {
        fn publish(
            &self,
            events: &[CloudEvent<ControlEvent>],
        ) {
            self.seen
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .extend(events.iter().map(|event| event.data.clone()));
        }
    }

    #[test]
    fn a_task_survives_a_restart_over_a_jsonl_log() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let root = directory.path().join("tasks");
        let id = {
            let store = Arc::new(JsonlEventStore::<TaskId, ControlEvent>::open(&root)?);
            let system = ControlSystem::new(store, Arc::new(NoopPublisher));
            let id = system.submit(new_task("scripted"))?.id;
            system.ask(&id, ControlCommand::Start)?;
            system.ask(
                &id,
                ControlCommand::OpenSession {
                    session_id: "session-1".to_owned(),
                },
            )?;
            system.ask(
                &id,
                ControlCommand::Complete {
                    summary: "done".to_owned(),
                },
            )?;
            id
        };

        let store = Arc::new(JsonlEventStore::<TaskId, ControlEvent>::open(&root)?);
        let system = ControlSystem::new(store, Arc::new(NoopPublisher));

        let state = system.state(&id)?;
        assert_eq!(state.status, TaskStatus::Completed);
        assert_eq!(state.outcome.as_deref(), Some("done"));
        assert_eq!(state.session.as_deref(), Some("session-1"));
        assert_eq!(state.updates, [] as [mokmok_control::AgentUpdate; 0]);
        assert_eq!(state.update_count, 0);

        let summaries = system.list()?;
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].id, id);
        assert_eq!(summaries[0].status, TaskStatus::Completed);
        assert_eq!(summaries[0].updates, 0);
        Ok(())
    }

    #[test]
    fn two_threads_race_one_terminal_command_into_one_contiguous_stream()
    -> Result<(), Box<dyn std::error::Error>> {
        let store = memory();
        let system = ControlSystem::without_idle_timeout(store.clone(), Arc::new(NoopPublisher));
        let id = system.submit(new_task("scripted"))?.id;
        system.ask(&id, ControlCommand::Start)?;

        let (first, second) = thread::scope(|scope| {
            let one = {
                let system = system.clone();
                let id = id.clone();
                scope.spawn(move || {
                    system.ask(
                        &id,
                        ControlCommand::Complete {
                            summary: "done".to_owned(),
                        },
                    )
                })
            };
            let two = {
                let system = system.clone();
                let id = id.clone();
                scope.spawn(move || {
                    system.ask(
                        &id,
                        ControlCommand::Cancel {
                            reason: "stop".to_owned(),
                        },
                    )
                })
            };
            (
                one.join().expect("the first asking thread did not panic"),
                two.join().expect("the second asking thread did not panic"),
            )
        });

        assert!(
            first.is_ok() ^ second.is_ok(),
            "exactly one terminal command may commit"
        );
        let records = store.load(&id)?;
        assert_eq!(records.len(), 3);
        let mut version = Version::ZERO;
        for record in &records {
            version = version.next();
            assert_eq!(record.version, version);
            assert_eq!(record.subject, id.as_str());
        }
        let state = system.state(&id)?;
        assert!(
            matches!(state.status, TaskStatus::Completed | TaskStatus::Cancelled),
            "{:?}",
            state.status
        );
        Ok(())
    }

    #[test]
    fn list_summarises_every_task() -> Result<(), Box<dyn std::error::Error>> {
        let system = ControlSystem::without_idle_timeout(memory(), Arc::new(NoopPublisher));
        let done = system.submit(new_task("scripted"))?.id;
        let waiting = system.submit(new_task("scripted"))?.id;
        let running = system.submit(new_task("other"))?.id;
        system.ask(&done, ControlCommand::Start)?;
        system.ask(
            &done,
            ControlCommand::Complete {
                summary: "done".to_owned(),
            },
        )?;
        system.ask(&running, ControlCommand::Start)?;

        let summaries = system.list()?;
        assert_eq!(summaries.len(), 3);
        let mut listed: Vec<TaskId> = summaries.iter().map(|summary| summary.id.clone()).collect();
        listed.sort();
        let mut expected = vec![done.clone(), waiting.clone(), running.clone()];
        expected.sort();
        assert_eq!(listed, expected, "every task is listed once");

        let status = |id: &TaskId| {
            summaries
                .iter()
                .find(|summary| &summary.id == id)
                .map(|summary| (summary.status, summary.agent.clone(), summary.updates))
        };
        assert_eq!(
            status(&done),
            Some((TaskStatus::Completed, "scripted".to_owned(), 0))
        );
        assert_eq!(
            status(&waiting),
            Some((TaskStatus::Submitted, "scripted".to_owned(), 0))
        );
        assert_eq!(
            status(&running),
            Some((TaskStatus::Running, "other".to_owned(), 0))
        );
        Ok(())
    }

    #[test]
    fn list_orders_by_the_first_record_not_the_id() -> Result<(), Box<dyn std::error::Error>> {
        // The older task has the lower id, so an ascending id sort puts it first and a
        // newest-first stamp sort puts it last. A comparator that ignored the stamp would
        // come out in id order, which disagrees.
        let store = memory();
        let system = ControlSystem::without_idle_timeout(store.clone(), Arc::new(NoopPublisher));
        let newer = TaskId::from_u128(9);
        let older = TaskId::from_u128(1);
        let unstamped = TaskId::from_u128(5);
        for (id, time) in [
            (&older, Some("2026-11-01T00:00:00.000Z")),
            (&newer, Some("2026-11-02T00:00:00.000Z")),
            (&unstamped, None),
        ] {
            let record = CloudEvent::new(
                id.as_str(),
                Version::new(1),
                ControlEvent::Submitted { spec: spec(id) },
            );
            let record = match time {
                Some(time) => record.with_time(time),
                None => record,
            };
            store.append(id, Version::ZERO, &[record])?;
        }

        let listed: Vec<TaskId> = system
            .list()?
            .into_iter()
            .map(|summary| summary.id)
            .collect();
        assert_eq!(
            listed,
            vec![newer, older, unstamped],
            "newest first by stamp, and a stream with no stamp sorts last"
        );
        Ok(())
    }

    #[test]
    fn a_zero_idle_timeout_retires_the_actor_and_the_next_command_replays()
    -> Result<(), Box<dyn std::error::Error>> {
        let store = Arc::new(CountingStore::default());
        let system = ControlSystem::with_idle_timeout(
            store.clone(),
            Arc::new(NoopPublisher),
            Duration::ZERO,
        );
        let id = system.submit(new_task("scripted"))?.id;
        system.ask(&id, ControlCommand::Start)?;

        // A busy host stretches the retirement well past 5 s; the bound stays under
        // cargo-mutants' 60 s, so a real hang still fails here.
        let deadline = Instant::now() + Duration::from_secs(15);
        let session = ControlCommand::OpenSession {
            session_id: "session-1".to_owned(),
        };
        loop {
            thread::sleep(Duration::from_millis(5));
            // The first ask opens the session; later asks are refused by the fresh
            // actor, and a load count above one is what proves it replayed.
            let _ = system.ask(&id, session.clone());
            if store.loads() > 1 {
                break;
            }
            assert!(Instant::now() < deadline, "the actor never retired");
        }

        assert_eq!(system.state(&id)?.status, TaskStatus::Running);
        assert_eq!(system.state(&id)?.session.as_deref(), Some("session-1"));
        let appended = system.ask(
            &id,
            ControlCommand::Complete {
                summary: "done".to_owned(),
            },
        )?;
        assert_eq!(
            appended.version.get(),
            u64::try_from(store.load(&id)?.len())?
        );
        Ok(())
    }

    #[test]
    fn a_store_conflict_closes_the_actor_and_the_next_command_recovers()
    -> Result<(), Box<dyn std::error::Error>> {
        let store = Arc::new(ConflictingStore {
            inner: MemoryEventStore::new(),
            failed: AtomicBool::new(false),
        });
        let system = ControlSystem::without_idle_timeout(store.clone(), Arc::new(NoopPublisher));
        let id = TaskId::from_u128(1);

        let conflict = system.ask(&id, ControlCommand::Submit(spec(&id)));
        assert!(
            matches!(conflict, Err(Error::Store(StoreError::Conflict { .. }))),
            "the store conflict must surface: {conflict:?}"
        );

        let appended = system.ask(&id, ControlCommand::Submit(spec(&id)))?;
        assert_eq!(appended.version, Version::new(1));
        assert_eq!(system.state(&id)?.status, TaskStatus::Submitted);
        assert_eq!(store.load(&id)?.len(), 1);
        Ok(())
    }

    // Commits the record and then reports a conflict, which is what a lost race looks
    // like: the bytes are in the log, the losing actor's version was stale.
    struct WonRaceStore {
        inner: MemoryEventStore<TaskId, ControlEvent>,
        raced: AtomicBool,
    }

    impl EventStore<TaskId, ControlEvent> for WonRaceStore {
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
            let committed = self.inner.append(id, expected, events)?;
            if !self.raced.swap(true, Ordering::SeqCst) {
                return Err(StoreError::Conflict {
                    expected,
                    actual: committed,
                });
            }
            Ok(committed)
        }
    }

    impl EnumerableStore<TaskId, ControlEvent> for WonRaceStore {
        fn keys(&self) -> Result<Vec<TaskId>, StoreError> {
            self.inner.keys()
        }
    }

    #[test]
    fn a_submit_that_loses_a_race_answers_from_the_winners_state()
    -> Result<(), Box<dyn std::error::Error>> {
        let store = Arc::new(WonRaceStore {
            inner: MemoryEventStore::new(),
            raced: AtomicBool::new(false),
        });
        let system = ControlSystem::without_idle_timeout(store.clone(), Arc::new(NoopPublisher));
        let mut new = new_task("scripted");
        new.request_id = Some("01890b0c-7f3a-7c2e-9f4a-2b1c3d4e5f60".to_owned());

        let submission = system.submit(new)?;

        assert!(
            !submission.created,
            "the record was already committed, so this call did not create the task"
        );
        assert_eq!(store.load(&submission.id)?.len(), 1, "one submission");
        assert_eq!(system.state(&submission.id)?.status, TaskStatus::Submitted);
        Ok(())
    }

    #[test]
    fn a_second_submit_for_a_different_spec_over_one_store_conflicts()
    -> Result<(), Box<dyn std::error::Error>> {
        let store = Arc::new(WonRaceStore {
            inner: MemoryEventStore::new(),
            raced: AtomicBool::new(false),
        });
        let system = ControlSystem::without_idle_timeout(store, Arc::new(NoopPublisher));
        let mut first = new_task("scripted");
        first.request_id = Some("01890b0c-7f3a-7c2e-9f4a-2b1c3d4e5f60".to_owned());
        let mut differing = first.clone();
        differing.instruction = "Make something else pass.".to_owned();

        let winner = system.submit(first)?;
        let loser = system.submit(differing);

        let conflicts = matches!(&loser, Err(Error::RequestConflict(id)) if *id == winner.id);
        assert!(
            conflicts,
            "a lost race with other content conflicts: {loser:?}"
        );
        Ok(())
    }

    #[test]
    fn submit_validates_the_spec_before_it_appends() -> Result<(), Box<dyn std::error::Error>> {
        let system = ControlSystem::without_idle_timeout(memory(), Arc::new(NoopPublisher));
        let mut new = new_task("scripted");
        new.title = String::new();

        let result = system.submit(new);
        assert!(
            matches!(
                result,
                Err(Error::Control(ControlError::InvalidSpec {
                    field: "title",
                    ..
                }))
            ),
            "an empty title must be refused: {result:?}"
        );
        assert!(system.list()?.is_empty(), "nothing was appended");
        Ok(())
    }

    #[test]
    fn a_request_id_names_one_task_whatever_state_it_reached()
    -> Result<(), Box<dyn std::error::Error>> {
        let store = memory();
        let system = ControlSystem::without_idle_timeout(store.clone(), Arc::new(NoopPublisher));
        let request_id = "01890b0c-7f3a-7c2e-9f4a-2b1c3d4e5f60";
        let mut new = new_task("scripted");
        new.request_id = Some(request_id.to_owned());

        let first = system.submit(new.clone())?;
        assert!(first.created);
        let repeated = |system: &ControlSystem| system.submit(new.clone());
        let same = Submission {
            id: first.id.clone(),
            created: false,
        };
        assert_eq!(repeated(&system)?, same, "a resend from Submitted");

        system.ask(&first.id, ControlCommand::Start)?;
        assert_eq!(repeated(&system)?, same, "a resend from Running");

        system.ask(
            &first.id,
            ControlCommand::Complete {
                summary: "done".to_owned(),
            },
        )?;
        assert_eq!(repeated(&system)?, same, "a resend from Completed");

        let submitted = store
            .load(&first.id)?
            .into_iter()
            .filter(|event| matches!(event.data, ControlEvent::Submitted { .. }))
            .count();
        assert_eq!(submitted, 1, "a resend commits no second submission");
        Ok(())
    }

    #[test]
    fn a_request_id_names_one_task_after_an_interrupt() -> Result<(), Box<dyn std::error::Error>> {
        let system = ControlSystem::without_idle_timeout(memory(), Arc::new(NoopPublisher));
        let mut new = new_task("scripted");
        new.request_id = Some("01890b0c-7f3a-7c2e-9f4a-2b1c3d4e5f60".to_owned());

        let first = system.submit(new.clone())?;
        system.ask(&first.id, ControlCommand::Start)?;
        system.ask(&first.id, ControlCommand::Interrupt)?;
        assert_eq!(system.state(&first.id)?.status, TaskStatus::Interrupted);

        assert_eq!(
            system.submit(new)?,
            Submission {
                id: first.id,
                created: false
            },
            "the state a client is most likely to resend against"
        );
        Ok(())
    }

    #[test]
    fn a_request_id_with_different_content_is_a_conflict() -> Result<(), Box<dyn std::error::Error>>
    {
        let system = ControlSystem::without_idle_timeout(memory(), Arc::new(NoopPublisher));
        let mut new = new_task("scripted");
        new.host = Some("eu".to_owned());
        new.request_id = Some("01890b0c-7f3a-7c2e-9f4a-2b1c3d4e5f60".to_owned());
        let first = system.submit(new.clone())?;

        // One differing value per field, so a comparison that forgets a field fails here.
        let mut title = new.clone();
        title.title = "Another title".to_owned();
        let mut instruction = new.clone();
        instruction.instruction = "Make something else pass.".to_owned();
        let mut agent = new.clone();
        agent.agent = "other".to_owned();
        let mut host = new.clone();
        host.host = None;
        let mut workspace = new;
        workspace.workspace = None;

        for (field, changed) in [
            ("title", title),
            ("instruction", instruction),
            ("agent", agent),
            ("host", host),
            ("workspace", workspace),
        ] {
            let conflict = system.submit(changed);
            let conflicts = matches!(&conflict, Err(Error::RequestConflict(id)) if *id == first.id);
            assert!(conflicts, "a differing {field} must conflict: {conflict:?}");
        }
        Ok(())
    }

    #[test]
    fn a_request_id_that_is_not_a_uuid_is_refused() -> Result<(), Box<dyn std::error::Error>> {
        let system = ControlSystem::without_idle_timeout(memory(), Arc::new(NoopPublisher));
        let mut new = new_task("scripted");
        new.request_id = Some("not-a-uuid".to_owned());

        assert!(matches!(
            system.submit(new),
            Err(Error::Control(ControlError::InvalidRequestId(_)))
        ));
        assert!(system.list()?.is_empty(), "nothing was appended");
        Ok(())
    }

    #[test]
    fn the_publisher_sees_each_batch_before_the_caller_is_answered()
    -> Result<(), Box<dyn std::error::Error>> {
        let publisher = Arc::new(RecordingPublisher::default());
        let system = ControlSystem::without_idle_timeout(memory(), publisher.clone());
        let id = system.submit(new_task("scripted"))?.id;
        assert_eq!(publisher.seen().len(), 1);
        assert!(matches!(
            publisher.seen()[0],
            ControlEvent::Submitted { .. }
        ));

        system.ask(&id, ControlCommand::Start)?;
        assert_eq!(publisher.seen().len(), 2);
        assert_eq!(publisher.seen()[1], ControlEvent::Started);

        system.ask(
            &id,
            ControlCommand::Complete {
                summary: "done".to_owned(),
            },
        )?;
        assert_eq!(publisher.seen().len(), 3);
        assert_eq!(
            publisher.seen()[2],
            ControlEvent::Completed {
                summary: "done".to_owned()
            }
        );
        Ok(())
    }

    #[test]
    fn list_names_the_task_whose_stream_does_not_replay() {
        let id = TaskId::from_u128(7);
        let store = Arc::new(IllegalStore {
            id: id.clone(),
            records: vec![CloudEvent::new(
                id.as_str(),
                Version::new(1),
                ControlEvent::Started,
            )],
        });
        let system = ControlSystem::without_idle_timeout(store, Arc::new(NoopPublisher));

        let listed = system.list();
        let Err(Error::Control(ControlError::CorruptHistory(message))) = listed else {
            panic!("the illegal stream must be reported as corrupt: {listed:?}");
        };
        assert!(
            message.contains(id.as_str()),
            "the message must name the task: {message}"
        );
    }
}

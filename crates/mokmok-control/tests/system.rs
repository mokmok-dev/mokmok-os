//! The actor runtime: durability, serialization, listing, and retirement.

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex, PoisonError};
    use std::thread;
    use std::time::{Duration, Instant};

    use mokmok_control::{
        AgentUpdate, CloudEvent, ControlCommand, ControlError, ControlEvent, ControlPublisher,
        ControlSystem, EnumerableStore, Error, EventStore, NewTask, NoopPublisher, StoreError,
        TaskId, TaskSpec, TaskStatus, Version,
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

    fn message(text: &str) -> AgentUpdate {
        AgentUpdate::Message {
            text: text.to_owned(),
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
            let id = system.submit(new_task("scripted"))?;
            system.ask(&id, ControlCommand::Start)?;
            system.ask(
                &id,
                ControlCommand::OpenSession {
                    session_id: "session-1".to_owned(),
                },
            )?;
            system.ask(
                &id,
                ControlCommand::Update {
                    update: message("working"),
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
        assert_eq!(state.updates, vec![message("working")]);
        assert_eq!(state.update_count, 1);

        let summaries = system.list()?;
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].id, id);
        assert_eq!(summaries[0].status, TaskStatus::Completed);
        assert_eq!(summaries[0].updates, 1);
        Ok(())
    }

    #[test]
    fn two_threads_append_one_contiguous_stream() -> Result<(), Box<dyn std::error::Error>> {
        const PER_THREAD: usize = 20;
        let store = memory();
        let system = ControlSystem::without_idle_timeout(store.clone(), Arc::new(NoopPublisher));
        let id = system.submit(new_task("scripted"))?;
        system.ask(&id, ControlCommand::Start)?;

        let mut handles = Vec::new();
        for worker in 0..2 {
            let system = system.clone();
            let id = id.clone();
            handles.push(thread::spawn(move || -> Result<(), Error> {
                for index in 0..PER_THREAD {
                    system.ask(
                        &id,
                        ControlCommand::Update {
                            update: message(&format!("{worker}-{index}")),
                        },
                    )?;
                }
                Ok(())
            }));
        }
        for handle in handles {
            handle.join().expect("the asking thread did not panic")?;
        }

        let state = system.state(&id)?;
        assert_eq!(state.updates.len(), 2 * PER_THREAD);
        assert_eq!(state.update_count, u64::try_from(2 * PER_THREAD)?);

        let records = store.load(&id)?;
        assert_eq!(records.len(), 2 + 2 * PER_THREAD);
        let mut version = Version::ZERO;
        for record in &records {
            version = version.next();
            assert_eq!(record.version, version);
            assert_eq!(record.subject, id.as_str());
        }
        Ok(())
    }

    #[test]
    fn list_summarises_every_task_sorted_by_id() -> Result<(), Box<dyn std::error::Error>> {
        let system = ControlSystem::without_idle_timeout(memory(), Arc::new(NoopPublisher));
        let done = system.submit(new_task("scripted"))?;
        let waiting = system.submit(new_task("scripted"))?;
        let running = system.submit(new_task("other"))?;
        system.ask(&done, ControlCommand::Start)?;
        system.ask(
            &done,
            ControlCommand::Complete {
                summary: "done".to_owned(),
            },
        )?;
        system.ask(&running, ControlCommand::Start)?;
        system.ask(
            &running,
            ControlCommand::Update {
                update: message("one"),
            },
        )?;

        let summaries = system.list()?;
        assert_eq!(summaries.len(), 3);

        let mut expected = vec![done.clone(), waiting.clone(), running.clone()];
        expected.sort();
        let listed: Vec<TaskId> = summaries.iter().map(|summary| summary.id.clone()).collect();
        assert_eq!(listed, expected);

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
            Some((TaskStatus::Running, "other".to_owned(), 1))
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
        let id = system.submit(new_task("scripted"))?;
        system.ask(&id, ControlCommand::Start)?;

        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            thread::sleep(Duration::from_millis(5));
            system.ask(
                &id,
                ControlCommand::Update {
                    update: message("tick"),
                },
            )?;
            if store.loads() > 1 {
                break;
            }
            assert!(Instant::now() < deadline, "the actor never retired");
        }

        assert_eq!(system.state(&id)?.status, TaskStatus::Running);
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
    fn the_publisher_sees_each_batch_before_the_caller_is_answered()
    -> Result<(), Box<dyn std::error::Error>> {
        let publisher = Arc::new(RecordingPublisher::default());
        let system = ControlSystem::without_idle_timeout(memory(), publisher.clone());
        let id = system.submit(new_task("scripted"))?;
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

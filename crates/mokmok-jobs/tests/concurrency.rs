use std::num::NonZeroU32;
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

use mokmok_jobs::{Error, EventStore, JobCommand, JobError, JobEvent, JobId, JobState, JobSystem};
use mokmok_jobs_memory::{MemoryEventStore, RecordingPublisher};

const WORKERS: usize = 8;

#[test]
fn concurrent_starts_admit_one_winner() -> Result<(), Box<dyn std::error::Error>> {
    let store: Arc<dyn EventStore<JobId, JobEvent>> = Arc::new(MemoryEventStore::new());
    let system = JobSystem::new(Arc::clone(&store), Arc::new(RecordingPublisher::new()));
    let id = JobId::parse("concurrent")?;
    system.ask(
        &id,
        JobCommand::Submit {
            input: b"payload".to_vec(),
            max_attempts: NonZeroU32::new(1).ok_or("attempts must be nonzero")?,
        },
    )?;

    let barrier = Arc::new(Barrier::new(WORKERS));
    let mut handles = Vec::with_capacity(WORKERS);
    for _ in 0..WORKERS {
        let system = system.clone();
        let id = id.clone();
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            barrier.wait();
            system.ask(&id, JobCommand::Start)
        }));
    }

    let mut results = Vec::with_capacity(WORKERS);
    for handle in handles {
        if let Ok(result) = handle.join() {
            results.push(result);
        }
    }

    assert_eq!(results.len(), WORKERS);
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    for result in &results {
        if result.is_err() {
            assert!(matches!(result, Err(Error::Job(JobError::NotQueued))));
        }
    }
    assert!(matches!(
        system.state(&id)?,
        JobState::Running { attempt: 1, .. }
    ));
    assert_eq!(store.load(&id)?.len(), 2);
    Ok(())
}

#[test]
fn expired_actors_still_answer_from_the_log() -> Result<(), Box<dyn std::error::Error>> {
    let store: Arc<dyn EventStore<JobId, JobEvent>> = Arc::new(MemoryEventStore::new());
    let system = JobSystem::with_idle_timeout(
        Arc::clone(&store),
        Arc::new(RecordingPublisher::new()),
        Duration::ZERO,
    );
    let id = JobId::parse("expired")?;
    system.ask(
        &id,
        JobCommand::Submit {
            input: b"payload".to_vec(),
            max_attempts: NonZeroU32::new(1).ok_or("attempts must be nonzero")?,
        },
    )?;

    let barrier = Arc::new(Barrier::new(WORKERS));
    let mut handles = Vec::with_capacity(WORKERS);
    for _ in 0..WORKERS {
        let system = system.clone();
        let id = id.clone();
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            barrier.wait();
            system.ask(&id, JobCommand::Start)
        }));
    }

    let mut results = Vec::with_capacity(WORKERS);
    for handle in handles {
        if let Ok(result) = handle.join() {
            results.push(result);
        }
    }

    assert_eq!(results.len(), WORKERS);
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    for result in &results {
        if result.is_err() {
            assert!(matches!(result, Err(Error::Job(JobError::NotQueued))));
        }
    }
    assert_eq!(store.load(&id)?.len(), 2);
    assert!(matches!(
        system.state(&id)?,
        JobState::Running { attempt: 1, .. }
    ));
    Ok(())
}

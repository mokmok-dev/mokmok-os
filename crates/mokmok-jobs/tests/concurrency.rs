use std::num::NonZeroU32;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Barrier, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use mokmok_jobs::{
    CloudEvent, Error, EventStore, JobCommand, JobError, JobEvent, JobId, JobState, JobSystem,
    StoreError, Version,
};
use mokmok_jobs_jsonl::JsonlEventStore;
use mokmok_jobs_memory::{MemoryEventStore, RecordingPublisher};

const WORKERS: usize = 8;

fn attempts() -> Result<NonZeroU32, Box<dyn std::error::Error>> {
    NonZeroU32::new(1).ok_or_else(|| "attempts must be nonzero".into())
}

fn submit(input: &[u8]) -> Result<JobCommand, Box<dyn std::error::Error>> {
    Ok(JobCommand::Submit {
        input: input.to_vec(),
        max_attempts: attempts()?,
    })
}

fn submitted() -> Result<JobEvent, Box<dyn std::error::Error>> {
    Ok(JobEvent::Submitted {
        input: b"payload".to_vec(),
        max_attempts: attempts()?,
    })
}

#[test]
fn concurrent_starts_admit_one_winner() -> Result<(), Box<dyn std::error::Error>> {
    let store: Arc<dyn EventStore<JobId, JobEvent>> = Arc::new(MemoryEventStore::new());
    let system = JobSystem::new(Arc::clone(&store), Arc::new(RecordingPublisher::new()));
    let id = JobId::from_u128(1);
    system.ask(&id, submit(b"payload")?)?;

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
    let id = JobId::from_u128(2);
    system.ask(&id, submit(b"payload")?)?;

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

#[test]
fn parallel_submits_on_distinct_jsonl_streams_all_commit() -> Result<(), Box<dyn std::error::Error>>
{
    let directory = tempfile::tempdir()?;
    let store: Arc<dyn EventStore<JobId, JobEvent>> =
        Arc::new(JsonlEventStore::open(directory.path().join("jobs"))?);
    let system = JobSystem::new(Arc::clone(&store), Arc::new(RecordingPublisher::new()));
    let barrier = Arc::new(Barrier::new(WORKERS));
    let mut handles = Vec::with_capacity(WORKERS);
    for n in 0..WORKERS {
        let system = system.clone();
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            barrier.wait();
            let id = JobId::from_u128(100 + u128::try_from(n).expect("worker fits"));
            system.ask(&id, submit(b"payload").expect("attempts are nonzero"))
        }));
    }

    let mut results = Vec::with_capacity(WORKERS);
    for handle in handles {
        results.push(handle.join().expect("submit thread"));
    }
    assert_eq!(
        results.iter().filter(|result| result.is_ok()).count(),
        WORKERS
    );
    for n in 0..WORKERS {
        let id = JobId::from_u128(100 + u128::try_from(n)?);
        assert_eq!(store.load(&id)?.len(), 1);
    }
    Ok(())
}

#[test]
fn concurrent_appends_to_one_jsonl_stream_conflict() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let store = Arc::new(JsonlEventStore::<JobId, JobEvent>::open(
        directory.path().join("jobs"),
    )?);
    let id = JobId::from_u128(3);
    store.append(
        &id,
        Version::ZERO,
        &[CloudEvent::new(id.as_str(), Version::new(1), submitted()?)],
    )?;

    let barrier = Arc::new(Barrier::new(WORKERS));
    let mut handles = Vec::with_capacity(WORKERS);
    for _ in 0..WORKERS {
        let store = Arc::clone(&store);
        let id = id.clone();
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            barrier.wait();
            store.append(
                &id,
                Version::new(1),
                &[CloudEvent::new(
                    id.as_str(),
                    Version::new(2),
                    JobEvent::Started { attempt: 1 },
                )],
            )
        }));
    }

    let mut results = Vec::with_capacity(WORKERS);
    for handle in handles {
        results.push(handle.join().expect("append thread"));
    }
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    for result in &results {
        if result.is_err() {
            assert!(matches!(
                result,
                Err(StoreError::Conflict {
                    expected,
                    actual,
                }) if *expected == Version::new(1) && *actual == Version::new(2)
            ));
        }
    }
    assert_eq!(store.load(&id)?.len(), 2);
    Ok(())
}

/// Blocks one job's `append` until the test releases it, the way a slow disk on that
/// stream does. Every other stream goes straight through.
struct StallingStore {
    inner: JsonlEventStore<JobId, JobEvent>,
    stalled: JobId,
    armed: AtomicBool,
    entered: Sender<()>,
    release: Mutex<Receiver<()>>,
}

impl EventStore<JobId, JobEvent> for StallingStore {
    fn load(
        &self,
        id: &JobId,
    ) -> Result<Vec<CloudEvent<JobEvent>>, StoreError> {
        self.inner.load(id)
    }

    fn append(
        &self,
        id: &JobId,
        expected: Version,
        events: &[CloudEvent<JobEvent>],
    ) -> Result<Version, StoreError> {
        if id == &self.stalled && self.armed.load(Ordering::SeqCst) {
            let _ = self.entered.send(());
            let _ = self
                .release
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .recv();
        }
        self.inner.append(id, expected, events)
    }
}

#[test]
fn a_stuck_jsonl_submit_does_not_block_another_job() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let stuck = JobId::from_u128(4);
    let free = JobId::from_u128(5);
    let (entered, entered_rx) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    let stalling = Arc::new(StallingStore {
        inner: JsonlEventStore::open(directory.path().join("jobs"))?,
        stalled: stuck.clone(),
        armed: AtomicBool::new(false),
        entered,
        release: Mutex::new(release_rx),
    });
    let store: Arc<dyn EventStore<JobId, JobEvent>> = Arc::clone(&stalling) as _;
    let system =
        JobSystem::without_idle_timeout(Arc::clone(&store), Arc::new(RecordingPublisher::new()));
    system.ask(&stuck, submit(b"payload")?)?;
    // The stall only applies once the job exists, so the submit above lands normally.
    stalling.armed.store(true, Ordering::SeqCst);

    let stalled = {
        let system = system.clone();
        thread::spawn(move || system.ask(&stuck, JobCommand::Start))
    };
    entered_rx.recv_timeout(Duration::from_secs(1))?;

    let started = Instant::now();
    system.ask(&free, submit(b"payload")?)?;
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "an unrelated submit must finish while another stream's append is stuck"
    );
    assert_eq!(store.load(&free)?.len(), 1);

    release.send(())?;
    let _ = stalled.join().expect("stalled start thread");
    Ok(())
}

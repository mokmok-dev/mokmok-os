use std::num::NonZeroU32;
use std::sync::{Arc, Barrier};
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

#[cfg(unix)]
#[test]
fn a_stuck_jsonl_submit_does_not_block_another_job() -> Result<(), Box<dyn std::error::Error>> {
    use std::fs::{self, File};

    let directory = tempfile::tempdir()?;
    let root = directory.path().join("jobs");
    let store: Arc<JsonlEventStore<JobId, JobEvent>> = Arc::new(JsonlEventStore::open(&root)?);
    let system = JobSystem::without_idle_timeout(
        Arc::clone(&store) as Arc<dyn EventStore<JobId, JobEvent>>,
        Arc::new(RecordingPublisher::new()),
    );
    let stuck = JobId::from_u128(4);
    let free = JobId::from_u128(5);
    system.ask(&stuck, submit(b"payload")?)?;

    let path = root.join(format!("{}.jsonl", stuck.as_str()));
    fs::remove_file(&path)?;
    make_fifo(&path)?;

    let stalled = {
        let system = system.clone();
        thread::spawn(move || system.ask(&stuck, JobCommand::Start))
    };
    thread::sleep(Duration::from_millis(100));
    assert!(
        !stalled.is_finished(),
        "the stalled start returned before the sibling submit ran"
    );

    let started = Instant::now();
    system.ask(&free, submit(b"payload")?)?;
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "an unrelated submit must finish while another stream is still in open()"
    );
    assert_eq!(store.load(&free)?.len(), 1);

    let _reader = File::open(&path)?;
    wait_until_finished(&stalled, Duration::from_secs(1))?;
    let _ = stalled.join().expect("stalled start thread");
    Ok(())
}

#[cfg(unix)]
fn make_fifo(path: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    let status = std::process::Command::new("mkfifo").arg(path).status()?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("mkfifo failed: {status}").into())
    }
}

#[cfg(unix)]
fn wait_until_finished<T>(
    handle: &thread::JoinHandle<T>,
    budget: Duration,
) -> Result<(), Box<dyn std::error::Error>> {
    let started = Instant::now();
    while !handle.is_finished() {
        if started.elapsed() >= budget {
            return Err("stalled work did not unblock after a reader opened the fifo".into());
        }
        thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}

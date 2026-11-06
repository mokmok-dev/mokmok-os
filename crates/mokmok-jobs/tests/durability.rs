use std::fs::OpenOptions;
use std::io::Write;
use std::num::NonZeroU32;
use std::sync::Arc;

use mokmok_jobs::{
    Error, EventStore, JobCommand, JobError, JobEvent, JobId, JobState, JobSystem, StoreError,
    Version,
};
use mokmok_jobs_jsonl::JsonlEventStore;
use mokmok_jobs_memory::RecordingPublisher;

type Store = JsonlEventStore<JobId, JobEvent>;

fn attempts(count: u32) -> Result<NonZeroU32, Box<dyn std::error::Error>> {
    NonZeroU32::new(count).ok_or_else(|| "attempts must be nonzero".into())
}

fn submitted(max_attempts: u32) -> Result<JobEvent, Box<dyn std::error::Error>> {
    Ok(JobEvent::Submitted {
        input: b"payload".to_vec(),
        max_attempts: attempts(max_attempts)?,
    })
}

#[test]
fn durable_log_replays_after_reopen() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let root = directory.path().join("jobs");
    let id = JobId::from_u128(1);
    {
        let system = JobSystem::new(
            Arc::new(Store::open(&root)?),
            Arc::new(RecordingPublisher::new()),
        );
        system.ask(
            &id,
            JobCommand::Submit {
                input: b"payload".to_vec(),
                max_attempts: attempts(1)?,
            },
        )?;
        system.ask(&id, JobCommand::Start)?;
        system.ask(
            &id,
            JobCommand::Succeed {
                output: b"done".to_vec(),
            },
        )?;
    }

    let system = JobSystem::new(
        Arc::new(Store::open(&root)?),
        Arc::new(RecordingPublisher::new()),
    );
    assert!(matches!(
        system.state(&id)?,
        JobState::Succeeded { output } if output.as_slice() == b"done"
    ));
    let duplicate = system.ask(
        &id,
        JobCommand::Submit {
            input: b"again".to_vec(),
            max_attempts: attempts(1)?,
        },
    );
    assert!(matches!(duplicate, Err(Error::Job(JobError::Terminal))));
    Ok(())
}

#[test]
fn stale_actor_is_fenced_then_recovers() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let root = directory.path().join("jobs");
    let store: Arc<dyn EventStore<JobId, JobEvent>> = Arc::new(Store::open(&root)?);
    let first = JobSystem::new(Arc::clone(&store), Arc::new(RecordingPublisher::new()));
    let second = JobSystem::new(Arc::clone(&store), Arc::new(RecordingPublisher::new()));
    let id = JobId::from_u128(2);

    first.ask(
        &id,
        JobCommand::Submit {
            input: b"payload".to_vec(),
            max_attempts: attempts(1)?,
        },
    )?;
    first.ask(&id, JobCommand::Start)?;
    second.ask(
        &id,
        JobCommand::Succeed {
            output: b"done".to_vec(),
        },
    )?;

    let fenced = first.ask(
        &id,
        JobCommand::Cancel {
            reason: "too late".to_owned(),
        },
    );
    assert!(matches!(
        fenced,
        Err(Error::Store(StoreError::Conflict {
            expected,
            actual,
        })) if expected == Version::new(2) && actual == Version::new(3)
    ));
    let recovered = first.ask(
        &id,
        JobCommand::Cancel {
            reason: "too late".to_owned(),
        },
    );
    assert!(matches!(recovered, Err(Error::Job(JobError::Terminal))));
    Ok(())
}

#[test]
fn a_torn_tail_in_one_stream_does_not_affect_another() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let root = directory.path().join("jobs");
    let first = JobId::from_u128(3);
    let second = JobId::from_u128(4);
    {
        let store = Store::open(&root)?;
        store.append(&first, Version::ZERO, &[submitted(1)?])?;
    }
    {
        let path = std::fs::read_dir(&root)?
            .next()
            .ok_or("no stream file".to_owned())??
            .path();
        let mut file = OpenOptions::new().append(true).open(path)?;
        file.write_all(format!("{{\"stream\":\"{}\",", first.as_str()).as_bytes())?;
    }

    let store = Store::open(&root)?;
    store.append(&second, Version::ZERO, &[submitted(1)?])?;
    assert_eq!(store.load(&first)?.len(), 1);
    assert_eq!(store.load(&second)?.len(), 1);
    Ok(())
}

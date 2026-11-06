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
    let path = directory.path().join("jobs.jsonl");
    let id = JobId::parse("durable")?;
    {
        let system = JobSystem::new(
            Arc::new(JsonlEventStore::open(&path)?),
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
        Arc::new(JsonlEventStore::open(&path)?),
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
    let path = directory.path().join("jobs.jsonl");
    let store: Arc<dyn EventStore> = Arc::new(JsonlEventStore::open(&path)?);
    let first = JobSystem::new(Arc::clone(&store), Arc::new(RecordingPublisher::new()));
    let second = JobSystem::new(Arc::clone(&store), Arc::new(RecordingPublisher::new()));
    let id = JobId::parse("fenced")?;

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
fn torn_tail_is_discarded_on_open() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("jobs.jsonl");
    let first = JobId::parse("first")?;
    let second = JobId::parse("second")?;
    {
        let store = JsonlEventStore::open(&path)?;
        store.append(&first, Version::ZERO, &[submitted(1)?])?;
    }
    {
        let mut file = OpenOptions::new().append(true).open(&path)?;
        file.write_all(b"{\"kind\":\"submitted\",")?;
    }

    let store = JsonlEventStore::open(&path)?;
    store.append(&second, Version::ZERO, &[submitted(1)?])?;
    assert_eq!(store.load(&first)?.len(), 1);
    assert_eq!(store.load(&second)?.len(), 1);
    Ok(())
}

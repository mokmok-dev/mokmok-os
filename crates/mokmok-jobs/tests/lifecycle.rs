use std::num::NonZeroU32;
use std::sync::Arc;

use mokmok_jobs::{
    Envelope, Error, EventPublisher, EventStore, JobCommand, JobError, JobEvent, JobId, JobState,
    JobSystem, StoreError, Version,
};
use mokmok_jobs_memory::{MemoryEventStore, RecordingPublisher};

fn attempts(count: u32) -> Result<NonZeroU32, Box<dyn std::error::Error>> {
    NonZeroU32::new(count).ok_or_else(|| "attempts must be nonzero".into())
}

#[test]
fn retryable_failure_requeues_then_succeeds() -> Result<(), Box<dyn std::error::Error>> {
    let store: Arc<dyn EventStore> = Arc::new(MemoryEventStore::new());
    let system = JobSystem::new(Arc::clone(&store), Arc::new(RecordingPublisher::new()));
    let id = JobId::parse("retryable")?;

    let submitted = system.ask(
        &id,
        JobCommand::Submit {
            input: b"payload".to_vec(),
            max_attempts: attempts(2)?,
        },
    )?;
    assert_eq!(submitted.version, Version::new(1));
    let started = system.ask(&id, JobCommand::Start)?;
    assert_eq!(started.version, Version::new(2));
    let retried = system.ask(
        &id,
        JobCommand::Fail {
            reason: "flaky".to_owned(),
            retryable: true,
        },
    )?;
    assert_eq!(retried.version, Version::new(3));
    assert!(matches!(
        retried.events.as_slice(),
        [Envelope {
            event: JobEvent::RetryScheduled {
                attempt: 2,
                reason,
            },
            ..
        }] if reason == "flaky"
    ));
    assert!(matches!(
        system.state(&id)?,
        JobState::Queued { attempt: 2, .. }
    ));

    let restarted = system.ask(&id, JobCommand::Start)?;
    assert_eq!(restarted.version, Version::new(4));
    let succeeded = system.ask(
        &id,
        JobCommand::Succeed {
            output: b"done".to_vec(),
        },
    )?;
    assert_eq!(succeeded.version, Version::new(5));

    let versions: Vec<u64> = store
        .load(&id)?
        .iter()
        .map(|envelope| envelope.version.get())
        .collect();
    assert_eq!(versions, vec![1, 2, 3, 4, 5]);
    assert!(matches!(
        system.state(&id)?,
        JobState::Succeeded { output } if output.as_slice() == b"done"
    ));
    Ok(())
}

#[test]
fn terminal_state_rejects_cancel_without_appending() -> Result<(), Box<dyn std::error::Error>> {
    let store: Arc<dyn EventStore> = Arc::new(MemoryEventStore::new());
    let system = JobSystem::new(Arc::clone(&store), Arc::new(RecordingPublisher::new()));
    let id = JobId::parse("terminal")?;
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

    let before = store.load(&id)?.len();
    let rejected = system.ask(
        &id,
        JobCommand::Cancel {
            reason: "too late".to_owned(),
        },
    );
    assert!(matches!(rejected, Err(Error::Job(JobError::Terminal))));
    assert_eq!(store.load(&id)?.len(), before);
    Ok(())
}

#[test]
fn errors_carry_their_message_and_source() {
    let job = Error::from(JobError::Terminal);
    assert!(job.to_string().contains("terminal"));
    assert!(std::error::Error::source(&job).is_some());

    let store = Error::from(StoreError::Io("disk".to_owned()));
    assert!(store.to_string().contains("disk"));
    assert!(std::error::Error::source(&store).is_some());

    let unavailable = Error::ActorUnavailable;
    assert!(unavailable.to_string().contains("unavailable"));
    assert!(std::error::Error::source(&unavailable).is_none());

    let spawn = Error::Spawn("nope".to_owned());
    assert!(spawn.to_string().contains("nope"));
    assert!(std::error::Error::source(&spawn).is_none());
}

#[test]
fn rejected_command_appends_nothing_and_publishes_nothing() -> Result<(), Box<dyn std::error::Error>>
{
    let store: Arc<dyn EventStore> = Arc::new(MemoryEventStore::new());
    let publisher = Arc::new(RecordingPublisher::new());
    let event_sink: Arc<dyn EventPublisher> = publisher.clone();
    let system = JobSystem::new(Arc::clone(&store), event_sink);
    let id = JobId::parse("rejected")?;

    let rejected = system.ask(&id, JobCommand::Start);
    assert!(matches!(rejected, Err(Error::Job(JobError::NotSubmitted))));
    assert_eq!(store.load(&id)?.len(), 0);
    assert_eq!(publisher.len(), 0);
    Ok(())
}

#[test]
fn publisher_receives_every_committed_event_in_version_order()
-> Result<(), Box<dyn std::error::Error>> {
    let store: Arc<dyn EventStore> = Arc::new(MemoryEventStore::new());
    let publisher = Arc::new(RecordingPublisher::new());
    let event_sink: Arc<dyn EventPublisher> = publisher.clone();
    let system = JobSystem::new(Arc::clone(&store), event_sink);
    let id = JobId::parse("publisher")?;

    system.ask(
        &id,
        JobCommand::Submit {
            input: b"payload".to_vec(),
            max_attempts: attempts(2)?,
        },
    )?;
    system.ask(&id, JobCommand::Start)?;
    system.ask(
        &id,
        JobCommand::Fail {
            reason: "flaky".to_owned(),
            retryable: true,
        },
    )?;
    system.ask(&id, JobCommand::Start)?;
    system.ask(
        &id,
        JobCommand::Succeed {
            output: b"done".to_vec(),
        },
    )?;

    let recorded = publisher.published();
    assert_eq!(recorded, store.load(&id)?);
    let versions: Vec<u64> = recorded
        .iter()
        .map(|envelope| envelope.version.get())
        .collect();
    assert_eq!(versions, vec![1, 2, 3, 4, 5]);
    assert_eq!(publisher.len(), 5);
    Ok(())
}

//! The actor registry that serializes commands per job.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::thread::{self, JoinHandle};

use mokmok_jobs_core::{
    Envelope, EventPublisher, EventStore, JobCommand, JobError, JobId, JobState, StoreError,
    Version, decide, evolve,
};

/// A handle to the job actors, cheap to clone and share across threads.
#[derive(Clone)]
pub struct JobSystem {
    inner: Arc<Inner>,
}

struct Inner {
    store: Arc<dyn EventStore>,
    publisher: Arc<dyn EventPublisher>,
    actors: Mutex<HashMap<JobId, Arc<Actor>>>,
}

struct Actor {
    tx: mpsc::Sender<Request>,
    // Taken by `Inner`'s drop, which must join only after every sender is closed.
    join: Mutex<Option<JoinHandle<()>>>,
}

struct Request {
    command: JobCommand,
    reply: mpsc::Sender<Result<Appended, Error>>,
}

enum Dispatch {
    Reply(Result<Appended, Error>),
    Gone(JobCommand),
}

/// The events a command committed, with the stream version they closed at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Appended {
    pub version: Version,
    pub events: Vec<Envelope>,
}

#[derive(Debug)]
pub enum Error {
    Job(JobError),
    Store(StoreError),
    ActorUnavailable,
    Spawn(String),
}

impl fmt::Display for Error {
    fn fmt(
        &self,
        formatter: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        match self {
            Self::Job(error) => write!(formatter, "job: {error}"),
            Self::Store(error) => write!(formatter, "store: {error}"),
            Self::ActorUnavailable => formatter.write_str("job actor is unavailable"),
            Self::Spawn(message) => write!(formatter, "cannot spawn job actor: {message}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Job(error) => Some(error),
            Self::Store(error) => Some(error),
            Self::ActorUnavailable | Self::Spawn(_) => None,
        }
    }
}

impl From<JobError> for Error {
    fn from(error: JobError) -> Self {
        Self::Job(error)
    }
}

impl From<StoreError> for Error {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}

impl JobSystem {
    #[must_use]
    pub fn new(
        store: Arc<dyn EventStore>,
        publisher: Arc<dyn EventPublisher>,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                store,
                publisher,
                actors: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// Sends one command to the job's actor and waits for its reply.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Job`] when the domain rejects the command, [`Error::Store`]
    /// when the append fails, and [`Error::ActorUnavailable`] or [`Error::Spawn`]
    /// when no actor can serve the command.
    pub fn ask(
        &self,
        id: &JobId,
        command: JobCommand,
    ) -> Result<Appended, Error> {
        let actor = self.actor(id)?;
        match dispatch(&actor, command) {
            Dispatch::Reply(reply) => reply,
            Dispatch::Gone(command) => {
                self.forget(id, &actor);
                let fresh = self.actor(id)?;
                match dispatch(&fresh, command) {
                    Dispatch::Reply(reply) => reply,
                    Dispatch::Gone(_) => Err(Error::ActorUnavailable),
                }
            },
        }
    }

    /// Reads the state straight from the store, bypassing the actor.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when the log cannot be read and [`Error::Job`] when
    /// the log does not replay.
    pub fn state(
        &self,
        id: &JobId,
    ) -> Result<JobState, Error> {
        let envelopes = self.inner.store.load(id).map_err(Error::Store)?;
        JobState::replay(&envelopes).map_err(Error::Job)
    }

    fn actor(
        &self,
        id: &JobId,
    ) -> Result<Arc<Actor>, Error> {
        if let Some(actor) = self.registered(id) {
            return Ok(actor);
        }
        let (state, version) = self.replay(id)?;
        let actor = self.spawn(id, state, version)?;
        let mut actors = self
            .inner
            .actors
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(registered) = actors.get(id) {
            // The loser's `Arc` drop closes its mailbox, so its thread leaves `recv`.
            return Ok(Arc::clone(registered));
        }
        actors.insert(id.clone(), Arc::clone(&actor));
        drop(actors);
        Ok(actor)
    }

    fn registered(
        &self,
        id: &JobId,
    ) -> Option<Arc<Actor>> {
        self.inner
            .actors
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(id)
            .cloned()
    }

    fn forget(
        &self,
        id: &JobId,
        actor: &Arc<Actor>,
    ) {
        let mut actors = self
            .inner
            .actors
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if actors
            .get(id)
            .is_some_and(|registered| Arc::ptr_eq(registered, actor))
        {
            actors.remove(id);
        }
    }

    fn replay(
        &self,
        id: &JobId,
    ) -> Result<(JobState, Version), Error> {
        let envelopes = self.inner.store.load(id).map_err(Error::Store)?;
        let state = JobState::replay(&envelopes).map_err(Error::Job)?;
        let version = envelopes
            .last()
            .map_or(Version::ZERO, |envelope| envelope.version);
        Ok((state, version))
    }

    fn spawn(
        &self,
        id: &JobId,
        state: JobState,
        version: Version,
    ) -> Result<Arc<Actor>, Error> {
        let (tx, requests) = mpsc::channel();
        let store = Arc::clone(&self.inner.store);
        let publisher = Arc::clone(&self.inner.publisher);
        let actor_id = id.clone();
        let handle = thread::Builder::new()
            .name(format!("job-actor-{}", id.as_str()))
            .spawn(move || run(store, publisher, actor_id, state, version, requests))
            .map_err(|error| Error::Spawn(error.to_string()))?;
        Ok(Arc::new(Actor {
            tx,
            join: Mutex::new(Some(handle)),
        }))
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        let mut registry = self.actors.lock().unwrap_or_else(PoisonError::into_inner);
        let drained: Vec<Arc<Actor>> = registry.drain().map(|(_, actor)| actor).collect();
        drop(registry);
        let mut handles = Vec::with_capacity(drained.len());
        for actor in &drained {
            let handle = actor
                .join
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take();
            if let Some(handle) = handle {
                handles.push(handle);
            }
        }
        // Every registry `Arc` dies here, so each actor's sender closes and its thread
        // returns from `recv`; joining before this point would wait for a live sender.
        drop(drained);
        for handle in handles {
            let _ = handle.join();
        }
    }
}

// The command comes back on a closed mailbox, so the retry resends the same value.
#[allow(clippy::needless_pass_by_value)]
fn dispatch(
    actor: &Actor,
    command: JobCommand,
) -> Dispatch {
    let (reply, wait) = mpsc::channel();
    match actor.tx.send(Request { command, reply }) {
        Ok(()) => Dispatch::Reply(wait.recv().unwrap_or(Err(Error::ActorUnavailable))),
        Err(mpsc::SendError(request)) => Dispatch::Gone(request.command),
    }
}

// By value is what makes the spawned closure `'static`; the actor thread owns both.
#[allow(clippy::needless_pass_by_value)]
fn run(
    store: Arc<dyn EventStore>,
    publisher: Arc<dyn EventPublisher>,
    id: JobId,
    mut state: JobState,
    mut version: Version,
    requests: mpsc::Receiver<Request>,
) {
    while let Ok(request) = requests.recv() {
        let result = handle(
            &*store,
            &*publisher,
            &id,
            &mut state,
            &mut version,
            request.command,
        );
        if matches!(&result, Err(Error::Store(StoreError::Conflict { .. }))) {
            // Closing the mailbox before replying makes the fence visible to the next
            // ask, which then respawns from the log instead of queueing behind it.
            drop(requests);
            let _ = request.reply.send(result);
            return;
        }
        if request.reply.send(result).is_err() {
            return;
        }
    }
}

fn handle(
    store: &dyn EventStore,
    publisher: &dyn EventPublisher,
    id: &JobId,
    state: &mut JobState,
    version: &mut Version,
    command: JobCommand,
) -> Result<Appended, Error> {
    let events = decide(state, command)?;
    if events.is_empty() {
        return Ok(Appended {
            version: *version,
            events: Vec::new(),
        });
    }
    let mut next_state = state.clone();
    for event in &events {
        evolve(&mut next_state, event)?;
    }
    let mut next_version = *version;
    let mut envelopes = Vec::with_capacity(events.len());
    for event in &events {
        next_version = next_version.next();
        envelopes.push(Envelope {
            job_id: id.clone(),
            version: next_version,
            event: event.clone(),
        });
    }
    let committed = store.append(id, *version, &events).map_err(Error::Store)?;
    publisher.publish(&envelopes);
    *state = next_state;
    *version = committed;
    Ok(Appended {
        version: committed,
        events: envelopes,
    })
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use mokmok_jobs_core::JobEvent;
    use mokmok_jobs_memory::{MemoryEventStore, RecordingPublisher};

    use super::*;

    #[derive(Default)]
    struct CountingStore {
        inner: MemoryEventStore,
        loads: AtomicUsize,
    }

    impl EventStore for CountingStore {
        fn load(
            &self,
            id: &JobId,
        ) -> Result<Vec<Envelope>, StoreError> {
            self.loads.fetch_add(1, Ordering::SeqCst);
            self.inner.load(id)
        }

        fn append(
            &self,
            id: &JobId,
            expected: Version,
            events: &[JobEvent],
        ) -> Result<Version, StoreError> {
            self.inner.append(id, expected, events)
        }
    }

    struct BlockingStore {
        inner: MemoryEventStore,
        entered: mpsc::Sender<()>,
        release: Mutex<mpsc::Receiver<()>>,
    }

    impl EventStore for BlockingStore {
        fn load(
            &self,
            id: &JobId,
        ) -> Result<Vec<Envelope>, StoreError> {
            self.inner.load(id)
        }

        fn append(
            &self,
            id: &JobId,
            expected: Version,
            events: &[JobEvent],
        ) -> Result<Version, StoreError> {
            let _ = self.entered.send(());
            let _ = self.release.lock().expect("release lock").recv();
            self.inner.append(id, expected, events)
        }
    }

    fn submit() -> JobCommand {
        JobCommand::Submit {
            input: b"in".to_vec(),
            max_attempts: NonZeroU32::new(1).expect("literal is nonzero"),
        }
    }

    #[test]
    fn a_cached_actor_serves_the_next_command_without_reloading() {
        let store = Arc::new(CountingStore::default());
        let event_store: Arc<dyn EventStore> = store.clone();
        let system = JobSystem::new(event_store, Arc::new(RecordingPublisher::new()));
        let id = JobId::parse("cached").expect("literal is valid");

        system.ask(&id, submit()).expect("submit commits");
        let loads = store.loads.load(Ordering::SeqCst);
        system.ask(&id, JobCommand::Start).expect("start commits");

        assert_eq!(
            store.loads.load(Ordering::SeqCst),
            loads,
            "the cached actor must not reload the stream"
        );
    }

    #[test]
    fn dropping_the_system_waits_for_an_in_flight_command() {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let store: Arc<dyn EventStore> = Arc::new(BlockingStore {
            inner: MemoryEventStore::new(),
            entered: entered_tx,
            release: Mutex::new(release_rx),
        });
        let system = JobSystem::new(store, Arc::new(RecordingPublisher::new()));
        let id = JobId::parse("blocking").expect("literal is valid");
        let actor = system.actor(&id).expect("actor spawns");
        let (reply, wait) = mpsc::channel();
        actor
            .tx
            .send(Request {
                command: submit(),
                reply,
            })
            .expect("actor accepts the command");
        drop(actor);
        entered_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("append started");

        let (drop_started, drop_wait) = mpsc::channel();
        let dropper = thread::spawn(move || {
            let _ = drop_started.send(());
            drop(system);
        });
        drop_wait
            .recv_timeout(Duration::from_secs(5))
            .expect("drop started");
        thread::sleep(Duration::from_millis(200));
        assert!(
            !dropper.is_finished(),
            "drop returned while a command was in flight"
        );

        release_tx.send(()).expect("append releases");
        dropper.join().expect("dropper did not panic");
        drop(wait);
    }
}

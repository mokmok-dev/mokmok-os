//! The actor registry that serializes commands per job.

use std::collections::HashMap;
use std::fmt;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use mokmok_jobs_core::{
    Envelope, EventPublisher, EventStore, JobCommand, JobError, JobEvent, JobId, JobState,
    StoreError, Version, decide, evolve,
};

/// The idle timeout [`JobSystem::new`] applies to every actor.
pub const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// A handle to the job actors, cheap to clone and share across threads.
#[derive(Clone)]
pub struct JobSystem {
    inner: Arc<Inner>,
}

struct Registry {
    entries: Mutex<HashMap<JobId, Arc<Actor>>>,
    // A replay holds this until its actor is registered. A retirement takes it before
    // removing its entry, so the actor a replay replaces cannot commit and retire
    // between that load and the spawn, leaving the replay behind the log.
    lifecycle: Mutex<()>,
}

struct Inner {
    store: Arc<dyn EventStore<JobId, JobEvent>>,
    publisher: Arc<dyn EventPublisher>,
    registry: Arc<Registry>,
    idle: Option<Duration>,
}

struct Actor {
    tx: Sender<Request>,
    // Taken by `Inner`'s drop, which must join only after every sender is closed.
    join: Mutex<Option<JoinHandle<()>>>,
}

// `Weak<Registry>` and not `Weak<Inner>`: an `Arc<Inner>` upgraded on the retirement
// path would drop on the actor's own thread, running `Inner::drop` there, and that
// drop joins the actor thread.
struct Worker {
    registry: Weak<Registry>,
    store: Arc<dyn EventStore<JobId, JobEvent>>,
    publisher: Arc<dyn EventPublisher>,
    id: JobId,
    idle: Option<Duration>,
    state: JobState,
    version: Version,
}

struct Request {
    command: JobCommand,
    reply: Sender<Result<Appended, Error>>,
}

enum Dispatch {
    Reply(Receiver<Result<Appended, Error>>),
    Gone(JobCommand),
}

/// The events a command committed, with the stream version they closed at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Appended {
    pub version: Version,
    pub events: Vec<Envelope<JobId, JobEvent>>,
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
    /// Builds a system that retires each actor after [`DEFAULT_IDLE_TIMEOUT`].
    ///
    /// See [`JobSystem::with_idle_timeout`] for what retirement does, and
    /// [`JobSystem::without_idle_timeout`] to opt out.
    #[must_use]
    pub fn new(
        store: Arc<dyn EventStore<JobId, JobEvent>>,
        publisher: Arc<dyn EventPublisher>,
    ) -> Self {
        Self::build(store, publisher, Some(DEFAULT_IDLE_TIMEOUT))
    }

    /// Builds a system that keeps each actor for as long as the system lives.
    ///
    /// For callers whose commands for a job are spaced far apart, or whose tests
    /// need an actor that stays registered.
    #[must_use]
    pub fn without_idle_timeout(
        store: Arc<dyn EventStore<JobId, JobEvent>>,
        publisher: Arc<dyn EventPublisher>,
    ) -> Self {
        Self::build(store, publisher, None)
    }

    /// Builds a system that offloads a job's actor after `idle` passes with no command.
    ///
    /// The actor's thread then exits and its registry entry is removed. The next
    /// command replays the job's log and spawns a fresh actor. [`Duration::ZERO`]
    /// retires an actor as soon as its mailbox is empty. [`JobSystem::new`] uses
    /// [`DEFAULT_IDLE_TIMEOUT`].
    #[must_use]
    pub fn with_idle_timeout(
        store: Arc<dyn EventStore<JobId, JobEvent>>,
        publisher: Arc<dyn EventPublisher>,
        idle: Duration,
    ) -> Self {
        Self::build(store, publisher, Some(idle))
    }

    fn build(
        store: Arc<dyn EventStore<JobId, JobEvent>>,
        publisher: Arc<dyn EventPublisher>,
        idle: Option<Duration>,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                store,
                publisher,
                registry: Arc::new(Registry {
                    entries: Mutex::new(HashMap::new()),
                    lifecycle: Mutex::new(()),
                }),
                idle,
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
        match self.deliver(id, command)? {
            Dispatch::Reply(reply) => reply.recv().unwrap_or(Err(Error::ActorUnavailable)),
            Dispatch::Gone(command) => match self.deliver(id, command)? {
                Dispatch::Reply(reply) => reply.recv().unwrap_or(Err(Error::ActorUnavailable)),
                Dispatch::Gone(_) => Err(Error::ActorUnavailable),
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

    // The registry lock orders a command send against retirement's final check: a send
    // under the guard either lands before `try_recv`, which then serves it in place, or
    // after the entry is gone, which sends the next command through `replay` and a
    // fresh actor. Neither side can drop it in between.
    fn deliver(
        &self,
        id: &JobId,
        command: JobCommand,
    ) -> Result<Dispatch, Error> {
        let mut registry = self
            .inner
            .registry
            .entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let command = match send_registered(&mut registry, id, command) {
            Ok(outcome) => return Ok(outcome),
            Err(command) => command,
        };
        drop(registry);
        let lifecycle = self
            .inner
            .registry
            .lifecycle
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let (state, version) = self.replay(id)?;
        let mut registry = self
            .inner
            .registry
            .entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let command = match send_registered(&mut registry, id, command) {
            Ok(outcome) => return Ok(outcome),
            Err(command) => command,
        };
        // Spawning, inserting, and sending before the guards are released keeps a zero
        // idle timeout from retiring the child before its first command is queued.
        let actor = self.spawn(id, state, version)?;
        let outcome = dispatch(&actor, command);
        if !matches!(outcome, Dispatch::Gone(_)) {
            registry.insert(id.clone(), actor);
        }
        drop(registry);
        drop(lifecycle);
        Ok(outcome)
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
        let worker = Worker {
            registry: Arc::downgrade(&self.inner.registry),
            store: Arc::clone(&self.inner.store),
            publisher: Arc::clone(&self.inner.publisher),
            id: id.clone(),
            idle: self.inner.idle,
            state,
            version,
        };
        let handle = thread::Builder::new()
            .name(format!("job-actor-{}", id.as_str()))
            .spawn(move || run(worker, requests))
            .map_err(|error| Error::Spawn(error.to_string()))?;
        Ok(Arc::new(Actor {
            tx,
            join: Mutex::new(Some(handle)),
        }))
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        let mut registry = self
            .registry
            .entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
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

// An absent entry hands the command back, so the caller can spawn an actor and send it
// without cloning. A closed mailbox leaves its entry behind, so remove it or the retry
// finds the same dead actor again.
fn send_registered(
    registry: &mut HashMap<JobId, Arc<Actor>>,
    id: &JobId,
    command: JobCommand,
) -> Result<Dispatch, JobCommand> {
    let Some(registered) = registry.get(id) else {
        return Err(command);
    };
    let actor = Arc::clone(registered);
    let outcome = dispatch(&actor, command);
    if matches!(outcome, Dispatch::Gone(_)) {
        registry.remove(id);
    }
    Ok(outcome)
}

// The command comes back on a closed mailbox, so the retry resends the same value.
#[allow(clippy::needless_pass_by_value)]
fn dispatch(
    actor: &Actor,
    command: JobCommand,
) -> Dispatch {
    let (reply, wait) = mpsc::channel();
    match actor.tx.send(Request { command, reply }) {
        Ok(()) => Dispatch::Reply(wait),
        Err(mpsc::SendError(request)) => Dispatch::Gone(request.command),
    }
}

// Removes the entry only after the mailbox is known empty, so a command sent under the
// guard is never dropped between that check and the removal.
fn retire(
    worker: &Worker,
    requests: &Receiver<Request>,
) -> Option<Request> {
    let registry = worker.registry.upgrade()?;
    let _lifecycle = registry
        .lifecycle
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let mut entries = registry
        .entries
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    if let Ok(request) = requests.try_recv() {
        return Some(request);
    }
    entries.remove(&worker.id);
    None
}

// By value is what makes the spawned closure `'static`; the actor thread owns both.
#[allow(clippy::needless_pass_by_value)]
fn run(
    mut worker: Worker,
    requests: Receiver<Request>,
) {
    loop {
        let request = match worker.idle {
            None => match requests.recv() {
                Ok(request) => request,
                Err(_) => return,
            },
            Some(idle) => match requests.recv_timeout(idle) {
                Ok(request) => request,
                Err(RecvTimeoutError::Disconnected) => return,
                Err(RecvTimeoutError::Timeout) => {
                    let Some(request) = retire(&worker, &requests) else {
                        return;
                    };
                    request
                },
            },
        };
        let result = handle(
            &*worker.store,
            &*worker.publisher,
            &worker.id,
            &mut worker.state,
            &mut worker.version,
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
    store: &dyn EventStore<JobId, JobEvent>,
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
            stream: id.clone(),
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
    use std::time::{Duration, Instant};

    use mokmok_jobs_core::JobEvent;
    use mokmok_jobs_memory::{MemoryEventStore, RecordingPublisher};

    use super::*;

    #[derive(Default)]
    struct CountingStore {
        inner: MemoryEventStore<JobId, JobEvent>,
        loads: AtomicUsize,
    }

    impl EventStore<JobId, JobEvent> for CountingStore {
        fn load(
            &self,
            id: &JobId,
        ) -> Result<Vec<Envelope<JobId, JobEvent>>, StoreError> {
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
        inner: MemoryEventStore<JobId, JobEvent>,
        entered: Sender<()>,
        release: Mutex<Receiver<()>>,
    }

    impl EventStore<JobId, JobEvent> for BlockingStore {
        fn load(
            &self,
            id: &JobId,
        ) -> Result<Vec<Envelope<JobId, JobEvent>>, StoreError> {
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

    fn blocked_store(
        entered: Sender<()>,
        release: Receiver<()>,
    ) -> Arc<dyn EventStore<JobId, JobEvent>> {
        Arc::new(BlockingStore {
            inner: MemoryEventStore::new(),
            entered,
            release: Mutex::new(release),
        })
    }

    #[test]
    fn a_cached_actor_serves_the_next_command_without_reloading() {
        let store = Arc::new(CountingStore::default());
        let event_store: Arc<dyn EventStore<JobId, JobEvent>> = store.clone();
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
    fn the_default_idle_timeout_is_sixty_seconds() {
        assert_eq!(DEFAULT_IDLE_TIMEOUT, Duration::from_secs(60));
    }

    #[test]
    fn without_idle_timeout_keeps_the_actor() {
        let store = Arc::new(CountingStore::default());
        let event_store: Arc<dyn EventStore<JobId, JobEvent>> = store.clone();
        let system =
            JobSystem::without_idle_timeout(event_store, Arc::new(RecordingPublisher::new()));
        let id = JobId::parse("resident").expect("literal is valid");

        system.ask(&id, submit()).expect("submit commits");
        let loads = store.loads.load(Ordering::SeqCst);
        // Longer than the 20ms the retirement test waits, shorter than the 60s default.
        thread::sleep(Duration::from_millis(50));
        system.ask(&id, JobCommand::Start).expect("start commits");

        assert_eq!(
            store.loads.load(Ordering::SeqCst),
            loads,
            "an opted-out actor must not retire"
        );
    }

    #[test]
    fn an_idle_actor_is_offloaded_and_the_next_command_replays() {
        let store = Arc::new(CountingStore::default());
        let event_store: Arc<dyn EventStore<JobId, JobEvent>> = store.clone();
        let system = JobSystem::with_idle_timeout(
            event_store,
            Arc::new(RecordingPublisher::new()),
            Duration::from_millis(20),
        );
        let id = JobId::parse("idle").expect("literal is valid");

        system.ask(&id, submit()).expect("submit commits");
        let loads = store.loads.load(Ordering::SeqCst);

        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let empty = system
                .inner
                .registry
                .entries
                .lock()
                .expect("registry lock")
                .is_empty();
            assert!(
                empty || Instant::now() < deadline,
                "actor was not offloaded"
            );
            if empty {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }

        let appended = system.ask(&id, JobCommand::Start).expect("start commits");
        assert_eq!(appended.version, Version::new(2));
        assert_eq!(store.loads.load(Ordering::SeqCst), loads + 1);
    }

    #[test]
    fn a_command_that_outlives_the_idle_deadline_still_answers() {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let system = JobSystem::with_idle_timeout(
            blocked_store(entered_tx, release_rx),
            Arc::new(RecordingPublisher::new()),
            Duration::from_millis(20),
        );
        let id = JobId::parse("slow").expect("literal is valid");

        let asking = thread::spawn(move || system.ask(&id, submit()));
        entered_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("append started");
        thread::sleep(Duration::from_millis(100));
        release_tx.send(()).expect("append releases");

        let appended = asking
            .join()
            .expect("asking thread did not panic")
            .expect("submit commits");
        assert_eq!(appended.version, Version::new(1));
    }

    #[test]
    fn dropping_the_system_waits_for_an_in_flight_command() {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let system = JobSystem::new(
            blocked_store(entered_tx, release_rx),
            Arc::new(RecordingPublisher::new()),
        );
        let id = JobId::parse("blocking").expect("literal is valid");
        let actor = system
            .spawn(&id, JobState::New, Version::ZERO)
            .expect("actor spawns");
        system
            .inner
            .registry
            .entries
            .lock()
            .expect("registry lock")
            .insert(id, Arc::clone(&actor));
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

        let dropping = system.clone();
        drop(system);
        let dropper = thread::spawn(move || drop(dropping));
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

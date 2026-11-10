//! The actor registry that serializes commands per task.

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime};

use mokmok_control_core::{
    CloudEvent, ControlCommand, ControlError, ControlEvent, EnumerableStore, StoreError, TaskId,
    TaskSpec, TaskState, TaskStatus, Version, decide, evolve,
};
use mokmok_journal::timestamp;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// The idle timeout [`ControlSystem::new`] applies to every actor.
pub const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(60);

const LIFECYCLE_STRIPES: usize = 64;

/// A task a caller wants to run, before it has an id.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewTask {
    pub title: String,
    pub instruction: String,
    pub agent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
}

/// One row of a task listing.
///
/// The row carries enough to place the task in a workspace and to seed its first
/// user turn, so a browser does not need to replay each task's stream to render it.
/// The instruction is a preview clipped to [`LISTING_INSTRUCTION_BYTES`] on a character
/// boundary plus an ellipsis; the task detail endpoint carries the full instruction.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskSummary {
    pub id: TaskId,
    pub title: String,
    pub instruction: String,
    pub instruction_truncated: bool,
    pub status: TaskStatus,
    pub agent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    pub updates: u64,
}

/// How much of a task's instruction a listing row carries before it is a preview.
const LISTING_INSTRUCTION_BYTES: usize = 4096;

fn listing_instruction(instruction: &str) -> (String, bool) {
    if instruction.len() <= LISTING_INSTRUCTION_BYTES {
        return (instruction.to_owned(), false);
    }
    let cut = instruction.floor_char_boundary(LISTING_INSTRUCTION_BYTES);
    (format!("{}…", &instruction[..cut]), true)
}

/// What gets every committed batch, so a live feed can follow the log.
pub trait ControlPublisher: Send + Sync {
    /// Called after the append succeeded and before the command's caller is answered.
    fn publish(
        &self,
        events: &[CloudEvent<ControlEvent>],
    );
}

/// A publisher for a system nobody is watching.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoopPublisher;

impl ControlPublisher for NoopPublisher {
    fn publish(
        &self,
        _events: &[CloudEvent<ControlEvent>],
    ) {
    }
}

/// A handle to the task actors, cheap to clone and share across threads.
#[derive(Clone)]
pub struct ControlSystem {
    inner: Arc<Inner>,
}

struct Registry {
    entries: Mutex<HashMap<TaskId, Arc<Actor>>>,
    // A replay holds the task's stripe until its actor is registered. A retirement takes
    // the same stripe before removing its entry, so the actor a replay replaces cannot
    // commit and retire between that load and the spawn, leaving the replay behind the log.
    // Striping keeps a slow load for one task from blocking a replay or retirement for
    // another.
    lifecycles: Vec<Mutex<()>>,
}

impl Registry {
    fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            lifecycles: (0..LIFECYCLE_STRIPES).map(|_| Mutex::new(())).collect(),
        }
    }

    fn lifecycle(
        &self,
        id: &TaskId,
    ) -> &Mutex<()> {
        let mut hasher = DefaultHasher::new();
        id.hash(&mut hasher);
        let index = usize::try_from(hasher.finish()).unwrap_or(usize::MAX) % self.lifecycles.len();
        &self.lifecycles[index]
    }
}

struct Inner {
    store: Arc<dyn EnumerableStore<TaskId, ControlEvent>>,
    publisher: Arc<dyn ControlPublisher>,
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
    store: Arc<dyn EnumerableStore<TaskId, ControlEvent>>,
    publisher: Arc<dyn ControlPublisher>,
    id: TaskId,
    idle: Option<Duration>,
    state: TaskState,
    version: Version,
}

struct Request {
    command: ControlCommand,
    reply: Sender<Result<Appended, Error>>,
}

enum Dispatch {
    Reply(Receiver<Result<Appended, Error>>),
    Gone(ControlCommand),
}

/// The events a command committed, with the stream version they closed at.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Appended {
    pub version: Version,
    pub events: Vec<CloudEvent<ControlEvent>>,
}

#[derive(Debug)]
pub enum Error {
    Control(ControlError),
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
            Self::Control(error) => write!(formatter, "task: {error}"),
            Self::Store(error) => write!(formatter, "store: {error}"),
            Self::ActorUnavailable => formatter.write_str("task actor is unavailable"),
            Self::Spawn(message) => write!(formatter, "cannot spawn task actor: {message}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Control(error) => Some(error),
            Self::Store(error) => Some(error),
            Self::ActorUnavailable | Self::Spawn(_) => None,
        }
    }
}

impl From<ControlError> for Error {
    fn from(error: ControlError) -> Self {
        Self::Control(error)
    }
}

impl From<StoreError> for Error {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}

impl ControlSystem {
    /// Builds a system that retires each actor after [`DEFAULT_IDLE_TIMEOUT`].
    ///
    /// Share one store instance per root across systems.
    ///
    /// See [`ControlSystem::with_idle_timeout`] for what retirement does, and
    /// [`ControlSystem::without_idle_timeout`] to opt out.
    #[must_use]
    pub fn new(
        store: Arc<dyn EnumerableStore<TaskId, ControlEvent>>,
        publisher: Arc<dyn ControlPublisher>,
    ) -> Self {
        Self::build(store, publisher, Some(DEFAULT_IDLE_TIMEOUT))
    }

    /// Builds a system that keeps each actor for as long as the system lives.
    ///
    /// For callers whose commands for a task are spaced far apart, or whose tests
    /// need an actor that stays registered.
    #[must_use]
    pub fn without_idle_timeout(
        store: Arc<dyn EnumerableStore<TaskId, ControlEvent>>,
        publisher: Arc<dyn ControlPublisher>,
    ) -> Self {
        Self::build(store, publisher, None)
    }

    /// Builds a system that offloads a task's actor after `idle` passes with no command.
    ///
    /// The actor's thread then exits and its registry entry is removed. The next
    /// command replays the task's log and spawns a fresh actor. [`Duration::ZERO`]
    /// retires an actor as soon as its mailbox is empty. [`ControlSystem::new`] uses
    /// [`DEFAULT_IDLE_TIMEOUT`].
    #[must_use]
    pub fn with_idle_timeout(
        store: Arc<dyn EnumerableStore<TaskId, ControlEvent>>,
        publisher: Arc<dyn ControlPublisher>,
        idle: Duration,
    ) -> Self {
        Self::build(store, publisher, Some(idle))
    }

    fn build(
        store: Arc<dyn EnumerableStore<TaskId, ControlEvent>>,
        publisher: Arc<dyn ControlPublisher>,
        idle: Option<Duration>,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                store,
                publisher,
                registry: Arc::new(Registry::new()),
                idle,
            }),
        }
    }

    /// Creates a task with a fresh `UUIDv7` id and submits it.
    ///
    /// The id is returned only after the [`ControlEvent::Submitted`] record is
    /// durable, so an id a caller holds always names a stream a restart can find.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Control`] when the spec fails validation, and every error
    /// [`ControlSystem::ask`] reports.
    pub fn submit(
        &self,
        new: NewTask,
    ) -> Result<TaskId, Error> {
        let id = TaskId::from_uuid(Uuid::now_v7());
        let spec = TaskSpec {
            task_id: id.clone(),
            title: new.title,
            instruction: new.instruction,
            agent: new.agent,
            host: new.host,
            workspace: new.workspace,
        };
        self.ask(&id, ControlCommand::Submit(spec))?;
        Ok(id)
    }

    /// Sends one command to the task's actor and waits for its reply.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Control`] when the domain rejects the command, [`Error::Store`]
    /// when the append fails, and [`Error::ActorUnavailable`] or [`Error::Spawn`]
    /// when no actor can serve the command.
    pub fn ask(
        &self,
        id: &TaskId,
        command: ControlCommand,
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
    /// Returns [`Error::Store`] when the log cannot be read and [`Error::Control`] when
    /// the log does not replay.
    pub fn state(
        &self,
        id: &TaskId,
    ) -> Result<TaskState, Error> {
        let records = self.inner.store.load(id).map_err(Error::Store)?;
        TaskState::replay(&records).map_err(Error::Control)
    }

    /// Summarises every task the store holds, sorted by task id.
    ///
    /// A stream that holds no records has no submission to describe and is skipped.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when the store cannot list or read a stream, and
    /// [`Error::Control`] naming the task id when one stream does not replay.
    pub fn list(&self) -> Result<Vec<TaskSummary>, Error> {
        let mut keys = self.inner.store.keys().map_err(Error::Store)?;
        keys.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        let mut summaries = Vec::with_capacity(keys.len());
        for id in keys {
            let records = self.inner.store.load(&id).map_err(Error::Store)?;
            let state = TaskState::replay(&records).map_err(|error| corrupt(&id, error))?;
            let Some(spec) = state.spec else {
                continue;
            };
            let (instruction, instruction_truncated) = listing_instruction(&spec.instruction);
            summaries.push(TaskSummary {
                id,
                title: spec.title,
                instruction,
                instruction_truncated,
                status: state.status,
                agent: spec.agent,
                host: spec.host,
                workspace: spec.workspace,
                outcome: state.outcome,
                updates: state.update_count,
            });
        }
        Ok(summaries)
    }

    /// Returns every task that is still submitted, skipping a stream that cannot be read
    /// or replayed.
    ///
    /// Restart recovery uses this so one damaged stream does not hide the healthy work a
    /// previous process left behind; [`ControlSystem::list`] stays strict for callers
    /// that must know the whole store is readable.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when the store cannot be enumerated.
    pub fn submitted(&self) -> Result<Vec<TaskId>, Error> {
        let mut keys = self.inner.store.keys().map_err(Error::Store)?;
        keys.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        let mut submitted = Vec::new();
        for id in keys {
            let records = match self.inner.store.load(&id) {
                Ok(records) => records,
                Err(error) => {
                    tracing::error!(task = id.as_str(), %error, "skipping an unreadable stream");
                    continue;
                },
            };
            match TaskState::replay(&records) {
                Ok(state) if state.status == TaskStatus::Submitted => submitted.push(id),
                Ok(_) => {},
                Err(error) => {
                    tracing::error!(
                        task = id.as_str(),
                        %error,
                        "skipping a stream that does not replay"
                    );
                },
            }
        }
        Ok(submitted)
    }

    // The entries guard orders a command send against retirement's final check: a send
    // under it either lands before `try_recv`, which then serves it in place, or after
    // the entry is gone, which sends the next command through `replay` and a fresh
    // actor. The task's stripe serializes a cold replay with that retirement.
    fn deliver(
        &self,
        id: &TaskId,
        command: ControlCommand,
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
            .lifecycle(id)
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
        id: &TaskId,
    ) -> Result<(TaskState, Version), Error> {
        let records = self.inner.store.load(id).map_err(Error::Store)?;
        let state = TaskState::replay(&records).map_err(Error::Control)?;
        let version = records
            .last()
            .map_or(Version::ZERO, |record| record.version);
        Ok((state, version))
    }

    fn spawn(
        &self,
        id: &TaskId,
        state: TaskState,
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
            .name(format!("task-actor-{}", id.as_str()))
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
#[allow(clippy::result_large_err)] // The miss returns the command itself, so the caller can resend it.
fn send_registered(
    registry: &mut HashMap<TaskId, Arc<Actor>>,
    id: &TaskId,
    command: ControlCommand,
) -> Result<Dispatch, ControlCommand> {
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
    command: ControlCommand,
) -> Dispatch {
    let (reply, wait) = mpsc::channel();
    match actor.tx.send(Request { command, reply }) {
        Ok(()) => Dispatch::Reply(wait),
        Err(mpsc::SendError(request)) => Dispatch::Gone(request.command),
    }
}

// Removes the entry only after the mailbox is known empty, so a command sent under the
// entries guard is never dropped between that check and the removal.
fn retire(
    worker: &Worker,
    requests: &Receiver<Request>,
) -> Option<Request> {
    let registry = worker.registry.upgrade()?;
    let _lifecycle = registry
        .lifecycle(&worker.id)
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
    store: &dyn EnumerableStore<TaskId, ControlEvent>,
    publisher: &dyn ControlPublisher,
    id: &TaskId,
    state: &mut TaskState,
    version: &mut Version,
    command: ControlCommand,
) -> Result<Appended, Error> {
    // The actor's id is the stream it appends to, so a spec naming another task would
    // leave one summary whose id and spec disagree.
    if let ControlCommand::Submit(spec) = &command
        && spec.task_id != *id
    {
        return Err(Error::Control(ControlError::WrongTaskId {
            stream: id.as_str().to_owned(),
            spec: spec.task_id.as_str().to_owned(),
        }));
    }
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
    let mut records = Vec::with_capacity(events.len());
    let now = timestamp(SystemTime::now());
    for event in &events {
        next_version = next_version.next();
        records
            .push(CloudEvent::new(id.as_str(), next_version, event.clone()).with_time(now.clone()));
    }
    let committed = store.append(id, *version, &records).map_err(Error::Store)?;
    publisher.publish(&records);
    *state = next_state;
    *version = committed;
    Ok(Appended {
        version: committed,
        events: records,
    })
}

// A replay failure in one stream has to name the stream, or a listing of many tasks
// cannot say which log is bad.
fn corrupt(
    id: &TaskId,
    error: ControlError,
) -> Error {
    match error {
        ControlError::CorruptHistory(message) => Error::Control(ControlError::CorruptHistory(
            format!("task {}: {message}", id.as_str()),
        )),
        other => Error::Control(other),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Instant;

    use mokmok_control_core::EventStore;
    use mokmok_jobs_memory::MemoryEventStore;

    use super::*;

    #[derive(Default)]
    struct CountingStore {
        inner: MemoryEventStore<TaskId, ControlEvent>,
        loads: AtomicUsize,
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

    #[test]
    fn the_default_idle_timeout_is_sixty_seconds() {
        assert_eq!(DEFAULT_IDLE_TIMEOUT, Duration::from_secs(60));
    }

    #[test]
    fn a_committed_record_carries_the_current_time() {
        let store = Arc::new(MemoryEventStore::<TaskId, ControlEvent>::new());
        let system = ControlSystem::without_idle_timeout(store.clone(), Arc::new(NoopPublisher));
        let id = system
            .submit(NewTask {
                title: "Fix the build".to_owned(),
                instruction: "Make it pass.".to_owned(),
                agent: "scripted".to_owned(),
                host: None,
                workspace: None,
            })
            .expect("submit commits");
        let records = store.load(&id).expect("the stream reads");
        let time = records[0].time.as_deref().expect("the record is stamped");
        assert!(time.ends_with('Z'), "{time}");
        assert_eq!(time.len(), "2026-11-08T06:19:08.123Z".len());
        assert!(
            time > "2024-01-01T00:00:00.000Z",
            "the stamp must be the current clock, not the epoch: {time}"
        );
    }

    #[test]
    fn a_cached_actor_serves_the_next_command_without_reloading() {
        let store = Arc::new(CountingStore::default());
        let system = ControlSystem::new(store.clone(), Arc::new(NoopPublisher));
        let id = system
            .submit(NewTask {
                title: "Fix the build".to_owned(),
                instruction: "Make it pass.".to_owned(),
                agent: "scripted".to_owned(),
                host: None,
                workspace: None,
            })
            .expect("submit commits");
        let loads = store.loads.load(Ordering::SeqCst);

        system
            .ask(&id, ControlCommand::Start)
            .expect("start commits");

        assert_eq!(
            store.loads.load(Ordering::SeqCst),
            loads,
            "the cached actor must not reload the stream"
        );
    }

    #[test]
    fn the_same_task_always_gets_the_same_lifecycle_stripe() {
        let system = ControlSystem::without_idle_timeout(
            Arc::new(MemoryEventStore::<TaskId, ControlEvent>::new()),
            Arc::new(NoopPublisher),
        );
        let id = TaskId::from_u128(5);
        let first = system.inner.registry.lifecycle(&id);
        let second = system.inner.registry.lifecycle(&id);
        assert!(std::ptr::eq(first, second));
    }

    #[test]
    fn a_submit_for_another_stream_is_refused() {
        let system = ControlSystem::without_idle_timeout(
            Arc::new(MemoryEventStore::<TaskId, ControlEvent>::new()),
            Arc::new(NoopPublisher),
        );
        let stream = TaskId::from_u128(1);
        let spec = TaskSpec {
            task_id: TaskId::from_u128(2),
            title: "Fix the build".to_owned(),
            instruction: "Make it pass.".to_owned(),
            agent: "scripted".to_owned(),
            host: None,
            workspace: None,
        };
        let error = system
            .ask(&stream, ControlCommand::Submit(spec))
            .expect_err("the spec names another stream");
        assert!(matches!(
            error,
            Error::Control(ControlError::WrongTaskId { .. })
        ));
        assert_eq!(
            system.state(&stream).expect("the stream reads").status,
            TaskStatus::New
        );
    }

    #[test]
    fn every_error_variant_describes_itself() {
        let cases = [
            Error::Control(ControlError::Terminal),
            Error::Store(StoreError::Io("the disk is full".to_owned())),
            Error::ActorUnavailable,
            Error::Spawn("thread limit".to_owned()),
        ];
        for error in &cases {
            assert!(!error.to_string().is_empty(), "{error:?}");
        }
        assert!(std::error::Error::source(&cases[0]).is_some());
        assert!(std::error::Error::source(&cases[1]).is_some());
        assert!(std::error::Error::source(&cases[2]).is_none());
        assert!(std::error::Error::source(&cases[3]).is_none());
    }

    #[test]
    fn an_idle_actor_retires_and_the_next_command_reloads_the_stream() {
        let store = Arc::new(CountingStore::default());
        let system = ControlSystem::with_idle_timeout(
            store.clone(),
            Arc::new(NoopPublisher),
            Duration::ZERO,
        );
        let id = system
            .submit(NewTask {
                title: "Fix the build".to_owned(),
                instruction: "Make it pass.".to_owned(),
                agent: "scripted".to_owned(),
                host: None,
                workspace: None,
            })
            .expect("submit commits");
        assert_eq!(store.loads.load(Ordering::SeqCst), 1);

        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let entries = system
                .inner
                .registry
                .entries
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if entries.is_empty() {
                break;
            }
            assert!(Instant::now() < deadline, "the idle actor never retired");
            drop(entries);
            thread::sleep(Duration::from_millis(5));
        }

        system
            .ask(&id, ControlCommand::Start)
            .expect("the fresh actor starts the task");
        assert_eq!(
            store.loads.load(Ordering::SeqCst),
            2,
            "a retired actor reloads the stream"
        );
    }
}

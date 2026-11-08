//! The supervisor: a work queue that runs submitted tasks on a runner.

use std::collections::{HashSet, VecDeque};
use std::mem;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::{self, JoinHandle};

use mokmok_control_core::{
    AgentUpdate, ControlCommand, ControlError, TaskId, TaskSpec, TaskStatus,
};

use crate::system::{ControlSystem, Error, NewTask};

/// Runs one task and reports what the agent does while it works.
pub trait Runner: Send + Sync {
    /// Runs `spec` to an outcome, reporting progress as it happens.
    ///
    /// The supervisor records whatever the runner returns; a transport or protocol
    /// failure is a [`RunOutcome::Failed`], never a panic.
    fn run(
        &self,
        spec: &TaskSpec,
        reporter: &dyn Reporter,
    ) -> RunOutcome;
}

/// What a runner tells the control plane while it works.
pub trait Reporter: Send + Sync {
    /// Records the agent's session, once per run.
    ///
    /// # Errors
    ///
    /// Returns the message of the error the control plane reported, usually because
    /// the task is already terminal.
    fn started(
        &self,
        session: &str,
    ) -> Result<(), String>;

    /// Records one transcript step.
    ///
    /// # Errors
    ///
    /// Returns the message of the error the control plane reported, usually because
    /// the task is already terminal.
    fn update(
        &self,
        update: AgentUpdate,
    ) -> Result<(), String>;

    /// Whether a human cancelled the task.
    fn cancelled(&self) -> bool;
}

/// How a run ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunOutcome {
    Completed { summary: String },
    Cancelled,
    Failed { reason: String },
}

/// Runs submitted tasks on a fixed number of worker threads.
pub struct Supervisor {
    work: Arc<Work>,
    workers: usize,
    threads: Mutex<Vec<JoinHandle<()>>>,
    started: AtomicBool,
}

// Everything a worker thread needs. A worker holds no handle on the supervisor, so it
// cannot keep the supervisor alive and `shutdown` can join it.
struct Work {
    system: ControlSystem,
    runner: Arc<dyn Runner>,
    cancelled: Mutex<HashSet<TaskId>>,
    queue: Queue,
}

struct Queue {
    pending: Mutex<VecDeque<TaskId>>,
    shutdown: AtomicBool,
    wake: Condvar,
}

impl Queue {
    const fn new() -> Self {
        Self {
            pending: Mutex::new(VecDeque::new()),
            shutdown: AtomicBool::new(false),
            wake: Condvar::new(),
        }
    }

    fn push(
        &self,
        id: TaskId,
    ) {
        let mut pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
        pending.push_back(id);
        drop(pending);
        self.wake.notify_one();
    }

    fn next(&self) -> Option<TaskId> {
        let mut pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if self.shutdown.load(Ordering::SeqCst) {
                return None;
            }
            if let Some(id) = pending.pop_front() {
                return Some(id);
            }
            pending = self
                .wake
                .wait(pending)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    fn stop(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
        self.wake.notify_all();
    }

    // A supervisor may be started again after `shutdown`, and the flag a previous
    // shutdown raised would otherwise make every new worker return at once.
    fn resume(&self) {
        self.shutdown.store(false, Ordering::SeqCst);
    }
}

impl Supervisor {
    /// Builds a supervisor over `system` that runs tasks on `runner`.
    ///
    /// `workers` is clamped to at least one. A worker is only spawned by
    /// [`Supervisor::start`]; [`Supervisor::run_pending`] runs in the caller's thread.
    #[must_use]
    pub fn new(
        system: ControlSystem,
        runner: Arc<dyn Runner>,
        workers: usize,
    ) -> Self {
        Self {
            work: Arc::new(Work {
                system,
                runner,
                cancelled: Mutex::new(HashSet::new()),
                queue: Queue::new(),
            }),
            workers: workers.max(1),
            threads: Mutex::new(Vec::new()),
            started: AtomicBool::new(false),
        }
    }

    /// Submits a task and queues it for the workers.
    ///
    /// # Errors
    ///
    /// Returns every error [`ControlSystem::submit`] reports; nothing is queued then.
    pub fn submit(
        &self,
        new: NewTask,
    ) -> Result<TaskId, Error> {
        let id = self.work.system.submit(new)?;
        self.work.queue.push(id.clone());
        Ok(id)
    }

    /// Asks the task to stop and marks it cancelled for its runner.
    ///
    /// # Errors
    ///
    /// Returns the error for a task that does not exist or is already terminal, which
    /// is what the domain reports; the task is not cancelled and its runner does not
    /// see the mark then.
    pub fn cancel(
        &self,
        id: &TaskId,
        reason: &str,
    ) -> Result<(), Error> {
        self.work.mark_cancelled(id);
        let asked = self.work.system.ask(
            id,
            ControlCommand::Cancel {
                reason: reason.to_owned(),
            },
        );
        if let Err(error) = asked {
            self.work.unmark_cancelled(id);
            return Err(error);
        }
        Ok(())
    }

    /// Runs every task that is submitted, one after another, in this thread.
    ///
    /// Returns how many tasks this call started. A task another worker started first is
    /// skipped: the `Start` command is the fence, so only one caller wins it.
    ///
    /// # Errors
    ///
    /// Returns every error listing the tasks, reading one, starting one, or recording an
    /// outcome reports. A failed task is not an error; it is a [`RunOutcome::Failed`].
    pub fn run_pending(&self) -> Result<usize, Error> {
        let mut ran = 0;
        for summary in self.work.system.list()? {
            if summary.status == TaskStatus::Submitted && self.work.run_one(&summary.id)? {
                ran += 1;
            }
        }
        Ok(ran)
    }

    /// Spawns the worker threads, once.
    ///
    /// Every task the store still holds in [`TaskStatus::Submitted`] is queued first,
    /// so a submission a previous process committed runs after a restart.
    pub fn start(&self) {
        if self.started.swap(true, Ordering::SeqCst) {
            return;
        }
        self.work.queue.resume();
        self.work.enqueue_submitted();
        let mut threads = self.threads.lock().unwrap_or_else(PoisonError::into_inner);
        for _ in 0..self.workers {
            let work = Arc::clone(&self.work);
            // A worker that cannot spawn leaves fewer workers; the rest of the queue is
            // still served, and `shutdown` joins the threads that exist.
            let Ok(handle) = thread::Builder::new()
                .name("control-worker".to_owned())
                .spawn(move || work.run_loop())
            else {
                break;
            };
            threads.push(handle);
        }
    }

    /// Stops the workers and joins them.
    ///
    /// A run already in progress finishes first. Tasks still queued stay submitted, and
    /// [`Supervisor::start`] queues every submitted task it finds in the store, so a
    /// restarted supervisor picks them up.
    pub fn shutdown(&self) {
        self.work.queue.stop();
        let handles: Vec<JoinHandle<()>> = {
            let mut threads = self.threads.lock().unwrap_or_else(PoisonError::into_inner);
            mem::take(&mut *threads)
        };
        for handle in handles {
            let _ = handle.join();
        }
        self.started.store(false, Ordering::SeqCst);
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl Work {
    fn run_loop(&self) {
        while let Some(id) = self.queue.next() {
            if let Err(error) = self.run_one(&id) {
                tracing::error!(task = id.as_str(), %error, "task could not run");
            }
        }
    }

    fn enqueue_submitted(&self) {
        match self.system.submitted() {
            Ok(ids) => {
                for id in ids {
                    self.queue.push(id);
                }
            },
            Err(error) => tracing::error!(%error, "cannot list submitted tasks"),
        }
    }

    fn mark_cancelled(
        &self,
        id: &TaskId,
    ) {
        self.cancelled
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id.clone());
    }

    fn unmark_cancelled(
        &self,
        id: &TaskId,
    ) {
        self.cancelled
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(id);
    }

    fn is_cancelled(
        &self,
        id: &TaskId,
    ) -> bool {
        self.cancelled
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(id)
    }

    fn run_one(
        &self,
        id: &TaskId,
    ) -> Result<bool, Error> {
        let state = self.system.state(id)?;
        if state.status != TaskStatus::Submitted {
            return Ok(false);
        }
        let Some(spec) = state.spec else {
            return Ok(false);
        };
        match self.system.ask(id, ControlCommand::Start) {
            Ok(_) => {},
            Err(Error::Control(_)) => return Ok(false),
            Err(error) => return Err(error),
        }
        let reporter = SystemReporter {
            work: self,
            id: id.clone(),
        };
        // `Start` is already committed, so a panicking runner must still settle the task
        // or the worker would leave it Running forever and lose itself.
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.runner.run(&spec, &reporter)
        }))
        .unwrap_or_else(|_| RunOutcome::Failed {
            reason: "the runner panicked".to_owned(),
        });
        self.settle(id, outcome)?;
        Ok(true)
    }

    // A human cancel can commit while the runner is still working, so the terminal error
    // its own outcome runs into is expected and must not be reported as a failure.
    fn settle(
        &self,
        id: &TaskId,
        outcome: RunOutcome,
    ) -> Result<(), Error> {
        let command = match outcome {
            RunOutcome::Completed { summary } => ControlCommand::Complete { summary },
            RunOutcome::Cancelled => ControlCommand::Cancel {
                reason: "cancelled by the runner".to_owned(),
            },
            RunOutcome::Failed { reason } => ControlCommand::Fail { reason },
        };
        match self.system.ask(id, command) {
            Ok(_) | Err(Error::Control(ControlError::Terminal)) => Ok(()),
            Err(error) => Err(error),
        }
    }
}

struct SystemReporter<'a> {
    work: &'a Work,
    id: TaskId,
}

impl Reporter for SystemReporter<'_> {
    fn started(
        &self,
        session: &str,
    ) -> Result<(), String> {
        self.send(ControlCommand::OpenSession {
            session_id: session.to_owned(),
        })
    }

    fn update(
        &self,
        update: AgentUpdate,
    ) -> Result<(), String> {
        self.send(ControlCommand::Update { update })
    }

    fn cancelled(&self) -> bool {
        self.work.is_cancelled(&self.id)
    }
}

impl SystemReporter<'_> {
    fn send(
        &self,
        command: ControlCommand,
    ) -> Result<(), String> {
        self.work
            .system
            .ask(&self.id, command)
            .map(|_| ())
            .map_err(|error| message_of(&error))
    }
}

// A store error names the data directory, and this message can reach a `Failed` event
// that `GET /api/tasks/{id}` serves, so it never carries one.
fn message_of(error: &Error) -> String {
    match error {
        Error::Store(_) => "the control plane could not record the session".to_owned(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use mokmok_control_core::ControlEvent;
    use mokmok_jobs_memory::MemoryEventStore;

    use super::*;
    use crate::system::{ControlSystem, NewTask, NoopPublisher};

    fn system() -> ControlSystem {
        ControlSystem::without_idle_timeout(
            Arc::new(MemoryEventStore::<TaskId, ControlEvent>::new()),
            Arc::new(NoopPublisher),
        )
    }

    fn new_task() -> NewTask {
        NewTask {
            title: "Fix the build".to_owned(),
            instruction: "Make the tests pass.".to_owned(),
            agent: "scripted".to_owned(),
            host: None,
            workspace: Some("/work/mokmok".to_owned()),
        }
    }

    fn supervisor() -> (ControlSystem, Supervisor) {
        let system = system();
        let supervisor = Supervisor::new(
            system.clone(),
            Arc::new(crate::runner::ScriptedRunner::new(Vec::new(), "done")),
            1,
        );
        (system, supervisor)
    }

    #[test]
    fn a_cancel_marks_the_task_for_its_runner() -> Result<(), Box<dyn std::error::Error>> {
        let (_, supervisor) = supervisor();
        let id = supervisor.submit(new_task())?;
        assert!(!supervisor.work.is_cancelled(&id));
        supervisor.cancel(&id, "stop")?;
        assert!(supervisor.work.is_cancelled(&id));
        Ok(())
    }

    #[test]
    fn a_failed_cancel_leaves_no_mark() -> Result<(), Box<dyn std::error::Error>> {
        let (system, supervisor) = supervisor();
        let id = supervisor.submit(new_task())?;
        assert_eq!(supervisor.run_pending()?, 1);
        assert_eq!(system.state(&id)?.status, TaskStatus::Completed);

        assert!(supervisor.cancel(&id, "too late").is_err());
        assert!(
            !supervisor.work.is_cancelled(&id),
            "the failed cancel must not reach the runner"
        );
        Ok(())
    }

    #[test]
    fn a_store_error_is_replaced_by_a_generic_message() {
        let error = Error::Store(mokmok_control_core::StoreError::Io(
            "/srv/mokmok/tasks/x.jsonl: permission denied".to_owned(),
        ));
        let message = message_of(&error);
        assert!(!message.contains("/srv/mokmok"), "{message}");
        assert_eq!(message, "the control plane could not record the session");
    }

    #[test]
    fn a_control_error_keeps_its_message() {
        let error = Error::Control(ControlError::Terminal);
        assert_eq!(message_of(&error), "task: task is in a terminal state");
    }

    struct PanickingRunner;

    impl Runner for PanickingRunner {
        fn run(
            &self,
            _spec: &TaskSpec,
            _reporter: &dyn Reporter,
        ) -> RunOutcome {
            panic!("the runner failed on purpose");
        }
    }

    #[test]
    fn a_panicking_runner_fails_the_task() -> Result<(), Box<dyn std::error::Error>> {
        let system = system();
        let supervisor = Supervisor::new(system.clone(), Arc::new(PanickingRunner), 1);
        let id = supervisor.submit(new_task())?;
        assert_eq!(supervisor.run_pending()?, 1);

        let state = system.state(&id)?;
        assert_eq!(state.status, TaskStatus::Failed);
        assert_eq!(state.outcome.as_deref(), Some("the runner panicked"));
        Ok(())
    }
}

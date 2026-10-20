use std::io;
use std::os::unix::process::ExitStatusExt as _;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use mokmok_protocol::ExecutionID;
use nix::errno::Errno;
use nix::sys::signal::{Signal as NixSignal, kill, killpg};
use nix::unistd::Pid;
use tokio::process::Child;
use tokio::task::JoinHandle;

use crate::artifact::{self, Artifact};
use crate::output::{Captured, Pump};
use crate::plan::{self, Reached, Status};
use crate::request::Request;
use crate::workspace::{self, Workspace};

/// A signal to the workload's process group.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Signal {
    Interrupt,
    Terminate,
    Kill,
}

impl Signal {
    /// The number `kill(2)` names this signal by.
    #[must_use]
    pub const fn raw(self) -> i32 {
        self.nix_signal() as i32
    }

    const fn nix_signal(self) -> NixSignal {
        match self {
            Self::Interrupt => NixSignal::SIGINT,
            Self::Terminate => NixSignal::SIGTERM,
            Self::Kill => NixSignal::SIGKILL,
        }
    }
}

/// How the workload ended.
///
/// [`Outcome::Overran`] means the deadline fired while the workload's pid was still present. A
/// workload that exited in the same instant the deadline fired may be reported as `Overran`: the
/// two cannot be told apart after the fact, and this does not pretend otherwise.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Outcome {
    /// The workload exited with this code.
    Exited(i32),
    /// The workload was killed by this signal number.
    Signalled(i32),
    /// The execution deadline passed while the workload was still running.
    Overran,
}

/// Whether the workload may have started, as far as the supervisor can tell.
///
/// A caller that must not repeat work blindly treats both variants as possibly started; only an
/// [`Error::Confine`] means the workload did not run.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Started {
    /// The confiner reported the exec attempt. The workload ran, unless the confiner stopped in
    /// the window between that report and the exec call.
    Attempted,
    /// Nothing reported an attempt, so whether the workload ran is unknown.
    Unknown,
}

/// Where the child reports a confinement refusal, if it reports at all.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Reporting {
    /// The confiner writes `ctl/status.json` beside the plan it read, so a run with no status at
    /// all is a confiner that never got as far as the plan.
    Confiner,
    /// A shell that carries the limits and nothing else: its exit is the whole story.
    Exit,
}

/// One running execution.
///
/// Every signal here reaches the workload, and its whole process group once the confiner has
/// started a session. That group holds forked grandchildren but not a workload that calls `setsid`
/// itself.
pub struct Run {
    child: Child,
    workspace: Workspace,
    directory: PathBuf,
    id: ExecutionID,
    pid: u32,
    stdout: Pump,
    stderr: Pump,
    watchdog: Option<JoinHandle<()>>,
    overran: Arc<AtomicBool>,
    grace: Duration,
    artifacts: Vec<PathBuf>,
    reporting: Reporting,
    reaped: bool,
    handed_off: bool,
}

impl Run {
    /// Takes over an already-spawned child, its own workspace, the directory it runs in, and the
    /// request it came from.
    ///
    /// # Errors
    ///
    /// Returns an error when the child has no process id or no output pipes, having killed it.
    pub fn start(
        mut child: Child,
        workspace: Workspace,
        directory: PathBuf,
        request: &Request,
        grace: Duration,
        reporting: Reporting,
    ) -> Result<Self, io::Error> {
        let Some(pid) = child.id() else {
            return Err(io::Error::other("the child started without a process id"));
        };
        let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
            let _ = killpg(nix_pid(pid), NixSignal::SIGKILL);

            return Err(io::Error::other(
                "the child started without its output pipes",
            ));
        };

        let cap = request.limits.output.get();
        let overran = Arc::new(AtomicBool::new(false));
        let watchdog = request
            .deadline
            .map(|deadline| watch(pid, deadline, grace, Arc::clone(&overran)));

        Ok(Self {
            child,
            workspace,
            directory,
            id: request.execution,
            pid,
            stdout: Pump::spawn(stdout, cap),
            stderr: Pump::spawn(stderr, cap),
            watchdog,
            overran,
            grace,
            artifacts: request.artifacts.clone(),
            reporting,
            reaped: false,
            handed_off: false,
        })
    }

    #[must_use]
    pub const fn id(&self) -> ExecutionID {
        self.id
    }

    #[must_use]
    pub const fn pid(&self) -> u32 {
        self.pid
    }

    /// The directory the workload runs in, and where its artifacts are looked for.
    #[must_use]
    pub fn workspace(&self) -> &Path {
        &self.directory
    }

    /// One signal to the workload.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Signal`] when neither the workload's process group nor the workload itself
    /// can be signalled, including when both are already gone.
    pub fn signal(
        &self,
        signal: Signal,
    ) -> Result<(), Error> {
        signal_execution(self.pid, signal.nix_signal()).map_err(|error| signal_error(signal, error))
    }

    /// TERM the group, wait [`Config::grace`](crate::Config::grace), KILL what is left.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Signal`] when the group cannot be signalled.
    pub async fn stop(&mut self) -> Result<(), Error> {
        escalate(self.pid, self.grace).await
    }

    /// Reaps the workload, capturing both streams and registering the declared artifacts.
    ///
    /// An `Ok` means the workload possibly started; [`Completed::started`] reports what the
    /// confiner said about the exec attempt.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Wait`] when the workload cannot be reaped, and [`Error::Confine`] when
    /// the confiner refused it, in which case the workload never ran.
    pub async fn wait(mut self) -> Result<Completed, Error> {
        // The watchdog has to stay armed while this waits: a deadline is exactly the case where
        // nothing else would end the workload. Reaping first is what makes it safe to stand it
        // down afterwards.
        let status = self.child.wait().await.map_err(Error::Wait)?;
        self.reaped = true;

        if let Some(watchdog) = self.watchdog.take() {
            watchdog.abort();
            // Waiting out the abort keeps a task already running on another thread from signalling
            // the group after the child is reaped, when the group id could name another group.
            let _ = watchdog.await;
        }

        if self.overran.load(Ordering::Acquire) {
            // The abort above can land between the watchdog's TERM and its KILL, which would leave
            // the grandchildren that ignored TERM running. The group id is still allocated while
            // any of them lives, and the kernel hands a freed id to a new group only by wrapping
            // the whole pid space, so this finishes the escalation rather than reaching a stranger.
            let _ = killpg(nix_pid(self.pid), NixSignal::SIGKILL);
        }

        let stdout = self.stdout.join().await.map_err(Error::Wait)?;
        let stderr = self.stderr.join().await.map_err(Error::Wait)?;

        let mut started = Started::Unknown;
        if let Reporting::Confiner = self.reporting {
            match plan::read_status(&self.workspace.status()) {
                Some(Status::Refused { failure }) => return Err(Error::Confine(failure)),
                Some(Status::Attempted) => started = Started::Attempted,
                Some(Status::Prepared) => {
                    return Err(Error::Confine(Failure::Interrupted {
                        reached: Reached::Prepared,
                    }));
                },
                Some(Status::Confined) => {
                    return Err(Error::Confine(Failure::Interrupted {
                        reached: Reached::Confined,
                    }));
                },
                None => {
                    // A confiner that started writes `Prepared` before anything else can fail, and
                    // every record is one fixed-size write replaced whole, so an absent or
                    // unparsable status means the confiner never wrote at all and the workload
                    // cannot have run. That holds with the deadline too: a pid that was still there
                    // was the confiner's, and reporting it as a workload that overran would claim a
                    // start this cannot prove.
                    return Err(Error::Confine(Failure::Unknown(format!(
                        "the confiner stopped ({status}) without reporting",
                    ))));
                },
            }
        }

        let outcome = if self.overran.load(Ordering::Acquire) {
            Outcome::Overran
        } else {
            outcome_of(status)
        };

        let artifacts = self
            .artifacts
            .iter()
            .filter_map(|declared| artifact::register(&self.directory, declared))
            .collect();
        let completed = Completed {
            id: self.id,
            outcome,
            started,
            stdout,
            stderr,
            artifacts,
            workspace: self.directory.clone(),
            directory: self.workspace.directory().to_path_buf(),
            cleaned: false,
        };
        self.handed_off = true;

        Ok(completed)
    }
}

/// Dropping a `Run` kills the group and removes the execution directory, best effort.
impl Drop for Run {
    fn drop(&mut self) {
        if let Some(watchdog) = self.watchdog.take() {
            // Dropping the handle would only detach the task, and a watchdog that fires after the
            // child is reaped signals a pid and group id the kernel may have handed to someone
            // else by then.
            watchdog.abort();
        }
        if !self.reaped {
            // Unreaped, so the pid and its group still name the workload.
            let _ = signal_execution(self.pid, NixSignal::SIGKILL);
        }
        if !self.handed_off {
            let _ = self.workspace.remove();
        }
    }
}

/// The end state of one execution.
///
/// A `Completed` means the workload possibly started; only an [`Error::Confine`] means it did not
/// run. [`Completed::started`] reports what the confiner said about the exec attempt.
///
/// The workspace stays where it is until [`Completed::cleanup`] is called or the value drops,
/// because the artifacts live inside it.
pub struct Completed {
    id: ExecutionID,
    outcome: Outcome,
    started: Started,
    stdout: Captured,
    stderr: Captured,
    artifacts: Vec<Artifact>,
    workspace: PathBuf,
    directory: PathBuf,
    cleaned: bool,
}

impl Completed {
    #[must_use]
    pub const fn id(&self) -> ExecutionID {
        self.id
    }

    #[must_use]
    pub const fn outcome(&self) -> Outcome {
        self.outcome
    }

    /// Whether the confiner reported that it was about to run the workload, which is all the
    /// supervisor can observe: the confiner cannot report again after a successful exec, because
    /// the exec replaces it.
    #[must_use]
    pub const fn started(&self) -> Started {
        self.started
    }

    #[must_use]
    pub const fn stdout(&self) -> &Captured {
        &self.stdout
    }

    #[must_use]
    pub const fn stderr(&self) -> &Captured {
        &self.stderr
    }

    #[must_use]
    pub fn artifacts(&self) -> &[Artifact] {
        &self.artifacts
    }

    #[must_use]
    pub fn workspace(&self) -> &Path {
        &self.workspace
    }

    /// Removes the execution directory.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Cleanup`] when the directory is there and cannot be removed.
    pub fn cleanup(mut self) -> Result<(), Error> {
        workspace::remove_directory(&self.directory).map_err(|source| Error::Cleanup {
            path: self.directory.clone(),
            source,
        })?;
        self.cleaned = true;

        Ok(())
    }
}

/// Dropping a `Completed` removes the execution directory, best effort. It is the same end state
/// [`Completed::cleanup`] reaches.
impl Drop for Completed {
    fn drop(&mut self) {
        if !self.cleaned {
            let _ = workspace::remove_directory(&self.directory);
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("the workload could not be reaped: {0}")]
    Wait(#[source] io::Error),
    #[error("the {signal:?} signal could not be sent to the workload group: {source}")]
    Signal { signal: Signal, source: io::Error },
    #[error("the confiner refused the workload: {0}")]
    Confine(#[from] Failure),
    #[error("the execution directory {} could not be removed: {source}", path.display())]
    Cleanup { path: PathBuf, source: io::Error },
}

/// Why the confiner refused to run the workload. The workload never ran.
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize, thiserror::Error)]
pub enum Failure {
    #[error("the plan could not be read: {0}")]
    Plan(String),
    #[error("the confiner could not start a session: {0}")]
    Session(String),
    #[error("the resource limits could not be applied: {0}")]
    Limits(String),
    #[error("the ruleset could not be enforced: {0}")]
    Ruleset(String),
    #[error("{} could not be executed: {message}", program.display())]
    Exec {
        #[serde(with = "crate::plan::text")]
        program: PathBuf,
        message: String,
    },
    #[error("the path {0} is not the object the plan recorded")]
    Path(String),
    #[error("the confiner stopped after reaching {reached:?} without attempting the workload")]
    Interrupted { reached: Reached },
    #[error("the confiner stopped without reporting: {0}")]
    Unknown(String),
}

/// One watchdog task for one deadline.
///
/// It holds the pid rather than the `Run`, so a `Run` that is never waited on still has its
/// workload bounded.
fn watch(
    pid: u32,
    deadline: Duration,
    grace: Duration,
    overran: Arc<AtomicBool>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        tokio::time::sleep(deadline).await;

        if !present(pid) {
            return;
        }

        overran.store(true, Ordering::Release);
        let _ = escalate(pid, grace).await;
    })
}

/// `Ok` and `EPERM` mean the pid is there; `ESRCH` means it is gone. A zombie is still there, so a
/// workload that exited in the same instant as the deadline is escalated and reported as
/// [`Outcome::Overran`].
fn present(pid: u32) -> bool {
    !matches!(kill(nix_pid(pid), None), Err(Errno::ESRCH))
}

/// TERM the workload, wait out `grace`, then KILL what is left.
///
/// [`Run::stop`] and the deadline watchdog both escalate here, so neither path can leave a
/// workload with a weaker stop than the other. A workload that is already gone is the state this
/// aims at, so `ESRCH` is not a failure.
async fn escalate(
    pid: u32,
    grace: Duration,
) -> Result<(), Error> {
    if let Err(error) = signal_execution(pid, NixSignal::SIGTERM)
        && error != Errno::ESRCH
    {
        return Err(signal_error(Signal::Terminate, error));
    }

    tokio::time::sleep(grace).await;

    match signal_execution(pid, NixSignal::SIGKILL) {
        Ok(()) | Err(Errno::ESRCH) => Ok(()),
        Err(error) => Err(signal_error(Signal::Kill, error)),
    }
}

/// Signals the workload's process group, falling back to the workload's own pid.
///
/// The confiner calls `setsid` after it starts, so for the first moments of an execution a signal
/// has no group to reach: the pid is all there is until then. Dropping a `Run` immediately after
/// `spawn` lands in exactly that window, and a group-only signal there would leave the workload
/// running to its own end.
fn signal_execution(
    pid: u32,
    signal: NixSignal,
) -> Result<(), Errno> {
    match killpg(nix_pid(pid), signal) {
        Err(Errno::ESRCH) => kill(nix_pid(pid), signal),
        outcome => outcome,
    }
}

fn signal_error(
    signal: Signal,
    source: Errno,
) -> Error {
    Error::Signal {
        signal,
        source: io::Error::from(source),
    }
}

fn outcome_of(status: ExitStatus) -> Outcome {
    status.signal().map_or_else(
        || Outcome::Exited(status.code().unwrap_or_default()),
        Outcome::Signalled,
    )
}

/// A pid is a positive `i32` on Linux; anything else could not have come from a child. `i32::MAX`
/// is above `pid_max`, so a value that somehow does not fit names no process and the signal goes
/// nowhere, which is the safe direction.
fn nix_pid(pid: u32) -> Pid {
    Pid::from_raw(i32::try_from(pid).unwrap_or(i32::MAX))
}

#[cfg(test)]
mod tests {
    use super::{Signal, escalate, present};
    use std::time::Duration;

    #[test]
    fn the_signal_numbers_are_the_ones_the_kernel_names() {
        assert_eq!(Signal::Interrupt.raw(), 2);
        assert_eq!(Signal::Terminate.raw(), 15);
        assert_eq!(Signal::Kill.raw(), 9);
    }

    #[test]
    fn a_pid_no_process_holds_is_not_present() {
        assert!(present(std::process::id()));
        // Above `pid_max`, so no process can hold it.
        assert!(!present(u32::MAX));
    }

    #[tokio::test]
    async fn escalating_a_workload_that_is_already_gone_is_not_a_failure() {
        assert!(escalate(u32::MAX, Duration::ZERO).await.is_ok());
    }
}

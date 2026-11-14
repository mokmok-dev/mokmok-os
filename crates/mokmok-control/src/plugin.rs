//! The plugin runner: a task's work done by a TypeScript plugin in a Node process.
//!
//! One process per run, spawned from the daemon and spoken to over its pipes. The
//! reasons for a process rather than an embedded runtime are the ones the ACP stdio
//! transport already relies on: the daemon can kill it, and a plugin that blocks its own
//! event loop cannot block the daemon's worker thread.
//!
//! The host script is compiled into the daemon ([`HOST_SCRIPT`]) and written to a temp
//! file per run, so a deployment needs nothing on disk but Node itself and the plugin's
//! entry file. The protocol is [`mokmok_plugin_protocol`]: one `begin` frame and any
//! number of `cancel` frames in, event and end frames out.
//!
//! The plugin runs with the daemon's user and its environment, except
//! `MOKMOK_CONTROL_TOKEN`, which is removed before the spawn. A plugin is code the
//! operator installed on purpose, so that is the trust model it runs under.

use std::io::{BufRead, BufReader, Read as _, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use mokmok_control_core::TaskSpec;
use mokmok_plugin_protocol::{
    DaemonFrame, HostFrame, MAX_FRAME_BYTES, PluginOutcome, PluginTask, line, parse_frame,
};
use uuid::Uuid;

use crate::runner::{AgentTable, AgentTransport, resolve_transport, resolve_workspace};
use crate::supervisor::{Reporter, RunOutcome, Runner};

/// The host script, run by Node as `node <host> <entry> <options-json>`.
///
/// Compiled in rather than installed beside the binary, so the host and the daemon that
/// speaks to it are always the same build.
const HOST_SCRIPT: &str = include_str!("../../../packages/plugin-sdk/src/host.mjs");

/// The wall clock a plugin run may take when neither the entry nor the daemon sets one.
pub const DEFAULT_PLUGIN_TIMEOUT: Duration = Duration::from_secs(600);

/// How long after a cancel the host may keep running before it is killed.
///
/// The host settles a cancel itself, so this only bounds a host stuck inside the plugin's
/// own code, which cannot be interrupted from outside the process.
const CANCEL_GRACE: Duration = Duration::from_secs(5);

const CANCEL_POLL: Duration = Duration::from_millis(50);

const NOT_A_PLUGIN_TRANSPORT: &str = "the agent table routes this task to an ACP agent";
const MISSING_ENTRY: &str = "the plugin entry does not exist";
const SPAWN_FAILED: &str = "the plugin host could not be started";
const HOST_EXITED: &str = "the plugin host exited without reporting an outcome";

/// How the daemon starts Node and which wall clock it allows a run.
pub struct PluginHost {
    /// The Node executable, resolved once at startup so every run uses the same one.
    pub node: PathBuf,
    pub timeout: Option<Duration>,
}

impl PluginHost {
    #[must_use]
    pub fn new(node: impl Into<PathBuf>) -> Self {
        Self {
            node: node.into(),
            timeout: None,
        }
    }

    #[must_use]
    pub const fn with_timeout(
        mut self,
        timeout: Duration,
    ) -> Self {
        self.timeout = Some(timeout);
        self
    }
}

impl std::fmt::Debug for PluginHost {
    fn fmt(
        &self,
        formatter: &mut std::fmt::Formatter<'_>,
    ) -> std::fmt::Result {
        formatter
            .debug_struct("PluginHost")
            .field("node", &self.node)
            .field("timeout", &self.timeout)
            .finish()
    }
}

pub struct PluginRunner {
    table: AgentTable,
    host: PluginHost,
    workspace_root: Option<PathBuf>,
}

impl PluginRunner {
    #[must_use]
    pub const fn new(
        table: AgentTable,
        host: PluginHost,
    ) -> Self {
        Self {
            table,
            host,
            workspace_root: None,
        }
    }

    /// Confines every run to a workspace under `root`.
    #[must_use]
    pub fn with_workspace_root(
        mut self,
        root: impl Into<PathBuf>,
    ) -> Self {
        self.workspace_root = Some(root.into());
        self
    }

    fn route(
        &self,
        spec: &TaskSpec,
    ) -> Result<(PathBuf, Duration), String> {
        let AgentTransport::Plugin {
            entry,
            timeout_secs,
        } = resolve_transport(&self.table, spec)?
        else {
            return Err(NOT_A_PLUGIN_TRANSPORT.to_owned());
        };
        let timeout = self
            .host
            .timeout
            .or_else(|| timeout_secs.map(Duration::from_secs))
            .unwrap_or(DEFAULT_PLUGIN_TIMEOUT);
        Ok((entry.clone(), timeout))
    }
}

impl Runner for PluginRunner {
    fn run(
        &self,
        spec: &TaskSpec,
        reporter: &dyn Reporter,
    ) -> RunOutcome {
        let (entry, timeout) = match self.route(spec) {
            Ok(route) => route,
            Err(reason) => return RunOutcome::Failed { reason },
        };
        if reporter.cancelled() {
            return RunOutcome::Cancelled;
        }
        let cwd = match resolve_workspace(spec.workspace.as_deref(), self.workspace_root.as_deref())
        {
            Ok(path) => path,
            Err(reason) => return RunOutcome::Failed { reason },
        };
        if !entry.is_file() {
            return RunOutcome::Failed {
                reason: format!("{MISSING_ENTRY}: {}", entry.display()),
            };
        }
        let session = Uuid::now_v7().to_string();
        if let Err(reason) = reporter.started(&session) {
            return RunOutcome::Failed { reason };
        }
        let mut run = match Spawned::start(&self.host.node, &entry, &cwd, timeout) {
            Ok(run) => run,
            Err(reason) => return RunOutcome::Failed { reason },
        };
        if let Err(reason) = run.begin(&plugin_task(spec)) {
            return RunOutcome::Failed { reason };
        }
        match run.finish(reporter) {
            Settled::Cancelled => RunOutcome::Cancelled,
            Settled::Reported(outcome) => match outcome.clipped() {
                PluginOutcome::Completed { summary } => RunOutcome::Completed { summary },
                PluginOutcome::Failed { reason } => RunOutcome::Failed { reason },
                PluginOutcome::Cancelled => RunOutcome::Cancelled,
            },
            Settled::Broken(reason) => RunOutcome::Failed { reason },
        }
    }
}

/// A free function rather than a `From` impl: `PluginTask` and `TaskSpec` both come from
/// other crates, so the orphan rule rejects the impl here.
fn plugin_task(spec: &TaskSpec) -> PluginTask {
    PluginTask {
        task_id: spec.task_id.as_str().to_owned(),
        title: spec.title.clone(),
        instruction: spec.instruction.clone(),
        workspace: spec.workspace.clone(),
    }
}

/// How a run ended.
#[derive(Debug)]
enum Settled {
    Reported(PluginOutcome),
    /// A human cancelled, so the host's own answer is ignored.
    Cancelled,
    Broken(String),
}

/// A running plugin host and what it has produced so far.
struct Spawned {
    child: Child,
    stdin: Option<ChildStdin>,
    frames: mpsc::Receiver<String>,
    reader: Option<thread::JoinHandle<()>>,
    // Kept alive for the whole run: the child reads this file as it starts.
    _host_file: HostFile,
    timeout: Duration,
    // How long a cancelled host may run before it is killed. A field so a test can
    // shorten it: the real value is five seconds, which no test should have to wait.
    grace: Duration,
    end: Option<PluginOutcome>,
    broken: Option<String>,
    timed_out: bool,
}

impl Spawned {
    fn start(
        node: &Path,
        entry: &Path,
        cwd: &Path,
        timeout: Duration,
    ) -> Result<Self, String> {
        let host_file = write_host_script().map_err(|error| format!("{SPAWN_FAILED}: {error}"))?;
        // The text bound travels with the options, so the protocol constant is the only
        // place the number is written.
        let options = serde_json::json!({
            "timeout_secs": timeout.as_secs(),
            "max_text_bytes": mokmok_plugin_protocol::MAX_SUMMARY_BYTES,
        });
        let mut child = Command::new(node)
            .arg(&host_file.path)
            .arg(entry)
            .arg(options.to_string())
            .current_dir(cwd)
            // The plugin's own logging is the daemon's log, never a protocol frame.
            .stdout(Stdio::piped())
            .stdin(Stdio::piped())
            .stderr(Stdio::inherit())
            // A plugin is steered by task text, so the daemon's own bearer token must not
            // be readable from its environment.
            .env_remove("MOKMOK_CONTROL_TOKEN")
            .spawn()
            .map_err(|error| format!("{SPAWN_FAILED} ({}): {error}", node.display()))?;
        let stdin = child.stdin.take();
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| format!("{SPAWN_FAILED}: the host has no stdout"))?;
        let (sender, frames) = mpsc::channel();
        let reader = thread::spawn(move || {
            pump_lines(stdout, |text| sender.send(text).is_ok());
        });
        Ok(Self {
            child,
            stdin,
            frames,
            reader: Some(reader),
            _host_file: host_file,
            timeout,
            grace: CANCEL_GRACE,
            end: None,
            broken: None,
            timed_out: false,
        })
    }

    fn begin(
        &mut self,
        task: &PluginTask,
    ) -> Result<(), String> {
        self.write_frame(&line(&DaemonFrame::Begin { task: task.clone() }))
    }

    fn cancel(&mut self) {
        let _ = self.write_frame(&line(&DaemonFrame::Cancel));
    }

    fn write_frame(
        &mut self,
        framed: &str,
    ) -> Result<(), String> {
        let Some(stdin) = self.stdin.as_mut() else {
            return Err(format!("{SPAWN_FAILED}: the host has no stdin"));
        };
        stdin
            .write_all(framed.as_bytes())
            .and_then(|()| stdin.flush())
            .map_err(|error| format!("{SPAWN_FAILED}: {error}"))
    }

    /// Reads frames until the host ends, dies, is cancelled, or runs out of time.
    ///
    fn pump(
        &mut self,
        reporter: &dyn Reporter,
    ) {
        let deadline = Instant::now() + self.timeout;
        let mut cancelled_at: Option<Instant> = None;
        loop {
            let now = Instant::now();
            match next_step(
                deadline,
                cancelled_at,
                now,
                reporter.cancelled(),
                self.grace,
            ) {
                Step::TimedOut => {
                    self.timed_out = true;
                    self.kill();
                    return;
                },
                Step::CancelNow => {
                    self.cancel();
                    cancelled_at = Some(now);
                },
                Step::Kill => {
                    self.kill();
                    return;
                },
                Step::Wait(_) | Step::KeepGoing => {},
            }
            let wait = (deadline - now)
                .min(CANCEL_POLL)
                .max(Duration::from_millis(1));
            match self.frames.recv_timeout(wait) {
                Ok(text) => {
                    let Some(frame) = self.read_frame(&text) else {
                        continue;
                    };
                    match frame {
                        HostFrame::Event { event } => {
                            // A journal failure is not the task's failure, matching the
                            // ACP runner: the task's status comes from the outcome.
                            let _ = reporter.update(event.to_journal());
                        },
                        HostFrame::End { result } => {
                            self.end = Some(result);
                            return;
                        },
                    }
                },
                Err(RecvTimeoutError::Timeout) => {},
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    }

    /// Parses one line, remembering the first bad one so the run can say why.
    fn read_frame(
        &mut self,
        text: &str,
    ) -> Option<HostFrame> {
        match parse_frame(text.trim_end_matches(['\n', '\r'])) {
            Ok(frame) => Some(frame),
            Err(error) => {
                self.broken.get_or_insert_with(|| error.to_string());
                None
            },
        }
    }

    fn kill(&mut self) {
        // Closing stdin ends a host waiting for another frame, and the kill ends one that
        // is not reading.
        self.stdin.take();
        let _ = self.child.kill();
    }

    /// Waits for the child until `deadline`.
    ///
    /// A child that outlived a kill (a stuck kernel wait, a zombie parent) must not hold
    /// the worker thread, so the wait is bounded and a timeout is reported as a run that
    /// did not exit cleanly.
    fn wait_until(
        &mut self,
        deadline: Instant,
    ) -> std::io::Result<std::process::ExitStatus> {
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => return Ok(status),
                Ok(None) => {},
                Err(error) => return Err(error),
            }
            if Instant::now() >= deadline {
                return Err(std::io::Error::other(
                    "the plugin host did not exit after it was killed",
                ));
            }
            thread::sleep(CANCEL_POLL);
        }
    }

    /// Runs the pump and maps what happened to how the task ended, then reaps the child.
    ///
    /// The run is bounded in two places. The pump stops at the entry's timeout and at
    /// [`CANCEL_GRACE`] after a cancel. This then waits at most [`CANCEL_GRACE`] more for
    /// the child to die, so a host that ignores its kill cannot hold the worker thread.
    fn finish(
        &mut self,
        reporter: &dyn Reporter,
    ) -> Settled {
        self.pump(reporter);
        self.stdin.take();
        // Any pump path that ends without an end frame leaves the child either exited or
        // stuck; killing an exited child is a no-op error, and killing a stuck one is
        // what stops the wait from blocking the worker thread for the host's lifetime.
        if self.end.is_none() {
            self.kill();
        }
        let status = self.wait_until(Instant::now() + self.grace);
        // The reader ends when the child's stdout closes. A child that outlived the kill
        // still holds it open, so joining would block for as long as that child runs; the
        // run is already reported broken, and the thread dies with the process.
        if status.is_ok()
            && let Some(reader) = self.reader.take()
        {
            let _ = reader.join();
        }
        // A cancel the host reported and one the daemon sent both mean the human already
        // committed the task's status, so that answer wins over whatever the child said.
        if reporter.cancelled() {
            return Settled::Cancelled;
        }
        if self.timed_out {
            return Settled::Broken(format!(
                "the plugin exceeded its {} second timeout",
                self.timeout.as_secs()
            ));
        }
        if let Some(result) = self.end.take() {
            return Settled::Reported(result);
        }
        if let Some(reason) = self.broken.take() {
            return Settled::Broken(reason);
        }
        Settled::Broken(exit_reason(status))
    }
}

fn exit_reason(status: std::io::Result<std::process::ExitStatus>) -> String {
    match status {
        Ok(status) if status.success() => format!("{HOST_EXITED}: it exited 0 with no end frame"),
        Ok(status) => status.code().map_or_else(
            || format!("{HOST_EXITED}: it was killed by a signal"),
            |code| format!("{HOST_EXITED} (status {code})"),
        ),
        Err(error) => format!("{HOST_EXITED}: {error}"),
    }
}

/// Reads `reader` line by line, handing each line to `accept` until it refuses.
///
/// A line is read up to one byte past [`MAX_FRAME_BYTES`], so a line at the limit arrives
/// whole and an overlong one is recognisable rather than truncated into something that
/// might parse. `read_until` retries an interrupted read itself, so any error that reaches
/// here is the end of the stream.
fn pump_lines<R: std::io::Read>(
    reader: R,
    mut accept: impl FnMut(String) -> bool,
) {
    let mut reader = BufReader::with_capacity(8 * 1024, reader);
    let mut buffer = Vec::new();
    loop {
        buffer.clear();
        match reader
            .by_ref()
            .take(MAX_FRAME_BYTES as u64 + 1)
            .read_until(b'\n', &mut buffer)
        {
            // End of stream, or a read error that `read_until` did not retry: either way
            // the host has stopped writing.
            Ok(0) | Err(_) => return,
            Ok(_) => {
                let text = String::from_utf8_lossy(&buffer).into_owned();
                if !accept(text) {
                    return;
                }
            },
        }
    }
}

/// Whether a cancel sent at `cancelled_at` has waited out its grace by `now`.
///
/// A run that was never cancelled has no grace to expire.
/// What the pump does on one turn of its loop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    /// The run passed its deadline; kill the host and report a timeout.
    TimedOut,
    /// A human cancelled and the host has not been told yet.
    CancelNow,
    /// The host has had its grace after the cancel; kill it.
    Kill,
    /// Nothing to do yet, so keep looping until the deadline.
    KeepGoing,
    /// Wait for a frame until then.
    Wait(Duration),
}

/// The pump's policy, as a pure function of its clock and the human's intent.
///
/// The deadline is decided before the cancel, so a run that hits both reports the
/// timeout. A cancel is sent once; after that the grace runs, and expiring it kills the
/// host whether or not the human is still marked as having cancelled.
fn next_step(
    deadline: Instant,
    cancelled_at: Option<Instant>,
    now: Instant,
    human_cancelled: bool,
    grace: Duration,
) -> Step {
    if now >= deadline {
        return Step::TimedOut;
    }
    if cancelled_at.is_none() {
        return if human_cancelled {
            Step::CancelNow
        } else {
            Step::Wait((deadline - now).min(CANCEL_POLL))
        };
    }
    if cancelled_at.is_some_and(|at| now.saturating_duration_since(at) >= grace) {
        return Step::Kill;
    }
    Step::KeepGoing
}

/// A host script on disk, removed when the run ends.
struct HostFile {
    path: PathBuf,
}

impl Drop for HostFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Writes the embedded host script to a temp file of its own.
///
/// Node needs a real path to run, and each run gets a unique one so a concurrent run
/// cannot read or remove another's.
fn write_host_script() -> std::io::Result<HostFile> {
    let mut path = std::env::temp_dir();
    path.push(format!("mokmok-plugin-host-{}.mjs", Uuid::now_v7()));
    let mut file = std::fs::File::create(&path)?;
    file.write_all(HOST_SCRIPT.as_bytes())?;
    Ok(HostFile { path })
}

#[cfg(test)]
mod tests {
    //! The pieces the runner tests cannot reach through a real Node process.
    //!
    //! A real host never interrupts a read, never exits in a shape the runner
    //! misreports, and never leaves a temp file behind in a way a test can see. These
    //! cover those directly.

    use std::io::{Error, ErrorKind, Read};

    use super::*;

    #[test]
    fn every_line_arrives_whole() {
        let mut lines = Vec::new();
        pump_lines(&b"one\ntwo\nthree\n"[..], |line| {
            lines.push(line);
            true
        });
        assert_eq!(lines, vec!["one\n", "two\n", "three\n"]);
    }

    #[test]
    fn a_final_line_without_a_newline_still_arrives() {
        // A host killed mid-write leaves its last line unterminated; the daemon has to
        // see it to report an invalid frame rather than silently losing the tail.
        let mut lines = Vec::new();
        pump_lines(&b"one\ntwo"[..], |line| {
            lines.push(line);
            true
        });
        assert_eq!(lines, vec!["one\n", "two"]);
    }

    #[test]
    fn the_pump_stops_when_the_sink_refuses() {
        // This is how a reader thread ends when the run is already over.
        let mut count = 0;
        pump_lines(&b"a\nb\nc\n"[..], |_| {
            count += 1;
            count < 2
        });
        assert_eq!(count, 2);
    }

    #[test]
    fn a_line_over_the_limit_is_recognised_rather_than_split() {
        // One byte past the limit still arrives as a single line, so the parse rejects it
        // as oversized instead of treating the tail as another frame.
        let over = vec![b'x'; MAX_FRAME_BYTES + 1];
        let mut lengths = Vec::new();
        pump_lines(&over[..], |line| {
            lengths.push(line.len());
            true
        });
        assert_eq!(lengths, vec![MAX_FRAME_BYTES + 1]);
    }

    #[test]
    fn a_line_at_the_limit_arrives_untruncated() {
        let exact = vec![b'y'; MAX_FRAME_BYTES];
        let mut lengths = Vec::new();
        pump_lines(&exact[..], |line| {
            lengths.push(line.len());
            true
        });
        assert_eq!(lengths, vec![MAX_FRAME_BYTES]);
    }

    struct FailingThenLine {
        kind: ErrorKind,
        failed: bool,
        served: bool,
    }

    impl Read for FailingThenLine {
        fn read(
            &mut self,
            buffer: &mut [u8],
        ) -> std::io::Result<usize> {
            if self.served {
                return Ok(0);
            }
            if self.failed {
                let line = b"line\n";
                buffer[..line.len()].copy_from_slice(line);
                self.served = true;
                return Ok(line.len());
            }
            self.failed = true;
            Err(Error::new(self.kind, "the probe failed"))
        }
    }

    fn pump_after_error(kind: ErrorKind) -> Vec<String> {
        let mut lines = Vec::new();
        pump_lines(
            FailingThenLine {
                kind,
                failed: false,
                served: false,
            },
            |line| {
                lines.push(line);
                true
            },
        );
        lines
    }

    #[test]
    fn an_interrupted_read_is_retried_rather_than_ending_the_stream() {
        // A signal is not the end of the stream, so the line after it still arrives.
        assert_eq!(pump_after_error(ErrorKind::Interrupted), vec!["line\n"]);
    }

    #[test]
    fn a_real_error_ends_the_pump() {
        assert_eq!(pump_after_error(ErrorKind::BrokenPipe), [] as [String; 0]);
    }

    #[cfg(unix)]
    fn status(code: i32) -> std::process::ExitStatus {
        use std::os::unix::process::ExitStatusExt as _;
        std::process::ExitStatus::from_raw(code << 8)
    }

    #[cfg(unix)]
    #[test]
    fn a_clean_exit_with_no_end_frame_says_so() {
        let reason = exit_reason(Ok(status(0)));
        assert!(reason.contains("exited 0 with no end frame"), "{reason}");
    }

    #[cfg(unix)]
    #[test]
    fn a_nonzero_exit_reports_its_status() {
        let reason = exit_reason(Ok(status(3)));
        assert!(reason.contains("status 3"), "{reason}");
        assert!(!reason.contains("exited 0"), "{reason}");
    }

    #[cfg(unix)]
    #[test]
    fn a_signalled_death_says_a_signal_killed_it() {
        use std::os::unix::process::ExitStatusExt as _;
        let reason = exit_reason(Ok(std::process::ExitStatus::from_raw(9)));
        assert!(reason.contains("killed by a signal"), "{reason}");
    }

    #[test]
    fn a_wait_error_is_reported_with_its_message() {
        let reason = exit_reason(Err(Error::other("no such process")));
        assert!(reason.contains("no such process"), "{reason}");
    }

    #[test]
    fn a_host_script_is_removed_when_its_run_ends() {
        let file = write_host_script().expect("the host script is written");
        let path = file.path.clone();
        assert!(path.is_file(), "the script exists while the run holds it");
        drop(file);
        assert!(!path.exists(), "the script is gone after the run");
    }

    #[test]
    fn each_run_gets_its_own_host_script() {
        // Two concurrent runs must not share a path, or one would delete the other's.
        let first = write_host_script().expect("the first host script is written");
        let second = write_host_script().expect("the second host script is written");
        assert_ne!(first.path, second.path);
    }

    #[test]
    fn the_written_host_script_is_the_embedded_one() {
        let file = write_host_script().expect("the host script is written");
        let written = std::fs::read_to_string(&file.path).expect("the script reads");
        assert_eq!(written, HOST_SCRIPT);
        assert!(
            written.contains("host.mjs") || written.contains("process.stdin"),
            "the embedded script is the Node host"
        );
    }

    #[test]
    fn both_runners_name_the_default_timeout_differently() {
        // The daemon's default and the ACP runner's are separate policy; this pins the
        // value a plugin gets when its entry names none.
        assert_eq!(DEFAULT_PLUGIN_TIMEOUT, Duration::from_secs(600));
    }

    #[test]
    fn the_cancel_grace_is_short_enough_to_be_a_bound() {
        // The host settles its own cancel, so this only has to catch a stuck host.
        assert!(CANCEL_GRACE > CANCEL_POLL);
        assert!(CANCEL_GRACE <= Duration::from_secs(30));
    }

    #[test]
    fn a_host_carries_the_node_it_was_given() {
        let host = PluginHost::new("/usr/bin/node");
        assert_eq!(host.node, PathBuf::from("/usr/bin/node"));
        assert_eq!(host.timeout, None);
        let bounded = host.with_timeout(Duration::from_secs(5));
        assert_eq!(bounded.timeout, Some(Duration::from_secs(5)));
    }

    #[test]
    fn debugging_a_host_names_the_node_and_the_timeout() {
        let text = format!("{:?}", PluginHost::new("/usr/bin/node"));
        assert!(text.contains("/usr/bin/node"), "{text}");
        assert!(text.contains("timeout"), "{text}");
    }
}

#[cfg(test)]
mod process_tests {
    //! The parts of the runner that only a real child process can show: that a cancel
    //! reaches the host as a frame, and that a host which ignores every other signal is
    //! killed.

    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    use super::*;
    use crate::supervisor::Reporter;

    /// A grace short enough that the suite, and every mutant run of it, stays fast.
    ///
    /// The production five seconds is the same code path; only the length changes.
    const FAST_GRACE: Duration = Duration::from_millis(100);

    /// Records what the runner told it, and reports a cancel on demand.
    struct Watcher {
        cancelled: std::sync::atomic::AtomicBool,
        events: std::sync::Mutex<Vec<mokmok_journal::JournalEvent>>,
    }

    impl Watcher {
        fn new() -> Self {
            Self {
                cancelled: std::sync::atomic::AtomicBool::new(false),
                events: std::sync::Mutex::new(Vec::new()),
            }
        }

        fn stop(&self) {
            self.cancelled
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }

        fn events(&self) -> Vec<mokmok_journal::JournalEvent> {
            self.events
                .lock()
                .expect("the watcher is not poisoned")
                .clone()
        }
    }

    impl Reporter for Watcher {
        fn started(
            &self,
            _session: &str,
        ) -> Result<(), String> {
            Ok(())
        }

        fn update(
            &self,
            event: mokmok_journal::JournalEvent,
        ) -> Result<(), String> {
            self.events
                .lock()
                .expect("the watcher is not poisoned")
                .push(event);
            Ok(())
        }

        fn cancelled(&self) -> bool {
            self.cancelled.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    /// Starts `sh` with a script and wraps it the way `Spawned::start` wraps Node.
    ///
    /// Only the child and its pipes matter here; the host script itself is covered by the
    /// Node tests. `sh` is guaranteed present everywhere the daemon runs.
    fn spawned(
        script: &str,
        timeout: Duration,
    ) -> Spawned {
        let host_file = write_host_script().expect("a host file is available");
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("sh starts");
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().expect("the child has stdout");
        let (sender, frames) = mpsc::channel();
        let reader = thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if sender.send(format!("{line}\n")).is_err() {
                    return;
                }
            }
        });
        Spawned {
            child,
            stdin,
            frames,
            reader: Some(reader),
            _host_file: host_file,
            timeout,
            grace: CANCEL_GRACE,
            end: None,
            broken: None,
            timed_out: false,
        }
    }

    /// Runs `finish` with a deadline, so a runner that never kills its child fails here
    /// instead of hanging the suite.
    fn finish_within(
        run: &mut Spawned,
        watcher: &Watcher,
        deadline: Duration,
    ) -> Settled {
        let (sender, received) = std::sync::mpsc::channel();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                let settled = run.finish(watcher);
                let _ = sender.send(settled);
            });
            // A stuck run keeps its thread, and the panic ends the process, so the probe
            // cannot turn into a hang.
            received.recv_timeout(deadline).unwrap_or_else(|error| {
                panic!("the runner never returned within {deadline:?}: {error}")
            })
        })
    }

    #[test]
    fn a_cancel_reaches_the_host_as_a_frame() {
        // The script answers only after it reads a line, so the frame the runner wrote is
        // the only thing that can make it produce an end frame.
        let mut run = spawned(
            "read line; echo '{\"type\":\"end\",\"result\":{\"kind\":\"cancelled\"}}'",
            Duration::from_secs(10),
        );
        let watcher = Watcher::new();
        watcher.stop();
        run.pump(&watcher);
        assert!(
            matches!(run.end, Some(PluginOutcome::Cancelled)),
            "the host answered the cancel frame, so it received one"
        );
    }

    #[test]
    fn a_cancelled_run_is_reported_cancelled() {
        let mut run = spawned("read line; sleep 30", Duration::from_secs(10));
        run.grace = FAST_GRACE;
        let watcher = Watcher::new();
        watcher.stop();
        assert!(matches!(
            finish_within(&mut run, &watcher, Duration::from_secs(20)),
            Settled::Cancelled
        ));
    }

    #[test]
    fn a_host_that_ignores_the_cancel_is_killed_after_the_grace() {
        // The script never reads stdin and never exits, so only the grace can end it.
        let started = Instant::now();
        let mut run = spawned("trap '' TERM INT; sleep 300", Duration::from_secs(120));
        run.grace = FAST_GRACE;
        let watcher = Watcher::new();
        watcher.stop();
        let settled = finish_within(&mut run, &watcher, Duration::from_secs(30));
        assert!(matches!(settled, Settled::Cancelled), "{settled:?}");
        assert!(
            started.elapsed() < FAST_GRACE + Duration::from_secs(10),
            "the grace bounded the run, took {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn a_host_that_runs_past_its_deadline_is_killed() {
        let started = Instant::now();
        let mut run = spawned("trap '' TERM INT; sleep 300", Duration::from_millis(200));
        let watcher = Watcher::new();
        let settled = finish_within(&mut run, &watcher, Duration::from_secs(30));
        assert!(
            matches!(&settled, Settled::Broken(reason) if reason.contains("timeout")),
            "{settled:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "the timeout bounded the run, took {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn a_host_that_exits_without_an_end_frame_reports_its_status() {
        let mut run = spawned(
            "echo '{\"type\":\"event\",\"event\":{\"kind\":\"message\",\"text\":\"hi\"}}'; exit 4",
            Duration::from_secs(10),
        );
        let watcher = Watcher::new();
        let settled = run.finish(&watcher);
        assert!(
            matches!(&settled, Settled::Broken(reason) if reason.contains("status 4")),
            "{settled:?}"
        );
        assert_eq!(watcher.events().len(), 1);
    }

    #[test]
    fn an_invalid_line_from_a_host_is_reported() {
        let mut run = spawned("echo 'not a frame'; exit 0", Duration::from_secs(10));
        let watcher = Watcher::new();
        let settled = run.finish(&watcher);
        assert!(
            matches!(&settled, Settled::Broken(reason) if reason.contains("not a host frame")),
            "{settled:?}"
        );
    }
}

#[cfg(test)]
mod timing_tests {
    //! The pump's policy, isolated from the process that runs it.

    use super::*;

    fn at(
        start: Instant,
        offset: Duration,
    ) -> Instant {
        start + offset
    }

    #[test]
    fn a_run_inside_its_deadline_waits_for_a_frame() {
        let start = Instant::now();
        let deadline = at(start, Duration::from_secs(60));
        assert_eq!(
            next_step(deadline, None, start, false, CANCEL_GRACE),
            Step::Wait(CANCEL_POLL)
        );
    }

    #[test]
    fn the_wait_never_passes_the_deadline() {
        let start = Instant::now();
        let deadline = at(start, Duration::from_millis(10));
        assert_eq!(
            next_step(deadline, None, start, false, CANCEL_GRACE),
            Step::Wait(Duration::from_millis(10))
        );
    }

    #[test]
    fn the_deadline_ends_the_run_exactly_when_it_is_reached() {
        let start = Instant::now();
        let deadline = at(start, CANCEL_GRACE);
        assert_eq!(
            next_step(
                deadline,
                None,
                at(deadline, Duration::from_nanos(1)),
                false,
                CANCEL_GRACE
            ),
            Step::TimedOut
        );
        assert_eq!(
            next_step(deadline, None, deadline, false, CANCEL_GRACE),
            Step::TimedOut
        );
    }

    #[test]
    fn a_timeout_wins_over_a_cancel_that_arrives_with_it() {
        // Both conditions true on the same turn: the run reports the timeout, so the
        // reason names the plugin's own slowness rather than a cancel nobody is waiting
        // for.
        let start = Instant::now();
        let deadline = at(start, CANCEL_GRACE);
        assert_eq!(
            next_step(
                deadline,
                Some(at(start, CANCEL_GRACE)),
                deadline,
                true,
                CANCEL_GRACE
            ),
            Step::TimedOut
        );
    }

    #[test]
    fn a_cancel_is_sent_once_then_the_grace_runs() {
        let start = Instant::now();
        let deadline = at(start, Duration::from_secs(60));
        assert_eq!(
            next_step(deadline, None, start, true, CANCEL_GRACE),
            Step::CancelNow,
            "the first turn after a cancel sends the frame"
        );
        assert_eq!(
            next_step(
                deadline,
                Some(start),
                at(start, CANCEL_POLL),
                true,
                CANCEL_GRACE
            ),
            Step::KeepGoing,
            "a second turn does not resend it"
        );
    }

    #[test]
    fn the_grace_kills_the_host_exactly_at_its_bound() {
        let start = Instant::now();
        let deadline = at(start, Duration::from_secs(60));
        let cancelled = Some(start);
        let just_under = CANCEL_GRACE
            .checked_sub(Duration::from_millis(1))
            .expect("the grace is longer than a millisecond");
        assert_eq!(
            next_step(
                deadline,
                cancelled,
                at(start, just_under),
                true,
                CANCEL_GRACE
            ),
            Step::KeepGoing
        );
        assert_eq!(
            next_step(
                deadline,
                cancelled,
                at(start, CANCEL_GRACE),
                true,
                CANCEL_GRACE
            ),
            Step::Kill
        );
    }

    #[test]
    fn the_grace_does_not_wait_for_the_human_to_keep_cancelling() {
        // Once the frame is sent, the grace runs on its own clock. A reporter that stops
        // reporting the cancel must not leave the host running.
        let start = Instant::now();
        let deadline = at(start, Duration::from_secs(60));
        assert_eq!(
            next_step(
                deadline,
                Some(start),
                at(start, CANCEL_GRACE),
                false,
                CANCEL_GRACE
            ),
            Step::Kill
        );
    }

    #[test]
    fn a_cancelled_run_still_ends_at_its_own_deadline() {
        let start = Instant::now();
        let deadline = at(start, Duration::from_secs(60));
        assert_eq!(
            next_step(deadline, Some(start), deadline, false, CANCEL_GRACE),
            Step::TimedOut
        );
    }
}

#[cfg(test)]
mod child_tests {
    //! Killing a child and waiting for it, on a real process.

    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    use super::*;

    /// A child running `script`, with the pipes `Spawned` expects.
    fn child(script: &str) -> Spawned {
        let host_file = write_host_script().expect("a host file is available");
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("sh starts");
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().expect("the child has stdout");
        let (sender, frames) = mpsc::channel();
        let reader = thread::spawn(move || {
            for line in std::io::BufReader::new(stdout)
                .lines()
                .map_while(Result::ok)
            {
                if sender.send(format!("{line}\n")).is_err() {
                    return;
                }
            }
        });
        Spawned {
            child,
            stdin,
            frames,
            reader: Some(reader),
            _host_file: host_file,
            timeout: Duration::from_secs(30),
            grace: CANCEL_GRACE,
            end: None,
            broken: None,
            timed_out: false,
        }
    }

    #[test]
    fn kill_ends_a_child_that_is_not_reading_its_stdin() {
        // The script never reads stdin, so closing the pipe cannot end it; only the signal
        // can.
        let mut run = child("trap '' TERM; sleep 300");
        let started = Instant::now();
        run.kill();
        let status = run
            .wait_until(started + Duration::from_secs(10))
            .expect("the killed child is reaped");
        assert!(
            !status.success(),
            "a killed child does not exit successfully: {status:?}"
        );
    }

    #[test]
    fn kill_ends_a_child_that_is_waiting_on_stdin() {
        // Closing stdin is what ends this one, which is why `kill` does both.
        let mut run = child("cat > /dev/null");
        let started = Instant::now();
        run.kill();
        assert!(
            run.wait_until(started + Duration::from_secs(10)).is_ok(),
            "the child ended once its stdin closed"
        );
    }

    #[test]
    fn waiting_past_the_deadline_reports_a_host_that_did_not_exit() {
        // A child that swallows every signal it can still be stopped by SIGKILL, so this
        // uses one that ignores SIGTERM and is never killed: the wait has to give up.
        let mut run = child("sleep 300");
        let started = Instant::now();
        let result = run.wait_until(started + Duration::from_millis(50));
        assert!(result.is_err(), "the wait gave up on a living child");
        let error = result.expect_err("the wait reported an error");
        assert!(
            error.to_string().contains("did not exit"),
            "the error names the cause: {error}"
        );
        // The child is still alive, so it must be killed by the caller (or here) to let
        // the test end.
        let _ = run.child.kill();
        let _ = run.child.wait();
    }

    #[test]
    fn waiting_returns_an_exited_child_immediately() {
        let mut run = child("exit 0");
        let started = Instant::now();
        let status = run
            .wait_until(started + Duration::from_secs(10))
            .expect("the child already exited");
        assert!(status.success(), "{status:?}");
    }
}

#[cfg(test)]
mod reap_tests {
    //! The reap window, which decides whether a child that exits shortly after its last
    //! frame is reported by its exit status or as a host that never died.

    use std::process::{Command, Stdio};
    use std::time::Duration;

    use super::*;

    struct Silent;

    impl Reporter for Silent {
        fn started(
            &self,
            _session: &str,
        ) -> Result<(), String> {
            Ok(())
        }

        fn update(
            &self,
            _event: mokmok_journal::JournalEvent,
        ) -> Result<(), String> {
            Ok(())
        }

        fn cancelled(&self) -> bool {
            false
        }
    }

    fn child(script: &str) -> Spawned {
        let host_file = write_host_script().expect("a host file is available");
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("sh starts");
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().expect("the child has stdout");
        let (sender, frames) = mpsc::channel();
        let reader = thread::spawn(move || {
            for line in std::io::BufReader::new(stdout)
                .lines()
                .map_while(Result::ok)
            {
                if sender.send(format!("{line}\n")).is_err() {
                    return;
                }
            }
        });
        Spawned {
            child,
            stdin,
            frames,
            reader: Some(reader),
            _host_file: host_file,
            timeout: Duration::from_secs(30),
            grace: CANCEL_GRACE,
            end: None,
            broken: None,
            timed_out: false,
        }
    }

    #[test]
    fn a_child_that_exits_just_after_its_last_frame_is_reaped() {
        // The script writes nothing and exits after a pause, which is the window where the
        // child is alive at the first `try_wait` and gone a moment later. The run must
        // report the exit status, so the reap has to wait rather than give up at once.
        let mut run = child("sleep 0.3; exit 0");
        let settled = run.finish(&Silent);
        let Settled::Broken(reason) = settled else {
            panic!("expected a broken run, got {settled:?}");
        };
        assert!(
            reason.contains("exited 0 with no end frame"),
            "the exit status was read after waiting: {reason}"
        );
    }
}

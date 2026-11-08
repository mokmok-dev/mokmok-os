//! A child process reached over its standard input and output.

use std::ffi::OsString;
use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread::JoinHandle;
use std::time::Duration;

use super::{Transport, TransportError};

/// How to launch an ACP agent as a subprocess.
///
/// ACP messages are newline-delimited JSON on the child's standard input and output. The child's
/// standard error is inherited, so an agent's own logging reaches the caller's terminal. The
/// child otherwise inherits the parent's environment, except `MOKMOK_CONTROL_TOKEN`, which is
/// removed before the spawn so the daemon's own bearer token cannot reach an agent.
#[derive(Clone, Debug)]
pub struct StdioConfig {
    program: OsString,
    args: Vec<OsString>,
    cwd: Option<PathBuf>,
    env: Vec<(OsString, OsString)>,
}

impl StdioConfig {
    /// Starts a configuration for `program`.
    #[must_use]
    pub fn new(program: impl Into<OsString>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            cwd: None,
            env: Vec::new(),
        }
    }

    /// The command Grok Build runs to speak ACP over stdio.
    #[must_use]
    pub fn grok() -> Self {
        Self::new("grok").args(["agent", "stdio"])
    }

    /// Appends one argument.
    #[must_use]
    pub fn arg(
        mut self,
        arg: impl Into<OsString>,
    ) -> Self {
        self.args.push(arg.into());
        self
    }

    /// Appends every argument in `args`.
    #[must_use]
    pub fn args<I, S>(
        mut self,
        args: I,
    ) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    /// Runs the child in `cwd`.
    #[must_use]
    pub fn cwd(
        mut self,
        cwd: impl Into<PathBuf>,
    ) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    /// Adds one environment variable to the child's environment.
    #[must_use]
    pub fn env(
        mut self,
        key: impl Into<OsString>,
        value: impl Into<OsString>,
    ) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    pub(crate) const fn program(&self) -> &OsString {
        &self.program
    }

    pub(crate) fn arguments(&self) -> &[OsString] {
        &self.args
    }

    pub(crate) const fn directory(&self) -> Option<&PathBuf> {
        self.cwd.as_ref()
    }

    pub(crate) fn environment(&self) -> &[(OsString, OsString)] {
        &self.env
    }
}

/// An ACP agent running as a child process.
pub struct StdioTransport {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
    reader: Option<JoinHandle<()>>,
    closed: bool,
}

impl StdioTransport {
    /// Spawns the configured child and starts a reader thread for its standard output.
    ///
    /// # Errors
    ///
    /// Returns [`TransportError::Io`] when the child cannot be spawned or its pipes cannot be
    /// taken.
    pub fn spawn(config: &StdioConfig) -> Result<Self, TransportError> {
        let mut command = Command::new(config.program());
        command.args(config.arguments());
        if let Some(directory) = config.directory() {
            command.current_dir(directory);
        }
        for (key, value) in config.environment() {
            command.env(key, value);
        }
        // A task's text steers this process, so the daemon's own bearer token must not
        // be readable from its environment.
        command.env_remove("MOKMOK_CONTROL_TOKEN");
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        let stdin = child.stdin.take();
        let Some(stdout) = child.stdout.take() else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(TransportError::Io(std::io::Error::other(
                "child standard output is not piped",
            )));
        };
        let (sender, lines) = mpsc::channel();
        let reader = std::thread::spawn(move || forward_lines(stdout, &sender));
        Ok(Self {
            child,
            stdin,
            lines,
            reader: Some(reader),
            closed: false,
        })
    }

    /// The child's process id.
    #[must_use]
    pub fn pid(&self) -> u32 {
        self.child.id()
    }
}

impl Drop for StdioTransport {
    fn drop(&mut self) {
        // The child outlives `stdin` closing on its own, so a dropped transport must
        // kill and reap it rather than leave it behind.
        let _ = self.close();
    }
}

impl Transport for StdioTransport {
    fn send(
        &mut self,
        message: &str,
    ) -> Result<(), TransportError> {
        if self.closed {
            return Err(TransportError::Closed);
        }
        let Some(stdin) = self.stdin.as_mut() else {
            return Err(TransportError::Closed);
        };
        let written = stdin
            .write_all(message.as_bytes())
            .and_then(|()| stdin.write_all(b"\n"))
            .and_then(|()| stdin.flush());
        match written {
            Ok(()) => Ok(()),
            Err(error) => Err(write_error(error)),
        }
    }

    fn recv(&mut self) -> Result<Option<String>, TransportError> {
        Ok(self.lines.recv().ok())
    }

    fn recv_timeout(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<String>, TransportError> {
        match self.lines.recv_timeout(timeout) {
            Ok(line) => Ok(Some(line)),
            Err(RecvTimeoutError::Timeout) => Err(TransportError::Timeout),
            Err(RecvTimeoutError::Disconnected) => Ok(None),
        }
    }

    fn close(&mut self) -> Result<(), TransportError> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;
        self.stdin = None;
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
        Ok(())
    }
}

fn write_error(error: std::io::Error) -> TransportError {
    if error.kind() == ErrorKind::BrokenPipe {
        TransportError::Closed
    } else {
        TransportError::Io(error)
    }
}

fn forward_lines(
    stdout: ChildStdout,
    sender: &mpsc::Sender<String>,
) {
    for line in BufReader::new(stdout).lines() {
        let Ok(line) = line else { break };
        if !line.is_empty() && sender.send(line).is_err() {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spawn(
        program: &str,
        args: &[&str],
    ) -> StdioTransport {
        StdioTransport::spawn(&StdioConfig::new(program).args(args)).expect("spawn")
    }

    #[test]
    fn recv_reads_one_line_then_reports_the_end() {
        let mut transport = spawn("echo", &[r#"{"jsonrpc":"2.0","id":1}"#]);
        assert_eq!(
            transport.recv().expect("line"),
            Some(r#"{"jsonrpc":"2.0","id":1}"#.to_owned())
        );
        assert_eq!(transport.recv().expect("end"), None);
    }

    #[test]
    fn recv_timeout_reports_a_quiet_child_and_still_reads_a_line() {
        let mut quiet = spawn("cat", &[]);
        assert!(matches!(
            quiet.recv_timeout(Duration::from_millis(50)),
            Err(TransportError::Timeout)
        ));
        quiet.close().expect("close");

        let mut transport = spawn("echo", &[r#"{"jsonrpc":"2.0","id":3}"#]);
        assert_eq!(
            transport
                .recv_timeout(Duration::from_secs(2))
                .expect("line"),
            Some(r#"{"jsonrpc":"2.0","id":3}"#.to_owned())
        );
        assert_eq!(
            transport.recv_timeout(Duration::from_secs(2)).expect("end"),
            None
        );
    }

    #[test]
    fn send_writes_one_newline_delimited_line() {
        let mut transport = spawn("cat", &[]);
        let message = r#"{"jsonrpc":"2.0","id":2}"#;
        transport.send(message).expect("sent");
        let echoed = transport
            .lines
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("echo");
        assert_eq!(echoed, message);
        transport.close().expect("close");
    }

    #[test]
    fn a_second_close_is_harmless() {
        let mut transport = spawn("echo", &["x"]);
        assert_eq!(transport.recv().expect("line").as_deref(), Some("x"));
        transport.close().expect("first close");
        transport.close().expect("second close");
    }

    #[test]
    fn send_to_a_departed_child_is_closed() {
        let mut transport = spawn("true", &[]);
        assert_eq!(transport.recv().expect("end"), None);
        assert!(matches!(transport.send("{}"), Err(TransportError::Closed)));
    }

    #[test]
    fn a_broken_pipe_is_closed_and_another_error_is_io() {
        assert!(matches!(
            write_error(std::io::Error::from(ErrorKind::BrokenPipe)),
            TransportError::Closed
        ));
        assert!(matches!(
            write_error(std::io::Error::other("x")),
            TransportError::Io(_)
        ));
    }

    #[test]
    fn the_builders_store_their_values() {
        let config = StdioConfig::new("agent")
            .arg("--one")
            .args(["--two", "--three"])
            .cwd("/tmp")
            .env("KEY", "VALUE");
        assert_eq!(config.program().to_string_lossy().as_ref(), "agent");
        let arguments: Vec<String> = config
            .arguments()
            .iter()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect();
        assert_eq!(arguments, ["--one", "--two", "--three"]);
        assert_eq!(config.directory(), Some(&PathBuf::from("/tmp")));
        assert_eq!(
            config.environment(),
            [(OsString::from("KEY"), OsString::from("VALUE"))]
        );
    }

    #[test]
    fn dropping_the_transport_kills_and_reaps_the_child() {
        let transport = spawn("sh", &["-c", "sleep 30"]);
        let pid = transport.pid();
        assert!(std::path::Path::new(&format!("/proc/{pid}")).exists());
        drop(transport);

        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while std::path::Path::new(&format!("/proc/{pid}")).exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "the child {pid} was not reaped"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn grok_runs_the_stdio_agent() {
        let config = StdioConfig::grok();
        assert_eq!(config.program().to_string_lossy().as_ref(), "grok");
        let arguments: Vec<String> = config
            .arguments()
            .iter()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect();
        assert_eq!(arguments, ["agent", "stdio"]);
    }
}

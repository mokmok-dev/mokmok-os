//! The tool surface: one command, the report a run produces, and the observation it becomes.

use std::time::Duration;

use mokmok_protocol::ExecutionID;
use mokmok_state_machine::{Resolution, TurnOutcome};

use crate::error::Error;
use crate::record::ArtifactRef;

/// How long one command may run when the caller names no deadline.
///
/// The frozen `Task` and `Budget` carry no deadline, and the runtime owns no clock, so the
/// deadline is a constant here until a caller with a clock supplies one.
pub const DEFAULT_DEADLINE: Duration = Duration::from_secs(120);

/// One command as the runtime names it: a command string and no exposure.
///
/// No cwd, no argument vector, and no sandbox type crosses this boundary: the workspace is fresh
/// and ephemeral per execution, and a command reaches the checkout by `cd`-ing in its own shell.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct ToolCall {
    pub execution: ExecutionID,
    pub command: String,
    pub deadline: Duration,
}

/// How one attempt at the tool ended.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Exit {
    /// The workload exited on its own with this code.
    Finished { code: i32 },
    /// A signal ended the workload with this number.
    Killed { signal: i32 },
    /// The execution's deadline passed while it was still running.
    TimedOut,
    /// The workload never ran: authority refused it, or confinement did.
    Refused,
    /// The supervisor could not prove what the workload did.
    Unknown,
}

impl Exit {
    /// The settlement this exit records.
    #[must_use]
    pub const fn resolution(self) -> Resolution {
        match self {
            Self::Finished { code } => Resolution::Finished { code },
            Self::Killed { signal } => Resolution::Killed { signal },
            Self::TimedOut => Resolution::TimedOut,
            Self::Refused => Resolution::Refused,
            Self::Unknown => Resolution::Unknown,
        }
    }

    /// How this exit reads as the turn's outcome.
    ///
    /// The mapping the kernel's own slice records for a tool, so a refused, killed, timed-out, or
    /// unproven command never reads later as a turn that reached its goal.
    #[must_use]
    pub const fn outcome(self) -> TurnOutcome {
        match self {
            Self::Finished { .. } => TurnOutcome::Completed,
            Self::Killed { .. } | Self::TimedOut | Self::Refused | Self::Unknown => {
                TurnOutcome::Failed
            },
        }
    }
}

/// What one execution produced, in the runtime's own vocabulary.
///
/// The bytes live in the artifact store; this holds the references that keep them alive.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct ToolReport {
    pub exit: Exit,
    /// Whether the confiner reported reaching the exec call, which is all a supervisor can observe.
    pub attempted: bool,
    pub stdout: ArtifactRef,
    pub stderr: ArtifactRef,
    /// The bounded rendering of this execution's outcome, produced by whoever holds the bytes.
    ///
    /// A report carries what the model will be shown, so the captured bytes never enter the log or
    /// the fold: a durable record holds this text and the references, not the streams themselves.
    pub observation: String,
}

/// The one tool the model is offered, in the runtime's vocabulary.
pub trait Tools: Sync {
    /// Decides authority, confines, runs, stores the bytes, and reports.
    ///
    /// A caller cannot name an exposure or a program: the operator's grants are the whole set.
    fn invoke(
        &self,
        call: &ToolCall,
    ) -> impl Future<Output = Result<ToolReport, Error>> + Send;
}

/// How many bytes of one stream are shown before the middle is elided.
const STREAM_BYTES: usize = 2_048;

/// The warning an unproven outcome becomes.
///
/// `Resolution::Unknown` never has a result, so this is a message about a fact rather than a
/// rendering of bytes. It is what the next turn's durable prompt carries, which is what makes an
/// interrupted command's effect impossible to assume.
pub const UNKNOWN_WARNING: &str = "the runner could not prove this command's outcome; the checkout \
     may have been changed, so do not assume the command took effect";

/// Renders what the model is shown for one execution's outcome.
///
/// An unproven outcome is the constant warning; a proven one is the exit and the captured output,
/// clipped head-and-tail, with the full bytes still addressable by the hash a report names. A caller
/// that holds no bytes passes empty streams and gets the exit or the warning alone.
#[must_use]
pub fn observation(
    resolution: Resolution,
    stdout: &[u8],
    stderr: &[u8],
) -> String {
    if resolution == Resolution::Unknown {
        return UNKNOWN_WARNING.to_owned();
    }

    let mut text = render_resolution(resolution);
    render_stream(&mut text, "stdout", stdout);
    render_stream(&mut text, "stderr", stderr);

    text
}

fn render_resolution(resolution: Resolution) -> String {
    match resolution {
        Resolution::Finished { code } => format!("exit code {code}"),
        Resolution::Killed { signal } => format!("killed by signal {signal}"),
        Resolution::TimedOut => "timed out".to_owned(),
        Resolution::Cancelled => "cancelled".to_owned(),
        Resolution::Refused => "refused".to_owned(),
        Resolution::Applied => "applied".to_owned(),
        Resolution::Unknown => UNKNOWN_WARNING.to_owned(),
    }
}

fn render_stream(
    text: &mut String,
    name: &str,
    bytes: &[u8],
) {
    if bytes.is_empty() {
        return;
    }

    text.push('\n');
    text.push_str(name);
    text.push_str(": ");
    text.push_str(&clip(bytes, STREAM_BYTES));
}

/// The head and tail of `bytes` as text, with the elided middle named by its size.
///
/// The whole byte sequence stays addressable by hash, so the clip costs the model no access.
#[must_use]
pub fn clip(
    bytes: &[u8],
    limit: usize,
) -> String {
    if bytes.len() <= limit {
        return String::from_utf8_lossy(bytes).into_owned();
    }

    let head = limit / 2;
    let tail = limit - head;
    let elided = bytes.len() - limit;
    format!(
        "{}\n… {elided} bytes elided …\n{}",
        String::from_utf8_lossy(&bytes[..head]),
        String::from_utf8_lossy(&bytes[bytes.len() - tail..]),
    )
}

#[cfg(test)]
mod tests {
    use super::{Exit, UNKNOWN_WARNING, observation};
    use mokmok_state_machine::{Resolution, TurnOutcome};

    #[test]
    fn only_a_command_that_finished_reads_as_a_completed_turn() {
        assert_eq!(Exit::Finished { code: 0 }.outcome(), TurnOutcome::Completed,);
        assert_eq!(
            Exit::Finished { code: 101 }.outcome(),
            TurnOutcome::Completed,
        );
        assert_eq!(Exit::Killed { signal: 9 }.outcome(), TurnOutcome::Failed);
        assert_eq!(Exit::TimedOut.outcome(), TurnOutcome::Failed);
        assert_eq!(Exit::Refused.outcome(), TurnOutcome::Failed);
        assert_eq!(Exit::Unknown.outcome(), TurnOutcome::Failed);
    }

    #[test]
    fn an_unproven_outcome_is_the_warning_rather_than_a_rendering() {
        assert_eq!(
            observation(Resolution::Unknown, &[], &[]),
            super::UNKNOWN_WARNING,
        );
    }

    #[test]
    fn a_proven_outcome_renders_the_exit_and_both_streams() {
        assert_eq!(
            observation(Resolution::Finished { code: 3 }, b"hello", b"oops"),
            "exit code 3\nstdout: hello\nstderr: oops",
        );
    }

    #[test]
    fn every_proven_resolution_renders_its_own_words() {
        assert_eq!(
            observation(Resolution::Finished { code: 7 }, &[], &[]),
            "exit code 7",
        );
        assert_eq!(
            observation(Resolution::Killed { signal: 9 }, &[], &[]),
            "killed by signal 9",
        );
        assert_eq!(observation(Resolution::TimedOut, &[], &[]), "timed out");
        assert_eq!(observation(Resolution::Cancelled, &[], &[]), "cancelled");
        assert_eq!(observation(Resolution::Refused, &[], &[]), "refused");
        assert_eq!(observation(Resolution::Unknown, &[], &[]), UNKNOWN_WARNING,);
    }

    #[test]
    fn a_long_stream_keeps_its_head_its_tail_and_the_size_of_what_is_gone() {
        let mut bytes = vec![b'a'; 2_500];
        bytes.extend(vec![b'b'; 2_500]);

        assert_eq!(
            super::clip(&bytes, 100),
            format!(
                "{}\n… 4900 bytes elided …\n{}",
                "a".repeat(50),
                "b".repeat(50),
            ),
        );
    }
}

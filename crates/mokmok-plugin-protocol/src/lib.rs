//! The contract between the control plane and a TypeScript plugin.
//!
//! One set of types serves three readers. A plugin author sees [`PluginTask`],
//! [`PluginEvent`], and [`PluginOutcome`] through the generated `index.d.ts`, so those
//! carry the napi attributes behind the `napi` feature. The daemon and the Node host
//! speak [`DaemonFrame`] and [`HostFrame`] on the child's pipes. Every type carries its
//! serde shape in both builds, so a plugin's own JSON is the same JSON the daemon
//! parses, and there is no second definition to drift.
//!
//! The wire is newline-delimited JSON in both directions. The daemon writes
//! [`DaemonFrame`] lines to the child's standard input and reads [`HostFrame`] lines from
//! its standard output.
//!
//! The bounds here are the contract's own: a frame line over [`MAX_FRAME_BYTES`] is
//! rejected, and a summary or failure reason is clipped to [`MAX_SUMMARY_BYTES`] and
//! [`MAX_REASON_BYTES`] before it crosses. A plugin cannot make the daemon hold or store
//! an unbounded string.

use mokmok_journal::JournalEvent;
use serde::{Deserialize, Serialize};

/// The longest line the daemon accepts from a plugin host.
///
/// A host that writes more is failing, not sending data: the daemon fails the task
/// rather than grow its own buffer.
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;

/// The longest summary a completed run carries. The ACP runner uses the same number, so
/// one bound holds whichever runner produced the task.
pub const MAX_SUMMARY_BYTES: usize = 4 * 1024;

/// The longest failure reason a failed run carries, wide enough for a stack trace.
pub const MAX_REASON_BYTES: usize = 4 * 1024;

/// The slice of a task a plugin may see.
///
/// This is what a plugin author reads in `index.d.ts`. `workspace` is absolute when it is
/// present, because a plugin may use it as a working directory.
#[cfg_attr(feature = "napi", napi_derive::napi(object))]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginTask {
    #[cfg_attr(feature = "napi", napi(js_name = "task_id"))]
    pub task_id: String,
    pub title: String,
    pub instruction: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
}

/// One transcript step a plugin emits.
///
/// The four kinds are the ones the UI renders, so a plugin cannot emit a step that has no
/// way to appear.
#[cfg_attr(
    feature = "napi",
    napi_derive::napi(discriminant = "kind", discriminant_case = "snake_case")
)]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PluginEvent {
    Message { text: String },
    Thought { text: String },
    ToolCall { title: String, status: String },
    Permission { title: String, status: String },
}

/// What a plugin's default export resolves to.
///
/// There is no `failed` variant: a plugin reports a failure by throwing, and the host
/// turns the thrown value into a failed outcome. An author has one way to say each thing.
#[cfg_attr(
    feature = "napi",
    napi_derive::napi(discriminant = "kind", discriminant_case = "snake_case")
)]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PluginResult {
    Completed { summary: String },
    Cancelled,
}

/// How a run ended, as the host reports it.
///
/// The host fills in `failed`, which a plugin cannot return, so this is the superset the
/// daemon reads and the author contract is the result type below.
#[cfg_attr(
    feature = "napi",
    napi_derive::napi(discriminant = "kind", discriminant_case = "snake_case")
)]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PluginOutcome {
    Completed { summary: String },
    Failed { reason: String },
    Cancelled,
}

/// One line the daemon writes to the plugin host's standard input.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DaemonFrame {
    /// Hand the run to the plugin. The daemon writes this once, first.
    Begin { task: PluginTask },
    /// Stop the run. A host ignores a second one, so the daemon may repeat it.
    Cancel,
}

/// One line the plugin host writes to its standard output.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HostFrame {
    /// One transcript step, at any point after `Begin`.
    Event { event: PluginEvent },
    /// The run ended. A host writes this once, last; the daemon stops reading after it.
    End { result: PluginOutcome },
}

impl PluginEvent {
    /// Returns this step in the journal's shape.
    ///
    /// The two types are separate on purpose: the author contract must not change when
    /// the journal does. This is the one place that decides they still agree.
    #[must_use]
    pub fn to_journal(&self) -> JournalEvent {
        match self {
            Self::Message { text } => JournalEvent::Message { text: text.clone() },
            Self::Thought { text } => JournalEvent::Thought { text: text.clone() },
            Self::ToolCall { title, status } => JournalEvent::ToolCall {
                title: title.clone(),
                status: status.clone(),
            },
            Self::Permission { title, status } => JournalEvent::Permission {
                title: title.clone(),
                status: status.clone(),
            },
        }
    }
}

impl PluginOutcome {
    #[must_use]
    pub fn clipped(&self) -> Self {
        match self {
            Self::Completed { summary } => Self::Completed {
                summary: clip(summary, MAX_SUMMARY_BYTES),
            },
            Self::Failed { reason } => Self::Failed {
                reason: clip(reason, MAX_REASON_BYTES),
            },
            Self::Cancelled => Self::Cancelled,
        }
    }
}

fn clip(
    text: &str,
    max_bytes: usize,
) -> String {
    text[..text.floor_char_boundary(max_bytes)].to_owned()
}

/// One newline-terminated protocol line.
///
/// The newline is part of the line, because a write that omits it leaves the reader
/// waiting, and the caller should not have to remember that.
#[must_use]
pub fn line<T: Serialize>(frame: &T) -> String {
    let mut text = serde_json::to_string(frame).unwrap_or_else(|error| {
        // Every frame here is plain data with no map keys to reject, so this is a
        // programming error rather than an input problem, and a malformed line would
        // fail the task with a misleading reason.
        unreachable!("a protocol frame always encodes: {error}")
    });
    text.push('\n');
    text
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FrameError {
    /// The line is not JSON, or not the shape of a [`HostFrame`].
    Invalid(String),
    /// The line is longer than [`MAX_FRAME_BYTES`].
    TooLong { bytes: usize },
}

impl std::fmt::Display for FrameError {
    fn fmt(
        &self,
        formatter: &mut std::fmt::Formatter<'_>,
    ) -> std::fmt::Result {
        match self {
            Self::Invalid(reason) => {
                write!(
                    formatter,
                    "the plugin wrote a line that is not a host frame: {reason}"
                )
            },
            Self::TooLong { bytes } => write!(
                formatter,
                "the plugin wrote a {bytes}-byte line, over the {MAX_FRAME_BYTES}-byte limit"
            ),
        }
    }
}

/// Parses one line the plugin host wrote.
///
/// This is the boundary where a child's untyped output becomes a typed frame. Anything
/// the host does not recognise is an error here, never a silently skipped line.
///
/// # Errors
///
/// Returns [`FrameError::TooLong`] for a line over [`MAX_FRAME_BYTES`], and
/// [`FrameError::Invalid`] for a line that is not JSON or not a [`HostFrame`].
pub fn parse_frame(line: &str) -> Result<HostFrame, FrameError> {
    if line.len() > MAX_FRAME_BYTES {
        return Err(FrameError::TooLong { bytes: line.len() });
    }
    serde_json::from_str(line).map_err(|error| FrameError::Invalid(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task() -> PluginTask {
        PluginTask {
            task_id: "018f".to_owned(),
            title: "Say hello".to_owned(),
            instruction: "Greet the user.".to_owned(),
            workspace: Some("/work/mokmok".to_owned()),
        }
    }

    #[test]
    fn a_task_carries_the_wire_names_the_daemon_reads() {
        let json = serde_json::to_value(task()).expect("the task encodes");
        assert_eq!(
            json,
            serde_json::json!({
                "task_id": "018f",
                "title": "Say hello",
                "instruction": "Greet the user.",
                "workspace": "/work/mokmok"
            })
        );
    }

    #[test]
    fn a_task_without_a_workspace_omits_it() {
        let json = serde_json::to_value(PluginTask {
            workspace: None,
            ..task()
        })
        .expect("the task encodes");
        assert_eq!(json.get("workspace"), None);
    }

    #[test]
    fn every_event_kind_round_trips_and_maps_onto_the_journal() {
        let cases = [
            PluginEvent::Message {
                text: "hello".to_owned(),
            },
            PluginEvent::Thought {
                text: "thinking".to_owned(),
            },
            PluginEvent::ToolCall {
                title: "read".to_owned(),
                status: "completed".to_owned(),
            },
            PluginEvent::Permission {
                title: "write".to_owned(),
                status: "allowed".to_owned(),
            },
        ];
        for event in cases {
            let encoded = serde_json::to_string(&event).expect("the event encodes");
            let decoded: PluginEvent = serde_json::from_str(&encoded).expect("the event decodes");
            assert_eq!(decoded, event);
            let journal = serde_json::to_value(event.to_journal()).expect("the journal encodes");
            assert_eq!(
                journal,
                serde_json::to_value(&event).expect("the event encodes"),
                "the author contract and the journal disagree about {event:?}"
            );
        }
    }

    #[test]
    fn every_event_kind_is_kind_tagged_in_snake_case() {
        let cases = [
            (
                PluginEvent::Message {
                    text: String::new(),
                },
                "message",
            ),
            (
                PluginEvent::Thought {
                    text: String::new(),
                },
                "thought",
            ),
            (
                PluginEvent::ToolCall {
                    title: String::new(),
                    status: String::new(),
                },
                "tool_call",
            ),
            (
                PluginEvent::Permission {
                    title: String::new(),
                    status: String::new(),
                },
                "permission",
            ),
        ];
        for (event, kind) in cases {
            let json = serde_json::to_value(&event).expect("the event encodes");
            assert_eq!(json["kind"], kind);
        }
    }

    #[test]
    fn every_outcome_round_trips_with_its_kind() {
        let cases = [
            (
                PluginOutcome::Completed {
                    summary: "done".to_owned(),
                },
                "completed",
            ),
            (
                PluginOutcome::Failed {
                    reason: "boom".to_owned(),
                },
                "failed",
            ),
            (PluginOutcome::Cancelled, "cancelled"),
        ];
        for (outcome, kind) in cases {
            let encoded = serde_json::to_string(&outcome).expect("the outcome encodes");
            assert_eq!(
                serde_json::from_str::<PluginOutcome>(&encoded).expect("the outcome decodes"),
                outcome
            );
            assert_eq!(
                serde_json::to_value(&outcome).expect("the outcome encodes")["kind"],
                kind
            );
        }
    }

    #[test]
    fn a_plugin_result_has_no_failure_variant() {
        let encoded = serde_json::to_string(&PluginResult::Completed {
            summary: "done".to_owned(),
        })
        .expect("the result encodes");
        assert!(serde_json::from_str::<PluginResult>(&encoded).is_ok());
        assert!(serde_json::from_str::<PluginResult>(r#"{"kind":"failed","reason":"x"}"#).is_err());
    }

    #[test]
    fn a_begin_frame_carries_the_task() {
        let framed = line(&DaemonFrame::Begin { task: task() });
        assert!(framed.ends_with('\n'));
        assert_eq!(framed.matches('\n').count(), 1);
        let json: serde_json::Value = serde_json::from_str(&framed).expect("the frame decodes");
        assert_eq!(json["type"], "begin");
        assert_eq!(json["task"]["task_id"], "018f");
    }

    #[test]
    fn a_cancel_frame_is_one_word_and_idempotent() {
        let framed = line(&DaemonFrame::Cancel);
        assert_eq!(framed, "{\"type\":\"cancel\"}\n");
        assert_eq!(line(&DaemonFrame::Cancel), framed);
    }

    #[test]
    fn every_host_frame_round_trips_through_a_line() {
        let cases = [
            HostFrame::Event {
                event: PluginEvent::Message {
                    text: "hi".to_owned(),
                },
            },
            HostFrame::End {
                result: PluginOutcome::Completed {
                    summary: "done".to_owned(),
                },
            },
            HostFrame::End {
                result: PluginOutcome::Failed {
                    reason: "boom".to_owned(),
                },
            },
            HostFrame::End {
                result: PluginOutcome::Cancelled,
            },
        ];
        for frame in cases {
            let framed = line(&frame);
            assert_eq!(parse_frame(framed.trim_end()), Ok(frame));
        }
    }

    #[test]
    fn an_event_frame_keeps_the_journal_line_the_ui_reads() {
        let framed = line(&HostFrame::Event {
            event: PluginEvent::ToolCall {
                title: "read".to_owned(),
                status: "completed".to_owned(),
            },
        });
        assert_eq!(
            framed,
            "{\"type\":\"event\",\"event\":{\"kind\":\"tool_call\",\"title\":\"read\",\"status\":\"completed\"}}\n"
        );
    }

    #[test]
    fn a_broken_line_is_rejected_rather_than_skipped() {
        for bad in [
            "",
            "not json",
            "{}",
            r#"{"type":"finished"}"#,
            r#"{"type":"event"}"#,
            r#"{"type":"event","event":{"kind":"message"}}"#,
        ] {
            assert!(
                matches!(parse_frame(bad), Err(FrameError::Invalid(_))),
                "{bad:?} should be invalid"
            );
        }
    }

    #[test]
    fn an_oversized_line_is_rejected_before_it_is_parsed() {
        let huge = format!(
            "{{\"type\":\"event\",\"event\":{{\"kind\":\"message\",\"text\":\"{}\"}}}}",
            "x".repeat(MAX_FRAME_BYTES)
        );
        assert_eq!(
            parse_frame(&huge),
            Err(FrameError::TooLong { bytes: huge.len() })
        );
    }

    #[test]
    fn a_frame_at_the_limit_is_accepted() {
        let prefix = "{\"type\":\"event\",\"event\":{\"kind\":\"message\",\"text\":\"";
        let suffix = "\"}}";
        let padding = MAX_FRAME_BYTES - prefix.len() - suffix.len();
        let exact = format!("{prefix}{}{suffix}", "x".repeat(padding));
        assert_eq!(exact.len(), MAX_FRAME_BYTES);
        assert!(parse_frame(&exact).is_ok());
    }

    #[test]
    fn a_summary_is_clipped_on_a_character_boundary() {
        let long = format!("{}é", "a".repeat(MAX_SUMMARY_BYTES - 1));
        let PluginOutcome::Completed { summary } =
            PluginOutcome::Completed { summary: long }.clipped()
        else {
            unreachable!("clipping keeps the variant");
        };
        assert!(summary.len() <= MAX_SUMMARY_BYTES);
        assert!(summary.is_char_boundary(summary.len()));
    }

    #[test]
    fn a_reason_is_clipped_on_a_character_boundary() {
        let long = format!("{}é", "a".repeat(MAX_REASON_BYTES - 1));
        let PluginOutcome::Failed { reason } = PluginOutcome::Failed { reason: long }.clipped()
        else {
            unreachable!("clipping keeps the variant");
        };
        assert!(reason.len() <= MAX_REASON_BYTES);
        assert!(reason.is_char_boundary(reason.len()));
    }

    #[test]
    fn clipping_leaves_a_short_outcome_alone() {
        let outcome = PluginOutcome::Completed {
            summary: "done".to_owned(),
        };
        assert_eq!(outcome.clipped(), outcome);
        assert_eq!(PluginOutcome::Cancelled.clipped(), PluginOutcome::Cancelled);
    }

    #[test]
    fn the_bounds_are_the_documented_sizes() {
        assert_eq!(MAX_FRAME_BYTES, 1_048_576);
        assert_eq!(MAX_SUMMARY_BYTES, 4_096);
        assert_eq!(MAX_REASON_BYTES, 4_096);
        const { assert!(MAX_FRAME_BYTES > MAX_SUMMARY_BYTES) };
        const { assert!(MAX_FRAME_BYTES > MAX_REASON_BYTES) };
    }

    #[test]
    fn every_frame_error_says_what_went_wrong() {
        let invalid = FrameError::Invalid("expected value".to_owned());
        let text = invalid.to_string();
        assert!(text.contains("not a host frame"), "{text}");
        assert!(text.contains("expected value"), "{text}");

        let too_long = FrameError::TooLong { bytes: 42 };
        let text = too_long.to_string();
        assert!(text.contains("42-byte"), "{text}");
        assert!(text.contains(&MAX_FRAME_BYTES.to_string()), "{text}");
    }
}

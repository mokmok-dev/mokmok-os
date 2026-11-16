//! The control-plane port over the daemon's state.
//!
//! [`mokmok_web`] renders the pages against this adapter, so the page code never sees
//! [`AppState`]. Every read runs through the same permits and blocking threads the JSON
//! API uses, so a page poll cannot exceed the daemon's read budget.

use std::sync::Arc;

use mokmok_control::{ControlError, Error as SystemError, NewTask, TaskId, TaskState};
use mokmok_journal::{Journal, JournalEntry};
use mokmok_web::port::{
    Agents, ControlPlane, Entry, NewSession, PortError, Read, Session, SessionState, Transcript,
};

use crate::{AppState, MAX_JOURNAL_TAIL, project_updates};

/// The pages' view of one daemon's state.
pub struct Plane {
    state: AppState,
}

impl Plane {
    /// Wraps the state the JSON API serves.
    #[must_use]
    pub const fn new(state: AppState) -> Self {
        Self { state }
    }
}

impl ControlPlane for Plane {
    fn sessions(&self) -> Read<'_, Vec<Session>> {
        Box::pin(async move {
            let system = self.state.system.clone();
            let journal = Arc::clone(&self.state.journal);
            // The counts run inside the blocking read, as `list_tasks` runs them, because
            // each one touches the journal's files.
            read_store(&self.state, move || {
                system.list().map(|tasks| {
                    tasks
                        .into_iter()
                        .map(|task| Session {
                            updates: journal_count(journal.as_ref(), task.id.as_str()),
                            id: task.id,
                            title: task.title,
                            instruction: task.instruction,
                            instruction_truncated: task.instruction_truncated,
                            status: task.status,
                            agent: task.agent,
                            workspace: task.workspace,
                            outcome: task.outcome,
                            submitted_at: task.submitted_at,
                        })
                        .collect()
                })
            })
            .await?
            .map_err(refusal)
        })
    }

    fn session_state(
        &self,
        id: TaskId,
    ) -> Read<'_, SessionState> {
        Box::pin(async move {
            let system = self.state.system.clone();
            let task = read_store(&self.state, move || system.state(&id))
                .await?
                .map_err(refusal)?;
            if task.spec.is_none() {
                return Err(PortError::NotFound);
            }
            Ok(SessionState {
                status: task.status,
                outcome: task.outcome,
            })
        })
    }

    fn transcript(
        &self,
        id: TaskId,
        limit: usize,
    ) -> Read<'_, Transcript> {
        Box::pin(async move {
            let store = Arc::clone(&self.state.store);
            let stream = id.clone();
            let events = read_store(&self.state, move || store.load(&stream))
                .await?
                .map_err(|_| PortError::Unavailable)?;
            let task =
                TaskState::replay(&events).map_err(|error| refusal(SystemError::Control(error)))?;
            if task.spec.is_none() {
                return Err(PortError::NotFound);
            }
            // A tail decodes every record it returns, so a page cannot ask for more than
            // the API route allows.
            let limit = limit.min(MAX_JOURNAL_TAIL);
            let journal = Arc::clone(&self.state.journal);
            let permit = Arc::clone(&self.state.journal_reads)
                .acquire_owned()
                .await
                .map_err(|_| PortError::Unavailable)?;
            let tail = tokio::task::spawn_blocking(move || {
                let _permit = permit;
                journal.tail(id.as_str(), limit)
            })
            .await
            .map_err(|error| {
                tracing::debug!(%error, "the journal read task failed");
                PortError::Unavailable
            })?
            .map_err(|_| PortError::Unavailable)?;
            // The journal wins once it has entries; a task logged before the split has
            // none, so its transcript is still in the domain log.
            if tail.entries.is_empty() {
                let (entries, total) = project_updates(&events, limit);
                return Ok(Transcript {
                    entries: entries.into_iter().map(entry).collect(),
                    total,
                });
            }
            Ok(Transcript {
                entries: tail.entries.into_iter().map(entry).collect(),
                total: tail.total,
            })
        })
    }

    fn agents(&self) -> Agents {
        Agents {
            names: self.state.agents.clone(),
            default: self.state.default_agent.clone(),
        }
    }

    fn submit(
        &self,
        spec: NewSession,
    ) -> Read<'_, TaskId> {
        Box::pin(async move {
            if !self.state.agents.iter().any(|agent| agent == &spec.agent) {
                return Err(PortError::Refused(format!(
                    "no agent is named {:?}",
                    spec.agent
                )));
            }
            self.state
                .supervisor
                .submit(NewTask {
                    title: spec.title,
                    instruction: spec.instruction,
                    agent: spec.agent,
                    host: None,
                    workspace: spec.workspace,
                    request_id: spec.request_id,
                })
                .map(|submitted| submitted.id)
                .map_err(refusal)
        })
    }

    fn cancel(
        &self,
        id: TaskId,
        reason: String,
    ) -> Read<'_, ()> {
        Box::pin(async move {
            // The read keeps a forged form from registering an actor for an id the store
            // never saw, which is why the JSON route reads before it cancels.
            let system = self.state.system.clone();
            let stream = id.clone();
            let task = read_store(&self.state, move || system.state(&stream))
                .await?
                .map_err(refusal)?;
            if task.spec.is_none() {
                return Err(PortError::NotFound);
            }
            self.state.supervisor.cancel(&id, &reason).map_err(refusal)
        })
    }
}

/// Runs one store read with a read permit on a blocking thread.
async fn read_store<T, F>(
    state: &AppState,
    read: F,
) -> Result<T, PortError>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    // A closed permit pool or a panicked read task leaves a page with nothing to render.
    state
        .store_read(read)
        .await
        .map_err(|_| PortError::Unavailable)
}

// The page only asks whether a transcript exists, so a zero count reads as none, and a
// failed count is retried on the next poll, which is why it logs at debug.
fn journal_count(
    journal: &dyn Journal,
    task: &str,
) -> Option<u64> {
    match journal.count(task) {
        Ok(count) => (count > 0).then_some(count),
        Err(error) => {
            tracing::debug!(task, %error, "cannot count the journal");
            None
        },
    }
}

/// Maps a system failure onto the reason a page shows.
fn refusal(error: SystemError) -> PortError {
    match error {
        SystemError::Control(ControlError::Terminal) => PortError::Finished,
        // A page never builds a bad uuid, so the closest answer for one that arrives is
        // that no such task exists.
        SystemError::Control(ControlError::NotSubmitted | ControlError::InvalidTaskId(_)) => {
            PortError::NotFound
        },
        SystemError::Control(ControlError::InvalidSpec { field, reason }) => {
            PortError::Refused(format!("{field} {reason}"))
        },
        SystemError::Control(ControlError::InvalidRequestId(_)) => {
            PortError::Refused("the form carried an invalid request id".to_owned())
        },
        // The composer mints one request id per render, so a differing spec reaches this
        // only from a stale form or a client that reused a value.
        SystemError::RequestConflict(id) => {
            tracing::debug!(
                task = id.as_str(),
                "a resubmitted form carried other content"
            );
            PortError::Refused(
                "this form was already sent with different content; send again to start a new session"
                    .to_owned(),
            )
        },
        // The rest name states a page command cannot reach, and a store, actor, or spawn
        // failure has no closer variant than unavailable.
        SystemError::Control(_)
        | SystemError::Store(_)
        | SystemError::ActorUnavailable
        | SystemError::Spawn(_) => PortError::Unavailable,
    }
}

fn entry(entry: JournalEntry) -> Entry {
    Entry {
        seq: entry.seq,
        event: entry.event,
    }
}

#[cfg(test)]
mod tests {
    use mokmok_control::{AgentUpdate, CloudEvent, ControlEvent, Version, journal_event};
    use mokmok_journal::JournalError;

    use super::*;

    #[test]
    fn a_finished_task_is_not_a_missing_one() {
        assert_eq!(
            refusal(SystemError::Control(ControlError::Terminal)),
            PortError::Finished
        );
        assert_eq!(
            refusal(SystemError::Control(ControlError::NotSubmitted)),
            PortError::NotFound
        );
    }

    #[test]
    fn an_invalid_spec_carries_the_field_and_reason() {
        assert_eq!(
            refusal(SystemError::Control(ControlError::InvalidSpec {
                field: "title",
                reason: "must not be empty".to_owned(),
            })),
            PortError::Refused("title must not be empty".to_owned())
        );
    }

    #[test]
    fn a_store_failure_is_unavailable() {
        assert_eq!(
            refusal(SystemError::ActorUnavailable),
            PortError::Unavailable
        );
    }

    #[test]
    fn a_counted_journal_marks_the_session_and_a_missing_one_does_not() {
        struct Counted(Result<u64, JournalError>);

        impl Journal for Counted {
            fn append(
                &self,
                _task: &str,
                _events: &[mokmok_journal::JournalEvent],
            ) -> Result<Vec<JournalEntry>, JournalError> {
                Ok(Vec::new())
            }

            fn tail(
                &self,
                _task: &str,
                _limit: usize,
            ) -> Result<mokmok_journal::JournalTail, JournalError> {
                Ok(mokmok_journal::JournalTail::default())
            }

            fn count(
                &self,
                _task: &str,
            ) -> Result<u64, JournalError> {
                self.0.clone()
            }
        }

        assert_eq!(journal_count(&Counted(Ok(3)), "task"), Some(3));
        assert_eq!(journal_count(&Counted(Ok(0)), "task"), None);
        assert_eq!(
            journal_count(
                &Counted(Err(JournalError::Io("no disk".to_owned()))),
                "task"
            ),
            None
        );
    }

    #[test]
    fn a_projected_update_becomes_a_transcript_entry() {
        let events = [CloudEvent::new(
            "task",
            Version::new(7),
            ControlEvent::UpdateRecorded {
                update: AgentUpdate::Message {
                    text: "from the log".to_owned(),
                },
            },
        )];
        let (entries, total) = project_updates(&events, 10);
        assert_eq!(total, 1);
        let entries: Vec<Entry> = entries.into_iter().map(entry).collect();
        assert_eq!(entries[0].seq, 7);
        assert_eq!(
            entries[0].event,
            journal_event(&AgentUpdate::Message {
                text: "from the log".to_owned(),
            })
        );
    }
}

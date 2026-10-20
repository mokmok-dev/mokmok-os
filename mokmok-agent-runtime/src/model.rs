//! The ports: everything the pure loop needs from the world, in the runtime's own types.

use mokmok_protocol::SessionID;
use mokmok_skill::SkillName;

use crate::context::Context;
use crate::dialogue::Transcript;
use crate::error::Error;
use crate::record::{ArtifactRef, Receipt, Step, StepId};

/// What the model answered one attempt with.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Action {
    /// The model is done and named no command.
    Final,
    Command {
        command: String,
    },
    Find {
        query: String,
    },
    /// The model sent one unified diff to rewrite one file.
    Edit {
        patch: String,
        expect: Option<String>,
    },
    UseSkill {
        name: SkillName,
    },
    /// The model asked to hand a goal to a child coding agent in its own durable session.
    Subagent {
        goal: String,
    },
    /// The model's tool arguments could not be decoded; it is recorded, never silently retried.
    Malformed {
        reason: String,
    },
}

/// What the model answered, in the runtime's canonical vocabulary.
///
/// The raw answer text is a reference; the vendor's JSON, headers, and ids were discarded at the
/// provider boundary, and `text` is an [`ArtifactRef`], never a wire type.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Answer {
    pub action: Action,
    /// What the model said, clipped to [`MAX_PROSE_BYTES`].
    ///
    /// The raw body is `text`; these are the words a human reads and the next prompt shows, so the
    /// loop's record of the answer carries them rather than a reference only a store could resolve.
    ///
    /// [`MAX_PROSE_BYTES`]: crate::record::MAX_PROSE_BYTES
    pub prose: String,
    pub text: ArtifactRef,
    pub model: String,
    pub usage: Usage,
}

/// What one model call cost.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
}

/// One model call, with the wire hidden behind it.
pub trait Provider: Sync {
    /// Encodes the context, calls the vendor, and parses back the answer text, the one action, and
    /// the token counts.
    ///
    /// Everything else is discarded here, so no HTTP, TLS, or vendor JSON type crosses the port.
    fn complete(
        &self,
        context: &Context,
    ) -> impl Future<Output = Result<Answer, Error>> + Send;

    /// The same call, with a callback for each prose fragment the provider emits while streaming.
    ///
    /// The default implementation delegates to [`complete`] and forwards the whole prose once. The
    /// observer is not `Send` because a terminal-bound watcher cannot cross await points safely,
    /// and the future is not `Send` for the same reason; callers that need `Send` use `complete`.
    fn complete_watched<'a>(
        &'a self,
        context: &'a Context,
        watch: &'a mut dyn FnMut(&str),
    ) -> impl Future<Output = Result<Answer, Error>> + 'a {
        async move {
            let answer = self.complete(context).await?;
            watch(&answer.prose);
            Ok(answer)
        }
    }
}

/// The session's durable past and the one way to extend it.
///
/// Every type on this port is the runtime's own: no entry, kind, payload, version, command id,
/// plan, request, exposure, or HTTP type crosses this line.
pub trait Journal {
    /// The fold of everything the session has committed.
    ///
    /// The fold carries the session's task even before its record commits, so the first decision a
    /// session makes is `Begin`; a caller that cannot name the task cannot drive the loop.
    ///
    /// # Errors
    ///
    /// Returns a journal failure when the history cannot be read or does not fold.
    fn transcript(
        &self,
        session: SessionID,
    ) -> Result<Transcript, Error>;

    /// Stores bytes and returns the reference that names them, always before the record that
    /// carries the reference.
    ///
    /// A crash therefore leaves an unreferenced blob the collector takes, never a reference to
    /// bytes that were never written.
    ///
    /// # Errors
    ///
    /// Returns a journal failure when the bytes cannot be stored.
    fn put(
        &mut self,
        bytes: &[u8],
        truncated: bool,
    ) -> Result<ArtifactRef, Error>;

    /// Whether the journal already holds the step with `id`.
    ///
    /// A command stream delivers at least once. A line that arrives twice asks this before it
    /// derives a new step, so a redelivery replays its own write instead of opening the next turn.
    /// A journal with no dedup answers false, which is the shape a test journal starts from.
    ///
    /// # Errors
    ///
    /// Returns a journal failure when the dedup row cannot be read.
    fn committed(
        &self,
        _id: StepId,
    ) -> Result<bool, Error> {
        Ok(false)
    }

    /// Commits every record of one step in order, or none of them.
    ///
    /// The identity the step was derived under is what makes a repeat a [`Receipt::Replayed`]
    /// rather than a second write.
    ///
    /// # Errors
    ///
    /// Returns a journal failure when the transaction cannot be committed or the fold refuses a
    /// record.
    fn append(
        &mut self,
        step: Step,
    ) -> Result<Receipt, Error>;
}

#[cfg(test)]
mod tests {
    use mokmok_protocol::SessionID;

    use crate::dialogue::Transcript;
    use crate::error::Error;
    use crate::record::{ArtifactRef, Receipt, Step, StepId, StepKey};
    use crate::test_support::{TestJournal, session, task};

    use super::Journal;

    /// A journal with no dedup, so its `committed` is the port's default.
    struct NoDedup(TestJournal);

    impl Journal for NoDedup {
        fn transcript(
            &self,
            session: SessionID,
        ) -> Result<Transcript, Error> {
            self.0.transcript(session)
        }

        fn put(
            &mut self,
            bytes: &[u8],
            truncated: bool,
        ) -> Result<ArtifactRef, Error> {
            self.0.put(bytes, truncated)
        }

        fn append(
            &mut self,
            step: Step,
        ) -> Result<Receipt, Error> {
            self.0.append(step)
        }
    }

    #[test]
    fn committed_answers_whether_the_journal_holds_the_step() {
        let session = session();
        let mut journal = TestJournal::new(session, task(1));
        let written = StepId::derive(session, StepKey::Task);
        let absent = StepId::derive(session, StepKey::Speech { turn: 1 });
        journal.mark(written);

        assert!(journal.committed(written).unwrap());
        assert!(!journal.committed(absent).unwrap());
    }

    #[test]
    fn a_journal_without_dedup_answers_false_for_every_step() {
        let session = session();
        let mut inner = TestJournal::new(session, task(1));
        let written = StepId::derive(session, StepKey::Task);
        let absent = StepId::derive(session, StepKey::Speech { turn: 1 });
        inner.mark(written);
        let journal = NoDedup(inner);

        assert!(!journal.committed(written).unwrap());
        assert!(!journal.committed(absent).unwrap());
    }
}

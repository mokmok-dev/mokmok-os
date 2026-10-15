//! The ports: everything the pure loop needs from the world, in the runtime's own types.

use mokmok_protocol::SessionID;

use crate::context::Context;
use crate::dialogue::Transcript;
use crate::error::Error;
use crate::record::{ArtifactRef, Receipt, Step};

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

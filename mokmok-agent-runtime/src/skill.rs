//! The sixth port: reads one advertised skill's body, and changes nothing.
//!
//! A read is read-only in the same way a find is: it writes no intent and no resolution, because
//! running it again cannot change the world. Progressive disclosure reads a skill only when the
//! model asks for it by name, so the advert the task froze names the file and binds it to the bytes
//! discovery measured; the port answers the body or the reason it could not.

use crate::error::Error;
use crate::task::Advert;

/// The refusal a load reads as when the session's task advertised no skill at all.
///
/// A name can never resolve to an advert, so this sentence is the whole answer; the port is never
/// called.
pub const NO_SKILLS: &str = "no skill was advertised, so none can be loaded";

/// What reading one advertised skill answered.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Read {
    /// Exactly the Markdown after the frontmatter.
    Body(String),
    /// Gone, changed, or no longer valid. Carries the reason the model reads.
    Refused(String),
}

/// The port activation drives to turn an advert into the instructions the model loads.
pub trait Skills: Sync {
    /// Reads the body of one advertised skill.
    ///
    /// # Errors
    ///
    /// Returns the adapter's own failure when no answer can be produced. A skill that is gone,
    /// changed, or no longer valid is [`Read::Refused`], not an error: it is an answer the model
    /// reads, not a failure of the port.
    fn read(
        &self,
        advert: &Advert,
    ) -> Result<Read, Error>;
}

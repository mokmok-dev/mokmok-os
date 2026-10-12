use crate::id::private::Sealed;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Agent {}
impl Sealed for Agent {}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Session {}
impl Sealed for Session {}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Execution {}
impl Sealed for Execution {}

pub type AgentID = crate::id::ID<Agent>;
pub type SessionID = crate::id::ID<Session>;
pub type ExecutionID = crate::id::ID<Execution>;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Turn {}
impl Sealed for Turn {}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Command {}
impl Sealed for Command {}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Event {}
impl Sealed for Event {}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Artifact {}
impl Sealed for Artifact {}

pub type TurnID = crate::id::ID<Turn>;
pub type CommandID = crate::id::ID<Command>;
pub type EventID = crate::id::ID<Event>;
pub type ArtifactID = crate::id::ID<Artifact>;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Message {}
impl Sealed for Message {}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Correlation {}
impl Sealed for Correlation {}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Principal {}
impl Sealed for Principal {}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Worker {}
impl Sealed for Worker {}

/// Names one produced message. Distinct from [`EventID`], which the store assigns at append: a
/// command's identity survives the log entry it caused.
pub type MessageID = crate::id::ID<Message>;
/// Ties messages that belong to one logical request without claiming causation.
pub type CorrelationID = crate::id::ID<Correlation>;
pub type PrincipalID = crate::id::ID<Principal>;
pub type WorkerID = crate::id::ID<Worker>;

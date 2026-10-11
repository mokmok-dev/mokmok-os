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

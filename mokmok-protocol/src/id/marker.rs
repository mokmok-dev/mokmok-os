use crate::id::private::Sealed;

pub enum Agent {}
impl Sealed for Agent {}

pub enum Session {}
impl Sealed for Session {}

pub enum Execution {}
impl Sealed for Execution {}

pub type AgentID = crate::id::ID<Agent>;
pub type SessionID = crate::id::ID<Session>;
pub type ExecutionID = crate::id::ID<Execution>;

pub enum Turn {}
impl Sealed for Turn {}

pub enum Command {}
impl Sealed for Command {}

pub enum Event {}
impl Sealed for Event {}

pub enum Artifact {}
impl Sealed for Artifact {}

pub type TurnID = crate::id::ID<Turn>;
pub type CommandID = crate::id::ID<Command>;
pub type EventID = crate::id::ID<Event>;
pub type ArtifactID = crate::id::ID<Artifact>;

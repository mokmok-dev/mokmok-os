//! An Agent Client Protocol client with a stdio transport and a WebSocket transport.
//!
//! ACP is JSON-RPC 2.0. Over stdio each message is one newline-delimited JSON object, as the
//! protocol's stdio transport specifies. Over WebSocket each message is one text frame, as the
//! transport RFD specifies and as Grok Build's reference server implements it.
//!
//! One task runs through [`AcpClient::delegate`]: open a connection, create a session, send a
//! prompt, stream the agent's updates to a [`Handler`], and read the stop reason. [`delegate_task`]
//! does the whole sequence for a single [`Task`].
//!
//! # Example
//!
//! ```no_run
//! use mokmok_acp::{AcpClient, ConnectionConfig, NoopHandler, StdioConfig};
//!
//! # fn main() -> Result<(), mokmok_acp::AcpError> {
//! let config = ConnectionConfig::Stdio(StdioConfig::grok());
//! let mut client = AcpClient::open(config)?;
//! let session = client.new_session("/tmp")?;
//! let mut handler = NoopHandler;
//! let turn = client.delegate(&session, "list the files here", &mut handler)?;
//! println!("{:?}", turn.stop_reason);
//! # Ok(())
//! # }
//! ```

mod client;
mod jsonrpc;
mod transport;

pub use agent_client_protocol_schema::{
    ContentBlock, InitializeResponse, PromptResponse, ProtocolVersion, RequestPermissionOutcome,
    RequestPermissionRequest, RequestPermissionResponse, SessionId, SessionNotification,
    SessionUpdate, StopReason, TextContent,
};
pub use client::{
    AcpClient, AcpError, ConnectionConfig, Handler, NoopHandler, Session, Task, Turn, delegate_task,
};
pub use transport::{StdioConfig, Transport, TransportError, WebSocketConfig};

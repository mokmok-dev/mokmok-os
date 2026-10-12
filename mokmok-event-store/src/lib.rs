//! A durable commit log for mokmok: one global ordered log, a snapshot per stream, and a cursor
//! per consumer, all in one SQLite file.
//!
//! An append is one transaction. It checks the command id first, so a retry replays the commit it
//! already made instead of conflicting with it, then checks the caller's expected stream version,
//! writes the events, the stream's snapshot, and the outbox rows together, and only then commits.
//! A consume is one transaction too: it writes the caller's state, the inbox row that suppresses a
//! redelivery, and the checkpoint together, so no checkpoint can name processing that did not
//! commit.
//!
//! ```no_run
//! use std::num::NonZeroUsize;
//! use std::path::Path;
//!
//! use mokmok_event_store::{
//!     Append, CommandId, ConsumerId, Expected, NewEvent, Outcome, OutboxMessage, Payload,
//!     Processed, Store, StreamId, Version,
//! };
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let mut store = Store::open(Path::new(":memory:"))?;
//! let stream = StreamId::new();
//! let consumer = ConsumerId::new();
//!
//! let loaded = store.history(stream, Version::INITIAL, NonZeroUsize::MIN)?;
//! assert!(loaded.commits().is_empty());
//!
//! let append = Append::new(
//!     CommandId::new(),
//!     stream,
//!     Expected::New,
//!     NewEvent::new(
//!         "turn.finished".parse()?,
//!         Payload::json(&serde_json::json!({"turn": 1}))?,
//!     ),
//! )
//! .event(NewEvent::new("turn.recorded".parse()?, Payload::json(&7)?))
//! .state(Payload::json(&serde_json::json!({"open": true}))?)
//! .outbox(OutboxMessage::new("turn.done", Payload::json(&1)?));
//!
//! if let Outcome::Committed(committed) = store.append(&append)? {
//!     let event = &committed.events()[0];
//!     let processed = store.consume(consumer, &committed, |tx| {
//!         tx.put_projection(consumer, "turn", event.payload(), event.position())
//!     })?;
//!     assert!(matches!(processed, Processed::Applied(())));
//! }
//! # Ok(())
//! # }
//! ```

mod append;
mod error;
mod event;
mod model;
mod payload;
mod store;
mod write;

pub use append::{Append, Committed, OutboxEntry, OutboxId, OutboxMessage, Outcome};
pub use error::Error;
pub use event::{Event, InvalidKind, Kind, NewEvent, Slice};
pub use model::{CommandId, CommitPosition, ConsumerId, Expected, InvalidId, StreamId, Version};
pub use payload::{Payload, PayloadError};
pub use store::{Processed, Store};
pub use write::WriteTx;

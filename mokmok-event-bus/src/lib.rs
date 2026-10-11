//! A durable, bounded append-only log per topic, served over WebSocket on a Unix socket.
//!
//! One process is the bus; publishers and subscribers are WebSocket clients of it. A connection
//! names one topic in its first frame and then publishes or pulls from that topic. Every event is
//! committed to SQLite before any subscriber can receive it, and each subscriber resumes from a
//! checkpoint the bus keeps for it, so a crash between commit and delivery loses nothing.
//!
//! ```no_run
//! use mokmok_event_bus::{
//!     Config, Payload, Publisher, Subscriber, SubscriberId, Topic, serve, signals,
//! };
//!
//! # async fn handle(event: &mokmok_event_bus::Event) -> Result<(), Box<dyn std::error::Error>> {
//! #     let _ = event;
//! #     Ok(())
//! # }
//! # fn load_id() -> SubscriberId {
//! #     SubscriberId::new()
//! # }
//! # async fn run() -> Result<(), Box<dyn std::error::Error>> {
//! let config = Config::default();
//! let socket = config.path.clone();
//! tokio::spawn(serve(config, signals()));
//!
//! let topic = Topic::Global;
//! let mut publisher = Publisher::open(&socket, topic).await?;
//! publisher.publish(Payload::json(&"turn finished")?).await?;
//!
//! let mut subscriber = Subscriber::open(&socket, topic, load_id()).await?;
//! loop {
//!     let batch = subscriber.next().await?;   // parks until a commit lands
//!     for event in batch.events() {
//!         handle(event).await?;
//!     }
//!     subscriber.ack(batch.after()).await?;   // only after the effect is durable
//! }
//! # }
//! ```
//!
//! [`Subscriber::open`] takes the [`SubscriberId`] whose checkpoint resumes the read, so a cold
//! start and a restart differ only in whether that id has been seen before. Delivery is
//! at-least-once: a batch not yet acked is replayed, so a consumer must tolerate duplicates.

// Internal types are `pub`, not `pub(crate)`: the workspace denies
// `clippy::redundant_pub_crate`, and a private module already keeps them crate-local.
mod bus;
mod client;
mod config;
mod event;
mod payload;
mod server;
mod store;
mod subscriber;
mod topic;
mod wire;

pub use client::{Error, Publisher, Subscriber};
pub use config::{Config, Limits, Retention, Timeouts};
pub use event::{Batch, Cursor, Event, Lapsed, LogEpoch, Published, Refusal, Seq, Window};
pub use payload::{Payload, PayloadError};
pub use server::{ServeError, serve, signals};
pub use subscriber::{InvalidSubscriberId, SubscriberId};
pub use topic::{InvalidTopic, Topic};

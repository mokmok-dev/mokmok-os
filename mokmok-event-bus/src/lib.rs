//! A bounded append-only log per topic, served over WebSocket on a Unix socket.
//!
//! One process is the bus; publishers and subscribers are WebSocket clients of it. A connection
//! names one topic in its first frame and then publishes or pulls from that topic. Each topic
//! keeps a bounded log, so a subscriber that was absent, slow, or not yet started can catch up
//! from a position it saved, and one that fell further behind than the log reaches is told what
//! it missed instead of silently losing it.
//!
//! ```
//! use std::path::PathBuf;
//!
//! use mokmok_event_bus::{
//!     Config, Payload, Position, Publisher, Subscriber, Topic, serve, signals,
//! };
//!
//! # async fn run() -> Result<(), Box<dyn std::error::Error>> {
//! let config = Config::default();
//! let socket = config.path.clone();
//! tokio::spawn(serve(config, signals()));
//!
//! let topic = Topic::Global;
//! let mut publisher = Publisher::open(&socket, topic).await?;
//! publisher.publish(Payload::json(&"turn finished")?).await?;
//!
//! let mut subscriber = Subscriber::open(&socket, topic, load_position()).await?;
//! let batch = subscriber.next().await?;
//! for event in batch.events() {
//!     println!("{}", event.payload().as_str());
//! }
//! save_position(subscriber.position());
//! # Ok(())
//! # }
//! # fn load_position() -> Position {
//! #     Position::Beginning
//! # }
//! # fn save_position(position: Position) {
//! #     let _ = position;
//! # }
//! # fn socket_path() -> PathBuf {
//! #     Config::default().path
//! # }
//! ```
//!
//! [`Subscriber::open`] takes the position to read from, so a cold start and a restart differ
//! only in the value the caller saved.

// Internal types are `pub`, not `pub(crate)`: the workspace denies
// `clippy::redundant_pub_crate`, and a private module already keeps them crate-local.
mod bus;
mod client;
mod config;
mod event;
mod log;
mod payload;
mod server;
mod topic;
mod wire;

pub use client::{Error, Publisher, Subscriber};
pub use config::{Config, Limits, Retention, Timeouts};
pub use event::{
    Batch, Cursor, Event, Lapsed, LogEpoch, Position, Published, Refusal, Seq, Window,
};
pub use payload::{Payload, PayloadError};
pub use server::{ServeError, serve, signals};
pub use topic::{InvalidTopic, Topic};

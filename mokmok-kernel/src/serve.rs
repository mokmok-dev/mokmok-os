//! Serves the event bus and relays the log's committed notices to it.
//!
//! A notice is written inside the transaction that records the change it names, so it exists only
//! after that history committed. The relay runs afterwards, one publisher per topic, and settles
//! the rows the bus acknowledged. A publish that fails leaves its row and every later row pending,
//! so the next pass tries them again: delivery here is at-least-once, which is what a durable outbox
//! buys and what makes a crash between the publish and the settle cost a redelivery rather than a
//! lost event.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use mokmok_event_bus::{
    Config as BusConfig, Payload, Publisher, Topic, serve as serve_bus, signals,
};

use crate::config::Config;
use crate::db::Db;
use crate::error::Error;

/// How long a pass waits for the bus to bind its socket before giving up.
const BIND: Duration = Duration::from_secs(5);

/// A pass interval when the relay serves rather than draining once.
const PASS: Duration = Duration::from_millis(200);

/// The outbox relay: one open publisher per topic it has published to this run.
pub struct Relay {
    socket: PathBuf,
    publishers: BTreeMap<String, Publisher>,
}

impl Relay {
    #[must_use]
    pub fn new(socket: impl Into<PathBuf>) -> Self {
        Self {
            socket: socket.into(),
            publishers: BTreeMap::new(),
        }
    }

    /// Publishes every pending notice in the order it was written, then settles what was
    /// acknowledged.
    ///
    /// The order matters: the store deletes every row at or below the id it is given, so stopping
    /// at the first publish that fails is what keeps an unpublished notice from being settled by a
    /// later one's success.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Topic`] when a notice names a topic the bus does not have,
    /// [`Error::NotJson`] when a stored payload is not JSON, [`Error::BusPayload`] when a published
    /// value cannot be encoded, [`Error::Bus`] when the bus refuses a connection or an append, and
    /// [`Error::Store`] when the log cannot be read or the settled rows cannot be deleted.
    pub async fn once(
        &mut self,
        db: &mut Db,
    ) -> Result<usize, Error> {
        let pending = db.pending()?;
        let mut through = None;
        let mut published = 0;

        for entry in &pending {
            let topic = entry.topic().parse::<Topic>()?;
            let value = serde_json::from_str::<serde_json::Value>(entry.payload().as_str())?;
            let payload = Payload::json(&value)?;
            self.publish(topic, payload).await?;

            through = Some(entry.id());
            published += 1;
        }

        if let Some(id) = through {
            db.settle_notices(id)?;
        }

        Ok(published)
    }

    async fn publish(
        &mut self,
        topic: Topic,
        payload: Payload,
    ) -> Result<(), Error> {
        let key = topic.to_string();

        if let Some(publisher) = self.publishers.get_mut(&key) {
            publisher.publish(payload).await?;

            return Ok(());
        }

        let mut publisher = Publisher::open(&self.socket, topic).await?;
        publisher.publish(payload).await?;
        self.publishers.insert(key, publisher);

        Ok(())
    }
}

/// Serves the bus on `socket` and relays what the log has committed.
///
/// With `once`, it drains the outbox as it stands and returns the number of notices the bus
/// acknowledged. Otherwise it keeps relaying until the process is stopped.
///
/// # Errors
///
/// As [`Relay::once`], plus [`Error::Io`] when the socket never appears.
pub async fn serve(
    config: &Config,
    socket: &Path,
    once: bool,
) -> Result<usize, Error> {
    let bus = BusConfig {
        path: socket.to_path_buf(),
        store: config.data_dir.join("bus.sqlite"),
        ..BusConfig::default()
    };
    let _serving = tokio::spawn(serve_bus(bus, signals()));
    let mut relay = Relay::new(socket);
    let mut db = Db::open(&config.events)?;

    wait_for_socket(socket).await?;
    let published = relay.once(&mut db).await?;
    if once {
        return Ok(published);
    }

    loop {
        tokio::time::sleep(PASS).await;
        if let Err(error) = relay.once(&mut db).await {
            eprintln!("mokmokd: relay pass failed: {error}");
        }
    }
}

async fn wait_for_socket(socket: &Path) -> Result<(), Error> {
    let deadline = tokio::time::Instant::now() + BIND;

    while tokio::time::Instant::now() < deadline {
        if socket.exists() {
            return Ok(());
        }

        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    Err(Error::Io(std::io::Error::new(
        std::io::ErrorKind::TimedOut,
        format!("the bus never bound {}", socket.display()),
    )))
}

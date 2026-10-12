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
    Config as BusConfig, Payload, Publisher, ServeError, Topic, serve as serve_bus, signals,
};
use tokio::sync::oneshot;
use tokio::time::Instant;

use crate::config::Config;
use crate::db::Db;
use crate::error::Error;
use crate::notify::{Cadence, Message, Notifier};

/// How long the loop waits for the bus to answer a session before giving up.
const BIND: Duration = Duration::from_secs(5);

/// How long one readiness probe may take before it counts as no answer.
///
/// A peer that accepts a connection without answering the handshake is not a bus, and this wait
/// exists to bound what that costs, so the probe itself needs a bound for `BIND` to be reached.
const PROBE: Duration = Duration::from_secs(1);

/// How long the loop waits between readiness probes.
const RETRY: Duration = Duration::from_millis(10);

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
/// acknowledged. Otherwise it serves until the process is told to stop, reporting readiness once
/// the bus answers and the outbox has drained, and returning the notices acknowledged over the run.
///
/// # Errors
///
/// As [`Relay::once`], plus [`Error::BusUnanswered`] when no bus answers within the bind deadline,
/// [`Error::BusStopped`] when the bus task ends, and [`Error::BusServe`] with the refusal that ended
/// it.
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

    if once {
        // A batch invocation that exits immediately has no readiness to announce, so `--once`
        // reports nothing to a supervisor.
        let _serving = tokio::spawn(serve_bus(bus, signals()));
        let mut relay = Relay::new(socket);
        let mut db = Db::open(&config.events)?;

        wait_for_bus(socket).await?;

        return relay.once(&mut db).await;
    }

    serve_until_stopped(config, bus).await
}

async fn serve_until_stopped(
    config: &Config,
    bus: BusConfig,
) -> Result<usize, Error> {
    let socket = bus.path.clone();

    // This loop owns the stop. A bus that listened for the signal itself would stop serving while
    // this loop went on relaying, which is the process that stays up with nothing behind it.
    let (stop, stopped) = oneshot::channel::<()>();
    let mut bus = tokio::spawn(serve_bus(bus, async move {
        let _ = stopped.await;
    }));

    let mut relay = Relay::new(&socket);
    let mut db = Db::open(&config.events)?;
    let notifier = Notifier::from_env()?;
    let mut shutdown = std::pin::pin!(signals());
    // The first tick waits a whole interval, so the bus has bound before the first pass rather
    // than failing on the rows a previous process left pending.
    let mut tick = tokio::time::interval_at(Instant::now() + PASS, PASS);
    // The keep-alive schedule, absent until readiness: a report for a state this process has not
    // reached is unrepresentable.
    let mut ready: Option<Cadence> = None;
    let mut published = 0;

    loop {
        tokio::select! {
            // A bus that ended is answered before anything else, so a process never stays up, or
            // stays ready, with no bus behind it. `biased` makes the order a rule and not a coin
            // toss.
            biased;
            result = &mut bus => return Err(bus_ended(result)),
            () = &mut shutdown => break,
            // A stop that arrives while a pass runs is honoured after it. That is the drain: the
            // pass is the unit of durable progress, and a cancelled one would only cost a
            // redelivery.
            _ = tick.tick() => (),
        }

        let drained = match relay.once(&mut db).await {
            Ok(count) => {
                published += count;
                true
            },
            // The rows stay pending and the next pass tries them again, which is the at-least-once
            // delivery the outbox already buys; a failed pass is not a reason to end a daemon.
            Err(error) => {
                eprintln!("mokmokd: relay pass failed: {error}");
                false
            },
        };

        if ready.is_none() && drained {
            // Readiness is a bus that answers a session and an outbox that drained, because that is
            // what a dependent connecting now finds. A socket file is neither of those things: a
            // process killed with SIGKILL leaves one behind.
            wait_for_bus(&socket).await?;
            report(&notifier, Message::Ready);
            ready = Some(Cadence::new(notifier.ping(), Instant::now()));
        } else if let Some(cadence) = &mut ready {
            // The keep-alive is sent from the pass rather than from a timer of its own, so a relay
            // wedged in `once` stops reporting and the watchdog is what ends this process. A timer
            // that only slept would keep pinging a process that had stopped relaying.
            if cadence.due(Instant::now()) {
                report(&notifier, Message::Alive);
            }
        }
    }

    // The bus unlinks its socket on the way out, so it is awaited before the process ends: a
    // restart must not meet a socket this instance left behind.
    report(&notifier, Message::Stopping);
    let _ = stop.send(());
    if let Ok(ended) = bus.await {
        ended?;
    }

    Ok(published)
}

async fn wait_for_bus(socket: &Path) -> Result<(), Error> {
    let deadline = Instant::now() + BIND;

    loop {
        if bus_answers(socket).await {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(Error::BusUnanswered(socket.to_path_buf()));
        }

        tokio::time::sleep(RETRY).await;
    }
}

/// A socket file is not a bus: a process killed with SIGKILL leaves one behind, and the bus that
/// replaces it unlinks and rebinds, so the file says nothing until a peer answers. Opening a
/// session moves no checkpoint and appends no event; the only durable effect is the topic row the
/// bus creates for a topic it has not seen.
async fn bus_answers(socket: &Path) -> bool {
    // A dropped probe closes the connection it opened, so a peer that never answers costs one
    // `PROBE` and leaves nothing behind.
    tokio::time::timeout(PROBE, Publisher::open(socket, Topic::Global))
        .await
        .is_ok_and(|opened| opened.is_ok())
}

fn bus_ended(result: Result<Result<(), ServeError>, tokio::task::JoinError>) -> Error {
    match result {
        Ok(Ok(())) | Err(_) => Error::BusStopped,
        Ok(Err(error)) => Error::BusServe(error),
    }
}

/// Reports one message, keeping a failure from ending the daemon.
fn report(
    notifier: &Notifier,
    message: Message,
) {
    if let Err(error) = notifier.send(message) {
        eprintln!("mokmokd: the supervisor was not told {message:?}: {error}");
    }
}

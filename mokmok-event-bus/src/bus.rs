use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, PoisonError, RwLock};
use std::time::{Instant, SystemTime};

use tokio::sync::watch;

use crate::config::Config;
use crate::event::{Batch, Position, Published, Refusal, Window};
use crate::log::Log;
use crate::payload::Payload;
use crate::topic::Topic;

pub struct Bus {
    inner: Arc<BusInner>,
}

struct BusInner {
    config: Config,
    topics: RwLock<HashMap<Topic, Arc<TopicEntry>>>,
    connections: AtomicUsize,
    stop: watch::Sender<bool>,
}

pub struct TopicEntry {
    log: RwLock<Log>,
    appended: watch::Sender<Window>,
}

pub struct ConnectionPermit {
    inner: Arc<BusInner>,
}

impl Bus {
    pub fn new(config: Config) -> Arc<Self> {
        let (stop, _) = watch::channel(false);

        Arc::new(Self {
            inner: Arc::new(BusInner {
                config,
                topics: RwLock::new(HashMap::new()),
                connections: AtomicUsize::new(0),
                stop,
            }),
        })
    }

    pub fn stop_handle(&self) -> watch::Sender<bool> {
        self.inner.stop.clone()
    }

    pub fn stop_receiver(&self) -> watch::Receiver<bool> {
        self.inner.stop.subscribe()
    }

    pub fn open(
        &self,
        topic: Topic,
    ) -> Result<(Arc<TopicEntry>, watch::Receiver<Window>), Refusal> {
        let mut topics = self
            .inner
            .topics
            .write()
            .unwrap_or_else(PoisonError::into_inner);

        if let Some(entry) = topics.get(&topic) {
            return Ok((Arc::clone(entry), entry.appended.subscribe()));
        }

        if topics.len() >= self.inner.config.limits.topics.get() {
            let now = Instant::now();
            let idle_after = self.inner.config.retention.idle;

            topics.retain(|name, entry| {
                if entry.appended.receiver_count() != 0 {
                    return true;
                }

                let (idle, empty) = {
                    let log = entry.log.read().unwrap_or_else(PoisonError::into_inner);
                    (log.is_idle(now, idle_after), log.is_empty())
                };

                if idle {
                    tracing::info!(topic = %name, empty, "reclaimed an unwatched topic");
                }

                !idle
            });
        }

        if topics.len() >= self.inner.config.limits.topics.get() {
            return Err(Refusal::TopicLimit);
        }

        let entry = Arc::new(TopicEntry::new(Log::new(self.inner.config.retention)));
        let ticks = entry.appended.subscribe();
        topics.insert(topic, Arc::clone(&entry));
        drop(topics);

        Ok((entry, ticks))
    }

    /// A pull that finds nothing parks the session and re-attempts here, and nothing awaits while
    /// the log lock is held.
    pub fn batch(
        &self,
        entry: &TopicEntry,
        at: Position,
        max: NonZeroUsize,
    ) -> Result<Batch, Refusal> {
        let max = max.min(self.inner.config.limits.batch);

        let log = entry.log.read().unwrap_or_else(PoisonError::into_inner);
        log.take(at, max)
    }

    pub fn try_connect(self: &Arc<Self>) -> Option<ConnectionPermit> {
        let ceiling = self.inner.config.limits.connections.get();
        let admitted = self
            .inner
            .connections
            .try_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                (current < ceiling).then_some(current + 1)
            })
            .is_ok();

        admitted.then(|| ConnectionPermit {
            inner: Arc::clone(&self.inner),
        })
    }

    pub fn connections(&self) -> usize {
        self.inner.connections.load(Ordering::Acquire)
    }
}

impl TopicEntry {
    fn new(log: Log) -> Self {
        let (appended, _) = watch::channel(log.window());

        Self {
            log: RwLock::new(log),
            appended,
        }
    }

    /// Appends to this entry's own log. The registry lock is not touched: the session already
    /// holds the entry from the `Open` frame, so two publishers on different topics never
    /// contend.
    pub fn append(
        &self,
        topic: Topic,
        payload: Payload,
    ) -> Published {
        let at = SystemTime::now();

        let (id, seq, window) = {
            let mut log = self.log.write().unwrap_or_else(PoisonError::into_inner);
            let (id, seq) = log.append(topic, payload, at);
            (id, seq, log.window())
        };

        self.appended.send_replace(window);

        Published::new(id, seq, at)
    }
}

impl Drop for ConnectionPermit {
    fn drop(&mut self) {
        self.inner.connections.fetch_sub(1, Ordering::AcqRel);
    }
}

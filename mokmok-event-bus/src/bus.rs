use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, PoisonError, RwLock};
use std::time::{Instant, SystemTime};

use tokio::sync::watch;
use uuid::Uuid;

use crate::config::Config;
use crate::event::{Batch, LogEpoch, Position, Published, Refusal, Window};
use crate::log::Log;
use crate::payload::Payload;
use crate::topic::Topic;

pub struct Bus {
    inner: Arc<BusInner>,
}

struct BusInner {
    config: Config,
    epoch: LogEpoch,
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
                epoch: LogEpoch::new(Uuid::new_v4()),
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

        let entry = Arc::new(TopicEntry::new(Log::new(
            self.inner.epoch,
            self.inner.config.retention,
        )));
        let ticks = entry.appended.subscribe();
        topics.insert(topic, Arc::clone(&entry));
        drop(topics);

        Ok((entry, ticks))
    }

    pub fn append(
        &self,
        topic: Topic,
        payload: Payload,
    ) -> Result<Published, Refusal> {
        let (entry, _ticks) = self.open(topic)?;
        let at = SystemTime::now();

        let (id, seq, window) = {
            let mut log = entry.log.write().unwrap_or_else(PoisonError::into_inner);
            let (id, seq) = log.append(topic, payload, at);
            (id, seq, log.window())
        };

        // Publishing the window after the log's write guard is released is what lets a reader
        // woken by the change observe the append; the other order would lose that guarantee.
        entry.appended.send_replace(window);

        Ok(Published::new(id, seq, at))
    }

    pub async fn pull(
        &self,
        entry: &TopicEntry,
        ticks: &mut watch::Receiver<Window>,
        at: Position,
        max: NonZeroUsize,
        stop: &mut watch::Receiver<bool>,
    ) -> Result<Batch, Refusal> {
        let max = max.min(self.inner.config.limits.batch);

        loop {
            ticks.borrow_and_update();

            let batch = {
                let log = entry.log.read().unwrap_or_else(PoisonError::into_inner);
                log.take(at, max)?
            };

            if !batch.is_empty() || batch.lapsed().is_some() {
                return Ok(batch);
            }

            tokio::select! {
                result = ticks.changed() => {
                    if result.is_err() {
                        return Err(Refusal::ShuttingDown);
                    }
                }
                _ = stop.changed() => return Err(Refusal::ShuttingDown),
            }
        }
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
}

impl Drop for ConnectionPermit {
    fn drop(&mut self) {
        self.inner.connections.fetch_sub(1, Ordering::AcqRel);
    }
}

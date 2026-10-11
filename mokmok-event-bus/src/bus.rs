use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, PoisonError, RwLock};
use std::time::{Duration, SystemTime};

use tokio::sync::watch;

use crate::config::Config;
use crate::event::{Published, Refusal, Window};
use crate::payload::Payload;
use crate::server::ServeError;
use crate::store::{AppendClock, Store, StoreError, TopicState};
use crate::topic::Topic;

pub struct Bus {
    inner: Arc<BusInner>,
}

struct BusInner {
    config: Config,
    topics: RwLock<HashMap<Topic, Arc<TopicEntry>>>,
    connections: AtomicUsize,
    stop: watch::Sender<bool>,
    store: Store,
}

pub struct TopicEntry {
    store: Store,
    appended: watch::Sender<Window>,
    last_append: AppendClock,
}

/// Why a session could not open a topic. A store failure is kept apart from a refusal so the
/// session can close the connection rather than answer a typed refusal that is not true.
pub enum OpenError {
    Refused(Refusal),
    Store(StoreError),
}

pub struct ConnectionPermit {
    inner: Arc<BusInner>,
}

impl Bus {
    /// Opens the store and starts serving topics from it.
    ///
    /// # Errors
    ///
    /// Returns [`ServeError::Store`] when the database cannot be opened.
    pub fn open(config: Config) -> Result<Arc<Self>, ServeError> {
        let store = Store::open(&config.store, config.retention).map_err(ServeError::Store)?;
        let (stop, _) = watch::channel(false);

        Ok(Arc::new(Self {
            inner: Arc::new(BusInner {
                config,
                topics: RwLock::new(HashMap::new()),
                connections: AtomicUsize::new(0),
                stop,
                store,
            }),
        }))
    }

    /// Stops the database thread so a later open on the same file sees every commit.
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`] when the thread does not stop cleanly.
    pub async fn shutdown(&self) -> Result<(), StoreError> {
        self.inner.store.shutdown().await
    }

    pub fn stop_handle(&self) -> watch::Sender<bool> {
        self.inner.stop.clone()
    }

    pub fn stop_receiver(&self) -> watch::Receiver<bool> {
        self.inner.stop.subscribe()
    }

    pub fn store(&self) -> &Store {
        &self.inner.store
    }

    /// Registers a topic and returns its entry plus a receiver for its commit notifications. The
    /// receiver must be held for as long as the session reads, or the reclaim sweep can drop the
    /// events this reader has not pulled yet.
    ///
    /// # Errors
    ///
    /// Returns [`OpenError::Refused`] at the topic ceiling and [`OpenError::Store`] when the
    /// topic row cannot be created.
    pub async fn open_topic(
        &self,
        topic: Topic,
    ) -> Result<(Arc<TopicEntry>, watch::Receiver<Window>), OpenError> {
        if let Some((entry, ticks)) = self.lookup(topic) {
            return Ok((entry, ticks));
        }

        let ceiling = self.inner.config.limits.topics.get();
        if self.topic_count() >= ceiling {
            self.sweep().await;

            if let Some((entry, ticks)) = self.lookup(topic) {
                return Ok((entry, ticks));
            }
            if self.topic_count() >= ceiling {
                return Err(OpenError::Refused(Refusal::TopicLimit));
            }
        }

        let (state, created) = self
            .inner
            .store
            .open_topic(topic)
            .await
            .map_err(OpenError::Store)?;
        if created {
            tracing::info!(%topic, "opened a topic");
        }

        let entry = Arc::new(TopicEntry::new(self.inner.store.clone(), state));
        let ticks = entry.appended.subscribe();

        let mut topics = self
            .inner
            .topics
            .write()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(existing) = topics.get(&topic) {
            return Ok((Arc::clone(existing), existing.appended.subscribe()));
        }
        topics.insert(topic, Arc::clone(&entry));
        drop(topics);

        Ok((entry, ticks))
    }

    fn lookup(
        &self,
        topic: Topic,
    ) -> Option<(Arc<TopicEntry>, watch::Receiver<Window>)> {
        let topics = self
            .inner
            .topics
            .read()
            .unwrap_or_else(PoisonError::into_inner);

        topics
            .get(&topic)
            .map(|entry| (Arc::clone(entry), entry.appended.subscribe()))
    }

    fn topic_count(&self) -> usize {
        self.inner
            .topics
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    /// Reclaims every idle topic nobody watches, so the ceiling is room for a new one. The
    /// events go but the row and its epoch stay, which is what lets a returning checkpoint be
    /// told `Lapsed` rather than `Unresumable`.
    async fn sweep(&self) {
        let now = SystemTime::now();
        let idle = self.inner.config.retention.idle;

        let candidates: Vec<(Topic, Arc<TopicEntry>)> = {
            let topics = self
                .inner
                .topics
                .read()
                .unwrap_or_else(PoisonError::into_inner);

            topics
                .iter()
                .filter(|(_, entry)| {
                    entry.appended.receiver_count() == 0 && entry.is_idle(now, idle)
                })
                .map(|(topic, entry)| (*topic, Arc::clone(entry)))
                .collect()
        };

        for (topic, entry) in candidates {
            // Rechecked after the lock is released: a session may have subscribed meanwhile, and
            // a watched topic must never lose the events it has not pulled.
            if entry.appended.receiver_count() != 0 {
                continue;
            }

            match self.inner.store.reclaim(topic, now).await {
                Ok(()) => tracing::info!(%topic, "reclaimed an idle topic"),
                Err(error) => {
                    tracing::warn!(%topic, %error, "could not reclaim a topic");
                    continue;
                },
            }

            let mut topics = self
                .inner
                .topics
                .write()
                .unwrap_or_else(PoisonError::into_inner);
            // A session that subscribed while the reclaim was in flight holds this entry. Dropping
            // it would orphan that reader on a watch no publisher updates, so the entry stays and
            // the reader meets the gap as a lapse.
            let attached = entry.appended.receiver_count() != 0;
            if !attached
                && topics
                    .get(&topic)
                    .is_some_and(|current| Arc::ptr_eq(current, &entry))
            {
                topics.remove(&topic);
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
    fn new(
        store: Store,
        state: TopicState,
    ) -> Self {
        let (appended, _) = watch::channel(Window::new(state.epoch, state.first, state.next));

        Self {
            store,
            appended,
            last_append: AppendClock::new(state.last_append),
        }
    }

    pub fn window(&self) -> Window {
        *self.appended.borrow()
    }

    fn is_idle(
        &self,
        now: SystemTime,
        idle: Duration,
    ) -> bool {
        self.last_append.idle(now, idle)
    }

    /// Appends to this entry's topic. The registry lock is not touched: the session already holds
    /// the entry from the `Open` frame, so two publishers on different topics never contend.
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`] when the append cannot be committed.
    pub async fn append(
        &self,
        topic: Topic,
        payload: Payload,
    ) -> Result<Published, StoreError> {
        let at = SystemTime::now();
        let (published, window) = self.store.append(topic, payload, at).await?;

        self.last_append.record(at);
        // Only after the commit resolves. A reader woken onto an append a crash would lose is the
        // loss this change removes.
        self.appended.send_replace(window);

        Ok(published)
    }
}

impl Drop for ConnectionPermit {
    fn drop(&mut self) {
        self.inner.connections.fetch_sub(1, Ordering::AcqRel);
    }
}

#[cfg(test)]
mod tests {
    use super::{Bus, OpenError, StoreError};
    use crate::config::{Config, Limits, Retention};
    use crate::event::{Refusal, Seq};
    use crate::payload::Payload;
    use crate::topic::Topic;
    use mokmok_protocol::AgentID;
    use std::num::NonZeroUsize;
    use std::path::PathBuf;
    use std::time::Duration;
    use uuid::Uuid;

    const OTHER: Topic = Topic::Agent(AgentID::from_uuid(Uuid::from_u128(1)));

    fn config(
        topics: usize,
        idle: Duration,
    ) -> Config {
        Config {
            store: PathBuf::from(":memory:"),
            limits: Limits {
                topics: NonZeroUsize::new(topics).unwrap(),
                ..Limits::default()
            },
            retention: Retention {
                idle,
                ..Retention::default()
            },
            ..Config::default()
        }
    }

    fn max(events: usize) -> NonZeroUsize {
        NonZeroUsize::new(events).unwrap()
    }

    fn payload() -> Payload {
        Payload::json(&0).unwrap()
    }

    #[tokio::test]
    async fn a_watched_topic_holds_its_slot_against_the_ceiling() {
        let bus = Bus::open(config(1, Duration::from_mins(10))).unwrap();
        let Ok((_entry, _ticks)) = bus.open_topic(Topic::Global).await else {
            panic!("the first topic was refused");
        };

        let refused = bus.open_topic(OTHER).await;

        assert!(matches!(
            refused,
            Err(OpenError::Refused(Refusal::TopicLimit))
        ));
        bus.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn the_reclaim_sweep_spares_a_topic_a_session_is_watching() {
        let bus = Bus::open(config(1, Duration::ZERO)).unwrap();
        let Ok((entry, _ticks)) = bus.open_topic(Topic::Global).await else {
            panic!("the first topic was refused");
        };
        for _ in 0..3 {
            entry.append(Topic::Global, payload()).await.unwrap();
        }

        let refused = bus.open_topic(OTHER).await;

        assert!(matches!(
            refused,
            Err(OpenError::Refused(Refusal::TopicLimit))
        ));
        let (_, events) = bus
            .store()
            .read(Topic::Global, Seq::new(0), max(8))
            .await
            .unwrap();
        assert_eq!(events.len(), 3);
        assert_eq!(
            events
                .iter()
                .map(|event| event.seq().get())
                .collect::<Vec<_>>(),
            [0, 1, 2]
        );
        bus.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn the_reclaim_sweep_spares_a_topic_that_is_not_idle() {
        let bus = Bus::open(config(1, Duration::from_mins(10))).unwrap();
        let Ok((entry, ticks)) = bus.open_topic(Topic::Global).await else {
            panic!("the first topic was refused");
        };
        for _ in 0..3 {
            entry.append(Topic::Global, payload()).await.unwrap();
        }
        drop(ticks);

        let refused = bus.open_topic(OTHER).await;

        assert!(matches!(
            refused,
            Err(OpenError::Refused(Refusal::TopicLimit))
        ));
        let (_, events) = bus
            .store()
            .read(Topic::Global, Seq::new(0), max(8))
            .await
            .unwrap();
        assert_eq!(events.len(), 3);
        bus.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn shutdown_closes_the_store_the_bus_holds() {
        let bus = Bus::open(config(1, Duration::from_mins(10))).unwrap();
        let Ok((entry, _ticks)) = bus.open_topic(Topic::Global).await else {
            panic!("the first topic was refused");
        };
        bus.shutdown().await.unwrap();

        let appended = entry.append(Topic::Global, payload()).await;

        assert!(matches!(appended, Err(StoreError::Closed)));
    }
}

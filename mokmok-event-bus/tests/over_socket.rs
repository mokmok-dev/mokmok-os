use mokmok_event_bus::{
    Batch, Client, Config, Cursor, Error, Limits, Payload, Refusal, Retention, Seq, ServeError,
    SubscriberId, Timeouts, Topic, serve,
};
use mokmok_protocol::AgentID;
use serde::Serialize;
use std::num::{NonZeroU32, NonZeroUsize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use uuid::Uuid;

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use std::sync::{Arc, Mutex, OnceLock};
    use tokio::net::UnixStream;
    use tokio_tungstenite::tungstenite::Message;

    static NEXT: AtomicUsize = AtomicUsize::new(0);

    /// The path `serve` routes; the host is ignored for a socket the harness opened itself.
    const URL: &str = "ws://localhost/ws";

    /// Long enough for a healthy bus to answer, short enough that a silenced one fails the test
    /// instead of hanging the suite.
    const BOUND: Duration = Duration::from_secs(3);

    fn unique(name: &str) -> PathBuf {
        let id = NEXT.fetch_add(1, Ordering::Relaxed);

        std::env::temp_dir().join(format!("mokmok-{name}-{}-{id}", std::process::id()))
    }

    fn socket_path() -> PathBuf {
        unique("bus")
    }

    fn store_path() -> PathBuf {
        unique("store")
    }

    /// A fresh socket and store per call, so no two tests share a database.
    fn config() -> Config {
        config_with(store_path())
    }

    fn config_with(store: PathBuf) -> Config {
        Config {
            path: socket_path(),
            store,
            ..Config::default()
        }
    }

    fn remove_store(store: &Path) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", store.display()));
        }
    }

    const fn agent_topic() -> Topic {
        Topic::Agent(AgentID::from_uuid(Uuid::from_u128(
            0x018f_2f3a_1b2c_7d3e_9f10_2233_4455_6677,
        )))
    }

    fn payload<T: Serialize + ?Sized>(value: &T) -> Payload {
        Payload::json(value).unwrap()
    }

    fn seqs(batch: &Batch) -> Vec<u64> {
        batch
            .events()
            .iter()
            .map(|event| event.seq().get())
            .collect()
    }

    fn payloads(batch: &Batch) -> Vec<String> {
        batch
            .events()
            .iter()
            .map(|event| event.payload().as_str().to_owned())
            .collect()
    }

    async fn within<T>(
        what: &str,
        future: impl std::future::Future<Output = T>,
    ) -> T {
        match tokio::time::timeout(BOUND, future).await {
            Ok(value) => value,
            Err(elapsed) => panic!("{what} did not answer within {BOUND:?}: {elapsed}"),
        }
    }

    async fn connect(path: &Path) -> tokio_tungstenite::WebSocketStream<UnixStream> {
        let stream = within("connect", UnixStream::connect(path)).await.unwrap();
        let (peer, _) = within("upgrade", tokio_tungstenite::client_async(URL, stream))
            .await
            .unwrap();

        peer
    }

    /// The first non-control frame. The bus pings on its first heartbeat tick, so a raw peer sees
    /// that ping before the answer it is waiting for.
    ///
    /// The deadline covers the whole wait rather than each frame, because a bus that answers
    /// nothing but pings would otherwise keep this loop alive forever.
    async fn next_frame(peer: &mut tokio_tungstenite::WebSocketStream<UnixStream>) -> Message {
        let deadline = tokio::time::Instant::now() + BOUND;

        loop {
            match tokio::time::timeout_at(deadline, peer.next()).await {
                Ok(Some(Ok(Message::Ping(_) | Message::Pong(_)))) => {},
                Ok(Some(Ok(frame))) => return frame,
                Ok(Some(Err(error))) => panic!("expected a frame, got {error}"),
                Ok(None) => panic!("the peer closed before sending a frame"),
                Err(elapsed) => panic!("no data frame within {BOUND:?}: {elapsed}"),
            }
        }
    }

    async fn send_json(
        peer: &mut tokio_tungstenite::WebSocketStream<UnixStream>,
        value: &impl serde::Serialize,
    ) {
        let text = serde_json::to_string(value).unwrap();
        within("send", peer.send(Message::Text(text.into())))
            .await
            .unwrap();
    }

    #[derive(Clone)]
    struct Capture(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for Capture {
        fn write(
            &mut self,
            bytes: &[u8],
        ) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);

            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
        type Writer = Self;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    static CAPTURE: OnceLock<Capture> = OnceLock::new();

    fn capture() -> &'static Capture {
        CAPTURE.get_or_init(|| {
            let capture = Capture(Arc::new(Mutex::new(Vec::new())));
            let _ = tracing_subscriber::fmt()
                .with_max_level(tracing::Level::DEBUG)
                .with_ansi(false)
                .with_writer(capture.clone())
                .try_init();

            capture
        })
    }

    struct Bus {
        path: PathBuf,
        store: PathBuf,
        stop: Option<oneshot::Sender<()>>,
        serving: Option<JoinHandle<Result<(), ServeError>>>,
    }

    impl Bus {
        async fn start(config: Config) -> Self {
            let path = config.path.clone();
            let store = config.store.clone();
            let (stop, stopped) = oneshot::channel();
            let serving = tokio::spawn(serve(config, async move {
                let _ = stopped.await;
            }));
            let bus = Self {
                path,
                store,
                stop: Some(stop),
                serving: Some(serving),
            };

            for _ in 0..500 {
                if bus.path.exists() {
                    return bus;
                }

                tokio::time::sleep(Duration::from_millis(10)).await;
            }

            panic!("the bus never bound {}", bus.path.display());
        }

        async fn shutdown(&mut self) -> Result<(), ServeError> {
            if let Some(stop) = self.stop.take() {
                let _ = stop.send(());
            }
            let serving = self.serving.take().expect("the bus is still serving");

            match tokio::time::timeout(Duration::from_secs(5), serving).await {
                Ok(Ok(served)) => served,
                Ok(Err(failure)) => panic!("the serve task failed: {failure}"),
                Err(elapsed) => panic!("serve did not return: {elapsed}"),
            }
        }
    }

    impl Drop for Bus {
        fn drop(&mut self) {
            if let Some(stop) = self.stop.take() {
                let _ = stop.send(());
            }
            if let Some(serving) = self.serving.take() {
                serving.abort();
            }

            remove_store(&self.store);
            let _ = std::fs::remove_file(&self.path);
        }
    }

    #[tokio::test]
    async fn publishing_reaches_a_subscriber_that_started_first() {
        let bus = Bus::start(config()).await;
        let topic = agent_topic();
        let client = within("connect", Client::connect(&bus.path)).await.unwrap();
        let mut subscriber = within("subscribe", client.subscribe(topic, SubscriberId::new()))
            .await
            .unwrap();

        let receipt = within(
            "publish",
            client.publish(topic, payload(&serde_json::json!({"turn": 7}))),
        )
        .await
        .unwrap();
        let batch = within("next", subscriber.next()).await.unwrap();

        assert_eq!(batch.events().len(), 1);
        assert_eq!(batch.events()[0].id(), receipt.id());
        assert_eq!(batch.events()[0].topic(), topic);
        assert_eq!(batch.events()[0].payload().as_str(), r#"{"turn":7}"#);
        assert_eq!(receipt.seq().get(), 0);
    }

    #[tokio::test]
    async fn two_subscribers_each_receive_every_event() {
        let bus = Bus::start(config()).await;
        let client = within("connect", Client::connect(&bus.path)).await.unwrap();
        let mut first = within(
            "first subscribe",
            client.subscribe(Topic::Global, SubscriberId::new()),
        )
        .await
        .unwrap();
        let mut second = within(
            "second subscribe",
            client.subscribe(Topic::Global, SubscriberId::new()),
        )
        .await
        .unwrap();

        for turn in 1..=3 {
            within(
                "publish",
                client.publish(Topic::Global, payload(&serde_json::json!({"turn": turn}))),
            )
            .await
            .unwrap();
        }

        let first_batch = within("first next", first.next()).await.unwrap();
        let second_batch = within("second next", second.next()).await.unwrap();

        assert_eq!(seqs(&first_batch), [0, 1, 2]);
        assert_eq!(
            payloads(&first_batch),
            [r#"{"turn":1}"#, r#"{"turn":2}"#, r#"{"turn":3}"#],
        );
        assert_eq!(
            first_batch
                .events()
                .iter()
                .map(|event| event.id())
                .collect::<Vec<_>>(),
            second_batch
                .events()
                .iter()
                .map(|event| event.id())
                .collect::<Vec<_>>(),
        );
    }

    #[tokio::test]
    async fn an_event_survives_a_bus_restart() {
        let store = store_path();
        let topic = Topic::Global;
        let mut first = Bus::start(config_with(store.clone())).await;
        let client = within("connect", Client::connect(&first.path))
            .await
            .unwrap();

        let receipt = within("publish", client.publish(topic, payload(&"durable")))
            .await
            .unwrap();
        drop(client);
        first.shutdown().await.unwrap();

        let second = Bus::start(config_with(store.clone())).await;
        let client = within("connect", Client::connect(&second.path))
            .await
            .unwrap();
        let mut resumed = within("resume", client.subscribe(topic, SubscriberId::new()))
            .await
            .unwrap();
        let batch = within("next", resumed.next()).await.unwrap();

        assert_eq!(batch.events().len(), 1);
        assert_eq!(batch.events()[0].id(), receipt.id());
        assert_eq!(payloads(&batch), [r#""durable""#]);
    }

    #[tokio::test]
    async fn a_reconnect_replays_events_that_were_not_acked() {
        let bus = Bus::start(config()).await;
        let topic = Topic::Global;
        let id = SubscriberId::new();
        let client = within("connect", Client::connect(&bus.path)).await.unwrap();

        within("first publish", client.publish(topic, payload(&"first")))
            .await
            .unwrap();
        within("second publish", client.publish(topic, payload(&"second")))
            .await
            .unwrap();

        let mut subscriber = within("subscribe", client.subscribe(topic, id))
            .await
            .unwrap();
        let first = within("next", subscriber.next()).await.unwrap();
        assert_eq!(seqs(&first), [0, 1]);
        drop(subscriber);

        within("third publish", client.publish(topic, payload(&"third")))
            .await
            .unwrap();

        let mut resumed = within("resume", client.subscribe(topic, id)).await.unwrap();
        let replayed = within("replayed next", resumed.next()).await.unwrap();

        assert_eq!(
            seqs(&replayed),
            [0, 1, 2],
            "an un-acked batch is replayed even when newer events exist",
        );
    }

    #[tokio::test]
    async fn a_replayed_event_carries_the_same_id() {
        let bus = Bus::start(config()).await;
        let topic = Topic::Global;
        let id = SubscriberId::new();
        let client = within("connect", Client::connect(&bus.path)).await.unwrap();
        within("publish", client.publish(topic, payload(&1)))
            .await
            .unwrap();

        let mut subscriber = within("subscribe", client.subscribe(topic, id))
            .await
            .unwrap();
        let first = within("next", subscriber.next()).await.unwrap();
        let original = first.events()[0].id();
        drop(subscriber);

        let mut resumed = within("resume", client.subscribe(topic, id)).await.unwrap();
        let replayed = within("replayed next", resumed.next()).await.unwrap();

        assert_eq!(
            replayed.events()[0].id(),
            original,
            "a duplicate must carry the same id so the consumer can deduplicate",
        );
    }

    #[tokio::test]
    async fn a_slow_consumer_is_handed_at_most_in_flight_events() {
        let bus = Bus::start(Config {
            limits: Limits {
                in_flight: NonZeroUsize::new(2).unwrap(),
                ..Limits::default()
            },
            ..config()
        })
        .await;
        let topic = Topic::Global;
        let client = within("connect", Client::connect(&bus.path)).await.unwrap();

        for turn in 0..10 {
            within("publish", client.publish(topic, payload(&turn)))
                .await
                .unwrap();
        }

        let mut subscriber = within("subscribe", client.subscribe(topic, SubscriberId::new()))
            .await
            .unwrap();
        let first = within("first next", subscriber.next()).await.unwrap();

        assert_eq!(
            seqs(&first),
            [0, 1],
            "a batch stops at the in-flight ceiling",
        );

        within("ack", subscriber.ack(first.after())).await.unwrap();
        let freed = within("freed next", subscriber.next()).await.unwrap();

        assert_eq!(
            seqs(&freed),
            [2, 3],
            "acking frees the credit for the next batch",
        );
    }

    #[tokio::test]
    async fn an_ack_advances_the_checkpoint_so_it_is_not_replayed() {
        let bus = Bus::start(config()).await;
        let topic = Topic::Global;
        let id = SubscriberId::new();
        let client = within("connect", Client::connect(&bus.path)).await.unwrap();

        for turn in 0..3 {
            within("publish", client.publish(topic, payload(&turn)))
                .await
                .unwrap();
        }

        let mut subscriber = within("subscribe", client.subscribe(topic, id))
            .await
            .unwrap();
        let batch = within("next", subscriber.next()).await.unwrap();
        assert_eq!(seqs(&batch), [0, 1, 2]);
        within("ack", subscriber.ack(batch.after())).await.unwrap();
        drop(subscriber);

        within("later publish", client.publish(topic, payload(&3)))
            .await
            .unwrap();

        let mut resumed = within("resume", client.subscribe(topic, id)).await.unwrap();
        let next = within("next after ack", resumed.next()).await.unwrap();

        assert_eq!(seqs(&next), [3], "the acked batch is not replayed");
    }

    #[tokio::test]
    async fn an_ack_answers_alone_so_the_pull_after_it_reads_the_next_event() {
        let bus = Bus::start(Config {
            limits: Limits {
                in_flight: NonZeroUsize::new(3).unwrap(),
                ..Limits::default()
            },
            ..config()
        })
        .await;
        let topic = Topic::Global;
        let client = within("connect", Client::connect(&bus.path)).await.unwrap();

        for turn in 0..3 {
            within("publish", client.publish(topic, payload(&turn)))
                .await
                .unwrap();
        }

        let mut subscriber = within("subscribe", client.subscribe(topic, SubscriberId::new()))
            .await
            .unwrap();
        let first = within("first next", subscriber.next()).await.unwrap();
        assert_eq!(seqs(&first), [0, 1, 2]);

        within("ack", subscriber.ack(first.after())).await.unwrap();

        within("fourth publish", client.publish(topic, payload(&3)))
            .await
            .unwrap();

        let second = within("second next", subscriber.next()).await.unwrap();
        assert_eq!(
            seqs(&second),
            [3],
            "a frame the ack left behind would be read as this pull's answer",
        );

        within("fifth publish", client.publish(topic, payload(&4)))
            .await
            .unwrap();
        within("second ack", subscriber.ack(second.after()))
            .await
            .unwrap();

        let third = within("third next", subscriber.next()).await.unwrap();

        assert_eq!(seqs(&third), [4]);
    }

    #[tokio::test]
    async fn an_ack_past_what_was_delivered_does_not_advance_the_checkpoint() {
        let bus = Bus::start(config()).await;
        let topic = Topic::Global;
        let id = SubscriberId::new();
        let client = within("connect", Client::connect(&bus.path)).await.unwrap();

        for turn in 0..2 {
            within("publish", client.publish(topic, payload(&turn)))
                .await
                .unwrap();
        }

        let mut subscriber = within("subscribe", client.subscribe(topic, id))
            .await
            .unwrap();
        let batch = within("next", subscriber.next()).await.unwrap();
        let epoch = batch.window().epoch();
        within("ack", subscriber.ack(batch.after())).await.unwrap();

        // The checkpoint is at two; an ack that names ninety-nine must not move it there.
        within("over-ack", subscriber.ack(Cursor::new(epoch, Seq::new(99))))
            .await
            .unwrap();
        drop(subscriber);

        within("later publish", client.publish(topic, payload(&2)))
            .await
            .unwrap();

        let mut resumed = within("resume", client.subscribe(topic, id)).await.unwrap();
        let next = within("next after over-ack", resumed.next()).await.unwrap();

        assert_eq!(
            seqs(&next),
            [2],
            "the checkpoint stayed where the ack left it"
        );
    }

    #[tokio::test]
    async fn an_ack_from_a_stale_epoch_does_not_advance_the_checkpoint() {
        let bus = Bus::start(config()).await;
        let topic = Topic::Global;
        let id = SubscriberId::new();
        let client = within("connect", Client::connect(&bus.path)).await.unwrap();

        for turn in 0..2 {
            within("publish", client.publish(topic, payload(&turn)))
                .await
                .unwrap();
        }

        // A second topic mints its own epoch, which is a foreign one for the first.
        let mut other = client
            .subscribe(agent_topic(), SubscriberId::new())
            .await
            .unwrap();
        within("other publish", client.publish(agent_topic(), payload(&9)))
            .await
            .unwrap();
        let other_batch = within("other next", other.next()).await.unwrap();

        let mut subscriber = within("subscribe", client.subscribe(topic, id))
            .await
            .unwrap();
        let batch = within("next", subscriber.next()).await.unwrap();
        assert_ne!(batch.window().epoch(), other_batch.window().epoch());

        within(
            "stale ack",
            subscriber.ack(Cursor::new(other_batch.window().epoch(), Seq::new(1))),
        )
        .await
        .unwrap();
        drop(subscriber);

        let mut resumed = within("resume", client.subscribe(topic, id)).await.unwrap();
        let replayed = within("replayed next", resumed.next()).await.unwrap();

        assert_eq!(
            seqs(&replayed),
            [0, 1],
            "a stale-epoch ack must not move the checkpoint",
        );
    }

    #[tokio::test]
    async fn a_checkpoint_older_than_retention_gets_lapsed() {
        let bus = Bus::start(Config {
            retention: Retention {
                events: NonZeroUsize::new(2).unwrap(),
                ..Retention::default()
            },
            ..config()
        })
        .await;
        let topic = Topic::Global;
        let id = SubscriberId::new();
        let client = within("connect", Client::connect(&bus.path)).await.unwrap();

        within("first publish", client.publish(topic, payload(&0)))
            .await
            .unwrap();
        within("second publish", client.publish(topic, payload(&1)))
            .await
            .unwrap();

        let mut subscriber = within("subscribe", client.subscribe(topic, id))
            .await
            .unwrap();
        let first = within("first next", subscriber.next()).await.unwrap();
        assert_eq!(seqs(&first), [0, 1]);
        within("ack", subscriber.ack(first.after())).await.unwrap();
        drop(subscriber);

        for turn in 2..6 {
            within("publish", client.publish(topic, payload(&turn)))
                .await
                .unwrap();
        }

        let mut resumed = within("resume", client.subscribe(topic, id)).await.unwrap();
        let behind = within("behind next", resumed.next()).await.unwrap();

        let lapsed = behind.lapsed().unwrap();
        assert_eq!(lapsed.held().seq().get(), 2);
        assert_eq!(lapsed.from().get(), 4);
        assert_eq!(lapsed.missed(), 2);
        assert_eq!(seqs(&behind), [4, 5]);
    }

    #[tokio::test]
    async fn a_checkpoint_from_a_reclaimed_topic_gets_lapsed() {
        let bus = Bus::start(Config {
            limits: Limits {
                topics: NonZeroUsize::new(1).unwrap(),
                ..Limits::default()
            },
            retention: Retention {
                idle: Duration::ZERO,
                ..Retention::default()
            },
            ..config()
        })
        .await;
        let id = SubscriberId::new();
        let client = within("connect", Client::connect(&bus.path)).await.unwrap();

        for turn in 0..2 {
            within("publish", client.publish(Topic::Global, payload(&turn)))
                .await
                .unwrap();
        }

        let mut subscriber = within("subscribe", client.subscribe(Topic::Global, id))
            .await
            .unwrap();
        let batch = within("next", subscriber.next()).await.unwrap();
        let epoch = batch.window().epoch();
        within("ack", subscriber.ack(Cursor::new(epoch, Seq::new(1))))
            .await
            .unwrap();
        drop(subscriber);
        drop(client);

        // Only an open at the topic ceiling sweeps, so a second topic forces the reclaim. It is
        // dropped again so the first topic can be reopened; each retry waits out the old session.
        let mut forced = None;
        for _ in 0..100 {
            let probe = Client::connect(&bus.path).await;
            if let Ok(client) = probe
                && client.publish(agent_topic(), payload(&0)).await.is_ok()
            {
                forced = Some(client);
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        drop(forced.expect("the second topic was never accepted"));

        let mut reopened = None;
        for _ in 0..100 {
            let probe = Client::connect(&bus.path).await;
            if let Ok(client) = probe
                && client.publish(Topic::Global, payload(&2)).await.is_ok()
            {
                reopened = Some(client);
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let reopened = reopened.expect("the first topic was never reopened");

        let mut resumed = within("resume", reopened.subscribe(Topic::Global, id))
            .await
            .unwrap();
        let answered = within("resumed next", resumed.next()).await.unwrap();

        let lapsed = answered
            .lapsed()
            .expect("a reclaimed topic must report the hole, not refuse it");
        assert_eq!(lapsed.held().seq().get(), 1);
        assert_eq!(lapsed.from().get(), 2);
        assert_eq!(lapsed.held().epoch(), answered.window().epoch());
        assert_eq!(seqs(&answered), [2]);
    }

    #[tokio::test]
    async fn a_subscriber_from_a_previous_epoch_is_refused_at_open() {
        let store = store_path();
        let topic = Topic::Global;
        let id = SubscriberId::new();
        let mut first = Bus::start(config_with(store.clone())).await;
        let client = within("connect", Client::connect(&first.path))
            .await
            .unwrap();

        within("publish", client.publish(topic, payload(&1)))
            .await
            .unwrap();
        let mut subscriber = within("subscribe", client.subscribe(topic, id))
            .await
            .unwrap();
        let batch = within("next", subscriber.next()).await.unwrap();
        let epoch = batch.window().epoch();
        within("ack", subscriber.ack(Cursor::new(epoch, Seq::new(1))))
            .await
            .unwrap();
        drop(subscriber);
        drop(client);
        first.shutdown().await.unwrap();

        // The topic row is replaced while the checkpoint survives, which is the one way the bus
        // can hold an id whose epoch it cannot account for.
        let connection = rusqlite::Connection::open(&store).unwrap();
        connection.execute("DELETE FROM topic", []).unwrap();
        drop(connection);

        let second = Bus::start(config_with(store.clone())).await;
        let client = within("connect", Client::connect(&second.path))
            .await
            .unwrap();
        let refused = within("resume", client.subscribe(topic, id)).await;

        assert!(
            matches!(refused, Err(Error::Refused(Refusal::Unresumable { .. }))),
            "expected an unresumable subscriber at open, got {:?}",
            refused.err(),
        );
    }

    #[tokio::test]
    async fn a_burst_arrives_in_order_exactly_once() {
        let bus = Bus::start(config()).await;
        let topic = Topic::Global;
        let client = within("connect", Client::connect(&bus.path)).await.unwrap();
        let mut subscriber = within("subscribe", client.subscribe(topic, SubscriberId::new()))
            .await
            .unwrap();

        for turn in 0..1000 {
            within("publish", client.publish(topic, payload(&turn)))
                .await
                .unwrap();
        }

        let mut seen = Vec::new();
        while seen.len() < 1000 {
            let batch = within("burst next", subscriber.next()).await.unwrap();
            assert!(!batch.is_empty(), "a batch must carry at least one event");
            let start = seen.len();
            for (offset, event) in batch.events().iter().enumerate() {
                assert_eq!(event.seq().get(), (start + offset) as u64);
                seen.push(event.seq().get());
            }
            within("ack", subscriber.ack(batch.after())).await.unwrap();
        }

        assert_eq!(seen, (0..1000).collect::<Vec<u64>>());
    }

    #[tokio::test]
    async fn the_bus_refuses_a_connection_past_its_ceiling() {
        let bus = Bus::start(Config {
            limits: Limits {
                connections: NonZeroUsize::new(1).unwrap(),
                ..Limits::default()
            },
            ..config()
        })
        .await;
        let busy = within("busy connect", Client::connect(&bus.path))
            .await
            .unwrap();
        let _kept = busy
            .subscribe(Topic::Global, SubscriberId::new())
            .await
            .unwrap();

        let second = within("second connect", Client::connect(&bus.path)).await;

        assert!(
            matches!(second, Err(Error::Refused(Refusal::ConnectionLimit))),
            "expected a refusal at the ceiling, got {:?}",
            second.err(),
        );
        assert!(
            within("third connect", Client::connect(&bus.path))
                .await
                .is_err(),
        );
    }

    #[tokio::test]
    async fn an_abandoned_parked_subscriber_frees_its_connection() {
        let bus = Bus::start(Config {
            limits: Limits {
                connections: NonZeroUsize::new(1).unwrap(),
                ..Limits::default()
            },
            ..config()
        })
        .await;
        let client = within("connect", Client::connect(&bus.path)).await.unwrap();

        let mut subscriber = within(
            "subscribe",
            client.subscribe(Topic::Global, SubscriberId::new()),
        )
        .await
        .unwrap();
        // Nothing has been published, so this pull parks the session; the timeout only bounds
        // how long the test waits before abandoning the client.
        let _ = tokio::time::timeout(Duration::from_millis(150), subscriber.next()).await;
        drop(subscriber);
        drop(client);

        let mut accepted = None;
        for _ in 0..100 {
            if let Ok(publisher) = within("retry connect", Client::connect(&bus.path)).await
                && publisher.publish(Topic::Global, payload(&0)).await.is_ok()
            {
                accepted = Some(publisher);
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        assert!(
            accepted.is_some(),
            "a fresh connection was never accepted after the parked subscriber went away",
        );
    }

    #[tokio::test]
    async fn a_parked_pull_delivers_the_next_event() {
        let bus = Bus::start(config()).await;
        let client = within("connect", Client::connect(&bus.path)).await.unwrap();
        let mut subscriber = within(
            "subscribe",
            client.subscribe(Topic::Global, SubscriberId::new()),
        )
        .await
        .unwrap();

        let parked = tokio::spawn(async move { subscriber.next().await });
        // The pull has to reach the server and park before the publish, or the subscriber reads
        // a non-empty batch and the wake path never runs.
        tokio::time::sleep(Duration::from_millis(200)).await;

        let receipt = within(
            "publish",
            client.publish(Topic::Global, payload(&serde_json::json!({"turn": 9}))),
        )
        .await
        .unwrap();

        let batch = within("parked next", parked).await.unwrap().unwrap();

        assert_eq!(batch.events().len(), 1);
        assert_eq!(batch.events()[0].payload().as_str(), r#"{"turn":9}"#);
        assert_eq!(batch.events()[0].seq(), receipt.seq());
        assert_eq!(batch.events()[0].id(), receipt.id());
    }

    #[tokio::test]
    async fn a_malformed_frame_closes_the_session() {
        let bus = Bus::start(Config {
            timeouts: Timeouts {
                heartbeat: NonZeroU32::new(20).unwrap(),
                deadline: NonZeroU32::new(300).unwrap(),
            },
            ..config()
        })
        .await;
        let mut peer = connect(&bus.path).await;

        // The bus pings on its first heartbeat tick, and tungstenite only queues the pong when it
        // reads that ping. Reading the ping before this client sends anything lets the pong leave
        // with the open frame, while the bus is alive.
        let ping = within("first heartbeat ping", peer.next()).await;
        assert!(
            matches!(ping, Some(Ok(Message::Ping(_)))),
            "expected the heartbeat ping before any answer, got {ping:?}",
        );

        send_json(
            &mut peer,
            &serde_json::json!({
                "op": "subscribe",
                "req_id": 1,
                "topic": "global",
                "subscriber": SubscriberId::new().to_string(),
            }),
        )
        .await;
        let subscribed = next_frame(&mut peer).await;
        let Message::Text(text) = subscribed else {
            panic!("expected the subscribed frame, got {subscribed:?}");
        };
        assert!(
            text.contains(r#""op":"subscribed""#),
            "expected a subscribed answer, got {text}",
        );

        // A publish frame that carries a subscription id is malformed under `deny_unknown_fields`.
        send_json(
            &mut peer,
            &serde_json::json!({
                "op": "publish",
                "req_id": 2,
                "topic": "global",
                "payload": 1,
                "sub": 0,
            }),
        )
        .await;

        loop {
            match within("close answer", peer.next()).await {
                Some(Ok(Message::Close(_))) => break,
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => {},
                other => panic!("expected a close frame, got {other:?}"),
            }
        }

        let ended = within("session end", peer.next()).await;
        assert!(
            matches!(ended, None | Some(Err(_))),
            "expected the connection to end, got {ended:?}"
        );
    }

    #[tokio::test]
    async fn a_client_that_closes_its_connection_releases_it() {
        let bus = Bus::start(Config {
            limits: Limits {
                connections: NonZeroUsize::new(1).unwrap(),
                ..Limits::default()
            },
            ..config()
        })
        .await;
        let client = within("connect", Client::connect(&bus.path)).await.unwrap();
        client
            .subscribe(Topic::Global, SubscriberId::new())
            .await
            .unwrap();
        drop(client);

        let mut accepted = None;
        for _ in 0..100 {
            if let Ok(publisher) = within("retry connect", Client::connect(&bus.path)).await
                && publisher.publish(Topic::Global, payload(&0)).await.is_ok()
            {
                accepted = Some(publisher);
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        assert!(
            accepted.is_some(),
            "the closed connection never gave its permit back",
        );
    }

    #[tokio::test]
    async fn a_peer_that_answers_pings_is_kept() {
        let bus = Bus::start(Config {
            timeouts: Timeouts {
                heartbeat: NonZeroU32::new(20).unwrap(),
                deadline: NonZeroU32::new(300).unwrap(),
            },
            ..config()
        })
        .await;
        let mut peer = connect(&bus.path).await;

        send_json(
            &mut peer,
            &serde_json::json!({
                "op": "subscribe",
                "req_id": 1,
                "topic": "global",
                "subscriber": SubscriberId::new().to_string(),
            }),
        )
        .await;
        let _subscribed = next_frame(&mut peer).await;

        // Read past the deadline so tungstenite answers every ping; a peer that answers has to
        // stay connected. The pause keeps an answer in flight across a heartbeat tick, which is
        // what separates answering from not answering.
        let until = tokio::time::Instant::now() + Duration::from_millis(1000);
        while tokio::time::Instant::now() < until {
            tokio::time::sleep(Duration::from_millis(100)).await;
            let _ = tokio::time::timeout(Duration::from_millis(50), peer.next()).await;
            let _ = tokio::time::timeout(Duration::from_millis(50), peer.next()).await;
        }

        send_json(
            &mut peer,
            &serde_json::json!({"op": "publish", "req_id": 2, "topic": "global", "payload": 1}),
        )
        .await;
        let answer = next_frame(&mut peer).await;

        let Message::Text(text) = answer else {
            panic!("expected the published frame, got {answer:?}");
        };
        assert!(
            text.contains(r#""op":"published""#),
            "expected a published answer, got {text}",
        );
    }

    #[tokio::test]
    async fn the_bus_logs_its_live_connection_count() {
        let bus = Bus::start(config()).await;
        let capture = capture();
        capture.0.lock().unwrap().clear();

        let first = within("first connect", Client::connect(&bus.path))
            .await
            .unwrap();
        let second = within("second connect", Client::connect(&bus.path))
            .await
            .unwrap();
        let _ = first.subscribe(Topic::Global, SubscriberId::new()).await;
        let _ = second.subscribe(Topic::Global, SubscriberId::new()).await;

        let logged = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();

        assert!(
            logged.contains("connections=2"),
            "no session logged two live connections: {logged}",
        );
    }

    #[tokio::test]
    async fn shutdown_unlinks_the_socket() {
        let config = config();
        let path = config.path.clone();
        let mut bus = Bus::start(config).await;
        assert!(path.exists(), "the socket exists while serving");

        bus.shutdown().await.unwrap();

        assert!(!path.exists(), "the socket is gone once serve returns");
    }

    #[tokio::test]
    async fn a_frame_larger_than_the_limit_is_refused_without_stopping_the_bus() {
        let bus = Bus::start(Config {
            limits: Limits {
                // Large enough for a subscribe, small enough that the oversized payload below is
                // refused.
                frame_bytes: NonZeroUsize::new(256).unwrap(),
                ..Limits::default()
            },
            ..config()
        })
        .await;
        let client = within("connect", Client::connect(&bus.path)).await.unwrap();
        let publisher = client.clone();
        let oversized = "x".repeat(4096);

        let refused = within(
            "publish",
            publisher.publish(Topic::Global, payload(&oversized)),
        )
        .await;

        // Tungstenite refuses the frame while assembling it and closes the connection, so the
        // caller gets no answer; what matters is that the bus is still there for everyone else.
        assert!(
            matches!(refused, Err(Error::Indeterminate)),
            "expected the over-limit frame to be refused, got {refused:?}",
        );

        let fresh = within("fresh connect", Client::connect(&bus.path))
            .await
            .unwrap();
        let mut subscriber = within(
            "subscribe",
            fresh.subscribe(Topic::Global, SubscriberId::new()),
        )
        .await
        .unwrap();
        within(
            "small publish",
            fresh.publish(Topic::Global, payload(&"small")),
        )
        .await
        .unwrap();

        assert_eq!(
            payloads(&within("next", subscriber.next()).await.unwrap()),
            [r#""small""#],
        );
    }

    #[tokio::test]
    async fn a_peer_that_stops_reading_is_closed() {
        let bus = Bus::start(Config {
            limits: Limits {
                connections: NonZeroUsize::new(1).unwrap(),
                ..Limits::default()
            },
            timeouts: Timeouts {
                heartbeat: NonZeroU32::new(20).unwrap(),
                deadline: NonZeroU32::new(150).unwrap(),
            },
            ..config()
        })
        .await;

        // Nothing ever reads this peer, so it cannot answer the bus's ping. The bus must hang up
        // and release the only connection permit, which the next connect proves.
        let _peer = connect(&bus.path).await;
        tokio::time::sleep(Duration::from_millis(1000)).await;

        let client = within("connect", Client::connect(&bus.path)).await.unwrap();
        assert!(
            client.publish(Topic::Global, payload(&1)).await.is_ok(),
            "the bus freed the permit held by the silent peer",
        );
    }

    #[tokio::test]
    async fn a_consumer_that_never_acks_is_handed_nothing_once_its_checkpoint_lapses() {
        let bus = Bus::start(Config {
            limits: Limits {
                in_flight: NonZeroUsize::new(1).unwrap(),
                ..Limits::default()
            },
            retention: Retention {
                events: NonZeroUsize::new(1).unwrap(),
                ..Retention::default()
            },
            ..config()
        })
        .await;
        let topic = Topic::Global;
        let id = SubscriberId::new();
        let client = within("connect", Client::connect(&bus.path)).await.unwrap();

        within("publish", client.publish(topic, payload(&0)))
            .await
            .unwrap();
        let mut subscriber = within("subscribe", client.subscribe(topic, id))
            .await
            .unwrap();
        let first = within("first next", subscriber.next()).await.unwrap();
        assert_eq!(seqs(&first), [0]);
        within("ack", subscriber.ack(first.after())).await.unwrap();
        drop(subscriber);

        // Two more appends between every pull keeps moving the retained floor past the checkpoint,
        // which is what decides how far a subscriber may read. A consumer that never acks must
        // still be handed no more than its credit.
        let mut subscriber = within("reconnect", client.subscribe(topic, id))
            .await
            .unwrap();
        let mut handed = 0;

        for turn in (1..=6).rev() {
            for _ in 0..2 {
                within("publish", client.publish(topic, payload(&turn)))
                    .await
                    .unwrap();
            }

            let batch = within("next", subscriber.next()).await.unwrap();

            handed += batch.events().len();
            assert!(
                batch.lapsed().is_some(),
                "the lapse is reported on every pull behind the floor",
            );
        }

        assert!(
            handed <= 1,
            "a subscriber that never acked was handed {handed} events against a credit of 1",
        );
    }

    #[tokio::test]
    async fn the_same_connection_publishes_while_a_subscription_is_parked() {
        let bus = Bus::start(config()).await;
        let topic = Topic::Global;
        let client = within("connect", Client::connect(&bus.path)).await.unwrap();
        let mut subscriber = client.subscribe(topic, SubscriberId::new()).await.unwrap();

        // Park a pull before any event exists.
        let parked = tokio::spawn(async move { subscriber.next().await });
        tokio::time::sleep(Duration::from_millis(100)).await;

        // The same connection publishes while the subscription is parked.
        let receipt = client
            .publish(topic, payload(&serde_json::json!({"same": true})))
            .await
            .unwrap();

        let batch = parked.await.unwrap().unwrap();
        assert_eq!(batch.events().len(), 1);
        assert_eq!(batch.events()[0].id(), receipt.id());
    }

    #[tokio::test]
    async fn a_second_pull_on_a_parked_subscription_is_refused() {
        let bus = Bus::start(config()).await;
        let mut peer = connect(&bus.path).await;

        send_json(
            &mut peer,
            &serde_json::json!({
                "op": "subscribe",
                "req_id": 1,
                "topic": "global",
                "subscriber": SubscriberId::new().to_string(),
            }),
        )
        .await;
        let subscribed = next_frame(&mut peer).await;
        let Message::Text(text) = subscribed else {
            panic!("expected a text frame, got {subscribed:?}");
        };
        let subscribed: serde_json::Value = serde_json::from_str(&text).unwrap();
        let sub = subscribed["sub"].as_u64().unwrap();

        send_json(
            &mut peer,
            &serde_json::json!({"op": "pull", "req_id": 2, "sub": sub, "max": 64}),
        )
        .await;
        // Give the first pull time to reach the server and park.
        tokio::time::sleep(Duration::from_millis(50)).await;

        send_json(
            &mut peer,
            &serde_json::json!({"op": "pull", "req_id": 3, "sub": sub, "max": 64}),
        )
        .await;
        let refused = next_frame(&mut peer).await;
        let Message::Text(text) = refused else {
            panic!("expected a text frame, got {refused:?}");
        };
        let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed["op"].as_str().unwrap(), "refused");
        assert_eq!(parsed["req_id"].as_u64().unwrap(), 3);
        assert_eq!(
            parsed["reason"].as_str().unwrap(),
            "pull_in_flight",
            "a second pull on the same subscription must be refused, got {text}",
        );
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn an_ack_frees_a_parked_spent_pull_on_the_same_connection() {
        let bus = Bus::start(Config {
            limits: Limits {
                in_flight: NonZeroUsize::new(2).unwrap(),
                ..Limits::default()
            },
            ..config()
        })
        .await;
        let mut peer = connect(&bus.path).await;

        send_json(
            &mut peer,
            &serde_json::json!({
                "op": "subscribe",
                "req_id": 1,
                "topic": "global",
                "subscriber": SubscriberId::new().to_string(),
            }),
        )
        .await;
        let subscribed = next_frame(&mut peer).await;
        let Message::Text(text) = subscribed else {
            panic!("expected a text frame, got {subscribed:?}");
        };
        let subscribed: serde_json::Value = serde_json::from_str(&text).unwrap();
        let sub = subscribed["sub"].as_u64().unwrap();

        // Publish enough events to fill the in-flight credit and leave the reader parked spent.
        for turn in 0..2 {
            send_json(
                &mut peer,
                &serde_json::json!({
                    "op": "publish",
                    "req_id": 10 + turn,
                    "topic": "global",
                    "payload": turn,
                }),
            )
            .await;
            let published = next_frame(&mut peer).await;
            let Message::Text(text) = published else {
                panic!("expected a text frame, got {published:?}");
            };
            assert!(
                text.contains(r#""op":"published""#),
                "expected published, got {text}"
            );
        }

        send_json(
            &mut peer,
            &serde_json::json!({"op": "pull", "req_id": 2, "sub": sub, "max": 64}),
        )
        .await;
        let batch = next_frame(&mut peer).await;
        let Message::Text(text) = batch else {
            panic!("expected a text frame, got {batch:?}");
        };
        let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed["op"].as_str().unwrap(), "batch");
        assert_eq!(parsed["events"].as_array().unwrap().len(), 2);
        let through = parsed["window"]["next"].as_u64().unwrap();

        // This pull parks spent because every event is already delivered and un-acked.
        send_json(
            &mut peer,
            &serde_json::json!({"op": "pull", "req_id": 3, "sub": sub, "max": 64}),
        )
        .await;
        tokio::time::sleep(Duration::from_millis(50)).await;

        // The ack frees the parked pull, but there are still no events, so it re-parks caught-up.
        send_json(
            &mut peer,
            &serde_json::json!({
                "op": "ack",
                "req_id": 4,
                "sub": sub,
                "through": {
                    "epoch": parsed["window"]["epoch"].as_str().unwrap(),
                    "seq": through,
                },
            }),
        )
        .await;

        let acked = next_frame(&mut peer).await;
        let Message::Text(text) = acked else {
            panic!("expected a text frame, got {acked:?}");
        };
        let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed["op"].as_str().unwrap(), "acked");
        assert_eq!(parsed["req_id"].as_u64().unwrap(), 4);

        // A new event wakes the re-parked pull and answers it under its original request id.
        send_json(
            &mut peer,
            &serde_json::json!({
                "op": "publish",
                "req_id": 5,
                "topic": "global",
                "payload": 2,
            }),
        )
        .await;
        let published = next_frame(&mut peer).await;
        let Message::Text(text) = published else {
            panic!("expected a text frame, got {published:?}");
        };
        assert!(
            text.contains(r#""op":"published""#),
            "expected published, got {text}"
        );

        let freed = next_frame(&mut peer).await;
        let Message::Text(text) = freed else {
            panic!("expected a text frame, got {freed:?}");
        };
        let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed["op"].as_str().unwrap(), "batch");
        assert_eq!(parsed["req_id"].as_u64().unwrap(), 3);
        assert_eq!(parsed["events"].as_array().unwrap().len(), 1);
    }
}

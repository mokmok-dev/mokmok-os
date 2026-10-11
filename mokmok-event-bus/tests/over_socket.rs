use mokmok_event_bus::{
    Batch, Config, Error, Limits, Payload, Position, Publisher, Refusal, Retention, ServeError,
    Subscriber, Timeouts, Topic, serve,
};
use mokmok_protocol::AgentID;
use serde::Serialize;
use std::num::{NonZeroU32, NonZeroUsize};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use uuid::Uuid;

// The `cfg(test)` module is what tells the `allow-unwrap-in-tests`, `allow-expect-in-tests`, and
// `allow-panic-in-tests` settings in `.clippy.toml` that this harness is test code.
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

    fn socket_path() -> PathBuf {
        let name = format!(
            "mokmok-bus-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        );

        std::env::temp_dir().join(name)
    }

    fn config() -> Config {
        Config {
            path: socket_path(),
            ..Config::default()
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

    async fn connect(path: &std::path::Path) -> tokio_tungstenite::WebSocketStream<UnixStream> {
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

    async fn name_topic(peer: &mut tokio_tungstenite::WebSocketStream<UnixStream>) {
        within(
            "open",
            peer.send(Message::Text(r#"{"op":"open","topic":"global"}"#.into())),
        )
        .await
        .unwrap();
        let opened = next_frame(peer).await;

        let Message::Text(text) = opened else {
            panic!("expected the opened frame, got {opened:?}");
        };
        assert!(
            text.contains(r#""op":"opened""#),
            "expected an opened answer, got {text}",
        );
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
        stop: Option<oneshot::Sender<()>>,
        serving: Option<JoinHandle<Result<(), ServeError>>>,
    }

    impl Bus {
        async fn start(config: Config) -> Self {
            let path = config.path.clone();
            let (stop, stopped) = oneshot::channel();
            let serving = tokio::spawn(serve(config, async move {
                let _ = stopped.await;
            }));
            let bus = Self {
                path,
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

            let _ = std::fs::remove_file(&self.path);
        }
    }

    #[tokio::test]
    async fn publishing_reaches_a_subscriber_that_started_first() {
        let bus = Bus::start(config()).await;
        let topic = agent_topic();
        let mut subscriber = within(
            "subscribe",
            Subscriber::open(&bus.path, topic, Position::Beginning),
        )
        .await
        .unwrap();
        let mut publisher = within("open", Publisher::open(&bus.path, topic))
            .await
            .unwrap();

        let receipt = within(
            "publish",
            publisher.publish(payload(&serde_json::json!({"turn": 7}))),
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
        let mut first = within(
            "first subscribe",
            Subscriber::open(&bus.path, Topic::Global, Position::Beginning),
        )
        .await
        .unwrap();
        let mut second = within(
            "second subscribe",
            Subscriber::open(&bus.path, Topic::Global, Position::Beginning),
        )
        .await
        .unwrap();
        let mut publisher = within("open", Publisher::open(&bus.path, Topic::Global))
            .await
            .unwrap();

        for turn in 1..=3 {
            within(
                "publish",
                publisher.publish(payload(&serde_json::json!({"turn": turn}))),
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
    async fn a_subscriber_that_reconnects_resumes_from_its_position() {
        let bus = Bus::start(config()).await;
        let mut publisher = within("open", Publisher::open(&bus.path, Topic::Global))
            .await
            .unwrap();
        let mut subscriber = within(
            "subscribe",
            Subscriber::open(&bus.path, Topic::Global, Position::Beginning),
        )
        .await
        .unwrap();

        within("first publish", publisher.publish(payload(&"first")))
            .await
            .unwrap();
        within("second publish", publisher.publish(payload(&"second")))
            .await
            .unwrap();

        let first = within("first next", subscriber.next()).await.unwrap();
        assert_eq!(payloads(&first), [r#""first""#, r#""second""#]);

        let saved = subscriber.position();
        drop(subscriber);

        within("third publish", publisher.publish(payload(&"third")))
            .await
            .unwrap();

        let mut resumed = within("resume", Subscriber::open(&bus.path, Topic::Global, saved))
            .await
            .unwrap();
        let third = within("resumed next", resumed.next()).await.unwrap();

        assert_eq!(payloads(&third), [r#""third""#]);
        assert_eq!(seqs(&third), [2]);
    }

    #[tokio::test]
    async fn a_subscriber_that_fell_behind_learns_what_it_missed() {
        let bus = Bus::start(Config {
            path: socket_path(),
            retention: Retention {
                events: NonZeroUsize::new(2).unwrap(),
                ..Retention::default()
            },
            ..Config::default()
        })
        .await;
        let mut publisher = within("open", Publisher::open(&bus.path, Topic::Global))
            .await
            .unwrap();
        let mut subscriber = within(
            "subscribe",
            Subscriber::open(&bus.path, Topic::Global, Position::Beginning),
        )
        .await
        .unwrap();

        within("first publish", publisher.publish(payload(&0)))
            .await
            .unwrap();
        within("second publish", publisher.publish(payload(&1)))
            .await
            .unwrap();
        let first = within("first next", subscriber.next()).await.unwrap();
        assert_eq!(seqs(&first), [0, 1]);

        for turn in 2..6 {
            within("publish", publisher.publish(payload(&turn)))
                .await
                .unwrap();
        }

        let behind = within("behind next", subscriber.next()).await.unwrap();

        let lapsed = behind.lapsed().unwrap();
        assert_eq!(lapsed.held().seq().get(), 2);
        assert_eq!(lapsed.from().get(), 4);
        assert_eq!(lapsed.missed(), 2);
        assert_eq!(seqs(&behind), [4, 5]);
    }

    #[tokio::test]
    async fn a_cursor_from_a_previous_epoch_is_unresumable() {
        let config = config();
        let path = config.path.clone();
        let mut first = Bus::start(config).await;
        let mut publisher = within("open", Publisher::open(&path, Topic::Global))
            .await
            .unwrap();
        let mut subscriber = within(
            "subscribe",
            Subscriber::open(&path, Topic::Global, Position::Beginning),
        )
        .await
        .unwrap();

        within("publish", publisher.publish(payload(&1)))
            .await
            .unwrap();
        let batch = within("next", subscriber.next()).await.unwrap();
        assert_eq!(seqs(&batch), [0]);
        let saved = subscriber.position();
        drop(subscriber);
        drop(publisher);

        first.shutdown().await.unwrap();
        assert!(!path.exists());

        let _second = Bus::start(Config {
            path: path.clone(),
            ..Config::default()
        })
        .await;
        let mut resumed = within("resume", Subscriber::open(&path, Topic::Global, saved))
            .await
            .unwrap();

        let refusal = within("refusal", resumed.next()).await.unwrap_err();

        assert!(
            matches!(refusal, Error::Refused(Refusal::Unresumable { .. })),
            "expected an unresumable cursor, got {refusal:?}",
        );
    }

    #[tokio::test]
    async fn the_bus_refuses_a_connection_past_its_ceiling() {
        let bus = Bus::start(Config {
            path: socket_path(),
            limits: Limits {
                connections: NonZeroUsize::new(1).unwrap(),
                ..Limits::default()
            },
            ..Config::default()
        })
        .await;
        let _busy = within("busy open", Publisher::open(&bus.path, Topic::Global))
            .await
            .unwrap();

        let second = within("second open", Publisher::open(&bus.path, Topic::Global)).await;

        assert!(
            matches!(second, Err(Error::Refused(Refusal::ConnectionLimit))),
            "expected a refusal at the ceiling, got {:?}",
            second.err(),
        );
        assert!(
            within("third open", Publisher::open(&bus.path, Topic::Global))
                .await
                .is_err(),
        );
    }

    #[tokio::test]
    async fn an_abandoned_parked_subscriber_frees_its_connection() {
        let bus = Bus::start(Config {
            path: socket_path(),
            limits: Limits {
                connections: NonZeroUsize::new(1).unwrap(),
                ..Limits::default()
            },
            ..Config::default()
        })
        .await;

        let mut subscriber = within(
            "subscribe",
            Subscriber::open(&bus.path, Topic::Global, Position::Beginning),
        )
        .await
        .unwrap();
        // Nothing has been published, so this pull parks the session; the timeout only bounds
        // how long the test waits before abandoning the client.
        let _ = tokio::time::timeout(Duration::from_millis(150), subscriber.next()).await;
        drop(subscriber);

        let mut accepted = None;
        for _ in 0..100 {
            if let Ok(publisher) =
                within("retry open", Publisher::open(&bus.path, Topic::Global)).await
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
        let mut subscriber = within(
            "subscribe",
            Subscriber::open(&bus.path, Topic::Global, Position::Beginning),
        )
        .await
        .unwrap();

        let parked = tokio::spawn(async move { subscriber.next().await });
        // The pull has to reach the server and park before the publish, or the subscriber reads
        // a non-empty batch and the wake path never runs.
        tokio::time::sleep(Duration::from_millis(200)).await;

        let mut publisher = within("open", Publisher::open(&bus.path, Topic::Global))
            .await
            .unwrap();
        let receipt = within(
            "publish",
            publisher.publish(payload(&serde_json::json!({"turn": 9}))),
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
    async fn a_frame_the_bus_cannot_answer_closes_the_session() {
        let bus = Bus::start(config()).await;
        let mut peer = connect(&bus.path).await;
        name_topic(&mut peer).await;

        // A second open is a valid frame the opened session cannot answer, so it reaches the
        // handler that closes the session rather than the frame decoder.
        within(
            "out of turn",
            peer.send(Message::Text(r#"{"op":"open","topic":"global"}"#.into())),
        )
        .await
        .unwrap();

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
            path: socket_path(),
            limits: Limits {
                connections: NonZeroUsize::new(1).unwrap(),
                ..Limits::default()
            },
            ..Config::default()
        })
        .await;

        let mut peer = connect(&bus.path).await;
        name_topic(&mut peer).await;

        within("close", peer.send(Message::Close(None)))
            .await
            .unwrap();
        drop(peer);

        let mut accepted = None;
        for _ in 0..100 {
            if let Ok(publisher) =
                within("retry open", Publisher::open(&bus.path, Topic::Global)).await
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
            path: socket_path(),
            timeouts: Timeouts {
                heartbeat: NonZeroU32::new(20).unwrap(),
                deadline: NonZeroU32::new(300).unwrap(),
            },
            ..Config::default()
        })
        .await;
        let mut peer = connect(&bus.path).await;
        name_topic(&mut peer).await;

        // Read past the deadline so tungstenite answers every ping; a peer that answers has to
        // stay connected. The pause keeps an answer in flight across a heartbeat tick, which is
        // what separates answering from not answering.
        let until = tokio::time::Instant::now() + Duration::from_millis(1000);
        while tokio::time::Instant::now() < until {
            tokio::time::sleep(Duration::from_millis(100)).await;
            let _ = tokio::time::timeout(Duration::from_millis(50), peer.next()).await;
            let _ = tokio::time::timeout(Duration::from_millis(50), peer.next()).await;
        }

        within(
            "publish",
            peer.send(Message::Text(r#"{"op":"publish","payload":1}"#.into())),
        )
        .await
        .unwrap();
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

        let _first = within("first open", Publisher::open(&bus.path, Topic::Global))
            .await
            .unwrap();
        let _second = within("second open", Publisher::open(&bus.path, Topic::Global))
            .await
            .unwrap();

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
            path: socket_path(),
            limits: Limits {
                frame_bytes: NonZeroUsize::new(64).unwrap(),
                ..Limits::default()
            },
            ..Config::default()
        })
        .await;
        let mut publisher = within("open", Publisher::open(&bus.path, Topic::Global))
            .await
            .unwrap();
        let oversized = "x".repeat(4096);

        let refused = within("publish", publisher.publish(payload(&oversized))).await;

        // Tungstenite refuses the frame while assembling it and closes the connection, so the
        // caller gets no answer; what matters is that the bus is still there for everyone else.
        assert!(
            matches!(refused, Err(Error::Indeterminate)),
            "expected the over-limit frame to be refused, got {refused:?}",
        );

        let mut fresh = within("fresh open", Publisher::open(&bus.path, Topic::Global))
            .await
            .unwrap();
        let mut subscriber = within(
            "subscribe",
            Subscriber::open(&bus.path, Topic::Global, Position::Beginning),
        )
        .await
        .unwrap();
        within("small publish", fresh.publish(payload(&"small")))
            .await
            .unwrap();

        assert_eq!(
            payloads(&within("next", subscriber.next()).await.unwrap()),
            [r#""small""#],
        );
    }

    #[tokio::test]
    async fn a_cursor_from_a_reclaimed_topic_is_unresumable() {
        let bus = Bus::start(Config {
            path: socket_path(),
            limits: Limits {
                topics: NonZeroUsize::new(1).unwrap(),
                ..Limits::default()
            },
            retention: Retention {
                idle: Duration::ZERO,
                ..Retention::default()
            },
            ..Config::default()
        })
        .await;

        let mut publisher = within("open", Publisher::open(&bus.path, Topic::Global))
            .await
            .unwrap();
        within("publish", publisher.publish(payload(&1)))
            .await
            .unwrap();
        let mut subscriber = within(
            "subscribe",
            Subscriber::open(&bus.path, Topic::Global, Position::Beginning),
        )
        .await
        .unwrap();
        assert_eq!(seqs(&within("next", subscriber.next()).await.unwrap()), [0]);
        let saved = subscriber.position();
        drop(subscriber);
        drop(publisher);

        // The sweep only reclaims a topic nobody watches, so wait for the session to let go.
        let mut second = None;
        for _ in 0..100 {
            if let Ok(opened) =
                within("second topic", Publisher::open(&bus.path, agent_topic())).await
            {
                second = Some(opened);
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let mut second = second.expect("the second topic was never accepted");
        within("second publish", second.publish(payload(&2)))
            .await
            .unwrap();
        drop(second);

        let mut resumed = within("resume", Subscriber::open(&bus.path, Topic::Global, saved))
            .await
            .unwrap();
        // The reopened log restarts its sequences at zero, so the stale cursor lands inside it.
        let mut again = within("again open", Publisher::open(&bus.path, Topic::Global))
            .await
            .unwrap();
        within("again publish", again.publish(payload(&3)))
            .await
            .unwrap();
        within("again second publish", again.publish(payload(&4)))
            .await
            .unwrap();

        let answered = within("resumed next", resumed.next()).await;

        assert!(
            matches!(answered, Err(Error::Refused(Refusal::Unresumable { .. }))),
            "expected an unresumable cursor from the reclaimed topic, got {answered:?}",
        );
    }

    #[tokio::test]
    async fn a_peer_that_stops_reading_is_closed() {
        let bus = Bus::start(Config {
            path: socket_path(),
            timeouts: Timeouts {
                heartbeat: NonZeroU32::new(20).unwrap(),
                deadline: NonZeroU32::new(150).unwrap(),
            },
            ..Config::default()
        })
        .await;
        let mut publisher = within("open", Publisher::open(&bus.path, Topic::Global))
            .await
            .unwrap();

        // Nothing reads this client's socket, so it never answers a ping; the bus hangs up long
        // before the sleep is over.
        tokio::time::sleep(Duration::from_millis(1000)).await;

        assert!(
            within("publish", publisher.publish(payload(&1)))
                .await
                .is_err()
        );
    }
}

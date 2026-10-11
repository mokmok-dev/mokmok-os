//! Every test here runs against a real `serve` on a real Unix socket, spoken to by the crate's
//! own WebSocket client.

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

    static NEXT: AtomicUsize = AtomicUsize::new(0);

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

        /// Resolves once `serve` has returned.
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
        let mut subscriber = Subscriber::open(&bus.path, topic, Position::Beginning)
            .await
            .unwrap();
        let mut publisher = Publisher::open(&bus.path, topic).await.unwrap();

        let receipt = publisher
            .publish(payload(&serde_json::json!({"turn": 7})))
            .await
            .unwrap();
        let batch = subscriber.next().await.unwrap();

        assert_eq!(batch.events().len(), 1);
        assert_eq!(batch.events()[0].id(), receipt.id());
        assert_eq!(batch.events()[0].topic(), topic);
        assert_eq!(batch.events()[0].payload().as_str(), r#"{"turn":7}"#);
        assert_eq!(receipt.seq().get(), 0);
    }

    #[tokio::test]
    async fn two_subscribers_each_receive_every_event() {
        let bus = Bus::start(config()).await;
        let mut first = Subscriber::open(&bus.path, Topic::Global, Position::Beginning)
            .await
            .unwrap();
        let mut second = Subscriber::open(&bus.path, Topic::Global, Position::Beginning)
            .await
            .unwrap();
        let mut publisher = Publisher::open(&bus.path, Topic::Global).await.unwrap();

        for turn in 1..=3 {
            publisher
                .publish(payload(&serde_json::json!({"turn": turn})))
                .await
                .unwrap();
        }

        let first_batch = first.next().await.unwrap();
        let second_batch = second.next().await.unwrap();

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
        let mut publisher = Publisher::open(&bus.path, Topic::Global).await.unwrap();
        let mut subscriber = Subscriber::open(&bus.path, Topic::Global, Position::Beginning)
            .await
            .unwrap();

        publisher.publish(payload(&"first")).await.unwrap();
        publisher.publish(payload(&"second")).await.unwrap();

        let first = subscriber.next().await.unwrap();
        assert_eq!(payloads(&first), [r#""first""#, r#""second""#]);

        let saved = subscriber.position();
        drop(subscriber);

        publisher.publish(payload(&"third")).await.unwrap();

        let mut resumed = Subscriber::open(&bus.path, Topic::Global, saved)
            .await
            .unwrap();
        let third = resumed.next().await.unwrap();

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
        let mut publisher = Publisher::open(&bus.path, Topic::Global).await.unwrap();
        let mut subscriber = Subscriber::open(&bus.path, Topic::Global, Position::Beginning)
            .await
            .unwrap();

        publisher.publish(payload(&0)).await.unwrap();
        publisher.publish(payload(&1)).await.unwrap();
        let first = subscriber.next().await.unwrap();
        assert_eq!(seqs(&first), [0, 1]);

        for turn in 2..6 {
            publisher.publish(payload(&turn)).await.unwrap();
        }

        let behind = subscriber.next().await.unwrap();

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
        let mut publisher = Publisher::open(&path, Topic::Global).await.unwrap();
        let mut subscriber = Subscriber::open(&path, Topic::Global, Position::Beginning)
            .await
            .unwrap();

        publisher.publish(payload(&1)).await.unwrap();
        let batch = subscriber.next().await.unwrap();
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
        let mut resumed = Subscriber::open(&path, Topic::Global, saved).await.unwrap();

        let refusal = resumed.next().await.unwrap_err();

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
        let _busy = Publisher::open(&bus.path, Topic::Global).await.unwrap();

        let second = Publisher::open(&bus.path, Topic::Global).await;

        assert!(
            matches!(second, Err(Error::Refused(Refusal::ConnectionLimit))),
            "expected a refusal at the ceiling, got {:?}",
            second.err(),
        );
        assert!(Publisher::open(&bus.path, Topic::Global).await.is_err());
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

        let mut subscriber = Subscriber::open(&bus.path, Topic::Global, Position::Beginning)
            .await
            .unwrap();
        // Nothing has been published, so this pull parks the session; the timeout only bounds
        // how long the test waits before abandoning the client.
        let _ = tokio::time::timeout(Duration::from_millis(150), subscriber.next()).await;
        drop(subscriber);

        let mut accepted = None;
        for _ in 0..100 {
            if let Ok(publisher) = Publisher::open(&bus.path, Topic::Global).await {
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
        let mut publisher = Publisher::open(&bus.path, Topic::Global).await.unwrap();
        let oversized = "x".repeat(4096);

        let refused = publisher.publish(payload(&oversized)).await;

        // Tungstenite refuses the frame while assembling it and closes the connection, so the
        // caller gets no answer; what matters is that the bus is still there for everyone else.
        assert!(
            matches!(refused, Err(Error::Indeterminate)),
            "expected the over-limit frame to be refused, got {refused:?}",
        );

        let mut fresh = Publisher::open(&bus.path, Topic::Global).await.unwrap();
        let mut subscriber = Subscriber::open(&bus.path, Topic::Global, Position::Beginning)
            .await
            .unwrap();
        fresh.publish(payload(&"small")).await.unwrap();

        assert_eq!(payloads(&subscriber.next().await.unwrap()), [r#""small""#]);
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

        let mut publisher = Publisher::open(&bus.path, Topic::Global).await.unwrap();
        publisher.publish(payload(&1)).await.unwrap();
        let mut subscriber = Subscriber::open(&bus.path, Topic::Global, Position::Beginning)
            .await
            .unwrap();
        assert_eq!(seqs(&subscriber.next().await.unwrap()), [0]);
        let saved = subscriber.position();
        drop(subscriber);
        drop(publisher);

        // The sweep only reclaims a topic nobody watches, so wait for the session to let go.
        let mut second = None;
        for _ in 0..100 {
            if let Ok(opened) = Publisher::open(&bus.path, agent_topic()).await {
                second = Some(opened);
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let mut second = second.expect("the second topic was never accepted");
        second.publish(payload(&2)).await.unwrap();
        drop(second);

        let mut resumed = Subscriber::open(&bus.path, Topic::Global, saved)
            .await
            .unwrap();
        // The reopened log restarts its sequences at zero, so the stale cursor lands inside it.
        let mut again = Publisher::open(&bus.path, Topic::Global).await.unwrap();
        again.publish(payload(&3)).await.unwrap();
        again.publish(payload(&4)).await.unwrap();

        let answered = resumed.next().await;

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
        let mut publisher = Publisher::open(&bus.path, Topic::Global).await.unwrap();

        // Nothing reads this client's socket, so it never answers a ping; the bus hangs up long
        // before the sleep is over.
        tokio::time::sleep(Duration::from_millis(1000)).await;

        assert!(publisher.publish(payload(&1)).await.is_err());
    }
}

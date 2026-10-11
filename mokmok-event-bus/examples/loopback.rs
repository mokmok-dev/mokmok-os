use std::io;
use std::path::Path;
use std::time::Duration;

use mokmok_event_bus::{Config, Payload, Position, Publisher, Subscriber, Topic, serve};
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

#[derive(Debug, Deserialize, Serialize)]
struct Turn {
    turn: u32,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .init();

    let path = std::env::temp_dir().join(format!("mokmok-loopback-{}", std::process::id()));
    let config = Config {
        path: path.clone(),
        ..Config::default()
    };
    let (stop, stopped) = oneshot::channel();
    let serving = tokio::spawn(serve(config, async move {
        let _ = stopped.await;
    }));
    wait_until_bound(&path).await?;
    println!("bus serving on {}", path.display());

    let topic = Topic::Global;
    let mut subscriber = Subscriber::open(&path, topic, Position::Beginning).await?;
    let mut publisher = Publisher::open(&path, topic).await?;

    for turn in 1..=3 {
        let receipt = publisher.publish(Payload::json(&Turn { turn })?).await?;
        println!("published turn {turn} as seq {}", receipt.seq().get());
    }

    let mut seen = Vec::new();
    while seen.len() < 3 {
        let batch = subscriber.next().await?;
        println!(
            "pulled the batch covering seq {}..{}",
            batch.window().first().get(),
            batch.window().next().get(),
        );

        for event in batch.events() {
            let turn = event.payload().decode::<Turn>()?.turn;

            println!("read seq {} carrying turn {turn}", event.seq().get());
            seen.push(turn);
        }
    }
    assert_eq!(seen, [1, 2, 3]);
    println!("read every published turn back, in order");

    let saved = subscriber.position();
    drop(subscriber);

    let receipt = publisher.publish(Payload::json(&Turn { turn: 4 })?).await?;
    println!("published turn 4 as seq {}", receipt.seq().get());

    let mut resumed = Subscriber::open(&path, topic, saved).await?;
    let batch = resumed.next().await?;

    assert_eq!(batch.events().len(), 1);
    assert_eq!(batch.events()[0].payload().decode::<Turn>()?.turn, 4);
    println!("resumed from the saved position and read turn 4 alone");

    let _ = stop.send(());
    serving.await??;
    assert!(!path.exists());
    println!("serve returned and unlinked {}", path.display());

    Ok(())
}

async fn wait_until_bound(path: &Path) -> Result<(), io::Error> {
    for _ in 0..500 {
        if path.exists() {
            return Ok(());
        }

        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        format!("the bus never bound {}", path.display()),
    ))
}

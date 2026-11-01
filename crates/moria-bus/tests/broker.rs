//! Tests contre un vrai broker : `AMQP_TEST_URL=amqp://guest:guest@127.0.0.1:5672
//! cargo test -p moria-bus -- --ignored --test-threads=1`

use futures::StreamExt;
use moria_bus::{Bus, Queue};
use std::time::Duration;
use tokio::time::timeout;

fn url() -> String {
    std::env::var("AMQP_TEST_URL").expect("AMQP_TEST_URL")
}

fn uniq(p: &str) -> String {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{p}.{n}")
}

const S2: Duration = Duration::from_secs(2);

#[tokio::test]
#[ignore]
async fn publish_reaches_subscriber() {
    let bus = Bus::connect(&url()).await.unwrap();
    assert!(bus.is_connected());
    let mut s = bus.subscribe("test.a", Queue::Exclusive).await.unwrap();
    bus.publish("test.a", b"x".to_vec(), false).await.unwrap();
    assert_eq!(timeout(S2, s.next()).await.unwrap().unwrap(), b"x");
}

#[tokio::test]
#[ignore]
async fn latest_only_keeps_one() {
    let bus = Bus::connect(&url()).await.unwrap();
    let key = uniq("test.latest");
    let q = || Queue::Shared {
        name: &key,
        latest_only: true,
    };
    bus.declare(&key, q()).await.unwrap();
    for b in [b"1", b"2", b"3"] {
        bus.publish(&key, b.to_vec(), false).await.unwrap();
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    let mut s = bus.subscribe(&key, q()).await.unwrap();
    assert_eq!(timeout(S2, s.next()).await.unwrap().unwrap(), b"3");
    assert!(timeout(Duration::from_millis(500), s.next()).await.is_err());
}

#[tokio::test]
#[ignore]
async fn durable_publish_survives_late_subscriber() {
    let bus = Bus::connect(&url()).await.unwrap();
    let key = uniq("test.durable");
    bus.declare(&key, Queue::Durable(&key)).await.unwrap();
    bus.publish(&key, b"d".to_vec(), true).await.unwrap();
    let mut s = bus.subscribe(&key, Queue::Durable(&key)).await.unwrap();
    assert_eq!(timeout(S2, s.next()).await.unwrap().unwrap(), b"d");
}

#[tokio::test]
#[ignore]
async fn durable_publish_to_unbound_key_is_an_error() {
    let bus = Bus::connect(&url()).await.unwrap();
    let key = uniq("test.unbound");
    assert!(bus.publish(&key, b"d".to_vec(), true).await.is_err());
}

#[tokio::test]
#[ignore]
async fn stream_ends_when_connection_closes() {
    let bus = Bus::connect(&url()).await.unwrap();
    let mut s = bus.subscribe("test.end", Queue::Exclusive).await.unwrap();
    bus.close().await;
    assert!(!bus.is_connected());
    assert!(
        timeout(Duration::from_secs(5), s.next())
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
#[ignore]
async fn unacked_delivery_is_redelivered_after_close() {
    let key = uniq("test.unacked");
    let bus = Bus::connect(&url()).await.unwrap();
    bus.declare(&key, Queue::Durable(&key)).await.unwrap();
    bus.publish(&key, b"keep".to_vec(), true).await.unwrap();
    let mut s = bus
        .subscribe_acked(&key, Queue::Durable(&key))
        .await
        .unwrap();
    let d = timeout(S2, s.next()).await.unwrap().unwrap();
    assert_eq!(d.body, b"keep");
    bus.close().await;
    let bus2 = Bus::connect(&url()).await.unwrap();
    let mut s2 = bus2
        .subscribe_acked(&key, Queue::Durable(&key))
        .await
        .unwrap();
    let d = timeout(S2, s2.next()).await.unwrap().unwrap();
    assert_eq!(d.body, b"keep");
    d.ack().await.unwrap();
}

#[tokio::test]
#[ignore]
async fn acked_delivery_is_not_redelivered() {
    let key = uniq("test.acked");
    let bus = Bus::connect(&url()).await.unwrap();
    bus.declare(&key, Queue::Durable(&key)).await.unwrap();
    bus.publish(&key, b"once".to_vec(), true).await.unwrap();
    let mut s = bus
        .subscribe_acked(&key, Queue::Durable(&key))
        .await
        .unwrap();
    let d = timeout(S2, s.next()).await.unwrap().unwrap();
    d.ack().await.unwrap();
    bus.close().await;
    let bus2 = Bus::connect(&url()).await.unwrap();
    let mut s2 = bus2
        .subscribe_acked(&key, Queue::Durable(&key))
        .await
        .unwrap();
    assert!(
        timeout(Duration::from_millis(700), s2.next())
            .await
            .is_err()
    );
}

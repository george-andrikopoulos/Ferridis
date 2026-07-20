//! End-to-end tests for the `BrokerRegistration` heartbeat loop against a
//! counting stub broker: ephemeral registrations re-register on the
//! configured interval, pinned ones don't, and dropping the handle stops
//! the heartbeat (abort-on-drop).

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use ferridis_adapter_sdk::broker::{
    AdapterUrl, BrokerConfig, BrokerRegistration, BrokerUrl, RegistrationPersistence, ServiceKind,
    ServiceName,
};

type Registrations = Arc<AtomicUsize>;

async fn spawn_counting_broker() -> (SocketAddr, Registrations) {
    let count: Registrations = Arc::new(AtomicUsize::new(0));
    let router = Router::new()
        .route(
            "/discovery/register",
            post(
                |State(count): State<Registrations>, Json(_body): Json<serde_json::Value>| async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    StatusCode::OK
                },
            ),
        )
        .with_state(Arc::clone(&count));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind stub broker");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        axum::serve(listener, router)
            .await
            .expect("serve stub broker");
    });
    (addr, count)
}

fn config(addr: SocketAddr, persistence: RegistrationPersistence) -> BrokerConfig {
    BrokerConfig::new(
        BrokerUrl::parse(&format!("http://{addr}")).expect("valid broker url"),
        ServiceName::parse("heartbeat-test").expect("valid name"),
        ServiceKind::Ferridis,
        AdapterUrl::parse("http://127.0.0.1:7821/").expect("valid adapter url"),
        persistence,
    )
}

/// Poll until `count` reaches `target` or the deadline passes.
async fn wait_for_count(count: &Registrations, target: usize, deadline: Duration) {
    let end = tokio::time::Instant::now() + deadline;
    while count.load(Ordering::SeqCst) < target {
        assert!(
            tokio::time::Instant::now() < end,
            "expected at least {target} registrations, saw {} before the deadline",
            count.load(Ordering::SeqCst)
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// An ephemeral registration re-registers on the heartbeat interval —
/// the guarantee that keeps the entry alive across the broker's TTL.
#[tokio::test]
async fn ephemeral_heartbeat_re_registers_within_interval() {
    let (addr, count) = spawn_counting_broker().await;
    let cfg = config(addr, RegistrationPersistence::Ephemeral)
        .with_heartbeat_interval(Duration::from_millis(50));
    let _reg = BrokerRegistration::connect(cfg)
        .await
        .expect("initial registration");

    // 1 initial + at least 3 heartbeats.
    wait_for_count(&count, 4, Duration::from_secs(10)).await;
}

/// Dropping the handle aborts the heartbeat — no further registrations
/// arrive after the drop settles.
#[tokio::test]
async fn dropping_the_handle_stops_the_heartbeat() {
    let (addr, count) = spawn_counting_broker().await;
    let cfg = config(addr, RegistrationPersistence::Ephemeral)
        .with_heartbeat_interval(Duration::from_millis(50));
    let reg = BrokerRegistration::connect(cfg)
        .await
        .expect("initial registration");

    wait_for_count(&count, 3, Duration::from_secs(10)).await;
    drop(reg);

    // One in-flight heartbeat may still land; after that the count must
    // stay frozen across many would-be intervals.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let frozen = count.load(Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(
        count.load(Ordering::SeqCst),
        frozen,
        "heartbeat kept firing after the handle was dropped"
    );
}

/// A pinned registration registers exactly once — the broker persists it,
/// so no heartbeat task is spawned.
#[tokio::test]
async fn pinned_registration_has_no_heartbeat() {
    let (addr, count) = spawn_counting_broker().await;
    let cfg = config(addr, RegistrationPersistence::Pinned)
        .with_heartbeat_interval(Duration::from_millis(50));
    let _reg = BrokerRegistration::connect(cfg)
        .await
        .expect("initial registration");

    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(
        count.load(Ordering::SeqCst),
        1,
        "pinned registration must not heartbeat"
    );
}

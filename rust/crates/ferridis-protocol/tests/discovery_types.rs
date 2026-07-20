//! RED tests for Task 2 — DiscoveredService + ServiceKind (v0.5).

use ferridis_protocol::discovery::{DiscoveredService, ServiceKind};
use url::Url;

#[test]
fn service_kind_mcp_and_ferridis_exist() {
    let _ = ServiceKind::Mcp;
    let _ = ServiceKind::Ferridis;
}

#[test]
fn discovered_service_round_trips_fields() {
    let url = Url::parse("http://192.168.1.42:7821/mcp").unwrap(); // allow:unwrap test-only
    let svc = DiscoveredService::new(
        "my-adapter".to_string(),
        ServiceKind::Mcp,
        url.clone(), // clone: second use of url
    );
    assert_eq!(svc.name(), "my-adapter");
    assert!(matches!(svc.kind(), ServiceKind::Mcp));
    assert_eq!(svc.url(), &url);
}

#[test]
fn discovered_service_is_debug_and_clone() {
    let url = Url::parse("http://localhost:7821/mcp").unwrap(); // allow:unwrap test-only
    let svc = DiscoveredService::new("a".into(), ServiceKind::Ferridis, url);
    let _cloned = svc.clone(); // clone: test-only clone of DiscoveredService
    let _dbg = format!("{svc:?}");
}

use ferridis_protocol::discovery::MdnsScanner;
use std::time::Duration;

#[tokio::test]
async fn mdns_scanner_starts_and_stops() {
    let (scanner, mut rx) = MdnsScanner::start();
    // Let it run briefly — no guarantee of receiving anything on CI
    tokio::time::timeout(Duration::from_millis(200), rx.recv())
        .await
        .ok();
    drop(scanner);
}

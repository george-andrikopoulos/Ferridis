//! RED tests for Tasks 4 & 5 — ServiceStore + axum routes.

use ferridis_discovery_broker::store::{RegisterRequest, ServiceStore};
use std::time::Duration;

#[tokio::test]
async fn register_and_list_service() {
    let store = ServiceStore::new();
    store
        .register(RegisterRequest::new("my-adapter", "mcp", "http://192.168.1.42:7821/mcp"))
        .await
        .unwrap();
    let services = store.list().await;
    assert_eq!(services.len(), 1);
    assert_eq!(services[0].service().name(), "my-adapter");
}

#[tokio::test]
async fn subscribe_receives_event_on_register() {
    let store = ServiceStore::new();
    let mut rx = store.subscribe();
    store
        .register(RegisterRequest::new("ev-adapter", "ferridis", "http://localhost:7824/"))
        .await
        .unwrap();
    let svc = rx.recv().await.unwrap();
    assert_eq!(svc.name(), "ev-adapter");
}

/// Registrations written to a state file survive a simulated process restart.
///
/// Store 1 registers a service → state file is written.
/// Store 2 is created from the same path, calls load_state() → finds the service.
#[tokio::test]
async fn state_file_persists_registrations_across_restarts() {
    let state_path =
        std::env::temp_dir().join("ferridis-discovery-state-persist-test.json");
    let _ = std::fs::remove_file(&state_path); // clean slate

    // "Process 1": register a persistent service — should atomically write the state file.
    {
        let store = ServiceStore::with_state(state_path.clone()); // clone: reused for second store
        store
            .register(
                RegisterRequest::new("persist-svc", "mcp", "http://127.0.0.1:9002/mcp")
                    .persistent(true),
            )
            .await
            .unwrap(); // allow:unwrap test-only
        assert!(state_path.exists(), "state file should be written after register");
    }

    // "Process 2": new store from same path, load state before serving.
    {
        let store2 = ServiceStore::with_state(state_path.clone()); // clone: path reused for cleanup
        let loaded = store2.load_state().await.unwrap(); // allow:unwrap test-only
        assert_eq!(loaded, 1, "should load exactly 1 service from state file");
        let services = store2.list().await;
        assert_eq!(services.len(), 1);
        assert_eq!(services[0].service().name(), "persist-svc");
    }

    let _ = std::fs::remove_file(&state_path); // cleanup
}

use axum_test::TestServer;
use ferridis_discovery_broker::routes::make_router;
use serde_json::json;

#[tokio::test]
async fn post_register_and_get_services() {
    let store = ServiceStore::new();
    let app = make_router(store);
    let server = TestServer::new(app).unwrap();

    let res = server
        .post("/discovery/register")
        .json(&json!({"name":"test-svc","kind":"mcp","url":"http://localhost:9000/"}))
        .await;
    assert_eq!(res.status_code(), 200);

    let list: serde_json::Value = server.get("/discovery/services").await.json();
    assert_eq!(list.as_array().unwrap().len(), 1);
}

// ── Persistent-registration tests (Option C) ──────────────────────────────────

/// A pinned service (persistent=true) ignores TTL — stays visible even after expiry.
#[tokio::test]
async fn persistent_registration_survives_ttl() {
    let store = ServiceStore::new_with_ttl(Duration::ZERO);
    store
        .register(RegisterRequest::new("pinned-svc", "mcp", "http://localhost:9010/mcp").persistent(true))
        .await
        .unwrap();
    let services = store.list().await;
    assert_eq!(services.len(), 1);
    assert_eq!(services[0].service().name(), "pinned-svc");
    assert!(services[0].persistent());
}

/// An ephemeral service (no persistent flag) expires normally when TTL elapses.
#[tokio::test]
async fn ephemeral_registration_expires_after_ttl() {
    let store = ServiceStore::new_with_ttl(Duration::ZERO);
    store
        .register(RegisterRequest::new("ephemeral-svc", "mcp", "http://localhost:9011/mcp"))
        .await
        .unwrap();
    let services = store.list().await;
    assert!(services.is_empty());
}

/// Entries loaded from the state file are always treated as pinned (survive TTL=0).
#[tokio::test]
async fn state_file_entries_are_always_pinned() {
    let state_path = std::env::temp_dir().join("ferridis-pinned-load-test.json");
    let _ = std::fs::remove_file(&state_path);

    {
        let store = ServiceStore::with_state(state_path.clone());
        store
            .register(
                RegisterRequest::new("pinned-load-svc", "mcp", "http://127.0.0.1:9012/mcp")
                    .persistent(true),
            )
            .await
            .unwrap();
    }

    {
        let store = ServiceStore::with_state_and_ttl(state_path.clone(), Duration::ZERO);
        let loaded = store.load_state().await.unwrap();
        assert_eq!(loaded, 1);
        let services = store.list().await;
        assert_eq!(services.len(), 1, "loaded entry should survive TTL=0");
        assert!(services[0].persistent(), "loaded entry should be marked persistent");
    }

    let _ = std::fs::remove_file(&state_path);
}

/// Deleting a service removes it from the store and from the persisted state file.
#[tokio::test]
async fn delete_removes_service_and_persists() {
    let state_path = std::env::temp_dir().join("ferridis-delete-test.json");
    let _ = std::fs::remove_file(&state_path);

    {
        let store = ServiceStore::with_state(state_path.clone());
        store
            .register(
                RegisterRequest::new("del-svc", "mcp", "http://127.0.0.1:9013/mcp")
                    .persistent(true),
            )
            .await
            .unwrap();
        assert_eq!(store.list().await.len(), 1);

        let removed = store.remove("del-svc").await.unwrap();
        assert!(removed, "remove should return true when service existed");
        assert!(store.list().await.is_empty());
    }

    {
        let store2 = ServiceStore::with_state(state_path.clone());
        let loaded = store2.load_state().await.unwrap();
        assert_eq!(loaded, 0, "deleted service must not be in state file");
    }

    let _ = std::fs::remove_file(&state_path);
}

/// HTTP DELETE /discovery/services/{name} returns 204 when service exists.
#[tokio::test]
async fn http_delete_returns_204() {
    let store = ServiceStore::new();
    store
        .register(
            RegisterRequest::new("http-del-svc", "mcp", "http://localhost:9014/mcp")
                .persistent(true),
        )
        .await
        .unwrap();
    let app = make_router(store);
    let server = TestServer::new(app).unwrap();

    let res = server.delete("/discovery/services/http-del-svc").await;
    assert_eq!(res.status_code(), 204);
}

/// HTTP DELETE of an unknown service returns 404.
#[tokio::test]
async fn http_delete_unknown_returns_404() {
    let store = ServiceStore::new();
    let app = make_router(store);
    let server = TestServer::new(app).unwrap();

    let res = server.delete("/discovery/services/nonexistent").await;
    assert_eq!(res.status_code(), 404);
}

/// GET /discovery/services includes a "persistent" boolean field per entry.
#[tokio::test]
async fn http_list_includes_persistent_field() {
    let store = ServiceStore::new();
    store
        .register(
            RegisterRequest::new("listed-svc", "mcp", "http://localhost:9015/mcp")
                .persistent(true),
        )
        .await
        .unwrap();
    let app = make_router(store);
    let server = TestServer::new(app).unwrap();

    let list: serde_json::Value = server.get("/discovery/services").await.json();
    let entry = &list.as_array().unwrap()[0];
    assert!(entry.get("persistent").is_some(), "response must include 'persistent' field");
    assert_eq!(entry["persistent"], true);
}

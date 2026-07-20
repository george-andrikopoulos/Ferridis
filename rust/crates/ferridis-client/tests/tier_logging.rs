//! Tier-used telemetry: every dispatch emits a `tracing::info!` record
//! carrying the capability, intent, and tier. This captures the actual
//! log output through a real dispatch — the Layer-6 guarantee that was
//! previously enforced by nothing.

use std::io::Write;
use std::sync::{Arc, Mutex};

use ferridis_adapter_fs::{FilesystemCapability, Root};
use ferridis_adapter_sdk::AdapterServer;
use ferridis_client::Client;
use ferridis_core::{CapabilityRef, IntentVerb};
use tempfile::TempDir;
use tokio::net::TcpListener;
use tracing::instrument::WithSubscriber as _;
use url::Url;

#[derive(Clone, Default)]
struct CaptureBuf(Arc<Mutex<Vec<u8>>>);

impl CaptureBuf {
    fn contents(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().expect("capture lock")).into_owned()
    }
}

impl Write for CaptureBuf {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("capture lock").extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn dispatch_emits_tier_used_log_record() {
    // Real fs adapter on a real port.
    let dir = TempDir::new().expect("tempdir");
    std::fs::write(dir.path().join("hello.txt"), "logged").expect("seed file");
    let root = Root::new(dir.path()).expect("absolute root");
    let cap = FilesystemCapability::new(root).expect("default manifest parses");
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local addr");
    let router = AdapterServer::new(cap).into_router();
    tokio::spawn(async move {
        axum::serve(listener, router).await.expect("serve adapter");
    });

    let client = Client::ephemeral();
    let cap_ref =
        CapabilityRef::parse("ferridis://public.ferridis.io/ferridis/fs@v1").expect("valid ref");
    let manifest_url = Url::parse(&format!("http://{addr}/manifest.json")).expect("valid url");
    let base_url = Url::parse(&format!("http://{addr}/")).expect("valid url");
    client
        .register(cap_ref.clone(), manifest_url, base_url)
        .await
        .expect("register fs adapter");

    // Capture INFO-level output for the dispatch only.
    let capture = CaptureBuf::default();
    let writer = capture.clone(); // clone: MakeWriter closure needs its own handle to the shared buffer
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_ansi(false)
        .with_writer(move || writer.clone()) // clone: one writer handle per log record, same shared buffer
        .finish();

    let read = IntentVerb::parse("read-file").expect("valid verb");
    let resp = async {
        client
            .dispatch(&cap_ref, read, serde_json::json!({"path": "hello.txt"}))
            .await
    }
    .with_subscriber(subscriber)
    .await
    .expect("dispatch succeeds");
    assert_eq!(resp["content"], "logged");

    // The tier-used record must name the capability, the intent, and
    // the tier that served the call.
    let logs = capture.contents();
    assert!(
        logs.contains("ferridis://public.ferridis.io/ferridis/fs@v1"),
        "log must carry the capability ref:\n{logs}"
    );
    assert!(
        logs.contains("read-file"),
        "log must carry the intent:\n{logs}"
    );
    assert!(
        logs.contains("tier"),
        "log must carry the tier field:\n{logs}"
    );
}

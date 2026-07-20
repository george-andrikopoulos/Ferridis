//! Run the Notion adapter as a standalone HTTP server.
//!
//! ```bash
//! NOTION_INTEGRATION_TOKEN=secret_... cargo run -p ferridis-adapter-notion --example serve
//! ```

use ferridis_adapter_notion::{IntegrationToken, NotionCapability};
use ferridis_adapter_sdk::AdapterServer;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let raw = std::env::var("NOTION_INTEGRATION_TOKEN").unwrap_or_else(|_| {
        eprintln!("NOTION_INTEGRATION_TOKEN must be set");
        std::process::exit(1);
    });
    let token = IntegrationToken::parse(raw).unwrap_or_else(|e| {
        eprintln!("invalid token: {e}");
        std::process::exit(1);
    });
    let cap = NotionCapability::new(token).unwrap_or_else(|e| {
        eprintln!("capability error: {e}");
        std::process::exit(1);
    });

    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(7830);
    let server = AdapterServer::new(cap);
    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{port}"))
        .await
        .unwrap_or_else(|e| {
            eprintln!("bind error: {e}");
            std::process::exit(1);
        });
    tracing::info!(port, "Notion adapter listening");
    axum::serve(listener, server.into_router())
        .await
        .unwrap_or_default();
}

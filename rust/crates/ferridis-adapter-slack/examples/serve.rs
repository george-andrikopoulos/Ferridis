//! Run the Slack adapter as a standalone HTTP server.
//!
//! ```bash
//! SLACK_BOT_TOKEN=xoxb-... cargo run -p ferridis-adapter-slack --example serve
//! ```

use ferridis_adapter_sdk::AdapterServer;
use ferridis_adapter_slack::{BotToken, SlackCapability};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let raw = std::env::var("SLACK_BOT_TOKEN").unwrap_or_else(|_| {
        eprintln!("SLACK_BOT_TOKEN must be set");
        std::process::exit(1);
    });
    let token = BotToken::parse(raw).unwrap_or_else(|e| {
        eprintln!("invalid token: {e}");
        std::process::exit(1);
    });
    let cap = SlackCapability::new(token).unwrap_or_else(|e| {
        eprintln!("capability error: {e}");
        std::process::exit(1);
    });

    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(7829);
    let server = AdapterServer::new(cap);
    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{port}"))
        .await
        .unwrap_or_else(|e| {
            eprintln!("bind error: {e}");
            std::process::exit(1);
        });
    tracing::info!(port, "Slack adapter listening");
    axum::serve(listener, server.into_router())
        .await
        .unwrap_or_default();
}

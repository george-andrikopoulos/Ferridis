//! Run the Google Calendar adapter as a standalone HTTP server.
//!
//! # Usage
//!
//! ```bash
//! # Minimal — access token only (expires after ~1 hour)
//! GOOGLE_CALENDAR_TOKEN=ya29.xxx cargo run -p ferridis-adapter-google-calendar --example serve
//!
//! # With OAuth refresh credentials for automatic token renewal
//! GOOGLE_CALENDAR_TOKEN=ya29.xxx \
//! GOOGLE_OAUTH_REFRESH_TOKEN=1//xxx \
//! GOOGLE_OAUTH_CLIENT_ID=xxx.apps.googleusercontent.com \
//! GOOGLE_OAUTH_CLIENT_SECRET=xxx \
//!   cargo run -p ferridis-adapter-google-calendar --example serve
//! ```

use ferridis_adapter_google_calendar::{
    AccessToken, ClientId, ClientSecret, GoogleCalendarCapability, OAuthCredentials, RefreshToken,
};
use ferridis_adapter_sdk::AdapterServer;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    // -- Access token (required) -------------------------------------------
    let raw_token = std::env::var("GOOGLE_CALENDAR_TOKEN").unwrap_or_else(|_| {
        eprintln!("GOOGLE_CALENDAR_TOKEN must be set");
        std::process::exit(1);
    });
    let token = AccessToken::parse(raw_token).unwrap_or_else(|e| {
        eprintln!("invalid token: {e}");
        std::process::exit(1);
    });

    // -- Capability -----------------------------------------------------------
    let mut cap = GoogleCalendarCapability::new(token).unwrap_or_else(|e| {
        eprintln!("failed to build capability: {e}");
        std::process::exit(1);
    });

    // -- OAuth refresh credentials (optional) --------------------------------
    let refresh_token = std::env::var("GOOGLE_OAUTH_REFRESH_TOKEN").ok();
    let client_id = std::env::var("GOOGLE_OAUTH_CLIENT_ID").ok();
    let client_secret = std::env::var("GOOGLE_OAUTH_CLIENT_SECRET").ok();

    if let (Some(rt), Some(cid), Some(cs)) = (refresh_token, client_id, client_secret) {
        match (
            RefreshToken::parse(rt),
            ClientId::parse(cid),
            ClientSecret::parse(cs),
        ) {
            (Ok(rt), Ok(cid), Ok(cs)) => {
                cap = cap.with_oauth_credentials(OAuthCredentials::new(rt, cid, cs));
                tracing::info!(
                    "OAuth refresh credentials loaded — automatic token renewal enabled"
                );
            }
            (rt, cid, cs) => {
                for e in [rt.err(), cid.err(), cs.err()].into_iter().flatten() {
                    eprintln!("OAuth credential error: {e}");
                }
                std::process::exit(1);
            }
        }
    }

    // -- Broker self-registration (optional) ---------------------------------
    let broker_url = std::env::var("FERRIDIS_BROKER_URL").ok();
    let adapter_url = std::env::var("FERRIDIS_ADAPTER_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:7828".to_string());
    let adapter_name = std::env::var("FERRIDIS_ADAPTER_NAME")
        .unwrap_or_else(|_| "ferridis-google-calendar".to_string());

    // -- Start server ---------------------------------------------------------
    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(7828);

    let server = AdapterServer::new(cap);
    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{port}"))
        .await
        .unwrap_or_else(|e| {
            eprintln!("failed to bind port {port}: {e}");
            std::process::exit(1);
        });

    tracing::info!(port, "Google Calendar adapter listening");

    // Self-register with broker after a short delay to let the server start.
    if let Some(broker) = broker_url {
        let broker = broker.clone(); // allow:clone moved into spawn
        let adapter_url = adapter_url.clone(); // allow:clone moved into spawn
        let adapter_name = adapter_name.clone(); // allow:clone moved into spawn
        tokio::spawn(async move {
            tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
            let body = serde_json::json!({
                "kind": "ferridis",
                "name": adapter_name,
                "url": format!("{adapter_url}/manifest.json"),
            });
            match reqwest::Client::new()
                .post(format!("{broker}/discovery/register"))
                .json(&body)
                .send()
                .await
            {
                Ok(r) if r.status().is_success() => {
                    tracing::info!(%broker, "registered with discovery broker");
                }
                Ok(r) => {
                    tracing::warn!(%broker, status = %r.status(), "broker registration failed")
                }
                Err(e) => tracing::warn!(%broker, error = %e, "broker registration error"),
            }
        });
    }

    axum::serve(listener, server.into_router())
        .await
        .unwrap_or_else(|e| {
            eprintln!("server error: {e}");
            std::process::exit(1);
        });
}

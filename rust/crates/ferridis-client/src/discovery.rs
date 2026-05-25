//! Automatic discovery integration — mDNS and broker push.
//!
//! Two discovery strategies are provided:
//!
//! - [`Client::discover_mdns`] — watches `_mcp._tcp.local` and
//!   `_ferridis._tcp.local` via mDNS, auto-registering every MCP
//!   service that resolves.
//! - [`Client::discover_broker`] — fetches an initial snapshot from a
//!   Ferridis discovery broker and then subscribes to its SSE push
//!   stream, auto-registering new MCP services as they appear.
//!
//! Both methods return a [`DiscoveryHandle`]. Drop the handle to stop
//! the background task.

use url::Url;

use crate::Client;

/// Drop-to-stop handle for a background discovery task.
///
/// While this handle is alive the associated background task continues
/// scanning or subscribing. Dropping the handle sends a cancellation
/// signal; the task exits cleanly on its next iteration.
pub struct DiscoveryHandle {
    _stop: tokio::sync::oneshot::Sender<()>,
}

impl DiscoveryHandle {
    fn new(stop: tokio::sync::oneshot::Sender<()>) -> Self {
        Self { _stop: stop }
    }
}

impl Client {
    /// Start mDNS discovery.
    ///
    /// Scans `_mcp._tcp.local` and `_ferridis._tcp.local`. Every MCP
    /// service that resolves is auto-registered via
    /// [`Client::register_mcp_sse`].
    ///
    /// Drop the returned [`DiscoveryHandle`] to stop the background task.
    pub fn discover_mdns(&self) -> DiscoveryHandle {
        let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel::<()>();
        let (scanner, mut rx) = ferridis_protocol::MdnsScanner::start();
        let client = self.clone(); // clone: client shared into background task

        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut stop_rx => break,
                    maybe = rx.recv() => {
                        let Some(svc) = maybe else { break };
                        if matches!(svc.kind(), ferridis_protocol::ServiceKind::Mcp) {
                            let url = svc.url().clone(); // clone: moved into register call
                            let _ = client.register_mcp_sse(url).await;
                        }
                    }
                }
            }
            drop(scanner);
        });

        DiscoveryHandle::new(stop_tx)
    }

    /// Fetch the broker's current service snapshot, register all MCP
    /// services, then subscribe to the broker's SSE event stream to
    /// auto-register new arrivals.
    ///
    /// Errors from the broker (unreachable, bad JSON, SSE connection
    /// failure) are silently swallowed — the handle is always returned
    /// so callers can drop it without branching on connectivity.
    ///
    /// Drop the returned [`DiscoveryHandle`] to stop the background
    /// subscription task.
    pub async fn discover_broker(&self, broker_url: Url) -> DiscoveryHandle {
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();

        // --- Initial snapshot (synchronous) ------------------------------------
        // Fetch and register the broker's current service list before spawning
        // the background task. This guarantees that when this function returns,
        // all snapshot entries are already in the client registry and callers
        // (e.g. ferridis-mcp-server) can build their tool catalogue immediately.
        // Errors are silently swallowed — the broker may simply be unreachable.
        if let Ok(services_url) = broker_url.join("/discovery/services")
            && let Ok(resp) = reqwest::get(services_url).await
            && let Ok(list) = resp.json::<Vec<serde_json::Value>>().await
        {
            for item in &list {
                try_register_item(self, item).await;
            }
        }

        // --- SSE push stream (background) -------------------------------------
        let client_bg = self.clone(); // clone: moved into SSE background task
        let broker_url_bg = broker_url; // moved into SSE background task
        tokio::spawn(async move {
            let mut stop_rx = stop_rx;
            if let Ok(events_url) = broker_url_bg.join("/discovery/events") {
                let proto_client = ferridis_protocol::Client::new();
                if let Ok(mut stream) =
                    ferridis_protocol::subscribe(&proto_client, &events_url).await
                {
                    use futures_util::StreamExt as _;
                    loop {
                        tokio::select! {
                            _ = &mut stop_rx => break,
                            maybe = stream.next() => {
                                let Some(event) = maybe else { break };
                                if let Ok(event) = event {
                                    try_register_item(&client_bg, &event.data).await;
                                }
                            }
                        }
                    }
                }
            }
        });

        DiscoveryHandle::new(stop_tx)
    }
}

/// Attempt to parse a broker service record and register it.
///
/// Silently discards records that are not `kind = "mcp"` or have an
/// unparseable URL — the broker stream may contain future service kinds
/// the client does not yet understand.
async fn try_register_item(client: &Client, item: &serde_json::Value) {
    let kind = item.get("kind").and_then(|v| v.as_str()).unwrap_or(""); // allow:unwrap not used — using unwrap_or
    let url_str = item.get("url").and_then(|v| v.as_str()).unwrap_or(""); // allow:unwrap not used — using unwrap_or
    if kind == "mcp"
        && let Ok(url) = url_str.parse::<Url>()
    {
        let _ = client.register_mcp_sse(url).await;
    }
}

//! Service discovery types shared between the mDNS scanner and the broker client.

use mdns_sd::{ServiceDaemon, ServiceEvent};
use tokio::sync::mpsc;
use url::Url;

/// The protocol a discovered service speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceKind {
    /// Service announces `_mcp._tcp.local` or registers via broker with `kind = "mcp"`.
    Mcp,
    /// Service announces `_ferridis._tcp.local` or registers via broker with `kind = "ferridis"`.
    Ferridis,
}

/// A service discovered via mDNS or the broker push stream.
#[derive(Debug, Clone)]
pub struct DiscoveredService {
    name: String,
    kind: ServiceKind,
    url: Url,
}

impl DiscoveredService {
    /// Construct a discovered service record.
    pub fn new(name: String, kind: ServiceKind, url: Url) -> Self {
        Self { name, kind, url }
    }

    /// Human-readable name (mDNS instance name or broker-registered name).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Whether the service speaks MCP or native Ferridis.
    pub fn kind(&self) -> ServiceKind {
        self.kind
    }

    /// Base URL of the service endpoint.
    pub fn url(&self) -> &Url {
        &self.url
    }
}

/// Handle to a running mDNS background scanner. Drop to stop scanning.
pub struct MdnsScanner {
    _stop: tokio::sync::oneshot::Sender<()>,
}

impl MdnsScanner {
    /// Start scanning `_mcp._tcp.local` and `_ferridis._tcp.local`.
    ///
    /// Returns the scanner handle (drop to stop) and a receiver for discovered services.
    pub fn start() -> (Self, mpsc::Receiver<DiscoveredService>) {
        let (tx, rx) = mpsc::channel(64);
        let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel::<()>();

        tokio::spawn(async move {
            let daemon = match ServiceDaemon::new() {
                Ok(d) => d,
                Err(_) => return,
            };

            let services = [
                ("_mcp._tcp.local.", ServiceKind::Mcp),
                ("_ferridis._tcp.local.", ServiceKind::Ferridis),
            ];

            let mut receivers = Vec::new();
            for (stype, _) in &services {
                if let Ok(recv) = daemon.browse(stype) {
                    receivers.push(recv);
                }
            }

            loop {
                tokio::select! {
                    _ = &mut stop_rx => break,
                    _ = tokio::time::sleep(std::time::Duration::from_millis(50)) => {
                        for (recv, (_, kind)) in receivers.iter().zip(services.iter()) {
                            while let Ok(event) = recv.try_recv() {
                                if let ServiceEvent::ServiceResolved(info) = event {
                                    let host = info.get_hostname().trim_end_matches('.');
                                    let port = info.get_port();
                                    if let Ok(url) = Url::parse(
                                        &format!("http://{host}:{port}/")
                                    ) {
                                        let svc = DiscoveredService::new(
                                            info.get_fullname().to_string(),
                                            *kind,
                                            url,
                                        );
                                        let _ = tx.send(svc).await;
                                    }
                                }
                            }
                        }
                    }
                }
            }

            let _ = daemon.shutdown();
        });

        (Self { _stop: stop_tx }, rx)
    }
}

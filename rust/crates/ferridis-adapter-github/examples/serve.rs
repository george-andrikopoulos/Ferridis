//! Stand up a `ferridis-adapter-github` instance on a chosen address.
//!
//! ```bash
//! cargo run --release -p ferridis-adapter-github --example serve -- \
//!     --token ghp_yourtoken \
//!     --bind 127.0.0.1:7827 \
//!     [--api-base-url https://github.example.com/api/v3] \
//!     [--discovery-broker http://127.0.0.1:7825] \
//!     [--service-name ferridis-github] \
//!     [--pinned]
//! ```
//!
//! The `--token` flag also reads from `GITHUB_TOKEN` when omitted.

use std::net::SocketAddr;

use ferridis_adapter_github::{GitHubCapability, GitHubToken};
use ferridis_adapter_sdk::{
    AdapterServer,
    broker::{
        AdapterUrl, BrokerConfig, BrokerRegistration, BrokerUrl, RegistrationPersistence,
        ServiceKind, ServiceName,
    },
};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")), // allow:unwrap
        )
        .init();

    let args: Args = match parse_args(std::env::args().skip(1).collect()) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {e}");
            print_help();
            std::process::exit(2);
        }
    };

    let token = match GitHubToken::parse(args.token) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("invalid token: {e}");
            std::process::exit(1);
        }
    };

    let mut cap = match GitHubCapability::new(token) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("failed to build capability: {e}");
            std::process::exit(1);
        }
    };

    if let Some(base_url) = args.api_base_url {
        cap = cap.with_api_base_url(base_url);
    }

    tracing::info!(addr = %args.bind, "ferridis-adapter-github serving");

    let _broker_reg = if let Some(broker_url) = args.discovery_broker {
        let persistence = if args.pinned {
            RegistrationPersistence::Pinned
        } else {
            RegistrationPersistence::Ephemeral
        };
        let cfg = BrokerConfig::new(
            broker_url,
            args.service_name,
            ServiceKind::Ferridis,
            AdapterUrl::from_socket(args.bind),
            persistence,
        );
        match BrokerRegistration::connect(cfg).await {
            Ok(reg) => Some(reg),
            Err(e) => {
                tracing::warn!(error = %e, "broker registration failed — continuing without it");
                None
            }
        }
    } else {
        None
    };

    let server = AdapterServer::new(cap);
    if let Err(e) = server.serve(args.bind).await {
        eprintln!("server error: {e}");
        std::process::exit(1);
    }
}

struct Args {
    token: String,
    bind: SocketAddr,
    api_base_url: Option<String>,
    discovery_broker: Option<BrokerUrl>,
    service_name: ServiceName,
    pinned: bool,
}

fn parse_args(argv: Vec<String>) -> Result<Args, String> {
    let mut token: Option<String> = std::env::var("GITHUB_TOKEN").ok();
    let mut bind: SocketAddr = "127.0.0.1:7827".parse().expect("literal default addr"); // allow:expect
    let mut api_base_url: Option<String> = None;
    let mut discovery_broker: Option<BrokerUrl> = None;
    let mut service_name =
        ServiceName::parse("ferridis-github").expect("literal default name is valid"); // allow:expect
    let mut pinned = false;

    let mut it = argv.into_iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--token" => {
                let v = it.next().ok_or("--token expects a value")?;
                token = Some(v);
            }
            "--bind" => {
                let v = it.next().ok_or("--bind expects host:port")?;
                bind = v.parse().map_err(|e| format!("--bind `{v}`: {e}"))?;
            }
            "--api-base-url" => {
                api_base_url = Some(it.next().ok_or("--api-base-url expects a URL")?);
            }
            "--discovery-broker" => {
                let v = it.next().ok_or("--discovery-broker expects a URL")?;
                discovery_broker = Some(
                    BrokerUrl::parse(&v)
                        .map_err(|e| format!("--discovery-broker `{v}`: {e}"))?,
                );
            }
            "--service-name" => {
                let v = it.next().ok_or("--service-name expects a name")?;
                service_name = ServiceName::parse(&v)
                    .map_err(|e| format!("--service-name `{v}`: {e}"))?;
            }
            "--pinned" => pinned = true,
            "--help" | "-h" => {
                print_help();
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }

    Ok(Args {
        token: token.ok_or("--token or GITHUB_TOKEN is required")?,
        bind,
        api_base_url,
        discovery_broker,
        service_name,
        pinned,
    })
}

fn print_help() {
    eprintln!("usage: serve --token TOKEN [options]");
    eprintln!();
    eprintln!("required (either flag or env var):");
    eprintln!("  --token TOKEN              GitHub personal access token");
    eprintln!("  GITHUB_TOKEN               environment variable alternative");
    eprintln!();
    eprintln!("optional:");
    eprintln!("  --bind ADDR:PORT           listen address (default: 127.0.0.1:7827)");
    eprintln!("  --api-base-url URL         GitHub API base (default: https://api.github.com)");
    eprintln!("                             Set to https://HOSTNAME/api/v3 for GitHub Enterprise");
    eprintln!("  --discovery-broker URL     register with a Ferridis discovery broker");
    eprintln!("  --service-name NAME        broker registration name (default: ferridis-github)");
    eprintln!("  --pinned                   make broker registration persistent (default: ephemeral)");
}

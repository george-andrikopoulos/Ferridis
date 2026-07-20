//! Stand up a `ferridis-adapter-fs` instance on a chosen address.
//!
//! ```bash
//! cargo run --release -p ferridis-adapter-fs --example serve -- \
//!     --root /path/to/exposed/dir \
//!     --bind 127.0.0.1:7821 \
//!     [--discovery-broker http://127.0.0.1:7825] \
//!     [--service-name ferridis-fs] \
//!     [--pinned]
//! ```

use std::net::SocketAddr;
use std::path::PathBuf;

use ferridis_adapter_fs::{FilesystemCapability, Root};
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

    let root = match Root::new(&args.root) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("invalid root `{}`: {e}", args.root.display());
            std::process::exit(1);
        }
    };
    let cap = match FilesystemCapability::new(root) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("failed to build capability: {e}");
            std::process::exit(1);
        }
    };

    tracing::info!(
        root = %args.root.display(),
        addr = %args.bind,
        "ferridis-adapter-fs serving"
    );

    // Optional: register with the discovery broker.
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
    root: PathBuf,
    bind: SocketAddr,
    discovery_broker: Option<BrokerUrl>,
    service_name: ServiceName,
    pinned: bool,
}

fn parse_args(argv: Vec<String>) -> Result<Args, String> {
    let mut root: Option<PathBuf> = None;
    let mut bind: SocketAddr = "127.0.0.1:7821".parse().expect("literal default addr"); // allow:expect
    let mut discovery_broker: Option<BrokerUrl> = None;
    let mut service_name =
        ServiceName::parse("ferridis-fs").expect("literal default name is valid"); // allow:expect
    let mut pinned = false;

    let mut it = argv.into_iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--root" => {
                let v = it.next().ok_or("--root expects a path")?;
                root = Some(PathBuf::from(v));
            }
            "--bind" => {
                let v = it.next().ok_or("--bind expects host:port")?;
                bind = v.parse().map_err(|e| format!("--bind `{v}`: {e}"))?;
            }
            "--discovery-broker" => {
                let v = it.next().ok_or("--discovery-broker expects a URL")?;
                discovery_broker = Some(
                    BrokerUrl::parse(&v).map_err(|e| format!("--discovery-broker `{v}`: {e}"))?,
                );
            }
            "--service-name" => {
                let v = it.next().ok_or("--service-name expects a name")?;
                service_name =
                    ServiceName::parse(&v).map_err(|e| format!("--service-name `{v}`: {e}"))?;
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
        root: root.ok_or("--root is required")?,
        bind,
        discovery_broker,
        service_name,
        pinned,
    })
}

fn print_help() {
    eprintln!("usage: serve --root <dir> [options]");
    eprintln!();
    eprintln!("required:");
    eprintln!("  --root PATH                directory the adapter exposes");
    eprintln!();
    eprintln!("optional:");
    eprintln!("  --bind ADDR:PORT           listen address (default: 127.0.0.1:7821)");
    eprintln!("  --discovery-broker URL     register with a Ferridis discovery broker");
    eprintln!("  --service-name NAME        broker registration name (default: ferridis-fs)");
    eprintln!(
        "  --pinned                   make the broker registration persistent (default: ephemeral)"
    );
}

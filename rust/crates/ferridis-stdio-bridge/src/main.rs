//! Bridge stdio MCP servers to HTTP SSE, with discovery-broker self-registration.
//!
//! Each `GET /sse` connection spawns a fresh instance of the configured
//! stdio MCP server. Requests arrive via `POST /messages?sessionId=<id>`
//! and are forwarded to the child's stdin; responses are streamed back
//! on the SSE channel.
//!
//! ```bash
//! ferridis-stdio-bridge \
//!     --command npx \
//!     --arg @modelcontextprotocol/server-filesystem \
//!     --arg /path/to/dir \
//!     --bind 127.0.0.1:7826 \
//!     [--discovery-broker http://127.0.0.1:7825] \
//!     [--service-name my-mcp-server] \
//!     [--pinned]
//! ```

use std::net::SocketAddr;

use ferridis_adapter_sdk::broker::{
    AdapterUrl, BrokerConfig, BrokerRegistration, BrokerUrl, RegistrationPersistence, ServiceKind,
    ServiceName,
};
use tokio::net::TcpListener;

use ferridis_stdio_bridge::bridge::{self, AppState};
use ferridis_stdio_bridge::types::{Command, SpawnConfig};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args: Args = match parse_args(std::env::args().skip(1).collect()) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {e}");
            eprintln!();
            print_help();
            std::process::exit(2);
        }
    };

    let mut spawn_cfg = SpawnConfig::new(args.command);
    for arg in args.command_args {
        spawn_cfg = spawn_cfg.with_arg(arg);
    }
    for (k, v) in args.env {
        spawn_cfg = spawn_cfg.with_env(k, v);
    }

    tracing::info!(addr = %args.bind, "ferridis-stdio-bridge listening");

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
            ServiceKind::Mcp,
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

    let state = AppState::new(spawn_cfg);
    let router = bridge::build_router(state);

    let listener = match TcpListener::bind(args.bind).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("bind {}: {e}", args.bind);
            std::process::exit(1);
        }
    };

    if let Err(e) = axum::serve(listener, router).await {
        eprintln!("server error: {e}");
        std::process::exit(1);
    }
}

// ── Args ──────────────────────────────────────────────────────────────────

struct Args {
    command: Command,
    command_args: Vec<String>,
    env: Vec<(String, String)>,
    bind: SocketAddr,
    discovery_broker: Option<BrokerUrl>,
    service_name: ServiceName,
    pinned: bool,
}

fn parse_args(argv: Vec<String>) -> Result<Args, String> {
    let mut command: Option<Command> = None;
    let mut command_args: Vec<String> = Vec::new();
    let mut env: Vec<(String, String)> = Vec::new();
    let mut bind: SocketAddr = "127.0.0.1:7826".parse().expect("literal default addr"); // allow:expect
    let mut discovery_broker: Option<BrokerUrl> = None;
    let mut service_name =
        ServiceName::parse("ferridis-stdio-bridge").expect("literal default name is valid"); // allow:expect
    let mut pinned = false;

    let mut it = argv.into_iter();
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--command" => {
                let v = it.next().ok_or("--command expects a value")?;
                command = Some(Command::new(v).map_err(|e| format!("--command: {e}"))?);
            }
            "--arg" => {
                let v = it.next().ok_or("--arg expects a value")?;
                command_args.push(v);
            }
            "--env" => {
                let v = it.next().ok_or("--env expects KEY=VALUE")?;
                let (k, val) = v
                    .split_once('=')
                    .ok_or_else(|| format!("--env `{v}` must be KEY=VALUE"))?;
                env.push((k.to_owned(), val.to_owned()));
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
        command: command.ok_or("--command is required")?,
        command_args,
        env,
        bind,
        discovery_broker,
        service_name,
        pinned,
    })
}

fn print_help() {
    eprintln!("usage: ferridis-stdio-bridge --command <cmd> [options]");
    eprintln!();
    eprintln!("required:");
    eprintln!("  --command CMD             stdio MCP server binary (e.g. npx, node, python)");
    eprintln!();
    eprintln!("optional:");
    eprintln!("  --arg ARG                 argument passed to CMD; repeatable");
    eprintln!("  --env KEY=VALUE           environment variable injected into CMD; repeatable");
    eprintln!("  --bind ADDR:PORT          listen address (default: 127.0.0.1:7826)");
    eprintln!("  --discovery-broker URL    register with a Ferridis discovery broker");
    eprintln!(
        "  --service-name NAME       broker registration name (default: ferridis-stdio-bridge)"
    );
    eprintln!(
        "  --pinned                  make the broker registration persistent (default: ephemeral)"
    );
}

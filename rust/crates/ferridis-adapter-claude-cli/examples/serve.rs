//! Spin up the Claude Code CLI adapter on a local socket.
//!
//! Minimal CLI — flags are positional/keyword pairs to keep the
//! example dependency-free (no `clap`). Repeatable: `--allowed-cwd`.
//!
//! ```sh
//! cargo run --example serve-claude-cli -p ferridis-adapter-claude-cli -- \
//!   --bind 127.0.0.1:7823 \
//!   --allowed-cwd "$HOME/Documents/Claude/Projects/Ferridis" \
//!   --default-cwd "$HOME/Documents/Claude/Projects/Ferridis" \
//!   --default-model sonnet \
//!   [--discovery-broker http://127.0.0.1:7825] \
//!   [--service-name ferridis-claude-cli] \
//!   [--pinned]
//! ```
//!
//! Pair with the AdapterServer's standard routes:
//!
//! - `GET  /manifest.json` — manifest
//! - `GET  /schema`        — OpenAPI body
//! - `POST /intents/submit-prompt`  (Accept: text/event-stream)
//! - `POST /intents/resume-session` (Accept: text/event-stream)

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;

use ferridis_adapter_claude_cli::{
    AllowedRoots, ClaudeCliCapability, ClaudeCliConfig, Model, ModelAllowList,
};
use ferridis_adapter_sdk::{
    AdapterServer,
    broker::{
        AdapterUrl, BrokerConfig, BrokerRegistration, BrokerUrl, RegistrationPersistence,
        ServiceKind, ServiceName,
    },
};

#[tokio::main]
async fn main() -> ExitCode {
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
            print_usage();
            return ExitCode::from(2);
        }
    };

    let mut roots = AllowedRoots::empty();
    for r in &args.allowed_cwds {
        roots = match roots.with_root(r) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("invalid --allowed-cwd `{}`: {e}", r.display());
                return ExitCode::from(2);
            }
        };
    }

    let mut models = ModelAllowList::standard();
    for alias in &args.extra_models {
        models = models.with_alias(alias);
    }

    let mut cfg = ClaudeCliConfig::new()
        .with_allowed_cwds(roots)
        .with_allowed_models(models);

    if let Some(b) = args.claude_binary {
        cfg = cfg.with_binary(b);
    }
    if let Some(c) = args.default_cwd {
        cfg = cfg.with_default_cwd(c);
    }
    if let Some(m) = args.default_model {
        let parsed = match Model::parse(&m, &ModelAllowList::standard().with_alias(&m)) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("invalid --default-model `{m}`: {e}");
                return ExitCode::from(2);
            }
        };
        cfg = cfg.with_default_model(parsed);
    }

    tracing::info!(addr = %args.bind, "ferridis-adapter-claude-cli serving");

    // Optional: register with the discovery broker.
    let _broker_reg = if let Some(broker_url) = args.discovery_broker {
        let persistence = if args.pinned {
            RegistrationPersistence::Pinned
        } else {
            RegistrationPersistence::Ephemeral
        };
        let broker_cfg = BrokerConfig::new(
            broker_url,
            args.service_name,
            ServiceKind::Ferridis,
            AdapterUrl::from_socket(args.bind),
            persistence,
        );
        match BrokerRegistration::connect(broker_cfg).await {
            Ok(reg) => Some(reg),
            Err(e) => {
                tracing::warn!(error = %e, "broker registration failed — continuing without it");
                None
            }
        }
    } else {
        None
    };

    let capability = ClaudeCliCapability::new(cfg);
    let server = AdapterServer::new(capability);

    if let Err(e) = server.serve(args.bind).await {
        eprintln!("serve failed: {e}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

struct Args {
    bind: SocketAddr,
    allowed_cwds: Vec<PathBuf>,
    extra_models: Vec<String>,
    default_cwd: Option<PathBuf>,
    default_model: Option<String>,
    claude_binary: Option<PathBuf>,
    discovery_broker: Option<BrokerUrl>,
    service_name: ServiceName,
    pinned: bool,
}

fn parse_args(argv: Vec<String>) -> Result<Args, String> {
    let mut bind: Option<SocketAddr> = None;
    let mut allowed_cwds = Vec::new();
    let mut extra_models = Vec::new();
    let mut default_cwd: Option<PathBuf> = None;
    let mut default_model: Option<String> = None;
    let mut claude_binary: Option<PathBuf> = None;
    let mut discovery_broker: Option<BrokerUrl> = None;
    let mut service_name =
        ServiceName::parse("ferridis-claude-cli").expect("literal default name is valid"); // allow:expect
    let mut pinned = false;

    let mut it = argv.into_iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--bind" => {
                let s = it.next().ok_or("missing value for --bind")?;
                bind = Some(s.parse().map_err(|e| format!("--bind `{s}`: {e}"))?);
            }
            "--allowed-cwd" => {
                let v = it.next().ok_or("missing value for --allowed-cwd")?;
                allowed_cwds.push(PathBuf::from(v));
            }
            "--default-cwd" => {
                let v = it.next().ok_or("missing value for --default-cwd")?;
                default_cwd = Some(PathBuf::from(v));
            }
            "--default-model" => {
                let v = it.next().ok_or("missing value for --default-model")?;
                default_model = Some(v);
            }
            "--model-alias" => {
                let v = it.next().ok_or("missing value for --model-alias")?;
                extra_models.push(v);
            }
            "--claude-binary" => {
                let v = it.next().ok_or("missing value for --claude-binary")?;
                claude_binary = Some(PathBuf::from(v));
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
            "-h" | "--help" => {
                print_usage();
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }

    Ok(Args {
        bind: bind.ok_or("--bind is required")?,
        allowed_cwds,
        extra_models,
        default_cwd,
        default_model,
        claude_binary,
        discovery_broker,
        service_name,
        pinned,
    })
}

fn print_usage() {
    eprintln!(
        "usage: serve --bind <addr:port> [--allowed-cwd <path> ...] \
[--default-cwd <path>] [--default-model <alias>] [--model-alias <alias> ...] \
[--claude-binary <path>] [--discovery-broker <url>] [--service-name <name>] [--pinned]"
    );
    eprintln!();
    eprintln!("required:");
    eprintln!("  --bind ADDR:PORT          where to listen (e.g. 127.0.0.1:7823)");
    eprintln!();
    eprintln!("optional:");
    eprintln!(
        "  --allowed-cwd PATH        operator-allowed root for client-supplied cwd; repeatable"
    );
    eprintln!("  --default-cwd PATH        cwd to use when the client doesn't supply one");
    eprintln!(
        "  --default-model ALIAS     model alias for clients that don't supply one (sonnet/opus/haiku)"
    );
    eprintln!(
        "  --model-alias ALIAS       extend the model allow-list with a custom alias; repeatable"
    );
    eprintln!("  --claude-binary PATH      override the `claude` binary path");
    eprintln!("  --discovery-broker URL    register with a Ferridis discovery broker");
    eprintln!(
        "  --service-name NAME       broker registration name (default: ferridis-claude-cli)"
    );
    eprintln!(
        "  --pinned                  make the broker registration persistent (default: ephemeral)"
    );
}

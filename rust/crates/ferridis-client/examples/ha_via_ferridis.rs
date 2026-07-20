//! Live demo: read a Home Assistant entity state through Ferridis,
//! where the Home Assistant *is* an MCP server.
//!
//! Flow:
//!
//! ```text
//! this binary
//!   ↓ Client::register_mcp_sse(http://192.0.2.1:8765/sse)
//! ferridis-client (MCP consumer)
//!   ↓ MCP initialize / tools/list / tools/call (over SSE)
//! Home Assistant MCP server
//!   ↓ HA REST API internally
//! HA entity state
//! ```
//!
//! Run:
//!
//! ```bash
//! cargo run --release -p ferridis-client --example ha_via_ferridis -- \
//!     --sse-url http://192.0.2.1:8765/sse \
//!     --entity sensor.office_temperature
//! ```

use ferridis_client::Client;
use ferridis_core::IntentVerb;
use url::Url;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            std::env::var("RUST_LOG")
                .unwrap_or_else(|_| "ferridis_client=info,ferridis_client::mcp=info".into()),
        )
        .try_init()
        .ok();

    let args = parse_args()?;
    eprintln!("→ registering MCP server: {}", args.sse_url);

    let client = Client::ephemeral();
    let capability = client.register_mcp_sse(args.sse_url.clone()).await?;
    eprintln!("→ registered under capability: {capability}");

    // List the projected intents so we can show them.
    {
        let registry = client.registry().lock().await;
        let record = registry.get(&capability).expect("just registered");
        eprintln!(
            "→ MCP server published {} tool(s):",
            record.manifest().intents().len()
        );
        for verb in record.manifest().intents() {
            eprintln!("    - {}", verb.as_str());
        }
    }

    // Find the right intent verb. MCP tool `ha_get_state` projects to
    // Ferridis intent verb `ha-get-state`.
    let intent_str = args.intent.unwrap_or_else(|| "ha-get-state".to_string());
    let intent = IntentVerb::parse(&intent_str)?;
    eprintln!(
        "→ dispatching intent `{intent_str}` with entity_id={}",
        args.entity
    );

    let body = serde_json::json!({ "entity_id": args.entity });
    let result = client.dispatch(&capability, intent, body).await?;

    println!();
    println!("=== Result (via Ferridis routing to MCP) ===");
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

struct Args {
    sse_url: Url,
    entity: String,
    intent: Option<String>,
}

fn parse_args() -> Result<Args, Box<dyn std::error::Error>> {
    let mut sse: Option<String> = None;
    let mut entity: Option<String> = None;
    let mut intent: Option<String> = None;
    let mut argv = std::env::args().skip(1);
    while let Some(a) = argv.next() {
        match a.as_str() {
            "--sse-url" => sse = argv.next(),
            "--entity" => entity = argv.next(),
            "--intent" => intent = argv.next(),
            "--help" | "-h" => {
                eprintln!(
                    "Usage: ha_via_ferridis --sse-url <url> --entity <entity_id> [--intent <verb>]"
                );
                eprintln!("  --intent defaults to ha-get-state");
                std::process::exit(0);
            }
            other => return Err(format!("unknown arg: {other}").into()),
        }
    }
    let sse = sse.ok_or("--sse-url is required")?;
    let entity = entity.ok_or("--entity is required")?;
    Ok(Args {
        sse_url: Url::parse(&sse)?,
        entity,
        intent,
    })
}

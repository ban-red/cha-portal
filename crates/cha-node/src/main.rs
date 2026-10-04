use std::path::PathBuf;

use anyhow::{Context, Result};
use cha_node::{Agent, Identity, enroll, init_tls, inventory, normalize_portal_url};
use clap::Parser;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

/// The Cha Node agent. Run it once with a join token from the portal's Nodes
/// page; afterwards it reconnects with the identity in its state directory.
#[derive(Parser)]
#[command(version)]
struct Args {
    /// The portal's URL, e.g. https://portal.example.
    #[arg(long, env = "CHA_PORTAL_URL")]
    portal_url: Option<String>,
    /// One-time join token from the portal (needed only to enroll).
    #[arg(long, env = "CHA_JOIN_TOKEN", hide_env_values = true)]
    join_token: Option<String>,
    /// This node's name in the portal; defaults to the hostname.
    #[arg(long, env = "CHA_NODE_NAME")]
    name: Option<String>,
    /// Where the node keeps its identity (including its private key).
    #[arg(long, env = "CHA_NODE_STATE", default_value = "/var/lib/cha-node")]
    state_dir: PathBuf,
    /// Print this machine's inventory as JSON and exit.
    #[arg(long)]
    print_inventory: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
    let mut args = Args::parse();
    // Compose passes unset variables through as empty strings.
    for value in [&mut args.portal_url, &mut args.join_token, &mut args.name] {
        if value.as_deref().is_some_and(|v| v.trim().is_empty()) {
            *value = None;
        }
    }
    if args.print_inventory {
        println!("{}", serde_json::to_string_pretty(&inventory::collect())?);
        return Ok(());
    }
    init_tls();

    let identity = match Identity::load(&args.state_dir)? {
        Some(mut identity) => {
            if args.join_token.is_some() {
                info!(node_id = %identity.node_id, "already enrolled; ignoring the join token");
            }
            if let Some(url) = &args.portal_url {
                let url = normalize_portal_url(url)?;
                if url != identity.portal_url {
                    warn!(old = %identity.portal_url, new = %url, "the portal URL changed");
                    identity.portal_url = url;
                    identity.save(&args.state_dir)?;
                }
            }
            identity
        }
        None => {
            let portal_url = args
                .portal_url
                .context("not enrolled yet: pass --portal-url and --join-token")?;
            let token = args
                .join_token
                .context("not enrolled yet: pass --join-token (from the portal's Nodes page)")?;
            let name = args.name.unwrap_or_else(|| inventory::collect().hostname);
            let identity = enroll(&portal_url, &token, &name).await?;
            identity.save(&args.state_dir)?;
            info!(node_id = %identity.node_id, %name, "enrolled");
            identity
        }
    };

    Agent::new(identity)?.run().await
}

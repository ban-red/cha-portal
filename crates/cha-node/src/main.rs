use std::path::PathBuf;

use anyhow::{Context, Result};
use cha_node::docker::{DEFAULT_SOCKET, Docker};
use cha_node::environments::{DockerConfig, DockerRuntime};
use cha_node::{Agent, Identity, doctor, enroll, init_tls, inventory, normalize_portal_url};
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
    /// Check that this machine can run environments, say how to fix what it
    /// can't, and exit (non-zero if something must be fixed).
    #[arg(long)]
    doctor: bool,
    /// The Docker engine's socket; environments need it.
    #[arg(long, env = "CHA_DOCKER_SOCKET", default_value = DEFAULT_SOCKET)]
    docker_socket: String,
    /// The streamer image each environment runs beside its app.
    #[arg(long, env = "CHA_STREAMER_IMAGE", default_value = "cha/streamer:dev")]
    streamer_image: String,
    /// The CDI device that gives containers the GPU.
    #[arg(long, env = "CHA_GPU_DEVICE", default_value = "nvidia.com/gpu=all")]
    gpu_device: String,
    /// The render node streamers composite on (default: the first GPU with an
    /// encoder).
    #[arg(long, env = "CHA_RENDER_NODE")]
    render_node: Option<String>,
    /// Streamers listen on this port and up (two each: HTTP, WebRTC).
    #[arg(long, env = "CHA_PORT_BASE", default_value_t = 47000)]
    port_base: u16,
    #[arg(long, env = "CHA_MAX_ENVIRONMENTS", default_value_t = 16)]
    max_environments: u16,
    /// The host's uinput device: streamers make virtual gamepads with it.
    /// Empty goes without gamepads (e.g. no `uinput` module).
    #[arg(long, env = "CHA_UINPUT", default_value = "/dev/uinput")]
    uinput: String,
    /// The router's public IP, if it forwards the streamers' UDP ports here
    /// (WAN without a mesh or TURN).
    #[arg(long, env = "CHA_PUBLIC_ADDRESS")]
    public_address: Option<String>,
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
    let docker = Docker::new(&args.docker_socket);
    let config = docker_config(&args);
    if args.doctor {
        let identity = Identity::load(&args.state_dir).ok().flatten();
        let ok = doctor::run(&docker, &config, identity.as_ref()).await;
        std::process::exit(if ok { 0 } else { 1 });
    }

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

    let mut agent = Agent::new(identity)?;
    match DockerRuntime::new(docker, config.clone()).await {
        Ok(runtime) => {
            info!(streamer = %config.streamer_image, render_node = %config.render_node, "running environments with Docker");
            agent = agent.with_runtime(runtime);
        }
        Err(err) => warn!(
            "can't run environments ({err:#}); mount {} into the agent's container",
            args.docker_socket
        ),
    }
    agent.run().await
}

/// How environments run here, from the arguments.
fn docker_config(args: &Args) -> DockerConfig {
    let render_node = args.render_node.clone().unwrap_or_else(|| {
        inventory::collect()
            .gpus
            .into_iter()
            .find(|g| !g.encoders.is_empty())
            .and_then(|g| g.render_node)
            .unwrap_or_else(|| "/dev/dri/renderD128".into())
    });
    DockerConfig {
        streamer_image: args.streamer_image.clone(),
        render_node,
        gpu_device: args.gpu_device.clone(),
        uinput: Some(args.uinput.trim().to_string()).filter(|u| !u.is_empty()),
        public_address: args
            .public_address
            .clone()
            .map(|a| a.trim().to_string())
            .filter(|a| !a.is_empty()),
        port_base: args.port_base,
        max_environments: args.max_environments,
    }
}

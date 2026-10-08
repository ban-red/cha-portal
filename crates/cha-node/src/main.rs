use std::path::PathBuf;
#[cfg(feature = "gamestream")]
use std::sync::Arc;

use anyhow::{Context, Result};
use cha_node::claim::{self, ClaimConfig};
use cha_node::docker::{DEFAULT_SOCKET, Docker};
use cha_node::environments::{DockerConfig, DockerRuntime, PublishedImages};
#[cfg(feature = "gamestream")]
use cha_node::gamestream;
use cha_node::moonlight::{self, Moonlight};
use cha_node::storage::{DataRoot, parse_shared_dirs};
use cha_node::{
    Agent, Identity, check_portal_transport, doctor, enroll, hostfiles, init_tls, inventory,
    normalize_portal_url,
};
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
    /// Let the portal URL be plain http:// to another machine (a dev portal on
    /// the LAN). Without it only https://, or http:// to this machine's
    /// loopback (a tunnel), is accepted: over plain HTTP the network can read
    /// the node's traffic and impersonate the portal.
    #[arg(long, env = "CHA_ALLOW_INSECURE_PORTAL")]
    allow_insecure_portal: bool,
    /// One-time join token from the portal (to enroll without being claimed).
    #[arg(long, env = "CHA_JOIN_TOKEN", hide_env_values = true)]
    join_token: Option<String>,
    /// This node's name in the portal; defaults to the hostname.
    #[arg(long, env = "CHA_NODE_NAME")]
    name: Option<String>,
    /// Without an identity or a join token, advertise this node on the LAN and
    /// show a pairing code to claim it with in the portal (Admin → Nodes).
    /// false needs a join token instead.
    #[arg(long, env = "CHA_DISCOVERY", default_value_t = true, action = clap::ArgAction::Set)]
    discovery: bool,
    /// The port an unclaimed node listens on for a claim.
    #[arg(long, env = "CHA_CLAIM_PORT", default_value_t = cha_wire::claim::DEFAULT_CLAIM_PORT)]
    claim_port: u16,
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
    /// Check that an image will run as a Cha environment on this node, say
    /// what is wrong with it, and exit (non-zero if a required check fails).
    /// A registry-named image that isn't here is pulled; a bare name never is.
    #[arg(long, value_name = "IMAGE")]
    check_image: Option<String>,
    /// The security profile `--check-image` runs the image under.
    #[arg(long, requires = "check_image", default_value = "standard",
          value_parser = ["standard", "browser", "steam"])]
    profile: String,
    /// Update this agent to a release (`0.2.1`) as the portal's Update button
    /// does, and exit: run it in the agent's container (`docker exec
    /// cha-node-agent-1 cha-node --update-to 0.2.1`). Pulls the images
    /// unless the engine has them, then hands over to the update helper
    /// (ADR 0018) and shows its log until this container is replaced.
    #[arg(long, value_name = "VERSION")]
    update_to: Option<String>,
    /// The update helper (ADR 0018, started by the agent from the new image):
    /// replace the agent's container ID with one running `--image`, wait for
    /// it to connect, and put the old one back if it doesn't.
    #[arg(long, value_name = "ID", requires = "image")]
    replace_agent: Option<String>,
    /// With `--replace-agent`: the new agent image.
    #[arg(long, requires = "replace_agent")]
    image: Option<String>,
    /// With `--replace-agent`: the compose directory whose `.env` follows the
    /// new release.
    #[arg(long, requires = "replace_agent")]
    env_dir: Option<PathBuf>,
    /// With `--replace-agent`: how many seconds the new agent has to connect.
    #[arg(long, default_value_t = cha_node::update::HEALTH_TIMEOUT.as_secs(), requires = "replace_agent")]
    health_timeout: u64,
    /// The Docker engine's socket; environments need it.
    #[arg(long, env = "CHA_DOCKER_SOCKET", default_value = DEFAULT_SOCKET)]
    docker_socket: String,
    /// The streamer image each environment runs beside its app.
    #[arg(long, env = "CHA_STREAMER_IMAGE", default_value = "cha/streamer:dev")]
    streamer_image: String,
    /// The image that streams a Moonlight host's app (Sunshine, Apollo); like
    /// app images, `cha/gateway:dev` runs as `<registry>/cha-gateway:<tag>`
    /// when `CHA_IMAGE_REGISTRY` is set.
    #[arg(long, env = "CHA_GATEWAY_IMAGE", default_value = "cha/gateway:dev")]
    gateway_image: String,
    /// Look for Moonlight hosts (Sunshine, Apollo) on the LAN so the portal can
    /// adopt them (`_nvstream._tcp`). false turns it off.
    #[arg(long, env = "CHA_MOONLIGHT", default_value_t = true, action = clap::ArgAction::Set)]
    moonlight: bool,
    /// The CDI device that gives containers the GPU.
    #[arg(long, env = "CHA_GPU_DEVICE", default_value = "nvidia.com/gpu=all")]
    gpu_device: String,
    /// The render node streamers composite on (default: the first GPU with an
    /// encoder).
    #[arg(long, env = "CHA_RENDER_NODE")]
    render_node: Option<String>,
    /// Streamers listen on this port and up (two each: HTTP, WebRTC).
    #[arg(long, env = "CHA_PORT_BASE", default_value_t = 7600)]
    port_base: u16,
    #[arg(long, env = "CHA_MAX_ENVIRONMENTS", default_value_t = 16)]
    max_environments: u16,
    /// Let Moonlight clients play this node's environments (ADR 0009): each
    /// environment's streamer gets ports for a session (three UDP ports each
    /// from `CHA_GAMESTREAM_PORT_BASE`, G2) and a secret for its local API,
    /// and the agent runs a GameStream host that clients pair with (a PIN
    /// typed in the portal) and list apps from (G3). Needs a streamer image
    /// built with the `gamestream` feature (the published ones are). Off by
    /// default; older streamers never get the arguments.
    #[arg(long, env = "CHA_GAMESTREAM", default_value_t = false, action = clap::ArgAction::Set)]
    gamestream: bool,
    /// Where those ports start: video, control and audio for the first
    /// environment, then three more for each.
    #[arg(long, env = "CHA_GAMESTREAM_PORT_BASE", default_value_t = 7700)]
    gamestream_port_base: u16,
    /// The GameStream host's HTTP port, which a Moonlight client adds the node
    /// by. Sunshine uses the same ports: one of them per machine.
    #[cfg(feature = "gamestream")]
    #[arg(long, env = "CHA_GAMESTREAM_HTTP_PORT", default_value_t = 47989)]
    gamestream_http_port: u16,
    /// The GameStream host's HTTPS port.
    #[cfg(feature = "gamestream")]
    #[arg(long, env = "CHA_GAMESTREAM_HTTPS_PORT", default_value_t = 47984)]
    gamestream_https_port: u16,
    /// The GameStream host's RTSP port.
    #[cfg(feature = "gamestream")]
    #[arg(long, env = "CHA_GAMESTREAM_RTSP_PORT", default_value_t = 48010)]
    gamestream_rtsp_port: u16,
    /// Quitting an app in Moonlight also stops its environment, when the
    /// Moonlight launch started it (the app list is what the user can run
    /// here, and launching one starts it). Off: a quit ends only the stream,
    /// and the user stops the environment in the portal. Never stops one the
    /// user started in the portal.
    #[cfg(feature = "gamestream")]
    #[arg(long, env = "CHA_GAMESTREAM_QUIT_STOPS", default_value_t = false, action = clap::ArgAction::Set)]
    gamestream_quit_stops: bool,
    /// The host's uinput device: streamers make virtual gamepads with it.
    /// Empty goes without gamepads (e.g. no `uinput` module).
    #[arg(long, env = "CHA_UINPUT", default_value = "/dev/uinput")]
    uinput: String,
    /// The host's uhid device: streamers make virtual DualSense and Steam
    /// Controllers with it. Empty goes without them (those fall back to an
    /// Xbox 360 pad).
    #[arg(long, env = "CHA_UHID", default_value = "/dev/uhid")]
    uhid: String,
    /// The router's public IP, if it forwards the streamers' UDP ports here
    /// (WAN without a mesh or TURN).
    #[arg(long, env = "CHA_PUBLIC_ADDRESS")]
    public_address: Option<String>,
    /// Where apps' data lives: each user's home for an app they keep data
    /// for, and what apps share. An absolute path on this machine, which the
    /// agent's container also sees at the same path (Docker is given host paths).
    #[arg(long, env = "CHA_DATA_ROOT", default_value = cha_wire::DEFAULT_DATA_ROOT)]
    data_root: PathBuf,
    /// Keeps a template's shared directory somewhere else, as TEMPLATE=/path
    /// (repeat the flag, or comma-separate in the variable): for Steam, a
    /// library on a NAS, say `steam=/mnt/games/steam`. Apps see it at that same
    /// path. The agent never creates, changes or removes anything in it, and
    /// its container needs it bound in read-only at that path to check it.
    #[arg(long = "shared-dir", env = "CHA_SHARED_DIRS", value_delimiter = ',')]
    shared_dirs: Vec<String>,
    /// NVIDIA's Wine DLLs on the host (`nvngx.dll`, for DLSS under Proton),
    /// bound into apps read-only at the same path when the host has the
    /// directory; the CDI spec leaves it out. Empty goes without.
    #[arg(
        long,
        env = "CHA_NVIDIA_WINE_DIR",
        default_value = "/usr/lib/x86_64-linux-gnu/nvidia/wine"
    )]
    nvidia_wine_dir: String,
    /// Run the catalog's apps from published images: the registry and owner,
    /// e.g. `ghcr.io/ban-red`, so `cha/env-chrome:dev` runs as
    /// `ghcr.io/ban-red/cha-env-chrome:<CHA_IMAGE_TAG>`, pulled when missing.
    /// Empty runs the local builds.
    #[arg(long, env = "CHA_IMAGE_REGISTRY")]
    image_registry: Option<String>,
    /// The release of the published app images (`0.1.0`).
    #[arg(long, env = "CHA_IMAGE_TAG")]
    image_tag: Option<String>,
    /// `auto` (the default) lets the portal pick this node for a launch;
    /// `manual` keeps it to launches that choose it, for test beds.
    #[arg(long, env = "CHA_PLACEMENT")]
    placement: Option<String>,
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
    if let Some(image) = &args.check_image {
        let profile = cha_node::image_check::parse_profile(&args.profile)?;
        let ok = cha_node::image_check::run(&docker, &args.streamer_image, image, profile).await?;
        std::process::exit(if ok { 0 } else { 1 });
    }
    if let (Some(id), Some(image)) = (&args.replace_agent, &args.image) {
        let timeout = std::time::Duration::from_secs(args.health_timeout.max(1));
        let result =
            cha_node::update::replace_agent(&docker, id, image, args.env_dir.as_deref(), timeout)
                .await;
        if let Err(err) = result {
            warn!("{err:#}");
            std::process::exit(1);
        }
        return Ok(());
    }
    if let Some(version) = &args.update_to {
        return update_to(&docker, version).await;
    }
    let config = docker_config(&args)?;
    #[cfg(feature = "gamestream")]
    let gamestream_host = gamestream_config(&args);
    if args.doctor {
        let identity = Identity::load(&args.state_dir).ok().flatten();
        #[allow(unused_mut)]
        let mut more = Vec::new();
        #[cfg(feature = "gamestream")]
        if let Some(mut host) = gamestream_host.clone() {
            host.name = args
                .name
                .clone()
                .unwrap_or_else(|| inventory::collect().hostname);
            more.extend(gamestream::doctor(&host));
        }
        let ok = doctor::run(
            &docker,
            &config,
            identity.as_ref(),
            &args.state_dir,
            args.moonlight,
            more,
        )
        .await;
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
        None if args.join_token.is_none() && args.discovery => {
            let name = args.name.unwrap_or_else(|| inventory::collect().hostname);
            let identity = claim::wait_for_claim(ClaimConfig {
                state_dir: args.state_dir.clone(),
                name: name.clone(),
                portal_url: args.portal_url.clone(),
                allow_insecure_portal: args.allow_insecure_portal,
                port: args.claim_port,
            })
            .await?;
            info!(node_id = %identity.node_id, %name, "claimed");
            identity
        }
        None => {
            let portal_url = args.portal_url.context(
                "not enrolled yet: pass --portal-url and --join-token, or leave the token out \
                 and claim this node in the portal (CHA_DISCOVERY)",
            )?;
            check_portal_transport(
                &normalize_portal_url(&portal_url)?,
                args.allow_insecure_portal,
            )?;
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

    // If the engine can't be asked, assume AppArmor: check everything.
    hostfiles::warn_if_stale(docker.apparmor().await.unwrap_or(true));
    check_portal_transport(&identity.portal_url, args.allow_insecure_portal)?;
    if identity.portal_url.starts_with("http://") && args.allow_insecure_portal {
        warn!(portal = %identity.portal_url, "plain HTTP to the portal (CHA_ALLOW_INSECURE_PORTAL): for development only");
    }
    #[cfg(feature = "gamestream")]
    let node_name = identity.name.clone();
    let placement = cha_node::inventory::parse_placement(args.placement.as_deref())?;
    let mut agent = Agent::new(identity)?
        .with_placement(placement)
        .with_state_dir(args.state_dir.clone());
    #[cfg(feature = "gamestream")]
    let mut browse = None;
    if args.moonlight {
        match Moonlight::spawn(moonlight::dir(&config.data_root)) {
            Ok(moonlight) => {
                #[cfg(feature = "gamestream")]
                {
                    browse = Some(Arc::clone(&moonlight));
                }
                agent = agent.with_moonlight(moonlight)
            }
            Err(err) => warn!("can't look for Moonlight hosts: {err:#}"),
        }
    }
    #[cfg(not(feature = "gamestream"))]
    if args.gamestream {
        warn!(
            "built without the gamestream feature: environments get their Moonlight ports, \
             but this node has no GameStream host"
        );
    }
    match DockerRuntime::new(docker, config.clone()).await {
        Ok(runtime) => {
            info!(streamer = %config.streamer_image, render_node = %config.render_node, data_root = %config.data_root.display(), "running environments with Docker");
            #[cfg(feature = "gamestream")]
            if let Some(mut host) = gamestream_host {
                host.name = node_name;
                agent = attach_gamestream(agent, host, &runtime, browse.as_deref()).await;
            }
            agent = agent.with_runtime(runtime);
        }
        Err(err) => warn!(
            "can't run environments ({err:#}); mount {} into the agent's container",
            args.docker_socket
        ),
    }
    agent.run().await
}

/// `--update-to`: what the portal's Update button does, run by hand.
async fn update_to(docker: &Docker, version: &str) -> Result<()> {
    cha_node::update::start_update(docker, docker.socket(), version, &mut |msg| {
        if let cha_wire::ToPortal::AgentUpdate { detail, .. } = msg {
            println!("{}", detail.unwrap_or_default());
        }
    })
    .await?;
    println!("The update helper is running; this agent is replaced once the new one connects.");
    cha_node::update::follow_helper(docker).await;
    Ok(())
}

/// The GameStream host's settings, when `CHA_GAMESTREAM` turns it on; the
/// host is named after the node once that is known.
#[cfg(feature = "gamestream")]
fn gamestream_config(args: &Args) -> Option<gamestream::Config> {
    args.gamestream.then(|| {
        let mut config = gamestream::Config::new(
            String::new(),
            args.data_root.clone(),
            gamestream::Ports {
                http: args.gamestream_http_port,
                https: args.gamestream_https_port,
                rtsp: args.gamestream_rtsp_port,
            },
        );
        config.directory.quit_stops = args.gamestream_quit_stops;
        config
    })
}

/// Starts the GameStream host beside the environments and gives it to the
/// agent. Environments run without it if it can't start (a port taken, say).
#[cfg(feature = "gamestream")]
async fn attach_gamestream(
    agent: Agent,
    config: gamestream::Config,
    runtime: &Arc<DockerRuntime>,
    browse: Option<&Moonlight>,
) -> Agent {
    match gamestream::GameStream::start(config, runtime.clone()).await {
        Ok(host) => {
            // The host advertises itself like Sunshine does; this node's own
            // Moonlight browse mustn't offer it to adopt.
            if let Some(browse) = browse {
                browse.ignore_host(host.unique_id());
            }
            agent.with_gamestream(host)
        }
        Err(err) => {
            warn!("the GameStream host didn't start: {err:#}");
            agent
        }
    }
}

/// How environments run here, from the arguments.
fn docker_config(args: &Args) -> Result<DockerConfig> {
    // Refused up front: Docker would be handed paths built from these.
    DataRoot::new(&args.data_root, 1000, 1000)?;
    let shared_dirs = parse_shared_dirs(&args.shared_dirs, &args.data_root)?;
    let gamestream_port_base = args.gamestream.then_some(args.gamestream_port_base);
    if let Some(base) = gamestream_port_base {
        cha_node::environments::check_gamestream_range(
            args.port_base,
            base,
            args.max_environments,
        )?;
    }
    let render_node = args.render_node.clone().unwrap_or_else(|| {
        inventory::collect()
            .gpus
            .into_iter()
            .find(|g| !g.encoders.is_empty())
            .and_then(|g| g.render_node)
            .unwrap_or_else(|| "/dev/dri/renderD128".into())
    });
    Ok(DockerConfig {
        streamer_image: args.streamer_image.clone(),
        render_node,
        gpu_device: args.gpu_device.clone(),
        uinput: Some(args.uinput.trim().to_string()).filter(|u| !u.is_empty()),
        uhid: Some(args.uhid.trim().to_string()).filter(|u| !u.is_empty()),
        public_address: args
            .public_address
            .clone()
            .map(|a| a.trim().to_string())
            .filter(|a| !a.is_empty()),
        port_base: args.port_base,
        gamestream_port_base,
        max_environments: args.max_environments,
        data_root: args.data_root.clone(),
        shared_dirs,
        nvidia_wine_dir: parse_nvidia_wine_dir(&args.nvidia_wine_dir)?,
        log_dir: Some(args.state_dir.join("logs")),
        app_images: PublishedImages::from_settings(
            args.image_registry.as_deref(),
            args.image_tag.as_deref(),
        )?,
        gateway_image: args.gateway_image.trim().to_string(),
    })
}

/// The NVIDIA Wine directory, `None` when empty. Docker is handed it as a bind
/// source and target, so it must be a plain absolute path.
fn parse_nvidia_wine_dir(value: &str) -> Result<Option<PathBuf>> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    let path = PathBuf::from(value);
    anyhow::ensure!(
        path.is_absolute()
            && path
                .components()
                .all(|c| !matches!(c, std::path::Component::ParentDir)),
        "CHA_NVIDIA_WINE_DIR must be an absolute path without `..`, or empty: {value}"
    );
    Ok(Some(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_nvidia_wine_dir() {
        assert_eq!(parse_nvidia_wine_dir("").unwrap(), None);
        assert_eq!(parse_nvidia_wine_dir("  ").unwrap(), None);
        assert_eq!(
            parse_nvidia_wine_dir("/opt/nvidia/wine").unwrap(),
            Some(PathBuf::from("/opt/nvidia/wine"))
        );
        assert!(parse_nvidia_wine_dir("nvidia/wine").is_err());
        assert!(parse_nvidia_wine_dir("/opt/../etc").is_err());
    }
}

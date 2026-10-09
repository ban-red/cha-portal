use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;

/// Cha Portal control plane.
#[derive(Parser, Debug)]
#[command(version, about)]
struct Args {
    #[arg(long, env = "CHA_LISTEN", default_value = "0.0.0.0:7676")]
    listen: SocketAddr,
    /// SQLite database file.
    #[arg(long, env = "CHA_DATABASE", default_value = "data/cha.db")]
    database: PathBuf,
    /// Built portal SPA.
    #[arg(long, env = "CHA_WEB_DIR", default_value = "web/apps/portal/dist")]
    web_dir: PathBuf,
    /// Mark the session cookie `Secure` (serve the portal over HTTPS).
    #[arg(long, env = "CHA_SECURE_COOKIES")]
    secure_cookies: bool,
    #[arg(long, env = "CHA_SESSION_DAYS", default_value_t = 14)]
    session_days: i64,
    /// STUN URLs for players, comma-separated (`stun:host:3478`).
    #[arg(long, env = "CHA_STUN_URLS", value_delimiter = ',')]
    stun: Vec<String>,
    /// TURN URLs for players, comma-separated
    /// (`turn:host:3478?transport=udp,turns:host:443?transport=tcp`).
    #[arg(long, env = "CHA_TURN_URLS", value_delimiter = ',')]
    turn: Vec<String>,
    /// The TURN server's shared secret (coturn `static-auth-secret`); the
    /// portal mints a day's credentials per connection with it.
    #[arg(long, env = "CHA_TURN_SECRET", hide_env_values = true)]
    turn_secret: Option<String>,
    /// Let loopback clients sign in as a `dev` admin with one click (local
    /// development only).
    #[arg(long, env = "CHA_DEV_LOGIN")]
    dev_login: bool,
    /// Browse the LAN for unclaimed nodes, which admins can then claim with
    /// the code the node shows (needs multicast: the host network in Docker).
    #[arg(long, env = "CHA_DISCOVER_NODES", default_value_t = true, action = clap::ArgAction::Set)]
    discover_nodes: bool,
    /// The URL nodes are told to reach the portal at when claimed, e.g.
    /// https://portal.example. Default: the URL the claiming admin is using.
    #[arg(long, env = "CHA_PUBLIC_URL")]
    public_url: Option<String>,
    /// `off` turns internet share links off (ADR 0022): no guest listener,
    /// no `cloudflared`.
    #[arg(long, env = "CHA_TUNNEL", default_value = "on", value_parser = ["on", "off"])]
    tunnel: String,
    /// The `cloudflared` binary.
    #[arg(long, env = "CHA_CLOUDFLARED", default_value = "cloudflared")]
    cloudflared: String,
    /// A named Cloudflare Tunnel's token; without it, internet links use a
    /// quick tunnel (a new `*.trycloudflare.com` address on every start).
    #[arg(long, env = "CHA_TUNNEL_TOKEN", hide_env_values = true)]
    tunnel_token: Option<String>,
    /// The named tunnel's public hostname, e.g. play.example.com.
    #[arg(long, env = "CHA_TUNNEL_HOSTNAME")]
    tunnel_hostname: Option<String>,
    /// The listener for internet share links only, which the tunnel points
    /// at. Keep it on loopback.
    #[arg(long, env = "CHA_GUEST_LISTEN", default_value = "127.0.0.1:7680")]
    guest_listen: SocketAddr,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,sqlx=warn,tower_http=warn".into()),
        )
        .init();
    let args = Args::parse();
    cha_control::run(cha_control::Config {
        listen: args.listen,
        database: args.database,
        web_dir: Some(args.web_dir),
        secure_cookies: args.secure_cookies,
        session_days: args.session_days,
        ice: cha_control::ice::IceConfig {
            stun: args
                .stun
                .into_iter()
                .filter(|u| !u.trim().is_empty())
                .collect(),
            turn: args
                .turn
                .into_iter()
                .filter(|u| !u.trim().is_empty())
                .collect(),
            turn_secret: args.turn_secret.filter(|s| !s.is_empty()),
        },
        dev_login: args.dev_login,
        discover_nodes: args.discover_nodes,
        public_url: args.public_url.filter(|u| !u.trim().is_empty()),
        tunnel: cha_control::tunnel::TunnelConfig {
            enabled: args.tunnel == "on",
            cloudflared: args.cloudflared,
            token: args.tunnel_token.filter(|t| !t.trim().is_empty()),
            hostname: args.tunnel_hostname.filter(|h| !h.trim().is_empty()),
            guest_listen: args.guest_listen,
        },
    })
    .await
}

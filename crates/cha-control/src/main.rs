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
    })
    .await
}

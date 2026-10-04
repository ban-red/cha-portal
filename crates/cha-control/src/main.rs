use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;

/// Cha Portal control plane.
#[derive(Parser, Debug)]
#[command(version, about)]
struct Args {
    #[arg(long, env = "CHA_LISTEN", default_value = "0.0.0.0:8080")]
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
    })
    .await
}

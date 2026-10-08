//! Cha Player: a native game-streaming client (ADR 0010). macOS first.

#[cfg(target_os = "macos")]
mod app;
#[cfg(target_os = "macos")]
mod audio;
#[cfg(target_os = "macos")]
mod awdl;
#[cfg(target_os = "macos")]
mod config;
#[cfg(target_os = "macos")]
mod demo;
#[cfg(target_os = "macos")]
mod health;
#[cfg(target_os = "macos")]
mod input;
#[cfg(target_os = "macos")]
mod overlay_prefs;
#[cfg(target_os = "macos")]
mod present;
#[cfg(target_os = "macos")]
mod render;
#[cfg(target_os = "macos")]
mod session;
#[cfg(target_os = "macos")]
mod stream_prefs;
#[cfg(target_os = "macos")]
mod theme;
#[cfg(target_os = "macos")]
mod ui;
#[cfg(all(target_os = "macos", feature = "portal"))]
mod urlscheme;
#[cfg(target_os = "macos")]
mod video;

#[cfg(target_os = "macos")]
fn main() -> std::process::ExitCode {
    macos::main()
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("cha-player runs on macOS only for now (ADR 0010); Linux and Windows come later.");
}

#[cfg(target_os = "macos")]
mod macos {
    use std::path::PathBuf;
    use std::process::ExitCode;

    use anyhow::{Context, Result};
    use cha_client::Transport;
    use clap::Parser;
    use tracing_subscriber::EnvFilter;

    use crate::{app, config, demo};

    #[derive(Parser)]
    #[command(
        version,
        about = "Cha Player: stream a remote desktop or game, natively"
    )]
    struct Args {
        /// Add a fake host that encodes a test pattern in this process, to try
        /// the player without a network.
        #[arg(long)]
        demo: bool,
        /// Start the first app of the first paired host as soon as it is known.
        #[arg(long)]
        autostart: bool,
        /// Exit after showing this many frames and print the session's stats
        /// (for smoke tests; implies --autostart).
        #[arg(long, value_name = "N")]
        frames: Option<u64>,
        /// Where settings and the transports' identities live.
        #[arg(long, value_name = "DIR")]
        data_dir: Option<PathBuf>,
    }

    pub fn main() -> ExitCode {
        tracing_subscriber::fmt()
            .with_env_filter(
                EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| EnvFilter::new("info,wgpu=warn,naga=warn,sctk=warn")),
            )
            .init();
        match run(Args::parse()) {
            Ok(0) => ExitCode::SUCCESS,
            Ok(code) => ExitCode::from(code as u8),
            Err(e) => {
                eprintln!("cha-player: {e:#}");
                ExitCode::FAILURE
            }
        }
    }

    fn run(args: Args) -> Result<i32> {
        let data_dir = match args.data_dir {
            Some(dir) => dir,
            None => config::data_dir()?,
        };
        std::fs::create_dir_all(&data_dir)
            .with_context(|| format!("creating {}", data_dir.display()))?;

        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .thread_name("transport")
            .build()
            .context("starting the async runtime")?;

        // Transports spawn tasks when they open, so open them inside the runtime.
        let mut transports: Vec<Box<dyn Transport>> = Vec::new();
        #[cfg(feature = "portal")]
        let mut portal = None;
        {
            let _guard = runtime.enter();
            #[cfg(feature = "gamestream")]
            match cha_client_gamestream::GameStream::open(data_dir.clone()) {
                Ok(t) => transports.push(Box::new(t)),
                Err(e) => tracing::warn!("Moonlight hosts unavailable: {e:#}"),
            }
            #[cfg(feature = "portal")]
            match cha_client_portal::Portal::open(&data_dir, &config::device_name()) {
                Ok(t) => {
                    portal = Some((transports.len(), t.clone()));
                    transports.push(Box::new(t));
                }
                Err(e) => tracing::warn!("Cha portals unavailable: {e:#}"),
            }
            if args.demo {
                transports.push(Box::new(demo::DemoTransport));
            }
        }
        if transports.is_empty() {
            anyhow::bail!(
                "no transports: build with the `gamestream` or `portal` feature or run with --demo"
            );
        }

        app::run(
            runtime,
            transports,
            data_dir,
            app::Options {
                frames: args.frames,
                autostart: args.autostart,
                #[cfg(feature = "portal")]
                portal,
            },
        )
    }
}

//! A GameStream (Moonlight) host for this environment's session: ADR 0009.
//!
//! The node's agent runs the host (pairing, the app list, RTSP); this is the
//! media half of a launched session, served by the streamer because that is
//! where the encoder is. Everything is in this directory and behind the
//! `gamestream` cargo feature, so removing the feature removes it, and the
//! browser transports (`wt.rs`, `session.rs`) never see it: a GameStream
//! session is one more viewer of the same engine.
//!
//! - `api`: the local control API the agent drives (`POST /gamestream/session`
//!   with a handoff, `DELETE`, `GET /gamestream/status`), behind a secret;
//! - `backend`: `cha_gamestream::MediaBackend` over `Media`, `Audio`,
//!   `Gamepads` and `Viewers`;
//! - `input`: Moonlight's keys, mouse and pads in our units;
//! - `feedback`: what apps do to the pads, as Moonlight takes it.
//!
//! `signal.rs` starts it when `--gamestream-ports` is given.

use std::future::Future;
use std::net::{IpAddr, Ipv4Addr};
use std::pin::Pin;
use std::sync::Arc;

use anyhow::{Result, bail};
use axum::Router;
use cha_gamestream::MediaPorts;
use tracing::info;

mod api;
mod backend;
mod feedback;
mod input;

use crate::audio::Audio;
use crate::gamepad::Gamepads;
use crate::media::Media;
use crate::viewers::Viewers;

/// The boxed future the protocol's traits return.
type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The environment's media, as the streamer built it.
#[derive(Clone)]
pub struct Engine {
    pub media: Arc<Media>,
    pub audio: Option<Arc<Audio>>,
    pub gamepads: Option<Arc<Gamepads>>,
    pub viewers: Arc<Viewers>,
}

const SECRET_VAR: &str = "CHA_GAMESTREAM_SECRET";
/// The agent makes 32 hex digits; anything shorter than this is a mistake.
const MIN_SECRET: usize = 16;

/// The control API's secret. It comes from the environment, never an
/// argument (those show in `ps`), and is taken out of it so that the apps the
/// streamer starts don't inherit it.
pub struct Secret(String);

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

/// Reads and removes `CHA_GAMESTREAM_SECRET`. Call it while the arguments are
/// parsed, before the streamer starts any thread.
pub fn take_secret() -> Option<Secret> {
    let secret = std::env::var(SECRET_VAR).ok();
    // SAFETY: this runs in `Args::parse()`, the first thing `main` does,
    // while the process has one thread; nothing reads the environment
    // concurrently.
    unsafe { std::env::remove_var(SECRET_VAR) };
    secret.map(Secret)
}

/// `--gamestream-ports video,control,audio`: three distinct UDP ports.
pub fn parse_ports(text: &str) -> Result<MediaPorts, String> {
    let ports: Vec<u16> = text
        .split(',')
        .map(|p| p.trim().parse::<u16>())
        .collect::<Result<_, _>>()
        .map_err(|e| format!("{e}"))?;
    let [video, control, audio] = ports[..] else {
        return Err("expected three ports: video,control,audio".into());
    };
    if [video, control, audio].contains(&0)
        || video == control
        || control == audio
        || video == audio
    {
        return Err("the three ports must be distinct and not 0".into());
    }
    Ok(MediaPorts {
        video,
        control,
        audio,
    })
}

/// Starts the module: the routes of its control API, to merge into the
/// streamer's router. Sessions are bound on `ports`, on every address.
pub fn start(ports: MediaPorts, secret: Option<Secret>, engine: Engine) -> Result<Router> {
    let Some(Secret(secret)) = secret else {
        bail!("--gamestream-ports needs {SECRET_VAR} in the environment");
    };
    if secret.len() < MIN_SECRET {
        bail!("{SECRET_VAR} must be at least {MIN_SECRET} characters");
    }
    info!(
        video = ports.video,
        control = ports.control,
        audio = ports.audio,
        "GameStream: control API up at /gamestream/*, media on these UDP ports"
    );
    let launcher = api::EngineLauncher::new(
        backend::EngineBackend::new(engine),
        // One socket per port covers every address of the machine.
        IpAddr::V4(Ipv4Addr::UNSPECIFIED),
        ports,
    );
    Ok(api::router(
        api::Registry::new(Arc::new(launcher)),
        api::Bearer::new(secret),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ports_parse() {
        assert_eq!(
            parse_ports("7700,7701,7702").unwrap(),
            MediaPorts {
                video: 7700,
                control: 7701,
                audio: 7702
            }
        );
        assert!(parse_ports(" 1, 2 ,3 ").is_ok());
        for bad in ["", "1,2", "1,2,3,4", "1,1,2", "0,1,2", "70000,1,2", "a,b,c"] {
            assert!(parse_ports(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_secret_stays_out_of_debug_output() {
        assert!(!format!("{:?}", Secret("hunter2hunter2hunter2".into())).contains("hunter"));
    }
}

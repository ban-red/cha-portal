//! The Cloudflare Tunnel for internet share links (ADR 0022; the contract is
//! `docs/plans/wan-sharing.md`).
//!
//! The portal runs `cloudflared` itself, as a child process, only while an
//! internet link is live. With a named tunnel's token and hostname the URL is
//! stable; without them it opens a quick tunnel (`*.trycloudflare.com`), whose
//! hostname is new every time it starts. The tunnel points at the guest-only
//! listener ([`crate::guest_app`]), never at the portal. The token goes to the
//! child in `TUNNEL_TOKEN`, not in its arguments, and is never logged or
//! returned by the API.

use std::collections::VecDeque;
use std::net::{IpAddr, SocketAddr};
use std::process::Stdio;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::{Json, Router, routing::get};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::{Mutex, oneshot};
use tracing::{debug, info, warn};

use crate::AppState;
use crate::auth::CurrentUser;
use crate::db;

/// How long `cloudflared` gets to say it is serving.
const START_TIMEOUT: Duration = Duration::from_secs(30);
/// How often the idle task looks, and how long a tunnel with no live link
/// stays up.
const IDLE_CHECK: Duration = Duration::from_secs(30);
const IDLE_GRACE: Duration = Duration::from_secs(60);
/// The last lines of `cloudflared`'s stderr kept for an error message.
const KEPT_LINES: usize = 5;
const MAX_LINE_CHARS: usize = 300;

/// The tunnel's settings (`CHA_TUNNEL*`, `CHA_CLOUDFLARED`, `CHA_GUEST_LISTEN`).
#[derive(Clone)]
pub struct TunnelConfig {
    /// `CHA_TUNNEL=off` turns internet links off: no listener, no process.
    pub enabled: bool,
    /// The `cloudflared` binary.
    pub cloudflared: String,
    /// A named tunnel's token (a secret).
    pub token: Option<String>,
    /// The named tunnel's public hostname.
    pub hostname: Option<String>,
    /// The guest-only listener the tunnel points at.
    pub guest_listen: SocketAddr,
}

impl Default for TunnelConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            cloudflared: "cloudflared".into(),
            token: None,
            hostname: None,
            guest_listen: SocketAddr::from(([127, 0, 0, 1], 7680)),
        }
    }
}

// The token must not reach a log through `{:?}` of the config.
impl std::fmt::Debug for TunnelConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TunnelConfig")
            .field("enabled", &self.enabled)
            .field("cloudflared", &self.cloudflared)
            .field("token", &self.token.as_ref().map(|_| "<hidden>"))
            .field("hostname", &self.hostname)
            .field("guest_listen", &self.guest_listen)
            .finish()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Off,
    Quick,
    Named,
}

impl Mode {
    fn name(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Quick => "quick",
            Self::Named => "named",
        }
    }
}

impl TunnelConfig {
    fn mode(&self) -> Mode {
        if !self.enabled {
            Mode::Off
        } else if self.token.is_some() {
            Mode::Named
        } else {
            Mode::Quick
        }
    }

    /// A named tunnel needs both halves; refuse to start half-configured
    /// rather than fall back to an anonymous tunnel the owner didn't ask for.
    pub fn validate(&self) -> Result<(), &'static str> {
        match (self.enabled, &self.token, self.host()) {
            (true, Some(_), None) => Err("CHA_TUNNEL_TOKEN needs CHA_TUNNEL_HOSTNAME"),
            (true, None, Some(_)) => Err("CHA_TUNNEL_HOSTNAME needs CHA_TUNNEL_TOKEN"),
            _ => Ok(()),
        }
    }

    /// The hostname without a scheme or trailing slash.
    fn host(&self) -> Option<String> {
        let host = self.hostname.as_deref()?.trim();
        let host = host.strip_prefix("https://").unwrap_or(host);
        let host = host.trim_end_matches('/');
        (!host.is_empty()).then(|| host.to_string())
    }

    /// Where the tunnel forwards to: the guest listener, by a loopback
    /// address when it listens on every interface.
    fn target(&self) -> String {
        let mut addr = self.guest_listen;
        if addr.ip().is_unspecified() {
            addr.set_ip(match addr.ip() {
                IpAddr::V4(_) => IpAddr::from([127, 0, 0, 1]),
                IpAddr::V6(_) => IpAddr::from([0, 0, 0, 0, 0, 0, 0, 1]),
            });
        }
        format!("http://{addr}")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Stopped,
    Starting,
    Up,
    Failed,
}

impl Phase {
    fn name(self) -> &'static str {
        match self {
            Self::Stopped => "stopped",
            Self::Starting => "starting",
            Self::Up => "up",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum TunnelError {
    /// Internet links are turned off (`CHA_TUNNEL=off`).
    Off,
    /// `cloudflared` didn't come up; why, without the token.
    Failed(String),
}

/// A tunnel that is up: its URL, and when it came up (Unix seconds).
pub struct Up {
    pub url: String,
    pub started_at: i64,
}

struct Status {
    state: Phase,
    url: Option<String>,
    error: Option<String>,
    /// What `cloudflared` has said so far while starting, for the error if
    /// it never comes up.
    log: Option<String>,
    started_at: i64,
    /// Tells one run of the process from the next: a run that was stopped
    /// or replaced no longer changes the status.
    generation: u64,
    stop: Option<oneshot::Sender<()>>,
    /// The last time a live internet link was seen (or the tunnel was asked
    /// for), for the idle stop.
    last_wanted: Instant,
}

pub struct Tunnel {
    config: TunnelConfig,
    /// Held while a start is in flight, so concurrent callers wait on the
    /// same one.
    start: Mutex<()>,
    status: Arc<StdMutex<Status>>,
}

impl Tunnel {
    pub fn new(config: TunnelConfig) -> Self {
        Self {
            config,
            start: Mutex::new(()),
            status: Arc::new(StdMutex::new(Status {
                state: Phase::Stopped,
                url: None,
                error: None,
                log: None,
                started_at: 0,
                generation: 0,
                stop: None,
                last_wanted: Instant::now(),
            })),
        }
    }

    pub fn mode(&self) -> Mode {
        self.config.mode()
    }

    fn status(&self) -> std::sync::MutexGuard<'_, Status> {
        self.status.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The tunnel's URL, starting `cloudflared` if it isn't up.
    pub async fn ensure_up(&self) -> Result<Up, TunnelError> {
        if self.mode() == Mode::Off {
            return Err(TunnelError::Off);
        }
        let _starting = self.start.lock().await;
        {
            let mut status = self.status();
            status.last_wanted = Instant::now();
            if status.state == Phase::Up
                && let Some(url) = status.url.clone()
            {
                return Ok(Up {
                    url,
                    started_at: status.started_at,
                });
            }
        }
        self.launch().await
    }

    async fn launch(&self) -> Result<Up, TunnelError> {
        let mode = self.mode();
        let named_url = self.config.host().map(|h| format!("https://{h}"));
        let mut cmd = Command::new(&self.config.cloudflared);
        cmd.arg("tunnel").arg("--no-autoupdate");
        match (mode, &self.config.token) {
            (Mode::Named, Some(token)) => {
                cmd.arg("run").env("TUNNEL_TOKEN", token);
            }
            _ => {
                cmd.arg("--url")
                    .arg(self.config.target())
                    .env_remove("TUNNEL_TOKEN");
            }
        }
        cmd.env_remove("CHA_TUNNEL_TOKEN")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let generation = {
            let mut status = self.status();
            status.generation += 1;
            status.state = Phase::Starting;
            status.url = None;
            status.error = None;
            status.log = None;
            status.stop = None;
            status.generation
        };
        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(err) => {
                let message = format!("can't run {}: {err}", self.config.cloudflared);
                return Err(self.fail(generation, message));
            }
        };
        let stderr = child.stderr.take().expect("stderr is piped");
        let (stop_tx, stop_rx) = oneshot::channel();
        let (ready_tx, ready_rx) = oneshot::channel();
        self.status().stop = Some(stop_tx);
        tokio::spawn(supervise(
            SupervisorEnv {
                status: Arc::clone(&self.status),
                generation,
                mode,
                named_url,
                token: self.config.token.clone(),
            },
            child,
            stderr,
            stop_rx,
            ready_tx,
        ));

        // The supervisor settles the status itself; this only waits for it.
        match tokio::time::timeout(START_TIMEOUT, ready_rx).await {
            Ok(Ok(Ok(url))) => {
                info!(%url, "tunnel is up");
                let mut status = self.status();
                status.last_wanted = Instant::now();
                let started_at = status.started_at;
                Ok(Up { url, started_at })
            }
            Ok(Ok(Err(message))) => Err(self.fail(generation, message)),
            Ok(Err(_)) => Err(self.fail(generation, "cloudflared stopped".into())),
            Err(_) => {
                let last = self.status().log.take();
                let message = match last {
                    Some(lines) => format!("cloudflared didn't come up in 30 s: {lines}"),
                    None => "cloudflared didn't come up in 30 s".into(),
                };
                Err(self.fail(generation, message))
            }
        }
    }

    /// Marks run `generation` failed and stops its process.
    fn fail(&self, generation: u64, message: String) -> TunnelError {
        warn!("tunnel failed: {message}");
        let mut status = self.status();
        if status.generation == generation {
            status.generation += 1;
            status.state = Phase::Failed;
            status.url = None;
            status.error = Some(message.clone());
            if let Some(stop) = status.stop.take() {
                let _ = stop.send(());
            }
        }
        TunnelError::Failed(message)
    }

    /// Stops the process, if one runs.
    pub fn stop(&self) {
        let mut status = self.status();
        if matches!(status.state, Phase::Stopped | Phase::Failed) {
            return;
        }
        status.generation += 1;
        status.state = Phase::Stopped;
        status.url = None;
        if let Some(stop) = status.stop.take() {
            let _ = stop.send(());
        }
        info!("tunnel stopped");
    }

    /// Records that an internet link is live, so the tunnel isn't idle.
    fn wanted(&self) {
        self.status().last_wanted = Instant::now();
    }

    /// Stops the tunnel once it has had no live link for the grace period;
    /// whether it did.
    fn stop_if_idle(&self) -> bool {
        let idle = {
            let status = self.status();
            matches!(status.state, Phase::Starting | Phase::Up)
                && status.last_wanted.elapsed() >= IDLE_GRACE
        };
        if idle {
            self.stop();
        }
        idle
    }

    fn view(&self) -> Value {
        let status = self.status();
        let url = match self.mode() {
            // A named tunnel's address doesn't depend on the process.
            Mode::Named => self.config.host().map(|h| format!("https://{h}")),
            _ => status.url.clone(),
        };
        json!({
            "mode": self.mode().name(),
            "state": status.state.name(),
            "url": url,
            "error": status.error,
        })
    }
}

struct SupervisorEnv {
    status: Arc<StdMutex<Status>>,
    generation: u64,
    mode: Mode,
    named_url: Option<String>,
    token: Option<String>,
}

impl SupervisorEnv {
    /// Changes the status, unless this run was stopped or replaced.
    fn update(&self, f: impl FnOnce(&mut Status)) {
        let mut status = self.status.lock().unwrap_or_else(|e| e.into_inner());
        if status.generation == self.generation {
            f(&mut status);
        }
    }
}

/// Owns the `cloudflared` process: reads its stderr (so the pipe never
/// fills), decides when it is serving, and settles the status when it exits.
async fn supervise(
    env: SupervisorEnv,
    mut child: tokio::process::Child,
    stderr: tokio::process::ChildStderr,
    mut stop: oneshot::Receiver<()>,
    ready: oneshot::Sender<Result<String, String>>,
) {
    let mut ready = Some(ready);
    let mut lines = BufReader::new(stderr).lines();
    let mut last: VecDeque<String> = VecDeque::new();
    loop {
        tokio::select! {
            _ = &mut stop => {
                let _ = child.kill().await;
                return;
            }
            line = lines.next_line() => {
                let Ok(Some(line)) = line else { break };
                let line = clean(&line, env.token.as_deref());
                debug!(target: "cloudflared", "{line}");
                if last.len() == KEPT_LINES {
                    last.pop_front();
                }
                last.push_back(line.clone());
                if ready.is_none() {
                    continue;
                }
                let url = match env.mode {
                    Mode::Quick => quick_url(&line),
                    Mode::Named if line.contains("Registered tunnel connection") => {
                        env.named_url.clone()
                    }
                    _ => None,
                };
                if let Some(url) = url
                    && let Some(tx) = ready.take()
                {
                    env.update(|status| {
                        status.state = Phase::Up;
                        status.url = Some(url.clone());
                        status.started_at = db::now();
                    });
                    let _ = tx.send(Ok(url));
                } else {
                    // What `ensure_up` reports if it times out.
                    env.update(|status| {
                        status.log = Some(last.iter().cloned().collect::<Vec<_>>().join(" | "));
                    });
                }
            }
        }
    }
    // stderr closed: the process is exiting.
    let code = child.wait().await.ok().and_then(|s| s.code());
    let why = if last.is_empty() {
        format!("cloudflared exited ({code:?})")
    } else {
        format!(
            "cloudflared exited: {}",
            last.iter().cloned().collect::<Vec<_>>().join(" | ")
        )
    };
    if let Some(tx) = ready.take() {
        let _ = tx.send(Err(why.clone()));
        return;
    }
    env.update(|status| {
        status.generation += 1;
        status.state = Phase::Failed;
        status.url = None;
        status.error = Some(why.clone());
        status.stop = None;
    });
    warn!("tunnel failed: {why}");
}

/// A stderr line, trimmed, without the token (should `cloudflared` ever echo
/// it) and cut to a sane length.
fn clean(line: &str, token: Option<&str>) -> String {
    let mut line = line.trim().to_string();
    if let Some(token) = token.filter(|t| !t.is_empty()) {
        line = line.replace(token, "<token>");
    }
    line.chars().take(MAX_LINE_CHARS).collect()
}

/// The quick tunnel's address in a `cloudflared` log line:
/// `https://<words>.trycloudflare.com`. `api.trycloudflare.com` is where
/// `cloudflared` asks for the tunnel, not the tunnel.
fn quick_url(line: &str) -> Option<String> {
    const SUFFIX: &str = ".trycloudflare.com";
    let start = line.find("https://")?;
    let rest = &line[start + "https://".len()..];
    let end = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '.'))
        .unwrap_or(rest.len());
    let host = &rest[..end];
    let name = host.strip_suffix(SUFFIX)?;
    (!name.is_empty() && name != "api" && !name.contains('.')).then(|| format!("https://{host}"))
}

pub fn routes() -> Router<AppState> {
    Router::new().route("/tunnel", get(status))
}

/// What internet links can use right now. Never the token.
async fn status(State(state): State<AppState>, _: CurrentUser) -> Json<Value> {
    Json(state.tunnel.view())
}

/// Every 30 s: keeps the tunnel up while an internet link is live, and stops
/// it a minute after the last one ended.
pub async fn idle_loop(state: AppState) {
    if state.tunnel.mode() == Mode::Off {
        return;
    }
    loop {
        tokio::time::sleep(IDLE_CHECK).await;
        match db::live_wan_shares(&state.db).await {
            Ok(0) => {
                state.tunnel.stop_if_idle();
            }
            Ok(_) => state.tunnel.wanted(),
            Err(err) => warn!("counting internet links: {err}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_quick_url_is_the_tunnels_not_the_apis() {
        assert_eq!(
            quick_url("2026-10-08T10:00:00Z INF |  https://sample-words-here.trycloudflare.com  |"),
            Some("https://sample-words-here.trycloudflare.com".into())
        );
        assert_eq!(
            quick_url("INF Requesting new quick Tunnel on trycloudflare.com..."),
            None
        );
        assert_eq!(
            quick_url("ERR Post \"https://api.trycloudflare.com/tunnel\": dial tcp: timeout"),
            None
        );
        assert_eq!(quick_url("see https://example.com/x"), None);
    }

    #[test]
    fn lines_lose_the_token() {
        assert_eq!(
            clean(
                "  connecting with eyJhIjoic2VjcmV0 now ",
                Some("eyJhIjoic2VjcmV0")
            ),
            "connecting with <token> now"
        );
    }

    #[test]
    fn a_named_tunnel_needs_token_and_hostname_together() {
        let named = |token: Option<&str>, hostname: Option<&str>| TunnelConfig {
            token: token.map(Into::into),
            hostname: hostname.map(Into::into),
            ..Default::default()
        };
        assert!(named(None, None).validate().is_ok());
        assert!(
            named(Some("t"), Some("play.example.com"))
                .validate()
                .is_ok()
        );
        assert!(named(Some("t"), None).validate().is_err());
        assert!(named(None, Some("play.example.com")).validate().is_err());
        assert_eq!(
            named(Some("t"), Some("https://play.example.com/")).host(),
            Some("play.example.com".into())
        );
        assert_eq!(named(Some("t"), Some("a.b")).mode(), Mode::Named);
        assert_eq!(named(None, None).mode(), Mode::Quick);
    }

    #[test]
    fn the_target_is_loopback_even_when_the_listener_is_not() {
        let at = |addr: &str| TunnelConfig {
            guest_listen: addr.parse().unwrap(),
            ..Default::default()
        };
        assert_eq!(at("127.0.0.1:7680").target(), "http://127.0.0.1:7680");
        assert_eq!(at("0.0.0.0:7680").target(), "http://127.0.0.1:7680");
        assert_eq!(at("[::]:7680").target(), "http://[::1]:7680");
    }

    #[test]
    fn the_token_stays_out_of_debug_output() {
        let config = TunnelConfig {
            token: Some("super-secret".into()),
            ..Default::default()
        };
        assert!(!format!("{config:?}").contains("super-secret"));
    }

    #[tokio::test]
    async fn a_tunnel_with_no_live_link_stops_after_a_minute() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("cloudflared");
        std::fs::write(
            &bin,
            "#!/bin/sh\necho 'INF |  https://idle-test.trycloudflare.com  |' >&2\nexec sleep 300\n",
        )
        .unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let tunnel = Tunnel::new(TunnelConfig {
            cloudflared: bin.display().to_string(),
            ..Default::default()
        });
        assert!(!tunnel.stop_if_idle(), "nothing runs yet");
        let up = tunnel.ensure_up().await.unwrap();
        assert_eq!(up.url, "https://idle-test.trycloudflare.com");
        assert!(!tunnel.stop_if_idle(), "just asked for");
        tunnel.status().last_wanted = Instant::now() - IDLE_GRACE;
        assert!(tunnel.stop_if_idle());
        assert_eq!(tunnel.view()["state"], "stopped");
        assert_eq!(tunnel.view()["url"], Value::Null);
    }
}

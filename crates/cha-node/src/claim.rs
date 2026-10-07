//! The unclaimed mode (ADR 0007): a node with no identity and no join token
//! shows a pairing code, advertises itself on the LAN and waits for a portal
//! to be claimed with that code.
//!
//! The claim port serves the three exchanges of [`cha_wire::claim`]; the
//! agent holds its key in memory until a portal proves it knows the code and
//! names the node's id. Only then does it save an identity and carry on as if
//! it had enrolled with a join token.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use axum::extract::{ConnectInfo, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use cha_wire::claim::{
    self, ClaimError, ClaimSession, DONE_PATH, DoneRequest, ErrorBody, FINISH_PATH, FinishRequest,
    FinishResponse, MDNS_SERVICE, NodeStart, START_PATH, StartRequest, StartResponse,
};
use cha_wire::{Gpu, NodeKey};
use mdns_sd::{ServiceDaemon, ServiceInfo};
use tokio::sync::watch;
use tracing::{info, warn};

use crate::{AGENT_VERSION, Identity, check_portal_transport, inventory, normalize_portal_url};

/// Where the pairing code is kept for `--doctor` (and the owner, who may not
/// have the log at hand).
const CODE_FILE: &str = "claim-code";
/// A claim nobody finished is replaced by the next after this long.
const STALE_CLAIM: Duration = Duration::from_secs(30);
/// This many failed confirmations replace the code.
const MAX_FAILURES: u32 = 5;
const COOLDOWN: Duration = Duration::from_secs(30);

/// What the claim mode needs to know.
#[derive(Debug, Clone)]
pub struct ClaimConfig {
    pub state_dir: PathBuf,
    /// This node's name in the portal.
    pub name: String,
    /// `CHA_PORTAL_URL`: the portal this node dials, whatever URL the admin used.
    pub portal_url: Option<String>,
    pub allow_insecure_portal: bool,
    /// `CHA_CLAIM_PORT`.
    pub port: u16,
}

/// The pairing code saved by an unclaimed agent, as shown (`4821-9375`).
pub fn read_code(state_dir: &Path) -> Option<String> {
    let digits = std::fs::read_to_string(state_dir.join(CODE_FILE)).ok()?;
    claim::normalize_code(&digits).map(|d| claim::format_code(&d))
}

/// Waits for a portal to claim this node, advertising it over mDNS meanwhile;
/// returns the identity it saved.
pub async fn wait_for_claim(config: ClaimConfig) -> Result<Identity> {
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", config.port))
        .await
        .with_context(|| {
            format!(
                "listening for a claim on port {} (CHA_CLAIM_PORT); \
                 set CHA_DISCOVERY=false to use a join token instead",
                config.port
            )
        })?;
    let key = new_key()?;
    let gpu = gpu_summary(&tokio::task::spawn_blocking(|| inventory::collect().gpus).await?);
    let advert = Advertisement::start(&config.name, config.port, &gpu, &key);
    let identity = serve_claims(listener, config, key).await;
    advert.stop().await;
    identity
}

fn new_key() -> Result<NodeKey> {
    let mut secret = [0u8; 32];
    getrandom::fill(&mut secret).map_err(|e| anyhow::anyhow!("random source: {e}"))?;
    Ok(NodeKey::from_secret(secret))
}

/// Serves the claim port on `listener` until a claim completes; the identity
/// is saved and the listener closed by then.
pub async fn serve_claims(
    listener: tokio::net::TcpListener,
    config: ClaimConfig,
    key: NodeKey,
) -> Result<Identity> {
    let code = claim::generate_code()?;
    announce(&config, &code, &key.public_b64())?;
    let shared = Arc::new(Shared {
        public_key: key.public_b64(),
        config,
        key,
        state: Mutex::new(Claiming {
            code,
            pending: None,
            failures: 0,
            cooldown_until: None,
        }),
        claimed: Mutex::new(None),
        claimed_tx: watch::channel(false).0,
    });
    let app = Router::new()
        .route(START_PATH, post(start))
        .route(FINISH_PATH, post(finish))
        .route(DONE_PATH, post(done))
        .with_state(Arc::clone(&shared));
    let mut stop = shared.claimed_tx.subscribe();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(async move {
            let _ = stop.wait_for(|claimed| *claimed).await;
        })
        .await
    });
    let _ = shared
        .claimed_tx
        .subscribe()
        .wait_for(|claimed| *claimed)
        .await;
    // The portal's /claim/done reply is still going out; give it a moment.
    if tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .is_err()
    {
        warn!("the claim port didn't close in time");
    }
    let identity = shared
        .claimed
        .lock()
        .expect("claim lock")
        .take()
        .context("the claim ended without an identity")?;
    Ok(identity)
}

/// Logs the code, and keeps it in the state directory. The fingerprint is
/// logged too: the portal lists it, so the admin can tell this node from
/// another advertising the same name (which would learn the code if given it).
fn announce(config: &ClaimConfig, code: &str, public_key: &str) -> Result<()> {
    write_code(&config.state_dir, code)?;
    info!(
        "unclaimed: in the portal, open Admin → Nodes and claim \"{}\" (fingerprint {}) with code {}",
        config.name,
        claim::fingerprint_b64(public_key).unwrap_or_default(),
        claim::format_code(code)
    );
    Ok(())
}

fn write_code(state_dir: &Path, code: &str) -> Result<()> {
    use std::io::Write;
    std::fs::create_dir_all(state_dir)
        .with_context(|| format!("creating {}", state_dir.display()))?;
    let path = state_dir.join(CODE_FILE);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options
        .open(&path)
        .with_context(|| format!("writing {}", path.display()))?;
    writeln!(file, "{}", claim::format_code(code))?;
    Ok(())
}

struct Shared {
    config: ClaimConfig,
    key: NodeKey,
    public_key: String,
    state: Mutex<Claiming>,
    claimed: Mutex<Option<Identity>>,
    /// Becomes true once the identity is saved.
    claimed_tx: watch::Sender<bool>,
}

struct Claiming {
    /// The 8 digits.
    code: String,
    pending: Option<Pending>,
    failures: u32,
    cooldown_until: Option<Instant>,
}

struct Pending {
    id: String,
    session: ClaimSession,
    /// What this node will dial.
    portal_url: String,
    started: Instant,
    /// The portal's confirmation checked out.
    confirmed: bool,
}

/// A refusal, as the portal reads it.
struct Refusal(StatusCode, &'static str, String);

impl IntoResponse for Refusal {
    fn into_response(self) -> Response {
        let body = ErrorBody {
            error: self.1.to_string(),
            message: self.2,
        };
        (self.0, Json(body)).into_response()
    }
}

fn bad_request(message: impl Into<String>) -> Refusal {
    Refusal(
        StatusCode::BAD_REQUEST,
        claim::code::BAD_REQUEST,
        message.into(),
    )
}

fn unknown_claim() -> Refusal {
    Refusal(
        StatusCode::NOT_FOUND,
        claim::code::UNKNOWN_CLAIM,
        "no such claim in progress; start again".into(),
    )
}

fn cooling_down(until: Instant) -> Refusal {
    let secs = until.saturating_duration_since(Instant::now()).as_secs() + 1;
    Refusal(
        StatusCode::TOO_MANY_REQUESTS,
        claim::code::COOLING_DOWN,
        format!("too many wrong codes; this node takes claims again in {secs} s, with a new code"),
    )
}

fn cooldown(state: &Claiming) -> Option<Instant> {
    state.cooldown_until.filter(|t| *t > Instant::now())
}

async fn start(
    State(shared): State<Arc<Shared>>,
    Json(req): Json<StartRequest>,
) -> Result<Json<StartResponse>, Refusal> {
    let mut state = shared.state.lock().expect("claim lock");
    if let Some(until) = cooldown(&state) {
        return Err(cooling_down(until));
    }
    if state
        .pending
        .as_ref()
        .is_some_and(|p| p.started.elapsed() < STALE_CLAIM)
    {
        return Err(Refusal(
            StatusCode::CONFLICT,
            claim::code::BUSY,
            "another claim is in progress on this node; try again in a moment".into(),
        ));
    }
    // The portal this node will dial: its own setting, else the one claiming.
    let portal_url = shared
        .config
        .portal_url
        .as_deref()
        .unwrap_or(&req.portal_url);
    let portal_url = normalize_portal_url(portal_url).map_err(|e| bad_request(format!("{e:#}")))?;
    check_portal_transport(&portal_url, shared.config.allow_insecure_portal).map_err(|e| {
        Refusal(
            StatusCode::CONFLICT,
            claim::code::INSECURE_PORTAL,
            format!(
                "{e:#}. Open the portal over https://, or set CHA_ALLOW_INSECURE_PORTAL=true \
                 on this node (a dev portal on a trusted LAN)"
            ),
        )
    })?;
    let id = random_id().map_err(|e| bad_request(e.to_string()))?;
    let (spake, session) = NodeStart::respond(&state.code, &req.spake, &req.portal_url, &id)
        .map_err(|e| bad_request(e.to_string()))?;
    state.pending = Some(Pending {
        id: id.clone(),
        session,
        portal_url,
        started: Instant::now(),
        confirmed: false,
    });
    Ok(Json(StartResponse {
        claim_id: id,
        spake,
    }))
}

async fn finish(
    State(shared): State<Arc<Shared>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(req): Json<FinishRequest>,
) -> Result<Json<FinishResponse>, Refusal> {
    let mut state = shared.state.lock().expect("claim lock");
    if let Some(until) = cooldown(&state) {
        return Err(cooling_down(until));
    }
    let pending = match state.pending.as_mut() {
        Some(p) if p.id == req.claim_id && !p.confirmed => p,
        _ => return Err(unknown_claim()),
    };
    if pending.session.verify_portal_mac(&req.mac).is_err() {
        // One guess per start: the exchange is spent either way.
        state.pending = None;
        state.failures += 1;
        warn!(%peer, failures = state.failures, "a claim with the wrong code");
        if state.failures >= MAX_FAILURES {
            let code = claim::generate_code().map_err(|e| bad_request(e.to_string()))?;
            warn!(%peer, "too many wrong codes; the pairing code is replaced");
            if let Err(err) = announce(&shared.config, &code, &shared.public_key) {
                warn!("saving the new pairing code: {err:#}");
            }
            state.code = code;
            state.failures = 0;
            state.cooldown_until = Some(Instant::now() + COOLDOWN);
        }
        return Err(Refusal(
            StatusCode::FORBIDDEN,
            claim::code::WRONG_CODE,
            "the pairing code doesn't match".into(),
        ));
    }
    pending.confirmed = true;
    let mac = pending
        .session
        .node_mac(&shared.public_key)
        .map_err(|e| bad_request(e.to_string()))?;
    Ok(Json(FinishResponse {
        public_key: shared.public_key.clone(),
        name: shared.config.name.clone(),
        agent_version: AGENT_VERSION.to_string(),
        mac,
    }))
}

async fn done(
    State(shared): State<Arc<Shared>>,
    Json(req): Json<DoneRequest>,
) -> Result<Json<HashMap<String, String>>, Refusal> {
    let mut state = shared.state.lock().expect("claim lock");
    let pending = match state.pending.as_ref() {
        Some(p) if p.id == req.claim_id && p.confirmed => p,
        _ => return Err(unknown_claim()),
    };
    if let Err(err) = pending.session.verify_done_mac(&req.node_id, &req.mac) {
        state.pending = None;
        return Err(Refusal(
            StatusCode::FORBIDDEN,
            claim::code::WRONG_CODE,
            err.to_string(),
        ));
    }
    let identity = Identity::new(
        pending.portal_url.clone(),
        req.node_id.clone(),
        shared.config.name.clone(),
        &shared.key,
    );
    identity
        .save(&shared.config.state_dir)
        .map_err(|e| bad_request(format!("saving the identity: {e:#}")))?;
    if let Err(err) = std::fs::remove_file(shared.config.state_dir.join(CODE_FILE)) {
        warn!("removing {CODE_FILE}: {err}");
    }
    state.pending = None;
    info!(node_id = %identity.node_id, portal = %identity.portal_url, "claimed");
    *shared.claimed.lock().expect("claim lock") = Some(identity);
    shared.claimed_tx.send_replace(true);
    Ok(Json(HashMap::new()))
}

fn random_id() -> Result<String, ClaimError> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|e| ClaimError::Random(e.to_string()))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// A short GPU name for the portal's list: `NVIDIA GeForce RTX 4090` → `RTX 4090`.
fn gpu_summary(gpus: &[Gpu]) -> String {
    let Some(gpu) = gpus.first() else {
        return String::new();
    };
    let name = gpu
        .name
        .trim()
        .trim_start_matches("NVIDIA ")
        .trim_start_matches("GeForce ")
        .trim_start_matches("AMD ")
        .trim_start_matches("Radeon ");
    name.chars().take(40).collect()
}

/// The node's mDNS registration, until it's claimed.
struct Advertisement(Option<(ServiceDaemon, String)>);

impl Advertisement {
    /// Advertises on every interface. A node that can't (no multicast, a
    /// container without host networking) still takes claims by address, so
    /// this only warns.
    fn start(name: &str, port: u16, gpu: &str, key: &NodeKey) -> Self {
        match register(name, port, gpu, key) {
            Ok(advert) => {
                info!(port, "advertising this node on the LAN ({MDNS_SERVICE})");
                Self(Some(advert))
            }
            Err(err) => {
                warn!("can't advertise on the LAN ({err:#}); the portal won't find this node");
                Self(None)
            }
        }
    }

    /// Withdraws the advertisement and stops the mDNS daemon.
    async fn stop(self) {
        let Some((daemon, fullname)) = self.0 else {
            return;
        };
        let _ = tokio::task::spawn_blocking(move || {
            if let Ok(done) = daemon.unregister(&fullname) {
                let _ = done.recv_timeout(Duration::from_secs(1));
            }
            if let Ok(done) = daemon.shutdown() {
                let _ = done.recv_timeout(Duration::from_secs(1));
            }
        })
        .await;
    }
}

fn register(name: &str, port: u16, gpu: &str, key: &NodeKey) -> Result<(ServiceDaemon, String)> {
    let daemon = ServiceDaemon::new()?;
    let fingerprint =
        claim::fingerprint_b64(&key.public_b64()).context("fingerprinting the node's key")?;
    let properties = HashMap::from([
        ("v".to_string(), "1".to_string()),
        ("name".to_string(), name.to_string()),
        ("gpu".to_string(), gpu.to_string()),
        ("fp".to_string(), fingerprint),
    ]);
    let host = format!("{}.local.", inventory::collect().hostname);
    let instance: String = name.chars().take(60).collect();
    let info =
        ServiceInfo::new(MDNS_SERVICE, &instance, &host, "", port, properties)?.enable_addr_auto();
    let fullname = info.get_fullname().to_string();
    daemon.register(info)?;
    Ok((daemon, fullname))
}

#[cfg(test)]
mod tests {
    use super::*;
    use cha_wire::claim::PortalStart;

    struct Node {
        base: String,
        http: reqwest::Client,
        state_dir: tempfile::TempDir,
        done: tokio::task::JoinHandle<Result<Identity>>,
    }

    async fn node(portal_url: Option<&str>, allow_insecure: bool) -> Node {
        crate::init_tls();
        let state_dir = tempfile::tempdir().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let config = ClaimConfig {
            state_dir: state_dir.path().to_path_buf(),
            name: "gpu-box".into(),
            portal_url: portal_url.map(str::to_string),
            allow_insecure_portal: allow_insecure,
            port: 0,
        };
        let done = tokio::spawn(serve_claims(listener, config, new_key().unwrap()));
        let node = Node {
            base,
            http: reqwest::Client::new(),
            state_dir,
            done,
        };
        // serve_claims writes the code before it listens.
        for _ in 0..100 {
            if node.code().is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        node
    }

    impl Node {
        fn code(&self) -> Option<String> {
            read_code(self.state_dir.path())
        }

        async fn post<T: serde::Serialize>(
            &self,
            path: &str,
            body: &T,
        ) -> (StatusCode, serde_json::Value) {
            let res = self
                .http
                .post(format!("{}{path}", self.base))
                .json(body)
                .send()
                .await
                .unwrap();
            let status = res.status();
            (status, res.json().await.unwrap_or_default())
        }

        /// The portal's side as far as `/claim/start`.
        async fn start(
            &self,
            code: &str,
            url: &str,
        ) -> Result<(ClaimSession, String), (StatusCode, serde_json::Value)> {
            let portal = PortalStart::new(code).unwrap();
            let (status, body) = self
                .post(
                    START_PATH,
                    &StartRequest {
                        spake: portal.message(),
                        portal_url: url.into(),
                    },
                )
                .await;
            if !status.is_success() {
                return Err((status, body));
            }
            let reply: StartResponse = serde_json::from_value(body).unwrap();
            let session = portal.finish(&reply.spake, url, &reply.claim_id).unwrap();
            Ok((session, reply.claim_id))
        }

        async fn attempt(&self, code: &str) -> (StatusCode, serde_json::Value) {
            match self.start(code, "https://portal.example").await {
                Ok((session, claim_id)) => {
                    self.post(
                        FINISH_PATH,
                        &FinishRequest {
                            claim_id,
                            mac: session.portal_mac(),
                        },
                    )
                    .await
                }
                Err(refused) => refused,
            }
        }
    }

    #[tokio::test]
    async fn the_right_code_claims_the_node() {
        let node = node(None, false).await;
        let code = node.code().unwrap();
        assert_eq!(code.len(), 9, "{code}");
        let url = "https://portal.example";
        let (session, claim_id) = node.start(&code, url).await.unwrap();
        let (status, body) = node
            .post(
                FINISH_PATH,
                &FinishRequest {
                    claim_id: claim_id.clone(),
                    mac: session.portal_mac(),
                },
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let reply: FinishResponse = serde_json::from_value(body).unwrap();
        assert_eq!(reply.name, "gpu-box");
        session
            .verify_node_mac(&reply.public_key, &reply.mac)
            .unwrap();
        let (status, _) = node
            .post(
                DONE_PATH,
                &DoneRequest {
                    claim_id,
                    node_id: "node-1".into(),
                    mac: session.done_mac("node-1"),
                },
            )
            .await;
        assert_eq!(status, StatusCode::OK);

        let Node {
            done, state_dir, ..
        } = node;
        let identity = done.await.unwrap().unwrap();
        let code_gone = read_code(state_dir.path()).is_none();
        assert_eq!(identity.node_id, "node-1");
        assert_eq!(identity.portal_url, url);
        assert_eq!(identity.key().unwrap().public_b64(), reply.public_key);
        let saved = Identity::load(state_dir.path()).unwrap().unwrap();
        assert_eq!(saved.node_id, "node-1");
        assert!(code_gone, "the code file is removed");
    }

    #[tokio::test]
    async fn the_nodes_own_portal_url_wins() {
        let node = node(Some("http://127.0.0.1:7676/"), false).await;
        let code = node.code().unwrap();
        let (session, claim_id) = node.start(&code, "https://portal.example").await.unwrap();
        let (status, _) = node
            .post(
                FINISH_PATH,
                &FinishRequest {
                    claim_id: claim_id.clone(),
                    mac: session.portal_mac(),
                },
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        node.post(
            DONE_PATH,
            &DoneRequest {
                claim_id,
                node_id: "n".into(),
                mac: session.done_mac("n"),
            },
        )
        .await;
        let identity = node.done.await.unwrap().unwrap();
        assert_eq!(identity.portal_url, "http://127.0.0.1:7676");
    }

    #[tokio::test]
    async fn five_wrong_codes_replace_the_code_and_pause_claims() {
        let node = node(None, false).await;
        let code = node.code().unwrap();
        let wrong = if code == "0000-0000" {
            "11111111"
        } else {
            "00000000"
        };
        for _ in 0..4 {
            let (status, body) = node.attempt(wrong).await;
            assert_eq!(status, StatusCode::FORBIDDEN);
            assert_eq!(body["error"], "wrong_code");
        }
        assert_eq!(node.code().unwrap(), code, "not replaced yet");
        let (status, _) = node.attempt(wrong).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_ne!(node.code().unwrap(), code, "replaced after the fifth");

        // Even the new code is refused during the pause.
        let new_code = node.code().unwrap();
        let (status, body) = node.attempt(&new_code).await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(body["error"], "cooling_down");
        node.done.abort();
    }

    #[tokio::test]
    async fn one_claim_at_a_time() {
        let node = node(None, false).await;
        let code = node.code().unwrap();
        node.start(&code, "https://portal.example").await.unwrap();
        let (status, body) = node
            .start(&code, "https://portal.example")
            .await
            .unwrap_err();
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["error"], "busy");
        node.done.abort();
    }

    #[tokio::test]
    async fn plain_http_to_another_machine_is_refused() {
        let node = node(None, false).await;
        let code = node.code().unwrap();
        let (status, body) = node
            .start(&code, "http://192.168.1.5:7676")
            .await
            .unwrap_err();
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["error"], "insecure_portal");
        assert!(
            body["message"]
                .as_str()
                .unwrap()
                .contains("CHA_ALLOW_INSECURE_PORTAL")
        );
        // Loopback is fine.
        node.start(&code, "http://127.0.0.1:7676").await.unwrap();
        node.done.abort();

        let lan = self::node(None, true).await;
        let code = lan.code().unwrap();
        lan.start(&code, "http://192.168.1.5:7676").await.unwrap();
        lan.done.abort();
    }

    #[test]
    fn gpus_are_named_briefly() {
        let gpu = |name: &str| Gpu {
            vendor: "nvidia".into(),
            name: name.into(),
            memory_mb: None,
            driver: None,
            render_node: None,
            encoders: vec![],
        };
        assert_eq!(gpu_summary(&[gpu("NVIDIA GeForce RTX 4090")]), "RTX 4090");
        assert_eq!(gpu_summary(&[gpu("AMD Radeon RX 7900 XTX")]), "RX 7900 XTX");
        assert_eq!(gpu_summary(&[]), "");
    }
}

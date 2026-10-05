//! The agent against a real portal on a random port: enrollment, the signed
//! handshake, inventory, a request through the node channel, removal, and the
//! environment lifecycle (with a fake runtime standing in for Docker).

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use cha_control::{AppState, Config, app, db};
use cha_node::environments::{Connect, Exit, Progress, Runtime};
use cha_node::{Agent, Identity, enroll, init_tls};
use cha_wire::{EnvironmentSpec, Gpu, Inventory, SecurityProfile, StreamerEndpoint, close};
use futures_util::future::BoxFuture;
use serde_json::{Value, json};
use tokio::sync::{Semaphore, broadcast};

struct Portal {
    url: String,
    http: reqwest::Client,
    cookie: String,
    _dir: tempfile::TempDir,
}

/// Starts a portal and signs in its first admin.
async fn portal() -> Portal {
    init_tls();
    let dir = tempfile::tempdir().unwrap();
    let config = Config {
        listen: "127.0.0.1:0".parse().unwrap(),
        database: dir.path().join("cha.db"),
        web_dir: None,
        secure_cookies: false,
        session_days: 1,
        ice: Default::default(),
        dev_login: false,
    };
    let pool = db::open(&config.database).await.unwrap();
    let state = AppState::new(config, pool).await.unwrap();
    let setup_token = state.setup_token.lock().await.clone().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(
            listener,
            app(state).into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });

    let http = reqwest::Client::new();
    let res = http
        .post(format!("{url}/api/setup"))
        .json(&json!({ "token": setup_token, "username": "admin", "password": "correct horse battery" }))
        .send()
        .await
        .unwrap();
    assert!(res.status().is_success());
    let cookie = res.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    Portal {
        url,
        http,
        cookie,
        _dir: dir,
    }
}

impl Portal {
    async fn admin(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        let mut req = self
            .http
            .request(method.parse().unwrap(), format!("{}{path}", self.url))
            .header("cookie", &self.cookie);
        if let Some(body) = body {
            req = req.json(&body);
        }
        let res = req.send().await.unwrap();
        let status = res.status().as_u16();
        (status, res.json().await.unwrap_or(Value::Null))
    }

    async fn join_token(&self) -> String {
        let (status, body) = self
            .admin(
                "POST",
                "/api/nodes/join-tokens",
                Some(json!({ "label": "test" })),
            )
            .await;
        assert_eq!(status, 200, "{body}");
        body["token"].as_str().unwrap().to_string()
    }

    /// Polls an environment until `check` passes.
    async fn wait_for_env(&self, id: &str, check: impl Fn(&Value) -> bool) -> Value {
        for _ in 0..100 {
            let (_, env) = self
                .admin("GET", &format!("/api/environments/{id}"), None)
                .await;
            if check(&env) {
                return env;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("environment {id} never reached the expected state");
    }

    /// Polls the node list until `check` passes for the node.
    async fn wait_for_node(&self, node_id: &str, check: impl Fn(&Value) -> bool) -> Value {
        for _ in 0..100 {
            let (_, nodes) = self.admin("GET", "/api/nodes", None).await;
            if let Some(node) = nodes
                .as_array()
                .unwrap()
                .iter()
                .find(|n| n["id"] == node_id)
                && check(node)
            {
                return node.clone();
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("node {node_id} never reached the expected state");
    }
}

/// Records what the portal asks for, as Docker would run it.
struct FakeRuntime {
    started: Mutex<Vec<EnvironmentSpec>>,
    connects: Mutex<Vec<Connect>>,
    stopped: Mutex<Vec<String>>,
    running: Mutex<Vec<String>>,
    exits: broadcast::Sender<Exit>,
    progress: broadcast::Sender<Progress>,
    /// Whether this node's agent knows app data: it reports a data root (and
    /// the share its owner keeps Steam's library on).
    storage: bool,
    /// What the portal asked it to delete, and the reason it refuses to, if set.
    deleted: Mutex<Vec<(String, String)>>,
    refuse_delete: Mutex<Option<String>>,
    /// Each start waits for a permit: a real one takes seconds, and an instant
    /// one can finish before the launch even answers.
    starts: Semaphore,
}

impl FakeRuntime {
    fn new(running: &[&str]) -> Arc<Self> {
        Self::build(running, true)
    }

    /// A node whose agent predates app data.
    fn without_storage() -> Arc<Self> {
        Self::build(&[], false)
    }

    fn build(running: &[&str], storage: bool) -> Arc<Self> {
        Arc::new(Self {
            started: Mutex::default(),
            connects: Mutex::default(),
            stopped: Mutex::default(),
            running: Mutex::new(running.iter().map(|s| s.to_string()).collect()),
            exits: broadcast::channel(8).0,
            progress: broadcast::channel(8).0,
            storage,
            deleted: Mutex::default(),
            refuse_delete: Mutex::default(),
            starts: Semaphore::new(0),
        })
    }

    /// Polls until the portal has had `id` stopped.
    async fn wait_for_stop(&self, id: &str) {
        for _ in 0..100 {
            if self.stopped.lock().unwrap().iter().any(|s| s == id) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("{id} was never stopped");
    }
}

impl Runtime for FakeRuntime {
    fn start(&self, spec: EnvironmentSpec) -> BoxFuture<'_, Result<StreamerEndpoint>> {
        Box::pin(async move {
            self.starts.acquire().await?.forget();
            self.running.lock().unwrap().push(spec.id.clone());
            self.started.lock().unwrap().push(spec);
            Ok(StreamerEndpoint {
                http_port: 47000,
                webrtc_port: 47001,
                webtransport_port: 47002,
            })
        })
    }

    fn stop(&self, id: String) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            self.running.lock().unwrap().retain(|r| *r != id);
            self.stopped.lock().unwrap().push(id);
            Ok(())
        })
    }

    fn running(&self) -> BoxFuture<'_, Result<Vec<String>>> {
        Box::pin(async move { Ok(self.running.lock().unwrap().clone()) })
    }

    fn exits(&self) -> broadcast::Receiver<Exit> {
        self.exits.subscribe()
    }

    fn progress(&self) -> broadcast::Receiver<Progress> {
        self.progress.subscribe()
    }

    fn data_root(&self) -> Option<String> {
        self.storage.then(|| "/srv/cha-portal".to_string())
    }

    fn shared_dirs(&self) -> BTreeMap<String, String> {
        if self.storage {
            BTreeMap::from([("steam".to_string(), "/mnt/games/steam".to_string())])
        } else {
            BTreeMap::new()
        }
    }

    fn delete_user_data(&self, user: String, template: String) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            if let Some(reason) = self.refuse_delete.lock().unwrap().clone() {
                anyhow::bail!(reason);
            }
            self.deleted.lock().unwrap().push((user, template));
            Ok(())
        })
    }

    fn connect(&self, request: Connect) -> BoxFuture<'_, Result<Value>> {
        Box::pin(async move {
            self.connects.lock().unwrap().push(request);
            Ok(json!({ "type": "answer", "sdp": "v=0 fake" }))
        })
    }
}

fn test_inventory() -> Inventory {
    Inventory {
        hostname: "test-host".into(),
        os: "Test OS".into(),
        arch: "x86_64".into(),
        cpus: 16,
        memory_mb: 65536,
        gpus: vec![Gpu {
            vendor: "nvidia".into(),
            name: "NVIDIA GeForce RTX 4090".into(),
            memory_mb: Some(24564),
            driver: Some("595.71.05".into()),
            render_node: Some("/dev/dri/renderD128".into()),
            encoders: vec!["h264".into(), "hevc".into(), "av1".into()],
        }],
        addresses: vec!["192.168.1.20".into()],
        // The agent adds these.
        data_root: None,
        shared_dirs: Default::default(),
    }
}

#[tokio::test]
async fn a_node_enrolls_connects_answers_and_is_removed() {
    let p = portal().await;
    let token = p.join_token().await;
    let identity = enroll(&p.url, &token, "test-node").await.unwrap();

    // Join tokens work once.
    let again = enroll(&p.url, &token, "second").await.unwrap_err();
    assert!(again.to_string().contains("403"), "{again}");

    let agent = Agent::new(identity.clone())
        .unwrap()
        .with_inventory(test_inventory);
    let running = tokio::spawn(async move { agent.run().await });

    let node = p
        .wait_for_node(&identity.node_id, |n| {
            n["online"] == true && !n["inventory"].is_null()
        })
        .await;
    assert_eq!(node["name"], "test-node");
    assert_eq!(node["agentVersion"], cha_node::AGENT_VERSION);
    assert_eq!(
        node["inventory"]["gpus"][0]["encoders"],
        json!(["h264", "hevc", "av1"])
    );
    assert!(node.get("publicKey").is_none(), "keys stay server-side");

    let (status, pong) = p
        .admin(
            "POST",
            &format!("/api/nodes/{}/ping", identity.node_id),
            None,
        )
        .await;
    assert_eq!(status, 200, "{pong}");
    assert!(pong["rttMs"].as_f64().unwrap() < 1000.0);

    // Removing the node hangs up on it, and the agent stops for good.
    let (status, _) = p
        .admin("DELETE", &format!("/api/nodes/{}", identity.node_id), None)
        .await;
    assert_eq!(status, 200);
    let stopped = tokio::time::timeout(Duration::from_secs(5), running)
        .await
        .expect("the agent stops once removed")
        .unwrap();
    assert!(
        stopped
            .unwrap_err()
            .to_string()
            .contains("no longer accepts")
    );

    let (status, _) = p
        .admin(
            "POST",
            &format!("/api/nodes/{}/ping", identity.node_id),
            None,
        )
        .await;
    assert_eq!(status, 409);
}

#[tokio::test]
async fn a_node_with_the_wrong_key_fails_the_challenge() {
    let p = portal().await;
    let identity = enroll(&p.url, &p.join_token().await, "real").await.unwrap();
    let impostor = enroll(&p.url, &p.join_token().await, "impostor")
        .await
        .unwrap();

    // The impostor's key, claiming the real node's id.
    let mut forged: Value = serde_json::to_value(&impostor).unwrap();
    forged["nodeId"] = json!(identity.node_id);
    let forged: Identity = serde_json::from_value(forged).unwrap();

    let mut welcomed = false;
    let ended = Agent::new(forged)
        .unwrap()
        .session(&mut welcomed)
        .await
        .unwrap();
    assert!(!welcomed);
    assert_eq!(ended.map(|c| c.code), Some(close::BAD_SIGNATURE));
}

#[tokio::test]
async fn admin_endpoints_need_an_admin() {
    let p = portal().await;
    let res = p
        .http
        .post(format!("{}/api/nodes/join-tokens", p.url))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status().as_u16(), 401);
    let res = p
        .http
        .get(format!("{}/api/nodes", p.url))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status().as_u16(), 401);
}

#[tokio::test]
async fn environments_launch_stop_fail_and_reconcile() {
    let p = portal().await;
    let identity = enroll(&p.url, &p.join_token().await, "gpu-box")
        .await
        .unwrap();
    // The node still runs one the portal never heard of: reconciling stops it.
    let runtime = FakeRuntime::new(&["leftover"]);
    let agent = Agent::new(identity.clone())
        .unwrap()
        .with_inventory(test_inventory)
        .with_runtime(runtime.clone());
    tokio::spawn(async move { agent.run().await });
    p.wait_for_node(&identity.node_id, |n| {
        n["online"] == true && !n["inventory"].is_null()
    })
    .await;

    let (status, catalog) = p.admin("GET", "/api/catalog", None).await;
    assert_eq!(status, 200);
    assert!(
        catalog
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["id"] == "chrome")
    );

    let (status, env) = p
        .admin(
            "POST",
            "/api/environments",
            Some(json!({ "templateId": "chrome" })),
        )
        .await;
    assert_eq!(status, 200, "{env}");
    assert_eq!(env["state"], "starting");
    let id = env["id"].as_str().unwrap().to_string();
    runtime.starts.add_permits(1);
    let env = p.wait_for_env(&id, |e| e["state"] == "running").await;
    assert_eq!(env["templateName"], "Google Chrome");
    assert_eq!(env["nodeName"], "gpu-box");
    assert_eq!(env["streamer"]["host"], "192.168.1.20");
    assert_eq!(env["streamer"]["httpPort"], 47000);
    let spec = runtime.started.lock().unwrap()[0].clone();
    assert_eq!(spec.id, id);
    assert_eq!(spec.image, "cha/env-chrome:dev");
    assert_eq!(spec.security, SecurityProfile::Browser);
    // Launched while reconciling may still be under way: it mustn't take the
    // new environment, missing from what the node had, for one it lost.
    runtime.wait_for_stop("leftover").await;

    // Connecting: the offer reaches the node with a media token the streamer
    // can check against the portal's key, for this environment and user.
    let offer = json!({ "type": "offer", "sdp": "v=0 browser" });
    let connect_path = format!("/api/environments/{id}/connect");
    let (status, reply) = p
        .admin(
            "POST",
            &connect_path,
            Some(json!({ "codec": "hevc", "offer": offer })),
        )
        .await;
    assert_eq!(status, 200, "{reply}");
    assert_eq!(reply["answer"]["sdp"], "v=0 fake");
    let connect = runtime.connects.lock().unwrap()[0].clone();
    assert_eq!(connect.environment_id, id);
    assert_eq!(connect.codec, "hevc");
    assert_eq!(connect.offer, offer);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let claims =
        cha_wire::verify_media_token(&spec.portal_key, &connect.media_token, &id, now).unwrap();
    assert_eq!(claims.role, "owner");
    assert!(claims.exp <= now + 60);
    let (status, _) = p
        .admin(
            "POST",
            &connect_path,
            Some(json!({ "codec": "vp9", "offer": offer })),
        )
        .await;
    assert_eq!(status, 400);

    let (status, _) = p
        .admin("DELETE", &format!("/api/environments/{id}"), None)
        .await;
    assert_eq!(status, 200);
    p.wait_for_env(&id, |e| e["state"] == "destroyed").await;
    assert!(runtime.stopped.lock().unwrap().contains(&id));
    let (status, _) = p
        .admin(
            "POST",
            &connect_path,
            Some(json!({ "codec": "hevc", "offer": offer })),
        )
        .await;
    assert_eq!(status, 409, "can't connect to a destroyed environment");

    // An app that dies is reported, and the environment fails with the reason.
    let (_, env) = p
        .admin(
            "POST",
            "/api/environments",
            Some(json!({ "templateId": "test-pattern" })),
        )
        .await;
    let id = env["id"].as_str().unwrap().to_string();
    runtime.starts.add_permits(1);
    p.wait_for_env(&id, |e| e["state"] == "running").await;
    runtime
        .exits
        .send(Exit {
            id: id.clone(),
            detail: "the app exited with code 1".into(),
            failed: true,
        })
        .unwrap();
    let env = p.wait_for_env(&id, |e| e["state"] == "failed").await;
    assert_eq!(env["detail"], "the app exited with code 1");

    let (status, list) = p.admin("GET", "/api/environments", None).await;
    assert_eq!(status, 200);
    assert_eq!(list.as_array().unwrap().len(), 2);
    let (status, _) = p
        .admin(
            "POST",
            "/api/environments",
            Some(json!({ "templateId": "nope" })),
        )
        .await;
    assert_eq!(status, 400);
}

// ---- App data ----

/// A portal with a node running `runtime`, and the admin's user id.
async fn portal_with_node(runtime: &Arc<FakeRuntime>) -> (Portal, String) {
    let p = portal().await;
    let identity = enroll(&p.url, &p.join_token().await, "gpu-box")
        .await
        .unwrap();
    let agent = Agent::new(identity.clone())
        .unwrap()
        .with_inventory(test_inventory)
        .with_runtime(runtime.clone());
    tokio::spawn(async move { agent.run().await });
    p.wait_for_node(&identity.node_id, |n| {
        n["online"] == true && !n["inventory"].is_null()
    })
    .await;
    let (_, me) = p.admin("GET", "/api/me", None).await;
    let user = me["id"].as_str().unwrap().to_string();
    (p, user)
}

impl Portal {
    /// Launches `template` and lets it start; returns the spec the node got.
    async fn launch(
        &self,
        runtime: &FakeRuntime,
        template: &str,
    ) -> (u16, Value, Option<EnvironmentSpec>) {
        let before = runtime.started.lock().unwrap().len();
        let (status, env) = self
            .admin(
                "POST",
                "/api/environments",
                Some(json!({ "templateId": template })),
            )
            .await;
        if status != 200 {
            return (status, env, None);
        }
        runtime.starts.add_permits(1);
        let id = env["id"].as_str().unwrap().to_string();
        self.wait_for_env(&id, |e| e["state"] == "running").await;
        let spec = runtime.started.lock().unwrap()[before].clone();
        (status, env, Some(spec))
    }

    async fn stop(&self, id: &str) {
        self.admin("DELETE", &format!("/api/environments/{id}"), None)
            .await;
        self.wait_for_env(id, |e| e["state"] == "destroyed").await;
    }
}

#[tokio::test]
async fn the_node_reports_where_it_keeps_app_data() {
    let runtime = FakeRuntime::new(&[]);
    let (p, _) = portal_with_node(&runtime).await;
    let (status, nodes) = p.admin("GET", "/api/nodes", None).await;
    assert_eq!(status, 200);
    assert_eq!(nodes[0]["inventory"]["dataRoot"], "/srv/cha-portal");
    assert_eq!(
        nodes[0]["inventory"]["sharedDirs"]["steam"],
        "/mnt/games/steam"
    );

    // So the settings say where the data is, and Steam's library is the NAS's.
    let (_, user) = p.admin("GET", "/api/storage", None).await;
    assert_eq!(user["root"], "/srv/cha-portal");
    let steam = user["apps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["template"] == "steam")
        .unwrap();
    assert_eq!(steam["sharedPath"], "/mnt/games/steam");
    let chrome = user["apps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["template"] == "chrome")
        .unwrap();
    assert!(chrome.get("sharedPath").is_none(), "kept under the root");
    let (_, admin) = p.admin("GET", "/api/admin/storage", None).await;
    assert_eq!(admin["root"], "/srv/cha-portal");
    let steam = admin["apps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["template"] == "steam")
        .unwrap();
    assert_eq!(steam["sharedPath"], "/mnt/games/steam");
    let (_, updated) = p
        .admin(
            "PUT",
            "/api/admin/storage/steam",
            Some(json!({ "sharedAccess": "read" })),
        )
        .await;
    assert_eq!(updated["sharedPath"], "/mnt/games/steam");
}

#[tokio::test]
async fn launches_carry_the_users_directories_and_one_environment_per_home() {
    let runtime = FakeRuntime::new(&[]);
    let (p, user) = portal_with_node(&runtime).await;

    // Steam keeps the user's data and shares its library, from the catalog.
    let (status, env, spec) = p.launch(&runtime, "steam").await;
    assert_eq!(status, 200, "{env}");
    let spec = spec.unwrap();
    assert_eq!(spec.home, None, "not the old way of keeping a home");
    assert_eq!(
        (spec.owner.as_str(), spec.template.as_str()),
        (user.as_str(), "steam")
    );
    let storage = spec.storage.clone().expect("steam keeps and shares");
    assert_eq!(storage.home, Some(format!("users/{user}/steam")));
    assert_eq!(
        storage.legacy_volume,
        Some(cha_wire::home_volume_name(&user, "steam"))
    );
    let shared = storage.shared.clone().unwrap();
    assert_eq!(shared.path, "shared/steam");
    assert!(shared.writable);
    assert_eq!(
        shared.per_user,
        ["steamapps/compatdata", "steamapps/shadercache"]
    );
    assert_eq!(storage.check(&user, "steam"), Ok(()));

    // Two would share one home.
    let (status, again) = p
        .admin(
            "POST",
            "/api/environments",
            Some(json!({ "templateId": "steam" })),
        )
        .await;
    assert_eq!(status, 409, "{again}");
    assert_eq!(again["error"], "already_running");
    let (_, listed) = p.admin("GET", "/api/storage", None).await;
    let steam = listed["apps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["template"] == "steam")
        .unwrap();
    assert_eq!(steam["live"], true);

    // The setting is shut while it runs, and then it is the user's call: with
    // nothing kept (and a library whose per-user parts need a home), a Steam
    // gets no storage at all, and any number of them can run.
    let (status, blocked) = p
        .admin(
            "PUT",
            "/api/storage/steam",
            Some(json!({ "persistent": false })),
        )
        .await;
    assert_eq!((status, blocked["error"].as_str()), (409, Some("live")));
    p.stop(env["id"].as_str().unwrap()).await;
    let (status, off) = p
        .admin(
            "PUT",
            "/api/storage/steam",
            Some(json!({ "persistent": false })),
        )
        .await;
    assert_eq!(status, 200, "{off}");
    let (_, _, first) = p.launch(&runtime, "steam").await;
    assert_eq!(first.unwrap().storage, None);
    let (status, _, second) = p.launch(&runtime, "steam").await;
    assert_eq!(status, 200, "no home to share, so no limit");
    assert_eq!(second.unwrap().storage, None);

    // Chrome keeps and shares nothing, until the admin and the user say so.
    let (_, first, chrome) = p.launch(&runtime, "chrome").await;
    assert_eq!(chrome.unwrap().storage, None);
    p.stop(first["id"].as_str().unwrap()).await;
    let (status, _) = p
        .admin(
            "PUT",
            "/api/admin/storage/chrome",
            Some(json!({ "defaultPersistent": true, "sharedAccess": "read" })),
        )
        .await;
    assert_eq!(status, 200);
    let (_, _, chrome) = p.launch(&runtime, "chrome").await;
    let storage = chrome.unwrap().storage.expect("kept and shared now");
    assert_eq!(storage.home, Some(format!("users/{user}/chrome")));
    let shared = storage.shared.unwrap();
    assert_eq!(
        (shared.path.as_str(), shared.writable),
        ("shared/chrome", false)
    );
    assert!(shared.per_user.is_empty());
    // And now Chrome keeps one home too.
    let (status, again) = p
        .admin(
            "POST",
            "/api/environments",
            Some(json!({ "templateId": "chrome" })),
        )
        .await;
    assert_eq!(
        (status, again["error"].as_str()),
        (409, Some("already_running"))
    );
}

#[tokio::test]
async fn a_node_that_predates_app_data_isnt_sent_any() {
    let runtime = FakeRuntime::without_storage();
    let (p, _) = portal_with_node(&runtime).await;
    let (_, nodes) = p.admin("GET", "/api/nodes", None).await;
    assert!(nodes[0]["inventory"].get("dataRoot").is_none());

    // Steam would keep nothing and share nothing: refused, not run without.
    let (status, body, _) = p.launch(&runtime, "steam").await;
    assert_eq!(status, 409, "{body}");
    assert_eq!(body["error"], "node_needs_update");
    assert!(runtime.started.lock().unwrap().is_empty());
    // What needs no storage still runs.
    let (status, _, spec) = p.launch(&runtime, "chrome").await;
    assert_eq!(status, 200);
    assert_eq!(spec.unwrap().storage, None);
    // The settings fall back to the default root.
    let (_, user) = p.admin("GET", "/api/storage", None).await;
    assert_eq!(user["root"], "/srv/cha-portal");
}

#[tokio::test]
async fn a_reset_reaches_the_node_and_its_refusal_comes_back() {
    let runtime = FakeRuntime::new(&[]);
    let (p, user) = portal_with_node(&runtime).await;

    let (status, reply) = p.admin("POST", "/api/storage/steam/reset", None).await;
    assert_eq!(status, 200, "{reply}");
    assert_eq!(reply["template"], "steam");
    assert_eq!(
        runtime.deleted.lock().unwrap().clone(),
        [(user.clone(), "steam".to_string())]
    );

    // The node's own reason, as a 502.
    *runtime.refuse_delete.lock().unwrap() =
        Some("an environment of theirs for this app is running on this node".into());
    let (status, reply) = p.admin("POST", "/api/storage/steam/reset", None).await;
    assert_eq!(status, 502, "{reply}");
    assert_eq!(reply["error"], "node_error");
    assert!(
        reply["message"]
            .as_str()
            .unwrap()
            .contains("running on this node")
    );
    assert_eq!(runtime.deleted.lock().unwrap().len(), 1);

    // Not while one is live, and audited when it happens.
    *runtime.refuse_delete.lock().unwrap() = None;
    let (_, _, _) = p.launch(&runtime, "steam").await;
    let (status, reply) = p.admin("POST", "/api/storage/steam/reset", None).await;
    assert_eq!((status, reply["error"].as_str()), (409, Some("live")));
    assert_eq!(runtime.deleted.lock().unwrap().len(), 1);
    let (_, audit) = p.admin("GET", "/api/audit", None).await;
    let reset: Vec<&Value> = audit
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["action"] == "storage.reset")
        .collect();
    assert_eq!(reset.len(), 1);
    assert_eq!(reset[0]["target"], "steam");
}

#[tokio::test]
async fn what_the_node_says_while_starting_shows_until_it_runs() {
    let runtime = FakeRuntime::new(&[]);
    let (p, _) = portal_with_node(&runtime).await;
    let (_, env) = p
        .admin(
            "POST",
            "/api/environments",
            Some(json!({ "templateId": "steam" })),
        )
        .await;
    let id = env["id"].as_str().unwrap().to_string();
    runtime
        .progress
        .send(Progress {
            id: id.clone(),
            detail: "Moving your files to the new storage (this happens once)".into(),
        })
        .unwrap();
    let env = p.wait_for_env(&id, |e| !e["detail"].is_null()).await;
    assert_eq!(env["state"], "starting");
    assert!(
        env["detail"]
            .as_str()
            .unwrap()
            .starts_with("Moving your files")
    );
    // Someone else's id, or an environment that isn't starting, is not news.
    runtime
        .progress
        .send(Progress {
            id: "no-such-environment".into(),
            detail: "x".into(),
        })
        .unwrap();
    runtime.starts.add_permits(1);
    let env = p.wait_for_env(&id, |e| e["state"] == "running").await;
    assert!(env["detail"].is_null(), "the note goes when it runs");
    runtime
        .progress
        .send(Progress {
            id: id.clone(),
            detail: "late".into(),
        })
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let (_, env) = p
        .admin("GET", &format!("/api/environments/{id}"), None)
        .await;
    assert!(env["detail"].is_null());
}

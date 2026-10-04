//! The agent against a real portal on a random port: enrollment, the signed
//! handshake, inventory, a request through the node channel, removal, and the
//! environment lifecycle (with a fake runtime standing in for Docker).

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use cha_control::{AppState, Config, app, db};
use cha_node::environments::{Connect, Exit, Runtime};
use cha_node::{Agent, Identity, enroll, init_tls};
use cha_wire::{EnvironmentSpec, Gpu, Inventory, SecurityProfile, StreamerEndpoint, close};
use futures_util::future::BoxFuture;
use serde_json::{Value, json};
use tokio::sync::broadcast;

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
}

impl FakeRuntime {
    fn new(running: &[&str]) -> Arc<Self> {
        Arc::new(Self {
            started: Mutex::default(),
            connects: Mutex::default(),
            stopped: Mutex::default(),
            running: Mutex::new(running.iter().map(|s| s.to_string()).collect()),
            exits: broadcast::channel(8).0,
        })
    }
}

impl Runtime for FakeRuntime {
    fn start(&self, spec: EnvironmentSpec) -> BoxFuture<'_, Result<StreamerEndpoint>> {
        Box::pin(async move {
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
    let env = p.wait_for_env(&id, |e| e["state"] == "running").await;
    assert_eq!(env["templateName"], "Google Chrome");
    assert_eq!(env["nodeName"], "gpu-box");
    assert_eq!(env["streamer"]["host"], "192.168.1.20");
    assert_eq!(env["streamer"]["httpPort"], 47000);
    let spec = runtime.started.lock().unwrap()[0].clone();
    assert_eq!(spec.id, id);
    assert_eq!(spec.image, "cha/env-chrome:dev");
    assert_eq!(spec.security, SecurityProfile::Browser);
    assert!(
        runtime
            .stopped
            .lock()
            .unwrap()
            .contains(&"leftover".to_string())
    );

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

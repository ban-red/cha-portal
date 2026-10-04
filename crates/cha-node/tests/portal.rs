//! The agent against a real portal on a random port: enrollment, the signed
//! handshake, inventory, a request through the node channel, and removal.

use std::net::SocketAddr;
use std::time::Duration;

use cha_control::{AppState, Config, app, db};
use cha_node::{Agent, Identity, enroll, init_tls};
use cha_wire::{Gpu, Inventory, close};
use serde_json::{Value, json};

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

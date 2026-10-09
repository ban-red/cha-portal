//! Internet share links (ADR 0022): the tunnel, the guest-only listener and
//! media over a WebSocket through the portal, against a fake `cloudflared`
//! (a shell script that prints what the real one does) and a hand-driven node.

use std::net::SocketAddr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode, header};
use cha_control::tunnel::TunnelConfig;
use cha_control::{AppState, Config, app, db, guest_app};
use cha_wire::{NodeKey, NodeRequest, NodeResponse, ToNode, ToPortal};
use futures_util::{SinkExt, StreamExt};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tower::ServiceExt;

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

const QUICK_HOST: &str = "quiet-fake-words.trycloudflare.com";

/// A `cloudflared` that records how it was run and says what the real one
/// does once it is serving.
fn fake_cloudflared(dir: &Path) -> PathBuf {
    let path = dir.join("cloudflared");
    std::fs::write(
        &path,
        r#"#!/bin/sh
echo "$@" > "$0.args"
echo "TUNNEL_TOKEN=${TUNNEL_TOKEN-unset}" > "$0.env"
echo started >> "$0.starts"
echo "2026-10-08T10:00:00Z INF Requesting new quick Tunnel on trycloudflare.com..." >&2
echo "2026-10-08T10:00:01Z INF |  https://quiet-fake-words.trycloudflare.com  |" >&2
echo "2026-10-08T10:00:02Z INF Registered tunnel connection connIndex=0" >&2
exec sleep 300
"#,
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

struct Portal {
    state: AppState,
    main: Router,
    guest: Router,
    main_addr: SocketAddr,
    guest_addr: SocketAddr,
    db: sqlx::SqlitePool,
    dir: tempfile::TempDir,
}

struct Reply {
    status: StatusCode,
    body: Value,
}

async fn portal(tunnel: impl FnOnce(&Path) -> TunnelConfig) -> Portal {
    let dir = tempfile::tempdir().unwrap();
    let config = Config {
        listen: "127.0.0.1:0".parse().unwrap(),
        database: dir.path().join("cha.db"),
        web_dir: None,
        secure_cookies: false,
        session_days: 14,
        ice: Default::default(),
        dev_login: true,
        discover_nodes: false,
        public_url: None,
        tunnel: tunnel(dir.path()),
    };
    let pool = db::open(&config.database).await.unwrap();
    let state = AppState::new(config, pool.clone()).await.unwrap();
    let (main, guest) = (app(state.clone()), guest_app(state.clone()));
    let mut addrs = Vec::new();
    for router in [main.clone(), guest.clone()] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        addrs.push(listener.local_addr().unwrap());
        tokio::spawn(async move {
            axum::serve(
                listener,
                router.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap();
        });
    }
    Portal {
        state,
        main,
        guest,
        main_addr: addrs[0],
        guest_addr: addrs[1],
        db: pool,
        dir,
    }
}

/// A portal whose tunnel is the fake `cloudflared`.
async fn quick() -> Portal {
    portal(|dir| TunnelConfig {
        cloudflared: fake_cloudflared(dir).display().to_string(),
        ..Default::default()
    })
    .await
}

async fn send(
    router: &Router,
    method: &str,
    path: &str,
    cookie: Option<&str>,
    headers: &[(&str, &str)],
    body: Option<Value>,
) -> (Reply, Option<String>) {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .extension(ConnectInfo("127.0.0.1:5000".parse::<SocketAddr>().unwrap()));
    if let Some(cookie) = cookie {
        req = req.header(header::COOKIE, cookie);
    }
    for (name, value) in headers {
        req = req.header(*name, *value);
    }
    let req = match body {
        Some(body) => req
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string())),
        None => req.body(Body::empty()),
    }
    .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let cookie = res
        .headers()
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .map(str::to_string);
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (Reply { status, body }, cookie)
}

impl Portal {
    async fn call(
        &self,
        method: &str,
        path: &str,
        cookie: Option<&str>,
        body: Option<Value>,
    ) -> Reply {
        send(&self.main, method, path, cookie, &[], body).await.0
    }

    /// A request on the guest listener, as the tunnel forwards it.
    async fn guest_call(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: Option<Value>,
    ) -> Reply {
        send(&self.guest, method, path, None, headers, body).await.0
    }

    /// Signs in as the dev admin and returns its cookie and id.
    async fn admin(&self) -> (String, String) {
        let (reply, cookie) = send(
            &self.main,
            "POST",
            "/api/auth/dev-login",
            None,
            &[],
            Some(json!({})),
        )
        .await;
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
        (
            cookie.unwrap(),
            reply.body["id"].as_str().unwrap().to_string(),
        )
    }

    /// A node with `key`, whose inventory says `relay`, and a running
    /// environment on it owned by `owner`.
    async fn environment(&self, owner: &str, key: &NodeKey, relay: bool) -> (String, String) {
        let node_id = db::new_id();
        let inventory = serde_json::to_string(&cha_wire::Inventory {
            relay,
            ..Default::default()
        })
        .unwrap();
        sqlx::query(
            "INSERT INTO nodes (id, name, public_key, enrolled_at, inventory) VALUES (?, 'box', ?, 0, ?)",
        )
        .bind(&node_id)
        .bind(key.public_b64())
        .bind(inventory)
        .execute(&self.db)
        .await
        .unwrap();
        let env = db::new_id();
        db::insert_environment(&self.db, &env, owner, "chrome", &node_id, None, "running")
            .await
            .unwrap();
        (node_id, env)
    }

    async fn share(&self, cookie: &str, env: &str, wan: bool) -> Reply {
        self.call(
            "POST",
            &format!("/api/environments/{env}/shares"),
            Some(cookie),
            Some(json!({ "role": "viewer", "wan": wan })),
        )
        .await
    }

    fn starts(&self) -> usize {
        std::fs::read_to_string(self.dir.path().join("cloudflared.starts"))
            .map(|s| s.lines().count())
            .unwrap_or(0)
    }
}

fn token_of(url: &str) -> &str {
    url.rsplit('/').next().unwrap()
}

fn key() -> NodeKey {
    let mut secret = [0u8; 32];
    getrandom::fill(&mut secret).unwrap();
    NodeKey::from_secret(secret)
}

// ---- The tunnel and internet links ----

#[tokio::test]
async fn an_internet_link_is_absolute_on_the_quick_tunnels_hostname() {
    let p = quick().await;
    let (admin, admin_id) = p.admin().await;
    let (_, env) = p.environment(&admin_id, &key(), true).await;

    let before = p.call("GET", "/api/tunnel", Some(&admin), None).await;
    assert_eq!(
        before.body,
        json!({ "mode": "quick", "state": "stopped", "url": null, "error": null })
    );

    let made = p.share(&admin, &env, true).await;
    assert_eq!(made.status, StatusCode::OK, "{}", made.body);
    let url = made.body["url"].as_str().unwrap();
    assert!(
        url.starts_with(&format!("https://{QUICK_HOST}/s/")),
        "url was {url}"
    );
    assert_eq!(made.body["wan"], true);

    // A link not marked wan stays a path.
    let lan = p.share(&admin, &env, false).await;
    assert!(lan.body["url"].as_str().unwrap().starts_with("/s/"));
    assert_eq!(lan.body["wan"], false);

    let listed = p
        .call(
            "GET",
            &format!("/api/environments/{env}/shares"),
            Some(&admin),
            None,
        )
        .await;
    let wans: Vec<bool> = listed
        .body
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["wan"].as_bool().unwrap())
        .collect();
    assert_eq!(wans.len(), 2);
    assert!(wans.contains(&true) && wans.contains(&false));

    let up = p.call("GET", "/api/tunnel", Some(&admin), None).await;
    assert_eq!(
        up.body,
        json!({ "mode": "quick", "state": "up", "url": format!("https://{QUICK_HOST}"), "error": null })
    );
    // It points at the guest listener, not the portal.
    let args = std::fs::read_to_string(p.dir.path().join("cloudflared.args")).unwrap();
    assert_eq!(
        args.trim(),
        "tunnel --no-autoupdate --url http://127.0.0.1:7680"
    );
    // The status needs a signed-in user.
    assert_eq!(
        p.call("GET", "/api/tunnel", None, None).await.status,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn callers_share_one_start_and_the_tunnel_restarts_after_a_stop() {
    let p = quick().await;
    let (admin, admin_id) = p.admin().await;
    let (_, env) = p.environment(&admin_id, &key(), true).await;
    let made = futures_util::future::join_all((0..3).map(|_| p.share(&admin, &env, true))).await;
    assert!(made.iter().all(|r| r.status == StatusCode::OK));
    assert_eq!(p.starts(), 1, "three callers, one cloudflared");

    p.state.tunnel.stop();
    let gone = p.call("GET", "/api/tunnel", Some(&admin), None).await;
    assert_eq!(gone.body["state"], "stopped");
    assert_eq!(p.share(&admin, &env, true).await.status, StatusCode::OK);
    assert_eq!(p.starts(), 2);
}

#[tokio::test]
async fn a_restarted_quick_tunnel_ends_the_links_made_on_the_old_one() {
    let p = quick().await;
    let (admin, admin_id) = p.admin().await;
    let (_, env) = p.environment(&admin_id, &key(), true).await;
    let old = p.share(&admin, &env, true).await;
    let old_token = token_of(old.body["url"].as_str().unwrap()).to_string();
    // The tunnel went down and came back (a new hostname on the real thing).
    p.state.tunnel.stop();
    sqlx::query("UPDATE shares SET created_at = created_at - 100")
        .execute(&p.db)
        .await
        .unwrap();
    let fresh = p.share(&admin, &env, true).await;
    assert_eq!(fresh.status, StatusCode::OK);
    let gone = p
        .guest_call("GET", &format!("/api/shares/{old_token}"), &[], None)
        .await;
    assert_eq!(gone.status, StatusCode::NOT_FOUND);
    let live = p
        .guest_call(
            "GET",
            &format!(
                "/api/shares/{}",
                token_of(fresh.body["url"].as_str().unwrap())
            ),
            &[],
            None,
        )
        .await;
    assert_eq!(live.status, StatusCode::OK);
}

#[tokio::test]
async fn a_named_tunnel_has_a_stable_hostname_and_keeps_its_token_in_the_environment() {
    let p = portal(|dir| TunnelConfig {
        cloudflared: fake_cloudflared(dir).display().to_string(),
        token: Some("eyJ-secret-tunnel-token".into()),
        hostname: Some("play.example.com".into()),
        ..Default::default()
    })
    .await;
    let (admin, admin_id) = p.admin().await;
    let (_, env) = p.environment(&admin_id, &key(), true).await;
    let made = p.share(&admin, &env, true).await;
    assert_eq!(made.status, StatusCode::OK, "{}", made.body);
    assert!(
        made.body["url"]
            .as_str()
            .unwrap()
            .starts_with("https://play.example.com/s/")
    );
    let args = std::fs::read_to_string(p.dir.path().join("cloudflared.args")).unwrap();
    assert_eq!(args.trim(), "tunnel --no-autoupdate run");
    let env_file = std::fs::read_to_string(p.dir.path().join("cloudflared.env")).unwrap();
    assert_eq!(env_file.trim(), "TUNNEL_TOKEN=eyJ-secret-tunnel-token");
    let status = p.call("GET", "/api/tunnel", Some(&admin), None).await;
    assert_eq!(status.body["mode"], "named");
    assert_eq!(status.body["url"], "https://play.example.com");
    assert!(!status.body.to_string().contains("secret-tunnel-token"));
    assert!(!format!("{:?}", p.state.config).contains("secret-tunnel-token"));
}

#[tokio::test]
async fn an_internet_link_needs_the_tunnel_to_be_on_and_to_start() {
    let off = portal(|_| TunnelConfig {
        enabled: false,
        ..Default::default()
    })
    .await;
    let (admin, admin_id) = off.admin().await;
    let (_, env) = off.environment(&admin_id, &key(), true).await;
    let refused = off.share(&admin, &env, true).await;
    assert_eq!(refused.status, StatusCode::CONFLICT);
    assert_eq!(refused.body["error"], "tunnel_off");
    let status = off.call("GET", "/api/tunnel", Some(&admin), None).await;
    assert_eq!(status.body["mode"], "off");
    // Ordinary links are unaffected.
    assert_eq!(off.share(&admin, &env, false).await.status, StatusCode::OK);

    // No such binary.
    let missing = portal(|dir| TunnelConfig {
        cloudflared: dir.join("nope").display().to_string(),
        ..Default::default()
    })
    .await;
    let (admin, admin_id) = missing.admin().await;
    let (_, env) = missing.environment(&admin_id, &key(), true).await;
    let failed = missing.share(&admin, &env, true).await;
    assert_eq!(failed.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(failed.body["error"], "tunnel_failed");
    assert!(
        failed.body["message"]
            .as_str()
            .unwrap()
            .contains("can't run")
    );

    // One that exits with an error: its last words are the reason.
    let dies = portal(|dir| TunnelConfig {
        cloudflared: script(dir, "dies", "echo 'ERR no route to cloudflare' >&2\nexit 1")
            .display()
            .to_string(),
        ..Default::default()
    })
    .await;
    let (admin, admin_id) = dies.admin().await;
    let (_, env) = dies.environment(&admin_id, &key(), true).await;
    let failed = dies.share(&admin, &env, true).await;
    assert_eq!(failed.status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        failed.body["message"]
            .as_str()
            .unwrap()
            .contains("no route to cloudflare"),
        "{}",
        failed.body
    );
    let status = dies.call("GET", "/api/tunnel", Some(&admin), None).await;
    assert_eq!(status.body["state"], "failed");
    assert!(status.body["error"].is_string());
}

// ---- The guest listener ----

#[tokio::test]
async fn the_guest_listener_serves_internet_links_and_nothing_else() {
    let p = quick().await;
    let (admin, admin_id) = p.admin().await;
    let (_, env) = p.environment(&admin_id, &key(), true).await;
    let wan = p.share(&admin, &env, true).await;
    let wan_token = token_of(wan.body["url"].as_str().unwrap()).to_string();
    let lan = p.share(&admin, &env, false).await;
    let lan_token = token_of(lan.body["url"].as_str().unwrap()).to_string();

    // The internet link works on both listeners, the LAN one on the main only.
    let guest = p
        .guest_call("GET", &format!("/api/shares/{wan_token}"), &[], None)
        .await;
    assert_eq!(guest.status, StatusCode::OK, "{}", guest.body);
    assert_eq!(guest.body["wan"], true);
    assert_eq!(guest.body["turn"], false);
    let main = p
        .call("GET", &format!("/api/shares/{lan_token}"), None, None)
        .await;
    assert_eq!(main.status, StatusCode::OK);
    assert_eq!(main.body["wan"], false);
    for path in [
        format!("/api/shares/{lan_token}"),
        format!("/api/shares/{lan_token}/ice"),
    ] {
        let reply = p.guest_call("GET", &path, &[], None).await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND, "{path}");
        assert_eq!(reply.body["error"], "unknown_share");
    }
    let connect = p
        .guest_call(
            "POST",
            &format!("/api/shares/{lan_token}/connect"),
            &[],
            Some(json!({ "codec": "h264", "transport": "websocket" })),
        )
        .await;
    assert_eq!(connect.status, StatusCode::NOT_FOUND);

    // Guests get ICE servers (none configured here, so an empty list).
    let ice = p
        .guest_call("GET", &format!("/api/shares/{wan_token}/ice"), &[], None)
        .await;
    assert_eq!(ice.body, json!({ "iceServers": [] }));

    // Everything else under /api is a 404, whatever the method or the cookie.
    for (method, path) in [
        ("POST", "/api/auth/login"),
        ("POST", "/api/auth/dev-login"),
        ("GET", "/api/auth/dev-accounts"),
        ("GET", "/api/me"),
        ("GET", "/api/setup"),
        ("GET", "/api/health"),
        ("GET", "/api/ice"),
        ("GET", "/api/tunnel"),
        ("GET", "/api/nodes"),
        ("GET", "/api/users"),
        ("GET", "/api/audit"),
        ("GET", "/api/node/connect"),
        ("GET", "/api/node/relay/abc"),
        ("POST", "/api/node/enroll"),
        ("GET", "/api/environments"),
        ("GET", &format!("/api/environments/{env}/shares")),
        ("DELETE", &format!("/api/environments/{env}/shares/x")),
        ("GET", "/api"),
        ("GET", "/api/"),
    ] {
        let (reply, _) = send(&p.guest, method, path, Some(&admin), &[], None).await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND, "{method} {path}");
    }
    // The same paths do exist on the main listener.
    assert_eq!(
        p.call("GET", "/api/health", None, None).await.status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn the_guest_listener_rate_limits_by_the_tunnels_client_address() {
    let p = quick().await;
    let probe = |ip: &'static str| {
        let p = &p;
        async move {
            p.guest_call(
                "GET",
                "/api/shares/guess",
                &[("cf-connecting-ip", ip), ("x-forwarded-for", "127.0.0.1")],
                None,
            )
            .await
            .status
        }
    };
    for _ in 0..30 {
        assert_eq!(probe("203.0.113.1").await, StatusCode::NOT_FOUND);
    }
    assert_eq!(probe("203.0.113.1").await, StatusCode::TOO_MANY_REQUESTS);
    // Another guest behind the same tunnel (the same loopback peer) is fine.
    assert_eq!(probe("203.0.113.2").await, StatusCode::NOT_FOUND);
    // A header that isn't an address doesn't mint a fresh bucket.
    for _ in 0..30 {
        p.guest_call(
            "GET",
            "/api/shares/guess",
            &[("cf-connecting-ip", "nonsense")],
            None,
        )
        .await;
    }
    let spoof = p
        .guest_call(
            "GET",
            "/api/shares/guess",
            &[("cf-connecting-ip", "nonsense")],
            None,
        )
        .await;
    assert_eq!(spoof.status, StatusCode::TOO_MANY_REQUESTS);
    // The main listener ignores the header: a fresh address in it doesn't help.
    let (reply, _) = send(
        &p.main,
        "GET",
        "/api/shares/guess",
        None,
        &[("cf-connecting-ip", "203.0.113.9")],
        None,
    )
    .await;
    assert_eq!(
        reply.status,
        StatusCode::TOO_MANY_REQUESTS,
        "counted against the peer, whose allowance the loop above used up"
    );
}

// ---- Media over a WebSocket ----

/// Connects as the enrolled node `id` with `key`.
async fn node_join(addr: SocketAddr, id: &str, key: &NodeKey) -> Socket {
    async fn next(ws: &mut Socket) -> ToNode {
        loop {
            let msg = tokio::time::timeout(Duration::from_secs(5), ws.next())
                .await
                .expect("the portal said nothing for five seconds")
                .expect("the connection ended")
                .expect("a websocket error");
            if let Message::Text(text) = msg {
                return serde_json::from_str(&text).unwrap();
            }
        }
    }
    let (mut ws, _) =
        tokio_tungstenite::connect_async(format!("ws://{addr}{}", cha_wire::CONNECT_PATH))
            .await
            .unwrap();
    let ToNode::Challenge { nonce, protocol } = next(&mut ws).await else {
        panic!("expected a challenge");
    };
    let hello = ToPortal::Hello {
        node_id: id.into(),
        signature: key.sign_b64(&cha_wire::hello_message(&nonce, id)),
        agent_version: "test".into(),
        protocol,
    };
    ws.send(Message::text(serde_json::to_string(&hello).unwrap()))
        .await
        .unwrap();
    next(&mut ws).await; // the welcome
    ws
}

async fn node_request(ws: &mut Socket) -> (u64, NodeRequest) {
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("the portal asked the node nothing for five seconds")
            .expect("the connection ended")
            .expect("a websocket error");
        if let Message::Text(text) = msg
            && let ToNode::Request { id, request } = serde_json::from_str(&text).unwrap()
        {
            return (id, request);
        }
    }
}

async fn node_respond(ws: &mut Socket, id: u64, result: Result<NodeResponse, String>) {
    let msg = ToPortal::Response { id, result };
    ws.send(Message::text(serde_json::to_string(&msg).unwrap()))
        .await
        .unwrap();
}

/// Dials the portal's relay endpoint as a node would.
async fn dial_relay(
    addr: SocketAddr,
    relay_id: &str,
    node_id: &str,
    signature: &str,
) -> Result<Socket, StatusCode> {
    let mut req = format!("ws://{addr}{}/{relay_id}", cha_wire::RELAY_PATH)
        .into_client_request()
        .unwrap();
    req.headers_mut()
        .insert(cha_wire::RELAY_NODE_HEADER, node_id.parse().unwrap());
    req.headers_mut()
        .insert(cha_wire::RELAY_SIGNATURE_HEADER, signature.parse().unwrap());
    match tokio_tungstenite::connect_async(req).await {
        Ok((ws, _)) => Ok(ws),
        Err(tokio_tungstenite::tungstenite::Error::Http(res)) => Err(res.status()),
        Err(other) => panic!("unexpected dial error: {other}"),
    }
}

async fn open_ticket(addr: SocketAddr, url: &str) -> Result<Socket, StatusCode> {
    match tokio_tungstenite::connect_async(format!("ws://{addr}{url}")).await {
        Ok((ws, _)) => Ok(ws),
        Err(tokio_tungstenite::tungstenite::Error::Http(res)) => Err(res.status()),
        Err(other) => panic!("unexpected error: {other}"),
    }
}

async fn ws_connect(p: &Portal, token: &str, codec: &str) -> Reply {
    p.guest_call(
        "POST",
        &format!("/api/shares/{token}/connect"),
        &[("cf-connecting-ip", "198.51.100.7")],
        Some(json!({ "codec": codec, "transport": "websocket" })),
    )
    .await
}

#[tokio::test]
async fn a_node_without_relay_support_is_outdated_and_pyrowave_is_refused() {
    let p = quick().await;
    let (admin, admin_id) = p.admin().await;
    let (_, old) = p.environment(&admin_id, &key(), false).await;
    let (_, new) = p.environment(&admin_id, &key(), true).await;
    let old_share = p.share(&admin, &old, true).await;
    let old_token = token_of(old_share.body["url"].as_str().unwrap());

    let outdated = ws_connect(&p, old_token, "h264").await;
    assert_eq!(outdated.status, StatusCode::CONFLICT);
    assert_eq!(outdated.body["error"], "node_outdated");
    assert!(
        outdated.body["message"]
            .as_str()
            .unwrap()
            .contains("needs updating")
    );

    let new_share = p.share(&admin, &new, true).await;
    let new_token = token_of(new_share.body["url"].as_str().unwrap());
    let pyro = ws_connect(&p, new_token, "pyrowave420").await;
    assert_eq!(pyro.status, StatusCode::BAD_REQUEST);
    assert_eq!(pyro.body["error"], "bad_codec");
    let bad = ws_connect(&p, new_token, "vp9").await;
    assert_eq!(bad.body["error"], "bad_codec");

    // Without a relay-capable node there's no ticket to open, either.
    let ok = ws_connect(&p, new_token, "av1").await;
    assert_eq!(ok.status, StatusCode::OK, "{}", ok.body);
    assert_eq!(ok.body["transport"], "websocket");
    assert_eq!(ok.body["codec"], "av1");
    let url = ok.body["urls"][0].as_str().unwrap();
    assert!(url.starts_with("/api/media/") && url.len() > "/api/media/".len() + 40);
}

#[tokio::test]
async fn a_ticket_is_unknown_to_a_plain_request_and_to_a_guess() {
    let p = quick().await;
    let missing = open_ticket(p.guest_addr, "/api/media/nope").await;
    assert_eq!(missing.err(), Some(StatusCode::NOT_FOUND));
    // Not a WebSocket upgrade: refused, and the ticket stays good.
    let (admin, admin_id) = p.admin().await;
    let (_, env) = p.environment(&admin_id, &key(), true).await;
    let made = p.share(&admin, &env, true).await;
    let ok = ws_connect(&p, token_of(made.body["url"].as_str().unwrap()), "h264").await;
    let url = ok.body["urls"][0].as_str().unwrap().to_string();
    let plain = p.guest_call("GET", &url, &[], None).await;
    assert_ne!(plain.status, StatusCode::NOT_FOUND);
    assert!(plain.status.is_client_error());
    // Still a 101 afterwards (the node isn't connected, so it then closes).
    let ws = open_ticket(p.guest_addr, &url).await;
    assert!(
        ws.is_ok(),
        "the ticket survived a request that wasn't an upgrade"
    );
}

#[tokio::test]
async fn messages_pass_between_the_guest_and_the_node_untouched() {
    let p = quick().await;
    let (admin, admin_id) = p.admin().await;
    let key = key();
    let (node_id, env) = p.environment(&admin_id, &key, true).await;
    let mut node = node_join(p.main_addr, &node_id, &key).await;
    let made = p.share(&admin, &env, true).await;
    let token = token_of(made.body["url"].as_str().unwrap()).to_string();

    let ok = ws_connect(&p, &token, "hevc").await;
    assert_eq!(ok.status, StatusCode::OK, "{}", ok.body);
    let ticket_url = ok.body["urls"][0].as_str().unwrap().to_string();
    // Through the guest listener, as the tunnel would.
    let mut guest = open_ticket(p.guest_addr, &ticket_url).await.unwrap();

    // The node is asked to open the relay, with the media token for this share.
    let (request_id, request) = node_request(&mut node).await;
    let NodeRequest::OpenRelay {
        relay_id,
        environment_id,
        codec,
        media_token,
    } = request
    else {
        panic!("expected an OpenRelay, got {request:?}");
    };
    assert_eq!(
        (environment_id.as_str(), codec.as_str()),
        (env.as_str(), "hevc")
    );
    let claims = cha_wire::verify_media_token(
        &p.state.media_key.public_b64(),
        &media_token,
        &env,
        db::now(),
    )
    .unwrap();
    assert_eq!(claims.role, "viewer");
    assert!(claims.sub.starts_with("share:"));

    // Dialling back needs the node's own signature, for this relay, from the
    // node it was asked of.
    let signed = key.sign_b64(&cha_wire::relay_message(&relay_id, &node_id));
    let other_key = self::key();
    let forged = other_key.sign_b64(&cha_wire::relay_message(&relay_id, &node_id));
    assert_eq!(
        dial_relay(p.main_addr, &relay_id, &node_id, &forged)
            .await
            .err(),
        Some(StatusCode::FORBIDDEN),
        "a signature by another key"
    );
    let for_another_relay = key.sign_b64(&cha_wire::relay_message("someone-elses", &node_id));
    assert_eq!(
        dial_relay(p.main_addr, &relay_id, &node_id, &for_another_relay)
            .await
            .err(),
        Some(StatusCode::FORBIDDEN)
    );
    assert_eq!(
        dial_relay(p.main_addr, &relay_id, "not-this-node", &signed)
            .await
            .err(),
        Some(StatusCode::FORBIDDEN),
        "a node the relay wasn't asked of"
    );
    assert_eq!(
        dial_relay(p.main_addr, "unknown-relay", &node_id, &signed)
            .await
            .err(),
        Some(StatusCode::FORBIDDEN)
    );
    // And it isn't on the guest listener at all.
    let on_guest = dial_relay(p.guest_addr, &relay_id, &node_id, &signed).await;
    assert_eq!(on_guest.err(), Some(StatusCode::NOT_FOUND));

    let mut relay = dial_relay(p.main_addr, &relay_id, &node_id, &signed)
        .await
        .expect("the right signature opens the relay");
    node_respond(&mut node, request_id, Ok(NodeResponse::RelayOpened)).await;
    // A relay id is good once.
    assert_eq!(
        dial_relay(p.main_addr, &relay_id, &node_id, &signed)
            .await
            .err(),
        Some(StatusCode::FORBIDDEN)
    );
    // And so is the ticket.
    assert_eq!(
        open_ticket(p.guest_addr, &ticket_url).await.err(),
        Some(StatusCode::NOT_FOUND)
    );

    // Text goes as text and binary as binary, in both directions.
    let hello = r#"{"t":"hello","transport":"websocket","maxDatagram":65536}"#;
    relay.send(Message::text(hello)).await.unwrap();
    let datagram: Vec<u8> = (0..70_000u32).map(|i| (i % 251) as u8).collect();
    relay.send(Message::binary(datagram.clone())).await.unwrap();
    assert_eq!(recv(&mut guest).await, Message::text(hello));
    assert_eq!(recv(&mut guest).await, Message::binary(datagram));
    let report = r#"{"t":"report","rtt":12}"#;
    guest.send(Message::text(report)).await.unwrap();
    guest.send(Message::binary(vec![1, 2, 3])).await.unwrap();
    assert_eq!(recv(&mut relay).await, Message::text(report));
    assert_eq!(recv(&mut relay).await, Message::binary(vec![1, 2, 3]));

    // The guest hangs up; the node's side is closed too.
    guest.close(None).await.unwrap();
    loop {
        match tokio::time::timeout(Duration::from_secs(5), relay.next())
            .await
            .expect("the relay stayed open after the guest left")
        {
            Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
            Some(Ok(_)) => {}
        }
    }
    // The join is audited with the guest's own address, not the tunnel's.
    let joined: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT detail, ip FROM audit_log WHERE action = 'share.joined' AND detail LIKE '%\"websocket\"%'",
    )
    .fetch_all(&p.db)
    .await
    .unwrap();
    assert_eq!(joined.len(), 1);
    assert_eq!(joined[0].1.as_deref(), Some("198.51.100.7"));
    assert!(
        !joined[0].0.contains(&token),
        "the token never reaches the log"
    );
}

async fn recv(ws: &mut Socket) -> Message {
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("no message for five seconds")
            .expect("the connection ended")
            .expect("a websocket error");
        if matches!(msg, Message::Text(_) | Message::Binary(_)) {
            return msg;
        }
    }
}

#[tokio::test]
async fn the_guest_is_closed_when_the_node_cannot_open_the_relay() {
    let p = quick().await;
    let (admin, admin_id) = p.admin().await;
    let key = key();
    let (node_id, env) = p.environment(&admin_id, &key, true).await;
    let mut node = node_join(p.main_addr, &node_id, &key).await;
    let made = p.share(&admin, &env, true).await;
    let token = token_of(made.body["url"].as_str().unwrap()).to_string();
    let ok = ws_connect(&p, &token, "h264").await;
    let mut guest = open_ticket(p.guest_addr, ok.body["urls"][0].as_str().unwrap())
        .await
        .unwrap();
    let (id, _) = node_request(&mut node).await;
    node_respond(&mut node, id, Err("the streamer refused: 503".into())).await;
    let closed = tokio::time::timeout(Duration::from_secs(5), guest.next())
        .await
        .expect("the guest stayed open");
    let Some(Ok(Message::Close(Some(frame)))) = closed else {
        panic!("expected a close frame, got {closed:?}");
    };
    assert_eq!(u16::from(frame.code), 1011);
    assert_eq!(frame.reason.as_str(), "node_error");

    // A node that isn't connected at all.
    let (_, offline_env) = p.environment(&admin_id, &self::key(), true).await;
    let made = p.share(&admin, &offline_env, true).await;
    let ok = ws_connect(&p, token_of(made.body["url"].as_str().unwrap()), "h264").await;
    let mut guest = open_ticket(p.guest_addr, ok.body["urls"][0].as_str().unwrap())
        .await
        .unwrap();
    let Some(Ok(Message::Close(Some(frame)))) = guest.next().await else {
        panic!("expected a close frame");
    };
    assert_eq!(frame.reason.as_str(), "node_offline");
}

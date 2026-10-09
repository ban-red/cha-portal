//! The player's sign-in against the real portal (`cha-control`) on loopback:
//! a `cha://` ticket, a device code approved in the browser, revocation, and
//! a device token kept out of the routes it doesn't open.

use std::net::SocketAddr;
use std::time::Duration;

use cha_client::Transport;
use cha_client_portal::{ConnectLink, PollTiming, Portal};
use cha_control::{AppState, Config, app, db};
use serde_json::{Value, json};

struct Running {
    origin: String,
    /// The admin's session cookie, as the browser holds it.
    cookie: String,
    http: reqwest::Client,
    _dir: tempfile::TempDir,
}

impl Running {
    async fn browser(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        let url = format!("{}{path}", self.origin);
        let req = match method {
            "GET" => self.http.get(url),
            "DELETE" => self.http.delete(url),
            _ => self.http.post(url),
        }
        .header("cookie", &self.cookie);
        let req = match body {
            Some(b) => req.json(&b),
            None => req,
        };
        let res = req.send().await.unwrap();
        let status = res.status().as_u16();
        (status, res.json().await.unwrap_or(Value::Null))
    }
}

async fn portal() -> Running {
    // reqwest is built without a TLS provider of its own; Portal installs ring.
    let _ = rustls::crypto::ring::default_provider().install_default();
    let dir = tempfile::tempdir().unwrap();
    let config = Config {
        listen: "127.0.0.1:0".parse().unwrap(),
        database: dir.path().join("cha.db"),
        web_dir: None,
        secure_cookies: false,
        session_days: 14,
        ice: Default::default(),
        dev_login: false,
        discover_nodes: false,
        public_url: None,
        tunnel: Default::default(),
    };
    let pool = db::open(&config.database).await.unwrap();
    let state = AppState::new(config, pool).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
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
        .post(format!("{origin}/api/setup"))
        .json(&json!({"username": "alex", "password": "correct horse"}))
        .send()
        .await
        .unwrap();
    assert!(res.status().is_success(), "setup: {}", res.status());
    let cookie = res
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find(|v| v.starts_with("cha_session="))
        .and_then(|v| v.split(';').next())
        .expect("a session cookie")
        .to_string();
    Running {
        origin,
        cookie,
        http,
        _dir: dir,
    }
}

fn player(dir: &tempfile::TempDir) -> Portal {
    Portal::open_with_timing(
        dir.path(),
        "Test Mac",
        PollTiming {
            min_interval: Duration::from_millis(50),
            slow_down_step: Duration::from_millis(50),
        },
    )
    .unwrap()
}

#[tokio::test]
async fn a_ticket_signs_the_player_in_and_revoking_signs_it_out() {
    let p = portal().await;
    let (status, body) = p
        .browser("POST", "/api/devices/tickets", Some(json!({})))
        .await;
    assert_eq!(status, 200, "{body}");
    let ticket = body["ticket"].as_str().unwrap().to_string();

    let data = tempfile::tempdir().unwrap();
    let portal = player(&data);
    let signed = portal
        .sign_in_with_ticket(&ConnectLink {
            portal: p.origin.clone(),
            ticket: ticket.clone(),
            launch: None,
        })
        .await
        .unwrap();
    assert_eq!(signed.username, "alex");
    assert!(portal.is_signed_in(&p.origin));
    portal
        .apps(&p.origin)
        .await
        .expect("the catalog with a device token");

    // The ticket was one use.
    let again = player(&tempfile::tempdir().unwrap());
    assert!(
        again
            .sign_in_with_ticket(&ConnectLink {
                portal: p.origin.clone(),
                ticket,
                launch: None,
            })
            .await
            .is_err()
    );

    // The browser sees the device and revokes it; the player is signed out.
    let (_, devices) = p.browser("GET", "/api/devices", None).await;
    let devices = devices.as_array().unwrap();
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0]["name"], "Test Mac");
    let id = devices[0]["id"].as_str().unwrap();
    let (status, _) = p
        .browser("DELETE", &format!("/api/devices/{id}"), None)
        .await;
    assert_eq!(status, 204);
    assert!(portal.apps(&p.origin).await.is_err());
    assert!(!portal.is_signed_in(&p.origin), "a 401 forgets the token");
}

#[tokio::test]
async fn a_device_code_approved_in_the_browser_signs_the_player_in() {
    let p = portal().await;
    let data = tempfile::tempdir().unwrap();
    let portal = player(&data);
    portal.add_host(&p.origin).await.unwrap();
    let pairing = portal.pair(&p.origin).await.unwrap();
    assert!(
        pairing.instructions.contains("/link"),
        "{}",
        pairing.instructions
    );

    // Typed in the portal in lower case, without the dash.
    let typed = pairing.pin.replace('-', "").to_lowercase();
    let (status, body) = p
        .browser("GET", &format!("/api/devices/codes/{typed}"), None)
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["name"], "Test Mac");
    let (status, _) = p
        .browser("POST", &format!("/api/devices/codes/{typed}/approve"), None)
        .await;
    assert_eq!(status, 200);

    tokio::time::timeout(Duration::from_secs(20), pairing.done)
        .await
        .expect("the poll ends")
        .expect("approved");
    assert!(portal.is_signed_in(&p.origin));
    portal.apps(&p.origin).await.unwrap();
}

#[tokio::test]
async fn a_device_token_opens_only_the_players_routes() {
    let p = portal().await;
    let (_, body) = p
        .browser("POST", "/api/devices/tickets", Some(json!({})))
        .await;
    let res = p
        .http
        .post(format!("{}/api/device/ticket", p.origin))
        .json(&json!({"ticket": body["ticket"], "install_id": "0b5c4d6e-1111-4222-8333-944445555666", "name": "curl"}))
        .send()
        .await
        .unwrap();
    let token = res.json::<Value>().await.unwrap()["token"]
        .as_str()
        .unwrap()
        .to_string();
    let get = |path: &str| {
        p.http
            .get(format!("{}{path}", p.origin))
            .bearer_auth(&token)
            .send()
    };
    for open in ["/api/me", "/api/catalog", "/api/environments"] {
        assert_eq!(get(open).await.unwrap().status().as_u16(), 200, "{open}");
    }
    for closed in ["/api/users", "/api/devices", "/api/audit", "/api/nodes"] {
        assert_eq!(
            get(closed).await.unwrap().status().as_u16(),
            401,
            "{closed}"
        );
    }
}

//! Launching through the portal: finding or starting the environment,
//! waiting for it, asking for a media token in the right codec, streaming
//! from a tiny in-process `cha-stream/1` streamer, and stopping.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use cha_client::{Codec, Ended, StreamConfig, Transport};
use cha_client_portal::{ConnectLink, Portal, pick_codecs};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use wtransport::{Endpoint, Identity, ServerConfig};

#[derive(Default)]
struct Fake {
    environments: Vec<Value>,
    /// `GET /api/environments/{id}` answers `starting` this many times, then `running`.
    starting_polls: u32,
    /// Answer to every poll instead, when set.
    poll_state: Option<(String, String)>,
    /// What `/connect` answers with.
    urls: Vec<String>,
    cert_hash: String,
    /// `/connect` refuses these codecs with `bad_codec`.
    refuse: Vec<String>,
    posts: Vec<Value>,
    connects: Vec<(String, Value)>,
    deletes: Vec<String>,
    bearers: Vec<String>,
    signed_out: bool,
}

type Shared = Arc<Mutex<Fake>>;

fn env(id: &str, template: &str, state: &str, codecs: Option<&[&str]>) -> Value {
    json!({"id": id, "templateId": template, "templateName": template, "ownerId": "u1",
        "state": state, "detail": null, "codecs": codecs, "createdAt": 1, "updatedAt": 1})
}

fn err(status: StatusCode, code: &str, message: &str) -> Response {
    (status, Json(json!({"error": code, "message": message}))).into_response()
}

async fn serve(fake: Shared) -> String {
    fn guard(f: &mut Fake, headers: &axum::http::HeaderMap) -> Option<Response> {
        let bearer = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let bad = f.signed_out || bearer != "Bearer chadev_ticket";
        f.bearers.push(bearer);
        bad.then(|| err(StatusCode::UNAUTHORIZED, "unauthorized", "no"))
    }
    async fn ticket() -> Response {
        Json(json!({"token": "chadev_ticket", "device_id": "d1",
            "user": {"id": "u1", "username": "alex", "role": "admin"}}))
        .into_response()
    }
    async fn catalog() -> Response {
        Json(json!([
            {"id": "chrome", "name": "Chrome"},
            {"id": "steam", "name": "Steam"},
        ]))
        .into_response()
    }
    async fn list(State(f): State<Shared>, h: axum::http::HeaderMap) -> Response {
        let mut f = f.lock().unwrap();
        if let Some(r) = guard(&mut f, &h) {
            return r;
        }
        Json(f.environments.clone()).into_response()
    }
    async fn launch(
        State(f): State<Shared>,
        h: axum::http::HeaderMap,
        Json(body): Json<Value>,
    ) -> Response {
        let mut f = f.lock().unwrap();
        if let Some(r) = guard(&mut f, &h) {
            return r;
        }
        f.posts.push(body.clone());
        let created = env(
            "new1",
            body["templateId"].as_str().unwrap(),
            "starting",
            None,
        );
        f.environments.insert(0, created.clone());
        Json(created).into_response()
    }
    async fn show(
        State(f): State<Shared>,
        h: axum::http::HeaderMap,
        Path(id): Path<String>,
    ) -> Response {
        let mut f = f.lock().unwrap();
        if let Some(r) = guard(&mut f, &h) {
            return r;
        }
        let mut found = f
            .environments
            .iter()
            .find(|e| e["id"] == id.as_str())
            .cloned()
            .unwrap();
        if let Some((state, detail)) = f.poll_state.clone() {
            found["state"] = state.into();
            found["detail"] = detail.into();
        } else if f.starting_polls > 0 {
            f.starting_polls -= 1;
        } else {
            found["state"] = "running".into();
            found["codecs"] = json!(["h264", "hevc"]);
        }
        Json(found).into_response()
    }
    async fn stop(
        State(f): State<Shared>,
        h: axum::http::HeaderMap,
        Path(id): Path<String>,
    ) -> Response {
        let mut f = f.lock().unwrap();
        if let Some(r) = guard(&mut f, &h) {
            return r;
        }
        f.deletes.push(id.clone());
        Json(env(&id, "x", "stopping", None)).into_response()
    }
    async fn connect(
        State(f): State<Shared>,
        h: axum::http::HeaderMap,
        Path(id): Path<String>,
        Json(body): Json<Value>,
    ) -> Response {
        let mut f = f.lock().unwrap();
        if let Some(r) = guard(&mut f, &h) {
            return r;
        }
        let codec = body["codec"].as_str().unwrap().to_string();
        f.connects.push((id, body));
        if f.refuse.contains(&codec) {
            return err(StatusCode::BAD_REQUEST, "bad_codec", "no such codec here");
        }
        Json(json!({"codec": codec, "transport": "webtransport",
            "urls": f.urls, "certHash": f.cert_hash}))
        .into_response()
    }
    let app = Router::new()
        .route("/api/device/ticket", post(ticket))
        .route("/api/catalog", get(catalog))
        .route("/api/environments", get(list).post(launch))
        .route("/api/environments/{id}", get(show).delete(stop))
        .route("/api/environments/{id}/connect", post(connect))
        .with_state(fake);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    origin
}

/// A streamer that says hello and gives the floor, and waits.
fn streamer() -> (u16, String) {
    let identity = Identity::self_signed(["localhost", "127.0.0.1"]).unwrap();
    let hash: String = Sha256::digest(identity.certificate_chain().as_slice()[0].der())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let config = ServerConfig::builder()
        .with_bind_address(SocketAddr::from(([127, 0, 0, 1], 0)))
        .with_identity(identity)
        .build();
    let endpoint = Endpoint::server(config).unwrap();
    let port = endpoint.local_addr().unwrap().port();
    tokio::spawn(async move {
        let conn = endpoint
            .accept()
            .await
            .await
            .unwrap()
            .accept()
            .await
            .unwrap();
        let (mut send, mut recv) = conn.accept_bi().await.unwrap();
        let mut buf = [0u8; 1024];
        let _ = recv.read(&mut buf).await;
        for line in [
            json!({"t":"hello","stream":{"codec":"hevc","width":2560,"height":1440,"input":true,
                "audio":true,"gamepads":true,"fps":60,"overlay":null,"transport":"webtransport",
                "maxDatagram":1200}}),
            json!({"t":"floor","control":true,"viewers":1}),
        ] {
            send.write_all(format!("{line}\n").as_bytes())
                .await
                .unwrap();
        }
        while let Ok(Some(_)) = recv.read(&mut buf).await {}
        drop(endpoint);
    });
    (port, hash)
}

fn config(codecs: Vec<Codec>) -> StreamConfig {
    StreamConfig {
        width: 2560,
        height: 1440,
        fps: 60,
        bitrate_kbps: 50_000,
        codecs,
        audio_channels: 2,
    }
}

async fn signed_in(fake: Shared) -> (Portal, String, tempfile::TempDir) {
    let origin = serve(fake).await;
    let dir = tempfile::tempdir().unwrap();
    let portal = Portal::open(dir.path(), "Alex's MacBook Pro").unwrap();
    portal
        .sign_in_with_ticket(&ConnectLink {
            portal: origin.clone(),
            ticket: "t".into(),
            launch: None,
        })
        .await
        .unwrap();
    (portal, origin, dir)
}

fn point_at(fake: &Shared, port: u16, hash: String) {
    let mut f = fake.lock().unwrap();
    f.urls = vec![format!(
        "https://127.0.0.1:{port}/media?codec=hevc&token=a.b"
    )];
    f.cert_hash = hash;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_launch_starts_the_environment_waits_and_streams() {
    let fake: Shared = Arc::default();
    fake.lock().unwrap().starting_polls = 1;
    let (port, hash) = streamer();
    point_at(&fake, port, hash);
    let (portal, origin, _dir) = signed_in(fake.clone()).await;
    let apps = portal.apps(&origin).await.unwrap();
    assert_eq!(apps[1].name, "Steam");

    // AV1 first, but the environment encodes h264 and hevc: hevc it is.
    let session = portal
        .launch(
            &origin,
            apps[1].id,
            config(vec![Codec::Av1, Codec::Hevc, Codec::H264]),
        )
        .await
        .expect("launch");
    {
        let f = fake.lock().unwrap();
        assert_eq!(f.posts, [json!({"templateId": "steam"})]);
        assert_eq!(f.connects.len(), 1);
        assert_eq!(f.connects[0].0, "new1");
        assert_eq!(
            f.connects[0].1,
            json!({"codec": "hevc", "transport": "webtransport"})
        );
        assert!(f.bearers.iter().all(|b| b == "Bearer chadev_ticket"));
    }
    assert_eq!(session.codec, Codec::Hevc);
    assert_eq!((session.width, session.height), (2560, 1440));

    // Stopping the stream alone leaves the environment be.
    // Quitting the app stops the environment.
    session.control.stop(true);
    let ended = tokio::time::timeout(Duration::from_secs(3), session.ended)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(ended, Ended::Stopped);
    for _ in 0..50 {
        if !fake.lock().unwrap().deletes.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(fake.lock().unwrap().deletes, ["new1"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_running_environment_of_the_app_is_reused_and_left_running() {
    let fake: Shared = Arc::default();
    {
        let mut f = fake.lock().unwrap();
        f.environments = vec![
            env("old", "steam", "stopping", None),
            env("other", "chrome", "running", Some(&["h264"])),
            env("mine", "steam", "running", Some(&["h264", "hevc"])),
        ];
    }
    let (port, hash) = streamer();
    point_at(&fake, port, hash);
    let (portal, origin, _dir) = signed_in(fake.clone()).await;
    let apps = portal.apps(&origin).await.unwrap();
    let session = portal
        .launch(&origin, apps[1].id, config(vec![Codec::H264]))
        .await
        .expect("launch");
    {
        let f = fake.lock().unwrap();
        assert!(f.posts.is_empty(), "no new environment");
        assert_eq!(f.connects[0].0, "mine");
        assert_eq!(f.connects[0].1["codec"], "h264");
    }
    session.control.stop(false);
    let ended = tokio::time::timeout(Duration::from_secs(3), session.ended)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(ended, Ended::Stopped);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(fake.lock().unwrap().deletes.is_empty(), "left running");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_environment_that_fails_says_why() {
    let fake: Shared = Arc::default();
    fake.lock().unwrap().poll_state = Some(("failed".into(), "the image didn't pull".into()));
    let (portal, origin, _dir) = signed_in(fake.clone()).await;
    let apps = portal.apps(&origin).await.unwrap();
    let e = portal
        .launch(&origin, apps[0].id, config(vec![Codec::H264]))
        .await
        .err()
        .expect("a failed environment is an error");
    let text = format!("{e:#}");
    assert!(
        text.contains("failed to start") && text.contains("the image didn't pull"),
        "{text}"
    );
    assert!(fake.lock().unwrap().connects.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_codec_the_streamer_refuses_falls_through_to_the_next() {
    let fake: Shared = Arc::default();
    {
        let mut f = fake.lock().unwrap();
        f.environments = vec![env("mine", "chrome", "running", None)];
        f.refuse = vec!["av1".into()];
    }
    let (port, hash) = streamer();
    point_at(&fake, port, hash);
    let (portal, origin, _dir) = signed_in(fake.clone()).await;
    let apps = portal.apps(&origin).await.unwrap();
    // The node didn't say what it encodes, so every codec is tried in turn.
    let session = portal
        .launch(&origin, apps[0].id, config(vec![Codec::Av1, Codec::Hevc]))
        .await
        .expect("launch");
    let codecs: Vec<String> = fake
        .lock()
        .unwrap()
        .connects
        .iter()
        .map(|c| c.1["codec"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(codecs, ["av1", "hevc"]);
    session.control.stop(false);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_token_the_portal_refuses_signs_this_player_out() {
    let fake: Shared = Arc::default();
    let (portal, origin, _dir) = signed_in(fake.clone()).await;
    let apps = portal.apps(&origin).await.unwrap();
    fake.lock().unwrap().signed_out = true;
    let e = portal
        .launch(&origin, apps[0].id, config(vec![Codec::H264]))
        .await
        .err()
        .expect("signed out");
    assert!(format!("{e}").contains("signed out"), "{e}");
    assert!(!portal.is_signed_in(&origin));
}

#[tokio::test]
async fn codecs_are_the_players_choices_the_environment_encodes() {
    let names = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let wanted = [Codec::Av1, Codec::Hevc, Codec::H264];
    assert_eq!(
        pick_codecs(&wanted, Some(&names(&["h264", "hevc", "pyrowave420"]))).unwrap(),
        [Codec::Hevc, Codec::H264]
    );
    assert_eq!(pick_codecs(&wanted, None).unwrap(), wanted);
    let e = pick_codecs(&[Codec::Av1], Some(&names(&["h264"]))).unwrap_err();
    assert!(format!("{e}").contains("h264") && format!("{e}").contains("av1"));
    assert!(pick_codecs(&wanted, Some(&names(&["pyrowave420"]))).is_err());
}

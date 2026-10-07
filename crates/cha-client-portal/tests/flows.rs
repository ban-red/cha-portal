//! The sign-in flows and the catalog against a tiny in-process portal that
//! follows `docs/plans/c2-device-signin.md`.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use cha_client::{Pairing, Transport};
use cha_client_portal::{ConnectLink, PollTiming, Portal};
use serde_json::{Value, json};

#[derive(Default)]
struct Fake {
    /// Tokens `/api/catalog` accepts.
    valid_tokens: Vec<String>,
    /// What `/api/device/token` answers, front first (then `authorization_pending`).
    polls: VecDeque<Value>,
    /// Every `Authorization` header seen by `/api/catalog`.
    bearers: Vec<String>,
    bodies: Vec<Value>,
    /// `/api/catalog` redirects here when set.
    redirect_to: Option<String>,
    ticket_ok: bool,
    expires_in: u64,
    poll_times: Vec<std::time::Instant>,
}

type Shared = Arc<Mutex<Fake>>;

fn grant(token: &str) -> Value {
    json!({"token": token, "device_id": "dev1", "user": {"id": "u1", "username": "alex", "role": "admin"}})
}

fn err(status: StatusCode, code: &str) -> Response {
    (
        status,
        Json(json!({"error": code, "message": format!("message for {code}")})),
    )
        .into_response()
}

async fn serve(fake: Shared) -> String {
    async fn ticket(State(f): State<Shared>, Json(body): Json<Value>) -> Response {
        let mut f = f.lock().unwrap();
        f.bodies.push(body);
        if f.ticket_ok {
            f.ticket_ok = false;
            Json(grant("chadev_ticket")).into_response()
        } else {
            err(StatusCode::BAD_REQUEST, "invalid_ticket")
        }
    }
    async fn code(State(f): State<Shared>, Json(body): Json<Value>) -> Response {
        let mut f = f.lock().unwrap();
        f.bodies.push(body);
        Json(
            json!({"device_code": "dc", "user_code": "ABCD-EFGH", "verification_path": "/link",
            "expires_in": f.expires_in, "interval": 0}),
        )
        .into_response()
    }
    async fn token(State(f): State<Shared>, Json(body): Json<Value>) -> Response {
        let mut f = f.lock().unwrap();
        f.bodies.push(body);
        f.poll_times.push(std::time::Instant::now());
        match f.polls.pop_front() {
            Some(v) if v.get("token").is_some() => Json(v).into_response(),
            Some(v) => err(StatusCode::BAD_REQUEST, v.as_str().unwrap()),
            None => err(StatusCode::BAD_REQUEST, "authorization_pending"),
        }
    }
    async fn catalog(State(f): State<Shared>, headers: HeaderMap) -> Response {
        let mut f = f.lock().unwrap();
        let bearer = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        f.bearers.push(bearer.clone());
        if let Some(to) = &f.redirect_to {
            return Redirect::temporary(&format!("{to}/api/catalog")).into_response();
        }
        match bearer.strip_prefix("Bearer ") {
            Some(t) if f.valid_tokens.iter().any(|v| v == t) => Json(json!([
                {"id": "chrome", "name": "Chrome", "description": "x", "image": "i", "extra": 1},
                {"id": "xfce", "name": "XFCE", "description": "y", "image": "j"},
            ]))
            .into_response(),
            _ => err(StatusCode::UNAUTHORIZED, "unauthorized"),
        }
    }
    let app = Router::new()
        .route("/api/device/ticket", post(ticket))
        .route("/api/device/code", post(code))
        .route("/api/device/token", post(token))
        .route("/api/catalog", get(catalog))
        .with_state(fake);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    origin
}

fn fast() -> PollTiming {
    PollTiming {
        slow_down_step: Duration::from_millis(300),
        min_interval: Duration::from_millis(10),
    }
}

fn player(dir: &std::path::Path) -> Portal {
    Portal::open_with_timing(dir, "Alex's MacBook Pro", fast()).unwrap()
}

fn link(portal: &str, ticket: &str) -> ConnectLink {
    ConnectLink {
        portal: portal.into(),
        ticket: ticket.into(),
        launch: None,
    }
}

#[tokio::test]
async fn a_ticket_signs_in_and_lists_the_catalog() {
    let fake: Shared = Arc::default();
    fake.lock().unwrap().ticket_ok = true;
    fake.lock()
        .unwrap()
        .valid_tokens
        .push("chadev_ticket".into());
    let origin = serve(fake.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let portal = player(dir.path());

    let signed_in = portal
        .sign_in_with_ticket(&link(&origin, "t1"))
        .await
        .unwrap();
    assert_eq!(signed_in.username, "alex");
    let body = fake.lock().unwrap().bodies[0].clone();
    assert_eq!(body["ticket"], "t1");
    assert_eq!(body["install_id"], portal.install_id());
    assert_eq!(body["name"], "Alex's MacBook Pro");

    let hosts = portal.hosts();
    assert_eq!(hosts.len(), 1);
    assert!(hosts[0].paired);
    assert_eq!(hosts[0].id, origin);

    let apps = portal.apps(&origin).await.unwrap();
    assert_eq!(
        apps.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(),
        ["Chrome", "XFCE"]
    );
    assert_eq!(
        portal.template_id(&origin, apps[1].id).as_deref(),
        Some("xfce")
    );
    assert_eq!(fake.lock().unwrap().bearers, ["Bearer chadev_ticket"]);

    // It survives a restart.
    let again = player(dir.path());
    assert!(again.is_signed_in(&origin));

    // A ticket is single use.
    let e = portal
        .sign_in_with_ticket(&link(&origin, "t1"))
        .await
        .unwrap_err();
    assert!(
        format!("{e}").contains("expired or was already used"),
        "{e}"
    );
}

#[tokio::test]
async fn a_revoked_token_reads_as_signed_out() {
    let fake: Shared = Arc::default();
    fake.lock().unwrap().ticket_ok = true;
    fake.lock()
        .unwrap()
        .valid_tokens
        .push("chadev_ticket".into());
    let origin = serve(fake.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let portal = player(dir.path());
    portal
        .sign_in_with_ticket(&link(&origin, "t"))
        .await
        .unwrap();

    fake.lock().unwrap().valid_tokens.clear();
    let e = portal.apps(&origin).await.unwrap_err();
    assert!(format!("{e}").starts_with("signed out"), "{e}");
    assert!(!portal.is_signed_in(&origin));
    assert!(!portal.hosts()[0].paired);
    // And asking again doesn't even send a request.
    assert!(portal.apps(&origin).await.is_err());
    assert_eq!(fake.lock().unwrap().bearers.len(), 1);
}

#[tokio::test]
async fn the_device_code_flow_polls_through_slow_down_to_approval() {
    let fake: Shared = Arc::default();
    {
        let mut f = fake.lock().unwrap();
        f.expires_in = 60;
        f.polls = [
            json!("authorization_pending"),
            json!("slow_down"),
            json!("authorization_pending"),
            grant("chadev_code"),
        ]
        .into();
    }
    let origin = serve(fake.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let portal = player(dir.path());

    let host = portal.add_host(&origin).await.unwrap();
    assert!(!host.paired);
    assert_eq!(host.id, origin);

    let Pairing {
        pin,
        instructions,
        done,
    } = portal.pair(&host.id).await.unwrap();
    assert_eq!(pin, "ABCD-EFGH");
    assert!(
        instructions.contains(&format!("{origin}/link")),
        "{instructions}"
    );
    done.await.unwrap();

    assert!(portal.is_signed_in(&origin));
    let f = fake.lock().unwrap();
    assert_eq!(f.bodies[0]["install_id"], portal.install_id());
    assert_eq!(f.bodies[1]["device_code"], "dc");
    // After slow_down (poll 2) the gap to the next poll grew by the step.
    let gap = |i: usize| f.poll_times[i + 1] - f.poll_times[i];
    assert!(gap(1) >= Duration::from_millis(300), "{:?}", gap(1));
    assert!(gap(0) < Duration::from_millis(250), "{:?}", gap(0));
    assert_eq!(f.poll_times.len(), 4);
}

#[tokio::test]
async fn denied_and_expired_codes_stop_the_polling() {
    for (answer, wanted) in [("access_denied", "denied"), ("expired_token", "expired")] {
        let fake: Shared = Arc::default();
        {
            let mut f = fake.lock().unwrap();
            f.expires_in = 60;
            f.polls = [json!(answer)].into();
        }
        let origin = serve(fake.clone()).await;
        let dir = tempfile::tempdir().unwrap();
        let portal = player(dir.path());
        portal.add_host(&origin).await.unwrap();
        let pairing = portal.pair(&origin).await.unwrap();
        let e = pairing.done.await.unwrap_err();
        assert!(format!("{e}").contains(wanted), "{e}");
        assert!(!portal.is_signed_in(&origin));
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            fake.lock().unwrap().poll_times.len(),
            1,
            "no polling after {answer}"
        );
    }
}

#[tokio::test]
async fn a_code_that_is_never_approved_times_out() {
    let fake: Shared = Arc::default();
    fake.lock().unwrap().expires_in = 0;
    let origin = serve(fake).await;
    let dir = tempfile::tempdir().unwrap();
    let portal = player(dir.path());
    portal.add_host(&origin).await.unwrap();
    let e = portal.pair(&origin).await.unwrap().done.await.unwrap_err();
    assert!(format!("{e}").contains("in time"), "{e}");
}

#[tokio::test]
async fn a_token_goes_only_to_its_own_portal() {
    let (a, b) = (Shared::default(), Shared::default());
    for f in [&a, &b] {
        f.lock().unwrap().ticket_ok = true;
    }
    a.lock().unwrap().valid_tokens.push("chadev_ticket".into());
    let (origin_a, origin_b) = (serve(a.clone()).await, serve(b.clone()).await);
    let dir = tempfile::tempdir().unwrap();
    let portal = player(dir.path());
    portal
        .sign_in_with_ticket(&link(&origin_a, "t"))
        .await
        .unwrap();
    // B is known but not signed in: listing its apps must not use A's token.
    portal.add_host(&origin_b).await.unwrap();
    assert!(portal.apps(&origin_b).await.is_err());
    assert!(b.lock().unwrap().bearers.is_empty());

    // A redirect from A to B isn't followed, so A's token can't be carried there.
    a.lock().unwrap().redirect_to = Some(origin_b.clone());
    assert!(portal.apps(&origin_a).await.is_err());
    assert!(b.lock().unwrap().bearers.is_empty());
}

#[tokio::test]
async fn transport_basics() {
    let dir = tempfile::tempdir().unwrap();
    let portal = player(dir.path());
    assert_eq!(portal.name(), "Cha Portal");
    assert!(portal.add_host("ftp://x.example").await.is_err());
    assert!(portal.pair("https://unknown.example").await.is_err());
    let e = portal
        .launch(
            "x",
            1,
            cha_client::StreamConfig {
                width: 1,
                height: 1,
                fps: 1,
                bitrate_kbps: 1,
                codecs: vec![],
                audio_channels: 2,
            },
        )
        .await
        .err()
        .unwrap();
    // Not a portal this player knows: nothing to launch on.
    assert!(
        format!("{e}").contains("isn't a portal this player knows"),
        "{e}"
    );
}

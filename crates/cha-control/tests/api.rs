//! The API end to end against a temporary SQLite database.

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use cha_control::{AppState, Config, app, db};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

struct TestPortal {
    app: Router,
    setup_token: String,
    _dir: tempfile::TempDir,
}

async fn portal() -> TestPortal {
    let dir = tempfile::tempdir().unwrap();
    let config = Config {
        listen: "127.0.0.1:0".parse().unwrap(),
        database: dir.path().join("cha.db"),
        web_dir: None,
        secure_cookies: false,
        session_days: 14,
        ice: Default::default(),
    };
    let pool = db::open(&config.database).await.unwrap();
    let state = AppState::new(config, pool).await.unwrap();
    let setup_token = state.setup_token.lock().await.clone().unwrap();
    TestPortal {
        app: app(state),
        setup_token,
        _dir: dir,
    }
}

struct Reply {
    status: StatusCode,
    body: Value,
    cookie: Option<String>,
}

impl TestPortal {
    async fn call(
        &self,
        method: &str,
        path: &str,
        cookie: Option<&str>,
        body: Option<Value>,
    ) -> Reply {
        let mut req = Request::builder().method(method).uri(path);
        if let Some(cookie) = cookie {
            req = req.header(header::COOKIE, cookie);
        }
        let req = match body {
            Some(body) => req
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string())),
            None => req.body(Body::empty()),
        }
        .unwrap();
        let res = self.app.clone().oneshot(req).await.unwrap();
        let status = res.status();
        // Keep "name=value" from Set-Cookie, as a browser would send it back.
        let cookie = res
            .headers()
            .get(header::SET_COOKIE)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(';').next())
            .map(str::to_string);
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        Reply {
            status,
            body,
            cookie,
        }
    }

    /// Runs first-run setup and returns the admin's session cookie.
    async fn setup_admin(&self) -> String {
        let reply = self
            .call(
                "POST",
                "/api/setup",
                None,
                Some(json!({ "token": self.setup_token, "username": "admin", "password": "correct horse battery" })),
            )
            .await;
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
        reply.cookie.expect("setup signs the admin in")
    }
}

#[tokio::test]
async fn first_run_setup_creates_one_admin() {
    let p = portal().await;
    assert_eq!(
        p.call("GET", "/api/setup", None, None).await.body,
        json!({ "needed": true })
    );

    let wrong = p
        .call("POST", "/api/setup", None, Some(json!({ "token": "nope", "username": "admin", "password": "correct horse battery" })))
        .await;
    assert_eq!(wrong.status, StatusCode::FORBIDDEN);
    assert_eq!(wrong.body["error"], "bad_setup_token");

    let cookie = p.setup_admin().await;
    let me = p.call("GET", "/api/me", Some(&cookie), None).await;
    assert_eq!(me.status, StatusCode::OK);
    assert_eq!(me.body["username"], "admin");
    assert_eq!(me.body["role"], "admin");
    assert!(
        me.body.get("passwordHash").is_none(),
        "never expose the hash"
    );

    assert_eq!(
        p.call("GET", "/api/setup", None, None).await.body,
        json!({ "needed": false })
    );
    let again = p
        .call("POST", "/api/setup", None, Some(json!({ "token": p.setup_token, "username": "x2", "password": "correct horse battery" })))
        .await;
    assert_eq!(again.status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn sign_in_and_out() {
    let p = portal().await;
    let admin = p.setup_admin().await;

    let bad = p
        .call(
            "POST",
            "/api/auth/login",
            None,
            Some(json!({ "username": "admin", "password": "wrong password" })),
        )
        .await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST);
    assert_eq!(bad.body["error"], "invalid_credentials");
    let unknown = p
        .call(
            "POST",
            "/api/auth/login",
            None,
            Some(json!({ "username": "ghost", "password": "wrong password" })),
        )
        .await;
    assert_eq!(
        unknown.body, bad.body,
        "unknown users look like wrong passwords"
    );

    // Usernames are case-insensitive.
    let ok = p
        .call(
            "POST",
            "/api/auth/login",
            None,
            Some(json!({ "username": "ADMIN", "password": "correct horse battery" })),
        )
        .await;
    assert_eq!(ok.status, StatusCode::OK);
    let session = ok.cookie.unwrap();
    assert_eq!(
        p.call("GET", "/api/me", Some(&session), None).await.status,
        StatusCode::OK
    );

    let out = p
        .call("POST", "/api/auth/logout", Some(&session), Some(json!({})))
        .await;
    assert_eq!(out.status, StatusCode::OK);
    assert_eq!(
        p.call("GET", "/api/me", Some(&session), None).await.status,
        StatusCode::UNAUTHORIZED
    );
    // The setup session is separate and still valid.
    assert_eq!(
        p.call("GET", "/api/me", Some(&admin), None).await.status,
        StatusCode::OK
    );
    assert_eq!(
        p.call("GET", "/api/me", None, None).await.status,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn only_admins_manage_users() {
    let p = portal().await;
    let admin = p.setup_admin().await;

    let created = p
        .call(
            "POST",
            "/api/users",
            Some(&admin),
            Some(json!({ "username": "player1", "displayName": "Player One", "password": "another long password", "role": "user" })),
        )
        .await;
    assert_eq!(created.status, StatusCode::OK, "{}", created.body);
    assert_eq!(created.body["displayName"], "Player One");

    let dup = p
        .call("POST", "/api/users", Some(&admin), Some(json!({ "username": "Player1", "password": "another long password", "role": "user" })))
        .await;
    assert_eq!(dup.status, StatusCode::CONFLICT);
    let weak = p
        .call(
            "POST",
            "/api/users",
            Some(&admin),
            Some(json!({ "username": "player2", "password": "short", "role": "user" })),
        )
        .await;
    assert_eq!(weak.body["error"], "weak_password");
    let bad_name = p
        .call(
            "POST",
            "/api/users",
            Some(&admin),
            Some(
                json!({ "username": "../x", "password": "another long password", "role": "user" }),
            ),
        )
        .await;
    assert_eq!(bad_name.body["error"], "bad_username");

    let player = p
        .call(
            "POST",
            "/api/auth/login",
            None,
            Some(json!({ "username": "player1", "password": "another long password" })),
        )
        .await
        .cookie
        .unwrap();
    let denied = p.call("GET", "/api/users", Some(&player), None).await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);
    let denied = p
        .call("POST", "/api/users", Some(&player), Some(json!({ "username": "sneaky", "password": "another long password", "role": "admin" })))
        .await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);

    let users = p.call("GET", "/api/users", Some(&admin), None).await;
    assert_eq!(users.body.as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn audit_log_records_security_events() {
    let p = portal().await;
    let admin = p.setup_admin().await;
    p.call(
        "POST",
        "/api/auth/login",
        None,
        Some(json!({ "username": "admin", "password": "nope nope nope" })),
    )
    .await;
    p.call(
        "POST",
        "/api/users",
        Some(&admin),
        Some(json!({ "username": "player1", "password": "another long password", "role": "user" })),
    )
    .await;
    let audit = p.call("GET", "/api/audit", Some(&admin), None).await;
    let actions: Vec<&str> = audit
        .body
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["action"].as_str().unwrap())
        .collect();
    for expected in ["setup.admin_created", "login.failed", "user.created"] {
        assert!(
            actions.contains(&expected),
            "{expected} missing from {actions:?}"
        );
    }
}

#[tokio::test]
async fn mutating_calls_need_json() {
    // A cross-site form post (text/plain or urlencoded) must not reach a handler.
    let p = portal().await;
    let req = Request::builder()
        .method("POST")
        .uri("/api/auth/login")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from("username=admin&password=x"))
        .unwrap();
    let res = p.app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
}

#[tokio::test]
async fn unknown_api_paths_are_json_404s() {
    let p = portal().await;
    let reply = p.call("GET", "/api/nope", None, None).await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    assert_eq!(reply.body["error"], "not_found");
}

//! The API end to end against a temporary SQLite database.

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode, header};
use cha_control::{AppState, Config, app, db};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

struct TestPortal {
    app: Router,
    db: sqlx::SqlitePool,
    setup_token: String,
    _dir: tempfile::TempDir,
}

async fn portal() -> TestPortal {
    portal_with(false).await
}

async fn portal_with(dev_login: bool) -> TestPortal {
    let dir = tempfile::tempdir().unwrap();
    let config = Config {
        listen: "127.0.0.1:0".parse().unwrap(),
        database: dir.path().join("cha.db"),
        web_dir: None,
        secure_cookies: false,
        session_days: 14,
        ice: Default::default(),
        dev_login,
    };
    let pool = db::open(&config.database).await.unwrap();
    let state = AppState::new(config, pool.clone()).await.unwrap();
    let setup_token = state.setup_token.lock().await.clone().unwrap();
    TestPortal {
        app: app(state),
        db: pool,
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

    /// POSTs to dev login as a client at `from`.
    async fn dev_login(&self, from: &str) -> Reply {
        let addr: std::net::SocketAddr = from.parse().unwrap();
        let req = Request::builder()
            .method("POST")
            .uri("/api/auth/dev-login")
            .header(header::CONTENT_TYPE, "application/json")
            .extension(ConnectInfo(addr))
            .body(Body::from("{}"))
            .unwrap();
        let res = self.app.clone().oneshot(req).await.unwrap();
        let status = res.status();
        let cookie = res
            .headers()
            .get(header::SET_COOKIE)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(';').next())
            .map(str::to_string);
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        Reply {
            status,
            body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
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
        json!({ "needed": true, "devLogin": false })
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
        json!({ "needed": false, "devLogin": false })
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

#[tokio::test]
async fn dev_login_is_off_by_default() {
    let p = portal().await;
    assert_eq!(
        p.dev_login("127.0.0.1:5000").await.status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn dev_login_creates_the_dev_admin_for_loopback_only() {
    let p = portal_with(true).await;
    assert_eq!(
        p.call("GET", "/api/setup", None, None).await.body,
        json!({ "needed": true, "devLogin": true })
    );

    let remote = p.dev_login("192.168.1.20:5000").await;
    assert_eq!(remote.status, StatusCode::FORBIDDEN);
    assert_eq!(remote.body["error"], "dev_login_loopback_only");

    let first = p.dev_login("127.0.0.1:5000").await;
    assert_eq!(first.status, StatusCode::OK, "{}", first.body);
    assert_eq!(first.body["username"], "dev");
    assert_eq!(first.body["role"], "admin");
    let me = p
        .call("GET", "/api/me", first.cookie.as_deref(), None)
        .await;
    assert_eq!(me.body["username"], "dev");

    // Setup is closed, and a second click reuses the same account.
    assert_eq!(
        p.call("GET", "/api/setup", None, None).await.body["needed"],
        false
    );
    let again = p.dev_login("[::1]:5000").await;
    assert_eq!(again.status, StatusCode::OK);
    assert_eq!(again.body["id"], first.body["id"]);
}

// ---- App data settings ----

impl TestPortal {
    /// An account with `role`, signed in: its cookie and id.
    async fn account(&self, admin: &str, username: &str, role: &str) -> (String, String) {
        let created = self
            .call(
                "POST",
                "/api/users",
                Some(admin),
                Some(json!({ "username": username, "password": "another long password", "role": role })),
            )
            .await;
        assert_eq!(created.status, StatusCode::OK, "{}", created.body);
        let cookie = self
            .call(
                "POST",
                "/api/auth/login",
                None,
                Some(json!({ "username": username, "password": "another long password" })),
            )
            .await
            .cookie
            .unwrap();
        (cookie, created.body["id"].as_str().unwrap().to_string())
    }

    /// Gives `user_id` a live environment of `template`, as a launch would.
    async fn live_environment(&self, user_id: &str, template: &str) {
        let node_id = db::new_id();
        sqlx::query(
            "INSERT INTO nodes (id, name, public_key, enrolled_at) VALUES (?, 'test', ?, 0)",
        )
        .bind(&node_id)
        .bind(&node_id)
        .execute(&self.db)
        .await
        .unwrap();
        db::insert_environment(
            &self.db,
            &db::new_id(),
            user_id,
            template,
            &node_id,
            "running",
        )
        .await
        .unwrap();
    }
}

fn app_of<'a>(body: &'a Value, template: &str) -> &'a Value {
    body["apps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["template"] == template)
        .unwrap_or_else(|| panic!("no {template} in {body}"))
}

#[tokio::test]
async fn app_data_starts_from_the_catalog() {
    let p = portal().await;
    let admin = p.setup_admin().await;
    let reply = p.call("GET", "/api/storage", Some(&admin), None).await;
    assert_eq!(reply.status, StatusCode::OK);
    // With no node to say otherwise, the default root.
    assert_eq!(reply.body["root"], "/srv/cha-portal");
    assert_eq!(
        app_of(&reply.body, "steam"),
        &json!({
            "template": "steam",
            "name": "Steam",
            "persistent": true,
            "default": true,
            "sharedAccess": "write",
            "live": false,
        })
    );
    assert_eq!(
        app_of(&reply.body, "chrome"),
        &json!({
            "template": "chrome",
            "name": "Google Chrome",
            "persistent": false,
            "default": false,
            "sharedAccess": "none",
            "live": false,
        })
    );
    // One entry per catalog template.
    let catalog = p.call("GET", "/api/catalog", Some(&admin), None).await;
    assert_eq!(
        reply.body["apps"].as_array().unwrap().len(),
        catalog.body.as_array().unwrap().len()
    );

    assert_eq!(
        p.call("GET", "/api/storage", None, None).await.status,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn a_user_chooses_per_app_and_others_are_unaffected() {
    let p = portal().await;
    let admin = p.setup_admin().await;
    let (alice, _) = p.account(&admin, "alice", "user").await;
    let (bob, _) = p.account(&admin, "bob", "user").await;

    let on = p
        .call(
            "PUT",
            "/api/storage/chrome",
            Some(&alice),
            Some(json!({ "persistent": true })),
        )
        .await;
    assert_eq!(on.status, StatusCode::OK, "{}", on.body);
    assert_eq!(
        on.body,
        json!({
            "template": "chrome",
            "name": "Google Chrome",
            "persistent": true,
            "default": false,
            "sharedAccess": "none",
            "live": false,
        })
    );
    // Steam is on by default; alice turns it off, and the default stays what it was.
    let off = p
        .call(
            "PUT",
            "/api/storage/steam",
            Some(&alice),
            Some(json!({ "persistent": false })),
        )
        .await;
    assert_eq!(off.body["persistent"], false);
    assert_eq!(off.body["default"], true);

    let alices = p.call("GET", "/api/storage", Some(&alice), None).await.body;
    assert_eq!(app_of(&alices, "chrome")["persistent"], true);
    assert_eq!(app_of(&alices, "steam")["persistent"], false);
    let bobs = p.call("GET", "/api/storage", Some(&bob), None).await.body;
    assert_eq!(app_of(&bobs, "chrome")["persistent"], false);
    assert_eq!(app_of(&bobs, "steam")["persistent"], true);

    // And back.
    let again = p
        .call(
            "PUT",
            "/api/storage/steam",
            Some(&alice),
            Some(json!({ "persistent": true })),
        )
        .await;
    assert_eq!(again.body["persistent"], true);

    let missing = p
        .call(
            "PUT",
            "/api/storage/nope",
            Some(&alice),
            Some(json!({ "persistent": true })),
        )
        .await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
    let no_body = p
        .call("PUT", "/api/storage/chrome", Some(&alice), Some(json!({})))
        .await;
    assert!(no_body.status.is_client_error());

    let audit = p.call("GET", "/api/audit", Some(&admin), None).await;
    let entry = audit
        .body
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["action"] == "storage.persistence_set" && e["target"] == "chrome")
        .expect("the choice is audited");
    assert_eq!(entry["detail"], json!({ "persistent": true }).to_string());
}

#[tokio::test]
async fn settings_wait_for_a_live_environment() {
    let p = portal().await;
    let admin = p.setup_admin().await;
    let (alice, alice_id) = p.account(&admin, "alice", "user").await;
    p.live_environment(&alice_id, "steam").await;

    let listed = p.call("GET", "/api/storage", Some(&alice), None).await.body;
    assert_eq!(app_of(&listed, "steam")["live"], true);
    assert_eq!(app_of(&listed, "chrome")["live"], false);

    let blocked = p
        .call(
            "PUT",
            "/api/storage/steam",
            Some(&alice),
            Some(json!({ "persistent": false })),
        )
        .await;
    assert_eq!(blocked.status, StatusCode::CONFLICT);
    assert_eq!(blocked.body["error"], "live");
    let reset = p
        .call("POST", "/api/storage/steam/reset", Some(&alice), None)
        .await;
    assert_eq!(reset.status, StatusCode::CONFLICT);
    assert_eq!(reset.body["error"], "live");
    // The setting didn't move.
    let after = p.call("GET", "/api/storage", Some(&alice), None).await.body;
    assert_eq!(app_of(&after, "steam")["persistent"], true);

    // Another app of theirs, and someone else's, are free.
    let other = p
        .call(
            "PUT",
            "/api/storage/chrome",
            Some(&alice),
            Some(json!({ "persistent": true })),
        )
        .await;
    assert_eq!(other.status, StatusCode::OK);
    let free = p
        .call(
            "PUT",
            "/api/storage/steam",
            Some(&admin),
            Some(json!({ "persistent": false })),
        )
        .await;
    assert_eq!(free.status, StatusCode::OK);
}

#[tokio::test]
async fn resetting_needs_a_node_to_delete_on() {
    let p = portal().await;
    let admin = p.setup_admin().await;
    let reply = p
        .call("POST", "/api/storage/steam/reset", Some(&admin), None)
        .await;
    assert_eq!(reply.status, StatusCode::BAD_GATEWAY);
    assert_eq!(reply.body["error"], "no_node");
    let unknown = p
        .call("POST", "/api/storage/nope/reset", Some(&admin), None)
        .await;
    assert_eq!(unknown.status, StatusCode::NOT_FOUND);
    assert_eq!(
        p.call("POST", "/api/storage/steam/reset", None, None)
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn guests_keep_no_data() {
    let p = portal().await;
    let admin = p.setup_admin().await;
    let (guest, _) = p.account(&admin, "guest1", "guest").await;
    let listed = p.call("GET", "/api/storage", Some(&guest), None).await;
    assert_eq!(listed.status, StatusCode::OK);
    assert_eq!(listed.body["apps"], json!([]));
    let set = p
        .call(
            "PUT",
            "/api/storage/chrome",
            Some(&guest),
            Some(json!({ "persistent": true })),
        )
        .await;
    assert_eq!(set.status, StatusCode::FORBIDDEN);
    let reset = p
        .call("POST", "/api/storage/chrome/reset", Some(&guest), None)
        .await;
    assert_eq!(reset.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn admins_set_the_defaults_and_the_sharing_per_app() {
    let p = portal().await;
    let admin = p.setup_admin().await;
    let (alice, _) = p.account(&admin, "alice", "user").await;

    let listed = p
        .call("GET", "/api/admin/storage", Some(&admin), None)
        .await;
    assert_eq!(listed.status, StatusCode::OK);
    assert_eq!(listed.body["root"], "/srv/cha-portal");
    assert_eq!(
        app_of(&listed.body, "steam"),
        &json!({
            "template": "steam",
            "name": "Steam",
            "defaultPersistent": true,
            "sharedAccess": "write",
        })
    );
    assert_eq!(app_of(&listed.body, "kde")["sharedAccess"], "none");

    // Only what is sent changes.
    let access = p
        .call(
            "PUT",
            "/api/admin/storage/steam",
            Some(&admin),
            Some(json!({ "sharedAccess": "read" })),
        )
        .await;
    assert_eq!(access.status, StatusCode::OK, "{}", access.body);
    assert_eq!(
        access.body,
        json!({
            "template": "steam",
            "name": "Steam",
            "defaultPersistent": true,
            "sharedAccess": "read",
        })
    );
    let default = p
        .call(
            "PUT",
            "/api/admin/storage/steam",
            Some(&admin),
            Some(json!({ "defaultPersistent": false })),
        )
        .await;
    assert_eq!(default.body["defaultPersistent"], false);
    assert_eq!(default.body["sharedAccess"], "read", "kept");
    let both = p
        .call(
            "PUT",
            "/api/admin/storage/kde",
            Some(&admin),
            Some(json!({ "defaultPersistent": true, "sharedAccess": "write" })),
        )
        .await;
    assert_eq!(both.body["defaultPersistent"], true);
    assert_eq!(both.body["sharedAccess"], "write");

    // Users see the new defaults and sharing, and their own choice still wins.
    let seen = p.call("GET", "/api/storage", Some(&alice), None).await.body;
    let steam = app_of(&seen, "steam");
    assert_eq!(
        (
            steam["persistent"].clone(),
            steam["default"].clone(),
            steam["sharedAccess"].clone()
        ),
        (json!(false), json!(false), json!("read"))
    );
    assert_eq!(app_of(&seen, "kde")["persistent"], true);
    p.call(
        "PUT",
        "/api/storage/kde",
        Some(&alice),
        Some(json!({ "persistent": false })),
    )
    .await;
    p.call(
        "PUT",
        "/api/admin/storage/kde",
        Some(&admin),
        Some(json!({ "defaultPersistent": true })),
    )
    .await;
    let kept = p.call("GET", "/api/storage", Some(&alice), None).await.body;
    assert_eq!(app_of(&kept, "kde")["persistent"], false);
    assert_eq!(app_of(&kept, "kde")["default"], true);

    // Bad requests.
    for body in [
        json!({}),
        json!({ "sharedAccess": "everyone" }),
        json!({ "defaultPersistent": "yes" }),
    ] {
        let reply = p
            .call(
                "PUT",
                "/api/admin/storage/steam",
                Some(&admin),
                Some(body.clone()),
            )
            .await;
        assert!(reply.status.is_client_error(), "{body}: {}", reply.status);
    }
    let unknown = p
        .call(
            "PUT",
            "/api/admin/storage/nope",
            Some(&admin),
            Some(json!({ "sharedAccess": "read" })),
        )
        .await;
    assert_eq!(unknown.status, StatusCode::NOT_FOUND);

    // Audited, newest first, with only what was sent.
    let audit = p.call("GET", "/api/audit", Some(&admin), None).await;
    let kde: Vec<Value> = audit
        .body
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["action"] == "storage.defaults_set" && e["target"] == "kde")
        .map(|e| serde_json::from_str(e["detail"].as_str().unwrap()).unwrap())
        .collect();
    assert_eq!(
        kde,
        [
            json!({ "defaultPersistent": true }),
            json!({ "defaultPersistent": true, "sharedAccess": "write" }),
        ]
    );
}

#[tokio::test]
async fn only_admins_touch_the_defaults() {
    let p = portal().await;
    let admin = p.setup_admin().await;
    let (alice, _) = p.account(&admin, "alice", "user").await;
    for (method, path, body) in [
        ("GET", "/api/admin/storage", None),
        (
            "PUT",
            "/api/admin/storage/steam",
            Some(json!({ "sharedAccess": "none" })),
        ),
    ] {
        let denied = p.call(method, path, Some(&alice), body.clone()).await;
        assert_eq!(denied.status, StatusCode::FORBIDDEN, "{path}");
        assert_eq!(denied.body["error"], "admin_only");
        let anonymous = p.call(method, path, None, body).await;
        assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);
    }
    // The default is still the catalog's.
    let listed = p
        .call("GET", "/api/admin/storage", Some(&admin), None)
        .await;
    assert_eq!(app_of(&listed.body, "steam")["sharedAccess"], "write");
}

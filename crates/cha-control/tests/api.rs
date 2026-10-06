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

    /// Calls `path` as a client at `from`, with `body` for a POST.
    async fn call_from(&self, from: &str, path: &str, body: Option<Value>) -> Reply {
        let addr: std::net::SocketAddr = from.parse().unwrap();
        let req = Request::builder()
            .method(if body.is_some() { "POST" } else { "GET" })
            .uri(path)
            .header(header::CONTENT_TYPE, "application/json")
            .extension(ConnectInfo(addr))
            .body(Body::from(body.map(|b| b.to_string()).unwrap_or_default()))
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

    /// POSTs to dev login as a client at `from`.
    async fn dev_login(&self, from: &str) -> Reply {
        self.call_from(from, "/api/auth/dev-login", Some(json!({})))
            .await
    }

    /// POSTs to dev login as `username`, from loopback.
    async fn dev_login_as(&self, username: &str) -> Reply {
        self.call_from(
            "127.0.0.1:5000",
            "/api/auth/dev-login",
            Some(json!({ "username": username })),
        )
        .await
    }

    /// GETs the dev accounts as a client at `from`.
    async fn dev_accounts(&self, from: &str) -> Reply {
        self.call_from(from, "/api/auth/dev-accounts", None).await
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

#[tokio::test]
async fn dev_accounts_lists_enabled_admins_other_than_dev() {
    let off = portal().await;
    assert_eq!(
        off.dev_accounts("127.0.0.1:5000").await.status,
        StatusCode::NOT_FOUND
    );

    let p = portal_with(true).await;
    let admin = p.setup_admin().await;
    p.account(&admin, "zed", "admin").await;
    p.account(&admin, "player1", "user").await;
    let (_, gone) = p.account(&admin, "gone", "admin").await;
    sqlx::query("UPDATE users SET disabled = 1 WHERE id = ?")
        .bind(&gone)
        .execute(&p.db)
        .await
        .unwrap();
    assert_eq!(p.dev_login("127.0.0.1:5000").await.status, StatusCode::OK);

    let remote = p.dev_accounts("192.168.1.20:5000").await;
    assert_eq!(remote.status, StatusCode::FORBIDDEN);
    assert_eq!(remote.body["error"], "dev_login_loopback_only");

    let list = p.dev_accounts("127.0.0.1:5000").await;
    assert_eq!(list.status, StatusCode::OK, "{}", list.body);
    assert_eq!(
        list.body,
        json!({ "accounts": [
            { "username": "admin", "displayName": "admin" },
            { "username": "zed", "displayName": "zed" },
        ] })
    );
}

#[tokio::test]
async fn dev_login_can_sign_in_as_an_existing_admin() {
    let p = portal_with(true).await;
    let admin = p.setup_admin().await;
    let me = p.call("GET", "/api/me", Some(&admin), None).await;

    let reply = p.dev_login_as("admin").await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(reply.body["id"], me.body["id"]);
    let session = p
        .call("GET", "/api/me", reply.cookie.as_deref(), None)
        .await;
    assert_eq!(session.body["username"], "admin");

    let audit = p.call("GET", "/api/audit", Some(&admin), None).await;
    let row = audit
        .body
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["action"] == "dev.login_as")
        .expect("dev.login_as is audited");
    assert_eq!(row["target"], me.body["id"]);
    let detail: Value = serde_json::from_str(row["detail"].as_str().unwrap()).unwrap();
    assert_eq!(detail["username"], "admin");
    assert!(
        audit
            .body
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["action"] == "login.dev")
    );
}

#[tokio::test]
async fn dev_login_as_refuses_what_is_not_an_enabled_admin() {
    let p = portal_with(true).await;
    let admin = p.setup_admin().await;
    p.account(&admin, "player1", "user").await;
    let (_, gone) = p.account(&admin, "gone", "admin").await;
    sqlx::query("UPDATE users SET disabled = 1 WHERE id = ?")
        .bind(&gone)
        .execute(&p.db)
        .await
        .unwrap();

    let non_admin = p.dev_login_as("player1").await;
    assert_eq!(non_admin.status, StatusCode::FORBIDDEN);
    assert_eq!(non_admin.body["error"], "dev_login_admin_only");
    assert!(non_admin.cookie.is_none());

    let unknown = p.dev_login_as("nobody").await;
    assert_eq!(unknown.status, StatusCode::NOT_FOUND);
    // Never created on the way.
    assert!(db::user_for_login(&p.db, "nobody").await.unwrap().is_none());

    let disabled = p.dev_login_as("gone").await;
    assert_eq!(disabled.status, StatusCode::FORBIDDEN);
    assert_eq!(disabled.body["error"], "disabled");

    let remote = p
        .call_from(
            "192.168.1.20:5000",
            "/api/auth/dev-login",
            Some(json!({ "username": "admin" })),
        )
        .await;
    assert_eq!(remote.status, StatusCode::FORBIDDEN);
    assert_eq!(remote.body["error"], "dev_login_loopback_only");

    let off = portal().await;
    off.setup_admin().await;
    assert_eq!(
        off.dev_login_as("admin").await.status,
        StatusCode::NOT_FOUND
    );
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
    /// Returns its node's id and its own.
    async fn live_environment(&self, user_id: &str, template: &str) -> (String, String) {
        let node_id = db::new_id();
        sqlx::query(
            "INSERT INTO nodes (id, name, public_key, enrolled_at) VALUES (?, 'test', ?, 0)",
        )
        .bind(&node_id)
        .bind(&node_id)
        .execute(&self.db)
        .await
        .unwrap();
        let id = db::new_id();
        db::insert_environment(&self.db, &id, user_id, template, &node_id, None, "running")
            .await
            .unwrap();
        (node_id, id)
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
    // Steam's display has a fixed size, so its page never asks for a resize.
    let template_of = |id: &str| {
        catalog
            .body
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["id"] == id)
            .unwrap()
            .clone()
    };
    assert_eq!(template_of("steam")["fixedSize"], true);
    assert_eq!(template_of("chrome")["fixedSize"], false);

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
async fn a_user_chooses_a_controller_per_app() {
    let p = portal().await;
    let admin = p.setup_admin().await;
    let (alice, _) = p.account(&admin, "alice", "user").await;
    let (bob, _) = p.account(&admin, "bob", "user").await;

    let listed = p
        .call("GET", "/api/controllers/apps", Some(&alice), None)
        .await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    assert_eq!(
        app_of(&listed.body, "steam"),
        &json!({ "template": "steam", "name": "Steam", "kind": null, "default": "xbox360" })
    );

    let set = p
        .call(
            "PUT",
            "/api/controllers/apps/steam",
            Some(&alice),
            Some(json!({ "kind": "dualsense" })),
        )
        .await;
    assert_eq!(set.status, StatusCode::OK, "{}", set.body);
    assert_eq!(set.body["kind"], "dualsense");
    assert_eq!(set.body["default"], "xbox360");

    // Only alice's own choice, only for steam.
    let alices = p
        .call("GET", "/api/controllers/apps", Some(&alice), None)
        .await
        .body;
    assert_eq!(app_of(&alices, "steam")["kind"], "dualsense");
    assert_eq!(app_of(&alices, "chrome")["kind"], json!(null));
    let bobs = p
        .call("GET", "/api/controllers/apps", Some(&bob), None)
        .await
        .body;
    assert_eq!(app_of(&bobs, "steam")["kind"], json!(null));

    // Changing it and clearing it.
    let steam_pad = p
        .call(
            "PUT",
            "/api/controllers/apps/steam",
            Some(&alice),
            Some(json!({ "kind": "steam" })),
        )
        .await;
    assert_eq!(steam_pad.body["kind"], "steam");
    let cleared = p
        .call(
            "PUT",
            "/api/controllers/apps/steam",
            Some(&alice),
            Some(json!({ "kind": null })),
        )
        .await;
    assert_eq!(cleared.status, StatusCode::OK);
    assert_eq!(cleared.body["kind"], json!(null));

    let missing = p
        .call(
            "PUT",
            "/api/controllers/apps/nope",
            Some(&alice),
            Some(json!({ "kind": "steam" })),
        )
        .await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
    for bad in [json!({ "kind": "ps5" }), json!({ "kind": 3 })] {
        let reply = p
            .call(
                "PUT",
                "/api/controllers/apps/steam",
                Some(&alice),
                Some(bad),
            )
            .await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.body);
    }
    // Leaving the kind out isn't a reset.
    let no_kind = p
        .call(
            "PUT",
            "/api/controllers/apps/steam",
            Some(&alice),
            Some(json!({})),
        )
        .await;
    assert!(no_kind.status.is_client_error());

    let audit = p.call("GET", "/api/audit", Some(&admin), None).await;
    let entry = audit
        .body
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["action"] == "controllers.gamepad_set" && e["target"] == "steam")
        .expect("the choice is audited");
    assert!(entry["detail"].as_str().unwrap().contains("kind"));

    assert_eq!(
        p.call("GET", "/api/controllers/apps", None, None)
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        p.call(
            "PUT",
            "/api/controllers/apps/steam",
            None,
            Some(json!({ "kind": "steam" })),
        )
        .await
        .status,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn guests_have_no_controller_settings() {
    let p = portal().await;
    let admin = p.setup_admin().await;
    let (guest, _) = p.account(&admin, "guest1", "guest").await;
    let listed = p
        .call("GET", "/api/controllers/apps", Some(&guest), None)
        .await;
    assert_eq!(listed.body["apps"], json!([]));
    let set = p
        .call(
            "PUT",
            "/api/controllers/apps/chrome",
            Some(&guest),
            Some(json!({ "kind": "steam" })),
        )
        .await;
    assert_eq!(set.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_user_chooses_a_frame_rate_per_app() {
    let p = portal().await;
    let admin = p.setup_admin().await;
    let (alice, _) = p.account(&admin, "alice", "user").await;
    let (bob, _) = p.account(&admin, "bob", "user").await;

    let listed = p
        .call("GET", "/api/apps/settings", Some(&alice), None)
        .await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    assert_eq!(
        app_of(&listed.body, "steam"),
        &json!({ "template": "steam", "name": "Steam", "fps": null, "defaultFps": 60 })
    );

    let set = p
        .call(
            "PUT",
            "/api/apps/settings/steam",
            Some(&alice),
            Some(json!({ "fps": 120 })),
        )
        .await;
    assert_eq!(set.status, StatusCode::OK, "{}", set.body);
    assert_eq!(set.body["fps"], 120);
    assert_eq!(set.body["defaultFps"], 60);

    // Only alice's own choice, only for steam.
    let alices = p
        .call("GET", "/api/apps/settings", Some(&alice), None)
        .await
        .body;
    assert_eq!(app_of(&alices, "steam")["fps"], 120);
    assert_eq!(app_of(&alices, "chrome")["fps"], json!(null));
    let bobs = p
        .call("GET", "/api/apps/settings", Some(&bob), None)
        .await
        .body;
    assert_eq!(app_of(&bobs, "steam")["fps"], json!(null));

    let ninety = p
        .call(
            "PUT",
            "/api/apps/settings/steam",
            Some(&alice),
            Some(json!({ "fps": 90 })),
        )
        .await;
    assert_eq!(ninety.body["fps"], 90);
    let cleared = p
        .call(
            "PUT",
            "/api/apps/settings/steam",
            Some(&alice),
            Some(json!({ "fps": null })),
        )
        .await;
    assert_eq!(cleared.status, StatusCode::OK);
    assert_eq!(cleared.body["fps"], json!(null));

    let missing = p
        .call(
            "PUT",
            "/api/apps/settings/nope",
            Some(&alice),
            Some(json!({ "fps": 90 })),
        )
        .await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
    for bad in [
        json!({ "fps": 75 }),
        json!({ "fps": 144 }),
        json!({ "fps": "60" }),
        json!({ "fps": -60 }),
    ] {
        let reply = p
            .call("PUT", "/api/apps/settings/steam", Some(&alice), Some(bad))
            .await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.body);
    }
    // Leaving the rate out isn't a reset.
    let no_fps = p
        .call(
            "PUT",
            "/api/apps/settings/steam",
            Some(&alice),
            Some(json!({})),
        )
        .await;
    assert!(no_fps.status.is_client_error());

    let audit = p.call("GET", "/api/audit", Some(&admin), None).await;
    let entry = audit
        .body
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["action"] == "apps.fps_set" && e["target"] == "steam")
        .expect("the choice is audited");
    assert!(entry["detail"].as_str().unwrap().contains("fps"));

    assert_eq!(
        p.call("GET", "/api/apps/settings", None, None).await.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        p.call(
            "PUT",
            "/api/apps/settings/steam",
            None,
            Some(json!({ "fps": 90 })),
        )
        .await
        .status,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn guests_have_no_app_settings() {
    let p = portal().await;
    let admin = p.setup_admin().await;
    let (guest, _) = p.account(&admin, "guest1", "guest").await;
    let listed = p
        .call("GET", "/api/apps/settings", Some(&guest), None)
        .await;
    assert_eq!(listed.body["apps"], json!([]));
    let set = p
        .call(
            "PUT",
            "/api/apps/settings/chrome",
            Some(&guest),
            Some(json!({ "fps": 90 })),
        )
        .await;
    assert_eq!(set.status, StatusCode::FORBIDDEN);
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
async fn a_nodes_warning_shows_on_the_environment_until_it_clears_or_ends() {
    let p = portal().await;
    let admin = p.setup_admin().await;
    let (alice, alice_id) = p.account(&admin, "alice", "user").await;
    let (node, id) = p.live_environment(&alice_id, "steam").await;
    let warning_of = |listed: &Value| listed[0]["warning"].clone();
    let listed = p.call("GET", "/api/environments", Some(&alice), None).await;
    assert_eq!(listed.status, StatusCode::OK);
    assert_eq!(warning_of(&listed.body), Value::Null);

    // What the node's message does (`ToPortal::EnvironmentWarning`).
    let set = db::set_environment_warning(&p.db, &id, &node, Some("folders came unmounted"))
        .await
        .unwrap();
    assert!(set);
    let listed = p.call("GET", "/api/environments", Some(&alice), None).await;
    assert_eq!(warning_of(&listed.body), "folders came unmounted");
    let shown = p
        .call(
            "GET",
            &format!("/api/environments/{id}"),
            Some(&alice),
            None,
        )
        .await;
    assert_eq!(shown.body["warning"], "folders came unmounted");

    // Only the environment's own node may say it.
    assert!(
        !db::set_environment_warning(&p.db, &id, "another-node", None)
            .await
            .unwrap()
    );
    let listed = p.call("GET", "/api/environments", Some(&alice), None).await;
    assert_eq!(warning_of(&listed.body), "folders came unmounted");

    // The node says it is over.
    db::set_environment_warning(&p.db, &id, &node, None)
        .await
        .unwrap();
    let listed = p.call("GET", "/api/environments", Some(&alice), None).await;
    assert_eq!(warning_of(&listed.body), Value::Null);

    // Ending the environment drops what it said, and a stopped one takes none.
    db::set_environment_warning(&p.db, &id, &node, Some("again"))
        .await
        .unwrap();
    db::transition_environment(&p.db, &id, &["running"], "destroyed", None)
        .await
        .unwrap();
    let listed = p.call("GET", "/api/environments", Some(&alice), None).await;
    assert_eq!(warning_of(&listed.body), Value::Null);
    assert!(
        !db::set_environment_warning(&p.db, &id, &node, Some("late"))
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn a_dead_environments_log_is_for_its_owner_and_admins() {
    let p = portal().await;
    let admin = p.setup_admin().await;
    let (alice, alice_id) = p.account(&admin, "alice", "user").await;
    let (bob, _) = p.account(&admin, "bob", "user").await;
    let (_, id) = p.live_environment(&alice_id, "steam").await;
    let path = format!("/api/environments/{id}");

    // A start the node reported as an error first; its exit follows with the log.
    db::transition_environment(&p.db, &id, &["running"], "failed", Some("start failed"))
        .await
        .unwrap();
    let log = json!(["a line", "Error: out of memory"]).to_string();
    assert!(
        db::record_exit(&p.db, &id, true, "The GPU is out of memory", Some(&log))
            .await
            .unwrap()
    );
    // A second report takes nothing: the log is already there.
    assert!(
        !db::record_exit(&p.db, &id, true, "again", Some("[]"))
            .await
            .unwrap()
    );

    let shown = p.call("GET", &path, Some(&alice), None).await;
    assert_eq!(shown.body["detail"], "The GPU is out of memory");
    assert_eq!(shown.body["log"], json!(["a line", "Error: out of memory"]));
    let listed = p.call("GET", "/api/environments", Some(&alice), None).await;
    assert_eq!(listed.body[0]["log"], shown.body["log"]);
    let seen = p.call("GET", &path, Some(&admin), None).await;
    assert_eq!(seen.body["log"], shown.body["log"]);
    // Anyone else doesn't get the environment, let alone its log.
    let other = p.call("GET", &path, Some(&bob), None).await;
    assert_eq!(other.status, StatusCode::NOT_FOUND);
    assert!(other.body.get("log").is_none());

    // One that ended with no log shows null.
    let (_, quiet) = p.live_environment(&alice_id, "chrome").await;
    assert!(
        db::record_exit(&p.db, &quiet, false, "the app exited", None)
            .await
            .unwrap()
    );
    let shown = p
        .call(
            "GET",
            &format!("/api/environments/{quiet}"),
            Some(&alice),
            None,
        )
        .await;
    assert_eq!(shown.body["state"], "destroyed");
    assert_eq!(shown.body["log"], Value::Null);
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

#[tokio::test]
async fn preferences_round_trip_per_user() {
    let p = portal().await;
    let admin = p.setup_admin().await;
    let (alice, _) = p.account(&admin, "alice", "user").await;
    let (bob, _) = p.account(&admin, "bob", "user").await;

    let empty = p.call("GET", "/api/me/prefs", Some(&alice), None).await;
    assert_eq!(empty.status, StatusCode::OK);
    assert_eq!(empty.body, json!({ "prefs": {} }));

    let prefs = json!({ "theme": "cha-jade", "appearance": "light", "contrast": "more" });
    let put = p
        .call(
            "PUT",
            "/api/me/prefs",
            Some(&alice),
            Some(json!({ "prefs": prefs })),
        )
        .await;
    assert_eq!(put.status, StatusCode::OK, "{}", put.body);
    let got = p.call("GET", "/api/me/prefs", Some(&alice), None).await;
    assert_eq!(got.body, json!({ "prefs": prefs }));

    // A second PUT replaces the object whole.
    p.call(
        "PUT",
        "/api/me/prefs",
        Some(&alice),
        Some(json!({ "prefs": { "motion": "reduced" } })),
    )
    .await;
    let got = p.call("GET", "/api/me/prefs", Some(&alice), None).await;
    assert_eq!(got.body, json!({ "prefs": { "motion": "reduced" } }));

    // Bob sees none of it.
    let bobs = p.call("GET", "/api/me/prefs", Some(&bob), None).await;
    assert_eq!(bobs.body, json!({ "prefs": {} }));
}

#[tokio::test]
async fn bad_preferences_are_refused() {
    let p = portal().await;
    let admin = p.setup_admin().await;
    let bad = [
        json!({ "prefs": { "colour": "red" } }),
        json!({ "prefs": { "appearance": "sepia" } }),
        json!({ "prefs": { "contrast": "high" } }),
        json!({ "prefs": { "motion": "none" } }),
        json!({ "prefs": { "transparency": 1 } }),
        json!({ "prefs": { "theme": "Cha Jade" } }),
        json!({ "prefs": { "theme": "a".repeat(41) } }),
        json!({ "prefs": [] }),
        json!({ "prefs": "dark" }),
        json!({ "prefs": null }),
        json!({}),
    ];
    for body in bad {
        let r = p
            .call("PUT", "/api/me/prefs", Some(&admin), Some(body.clone()))
            .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{body}: {}", r.body);
    }
    // The key set is closed, so the 4 KiB cap on the stored object is a backstop; a body
    // far past the request limit is refused unread.
    let huge = json!({ "prefs": {}, "pad": "x".repeat(32 * 1024) });
    let r = p
        .call("PUT", "/api/me/prefs", Some(&admin), Some(huge))
        .await;
    assert_eq!(r.status, StatusCode::PAYLOAD_TOO_LARGE);
    // Nothing was stored by any of that.
    let got = p.call("GET", "/api/me/prefs", Some(&admin), None).await;
    assert_eq!(got.body, json!({ "prefs": {} }));
}

#[tokio::test]
async fn preferences_need_a_session_and_guests_can_save() {
    let p = portal().await;
    let admin = p.setup_admin().await;
    assert_eq!(
        p.call("GET", "/api/me/prefs", None, None).await.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        p.call("PUT", "/api/me/prefs", None, Some(json!({ "prefs": {} })))
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );
    let (guest, _) = p.account(&admin, "guest1", "guest").await;
    let put = p
        .call(
            "PUT",
            "/api/me/prefs",
            Some(&guest),
            Some(json!({ "prefs": { "appearance": "dark" } })),
        )
        .await;
    assert_eq!(put.status, StatusCode::OK, "{}", put.body);
    let got = p.call("GET", "/api/me/prefs", Some(&guest), None).await;
    assert_eq!(got.body, json!({ "prefs": { "appearance": "dark" } }));
}

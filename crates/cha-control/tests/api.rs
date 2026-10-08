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
    discovered: std::sync::Arc<cha_control::discovery::Discovered>,
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
        discover_nodes: true,
        public_url: None,
    };
    let pool = db::open(&config.database).await.unwrap();
    let state = AppState::new(config, pool.clone()).await.unwrap();
    TestPortal {
        discovered: state.discovered.clone(),
        app: app(state),
        db: pool,
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

    /// POSTs to dev login from loopback through a proxy that sets `header`.
    async fn dev_login_proxied(&self, header: &str) -> Reply {
        let addr: std::net::SocketAddr = "127.0.0.1:5000".parse().unwrap();
        let req = Request::builder()
            .method("POST")
            .uri("/api/auth/dev-login")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header, "203.0.113.9")
            .extension(ConnectInfo(addr))
            .body(Body::from("{}"))
            .unwrap();
        let res = self.app.clone().oneshot(req).await.unwrap();
        let status = res.status();
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        Reply {
            status,
            body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
            cookie: None,
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
                Some(json!({ "username": "admin", "password": "correct horse battery" })),
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

    let short = p
        .call(
            "POST",
            "/api/setup",
            None,
            Some(json!({ "username": "ad", "password": "abc" })),
        )
        .await;
    assert_eq!(short.body["error"], "bad_username");
    let short = p
        .call(
            "POST",
            "/api/setup",
            None,
            Some(json!({ "username": "adm", "password": "ab" })),
        )
        .await;
    assert_eq!(short.body["error"], "weak_password");

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
        .call(
            "POST",
            "/api/setup",
            None,
            Some(json!({ "username": "x2", "password": "correct horse battery" })),
        )
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
            Some(json!({ "username": "player2", "password": "ab", "role": "user" })),
        )
        .await;
    assert_eq!(weak.body["error"], "weak_password");
    let bad_name = p
        .call(
            "POST",
            "/api/users",
            Some(&admin),
            Some(json!({ "username": " x ", "password": "another long password", "role": "user" })),
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
    for proxy in ["x-forwarded-for", "forwarded", "x-real-ip"] {
        let proxied = p.dev_login_proxied(proxy).await;
        assert_eq!(proxied.status, StatusCode::FORBIDDEN, "{proxy}");
        assert_eq!(proxied.body["error"], "dev_login_loopback_only");
    }

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
    assert_eq!(app_of(&listed.body, "chrome")["sharedAccess"], "none");

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
            "/api/admin/storage/chrome",
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
    assert_eq!(app_of(&seen, "chrome")["persistent"], true);
    p.call(
        "PUT",
        "/api/storage/chrome",
        Some(&alice),
        Some(json!({ "persistent": false })),
    )
    .await;
    p.call(
        "PUT",
        "/api/admin/storage/chrome",
        Some(&admin),
        Some(json!({ "defaultPersistent": true })),
    )
    .await;
    let kept = p.call("GET", "/api/storage", Some(&alice), None).await.body;
    assert_eq!(app_of(&kept, "chrome")["persistent"], false);
    assert_eq!(app_of(&kept, "chrome")["default"], true);

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
    let chrome: Vec<Value> = audit
        .body
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["action"] == "storage.defaults_set" && e["target"] == "chrome")
        .map(|e| serde_json::from_str(e["detail"].as_str().unwrap()).unwrap())
        .collect();
    assert_eq!(
        chrome,
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
async fn environments_page_preferences_round_trip() {
    let p = portal().await;
    let admin = p.setup_admin().await;
    let prefs = json!({
        "theme": "cha-magenta",
        "pinned": ["google-chrome", "steam"],
        "envView": "list",
        "envSort": "recent"
    });
    let put = p
        .call(
            "PUT",
            "/api/me/prefs",
            Some(&admin),
            Some(json!({ "prefs": prefs })),
        )
        .await;
    assert_eq!(put.status, StatusCode::OK, "{}", put.body);
    let got = p.call("GET", "/api/me/prefs", Some(&admin), None).await;
    assert_eq!(got.body, json!({ "prefs": prefs }));
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
        json!({ "prefs": { "envView": "table" } }),
        json!({ "prefs": { "envSort": "size" } }),
        json!({ "prefs": { "pinned": "chrome" } }),
        json!({ "prefs": { "pinned": ["Chrome"] } }),
        json!({ "prefs": { "pinned": ["chrome", "chrome"] } }),
        json!({ "prefs": { "pinned": [7] } }),
        json!({ "prefs": { "pinned": ["a".repeat(41)] } }),
        json!({ "prefs": { "pinned": (0..65).map(|i| format!("app-{i}")).collect::<Vec<_>>() } }),
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

#[tokio::test]
async fn catalog_logos_are_inert_images() {
    let p = portal().await;
    let admin = p.setup_admin().await;
    let catalog = p.call("GET", "/api/catalog", Some(&admin), None).await;
    let chrome = catalog
        .body
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == "chrome")
        .unwrap();
    assert_eq!(chrome["icon"], "icon.svg");

    let req = Request::builder()
        .uri("/api/catalog/chrome/icon")
        .header(header::COOKIE, &admin)
        .body(Body::empty())
        .unwrap();
    let res = p.app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers()[header::CONTENT_TYPE], "image/svg+xml");
    assert_eq!(res.headers()[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
    assert!(
        res.headers()[header::CONTENT_SECURITY_POLICY]
            .to_str()
            .unwrap()
            .starts_with("default-src 'none'")
    );
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    assert!(std::str::from_utf8(&bytes).unwrap().contains("<svg"));

    // The test pattern has no logo; nobody signed in gets nothing.
    assert_eq!(
        p.call("GET", "/api/catalog/test-pattern/icon", Some(&admin), None)
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        p.call("GET", "/api/catalog/chrome/icon", None, None)
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );
}

// ---- Claiming nodes found on the LAN (ADR 0007) ----

mod claiming {
    use super::*;
    use axum::Json;
    use axum::extract::State;
    use axum::routing::post;
    use cha_control::discovery::{DiscoveredNode, unix_ms};
    use cha_wire::NodeKey;
    use cha_wire::claim::{self, *};
    use std::sync::{Arc, Mutex};

    const CODE: &str = "48219375";

    /// A node's claim port, played with the same helpers the real one uses.
    struct FakeNode {
        port: u16,
        key: NodeKey,
        /// The URL the portal said it was reached at.
        portal_url: Mutex<Option<String>>,
        /// The id the portal gave the node, once it did.
        node_id: Mutex<Option<String>>,
        session: Mutex<Option<ClaimSession>>,
        /// Answers `/claim/start` with this refusal instead.
        refuse: Option<(StatusCode, &'static str)>,
        /// Fails `/claim/done`.
        fail_done: bool,
    }

    fn refusal(status: StatusCode, code: &str, message: &str) -> (StatusCode, Json<ErrorBody>) {
        (
            status,
            Json(ErrorBody {
                error: code.into(),
                message: message.into(),
            }),
        )
    }

    async fn fake_node(
        refuse: Option<(StatusCode, &'static str)>,
        fail_done: bool,
    ) -> Arc<FakeNode> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let node = Arc::new(FakeNode {
            port: listener.local_addr().unwrap().port(),
            key: NodeKey::from_secret([9; 32]),
            portal_url: Mutex::new(None),
            node_id: Mutex::new(None),
            session: Mutex::new(None),
            refuse,
            fail_done,
        });
        let router = Router::new()
            .route(
                START_PATH,
                post(
                    |State(n): State<Arc<FakeNode>>, Json(req): Json<StartRequest>| async move {
                        if let Some((status, message)) = n.refuse {
                            return Err(refusal(status, "insecure_portal", message));
                        }
                        *n.portal_url.lock().unwrap() = Some(req.portal_url.clone());
                        let (spake, session) =
                            NodeStart::respond(CODE, &req.spake, &req.portal_url, "claim-1")
                                .unwrap();
                        *n.session.lock().unwrap() = Some(session);
                        Ok(Json(StartResponse {
                            claim_id: "claim-1".into(),
                            spake,
                        }))
                    },
                ),
            )
            .route(
                FINISH_PATH,
                post(
                    |State(n): State<Arc<FakeNode>>, Json(req): Json<FinishRequest>| async move {
                        let session = n.session.lock().unwrap();
                        let session = session.as_ref().unwrap();
                        if session.verify_portal_mac(&req.mac).is_err() {
                            return Err(refusal(StatusCode::FORBIDDEN, "wrong_code", "no"));
                        }
                        let public_key = n.key.public_b64();
                        Ok(Json(FinishResponse {
                            mac: session.node_mac(&public_key).unwrap(),
                            public_key,
                            name: "gpu-box".into(),
                            agent_version: "0.1.0".into(),
                        }))
                    },
                ),
            )
            .route(
                DONE_PATH,
                post(
                    |State(n): State<Arc<FakeNode>>, Json(req): Json<DoneRequest>| async move {
                        if n.fail_done {
                            return Err(refusal(
                                StatusCode::INTERNAL_SERVER_ERROR,
                                "bad_request",
                                "disk full",
                            ));
                        }
                        let session = n.session.lock().unwrap();
                        let session = session.as_ref().unwrap();
                        if session.verify_done_mac(&req.node_id, &req.mac).is_err() {
                            return Err(refusal(StatusCode::FORBIDDEN, "wrong_code", "no"));
                        }
                        *n.node_id.lock().unwrap() = Some(req.node_id);
                        Ok(Json(serde_json::json!({})))
                    },
                ),
            )
            .with_state(Arc::clone(&node));
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        node
    }

    impl FakeNode {
        fn fingerprint(&self) -> String {
            claim::fingerprint_b64(&self.key.public_b64()).unwrap()
        }

        fn discovered(&self, id: &str) -> DiscoveredNode {
            DiscoveredNode {
                id: id.into(),
                name: "gpu-box".into(),
                gpu: "RTX 4090".into(),
                fingerprint: self.fingerprint(),
                addresses: vec!["::1".into(), "127.0.0.1".into()],
                port: self.port,
                last_seen: unix_ms(),
            }
        }
    }

    impl TestPortal {
        async fn claim(
            &self,
            cookie: Option<&str>,
            origin: Option<&str>,
            id: &str,
            code: &str,
        ) -> Reply {
            let mut req = Request::builder()
                .method("POST")
                .uri("/api/nodes/discovered/claim")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::HOST, "portal.lan:7676");
            if let Some(cookie) = cookie {
                req = req.header(header::COOKIE, cookie);
            }
            if let Some(origin) = origin {
                req = req.header(header::ORIGIN, origin);
            }
            let req = req
                .body(Body::from(json!({ "id": id, "code": code }).to_string()))
                .unwrap();
            let res = self.app.clone().oneshot(req).await.unwrap();
            let status = res.status();
            let bytes = res.into_body().collect().await.unwrap().to_bytes();
            Reply {
                status,
                body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
                cookie: None,
            }
        }

        async fn node_count(&self) -> i64 {
            sqlx::query_scalar("SELECT COUNT(*) FROM nodes")
                .fetch_one(&self.db)
                .await
                .unwrap()
        }
    }

    #[tokio::test]
    async fn a_found_node_is_listed_to_admins_until_it_is_enrolled() {
        let p = portal().await;
        let admin = p.setup_admin().await;
        let (player, _) = p.account(&admin, "player1", "user").await;
        let node = fake_node(None, false).await;
        p.discovered
            .insert(node.discovered("gpu-box._cha-node._tcp.local."));

        let listed = p
            .call("GET", "/api/nodes/discovered", Some(&admin), None)
            .await;
        assert_eq!(listed.status, StatusCode::OK);
        assert_eq!(listed.body["enabled"], true);
        let nodes = listed.body["nodes"].as_array().unwrap();
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0]["id"], "gpu-box._cha-node._tcp.local.");
        assert_eq!(nodes[0]["name"], "gpu-box");
        assert_eq!(nodes[0]["gpu"], "RTX 4090");
        assert_eq!(nodes[0]["fingerprint"], node.fingerprint());
        assert_eq!(nodes[0]["port"], node.port);
        assert!(nodes[0]["lastSeen"].as_i64().unwrap() > 0);

        assert_eq!(
            p.call("GET", "/api/nodes/discovered", Some(&player), None)
                .await
                .status,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            p.call("GET", "/api/nodes/discovered", None, None)
                .await
                .status,
            StatusCode::UNAUTHORIZED
        );

        // Once its key is enrolled it isn't offered again.
        db::insert_node(
            &p.db,
            &db::new_id(),
            "gpu-box",
            &node.key.public_b64(),
            "0.1.0",
        )
        .await
        .unwrap();
        let listed = p
            .call("GET", "/api/nodes/discovered", Some(&admin), None)
            .await;
        assert_eq!(listed.body["nodes"], json!([]));
    }

    #[tokio::test]
    async fn the_right_code_enrolls_the_node_and_tells_it_its_id() {
        let p = portal().await;
        let admin = p.setup_admin().await;
        let node = fake_node(None, false).await;
        p.discovered.insert(node.discovered("n1"));

        let claimed = p
            .claim(
                Some(&admin),
                Some("https://portal.example/"),
                "n1",
                "4821-9375",
            )
            .await;
        assert_eq!(claimed.status, StatusCode::OK, "{}", claimed.body);
        assert_eq!(claimed.body["name"], "gpu-box");
        let node_id = claimed.body["nodeId"].as_str().unwrap();
        assert_eq!(node.node_id.lock().unwrap().as_deref(), Some(node_id));
        assert_eq!(
            node.portal_url.lock().unwrap().as_deref(),
            Some("https://portal.example"),
            "the admin's origin"
        );

        let nodes = p.call("GET", "/api/nodes", Some(&admin), None).await;
        assert_eq!(nodes.body[0]["id"], node_id);
        assert_eq!(nodes.body[0]["agentVersion"], "0.1.0");
        let audit = db::recent_audit(&p.db, 10).await.unwrap();
        let entry = audit.iter().find(|e| e.action == "node.claimed").unwrap();
        assert_eq!(entry.target.as_deref(), Some(node_id));
        assert!(
            entry
                .detail
                .as_deref()
                .unwrap()
                .contains(&node.fingerprint())
        );
        let listed = p
            .call("GET", "/api/nodes/discovered", Some(&admin), None)
            .await;
        assert_eq!(listed.body["nodes"], json!([]));
    }

    #[tokio::test]
    async fn without_an_origin_the_host_header_names_the_portal() {
        let p = portal().await;
        let admin = p.setup_admin().await;
        let node = fake_node(None, false).await;
        p.discovered.insert(node.discovered("n1"));
        let claimed = p.claim(Some(&admin), None, "n1", "48219375").await;
        assert_eq!(claimed.status, StatusCode::OK, "{}", claimed.body);
        assert_eq!(
            node.portal_url.lock().unwrap().as_deref(),
            Some("http://portal.lan:7676")
        );
    }

    #[tokio::test]
    async fn a_wrong_code_enrolls_nothing() {
        let p = portal().await;
        let admin = p.setup_admin().await;
        let node = fake_node(None, false).await;
        p.discovered.insert(node.discovered("n1"));
        let claimed = p.claim(Some(&admin), None, "n1", "00000000").await;
        assert_eq!(claimed.status, StatusCode::FORBIDDEN);
        assert_eq!(claimed.body["error"], "wrong_code");
        assert_eq!(p.node_count().await, 0);
        assert!(node.node_id.lock().unwrap().is_none());
    }

    #[tokio::test]
    async fn bad_requests_are_told_apart() {
        let p = portal().await;
        let admin = p.setup_admin().await;
        let (player, _) = p.account(&admin, "player1", "user").await;
        let node = fake_node(None, false).await;
        p.discovered.insert(node.discovered("n1"));

        let bad = p.claim(Some(&admin), None, "n1", "1234").await;
        assert_eq!(bad.status, StatusCode::BAD_REQUEST);
        assert_eq!(bad.body["error"], "bad_code");
        let missing = p.claim(Some(&admin), None, "nope", "48219375").await;
        assert_eq!(missing.status, StatusCode::NOT_FOUND);
        assert_eq!(missing.body["error"], "not_found");
        let denied = p.claim(Some(&player), None, "n1", "48219375").await;
        assert_eq!(denied.status, StatusCode::FORBIDDEN);
        assert_eq!(denied.body["error"], "admin_only");
        assert_eq!(
            p.claim(None, None, "n1", "48219375").await.status,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(p.node_count().await, 0);
    }

    #[tokio::test]
    async fn a_key_already_enrolled_is_a_conflict() {
        let p = portal().await;
        let admin = p.setup_admin().await;
        let node = fake_node(None, false).await;
        db::insert_node(&p.db, &db::new_id(), "old", &node.key.public_b64(), "0.1.0")
            .await
            .unwrap();
        p.discovered.insert(node.discovered("n1"));
        let claimed = p.claim(Some(&admin), None, "n1", "48219375").await;
        assert_eq!(claimed.status, StatusCode::CONFLICT);
        assert_eq!(claimed.body["error"], "already_enrolled");
        assert_eq!(p.node_count().await, 1);
    }

    #[tokio::test]
    async fn what_the_node_refuses_is_passed_on() {
        let p = portal().await;
        let admin = p.setup_admin().await;
        let node = fake_node(
            Some((StatusCode::CONFLICT, "open the portal over https")),
            false,
        )
        .await;
        p.discovered.insert(node.discovered("n1"));
        let claimed = p.claim(Some(&admin), None, "n1", "48219375").await;
        assert_eq!(claimed.status, StatusCode::CONFLICT);
        assert_eq!(claimed.body["error"], "node_refused");
        assert_eq!(claimed.body["message"], "open the portal over https");
    }

    #[tokio::test]
    async fn an_unreachable_or_swapped_node_is_a_bad_gateway() {
        let p = portal().await;
        let admin = p.setup_admin().await;
        let node = fake_node(None, false).await;
        let mut gone = node.discovered("gone");
        gone.port = 1;
        gone.addresses = vec!["127.0.0.1".into()];
        p.discovered.insert(gone);
        let claimed = p.claim(Some(&admin), None, "gone", "48219375").await;
        assert_eq!(claimed.status, StatusCode::BAD_GATEWAY);
        assert_eq!(claimed.body["error"], "node_unreachable");

        // Another machine answering where the advertised one was.
        let mut swapped = node.discovered("swapped");
        swapped.fingerprint = "ffffffffffffffff".into();
        p.discovered.insert(swapped);
        let claimed = p.claim(Some(&admin), None, "swapped", "48219375").await;
        assert_eq!(claimed.status, StatusCode::BAD_GATEWAY);
        assert_eq!(p.node_count().await, 0);
    }

    #[tokio::test]
    async fn a_node_that_misses_its_id_stays_enrolled_and_the_error_says_so() {
        let p = portal().await;
        let admin = p.setup_admin().await;
        let node = fake_node(None, true).await;
        p.discovered.insert(node.discovered("n1"));
        let claimed = p.claim(Some(&admin), None, "n1", "48219375").await;
        assert_eq!(claimed.status, StatusCode::BAD_GATEWAY);
        let message = claimed.body["message"].as_str().unwrap();
        assert!(message.contains("claim it again"), "{message}");
        assert_eq!(p.node_count().await, 1);
    }
}

#[tokio::test]
async fn admins_set_the_idle_shutoff_and_it_defaults_to_thirty_minutes() {
    let p = portal().await;
    let admin = p.setup_admin().await;
    let (alice, _) = p.account(&admin, "alice", "user").await;

    let seen = p
        .call("GET", "/api/admin/settings", Some(&admin), None)
        .await;
    assert_eq!(seen.status, StatusCode::OK);
    assert_eq!(seen.body, json!({ "idleShutdownMinutes": 30 }));

    let set = p
        .call(
            "PUT",
            "/api/admin/settings",
            Some(&admin),
            Some(json!({ "idleShutdownMinutes": 0 })),
        )
        .await;
    assert_eq!(set.status, StatusCode::OK, "{}", set.body);
    let seen = p
        .call("GET", "/api/admin/settings", Some(&admin), None)
        .await;
    assert_eq!(
        seen.body["idleShutdownMinutes"], 0,
        "0 is off, and it stays"
    );

    let too_long = p
        .call(
            "PUT",
            "/api/admin/settings",
            Some(&admin),
            Some(json!({ "idleShutdownMinutes": 100_000 })),
        )
        .await;
    assert_eq!(too_long.status, StatusCode::BAD_REQUEST);

    // Admins only.
    let denied = p
        .call("GET", "/api/admin/settings", Some(&alice), None)
        .await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);
}

// ---- Cha Player devices (C2.1) ----

impl TestPortal {
    /// Calls `path` with a device token as the bearer credential.
    async fn call_bearer(
        &self,
        method: &str,
        path: &str,
        token: &str,
        body: Option<Value>,
    ) -> Reply {
        let req = Request::builder()
            .method(method)
            .uri(path)
            .header(header::AUTHORIZATION, format!("Bearer {token}"));
        let req = match body {
            Some(body) => req
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string())),
            None => req.body(Body::empty()),
        }
        .unwrap();
        let res = self.app.clone().oneshot(req).await.unwrap();
        let status = res.status();
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        Reply {
            status,
            body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
            cookie: None,
        }
    }

    /// A ticket for the cookie's user.
    async fn ticket(&self, cookie: &str) -> String {
        let reply = self
            .call(
                "POST",
                "/api/devices/tickets",
                Some(cookie),
                Some(json!({})),
            )
            .await;
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
        assert_eq!(reply.body["expires_in"], 60);
        reply.body["ticket"].as_str().unwrap().to_string()
    }

    /// Signs a player in with a ticket; its reply.
    async fn swap(&self, ticket: &str, install_id: &str, name: &str) -> Reply {
        self.call(
            "POST",
            "/api/device/ticket",
            None,
            Some(json!({ "ticket": ticket, "install_id": install_id, "name": name })),
        )
        .await
    }

    /// A signed-in player's token for the cookie's user.
    async fn device_token(&self, cookie: &str, install_id: &str) -> (String, String) {
        let ticket = self.ticket(cookie).await;
        let reply = self.swap(&ticket, install_id, "Test Mac").await;
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
        (
            reply.body["token"].as_str().unwrap().to_string(),
            reply.body["device_id"].as_str().unwrap().to_string(),
        )
    }

    async fn poll(&self, device_code: &str) -> Reply {
        self.call(
            "POST",
            "/api/device/token",
            None,
            Some(json!({ "device_code": device_code })),
        )
        .await
    }
}

#[tokio::test]
async fn a_ticket_signs_a_player_in_once() {
    let p = portal().await;
    let admin = p.setup_admin().await;

    // Tickets need a session.
    let anon = p
        .call("POST", "/api/devices/tickets", None, Some(json!({})))
        .await;
    assert_eq!(anon.status, StatusCode::UNAUTHORIZED);
    let bad_launch = p
        .call(
            "POST",
            "/api/devices/tickets",
            Some(&admin),
            Some(json!({ "launch": "no-such-app" })),
        )
        .await;
    assert_eq!(bad_launch.status, StatusCode::BAD_REQUEST);
    let with_launch = p
        .call(
            "POST",
            "/api/devices/tickets",
            Some(&admin),
            Some(json!({ "launch": "chrome" })),
        )
        .await;
    assert_eq!(with_launch.status, StatusCode::OK, "{}", with_launch.body);

    let ticket = p.ticket(&admin).await;
    let swapped = p.swap(&ticket, "install-1", "Alex's MacBook Pro").await;
    assert_eq!(swapped.status, StatusCode::OK, "{}", swapped.body);
    let token = swapped.body["token"].as_str().unwrap();
    assert!(token.starts_with("chadev_"));
    assert_eq!(token.len(), 7 + 43);
    assert_eq!(swapped.body["user"]["username"], "admin");
    assert_eq!(swapped.body["user"]["role"], "admin");
    assert!(swapped.body["device_id"].is_string());

    let me = p.call_bearer("GET", "/api/me", token, None).await;
    assert_eq!(me.status, StatusCode::OK, "{}", me.body);
    assert_eq!(me.body["username"], "admin");

    // One use.
    let again = p.swap(&ticket, "install-1", "Alex's MacBook Pro").await;
    assert_eq!(again.status, StatusCode::BAD_REQUEST);
    assert_eq!(again.body["error"], "invalid_ticket");

    // Unknown tickets, and bad names and install ids.
    let unknown = p.swap("nope", "install-1", "Mac").await;
    assert_eq!(unknown.body["error"], "invalid_ticket");
    let t = p.ticket(&admin).await;
    let long = "x".repeat(101);
    assert_eq!(
        p.swap(&t, "install-1", &long).await.body["error"],
        "invalid_name"
    );
    assert_eq!(
        p.swap(&t, "", "Mac").await.body["error"],
        "invalid_install_id"
    );
    // A refused request doesn't spend the ticket.
    assert_eq!(p.swap(&t, "install-1", "Mac").await.status, StatusCode::OK);
}

#[tokio::test]
async fn tickets_expire_after_a_minute() {
    let p = portal().await;
    let admin = p.setup_admin().await;
    let ticket = p.ticket(&admin).await;
    sqlx::query("UPDATE device_tickets SET expires_at = ?")
        .bind(db::now() - 1)
        .execute(&p.db)
        .await
        .unwrap();
    let reply = p.swap(&ticket, "install-1", "Mac").await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    assert_eq!(reply.body["error"], "invalid_ticket");
}

#[tokio::test]
async fn signing_in_again_from_one_install_replaces_its_token() {
    let p = portal().await;
    let admin = p.setup_admin().await;
    let (first, first_id) = p.device_token(&admin, "install-1").await;
    let (second, second_id) = p.device_token(&admin, "install-1").await;
    assert_ne!(first, second);
    assert_eq!(first_id, second_id, "one row per (user, install)");

    let old = p.call_bearer("GET", "/api/me", &first, None).await;
    assert_eq!(old.status, StatusCode::UNAUTHORIZED);
    assert_eq!(old.body["error"], "unauthorized");
    let new = p.call_bearer("GET", "/api/me", &second, None).await;
    assert_eq!(new.status, StatusCode::OK);

    // Another install is another device.
    let (third, third_id) = p.device_token(&admin, "install-2").await;
    assert_ne!(third_id, first_id);
    assert_eq!(
        p.call_bearer("GET", "/api/me", &third, None).await.status,
        StatusCode::OK
    );
    assert_eq!(
        p.call_bearer("GET", "/api/me", &second, None).await.status,
        StatusCode::OK
    );
    let list = p.call("GET", "/api/devices", Some(&admin), None).await;
    assert_eq!(list.body.as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn a_player_signs_in_with_a_device_code() {
    let p = portal().await;
    let admin = p.setup_admin().await;

    let started = p
        .call(
            "POST",
            "/api/device/code",
            None,
            Some(json!({ "install_id": "install-1", "name": "Alex's MacBook Pro" })),
        )
        .await;
    assert_eq!(started.status, StatusCode::OK, "{}", started.body);
    let device_code = started.body["device_code"].as_str().unwrap().to_string();
    let user_code = started.body["user_code"].as_str().unwrap().to_string();
    assert_eq!(started.body["verification_path"], "/link");
    assert_eq!(started.body["expires_in"], 600);
    assert_eq!(started.body["interval"], 5);
    assert_eq!(user_code.len(), 9);
    assert_eq!(user_code.as_bytes()[4], b'-');
    assert!(
        user_code
            .chars()
            .all(|c| c == '-' || "BCDFGHJKLMNPQRSTVWXZ".contains(c))
    );

    // Pending; asking again at once is too fast.
    let pending = p.poll(&device_code).await;
    assert_eq!(pending.status, StatusCode::BAD_REQUEST);
    assert_eq!(pending.body["error"], "authorization_pending");
    let fast = p.poll(&device_code).await;
    assert_eq!(fast.status, StatusCode::BAD_REQUEST);
    assert_eq!(fast.body["error"], "slow_down");

    // The signed-in user sees the device's name, however they type the code.
    let typed = user_code.to_lowercase().replace('-', " ");
    let path = format!("/api/devices/codes/{}", typed.replace(' ', ""));
    let seen = p.call("GET", &path, Some(&admin), None).await;
    assert_eq!(seen.status, StatusCode::OK, "{}", seen.body);
    assert_eq!(seen.body["name"], "Alex's MacBook Pro");
    assert!(seen.body["expires_at"].as_i64().unwrap() > db::now());
    let anon = p.call("GET", &path, None, None).await;
    assert_eq!(anon.status, StatusCode::UNAUTHORIZED);
    let unknown = p
        .call("GET", "/api/devices/codes/BCDF-GHJK", Some(&admin), None)
        .await;
    assert_eq!(unknown.status, StatusCode::NOT_FOUND);
    assert_eq!(unknown.body["error"], "unknown_code");

    let approve = format!("/api/devices/codes/{user_code}/approve");
    let approved = p
        .call("POST", &approve, Some(&admin), Some(json!({})))
        .await;
    assert_eq!(approved.status, StatusCode::OK, "{}", approved.body);
    assert_eq!(approved.body["ok"], true);

    // Approved: the next poll gets the token at once, whatever the timing.
    let done = p.poll(&device_code).await;
    assert_eq!(done.status, StatusCode::OK, "{}", done.body);
    let token = done.body["token"].as_str().unwrap();
    assert!(token.starts_with("chadev_"));
    assert_eq!(done.body["user"]["username"], "admin");
    let me = p.call_bearer("GET", "/api/me", token, None).await;
    assert_eq!(me.status, StatusCode::OK);

    // Once.
    let spent = p.poll(&device_code).await;
    assert_eq!(spent.body["error"], "expired_token");
    // And the code can't be approved again.
    let late = p
        .call("POST", &approve, Some(&admin), Some(json!({})))
        .await;
    assert_eq!(late.status, StatusCode::NOT_FOUND);

    let devices = p.call("GET", "/api/devices", Some(&admin), None).await;
    assert_eq!(devices.body[0]["name"], "Alex's MacBook Pro");
}

#[tokio::test]
async fn a_denied_or_expired_code_ends_the_sign_in() {
    let p = portal().await;
    let admin = p.setup_admin().await;
    let start = |install: &'static str| {
        let p = &p;
        async move {
            let r = p
                .call(
                    "POST",
                    "/api/device/code",
                    None,
                    Some(json!({ "install_id": install, "name": "Mac" })),
                )
                .await;
            (
                r.body["device_code"].as_str().unwrap().to_string(),
                r.body["user_code"].as_str().unwrap().to_string(),
            )
        }
    };

    let (device_code, user_code) = start("install-1").await;
    let denied = p
        .call(
            "POST",
            &format!("/api/devices/codes/{user_code}/deny"),
            Some(&admin),
            Some(json!({})),
        )
        .await;
    assert_eq!(denied.status, StatusCode::OK, "{}", denied.body);
    let reply = p.poll(&device_code).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    assert_eq!(reply.body["error"], "access_denied");
    // A denied code can't then be approved.
    let approve = p
        .call(
            "POST",
            &format!("/api/devices/codes/{user_code}/approve"),
            Some(&admin),
            Some(json!({})),
        )
        .await;
    assert_eq!(approve.status, StatusCode::NOT_FOUND);

    let (device_code, user_code) = start("install-2").await;
    sqlx::query("UPDATE device_codes SET expires_at = ?")
        .bind(db::now() - 1)
        .execute(&p.db)
        .await
        .unwrap();
    let reply = p.poll(&device_code).await;
    assert_eq!(reply.body["error"], "expired_token");
    let seen = p
        .call(
            "GET",
            &format!("/api/devices/codes/{user_code}"),
            Some(&admin),
            None,
        )
        .await;
    assert_eq!(seen.body["error"], "unknown_code");
    let approve = p
        .call(
            "POST",
            &format!("/api/devices/codes/{user_code}/approve"),
            Some(&admin),
            Some(json!({})),
        )
        .await;
    assert_eq!(approve.status, StatusCode::NOT_FOUND);

    let nobody = p.poll("never-issued").await;
    assert_eq!(nobody.body["error"], "expired_token");
}

#[tokio::test]
async fn a_device_token_opens_the_players_routes_and_nothing_else() {
    let p = portal().await;
    let admin = p.setup_admin().await;
    let (token, _) = p.device_token(&admin, "install-1").await;

    for path in [
        "/api/me",
        "/api/catalog",
        "/api/catalog/chrome/icon",
        "/api/environments",
        "/api/ice",
        "/api/controllers/apps",
        "/api/storage",
        "/api/apps/settings",
    ] {
        let reply = p.call_bearer("GET", path, &token, None).await;
        assert_eq!(reply.status, StatusCode::OK, "GET {path}: {}", reply.body);
    }
    let missing = p
        .call_bearer("GET", "/api/environments/nope", &token, None)
        .await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND, "reaches the handler");
    let stop = p
        .call_bearer("DELETE", "/api/environments/nope", &token, None)
        .await;
    assert_eq!(stop.status, StatusCode::NOT_FOUND);

    // Cookie-only, for an admin's token too.
    for (method, path, body) in [
        ("GET", "/api/users", None),
        (
            "POST",
            "/api/users",
            Some(json!({ "username": "eve", "password": "a long password", "role": "user" })),
        ),
        ("GET", "/api/audit", None),
        ("GET", "/api/devices", None),
        ("GET", "/api/devices?all=1", None),
        ("POST", "/api/devices/tickets", Some(json!({}))),
        (
            "POST",
            "/api/devices/codes/BCDF-GHJK/approve",
            Some(json!({})),
        ),
        ("GET", "/api/me/prefs", None),
        (
            "PUT",
            "/api/apps/settings/chrome",
            Some(json!({ "fps": 60 })),
        ),
        (
            "PUT",
            "/api/controllers/apps/chrome",
            Some(json!({ "kind": null })),
        ),
        (
            "PUT",
            "/api/storage/chrome",
            Some(json!({ "persistent": true })),
        ),
        ("GET", "/api/admin/storage", None),
        ("GET", "/api/nodes", None),
    ] {
        let reply = p.call_bearer(method, path, &token, body).await;
        assert_eq!(
            reply.status,
            StatusCode::UNAUTHORIZED,
            "{method} {path}: {}",
            reply.body
        );
        assert_eq!(reply.body["error"], "unauthorized");
    }
    let nobody = p.call("GET", "/api/users", None, None).await;
    assert_eq!(nobody.status, StatusCode::UNAUTHORIZED);

    // Not any bearer: unknown tokens, other schemes, session tokens.
    for value in [
        "Bearer chadev_nope",
        "Bearer nope",
        "Basic abc",
        "chadev_nope",
    ] {
        let req = Request::builder()
            .uri("/api/me")
            .header(header::AUTHORIZATION, value)
            .body(Body::empty())
            .unwrap();
        let res = p.app.clone().oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED, "{value}");
    }
    let session_token = admin.split('=').nth(1).unwrap();
    let as_bearer = p.call_bearer("GET", "/api/me", session_token, None).await;
    assert_eq!(as_bearer.status, StatusCode::UNAUTHORIZED);

    // The cookie still opens the same routes.
    assert_eq!(
        p.call("GET", "/api/me", Some(&admin), None).await.status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn a_disabled_account_s_token_stops_working() {
    let p = portal().await;
    let admin = p.setup_admin().await;
    let (cookie, user_id) = p.account(&admin, "alice", "user").await;
    let (token, _) = p.device_token(&cookie, "install-1").await;
    assert_eq!(
        p.call_bearer("GET", "/api/me", &token, None).await.status,
        StatusCode::OK
    );
    sqlx::query("UPDATE users SET disabled = 1 WHERE id = ?")
        .bind(&user_id)
        .execute(&p.db)
        .await
        .unwrap();
    assert_eq!(
        p.call_bearer("GET", "/api/me", &token, None).await.status,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn revoking_a_device_ends_its_token() {
    let p = portal().await;
    let admin = p.setup_admin().await;
    let (alice, _) = p.account(&admin, "alice", "user").await;
    let (bob, _) = p.account(&admin, "bob", "user").await;
    let (alice_token, alice_device) = p.device_token(&alice, "install-a").await;
    let (bob_token, bob_device) = p.device_token(&bob, "install-b").await;

    // Everyone sees their own devices only.
    let mine = p.call("GET", "/api/devices", Some(&alice), None).await;
    assert_eq!(mine.status, StatusCode::OK);
    let rows = mine.body.as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["id"], alice_device.as_str());
    assert_eq!(rows[0]["name"], "Test Mac");
    assert!(rows[0]["created_at"].is_i64());
    assert!(rows[0]["last_used_at"].is_i64());
    assert!(rows[0].get("user").is_none());
    assert!(rows[0].get("token_hash").is_none());
    // `all` is for admins.
    let all = p
        .call("GET", "/api/devices?all=1", Some(&alice), None)
        .await;
    assert_eq!(all.status, StatusCode::FORBIDDEN);
    let all = p
        .call("GET", "/api/devices?all=1", Some(&admin), None)
        .await;
    assert_eq!(all.status, StatusCode::OK);
    let users: Vec<&str> = all
        .body
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["user"].as_str().unwrap())
        .collect();
    assert_eq!(users, ["alice", "bob"]);

    // Not someone else's.
    let theirs = p
        .call(
            "DELETE",
            &format!("/api/devices/{bob_device}"),
            Some(&alice),
            None,
        )
        .await;
    assert_eq!(theirs.status, StatusCode::NOT_FOUND);
    assert_eq!(
        p.call_bearer("GET", "/api/me", &bob_token, None)
            .await
            .status,
        StatusCode::OK
    );
    // A device token can't revoke either.
    let by_token = p
        .call_bearer(
            "DELETE",
            &format!("/api/devices/{alice_device}"),
            &alice_token,
            None,
        )
        .await;
    assert_eq!(by_token.status, StatusCode::UNAUTHORIZED);

    // Your own.
    let revoked = p
        .call(
            "DELETE",
            &format!("/api/devices/{alice_device}"),
            Some(&alice),
            None,
        )
        .await;
    assert_eq!(revoked.status, StatusCode::NO_CONTENT);
    let after = p.call_bearer("GET", "/api/me", &alice_token, None).await;
    assert_eq!(after.status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        p.call("GET", "/api/devices", Some(&alice), None)
            .await
            .body
            .as_array()
            .unwrap()
            .len(),
        0
    );
    let again = p
        .call(
            "DELETE",
            &format!("/api/devices/{alice_device}"),
            Some(&alice),
            None,
        )
        .await;
    assert_eq!(again.status, StatusCode::NOT_FOUND);

    // Admins revoke anyone's.
    let by_admin = p
        .call(
            "DELETE",
            &format!("/api/devices/{bob_device}"),
            Some(&admin),
            None,
        )
        .await;
    assert_eq!(by_admin.status, StatusCode::NO_CONTENT);
    assert_eq!(
        p.call_bearer("GET", "/api/me", &bob_token, None)
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn device_sign_ins_are_audited() {
    let p = portal().await;
    let admin = p.setup_admin().await;
    let (_, device) = p.device_token(&admin, "install-1").await;
    let started = p
        .call(
            "POST",
            "/api/device/code",
            None,
            Some(json!({ "install_id": "install-2", "name": "Mac" })),
        )
        .await;
    let user_code = started.body["user_code"].as_str().unwrap();
    p.call(
        "POST",
        &format!("/api/devices/codes/{user_code}/approve"),
        Some(&admin),
        Some(json!({})),
    )
    .await;
    p.call(
        "DELETE",
        &format!("/api/devices/{device}"),
        Some(&admin),
        None,
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
    for expected in [
        "device.ticket_issued",
        "device.token_issued",
        "device.code_requested",
        "device.code_approved",
        "device.revoked",
    ] {
        assert!(actions.contains(&expected), "{expected} in {actions:?}");
    }
}

#[tokio::test]
async fn a_device_token_updates_its_last_use_at_most_once_a_minute() {
    let p = portal().await;
    let admin = p.setup_admin().await;
    let (token, device) = p.device_token(&admin, "install-1").await;
    let old = db::now() - 3600;
    sqlx::query("UPDATE devices SET last_used_at = ?, last_ip = NULL WHERE id = ?")
        .bind(old)
        .bind(&device)
        .execute(&p.db)
        .await
        .unwrap();
    p.call_bearer("GET", "/api/me", &token, None).await;
    let used: i64 = sqlx::query_scalar("SELECT last_used_at FROM devices WHERE id = ?")
        .bind(&device)
        .fetch_one(&p.db)
        .await
        .unwrap();
    assert!(used > old, "an hour-old use is refreshed");

    // Within a minute, left alone.
    let recent = db::now() - 10;
    sqlx::query("UPDATE devices SET last_used_at = ? WHERE id = ?")
        .bind(recent)
        .bind(&device)
        .execute(&p.db)
        .await
        .unwrap();
    p.call_bearer("GET", "/api/me", &token, None).await;
    let used: i64 = sqlx::query_scalar("SELECT last_used_at FROM devices WHERE id = ?")
        .bind(&device)
        .fetch_one(&p.db)
        .await
        .unwrap();
    assert_eq!(used, recent);
}

// ---- Share links for players (ADR 0014) ----

impl TestPortal {
    /// Makes a share on `slot` of environment `env` as `cookie`; its reply.
    async fn share(&self, cookie: &str, env: &str, slot: i64) -> Reply {
        self.call(
            "POST",
            &format!("/api/environments/{env}/shares"),
            Some(cookie),
            Some(json!({ "role": "player", "slot": slot })),
        )
        .await
    }
}

/// The token in a share's `url`.
fn token_of(share: &Reply) -> String {
    share.body["url"]
        .as_str()
        .and_then(|u| u.strip_prefix("/s/"))
        .unwrap_or_else(|| panic!("no share url in {}", share.body))
        .to_string()
}

#[tokio::test]
async fn only_the_owner_or_an_admin_makes_a_share_of_a_running_environment() {
    let p = portal().await;
    let admin = p.setup_admin().await;
    let (alice, alice_id) = p.account(&admin, "alice", "user").await;
    let (bob, _) = p.account(&admin, "bob", "user").await;
    let (_, env) = p.live_environment(&alice_id, "chrome").await;

    let made = p.share(&alice, &env, 1).await;
    assert_eq!(made.status, StatusCode::OK, "{}", made.body);
    assert_eq!(made.body["role"], "player");
    assert_eq!(made.body["slot"], 1);
    let ttl = made.body["expires_at"].as_i64().unwrap() - db::now();
    assert!((24 * 3600 - 5..=24 * 3600).contains(&ttl), "{ttl}");
    // 256 bits, base64url.
    assert_eq!(token_of(&made).len(), 43);

    // An admin may; another user can't see the environment; nobody else is signed in.
    assert_eq!(p.share(&admin, &env, 2).await.status, StatusCode::OK);
    assert_eq!(p.share(&bob, &env, 3).await.status, StatusCode::NOT_FOUND);
    let anon = p
        .call(
            "POST",
            &format!("/api/environments/{env}/shares"),
            None,
            Some(json!({ "role": "player", "slot": 1 })),
        )
        .await;
    assert_eq!(anon.status, StatusCode::UNAUTHORIZED);
    let listed = p
        .call(
            "GET",
            &format!("/api/environments/{env}/shares"),
            Some(&bob),
            None,
        )
        .await;
    assert_eq!(listed.status, StatusCode::NOT_FOUND);

    // Only player 2 to 4, and only the player role.
    for slot in [0, 4, -1] {
        let bad = p.share(&alice, &env, slot).await;
        assert_eq!(bad.status, StatusCode::BAD_REQUEST, "slot {slot}");
        assert_eq!(bad.body["error"], "bad_slot");
    }
    let viewer = p
        .call(
            "POST",
            &format!("/api/environments/{env}/shares"),
            Some(&alice),
            Some(json!({ "role": "viewer", "slot": 1 })),
        )
        .await;
    assert_eq!(viewer.body["error"], "bad_role");

    // The list shows the live ones and never a token.
    let listed = p
        .call(
            "GET",
            &format!("/api/environments/{env}/shares"),
            Some(&alice),
            None,
        )
        .await;
    let listed = listed.body.as_array().unwrap();
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0]["slot"], 1);
    assert_eq!(listed[1]["slot"], 2);
    for share in listed {
        assert!(share.get("url").is_none() && share.get("token").is_none());
        assert!(share["id"].is_string() && share["created_at"].is_i64());
    }

    // Only a running environment can be shared.
    let (_, starting) = p.live_environment(&alice_id, "chrome").await;
    db::transition_environment(&p.db, &starting, &["running"], "stopping", None)
        .await
        .unwrap();
    let refused = p.share(&alice, &starting, 1).await;
    assert_eq!(refused.status, StatusCode::CONFLICT);
    assert_eq!(refused.body["error"], "not_running");
}

#[tokio::test]
async fn a_new_share_on_a_slot_replaces_the_old() {
    let p = portal().await;
    let admin = p.setup_admin().await;
    let (alice, alice_id) = p.account(&admin, "alice", "user").await;
    let (_, env) = p.live_environment(&alice_id, "chrome").await;

    let first = p.share(&alice, &env, 1).await;
    let other_slot = p.share(&alice, &env, 2).await;
    let second = p.share(&alice, &env, 1).await;
    assert_ne!(token_of(&first), token_of(&second));

    let old = p
        .call(
            "GET",
            &format!("/api/shares/{}", token_of(&first)),
            None,
            None,
        )
        .await;
    assert_eq!(old.status, StatusCode::NOT_FOUND);
    for live in [&second, &other_slot] {
        let ok = p
            .call(
                "GET",
                &format!("/api/shares/{}", token_of(live)),
                None,
                None,
            )
            .await;
        assert_eq!(ok.status, StatusCode::OK, "{}", ok.body);
    }
    let listed = p
        .call(
            "GET",
            &format!("/api/environments/{env}/shares"),
            Some(&alice),
            None,
        )
        .await;
    let ids: Vec<_> = listed
        .body
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].clone())
        .collect();
    assert_eq!(
        ids,
        vec![second.body["id"].clone(), other_slot.body["id"].clone()]
    );
}

#[tokio::test]
async fn a_token_shows_what_it_is_for_and_every_dead_one_is_the_same_404() {
    let p = portal().await;
    let admin = p.setup_admin().await;
    let (alice, alice_id) = p.account(&admin, "alice", "user").await;
    let (_, env) = p.live_environment(&alice_id, "chrome").await;
    let me = p.call("GET", "/api/me", Some(&alice), None).await;

    let made = p.share(&alice, &env, 2).await;
    // No cookie: the token is the credential.
    let info = p
        .call(
            "GET",
            &format!("/api/shares/{}", token_of(&made)),
            None,
            None,
        )
        .await;
    assert_eq!(info.status, StatusCode::OK, "{}", info.body);
    assert_eq!(info.body["owner"], me.body["displayName"]);
    assert_eq!(info.body["role"], "player");
    assert_eq!(info.body["slot"], 2);
    assert_eq!(info.body["state"], "running");
    let catalog = p.call("GET", "/api/catalog", Some(&alice), None).await;
    let name = catalog
        .body
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == "chrome")
        .unwrap()["name"]
        .clone();
    assert_eq!(info.body["app"], name);

    // Unknown, revoked, expired and stopped answer alike.
    let unknown = p.call("GET", "/api/shares/not-a-token", None, None).await;
    assert_eq!(unknown.status, StatusCode::NOT_FOUND);
    assert_eq!(unknown.body["error"], "unknown_share");

    let revoked = p.share(&alice, &env, 1).await;
    p.call(
        "DELETE",
        &format!(
            "/api/environments/{env}/shares/{}",
            revoked.body["id"].as_str().unwrap()
        ),
        Some(&alice),
        None,
    )
    .await;
    let expired = p.share(&alice, &env, 3).await;
    sqlx::query("UPDATE shares SET expires_at = ? WHERE id = ?")
        .bind(db::now() - 1)
        .bind(expired.body["id"].as_str().unwrap())
        .execute(&p.db)
        .await
        .unwrap();
    for token in [token_of(&revoked), token_of(&expired)] {
        let dead = p
            .call("GET", &format!("/api/shares/{token}"), None, None)
            .await;
        assert_eq!(dead.status, StatusCode::NOT_FOUND);
        assert_eq!(dead.body, unknown.body);
    }
    // An expired one isn't listed either.
    let listed = p
        .call(
            "GET",
            &format!("/api/environments/{env}/shares"),
            Some(&alice),
            None,
        )
        .await;
    assert_eq!(listed.body.as_array().unwrap().len(), 1);

    // Stopped, whether or not the revoking has happened yet.
    sqlx::query("UPDATE environments SET state = 'stopping' WHERE id = ?")
        .bind(&env)
        .execute(&p.db)
        .await
        .unwrap();
    let stopped = p
        .call(
            "GET",
            &format!("/api/shares/{}", token_of(&made)),
            None,
            None,
        )
        .await;
    assert_eq!(stopped.status, StatusCode::NOT_FOUND);
    assert_eq!(stopped.body, unknown.body);
}

#[tokio::test]
async fn revoking_a_share_ends_it_for_the_owner_and_admins_only() {
    let p = portal().await;
    let admin = p.setup_admin().await;
    let (alice, alice_id) = p.account(&admin, "alice", "user").await;
    let (bob, _) = p.account(&admin, "bob", "user").await;
    let (_, env) = p.live_environment(&alice_id, "chrome").await;
    let made = p.share(&alice, &env, 1).await;
    let path = format!(
        "/api/environments/{env}/shares/{}",
        made.body["id"].as_str().unwrap()
    );

    assert_eq!(
        p.call("DELETE", &path, Some(&bob), None).await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        p.call(
            "GET",
            &format!("/api/shares/{}", token_of(&made)),
            None,
            None
        )
        .await
        .status,
        StatusCode::OK
    );
    assert_eq!(
        p.call("DELETE", &path, Some(&alice), None).await.status,
        StatusCode::NO_CONTENT
    );
    // Again: it is gone.
    assert_eq!(
        p.call("DELETE", &path, Some(&alice), None).await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        p.call(
            "GET",
            &format!("/api/shares/{}", token_of(&made)),
            None,
            None
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );
    // The slot is free for a new link, and an admin may revoke it.
    let again = p.share(&alice, &env, 1).await;
    let path = format!(
        "/api/environments/{env}/shares/{}",
        again.body["id"].as_str().unwrap()
    );
    assert_eq!(
        p.call("DELETE", &path, Some(&admin), None).await.status,
        StatusCode::NO_CONTENT
    );
}

#[tokio::test]
async fn an_environment_that_leaves_running_revokes_its_shares() {
    let p = portal().await;
    let admin = p.setup_admin().await;
    let (alice, alice_id) = p.account(&admin, "alice", "user").await;
    let token_after = |how: &'static str| {
        let (p, alice, alice_id, admin) = (&p, alice.clone(), alice_id.clone(), admin.clone());
        async move {
            let (_, env) = p.live_environment(&alice_id, "chrome").await;
            let made = p.share(&alice, &env, 1).await;
            let token = token_of(&made);
            // Stopping through the API, the node's exit report, or the node going.
            match how {
                "stop" => {
                    let stopped = p
                        .call(
                            "DELETE",
                            &format!("/api/environments/{env}"),
                            Some(&alice),
                            None,
                        )
                        .await;
                    assert_eq!(stopped.status, StatusCode::OK, "{}", stopped.body);
                }
                "exit" => {
                    db::record_exit(&p.db, &env, false, "the app exited", None)
                        .await
                        .unwrap();
                }
                "failed" => {
                    db::transition_environment(&p.db, &env, &["running"], "failed", Some("x"))
                        .await
                        .unwrap();
                }
                _ => unreachable!(),
            }
            let live: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM shares WHERE environment_id = ? AND revoked_at IS NULL",
            )
            .bind(&env)
            .fetch_one(&p.db)
            .await
            .unwrap();
            assert_eq!(live, 0, "{how}");
            let reply = p
                .call("GET", &format!("/api/shares/{token}"), None, None)
                .await;
            assert_eq!(reply.status, StatusCode::NOT_FOUND, "{how}");
            let listed = p
                .call(
                    "GET",
                    &format!("/api/environments/{env}/shares"),
                    Some(&admin),
                    None,
                )
                .await;
            assert_eq!(listed.body, json!([]), "{how}");
        }
    };
    for how in ["stop", "exit", "failed"] {
        token_after(how).await;
    }
    let audit = p.call("GET", "/api/audit", Some(&admin), None).await;
    let revoked = audit
        .body
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["action"] == "share.revoked")
        .count();
    assert_eq!(revoked, 3);
}

#[tokio::test]
async fn sharing_is_audited_and_the_token_never_is() {
    let p = portal().await;
    let admin = p.setup_admin().await;
    let (alice, alice_id) = p.account(&admin, "alice", "user").await;
    let (_, env) = p.live_environment(&alice_id, "chrome").await;
    let first = p.share(&alice, &env, 1).await;
    let second = p.share(&alice, &env, 1).await;
    p.call(
        "DELETE",
        &format!(
            "/api/environments/{env}/shares/{}",
            second.body["id"].as_str().unwrap()
        ),
        Some(&alice),
        None,
    )
    .await;
    let audit = p.call("GET", "/api/audit", Some(&admin), None).await;
    let actions: Vec<&str> = audit
        .body
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["action"].as_str())
        .filter(|a| a.starts_with("share."))
        .collect();
    // Newest first: created, replaced (revoked), created, revoked.
    assert_eq!(
        actions,
        [
            "share.revoked",
            "share.created",
            "share.revoked",
            "share.created"
        ]
    );
    let text = audit.body.to_string();
    assert!(!text.contains(&token_of(&first)) && !text.contains(&token_of(&second)));
    // Only the hash is stored.
    let stored: Vec<String> = sqlx::query_scalar("SELECT token_hash FROM shares")
        .fetch_all(&p.db)
        .await
        .unwrap();
    assert!(!stored.contains(&token_of(&first)) && stored.len() == 2);
}

#[tokio::test]
async fn the_unauthenticated_share_routes_are_limited_per_address() {
    let p = portal().await;
    for _ in 0..30 {
        let r = p
            .call_from("203.0.113.7:1000", "/api/shares/guess", None)
            .await;
        assert_eq!(r.status, StatusCode::NOT_FOUND);
    }
    let limited = p
        .call_from("203.0.113.7:1000", "/api/shares/guess", None)
        .await;
    assert_eq!(limited.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(limited.body["error"], "rate_limited");
    // The connect route shares the budget.
    let connect = p
        .call_from(
            "203.0.113.7:1001",
            "/api/shares/guess/connect",
            Some(json!({ "codec": "h264", "offer": { "sdp": "x" } })),
        )
        .await;
    assert_eq!(connect.status, StatusCode::TOO_MANY_REQUESTS);
    // Another address has its own.
    let other = p
        .call_from("203.0.113.8:1000", "/api/shares/guess", None)
        .await;
    assert_eq!(other.status, StatusCode::NOT_FOUND);
}

/// A node, connected over a real WebSocket, that answers a connect.
mod node {
    use std::net::SocketAddr;
    use std::time::Duration;

    use cha_wire::{NodeKey, NodeRequest, NodeResponse, ToNode, ToPortal};
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

    pub type Socket = tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >;

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

    /// Connects as the enrolled node `id` with `key`.
    pub async fn join(addr: SocketAddr, id: &str, key: &NodeKey) -> Socket {
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

    /// The next request the portal makes of the node.
    pub async fn request(ws: &mut Socket) -> (u64, NodeRequest) {
        loop {
            if let ToNode::Request { id, request } = next(ws).await {
                return (id, request);
            }
        }
    }

    pub async fn respond(ws: &mut Socket, id: u64, result: Result<NodeResponse, String>) {
        let msg = ToPortal::Response { id, result };
        ws.send(Message::text(serde_json::to_string(&msg).unwrap()))
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn a_guest_connects_with_a_player_token_that_carries_the_slot() {
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
    };
    let pool = db::open(&config.database).await.unwrap();
    let state = AppState::new(config, pool.clone()).await.unwrap();
    let portal_key = state.media_key.public_b64();
    let router = app(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let served = router.clone();
    tokio::spawn(async move {
        axum::serve(
            listener,
            served.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
    });
    let p = TestPortal {
        app: router,
        db: pool.clone(),
        discovered: Default::default(),
        _dir: dir,
    };
    let admin = p.setup_admin().await;
    let (alice, alice_id) = p.account(&admin, "alice", "user").await;

    let mut secret = [0u8; 32];
    getrandom::fill(&mut secret).unwrap();
    let key = cha_wire::NodeKey::from_secret(secret);
    let node_id = db::new_id();
    sqlx::query("INSERT INTO nodes (id, name, public_key, enrolled_at) VALUES (?, 'box', ?, 0)")
        .bind(&node_id)
        .bind(key.public_b64())
        .execute(&pool)
        .await
        .unwrap();
    let env = db::new_id();
    db::insert_environment(&pool, &env, &alice_id, "chrome", &node_id, None, "running")
        .await
        .unwrap();
    let mut ws = node::join(addr, &node_id, &key).await;

    let made = p.share(&alice, &env, 2).await;
    let share_id = made.body["id"].as_str().unwrap().to_string();
    let path = format!("/api/shares/{}/connect", token_of(&made));

    // WebRTC: the node is handed the offer and a token for a player on slot 2.
    let guest = {
        let app = p.app.clone();
        let path = path.clone();
        tokio::spawn(async move {
            let req = Request::builder()
                .method("POST")
                .uri(path)
                .header(header::CONTENT_TYPE, "application/json")
                .extension(ConnectInfo(
                    "198.51.100.4:4000".parse::<std::net::SocketAddr>().unwrap(),
                ))
                .body(Body::from(
                    json!({ "codec": "h264", "offer": { "type": "offer", "sdp": "v=0" } })
                        .to_string(),
                ))
                .unwrap();
            let res = app.oneshot(req).await.unwrap();
            let status = res.status();
            let bytes = res.into_body().collect().await.unwrap().to_bytes();
            (status, serde_json::from_slice::<Value>(&bytes).unwrap())
        })
    };
    let (id, request) = node::request(&mut ws).await;
    let cha_wire::NodeRequest::Connect {
        environment_id,
        media_token,
        codec,
        ..
    } = request
    else {
        panic!("expected a connect, got {request:?}");
    };
    assert_eq!(
        (environment_id.as_str(), codec.as_str()),
        (env.as_str(), "h264")
    );
    let claims = cha_wire::verify_media_token(&portal_key, &media_token, &env, db::now()).unwrap();
    assert_eq!(claims.role, "player");
    assert_eq!(claims.slot, Some(2));
    assert_eq!(claims.sub, format!("share:{share_id}"));
    assert_eq!(claims.env, env);
    node::respond(
        &mut ws,
        id,
        Ok(cha_wire::NodeResponse::Answer {
            answer: json!({ "type": "answer", "sdp": "v=0" }),
        }),
    )
    .await;
    let (status, body) = guest.await.unwrap();
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["transport"], "webrtc");
    assert_eq!(body["answer"]["type"], "answer");

    // WebTransport: the token rides in the URLs.
    let guest = {
        let app = p.app.clone();
        let path = path.clone();
        tokio::spawn(async move {
            let req = Request::builder()
                .method("POST")
                .uri(path)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({ "codec": "h264", "transport": "webtransport" }).to_string(),
                ))
                .unwrap();
            let res = app.oneshot(req).await.unwrap();
            let bytes = res.into_body().collect().await.unwrap().to_bytes();
            serde_json::from_slice::<Value>(&bytes).unwrap()
        })
    };
    let (id, request) = node::request(&mut ws).await;
    assert!(matches!(
        request,
        cha_wire::NodeRequest::StreamerInfo { .. }
    ));
    node::respond(
        &mut ws,
        id,
        Ok(cha_wire::NodeResponse::StreamerInfo {
            info: json!({ "wt_port": 7602, "cert_hash_hex": "ab", "addresses": ["192.168.1.20"] }),
        }),
    )
    .await;
    let body = guest.await.unwrap();
    let url = body["urls"][0].as_str().unwrap();
    let token = url.split("token=").nth(1).unwrap();
    let claims = cha_wire::verify_media_token(&portal_key, token, &env, db::now()).unwrap();
    assert_eq!((claims.role.as_str(), claims.slot), ("player", Some(2)));

    // Each join is audited with the address and no token.
    let audit = p.call("GET", "/api/audit", Some(&admin), None).await;
    let joined: Vec<_> = audit
        .body
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["action"] == "share.joined")
        .collect();
    assert_eq!(joined.len(), 2);
    assert!(joined.iter().any(|e| e["ip"] == "198.51.100.4"));
    assert!(!audit.body.to_string().contains(&token_of(&made)));

    // A revoked link connects no more, and the node isn't asked.
    p.call(
        "DELETE",
        &format!("/api/environments/{env}/shares/{share_id}"),
        Some(&alice),
        None,
    )
    .await;
    let gone = p
        .call(
            "POST",
            &path,
            None,
            Some(json!({ "codec": "h264", "offer": { "sdp": "v=0" } })),
        )
        .await;
    assert_eq!(gone.status, StatusCode::NOT_FOUND);
    assert_eq!(gone.body["error"], "unknown_share");
    let silent = tokio::time::timeout(
        std::time::Duration::from_millis(300),
        node::request(&mut ws),
    )
    .await;
    assert!(silent.is_err());
}

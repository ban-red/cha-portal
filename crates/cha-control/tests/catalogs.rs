//! Catalogs an admin loads (ADR 0019): add, refresh, approve, remove, and
//! what players then see.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode, header};
use cha_control::{AppState, Config, app, db};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

struct Portal {
    app: Router,
    db: sqlx::SqlitePool,
    _dir: tempfile::TempDir,
}

async fn portal() -> Portal {
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
    let state = AppState::new(config, pool.clone()).await.unwrap();
    Portal {
        app: app(state),
        db: pool,
        _dir: dir,
    }
}

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    bytes: Vec<u8>,
    body: Value,
    cookie: Option<String>,
}

impl Portal {
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
        let headers = res.headers().clone();
        let cookie = headers
            .get(header::SET_COOKIE)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(';').next())
            .map(str::to_string);
        let bytes = res.into_body().collect().await.unwrap().to_bytes().to_vec();
        let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        Reply {
            status,
            headers,
            bytes,
            body,
            cookie,
        }
    }

    async fn setup_admin(&self) -> String {
        let r = self
            .call(
                "POST",
                "/api/setup",
                None,
                Some(json!({ "username": "admin", "password": "correct horse battery" })),
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.body);
        r.cookie.unwrap()
    }

    async fn setup_player(&self, admin: &str) -> String {
        let r = self
            .call(
                "POST",
                "/api/users",
                Some(admin),
                Some(json!({ "username": "player1", "password": "another long password", "role": "user" })),
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.body);
        let login = self
            .call(
                "POST",
                "/api/auth/login",
                None,
                Some(json!({ "username": "player1", "password": "another long password" })),
            )
            .await;
        login.cookie.unwrap()
    }

    async fn add(&self, admin: &str, body: Value) -> Reply {
        self.call("POST", "/api/admin/catalogs", Some(admin), Some(body))
            .await
    }

    async fn catalog_ids(&self, cookie: &str) -> Vec<String> {
        let r = self.call("GET", "/api/catalog", Some(cookie), None).await;
        assert_eq!(r.status, StatusCode::OK);
        r.body
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["id"].as_str().unwrap().to_string())
            .collect()
    }
}

fn template(id: &str, image: &str, security: &str) -> Value {
    json!({
        "id": id, "name": format!("App {id}"), "description": "an app",
        "image": image, "class": "browser", "security": security, "shmMb": 512,
    })
}

fn doc(templates: Vec<Value>) -> Value {
    json!({ "version": 1, "name": "Acme apps", "templates": templates })
}

/// A catalog server on localhost: path to (content type, body), switchable to
/// fail.
type Served = (String, Vec<u8>);

#[derive(Clone, Default)]
struct Files(Arc<Mutex<HashMap<String, Served>>>, Arc<Mutex<bool>>);

impl Files {
    fn put(&self, path: &str, content_type: &str, body: impl Into<Vec<u8>>) {
        self.0
            .lock()
            .unwrap()
            .insert(path.into(), (content_type.into(), body.into()));
    }
    fn fail(&self, on: bool) {
        *self.1.lock().unwrap() = on;
    }
}

async fn serve(files: Files) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (mut sock, _) = listener.accept().await.unwrap();
            let files = files.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..n]).to_string();
                let path = head.split_whitespace().nth(1).unwrap_or("/").to_string();
                let failing = *files.1.lock().unwrap();
                let hit = files.0.lock().unwrap().get(&path).cloned();
                let out = match hit {
                    Some((ct, body)) if !failing => {
                        let mut v = format!(
                            "HTTP/1.1 200 OK\r\ncontent-type: {ct}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                            body.len()
                        )
                        .into_bytes();
                        v.extend(body);
                        v
                    }
                    _ => b"HTTP/1.1 500 Oops\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                        .to_vec(),
                };
                let _ = sock.write_all(&out).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    format!("http://{addr}")
}

const SVG: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 8 8"><rect width="8" height="8"/></svg>"#;

#[tokio::test]
async fn adding_by_document_namespaces_the_templates() {
    let p = portal().await;
    let admin = p.setup_admin().await;
    let player = p.setup_player(&admin).await;
    let d = doc(vec![template(
        "notes",
        "ghcr.io/acme/notes:{version}",
        "standard",
    )]);
    // As text, as pasted.
    let r = p
        .add(&admin, json!({ "slug": "acme", "document": d.to_string() }))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["slug"], "acme");
    assert_eq!(r.body["name"], "Acme apps");
    assert_eq!(r.body["url"], Value::Null);
    let t = &r.body["templates"][0];
    assert_eq!(t["id"], "acme.notes");
    assert_eq!(t["available"], true);
    assert_eq!(t["imageHost"], "ghcr.io");

    let ids = p.catalog_ids(&player).await;
    assert!(ids.contains(&"acme.notes".to_string()));
    assert!(
        ids.contains(&"chrome".to_string()),
        "built-in ones stay bare"
    );

    let again = p
        .add(&admin, json!({ "slug": "acme", "document": d }))
        .await;
    assert_eq!(again.status, StatusCode::CONFLICT);
    assert_eq!(again.body["error"], "slug_taken");

    // The slug can come from the document's own id; a built-in's name can't be used.
    let mut with_id = doc(vec![template("a", "ghcr.io/x/a:1", "standard")]);
    with_id["id"] = json!("fromdoc");
    assert_eq!(
        p.add(&admin, json!({ "document": with_id })).await.body["slug"],
        "fromdoc"
    );
    let r = p
        .add(&admin, json!({ "slug": "chrome", "document": d }))
        .await;
    assert_eq!(r.body["error"], "slug_reserved");
    let r = p
        .add(&admin, json!({ "slug": "Bad.Slug", "document": d }))
        .await;
    assert_eq!(r.body["error"], "bad_slug");

    // Persisted: a fresh state over the same database sees it.
    let config = Config {
        listen: "127.0.0.1:0".parse().unwrap(),
        database: p._dir.path().join("cha.db"),
        web_dir: None,
        secure_cookies: false,
        session_days: 14,
        ice: Default::default(),
        dev_login: false,
        discover_nodes: false,
        public_url: None,
        tunnel: Default::default(),
    };
    let state = AppState::new(config, p.db.clone()).await.unwrap();
    assert!(state.catalogs.available_by_id("acme.notes").is_some());
}

#[tokio::test]
async fn bad_documents_are_refused_with_the_template_and_field() {
    let p = portal().await;
    let admin = p.setup_admin().await;
    let cases: Vec<(Value, &str)> = vec![
        (
            template("t", "cha/env-chrome:dev", "standard"),
            "must name a registry",
        ),
        (
            {
                let mut t = template("t", "ghcr.io/a/b:1", "standard");
                t["localImage"] = json!("cha/x:dev");
                t
            },
            "localImage",
        ),
        (template("t", "ghcr.io/a/b:1", "root"), "unknown variant"),
        (
            template("Bad_Id", "ghcr.io/a/b:1", "standard"),
            "template id",
        ),
        (
            template("-lead", "ghcr.io/a/b:1", "standard"),
            "template id",
        ),
        (
            {
                let mut t = template("t", "ghcr.io/a/b:1", "standard");
                t["class"] = json!("moonlight");
                t
            },
            "Moonlight",
        ),
    ];
    for (t, want) in cases {
        let r = p
            .add(&admin, json!({ "slug": "bad", "document": doc(vec![t]) }))
            .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{want}: {}", r.body);
        assert_eq!(r.body["error"], "bad_catalog");
        assert!(r.body.to_string().contains(want), "{want}: {}", r.body);
    }
    let ok = template("t", "ghcr.io/a/b:1", "standard");
    let dup = p
        .add(
            &admin,
            json!({ "slug": "bad", "document": doc(vec![ok.clone(), ok]) }),
        )
        .await;
    assert_eq!(dup.status, StatusCode::BAD_REQUEST);
    assert!(dup.body.to_string().contains("repeated"));
    let r = p
        .add(&admin, json!({ "slug": "bad", "document": "{" }))
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let big = "x".repeat(1 << 20);
    let r = p
        .add(&admin, json!({ "slug": "bad", "document": json!({ "templates": [], "pad": big }).to_string() }))
        .await;
    assert!(r.body.to_string().contains("the most is"), "{}", r.body);
    let r = p.add(&admin, json!({ "slug": "bad" })).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let none = p
        .call("GET", "/api/admin/catalogs", Some(&admin), None)
        .await;
    assert_eq!(none.body["catalogs"], json!([]));
}

#[tokio::test]
async fn adding_by_url_fetches_icons_and_serves_them() {
    let p = portal().await;
    let admin = p.setup_admin().await;
    let player = p.setup_player(&admin).await;
    let files = Files::default();
    let base = serve(files.clone()).await;
    let mut t = template("notes", "ghcr.io/acme/notes:1", "standard");
    t["icon"] = json!("icons/notes.svg");
    let mut broken = template("broken", "ghcr.io/acme/broken:1", "standard");
    broken["icon"] = json!("icons/missing.svg");
    let mut insecure = template("plain", "ghcr.io/acme/plain:1", "standard");
    insecure["icon"] = json!("http://example.com/x.svg");
    files.put(
        "/catalog.json",
        "application/json",
        doc(vec![t, broken, insecure]).to_string(),
    );
    files.put("/icons/notes.svg", "image/svg+xml", SVG);

    let r = p
        .add(
            &admin,
            json!({ "slug": "acme", "url": format!("{base}/catalog.json") }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let ts = r.body["templates"].as_array().unwrap();
    assert_eq!(ts[0]["hasIcon"], true);
    // A failed icon doesn't fail the catalog.
    assert_eq!(ts[1]["hasIcon"], false);
    assert!(ts[1]["iconError"].is_string());
    assert_eq!(ts[2]["hasIcon"], false);
    assert!(ts[2]["iconError"].as_str().unwrap().contains("https"));

    let icon = p
        .call("GET", "/api/catalog/acme.notes/icon", Some(&player), None)
        .await;
    assert_eq!(icon.status, StatusCode::OK);
    assert_eq!(icon.bytes, SVG.as_bytes());
    assert_eq!(icon.headers[header::CONTENT_TYPE], "image/svg+xml");
    assert_eq!(
        icon.headers[header::CONTENT_SECURITY_POLICY],
        "default-src 'none'; style-src 'unsafe-inline'"
    );
    assert_eq!(icon.headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
    let none = p
        .call("GET", "/api/catalog/acme.broken/icon", Some(&player), None)
        .await;
    assert_eq!(none.status, StatusCode::NOT_FOUND);
    // The player's catalog says which have an icon.
    let cat = p.call("GET", "/api/catalog", Some(&player), None).await;
    let notes = cat
        .body
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == "acme.notes")
        .unwrap();
    assert!(notes["icon"].is_string());
    let broken = cat
        .body
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == "acme.broken")
        .unwrap();
    assert!(broken.get("icon").is_none());

    // A failing server on a refresh keeps the last good document and says why.
    files.fail(true);
    let r = p
        .call(
            "POST",
            "/api/admin/catalogs/acme/refresh",
            Some(&admin),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_GATEWAY, "{}", r.body);
    let list = p
        .call("GET", "/api/admin/catalogs", Some(&admin), None)
        .await;
    let c = &list.body["catalogs"][0];
    assert!(c["lastError"].as_str().unwrap().contains("500"));
    assert_eq!(c["templates"].as_array().unwrap().len(), 3);
    assert!(
        p.catalog_ids(&player)
            .await
            .contains(&"acme.notes".to_string())
    );

    // A refresh that works clears the error; a bad new document leaves the old one.
    files.fail(false);
    let r = p
        .call(
            "POST",
            "/api/admin/catalogs/acme/refresh",
            Some(&admin),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["lastError"], Value::Null);
    files.put("/catalog.json", "application/json", "{ nope");
    let r = p
        .call(
            "POST",
            "/api/admin/catalogs/acme/refresh",
            Some(&admin),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let list = p
        .call("GET", "/api/admin/catalogs", Some(&admin), None)
        .await;
    assert!(list.body["catalogs"][0]["lastError"].is_string());
    assert_eq!(
        list.body["catalogs"][0]["templates"]
            .as_array()
            .unwrap()
            .len(),
        3
    );

    // Unsupported schemes are refused up front.
    let r = p
        .add(&admin, json!({ "slug": "f", "url": "file:///etc/passwd" }))
        .await;
    assert_eq!(r.body["error"], "bad_url");
    // A pasted catalog has nothing to refresh from.
    p.add(&admin, json!({ "slug": "pasted", "document": doc(vec![]) }))
        .await;
    let r = p
        .call(
            "POST",
            "/api/admin/catalogs/pasted/refresh",
            Some(&admin),
            None,
        )
        .await;
    assert_eq!(r.body["error"], "no_url");
}

#[tokio::test]
async fn elevated_profiles_wait_for_approval_and_lose_it_when_they_change() {
    let p = portal().await;
    let admin = p.setup_admin().await;
    let player = p.setup_player(&admin).await;
    let files = Files::default();
    let base = serve(files.clone()).await;
    let url = format!("{base}/c.json");
    let v1 = doc(vec![
        template("web", "ghcr.io/acme/web:1", "browser"),
        template("plain", "ghcr.io/acme/plain:1", "standard"),
    ]);
    files.put("/c.json", "application/json", v1.to_string());
    let r = p.add(&admin, json!({ "slug": "acme", "url": url })).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let web = &r.body["templates"][0];
    assert_eq!(web["available"], false);
    assert_eq!(
        web["unavailableReason"],
        "needs approval for the browser profile"
    );

    let ids = p.catalog_ids(&player).await;
    assert!(ids.contains(&"acme.plain".to_string()));
    assert!(
        !ids.contains(&"acme.web".to_string()),
        "hidden until approved"
    );
    let launch = |id: &str| json!({ "templateId": id });
    let r = p
        .call(
            "POST",
            "/api/environments",
            Some(&player),
            Some(launch("acme.web")),
        )
        .await;
    assert_eq!(r.body["error"], "unknown_template");
    // An available one gets past template lookup (there is just no node).
    let r = p
        .call(
            "POST",
            "/api/environments",
            Some(&player),
            Some(launch("acme.plain")),
        )
        .await;
    assert_ne!(r.body["error"], "unknown_template", "{}", r.body);

    let approve = |app: &str, yes: bool| {
        let path = format!("/api/admin/catalogs/acme/templates/{app}/approval");
        (path, json!({ "approved": yes }))
    };
    let (path, body) = approve("plain", true);
    let r = p.call("PUT", &path, Some(&admin), Some(body)).await;
    assert_eq!(r.body["error"], "no_approval_needed");
    let (path, body) = approve("web", true);
    let r = p
        .call("PUT", &path, Some(&player), Some(body.clone()))
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = p.call("PUT", &path, Some(&admin), Some(body)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["templates"][0]["approved"], true);
    assert!(
        p.catalog_ids(&player)
            .await
            .contains(&"acme.web".to_string())
    );
    let r = p
        .call(
            "POST",
            "/api/environments",
            Some(&player),
            Some(launch("acme.web")),
        )
        .await;
    assert_ne!(r.body["error"], "unknown_template");

    // Same profile and registry host with a new tag: still approved.
    let v2 = doc(vec![
        template("web", "ghcr.io/acme/web:2", "browser"),
        template("plain", "ghcr.io/acme/plain:1", "standard"),
    ]);
    files.put("/c.json", "application/json", v2.to_string());
    let r = p
        .call(
            "POST",
            "/api/admin/catalogs/acme/refresh",
            Some(&admin),
            None,
        )
        .await;
    assert_eq!(r.body["templates"][0]["image"], "ghcr.io/acme/web:2");
    assert_eq!(r.body["templates"][0]["available"], true);

    // A new registry host clears it.
    let v3 = doc(vec![template("web", "quay.io/acme/web:2", "browser")]);
    files.put("/c.json", "application/json", v3.to_string());
    let r = p
        .call(
            "POST",
            "/api/admin/catalogs/acme/refresh",
            Some(&admin),
            None,
        )
        .await;
    assert_eq!(r.body["templates"][0]["available"], false);
    assert!(
        !p.catalog_ids(&player)
            .await
            .contains(&"acme.web".to_string())
    );
    // Approve again, then a profile change clears it.
    let (path, body) = approve("web", true);
    p.call("PUT", &path, Some(&admin), Some(body)).await;
    assert!(
        p.catalog_ids(&player)
            .await
            .contains(&"acme.web".to_string())
    );
    let v4 = doc(vec![template("web", "quay.io/acme/web:2", "steam")]);
    files.put("/c.json", "application/json", v4.to_string());
    let r = p
        .call(
            "POST",
            "/api/admin/catalogs/acme/refresh",
            Some(&admin),
            None,
        )
        .await;
    assert_eq!(r.body["templates"][0]["available"], false);
    assert_eq!(
        r.body["templates"][0]["unavailableReason"],
        "needs approval for the steam profile"
    );
    // Withdrawing an approval hides it again.
    let (path, body) = approve("web", true);
    p.call("PUT", &path, Some(&admin), Some(body)).await;
    let (path, body) = approve("web", false);
    let r = p.call("PUT", &path, Some(&admin), Some(body)).await;
    assert_eq!(r.body["templates"][0]["available"], false);
}

#[tokio::test]
async fn a_vm_template_waits_for_approval_like_the_other_elevated_profiles() {
    let p = portal().await;
    let admin = p.setup_admin().await;
    let player = p.setup_player(&admin).await;
    let d = doc(vec![template("win", "ghcr.io/acme/win:1", "vm")]);
    let r = p
        .add(&admin, json!({ "slug": "acme", "document": d }))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["templates"][0]["security"], "vm");
    assert_eq!(r.body["templates"][0]["available"], false);
    assert_eq!(
        r.body["templates"][0]["unavailableReason"],
        "needs approval for the vm profile"
    );
    assert!(
        !p.catalog_ids(&player)
            .await
            .contains(&"acme.win".to_string())
    );
    let r = p
        .call(
            "PUT",
            "/api/admin/catalogs/acme/templates/win/approval",
            Some(&admin),
            Some(json!({ "approved": true })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["templates"][0]["available"], true);
}

#[tokio::test]
async fn a_catalog_with_live_environments_cannot_be_removed() {
    let p = portal().await;
    let admin = p.setup_admin().await;
    let player = p.setup_player(&admin).await;
    let d = doc(vec![template("notes", "ghcr.io/acme/notes:1", "standard")]);
    p.add(&admin, json!({ "slug": "acme", "document": d }))
        .await;
    // A look-alike prefix doesn't count.
    p.add(&admin, json!({ "slug": "acme2", "document": doc(vec![template("x", "ghcr.io/a/x:1", "standard")]) })).await;

    let me = p.call("GET", "/api/me", Some(&admin), None).await;
    let uid = me.body["id"].as_str().unwrap().to_string();
    sqlx::query(
        "INSERT INTO environments (id, owner_id, template_id, state, created_at, updated_at) \
         VALUES ('e1', ?, 'acme.notes', 'running', 1, 1)",
    )
    .bind(&uid)
    .execute(&p.db)
    .await
    .unwrap();

    let r = p
        .call("DELETE", "/api/admin/catalogs/acme", Some(&player), None)
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = p
        .call("DELETE", "/api/admin/catalogs/acme", Some(&admin), None)
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    assert_eq!(r.body["error"], "in_use");
    assert!(
        p.catalog_ids(&player)
            .await
            .contains(&"acme.notes".to_string())
    );
    // The other one is free to go.
    let r = p
        .call("DELETE", "/api/admin/catalogs/acme2", Some(&admin), None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);

    sqlx::query("UPDATE environments SET state = 'destroyed'")
        .execute(&p.db)
        .await
        .unwrap();
    let r = p
        .call("DELETE", "/api/admin/catalogs/acme", Some(&admin), None)
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(
        !p.catalog_ids(&player)
            .await
            .contains(&"acme.notes".to_string())
    );
    let r = p
        .call("DELETE", "/api/admin/catalogs/acme", Some(&admin), None)
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    // The history names it by id still; the audit log has the changes.
    let audit = p.call("GET", "/api/audit", Some(&admin), None).await;
    let actions = audit.body.to_string();
    for a in ["catalog.added", "catalog.removed"] {
        assert!(actions.contains(a), "{a}");
    }
}

#[tokio::test]
async fn only_admins_manage_catalogs() {
    let p = portal().await;
    let admin = p.setup_admin().await;
    let player = p.setup_player(&admin).await;
    let d = doc(vec![]);
    for (method, path, body) in [
        ("GET", "/api/admin/catalogs", None),
        (
            "POST",
            "/api/admin/catalogs",
            Some(json!({ "slug": "x", "document": d })),
        ),
        ("POST", "/api/admin/catalogs/x/refresh", None),
        ("DELETE", "/api/admin/catalogs/x", None),
    ] {
        let r = p.call(method, path, Some(&player), body.clone()).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{method} {path}");
        let r = p.call(method, path, None, body).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{method} {path}");
    }
}

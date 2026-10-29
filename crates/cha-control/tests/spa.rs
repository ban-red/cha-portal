//! Serving the built web app: precompressed files, cache headers, and the
//! fallback to `index.html` for client-side routes.

use std::path::Path;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use cha_control::{AppState, Config, app, db};
use http_body_util::BodyExt;
use tower::ServiceExt;

const INDEX: &str = "<!doctype html><script src=\"/assets/app-1a2b.js\"></script>";
const SCRIPT: &str = "console.log('the portal');";
const IMMUTABLE: &str = "public, max-age=31536000, immutable";

/// A built app as Vite leaves it, with the `.br` and `.gz` copies the build
/// writes (their bytes stand in for compressed ones: the server only picks).
fn web_dir(root: &Path) {
    let assets = root.join("assets");
    std::fs::create_dir_all(&assets).unwrap();
    std::fs::write(root.join("index.html"), INDEX).unwrap();
    std::fs::write(root.join("index.html.br"), "index as brotli").unwrap();
    std::fs::write(root.join("favicon.svg"), "<svg/>").unwrap();
    std::fs::write(assets.join("app-1a2b.js"), SCRIPT).unwrap();
    std::fs::write(assets.join("app-1a2b.js.br"), "script as brotli").unwrap();
    std::fs::write(assets.join("app-1a2b.js.gz"), "script as gzip").unwrap();
}

async fn portal() -> (Router, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let web = dir.path().join("web");
    web_dir(&web);
    let config = Config {
        listen: "127.0.0.1:0".parse().unwrap(),
        database: dir.path().join("cha.db"),
        web_dir: Some(web),
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
    (app(state), dir)
}

struct Got {
    status: StatusCode,
    headers: header::HeaderMap,
    body: String,
}

async fn get(app: &Router, path: &str, headers: &[(header::HeaderName, &str)]) -> Got {
    let mut req = Request::builder().uri(path);
    for (name, value) in headers {
        req = req.header(name, *value);
    }
    let res = app
        .clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let body = res.into_body().collect().await.unwrap().to_bytes();
    Got {
        status,
        headers,
        body: String::from_utf8_lossy(&body).into_owned(),
    }
}

fn header(got: &Got, name: header::HeaderName) -> Option<&str> {
    got.headers.get(name).map(|v| v.to_str().unwrap())
}

#[tokio::test]
async fn hashed_assets_are_kept_for_good_in_the_best_encoding_offered() {
    let (app, _dir) = portal().await;
    let accept = header::ACCEPT_ENCODING;

    let br = get(&app, "/assets/app-1a2b.js", &[(accept.clone(), "gzip, br")]).await;
    assert_eq!(br.status, StatusCode::OK);
    assert_eq!(br.body, "script as brotli");
    assert_eq!(header(&br, header::CONTENT_ENCODING), Some("br"));
    assert!(
        header(&br, header::CONTENT_TYPE)
            .unwrap()
            .contains("javascript")
    );
    assert_eq!(header(&br, header::CACHE_CONTROL), Some(IMMUTABLE));
    let vary: Vec<_> = br.headers.get_all(header::VARY).iter().collect();
    assert_eq!(
        vary,
        ["accept-encoding"],
        "one Vary, so caches key on the encoding"
    );

    let gz = get(&app, "/assets/app-1a2b.js", &[(accept, "gzip")]).await;
    assert_eq!(gz.body, "script as gzip");
    assert_eq!(header(&gz, header::CONTENT_ENCODING), Some("gzip"));

    let plain = get(&app, "/assets/app-1a2b.js", &[]).await;
    assert_eq!(plain.body, SCRIPT);
    assert_eq!(header(&plain, header::CONTENT_ENCODING), None);
    assert_eq!(header(&plain, header::CACHE_CONTROL), Some(IMMUTABLE));
}

#[tokio::test]
async fn a_missing_asset_is_a_404_that_isnt_kept() {
    let (app, _dir) = portal().await;
    let got = get(&app, "/assets/app-old.js", &[]).await;
    assert_eq!(got.status, StatusCode::NOT_FOUND);
    assert_ne!(got.body, INDEX, "not the app's page in place of a script");
    assert_eq!(header(&got, header::CACHE_CONTROL), None);
}

#[tokio::test]
async fn the_page_and_client_routes_are_checked_on_every_load() {
    let (app, _dir) = portal().await;
    for path in ["/", "/admin/nodes", "/s/some-share-token"] {
        let got = get(&app, path, &[]).await;
        assert_eq!(got.status, StatusCode::OK, "{path}");
        assert_eq!(got.body, INDEX, "{path}");
        assert_eq!(
            header(&got, header::CACHE_CONTROL),
            Some("no-cache"),
            "{path}"
        );
    }
    // Precompressed too, when the browser takes it.
    let br = get(&app, "/admin/nodes", &[(header::ACCEPT_ENCODING, "br")]).await;
    assert_eq!(br.body, "index as brotli");
    assert_eq!(header(&br, header::CONTENT_ENCODING), Some("br"));
    // Other files from public/ the same way.
    let icon = get(&app, "/favicon.svg", &[]).await;
    assert_eq!(icon.body, "<svg/>");
    assert_eq!(header(&icon, header::CACHE_CONTROL), Some("no-cache"));
}

#[tokio::test]
async fn an_unchanged_page_is_a_304() {
    let (app, _dir) = portal().await;
    let first = get(&app, "/", &[]).await;
    let modified = header(&first, header::LAST_MODIFIED)
        .expect("Last-Modified")
        .to_owned();
    let again = get(&app, "/", &[(header::IF_MODIFIED_SINCE, &modified)]).await;
    assert_eq!(again.status, StatusCode::NOT_MODIFIED);
    assert!(again.body.is_empty());
}

#[tokio::test]
async fn the_api_is_not_the_web_app() {
    let (app, _dir) = portal().await;
    let got = get(&app, "/api/health", &[]).await;
    assert_eq!(got.status, StatusCode::OK);
    assert_eq!(header(&got, header::CACHE_CONTROL), None);
}

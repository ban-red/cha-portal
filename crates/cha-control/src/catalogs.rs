//! Catalogs an admin loads (ADR 0019): added by URL or pasted JSON, refreshed
//! on request, their elevated profiles approved per template. Their templates
//! appear beside the built-in ones as `<catalog>.<app>`.
//!
//! The database holds the truth (the last good document, approvals, fetched
//! icons). [`Registry`] is its parsed copy in [`AppState`], rebuilt after every
//! change, so the many synchronous template lookups never touch the database.

use std::sync::{Arc, OnceLock, RwLock};
use std::time::Duration;

use axum::extract::{Path, State};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use cha_wire::SecurityProfile;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::SqlitePool;
use tracing::{info, warn};

use crate::AppState;
use crate::auth::{AdminUser, ClientInfo};
use crate::db;
use crate::environments::{self, CatalogSource, Template, image_host, parse_document, valid_slug};
use crate::error::{ApiError, ApiResult};

const FETCH_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_REDIRECTS: usize = 3;
pub(crate) const MAX_ICON_BYTES: usize = 256 * 1024;
/// Slugs the portal keeps for itself (`custom` is the ids of custom
/// environments, ADR 0021).
const RESERVED_SLUGS: &[&str] = &["moonlight", "custom"];

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/admin/catalogs", get(list).post(add))
        .route("/admin/catalogs/{slug}", axum::routing::delete(remove))
        .route("/admin/catalogs/{slug}/refresh", post(refresh))
        .route(
            "/admin/catalogs/{slug}/templates/{app}/approval",
            put(approve),
        )
}

// ---- The registry ----

/// One template of a loaded catalog.
#[derive(Clone)]
struct Loaded {
    /// With its namespaced id. `icon` is set only when one was fetched.
    template: Template,
    app: String,
    /// `None` when a user can launch it, else why not.
    unavailable: Option<String>,
    approved: bool,
    icon: Option<Arc<Vec<u8>>>,
    icon_error: Option<String>,
}

#[derive(Clone)]
struct LoadedCatalog {
    slug: String,
    name: String,
    url: Option<String>,
    added_at: i64,
    fetched_at: i64,
    last_error: Option<String>,
    last_error_at: Option<i64>,
    templates: Vec<Loaded>,
}

/// Every loaded catalog, as last read from the database.
#[derive(Default)]
pub struct Registry {
    loaded: RwLock<Vec<LoadedCatalog>>,
    /// Held across a change (add, refresh, approve, remove), so two admins
    /// can't interleave them.
    changing: tokio::sync::Mutex<()>,
}

impl Registry {
    /// Templates a user can launch.
    pub fn available(&self) -> Vec<Template> {
        let loaded = self.loaded.read().unwrap();
        loaded
            .iter()
            .flat_map(|c| &c.templates)
            .filter(|l| l.unavailable.is_none())
            .map(|l| l.template.clone())
            .collect()
    }

    pub fn available_by_id(&self, id: &str) -> Option<Template> {
        self.find(id, |l| l.unavailable.is_none())
            .map(|l| l.template)
    }

    /// Any loaded template, launchable or not.
    pub fn by_id(&self, id: &str) -> Option<Template> {
        self.find(id, |_| true).map(|l| l.template)
    }

    /// Any loaded template with why a user can't launch it, if they can't.
    pub fn status(&self, id: &str) -> Option<(Template, Option<String>)> {
        self.find(id, |_| true).map(|l| (l.template, l.unavailable))
    }

    pub fn icon(&self, id: &str) -> Option<Arc<Vec<u8>>> {
        self.find(id, |_| true).and_then(|l| l.icon)
    }

    fn find(&self, id: &str, keep: impl Fn(&Loaded) -> bool) -> Option<Loaded> {
        let (slug, _) = id.split_once('.')?;
        let loaded = self.loaded.read().unwrap();
        loaded
            .iter()
            .find(|c| c.slug == slug)?
            .templates
            .iter()
            .find(|l| l.template.id == id && keep(l))
            .cloned()
    }

    /// Rebuilds from the database.
    pub async fn reload(&self, db: &SqlitePool) -> anyhow::Result<()> {
        let rows: Vec<CatalogRow> = sqlx::query_as(
            "SELECT slug, name, url, document, added_at, fetched_at, last_error, last_error_at \
             FROM catalogs ORDER BY slug",
        )
        .fetch_all(db)
        .await?;
        let approvals: Vec<(String, String, String, String)> =
            sqlx::query_as("SELECT slug, app, profile, image_host FROM catalog_approvals")
                .fetch_all(db)
                .await?;
        let icons: Vec<StoredIcon> =
            sqlx::query_as("SELECT slug, app, svg, error FROM catalog_icons")
                .fetch_all(db)
                .await?;
        let mut loaded = Vec::new();
        for row in rows {
            let doc = match parse_document(&row.document, CatalogSource::External) {
                Ok(doc) => doc,
                Err(err) => {
                    warn!(catalog = %row.slug, "the stored catalog no longer reads: {err}");
                    continue;
                }
            };
            let templates = doc
                .templates
                .into_iter()
                .map(|mut t| {
                    let namespaced = format!("{}.{}", row.slug, t.id);
                    let app = std::mem::replace(&mut t.id, namespaced);
                    let profile = profile_name(t.security);
                    let host = image_host(&t.image).unwrap_or_default().to_string();
                    let approved = approvals.iter().any(|(s, a, p, h)| {
                        *s == row.slug && *a == app && *p == profile && *h == host
                    });
                    let unavailable = (t.security != SecurityProfile::Standard && !approved)
                        .then(|| format!("needs approval for the {profile} profile"));
                    let (svg, icon_error) = icons
                        .iter()
                        .find(|(s, a, _, _)| *s == row.slug && *a == app)
                        .map(|(_, _, svg, err)| (svg.clone(), err.clone()))
                        .unwrap_or_default();
                    let icon = svg.filter(|b| !b.is_empty()).map(Arc::new);
                    if icon.is_none() {
                        t.icon = None;
                    }
                    Loaded {
                        template: t,
                        app,
                        unavailable,
                        approved,
                        icon,
                        icon_error,
                    }
                })
                .collect();
            loaded.push(LoadedCatalog {
                name: row.name,
                url: row.url,
                added_at: row.added_at,
                fetched_at: row.fetched_at,
                last_error: row.last_error,
                last_error_at: row.last_error_at,
                slug: row.slug,
                templates,
            });
        }
        *self.loaded.write().unwrap() = loaded;
        Ok(())
    }
}

/// (slug, app, svg, error)
type StoredIcon = (String, String, Option<Vec<u8>>, Option<String>);

#[derive(sqlx::FromRow)]
struct CatalogRow {
    slug: String,
    name: String,
    url: Option<String>,
    document: String,
    added_at: i64,
    fetched_at: i64,
    last_error: Option<String>,
    last_error_at: Option<i64>,
}

fn profile_name(profile: SecurityProfile) -> String {
    serde_json::to_value(profile)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

// ---- Fetching ----

fn client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        // Safe to repeat: the second install just reports it's there.
        let _ = rustls::crypto::ring::default_provider().install_default();
        reqwest::Client::builder()
            .timeout(FETCH_TIMEOUT)
            .redirect(reqwest::redirect::Policy::limited(MAX_REDIRECTS))
            .user_agent(concat!("cha-control/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("an HTTP client with these options")
    })
}

/// GETs `url` (http or https), reading at most `max` bytes.
async fn fetch(url: &reqwest::Url, max: usize) -> Result<Vec<u8>, String> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err(format!("{url}: only http and https URLs can be fetched"));
    }
    let mut res = client()
        .get(url.clone())
        .send()
        .await
        .map_err(|e| format!("fetching {url}: {}", short_error(&e)))?;
    if !res.status().is_success() {
        return Err(format!(
            "fetching {url}: the server answered {}",
            res.status()
        ));
    }
    if res.content_length().is_some_and(|n| n > max as u64) {
        return Err(format!("{url} is larger than {max} bytes"));
    }
    let mut body = Vec::new();
    while let Some(chunk) = res
        .chunk()
        .await
        .map_err(|e| format!("reading {url}: {}", short_error(&e)))?
    {
        if body.len() + chunk.len() > max {
            return Err(format!("{url} is larger than {max} bytes"));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn short_error(err: &reqwest::Error) -> String {
    if err.is_timeout() {
        "timed out".into()
    } else if err.is_redirect() {
        format!("too many redirects (the most is {MAX_REDIRECTS})")
    } else if err.is_connect() {
        "couldn't connect".into()
    } else {
        err.to_string()
    }
}

/// An icon's address: an `https://` URL, or a path relative to the catalog's
/// own URL.
fn icon_url(base: Option<&reqwest::Url>, icon: &str) -> Result<reqwest::Url, String> {
    if let Ok(url) = reqwest::Url::parse(icon) {
        return if url.scheme() == "https" {
            Ok(url)
        } else {
            Err(format!(
                "icon {icon:?} must be an https:// URL or a relative path"
            ))
        };
    }
    base.ok_or_else(|| {
        format!("icon {icon:?} is relative, and a pasted catalog has no URL to resolve it against")
    })?
    .join(icon)
    .map_err(|e| format!("icon {icon:?}: {e}"))
}

async fn fetch_icon(base: Option<&reqwest::Url>, icon: &str) -> Result<Vec<u8>, String> {
    let url = icon_url(base, icon)?;
    let body = fetch(&url, MAX_ICON_BYTES).await?;
    check_svg(&body).map_err(|e| format!("{url} {e}"))?;
    Ok(body)
}

/// Whether `body` is an SVG document (text, starting with `<svg` or an XML
/// prolog before it, not an HTML page).
pub(crate) fn check_svg(body: &[u8]) -> Result<(), String> {
    let text = std::str::from_utf8(body).map_err(|_| "isn't an SVG (not text)".to_string())?;
    let head = text.trim_start().to_ascii_lowercase();
    let svg = head.starts_with("<svg") || (head.starts_with("<?xml") && head.contains("<svg"));
    if !svg || head.contains("<!doctype html") {
        return Err("isn't an SVG".into());
    }
    Ok(())
}

/// A catalog document read and its icons fetched, ready to store.
struct Prepared {
    name: String,
    document: String,
    templates: Vec<Template>,
    icons: Vec<IconResult>,
}

struct IconResult {
    app: String,
    source: String,
    outcome: Result<Vec<u8>, String>,
}

async fn prepare(slug: &str, url: Option<&str>, text: String) -> Result<Prepared, ApiError> {
    let doc = parse_document(&text, CatalogSource::External)
        .map_err(|e| ApiError::bad_request("bad_catalog", e))?;
    let base = url.and_then(|u| reqwest::Url::parse(u).ok());
    let mut tasks = tokio::task::JoinSet::new();
    for t in &doc.templates {
        if let Some(icon) = t.icon.clone() {
            let (app, base) = (t.id.clone(), base.clone());
            tasks.spawn(async move {
                let outcome = fetch_icon(base.as_ref(), &icon).await;
                IconResult {
                    app,
                    source: icon,
                    outcome,
                }
            });
        }
    }
    let mut icons = Vec::new();
    while let Some(done) = tasks.join_next().await {
        if let Ok(done) = done {
            icons.push(done);
        }
    }
    Ok(Prepared {
        name: doc
            .name
            .filter(|n| !n.trim().is_empty())
            .map(|n| n.chars().take(80).collect())
            .unwrap_or_else(|| slug.to_string()),
        document: text,
        templates: doc.templates,
        icons,
    })
}

/// The document as text: fetched from `url`, or pasted.
async fn document_text(url: Option<&str>, pasted: Option<Value>) -> Result<String, ApiError> {
    match (url, pasted) {
        (Some(url), None) => {
            let parsed = reqwest::Url::parse(url)
                .map_err(|e| ApiError::bad_request("bad_url", format!("{url:?}: {e}")))?;
            if !matches!(parsed.scheme(), "http" | "https") {
                return Err(ApiError::bad_request(
                    "bad_url",
                    "the catalog URL must be http or https",
                ));
            }
            let body = fetch(&parsed, environments::MAX_CATALOG_BYTES)
                .await
                .map_err(|e| ApiError::BadGateway("fetch_failed", e))?;
            String::from_utf8(body).map_err(|_| {
                ApiError::bad_request(
                    "bad_catalog",
                    "not a catalog: the document isn't UTF-8 text",
                )
            })
        }
        (None, Some(Value::String(text))) => Ok(text),
        (None, Some(other)) => Ok(other.to_string()),
        _ => Err(ApiError::bad_request(
            "bad_request",
            "send either `url` or `document`",
        )),
    }
}

// ---- Storage ----

/// Stores a prepared document for `slug` (new or refreshed): the document, the
/// approvals that still hold, the icons.
async fn store(
    db: &SqlitePool,
    slug: &str,
    url: Option<&str>,
    admin: &str,
    p: &Prepared,
) -> ApiResult<()> {
    let now = db::now();
    let mut tx = db.begin().await.map_err(anyhow::Error::from)?;
    sqlx::query(
        "INSERT INTO catalogs (slug, name, url, document, added_by, added_at, fetched_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT (slug) DO UPDATE SET name = excluded.name, url = excluded.url, \
         document = excluded.document, fetched_at = excluded.fetched_at, \
         last_error = NULL, last_error_at = NULL",
    )
    .bind(slug)
    .bind(&p.name)
    .bind(url)
    .bind(&p.document)
    .bind(admin)
    .bind(now)
    .bind(now)
    .execute(&mut *tx)
    .await
    .map_err(anyhow::Error::from)?;

    // An approval holds only for the profile and registry host it was given for.
    let approvals: Vec<(String, String, String)> =
        sqlx::query_as("SELECT app, profile, image_host FROM catalog_approvals WHERE slug = ?")
            .bind(slug)
            .fetch_all(&mut *tx)
            .await
            .map_err(anyhow::Error::from)?;
    for (app, profile, host) in approvals {
        let still = p.templates.iter().any(|t| {
            t.id == app
                && profile_name(t.security) == profile
                && image_host(&t.image).unwrap_or_default() == host
        });
        if !still {
            sqlx::query("DELETE FROM catalog_approvals WHERE slug = ? AND app = ?")
                .bind(slug)
                .bind(&app)
                .execute(&mut *tx)
                .await
                .map_err(anyhow::Error::from)?;
        }
    }

    // A failed fetch of an unchanged icon keeps the one already stored.
    let old: Vec<(String, String, Option<Vec<u8>>)> =
        sqlx::query_as("SELECT app, source, svg FROM catalog_icons WHERE slug = ?")
            .bind(slug)
            .fetch_all(&mut *tx)
            .await
            .map_err(anyhow::Error::from)?;
    sqlx::query("DELETE FROM catalog_icons WHERE slug = ?")
        .bind(slug)
        .execute(&mut *tx)
        .await
        .map_err(anyhow::Error::from)?;
    for icon in &p.icons {
        let (svg, error) = match &icon.outcome {
            Ok(svg) => (Some(svg.clone()), None),
            Err(err) => {
                warn!(catalog = %slug, app = %icon.app, "icon not loaded: {err}");
                let kept = old
                    .iter()
                    .find(|(a, s, _)| *a == icon.app && *s == icon.source)
                    .and_then(|(_, _, svg)| svg.clone());
                (kept, Some(err.clone()))
            }
        };
        sqlx::query(
            "INSERT INTO catalog_icons (slug, app, source, svg, error) VALUES (?, ?, ?, ?, ?)",
        )
        .bind(slug)
        .bind(&icon.app)
        .bind(&icon.source)
        .bind(svg)
        .bind(error)
        .execute(&mut *tx)
        .await
        .map_err(anyhow::Error::from)?;
    }
    tx.commit().await.map_err(anyhow::Error::from)?;
    Ok(())
}

async fn record_error(db: &SqlitePool, slug: &str, message: &str) {
    let result =
        sqlx::query("UPDATE catalogs SET last_error = ?, last_error_at = ? WHERE slug = ?")
            .bind(message)
            .bind(db::now())
            .bind(slug)
            .execute(db)
            .await;
    if let Err(err) = result {
        warn!(catalog = %slug, "recording a catalog error: {err}");
    }
}

// ---- API ----

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TemplateView {
    /// The namespaced id (`<slug>.<app>`), as launches and `/api/catalog` use.
    id: String,
    app: String,
    name: String,
    description: String,
    image: String,
    image_host: String,
    class: String,
    security: SecurityProfile,
    /// A user can launch it.
    available: bool,
    /// Why not, when it isn't available.
    unavailable_reason: Option<String>,
    /// An admin approved its elevated profile (always false for `standard`).
    approved: bool,
    has_icon: bool,
    icon_error: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CatalogView {
    slug: String,
    name: String,
    /// Absent for a pasted document.
    url: Option<String>,
    added_at: i64,
    fetched_at: i64,
    last_error: Option<String>,
    last_error_at: Option<i64>,
    templates: Vec<TemplateView>,
}

fn view(c: &LoadedCatalog) -> CatalogView {
    CatalogView {
        slug: c.slug.clone(),
        name: c.name.clone(),
        url: c.url.clone(),
        added_at: c.added_at,
        fetched_at: c.fetched_at,
        last_error: c.last_error.clone(),
        last_error_at: c.last_error_at,
        templates: c
            .templates
            .iter()
            .map(|l| TemplateView {
                id: l.template.id.clone(),
                app: l.app.clone(),
                name: l.template.name.clone(),
                description: l.template.description.clone(),
                image: l.template.image.clone(),
                image_host: image_host(&l.template.image)
                    .unwrap_or_default()
                    .to_string(),
                class: l.template.class.clone(),
                security: l.template.security,
                available: l.unavailable.is_none(),
                unavailable_reason: l.unavailable.clone(),
                approved: l.approved,
                has_icon: l.icon.is_some(),
                icon_error: l.icon_error.clone(),
            })
            .collect(),
    }
}

fn catalog_view(state: &AppState, slug: &str) -> ApiResult<CatalogView> {
    let loaded = state.catalogs.loaded.read().unwrap();
    loaded
        .iter()
        .find(|c| c.slug == slug)
        .map(view)
        .ok_or_else(|| ApiError::NotFound("no such catalog".into()))
}

/// `GET /api/admin/catalogs`: `{ "catalogs": [...] }`.
async fn list(State(state): State<AppState>, _: AdminUser) -> Json<Value> {
    let loaded = state.catalogs.loaded.read().unwrap();
    Json(json!({ "catalogs": loaded.iter().map(view).collect::<Vec<_>>() }))
}

#[derive(Deserialize)]
struct AddRequest {
    /// Absent: the document's own `id`.
    slug: Option<String>,
    url: Option<String>,
    /// The JSON text, or the JSON itself.
    document: Option<Value>,
}

/// `POST /api/admin/catalogs`.
async fn add(
    State(state): State<AppState>,
    AdminUser(admin): AdminUser,
    client: ClientInfo,
    Json(req): Json<AddRequest>,
) -> ApiResult<Json<CatalogView>> {
    let _changing = state.catalogs.changing.lock().await;
    let text = document_text(req.url.as_deref(), req.document).await?;
    let slug = match req.slug {
        Some(slug) => slug,
        None => parse_document(&text, CatalogSource::External)
            .ok()
            .and_then(|d| d.id)
            .ok_or_else(|| {
                ApiError::bad_request("bad_slug", "give a slug: the document has no `id` to use")
            })?,
    };
    if !valid_slug(&slug) {
        return Err(ApiError::bad_request(
            "bad_slug",
            "the slug is lower-case letters, digits and hyphens, 1 to 32 characters, not starting or ending with a hyphen",
        ));
    }
    if environments::template(&slug).is_some() || RESERVED_SLUGS.contains(&slug.as_str()) {
        return Err(ApiError::bad_request(
            "slug_reserved",
            format!("{slug:?} is a name the portal keeps; pick another slug"),
        ));
    }
    let taken: Option<String> = sqlx::query_scalar("SELECT slug FROM catalogs WHERE slug = ?")
        .bind(&slug)
        .fetch_optional(&state.db)
        .await
        .map_err(anyhow::Error::from)?;
    if taken.is_some() {
        return Err(ApiError::conflict(
            "slug_taken",
            format!("a catalog called {slug:?} is already loaded"),
        ));
    }
    let prepared = prepare(&slug, req.url.as_deref(), text).await?;
    store(&state.db, &slug, req.url.as_deref(), &admin.id, &prepared).await?;
    state.catalogs.reload(&state.db).await?;
    db::audit(
        &state.db,
        Some(&admin.id),
        "catalog.added",
        Some(&slug),
        Some(json!({ "url": req.url, "templates": prepared.templates.len() })),
        client.ip.as_deref(),
    )
    .await?;
    info!(catalog = %slug, templates = prepared.templates.len(), "catalog added");
    Ok(Json(catalog_view(&state, &slug)?))
}

#[derive(Deserialize, Default)]
struct RefreshRequest {
    /// A new document for a catalog that was pasted (it has no URL to fetch).
    document: Option<Value>,
}

/// `POST /api/admin/catalogs/{slug}/refresh`: fetches the URL again (or takes
/// a new pasted `document`). On failure the last good document stays and the
/// error is recorded on the catalog.
async fn refresh(
    State(state): State<AppState>,
    AdminUser(admin): AdminUser,
    client: ClientInfo,
    Path(slug): Path<String>,
    body: Option<Json<RefreshRequest>>,
) -> ApiResult<Json<CatalogView>> {
    let _changing = state.catalogs.changing.lock().await;
    let url: Option<Option<String>> = sqlx::query_scalar("SELECT url FROM catalogs WHERE slug = ?")
        .bind(&slug)
        .fetch_optional(&state.db)
        .await
        .map_err(anyhow::Error::from)?;
    let Some(url) = url else {
        return Err(ApiError::NotFound("no such catalog".into()));
    };
    let pasted = body.and_then(|Json(b)| b.document);
    let result = async {
        let text = match (&url, pasted) {
            (Some(url), None) => document_text(Some(url), None).await?,
            (None, Some(doc)) => document_text(None, Some(doc)).await?,
            (Some(_), Some(_)) => {
                return Err(ApiError::bad_request(
                    "has_url",
                    "this catalog is fetched from its URL; remove it and add it again to paste instead",
                ));
            }
            (None, None) => {
                return Err(ApiError::bad_request(
                    "no_url",
                    "this catalog was pasted: send the new `document`",
                ));
            }
        };
        prepare(&slug, url.as_deref(), text).await
    }
    .await;
    let prepared = match result {
        Ok(p) => p,
        Err(err) => {
            if !matches!(&err, ApiError::BadRequest("has_url" | "no_url", _)) {
                record_error(&state.db, &slug, &err.to_string()).await;
                state.catalogs.reload(&state.db).await?;
            }
            return Err(err);
        }
    };
    store(&state.db, &slug, url.as_deref(), &admin.id, &prepared).await?;
    state.catalogs.reload(&state.db).await?;
    db::audit(
        &state.db,
        Some(&admin.id),
        "catalog.refreshed",
        Some(&slug),
        Some(json!({ "templates": prepared.templates.len() })),
        client.ip.as_deref(),
    )
    .await?;
    Ok(Json(catalog_view(&state, &slug)?))
}

#[derive(Deserialize)]
struct ApprovalRequest {
    approved: bool,
}

/// `PUT /api/admin/catalogs/{slug}/templates/{app}/approval`.
async fn approve(
    State(state): State<AppState>,
    AdminUser(admin): AdminUser,
    client: ClientInfo,
    Path((slug, app)): Path<(String, String)>,
    Json(req): Json<ApprovalRequest>,
) -> ApiResult<Json<CatalogView>> {
    let _changing = state.catalogs.changing.lock().await;
    let id = format!("{slug}.{app}");
    let template = state
        .catalogs
        .by_id(&id)
        .ok_or_else(|| ApiError::NotFound("no such template in that catalog".into()))?;
    if template.security == SecurityProfile::Standard {
        return Err(ApiError::bad_request(
            "no_approval_needed",
            "a standard template is available once its catalog is loaded",
        ));
    }
    if req.approved {
        sqlx::query(
            "INSERT OR REPLACE INTO catalog_approvals \
             (slug, app, profile, image_host, approved_by, approved_at) VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(&slug)
        .bind(&app)
        .bind(profile_name(template.security))
        .bind(image_host(&template.image).unwrap_or_default())
        .bind(&admin.id)
        .bind(db::now())
        .execute(&state.db)
        .await
        .map_err(anyhow::Error::from)?;
    } else {
        sqlx::query("DELETE FROM catalog_approvals WHERE slug = ? AND app = ?")
            .bind(&slug)
            .bind(&app)
            .execute(&state.db)
            .await
            .map_err(anyhow::Error::from)?;
    }
    state.catalogs.reload(&state.db).await?;
    db::audit(
        &state.db,
        Some(&admin.id),
        if req.approved {
            "catalog.template_approved"
        } else {
            "catalog.template_unapproved"
        },
        Some(&id),
        Some(json!({ "profile": profile_name(template.security) })),
        client.ip.as_deref(),
    )
    .await?;
    Ok(Json(catalog_view(&state, &slug)?))
}

/// `DELETE /api/admin/catalogs/{slug}`: refused (409 `in_use`) while an
/// environment of one of its templates, or of a custom template made from
/// one, is live. Users' choices for its apps
/// and the nodes' app data are left alone.
async fn remove(
    State(state): State<AppState>,
    AdminUser(admin): AdminUser,
    client: ClientInfo,
    Path(slug): Path<String>,
) -> ApiResult<Json<Value>> {
    let _changing = state.catalogs.changing.lock().await;
    catalog_view(&state, &slug)?;
    let prefix = format!("{slug}.");
    let live: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM environments WHERE (substr(template_id, 1, ?1) = ?2 \
         OR template_id IN (SELECT id FROM custom_templates WHERE substr(base, 1, ?1) = ?2)) \
         AND state IN ('starting', 'running', 'stopping')",
    )
    .bind(prefix.chars().count() as i64)
    .bind(&prefix)
    .fetch_one(&state.db)
    .await
    .map_err(anyhow::Error::from)?;
    if live > 0 {
        return Err(ApiError::conflict(
            "in_use",
            format!("{live} environment(s) of this catalog's apps are live; stop them first"),
        ));
    }
    let mut tx = state.db.begin().await.map_err(anyhow::Error::from)?;
    for table in ["catalog_approvals", "catalog_icons", "catalogs"] {
        sqlx::query(&format!("DELETE FROM {table} WHERE slug = ?"))
            .bind(&slug)
            .execute(&mut *tx)
            .await
            .map_err(anyhow::Error::from)?;
    }
    tx.commit().await.map_err(anyhow::Error::from)?;
    state.catalogs.reload(&state.db).await?;
    db::audit(
        &state.db,
        Some(&admin.id),
        "catalog.removed",
        Some(&slug),
        None,
        client.ip.as_deref(),
    )
    .await?;
    Ok(Json(json!({ "removed": slug })))
}

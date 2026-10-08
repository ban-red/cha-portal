//! Custom environments (ADR 0021): an admin takes a built-in or catalog
//! template, saves it under a new name (`custom.<slug>`) and changes some of
//! its fields. The database holds the base's id and only the overrides; a
//! custom template is resolved whenever it is looked up (base, then
//! overrides), so a base updated by a catalog refresh reaches every field the
//! custom one didn't change.
//!
//! [`Registry`] is the parsed copy of the table in [`AppState`], rebuilt after
//! every change, so the synchronous template lookups never touch the database.
//! Host options (mounts, ports, capabilities, devices) are part of a custom
//! template but only reach a node whose owner allows them (`cha_wire::host`,
//! `crate::placement`).

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::routing::{get, put};
use axum::{Json, Router};
use cha_wire::{GamepadKind, HostOptions, Inventory, SecurityProfile};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::SqlitePool;
use tracing::{info, warn};

use crate::AppState;
use crate::auth::{AdminUser, ClientInfo};
use crate::catalogs::{MAX_ICON_BYTES, check_svg};
use crate::db;
use crate::environments::{self, CustomInfo, Template};
use crate::error::{ApiError, ApiResult};
use crate::moonlight;
use crate::storage::{SharedAccess, SharedDefaults};

/// The namespace of custom templates' ids: `custom.<slug>`.
pub const CATALOG_SLUG: &str = "custom";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/admin/custom-templates", get(list).post(create))
        .route("/admin/custom-templates/{id}", put(update).delete(remove))
        .route(
            "/admin/custom-templates/{id}/icon",
            put(set_icon).delete(clear_icon),
        )
        .route("/admin/host-options", get(host_options))
}

// ---- What a custom template overrides ----

/// The fields a custom template may change, all optional. What is absent is
/// inherited from the base.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Overrides {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Replaces the base's image, and drops its `localImage`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shm_mb: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fps: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gamepad: Option<GamepadKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fixed_size: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub needs_gpu: Option<bool>,
    /// The default for keeping users' data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persistent: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shared_access: Option<SharedAccess>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub security: Option<SecurityProfile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<BTreeMap<String, String>>,
}

impl Overrides {
    /// Why these overrides are refused, as (field, reason).
    fn check(&self) -> Result<(), (&'static str, String)> {
        if let Some(name) = &self.name
            && (name.trim().is_empty()
                || name.chars().count() > 80
                || name.chars().any(char::is_control))
        {
            return Err(("name", "must be 1 to 80 characters".into()));
        }
        if let Some(d) = &self.description
            && (d.chars().count() > 500 || d.chars().any(|c| c.is_control() && c != '\n'))
        {
            return Err(("description", "is at most 500 characters".into()));
        }
        if let Some(image) = &self.image
            && (image.trim().is_empty()
                || image.len() > 255
                || image.chars().any(|c| c.is_whitespace() || c.is_control()))
        {
            return Err((
                "image",
                "must be one reference of at most 255 characters, with no spaces".into(),
            ));
        }
        if let Some(class) = &self.class {
            let ok = !class.is_empty()
                && class.len() <= 24
                && class
                    .bytes()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-');
            if !ok {
                return Err((
                    "class",
                    "must be lower-case letters, digits and hyphens, at most 24 characters".into(),
                ));
            }
            if class == moonlight::CLASS {
                return Err(("class", format!("{class:?} is the portal's own")));
            }
        }
        if self.shm_mb.is_some_and(|m| m > 16384) {
            return Err(("shmMb", "is at most 16384".into()));
        }
        if let Some(fps) = self.fps
            && !crate::apps::FPS_CHOICES.contains(&fps)
        {
            return Err(("fps", "is 60, 90 or 120".into()));
        }
        if let Some(env) = &self.env {
            cha_wire::check_env(env).map_err(|e| ("env", e))?;
        }
        Ok(())
    }

    fn fields(&self) -> Vec<&'static str> {
        let set = [
            ("name", self.name.is_some()),
            ("description", self.description.is_some()),
            ("image", self.image.is_some()),
            ("class", self.class.is_some()),
            ("shmMb", self.shm_mb.is_some()),
            ("fps", self.fps.is_some()),
            ("gamepad", self.gamepad.is_some()),
            ("fixedSize", self.fixed_size.is_some()),
            ("needsGpu", self.needs_gpu.is_some()),
            ("persistent", self.persistent.is_some()),
            ("sharedAccess", self.shared_access.is_some()),
            ("security", self.security.is_some()),
            ("env", self.env.is_some()),
        ];
        set.into_iter()
            .filter(|(_, on)| *on)
            .map(|(f, _)| f)
            .collect()
    }
}

// ---- The registry ----

/// One row of `custom_templates`, parsed.
#[derive(Clone)]
struct Stored {
    id: String,
    base: String,
    share_data: bool,
    overrides: Overrides,
    host: Option<HostOptions>,
    icon: Option<Arc<Vec<u8>>>,
    created_at: i64,
    updated_at: i64,
}

type Row = (
    String,
    String,
    bool,
    String,
    Option<String>,
    Option<Vec<u8>>,
    i64,
    i64,
);

#[derive(Default)]
pub struct Registry {
    rows: RwLock<Vec<Stored>>,
    /// Held across a change, so two admins can't interleave them.
    changing: tokio::sync::Mutex<()>,
}

impl Registry {
    /// Rebuilds from the database.
    pub async fn reload(&self, db: &SqlitePool) -> anyhow::Result<()> {
        let rows: Vec<Row> = sqlx::query_as(
            "SELECT id, base, share_data, overrides, host, icon, created_at, updated_at \
             FROM custom_templates ORDER BY created_at, id",
        )
        .fetch_all(db)
        .await?;
        let mut parsed = Vec::new();
        for (id, base, share_data, overrides, host, icon, created_at, updated_at) in rows {
            let overrides = match serde_json::from_str(&overrides) {
                Ok(o) => o,
                Err(err) => {
                    warn!(template = %id, "the stored custom template no longer reads: {err}");
                    continue;
                }
            };
            let host = host.and_then(|h| match serde_json::from_str::<HostOptions>(&h) {
                Ok(h) => Some(h),
                Err(err) => {
                    warn!(template = %id, "the stored host options no longer read: {err}");
                    None
                }
            });
            parsed.push(Stored {
                id,
                base,
                share_data,
                overrides,
                host,
                icon: icon.filter(|b| !b.is_empty()).map(Arc::new),
                created_at,
                updated_at,
            });
        }
        *self.rows.write().unwrap() = parsed;
        Ok(())
    }

    fn get(&self, id: &str) -> Option<Stored> {
        self.rows
            .read()
            .unwrap()
            .iter()
            .find(|r| r.id == id)
            .cloned()
    }

    fn all(&self) -> Vec<Stored> {
        self.rows.read().unwrap().clone()
    }
}

/// The base a custom template was made from, and why a user can't launch it
/// if they can't. Never a custom template.
fn base_status(state: &AppState, id: &str) -> Option<(Template, Option<String>)> {
    if id.starts_with("custom.") {
        return None;
    }
    environments::template(id)
        .map(|t| (t.clone(), None))
        .or_else(|| state.catalogs.status(id))
}

/// `base` with `row`'s overrides applied.
fn apply(base: &Template, row: &Stored) -> Template {
    let o = &row.overrides;
    let mut t = base.clone();
    t.id = row.id.clone();
    if let Some(v) = &o.name {
        t.name = v.trim().to_string();
    }
    if let Some(v) = &o.description {
        t.description = v.clone();
    }
    if let Some(v) = &o.image {
        t.image = v.clone();
        t.local_image = None;
    }
    if let Some(v) = &o.class {
        t.class = v.clone();
    }
    if let Some(v) = o.shm_mb {
        t.shm_mb = v;
    }
    if let Some(v) = o.fps {
        t.fps = Some(v);
    }
    if let Some(v) = o.gamepad {
        t.gamepad = Some(v);
    }
    if let Some(v) = o.fixed_size {
        t.fixed_size = v;
    }
    if let Some(v) = o.needs_gpu {
        t.needs_gpu = v;
    }
    if let Some(v) = o.persistent {
        t.persistent = v;
    }
    if let Some(access) = o.shared_access {
        t.shared = Some(SharedDefaults {
            access,
            per_user: base
                .shared
                .as_ref()
                .map(|s| s.per_user.clone())
                .unwrap_or_default(),
        });
    }
    if let Some(v) = o.security {
        t.security = v;
    }
    t.env = o.env.clone().unwrap_or_default();
    t.icon = row
        .icon
        .as_ref()
        .map(|_| "icon.svg".to_string())
        .or(base.icon.clone());
    t.custom = Some(CustomInfo {
        base: row.base.clone(),
        share_data: row.share_data,
        host: row.host.clone(),
    });
    t
}

/// The resolved template, and why a user can't launch it if they can't. `Err`:
/// its base is gone.
fn resolve(state: &AppState, row: &Stored) -> Result<(Template, Option<String>), String> {
    let Some((base, unavailable)) = base_status(state, &row.base) else {
        return Err(format!(
            "its base {} is gone (a catalog was removed, or refreshed without it); rebase it or delete it",
            row.base
        ));
    };
    let unavailable = unavailable.map(|why| format!("its base {} {why}", base.name));
    Ok((apply(&base, row), unavailable))
}

/// The custom templates a user can launch.
pub(crate) fn available(state: &AppState) -> Vec<Template> {
    state
        .customs
        .all()
        .iter()
        .filter_map(|r| match resolve(state, r) {
            Ok((t, None)) => Some(t),
            _ => None,
        })
        .collect()
}

/// A custom template by id: launchable ones only, or, with `any`, every one
/// whose base still exists.
pub(crate) fn find(state: &AppState, id: &str, any: bool) -> Option<Template> {
    let row = state.customs.get(id)?;
    match resolve(state, &row) {
        Ok((t, None)) => Some(t),
        Ok((t, Some(_))) if any => Some(t),
        _ => None,
    }
}

/// A custom template's logo: its own, else its base's.
pub(crate) fn icon(state: &AppState, id: &str) -> Option<Arc<Vec<u8>>> {
    let row = state.customs.get(id)?;
    row.icon
        .clone()
        .or_else(|| environments::icon_of(state, &row.base))
}

// ---- The API ----

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Item {
    /// `custom.<slug>`.
    id: String,
    slug: String,
    base: String,
    /// The base's name; its id when the base is gone.
    base_name: String,
    share_data: bool,
    overrides: Overrides,
    host: Option<HostOptions>,
    has_icon: bool,
    /// Why a user can't launch it, if they can't.
    unavailable: Option<String>,
    created_at: i64,
    updated_at: i64,
}

fn item(state: &AppState, row: &Stored) -> Item {
    let (base_name, unavailable) = match resolve(state, row) {
        Ok((t, why)) => (
            base_status(state, &row.base).map_or(t.name, |(b, _)| b.name),
            why,
        ),
        Err(why) => (row.base.clone(), Some(why)),
    };
    Item {
        id: row.id.clone(),
        slug: row
            .id
            .strip_prefix("custom.")
            .unwrap_or(&row.id)
            .to_string(),
        base: row.base.clone(),
        base_name,
        share_data: row.share_data,
        overrides: row.overrides.clone(),
        host: row.host.clone(),
        has_icon: row.icon.is_some(),
        unavailable,
        created_at: row.created_at,
        updated_at: row.updated_at,
    }
}

fn existing(state: &AppState, id: &str) -> ApiResult<Stored> {
    state
        .customs
        .get(id)
        .ok_or_else(|| ApiError::NotFound("no such custom template".into()))
}

fn parse_overrides(value: Value) -> ApiResult<Overrides> {
    let value = if value.is_null() { json!({}) } else { value };
    let overrides: Overrides = serde_json::from_value(value)
        .map_err(|e| ApiError::bad_request("bad_overrides", format!("overrides: {e}")))?;
    overrides.check().map_err(|(field, why)| {
        ApiError::bad_request("bad_overrides", format!("overrides.{field} {why}"))
    })?;
    Ok(overrides)
}

/// Host options as sent; `None` for absent, null or empty.
fn parse_host(value: Option<Value>) -> ApiResult<Option<HostOptions>> {
    let Some(value) = value.filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let host: HostOptions = serde_json::from_value(value)
        .map_err(|e| ApiError::bad_request("bad_host", format!("host: {e}")))?;
    host.check_shape()
        .map_err(|e| ApiError::bad_request("bad_host", format!("host: {e}")))?;
    Ok((!host.is_empty()).then_some(host))
}

fn profile_rank(p: SecurityProfile) -> u8 {
    match p {
        SecurityProfile::Standard => 0,
        SecurityProfile::Browser => 1,
        SecurityProfile::Steam => 2,
        // Not a wider sandbox than steam's, but a device none of the others
        // get: last, so a custom environment moved to it is flagged as wider.
        SecurityProfile::Vm => 3,
    }
}

fn profile_name(p: SecurityProfile) -> String {
    serde_json::to_value(p)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

/// What the audit log records of a saved custom template.
fn audit_detail(base: &Template, row: &Stored) -> Value {
    let mut d = json!({
        "base": row.base,
        "shareData": row.share_data,
        "fields": row.overrides.fields(),
    });
    let map = d.as_object_mut().expect("an object");
    // Values may be secrets; the names are what matters.
    if let Some(env) = &row.overrides.env {
        map.insert("envNames".into(), json!(env.keys().collect::<Vec<_>>()));
    }
    if let Some(security) = row.overrides.security {
        map.insert("security".into(), json!(profile_name(security)));
        if profile_rank(security) > profile_rank(base.security) {
            map.insert(
                "securityWiderThanBase".into(),
                json!({ "base": profile_name(base.security), "custom": profile_name(security) }),
            );
        }
    }
    if let Some(host) = &row.host {
        map.insert("host".into(), json!(host));
    }
    d
}

async fn save(db: &SqlitePool, row: &Stored, admin: &str, create: bool) -> ApiResult<()> {
    let overrides = serde_json::to_string(&row.overrides).map_err(anyhow::Error::from)?;
    let host = row
        .host
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(anyhow::Error::from)?;
    if create {
        sqlx::query(
            "INSERT INTO custom_templates \
             (id, base, share_data, overrides, host, created_by, updated_by, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&row.id)
        .bind(&row.base)
        .bind(row.share_data)
        .bind(overrides)
        .bind(host)
        .bind(admin)
        .bind(admin)
        .bind(row.created_at)
        .bind(row.updated_at)
        .execute(db)
        .await
        .map_err(anyhow::Error::from)?;
    } else {
        sqlx::query(
            "UPDATE custom_templates SET share_data = ?, overrides = ?, host = ?, \
             updated_by = ?, updated_at = ? WHERE id = ?",
        )
        .bind(row.share_data)
        .bind(overrides)
        .bind(host)
        .bind(admin)
        .bind(row.updated_at)
        .bind(&row.id)
        .execute(db)
        .await
        .map_err(anyhow::Error::from)?;
    }
    Ok(())
}

/// `GET /api/admin/custom-templates`: `[Item]`.
async fn list(State(state): State<AppState>, _: AdminUser) -> Json<Vec<Item>> {
    Json(
        state
            .customs
            .all()
            .iter()
            .map(|r| item(&state, r))
            .collect(),
    )
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateRequest {
    slug: String,
    base: String,
    #[serde(default)]
    share_data: bool,
    #[serde(default)]
    overrides: Value,
    #[serde(default)]
    host: Option<Value>,
}

/// `POST /api/admin/custom-templates`.
async fn create(
    State(state): State<AppState>,
    AdminUser(admin): AdminUser,
    client: ClientInfo,
    Json(req): Json<CreateRequest>,
) -> ApiResult<Json<Item>> {
    let _changing = state.customs.changing.lock().await;
    if !cha_wire::valid_catalog_app(&req.slug) {
        return Err(ApiError::bad_request(
            "bad_slug",
            "slug is lower-case letters, digits and hyphens, 1 to 40 characters, not starting or ending with a hyphen, and not `migrated` or `migrating`",
        ));
    }
    let id = format!("{CATALOG_SLUG}.{}", req.slug);
    if !cha_wire::valid_template_id(&id) {
        return Err(ApiError::bad_request(
            "bad_slug",
            format!("slug: {id:?} isn't a template id"),
        ));
    }
    let Some((base, unavailable)) = base_status(&state, &req.base) else {
        return Err(ApiError::bad_request(
            "bad_base",
            format!(
                "base: no such template {:?} (a custom template can't be a base)",
                req.base
            ),
        ));
    };
    if let Some(why) = unavailable {
        return Err(ApiError::bad_request(
            "bad_base",
            format!("base: {} {why}", base.name),
        ));
    }
    let overrides = parse_overrides(req.overrides)?;
    let host = parse_host(req.host)?;
    if state.customs.get(&id).is_some() {
        return Err(ApiError::conflict(
            "slug_taken",
            format!("a custom template called {id:?} already exists"),
        ));
    }
    let now = db::now();
    let row = Stored {
        id: id.clone(),
        base: req.base,
        share_data: req.share_data,
        overrides,
        host,
        icon: None,
        created_at: now,
        updated_at: now,
    };
    save(&state.db, &row, &admin.id, true).await?;
    state.customs.reload(&state.db).await?;
    db::audit(
        &state.db,
        Some(&admin.id),
        "custom_template.created",
        Some(&id),
        Some(audit_detail(&base, &row)),
        client.ip.as_deref(),
    )
    .await?;
    info!(template = %id, base = %row.base, "custom template created");
    Ok(Json(item(&state, &existing(&state, &id)?)))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateRequest {
    #[serde(default)]
    overrides: Value,
    #[serde(default)]
    host: Option<Value>,
    share_data: bool,
}

/// Live (starting, running or stopping) environments, of any user, of these
/// templates.
async fn live_of(db: &SqlitePool, ids: &[&str]) -> ApiResult<i64> {
    let mut n = 0;
    for id in ids {
        n += sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM environments WHERE template_id = ? \
             AND state IN ('starting', 'running', 'stopping')",
        )
        .bind(id)
        .fetch_one(db)
        .await
        .map_err(anyhow::Error::from)?;
    }
    Ok(n)
}

/// `PUT /api/admin/custom-templates/{id}`: replaces the overrides, host
/// options and data choice. Applies to launches from now on; the data choice
/// changes only while nothing of this template or its base is live.
async fn update(
    State(state): State<AppState>,
    AdminUser(admin): AdminUser,
    client: ClientInfo,
    Path(id): Path<String>,
    Json(req): Json<UpdateRequest>,
) -> ApiResult<Json<Item>> {
    let _changing = state.customs.changing.lock().await;
    let old = existing(&state, &id)?;
    let overrides = parse_overrides(req.overrides)?;
    let host = parse_host(req.host)?;
    if req.share_data != old.share_data && live_of(&state.db, &[&id, &old.base]).await? > 0 {
        return Err(ApiError::conflict(
            "live",
            "an environment of this template or its base is live; stop it before changing whose data it keeps",
        ));
    }
    let row = Stored {
        share_data: req.share_data,
        overrides,
        host,
        updated_at: db::now(),
        ..old.clone()
    };
    save(&state.db, &row, &admin.id, false).await?;
    state.customs.reload(&state.db).await?;
    let mut detail = base_status(&state, &row.base)
        .map(|(base, _)| audit_detail(&base, &row))
        .unwrap_or_else(|| json!({ "base": row.base }));
    if let Some(map) = detail.as_object_mut() {
        map.insert(
            "shareDataChanged".into(),
            json!(old.share_data != row.share_data),
        );
        map.insert("hostChanged".into(), json!(old.host != row.host));
    }
    db::audit(
        &state.db,
        Some(&admin.id),
        "custom_template.updated",
        Some(&id),
        Some(detail),
        client.ip.as_deref(),
    )
    .await?;
    Ok(Json(item(&state, &existing(&state, &id)?)))
}

/// `DELETE /api/admin/custom-templates/{id}`: refused (409 `live`) while an
/// environment of it is live. App data on the nodes and users' choices are
/// left alone, as for a removed catalog.
async fn remove(
    State(state): State<AppState>,
    AdminUser(admin): AdminUser,
    client: ClientInfo,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let _changing = state.customs.changing.lock().await;
    let old = existing(&state, &id)?;
    let live = live_of(&state.db, &[&id]).await?;
    if live > 0 {
        return Err(ApiError::conflict(
            "live",
            format!("{live} environment(s) of this template are live; stop them first"),
        ));
    }
    sqlx::query("DELETE FROM custom_templates WHERE id = ?")
        .bind(&id)
        .execute(&state.db)
        .await
        .map_err(anyhow::Error::from)?;
    state.customs.reload(&state.db).await?;
    db::audit(
        &state.db,
        Some(&admin.id),
        "custom_template.deleted",
        Some(&id),
        Some(json!({ "base": old.base, "shareData": old.share_data, "host": old.host })),
        client.ip.as_deref(),
    )
    .await?;
    Ok(Json(json!({ "removed": id })))
}

async fn store_icon(
    state: &AppState,
    admin: &AdminUser,
    client: &ClientInfo,
    id: &str,
    svg: Option<&[u8]>,
) -> ApiResult<Json<Item>> {
    let _changing = state.customs.changing.lock().await;
    existing(state, id)?;
    sqlx::query(
        "UPDATE custom_templates SET icon = ?, updated_by = ?, updated_at = ? WHERE id = ?",
    )
    .bind(svg)
    .bind(&admin.0.id)
    .bind(db::now())
    .bind(id)
    .execute(&state.db)
    .await
    .map_err(anyhow::Error::from)?;
    state.customs.reload(&state.db).await?;
    db::audit(
        &state.db,
        Some(&admin.0.id),
        "custom_template.updated",
        Some(id),
        Some(json!({ "icon": if svg.is_some() { "set" } else { "cleared" } })),
        client.ip.as_deref(),
    )
    .await?;
    Ok(Json(item(state, &existing(state, id)?)))
}

/// `PUT /api/admin/custom-templates/{id}/icon`: the body is the SVG.
async fn set_icon(
    State(state): State<AppState>,
    admin: AdminUser,
    client: ClientInfo,
    Path(id): Path<String>,
    body: Bytes,
) -> ApiResult<Json<Item>> {
    existing(&state, &id)?;
    if body.len() > MAX_ICON_BYTES {
        return Err(ApiError::bad_request(
            "bad_icon",
            format!("the icon is larger than {} KiB", MAX_ICON_BYTES / 1024),
        ));
    }
    check_svg(&body).map_err(|e| ApiError::bad_request("bad_icon", format!("the icon {e}")))?;
    let text = String::from_utf8_lossy(&body).to_ascii_lowercase();
    if !text.contains("viewbox") {
        return Err(ApiError::bad_request(
            "bad_icon",
            "the icon has no viewBox, so it can't scale",
        ));
    }
    store_icon(&state, &admin, &client, &id, Some(&body)).await
}

/// `DELETE /api/admin/custom-templates/{id}/icon`: back to the base's icon.
async fn clear_icon(
    State(state): State<AppState>,
    admin: AdminUser,
    client: ClientInfo,
    Path(id): Path<String>,
) -> ApiResult<Json<Item>> {
    store_icon(&state, &admin, &client, &id, None).await
}

/// `GET /api/admin/host-options`: what each node allows, for the editor.
async fn host_options(State(state): State<AppState>, _: AdminUser) -> ApiResult<Json<Value>> {
    let mut out = Vec::new();
    for node in db::list_nodes(&state.db).await? {
        let inventory = node
            .inventory
            .as_deref()
            .and_then(|j| serde_json::from_str::<Inventory>(j).ok());
        out.push(json!({
            "nodeId": node.id,
            "nodeName": node.name,
            "online": state.nodes.connected_since(&node.id).await.is_some(),
            "specFeatures": inventory.as_ref().map(|i| i.spec_features.clone()).unwrap_or_default(),
            "policy": inventory.and_then(|i| i.host_options),
        }));
    }
    Ok(Json(Value::Array(out)))
}

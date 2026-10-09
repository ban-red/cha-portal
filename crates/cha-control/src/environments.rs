//! Environments (plan §4; Phase 1, P1.4): the catalog, launching a template on
//! a node, stopping it, and keeping the portal's view in step with what the
//! nodes actually run.
//!
//! States: `starting → running → stopping → destroyed`, or `failed` (with a
//! reason) from any of them. Stopping one destroys it; what an app keeps for a
//! user between launches is app data ([`crate::storage`]), not the
//! environment. Node calls run in the background; the SPA polls.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use cha_wire::{
    Device, DeviceChoice, DeviceKind, EnvironmentSpec, GamepadKind, HostOptions, HostPort,
    Inventory, MediaClaims, NodeRequest, NodeResponse, SecurityProfile, sign_media_token,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tracing::{info, warn};

use crate::AppState;
use crate::apps;
use crate::auth::{ClientInfo, PlayerUser};
use crate::controllers;
use crate::custom;
use crate::db::{self, EnvironmentRow, NodeRow, Role, User};
use crate::error::{ApiError, ApiResult};
use crate::moonlight;
use crate::placement;
use crate::storage::{self, SharedDefaults};

/// The first start may pull images, and the first with a user's data under
/// the node's data root copies their old home in (a Steam library is
/// gigabytes).
const START_TIMEOUT: Duration = Duration::from_secs(600);
/// Longer than the node's longest stop grace: a `vm` app's 90 s, for the
/// guest to shut down. Other apps stop within seconds.
const STOP_TIMEOUT: Duration = Duration::from_secs(120);
/// Environments one user may have live at once.
const MAX_LIVE_PER_USER: i64 = 4;
const LIST_LIMIT: i64 = 50;
/// What every environment starts at until clients ask for their own size.
/// How long a media token stays good: long enough to carry an offer to the
/// streamer, no longer.
const MEDIA_TOKEN_SECS: i64 = 60;
const CODECS: [&str; 3] = ["h264", "hevc", "av1"];
/// The LAN tier's codec (plan §3.2), over WebTransport only.
const PYROWAVE_CODECS: [&str; 2] = ["pyrowave420", "pyrowave444"];
const WIDTH: u32 = 2560;
const HEIGHT: u32 = 1440;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/catalog", get(list_catalog))
        .route("/catalog/{id}/icon", get(catalog_icon))
        .route("/environments", get(list).post(launch))
        .route("/environments/{id}", get(show).delete(stop))
        .route("/environments/{id}/connect", post(connect))
}

// ---- The catalog ----

/// One thing a user can launch, from `images/catalog.json`.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Template {
    pub id: String,
    pub name: String,
    pub description: String,
    /// The reference to run. A catalog's names a registry and may hold
    /// `{version}`, which the node fills with its release (ADR 0017).
    pub image: String,
    /// A dev build the node runs when it has it, before pulling `image`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_image: Option<String>,
    /// `browser`, `desktop`, `test`, …
    pub class: String,
    /// Its logo, an SVG beside its image (`images/<id>/<icon>`), served at
    /// `/api/catalog/<id>/icon`; absent, the portal draws a generic icon.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    pub security: SecurityProfile,
    pub shm_mb: u32,
    /// Users keep their data for it between launches (their home in it
    /// survives stopping), unless they or an admin say otherwise: this is the
    /// default `crate::storage` starts from.
    #[serde(default)]
    pub persistent: bool,
    /// Its display has a fixed size (Steam's gamescope): the page never asks
    /// the streamer to resize, and the browser letterboxes the picture.
    #[serde(default)]
    pub fixed_size: bool,
    /// What it shares across users, unless an admin says otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shared: Option<SharedDefaults>,
    /// The virtual controller it gets unless the user chooses another; absent
    /// is `xbox360` (`crate::controllers`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gamepad: Option<GamepadKind>,
    /// The frame rate it gets unless the user chooses another (60, 90 or 120);
    /// absent is 60 (`crate::apps`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fps: Option<u32>,
    /// It needs real 3D (Steam, games): it never runs on the CPU
    /// (`crate::placement`).
    #[serde(default)]
    pub needs_gpu: bool,
    /// Extra variables for the app, from a custom environment (ADR 0021).
    /// Never read from a catalog document, never shown to players.
    #[serde(skip)]
    pub env: BTreeMap<String, String>,
    /// Set on a custom environment's resolved template; never read from a
    /// catalog document.
    #[serde(default, skip_deserializing, skip_serializing_if = "Option::is_none")]
    pub custom: Option<CustomInfo>,
}

/// What makes a template a custom environment (`crate::custom`).
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomInfo {
    /// The built-in or catalog template it was made from.
    pub base: String,
    /// It keeps its base's app data instead of its own.
    pub share_data: bool,
    /// Mounts, ports, capabilities and devices it asks of the node.
    pub host: Option<HostOptions>,
}

impl Template {
    /// The template whose app data this one uses: its own id, or its base's
    /// for a custom environment that shares data. Storage settings, the
    /// directories on the node and the one-home-at-a-time rule all go by it.
    pub fn data_id(&self) -> &str {
        match &self.custom {
            Some(c) if c.share_data => &c.base,
            _ => &self.id,
        }
    }

    /// The host options it asks for, if any.
    pub fn host_options(&self) -> Option<&HostOptions> {
        self.custom
            .as_ref()
            .and_then(|c| c.host.as_ref())
            .filter(|h| !h.is_empty())
    }
}

impl Template {
    /// The images to try, in order: the local build if any, then `image`
    /// (its `{version}` unexpanded, for the node to fill).
    pub fn image_candidates(&self) -> Vec<String> {
        self.local_image
            .iter()
            .chain(std::iter::once(&self.image))
            .cloned()
            .collect()
    }
}

/// A catalog document as read: the templates, and the optional `id` (a
/// suggested namespace) and `name` an external one may carry.
#[derive(Debug)]
pub struct Document {
    pub id: Option<String>,
    pub name: Option<String>,
    pub templates: Vec<Template>,
}

/// The catalog format this portal reads.
pub const CATALOG_VERSION: u32 = 1;

/// The most an external catalog document may be, and how many templates it may
/// hold.
pub const MAX_CATALOG_BYTES: usize = 1 << 20;
pub const MAX_CATALOG_TEMPLATES: usize = 64;

/// Where a catalog came from, which decides what it may hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CatalogSource {
    /// `images/catalog.json`, built in: bare dev names and `localImage` are
    /// fine.
    BuiltIn,
    /// A catalog an owner adds: every image names a registry, with no
    /// `localImage`, so a node only ever pulls from somewhere it can say.
    External,
}

/// Whether `image` starts with a registry host (`ghcr.io/…`,
/// `localhost:5000/…`): its first path part holds a `.` or `:` or is
/// `localhost`, as Docker reads it. A bare `cha/env-chrome:dev` doesn't.
pub fn names_registry(image: &str) -> bool {
    let Some((first, rest)) = image.split_once('/') else {
        return false;
    };
    !rest.is_empty() && (first.contains('.') || first.contains(':') || first == "localhost")
}

/// The registry host (with its port, if any) an image names, if it names one.
pub fn image_host(image: &str) -> Option<&str> {
    names_registry(image).then(|| image.split_once('/').map_or(image, |(host, _)| host))
}

/// A template id inside an external catalog, by the node's rule for the app
/// part of `<catalog>.<app>` (`cha_wire::valid_catalog_app`).
pub fn valid_local_id(id: &str) -> bool {
    cha_wire::valid_catalog_app(id)
}

/// The slug an admin gives a catalog, by the node's rule
/// (`cha_wire::valid_catalog_slug`).
pub fn valid_slug(slug: &str) -> bool {
    cha_wire::valid_catalog_slug(slug)
}

/// Reads a catalog document and checks it for `source`.
pub fn parse_catalog(json: &str, source: CatalogSource) -> Result<Vec<Template>, String> {
    parse_document(json, source).map(|d| d.templates)
}

/// Reads a catalog document and checks it for `source`. An external catalog's
/// errors say which template and which field.
pub fn parse_document(json: &str, source: CatalogSource) -> Result<Document, String> {
    if source == CatalogSource::External && json.len() > MAX_CATALOG_BYTES {
        return Err(format!(
            "the catalog is {} bytes; the most is {MAX_CATALOG_BYTES}",
            json.len()
        ));
    }
    let value: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("not a catalog: {e}"))?;
    let Some(object) = value.as_object() else {
        return Err("not a catalog: expected a JSON object with a `templates` list".into());
    };
    match object.get("version") {
        None | Some(serde_json::Value::Null) => {}
        Some(v) if v.as_u64() == Some(CATALOG_VERSION.into()) => {}
        Some(other) => {
            return Err(format!(
                "catalog version {other} isn't one this portal reads (it reads {CATALOG_VERSION})"
            ));
        }
    }
    let text = |key: &str| -> Result<Option<String>, String> {
        match object.get(key) {
            None | Some(serde_json::Value::Null) => Ok(None),
            Some(serde_json::Value::String(s)) => Ok(Some(s.clone())),
            Some(_) => Err(format!("not a catalog: `{key}` must be a string")),
        }
    };
    let (id, name) = (text("id")?, text("name")?);
    let Some(items) = object.get("templates").and_then(|t| t.as_array()) else {
        return Err("not a catalog: no `templates` list".into());
    };
    if source == CatalogSource::External && items.len() > MAX_CATALOG_TEMPLATES {
        return Err(format!(
            "the catalog has {} templates; the most is {MAX_CATALOG_TEMPLATES}",
            items.len()
        ));
    }
    let mut templates = Vec::with_capacity(items.len());
    for (i, item) in items.iter().enumerate() {
        let label = item
            .get("id")
            .and_then(|v| v.as_str())
            .map_or_else(|| format!("template #{}", i + 1), str::to_string);
        let t: Template =
            serde_json::from_value(item.clone()).map_err(|e| format!("{label}: {e}"))?;
        templates.push(t);
    }
    let mut seen = std::collections::HashSet::new();
    for t in &templates {
        if t.id.is_empty() || !seen.insert(t.id.as_str()) {
            return Err(format!("template id {:?} is empty or repeated", t.id));
        }
        if t.image.trim().is_empty() {
            return Err(format!("{}: no image", t.id));
        }
        if source == CatalogSource::External {
            check_external(t)?;
        }
    }
    Ok(Document {
        id,
        name,
        templates,
    })
}

/// What an external template may hold, beyond what every template must.
fn check_external(t: &Template) -> Result<(), String> {
    let id = &t.id;
    if !valid_local_id(id) {
        return Err(format!(
            "template id {id:?} must be lower-case letters, digits and hyphens, 1 to 40 characters, not starting or ending with a hyphen, and not `migrated` or `migrating`"
        ));
    }
    if t.local_image.is_some() {
        return Err(format!("{id}: localImage is for the built-in catalog"));
    }
    if !names_registry(&t.image) {
        return Err(format!(
            "{id}: image {:?} must name a registry (like ghcr.io/owner/name:tag)",
            t.image
        ));
    }
    if t.image.len() > 255 || t.image.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(format!(
            "{id}: image {:?} must be one reference of at most 255 characters, with no spaces",
            t.image
        ));
    }
    if t.name.trim().is_empty()
        || t.name.chars().count() > 80
        || t.name.chars().any(char::is_control)
    {
        return Err(format!("{id}: name must be 1 to 80 characters"));
    }
    if t.description.chars().count() > 500 {
        return Err(format!("{id}: description is at most 500 characters"));
    }
    let class_ok = !t.class.is_empty()
        && t.class.len() <= 24
        && t.class
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-');
    if !class_ok {
        return Err(format!(
            "{id}: class must be lower-case letters, digits and hyphens, at most 24 characters"
        ));
    }
    if t.class == moonlight::CLASS {
        return Err(format!(
            "{id}: class {:?} is the portal's own, for Moonlight hosts",
            t.class
        ));
    }
    if t.shm_mb > 16384 {
        return Err(format!("{id}: shmMb is at most 16384"));
    }
    if let Some(fps) = t.fps
        && !apps::FPS_CHOICES.contains(&fps)
    {
        return Err(format!("{id}: fps is 60, 90 or 120"));
    }
    if let Some(shared) = &t.shared
        && (shared.per_user.len() > 16
            || !shared
                .per_user
                .iter()
                .all(|p| cha_wire::valid_relative_path(p)))
    {
        return Err(format!(
            "{id}: shared.perUser holds at most 16 relative paths of plain characters"
        ));
    }
    Ok(())
}

/// The built-in catalog (`images/catalog.json`). Loaded catalogs come on top
/// of it through [`all`] and [`find`].
pub fn catalog() -> &'static [Template] {
    static CATALOG: OnceLock<Vec<Template>> = OnceLock::new();
    CATALOG.get_or_init(|| {
        parse_catalog(
            include_str!("../../../images/catalog.json"),
            CatalogSource::BuiltIn,
        )
        .expect("images/catalog.json is valid (checked by a test)")
    })
}

/// A built-in template by id.
pub(crate) fn template(id: &str) -> Option<&'static Template> {
    catalog().iter().find(|t| t.id == id)
}

/// Every template a user can launch: the built-in ones, then the available
/// ones of loaded catalogs (`<catalog>.<app>`), then custom ones
/// (`custom.<slug>`).
pub(crate) fn all(state: &AppState) -> Vec<Template> {
    let mut all = catalog().to_vec();
    all.extend(state.catalogs.available());
    all.extend(custom::available(state));
    all
}

/// The template `id` names, built in or from a loaded catalog, if a user can
/// launch it.
pub(crate) fn find(state: &AppState, id: &str) -> Option<Template> {
    template(id)
        .cloned()
        .or_else(|| state.catalogs.available_by_id(id))
        .or_else(|| custom::find(state, id, false))
}

/// Like [`find`], but also templates waiting for approval: for naming
/// environments that already exist.
pub(crate) fn find_any(state: &AppState, id: &str) -> Option<Template> {
    template(id)
        .cloned()
        .or_else(|| state.catalogs.by_id(id))
        .or_else(|| custom::find(state, id, true))
}

async fn list_catalog(State(state): State<AppState>, _: PlayerUser) -> Json<Vec<Template>> {
    Json(all(&state))
}

/// The logos the catalog names, built in like the catalog itself. A test
/// checks this list against `icon` in `images/catalog.json`.
const ICONS: &[(&str, &[u8])] = &[
    ("chrome", include_bytes!("../../../images/chrome/icon.svg")),
    (
        "firefox",
        include_bytes!("../../../images/firefox/icon.svg"),
    ),
    ("kde", include_bytes!("../../../images/kde/icon.svg")),
    ("steam", include_bytes!("../../../images/steam/icon.svg")),
    ("xfce", include_bytes!("../../../images/xfce/icon.svg")),
];

/// A template's logo. Served as an inert image: no scripts, nothing fetched.
async fn catalog_icon(
    State(state): State<AppState>,
    _: PlayerUser,
    Path(id): Path<String>,
) -> ApiResult<Response> {
    let svg = icon_of(&state, &id)
        .ok_or_else(|| ApiError::NotFound("no icon for that template".into()))?;
    Ok((
        [
            (header::CONTENT_TYPE, "image/svg+xml"),
            (header::CACHE_CONTROL, "private, max-age=86400"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (
                header::CONTENT_SECURITY_POLICY,
                "default-src 'none'; style-src 'unsafe-inline'",
            ),
        ],
        svg.to_vec(),
    )
        .into_response())
}

/// A template's logo: a built-in one, a loaded catalog's, or a custom
/// template's own (else its base's).
pub(crate) fn icon_of(state: &AppState, id: &str) -> Option<Arc<Vec<u8>>> {
    if let Some((_, svg)) = ICONS.iter().find(|(t, _)| *t == id) {
        return Some(Arc::new(svg.to_vec()));
    }
    state.catalogs.icon(id).or_else(|| custom::icon(state, id))
}

// ---- Views ----

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct EnvironmentView {
    id: String,
    template_id: String,
    template_name: String,
    owner_id: String,
    node_id: Option<String>,
    node_name: Option<String>,
    state: String,
    detail: Option<String>,
    /// How far along a download or similar step is, while it is starting and
    /// its node can count it; `None` otherwise.
    progress: Option<ProgressView>,
    /// Something to tell the user while it runs (its node noticed a problem).
    warning: Option<String>,
    /// What its containers last logged when it died, for its owner and admins.
    log: Option<Vec<String>>,
    /// The codecs its device encodes (from its node's inventory), so the page
    /// asks only for those; `None` when the node doesn't say.
    codecs: Option<Vec<String>>,
    /// The device it runs on, so the page can say whether it is hardware
    /// accelerated; `None` when the node doesn't say.
    device: Option<DeviceView>,
    /// What it uses of its node now (running, on a node that reports it).
    usage: Option<UsageView>,
    /// The host ports published for its host options, with the node's port
    /// filled in; absent without any.
    #[serde(skip_serializing_if = "Option::is_none")]
    ports: Option<Vec<HostPort>>,
    created_at: i64,
    updated_at: i64,
    /// Where the streamer listens, while it runs (the portal brokers
    /// connections from P1.5; until then this is for diagnostics).
    streamer: Option<StreamerView>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ProgressView {
    done: u64,
    total: u64,
    /// `bytes`.
    unit: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct UsageView {
    /// Percent of the node's whole CPU.
    cpu: f64,
    /// Bytes of RAM.
    mem: u64,
    /// Bytes of GPU memory; absent when the node can't tell.
    #[serde(skip_serializing_if = "Option::is_none")]
    vram: Option<u64>,
    /// The node's RAM, and the memory of the GPU it runs on, for scale.
    mem_total: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    vram_total: Option<u64>,
}

/// Fills in what a running environment uses, from its node's latest report.
fn attach_usage(state: &AppState, view: &mut EnvironmentView) {
    if view.state == "starting" {
        view.progress = state.nodes.progress(&view.id).map(|p| ProgressView {
            done: p.done,
            total: p.total,
            unit: p.unit,
        });
    }
    if view.state != "running" {
        return;
    }
    let Some(node_id) = view.node_id.as_deref() else {
        return;
    };
    let device_name = view.device.as_ref().map(|d| d.name.as_str());
    view.usage = state.nodes.usage(node_id).and_then(|(usage, _)| {
        // The GPU it runs on: by name, else the node's first.
        let vram_total = usage
            .gpus
            .iter()
            .find(|g| Some(g.name.as_str()) == device_name)
            .or(usage.gpus.first())
            .and_then(|g| g.vram_total);
        let mem_total = usage.mem_total;
        usage
            .by_environment
            .into_iter()
            .find(|e| e.id == view.id)
            .map(|e| UsageView {
                cpu: e.cpu,
                mem: e.mem,
                vram: e.vram,
                mem_total,
                vram_total,
            })
    });
}

#[derive(Serialize)]
struct DeviceView {
    kind: DeviceKind,
    name: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StreamerView {
    host: Option<String>,
    http_port: i64,
    webrtc_port: i64,
}

/// What a device's streamer offers. The inventory lists an NVIDIA GPU's NVENC
/// codecs; its streamer also makes PyroWave (Vulkan, on the same GPU), which
/// the page asks for over WebTransport.
fn device_codecs(device: Device) -> Vec<String> {
    let mut codecs = device.codecs;
    if device.kind == DeviceKind::Nvidia {
        for c in PYROWAVE_CODECS {
            if !codecs.iter().any(|have| have == c) {
                codecs.push(c.into());
            }
        }
    }
    codecs
}

/// The codecs `row`'s device encodes (None: unknown), as its view shows them.
pub(crate) async fn codecs_of(
    state: &AppState,
    row: EnvironmentRow,
) -> ApiResult<Option<Vec<String>>> {
    let owner = db::user_by_id(&state.db, &row.owner_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("no such environment".into()))?;
    Ok(view(
        state,
        row,
        &nodes_by_id(state).await?,
        &owner,
        &moonlight::Index::load(state).await?,
    )
    .codecs)
}

fn view(
    state: &AppState,
    row: EnvironmentRow,
    nodes: &HashMap<String, NodeRow>,
    viewer: &User,
    moonlight: &moonlight::Index,
) -> EnvironmentView {
    let node = row.node_id.as_ref().and_then(|id| nodes.get(id));
    let inventory = node
        .and_then(|n| n.inventory.as_deref())
        .and_then(|j| serde_json::from_str::<Inventory>(j).ok());
    // Its device; environments from before devices ran on the NVIDIA GPU.
    let device = inventory.as_ref().and_then(|inv| {
        let devices = inv.devices_or_derived();
        match row.device.as_deref() {
            Some(id) => devices.into_iter().find(|d| d.id == id),
            None => devices.into_iter().find(|d| d.kind == DeviceKind::Nvidia),
        }
    });
    let device_view = device.as_ref().map(|d| DeviceView {
        kind: d.kind,
        name: d.name.clone(),
    });
    let codecs = device.map(device_codecs);
    // A Moonlight environment's streamer is its host's.
    let codecs = moonlight.codecs(&row.template_id).or(codecs);
    let host = inventory.and_then(|inv| inv.addresses.into_iter().next());
    let streamer = match (row.state.as_str(), row.http_port, row.webrtc_port) {
        ("running", Some(http_port), Some(webrtc_port)) => Some(StreamerView {
            host,
            http_port,
            webrtc_port,
        }),
        _ => None,
    };
    let log = (row.owner_id == viewer.id || viewer.role == Role::Admin)
        .then_some(row.log.as_deref())
        .flatten()
        .and_then(|j| serde_json::from_str(j).ok());
    EnvironmentView {
        template_name: find_any(state, &row.template_id)
            .map(|t| t.name)
            .or_else(|| moonlight.name(&row.template_id).map(str::to_string))
            .unwrap_or_else(|| row.template_id.clone()),
        node_name: node.map(|n| n.name.clone()),
        id: row.id,
        template_id: row.template_id,
        owner_id: row.owner_id,
        node_id: row.node_id,
        state: row.state,
        detail: row.detail,
        progress: None,
        warning: row.warning,
        log,
        codecs,
        device: device_view,
        usage: None,
        ports: row
            .ports
            .as_deref()
            .and_then(|j| serde_json::from_str::<Vec<HostPort>>(j).ok())
            .filter(|p| !p.is_empty()),
        created_at: row.created_at,
        updated_at: row.updated_at,
        streamer,
    }
}

async fn nodes_by_id(state: &AppState) -> ApiResult<HashMap<String, NodeRow>> {
    Ok(db::list_nodes(&state.db)
        .await?
        .into_iter()
        .map(|n| (n.id.clone(), n))
        .collect())
}

/// The environment, if this user may see it (its owner, or an admin).
pub(crate) async fn visible(state: &AppState, user: &User, id: &str) -> ApiResult<EnvironmentRow> {
    db::environment_by_id(&state.db, id)
        .await?
        .filter(|e| e.owner_id == user.id || user.role == Role::Admin)
        .ok_or_else(|| ApiError::NotFound("no such environment".into()))
}

// ---- The API ----

async fn list(
    State(state): State<AppState>,
    PlayerUser(user): PlayerUser,
) -> ApiResult<Json<Vec<EnvironmentView>>> {
    let rows = db::list_environments(&state.db, Some(&user.id), LIST_LIMIT).await?;
    let nodes = nodes_by_id(&state).await?;
    let index = moonlight::Index::load(&state).await?;
    let mut views: Vec<_> = rows
        .into_iter()
        .map(|r| view(&state, r, &nodes, &user, &index))
        .collect();
    for v in &mut views {
        attach_usage(&state, v);
    }
    Ok(Json(views))
}

async fn show(
    State(state): State<AppState>,
    PlayerUser(user): PlayerUser,
    Path(id): Path<String>,
) -> ApiResult<Json<EnvironmentView>> {
    let row = visible(&state, &user, &id).await?;
    let mut v = view(
        &state,
        row,
        &nodes_by_id(&state).await?,
        &user,
        &moonlight::Index::load(&state).await?,
    );
    attach_usage(&state, &mut v);
    Ok(Json(v))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LaunchRequest {
    pub(crate) template_id: String,
    /// Where to run it: a node's id, and one of its devices' ids (from
    /// `GET /api/placements`). Neither is the automatic choice; a node alone
    /// is its best device.
    #[serde(default)]
    pub(crate) node: Option<String>,
    #[serde(default)]
    pub(crate) device: Option<String>,
}

/// Who asked for a launch or a stop, for the audit log: where it came from
/// (the browser's address) and by which door, when it wasn't the API (`via`).
#[derive(Clone, Copy, Default)]
pub(crate) struct Caller<'a> {
    pub(crate) ip: Option<&'a str>,
    pub(crate) via: Option<&'static str>,
}

impl Caller<'_> {
    /// `details` with the door the request came through, if it wasn't the API.
    fn audit_details(&self, mut details: serde_json::Value) -> serde_json::Value {
        if let (Some(via), Some(map)) = (self.via, details.as_object_mut()) {
            map.insert("via".into(), json!(via));
        }
        details
    }
}

async fn launch(
    State(state): State<AppState>,
    PlayerUser(user): PlayerUser,
    client: ClientInfo,
    Json(req): Json<LaunchRequest>,
) -> ApiResult<Json<EnvironmentView>> {
    let caller = Caller {
        ip: client.ip.as_deref(),
        via: None,
    };
    let row = launch_environment(&state, &user, caller, &req).await?;
    Ok(Json(view(
        &state,
        row,
        &nodes_by_id(&state).await?,
        &user,
        &moonlight::Index::load(&state).await?,
    )))
}

/// Starts an environment of `req`'s template for `user`: the one way anything
/// launches (the API, a Moonlight client's launch), so the same rules apply to
/// all. Checks who may launch what and how many, picks the user's controller,
/// frame rate and storage, places it (on `req`'s node and device, else the best
/// there is), records it and has the node start it in the background. The row
/// it returns is `starting`.
pub(crate) async fn launch_environment(
    state: &AppState,
    user: &User,
    caller: Caller<'_>,
    req: &LaunchRequest,
) -> ApiResult<EnvironmentRow> {
    if user.role == Role::Guest {
        return Err(ApiError::forbidden(
            "guests_cannot_launch",
            "guests can't launch environments",
        ));
    }
    let template = moonlight::resolve_template(state, &req.template_id)
        .await?
        .ok_or_else(|| ApiError::bad_request("unknown_template", "no such template"))?;
    let template = &template;
    // A host plays for one person at a time: the check and the new row are one step.
    let _host = if template.class == moonlight::CLASS {
        let guard = state.moonlight.launch.lock().await;
        moonlight::check_free(state, template).await?;
        Some(guard)
    } else {
        None
    };
    if db::count_live_environments(&state.db, &user.id).await? >= MAX_LIVE_PER_USER {
        return Err(ApiError::conflict(
            "too_many_environments",
            format!("you can have {MAX_LIVE_PER_USER} environments at once; stop one first"),
        ));
    }
    let settings = storage::effective_for(state, &user.id, template).await?;
    // Two environments would share one home.
    if settings.persistent
        && let Some(live) = db::live_environments_of(&state.db, &user.id, &template.id)
            .await?
            .into_iter()
            .next()
    {
        let node = match &live.node_id {
            Some(id) => db::node_by_id(&state.db, id).await?.map(|n| n.name),
            None => None,
        };
        return Err(ApiError::conflict(
            "already_running",
            format!(
                "your {} is already running{}; it keeps one home, so connect to that one",
                template.name,
                node.map(|n| format!(" on {n}")).unwrap_or_default()
            ),
        ));
    }
    // A custom environment sharing its base's data (or the base, while one
    // does) would write the same home as the one that is live.
    if settings.persistent
        && let Some(live) = storage::live_on_data(state, &user.id, template.data_id()).await?
    {
        let name = find_any(state, &live.template_id).map_or(live.template_id.clone(), |t| t.name);
        return Err(ApiError::conflict(
            "data_in_use",
            format!(
                "your {name} (environment {}) is {} and keeps the same app data as {}; stop it first",
                live.id, live.state, template.name
            ),
        ));
    }
    let gamepad = controllers::effective_for(state, &user.id, template).await?;
    let fps = apps::fps_for(state, &user.id, template).await?;
    let (node, device, gateway) = if template.class == moonlight::CLASS {
        let (node, device, gateway) = moonlight::place(state, template).await?;
        (node, device, Some(Box::new(gateway)))
    } else {
        let (node, device) = place(state, &user.id, template, req).await?;
        (node, device, None)
    };
    let app_data = storage::spec_storage(&user.id, template, settings);
    // A node that predates app data would drop what it can't read, and the
    // user's data would quietly not be kept (or shared). Placement leaves
    // such nodes out; this is for one that changed since.
    if app_data.is_some() && !storage::node_has_storage(&node) {
        return Err(ApiError::conflict(
            "node_needs_update",
            format!(
                "{} can't keep or share app data yet: update its agent (it reports its data root once it can)",
                node.name
            ),
        ));
    }
    // Placement leaves out nodes that would drop what the spec adds; this is
    // for one whose agent changed since.
    if let Some(why) = placement::spec_refusal(
        &placement::Needs::of(template, app_data.as_ref()),
        storage::node_inventory(&node).as_ref(),
    ) {
        return Err(ApiError::conflict(
            "node_needs_update",
            format!("{} can't run {}: {why}", node.name, template.name),
        ));
    }
    let id = db::new_id();
    db::insert_environment(
        &state.db,
        &id,
        &user.id,
        &template.id,
        &node.id,
        Some(&device.id),
        "starting",
    )
    .await?;
    db::audit(
        &state.db,
        Some(&user.id),
        "environment.launched",
        Some(&id),
        Some(caller.audit_details(json!({
            "template": template.id,
            "node": node.name,
            "device": device.id,
            "host": template.host_options(),
        }))),
        caller.ip,
    )
    .await?;
    info!(%id, template = %template.id, node = %node.name, device = %device.id, user = %user.username, via = caller.via.unwrap_or("api"), "launching");
    let spec = environment_spec(
        id.clone(),
        &user.id,
        template,
        state.media_key.public_b64(),
        app_data.map(Box::new),
        Settings {
            gamepad,
            fps,
            device,
            gateway,
        },
    );
    tokio::spawn(start_on_node(state.clone(), node.id.clone(), spec));
    db::environment_by_id(&state.db, &id)
        .await?
        .ok_or_else(|| ApiError::Internal(anyhow::anyhow!("the new environment vanished")))
}

/// What a launch settles besides the app: the user's controller and frame rate
/// and the device it runs on.
struct Settings {
    gamepad: GamepadKind,
    fps: u32,
    device: Device,
    /// The Moonlight host and app it streams, for a Moonlight template.
    gateway: Option<Box<cha_wire::GatewaySpec>>,
}

/// What a node runs for `owner`'s launch of `template`, with the app data
/// ([`storage::spec_storage`]) it mounts.
fn environment_spec(
    id: String,
    owner: &str,
    template: &Template,
    portal_key: String,
    storage: Option<Box<cha_wire::Storage>>,
    settings: Settings,
) -> EnvironmentSpec {
    let Settings {
        gamepad,
        fps,
        device,
        gateway,
    } = settings;
    EnvironmentSpec {
        id,
        image: template.image_candidates().remove(0),
        image_candidates: template.image_candidates(),
        security: template.security,
        shm_mb: template.shm_mb,
        width: WIDTH,
        height: HEIGHT,
        fps,
        portal_key,
        // Not the old way of keeping a home (a volume the portal named): the
        // node moves such a volume's files into the new directory itself.
        home: None,
        owner: owner.to_string(),
        template: template.id.clone(),
        // The data lives on the node it was first made on: placement keeps a
        // user on one node while there is one (Phase 3 makes it follow).
        storage,
        // Left out for the default, so an older node reads what it always did.
        gamepad: (gamepad != GamepadKind::default()).then_some(gamepad),
        // An older node ignores it and uses its NVIDIA GPU, which is all it
        // offered.
        device: Some(Box::new(DeviceChoice {
            id: device.id,
            kind: device.kind,
            render_node: device.render_node,
        })),
        gateway,
        env: (!template.env.is_empty()).then(|| Box::new(template.env.clone())),
        host: template.host_options().cloned().map(Box::new),
    }
}

async fn stop(
    State(state): State<AppState>,
    PlayerUser(user): PlayerUser,
    client: ClientInfo,
    Path(id): Path<String>,
) -> ApiResult<Json<EnvironmentView>> {
    let row = visible(&state, &user, &id).await?;
    let caller = Caller {
        ip: client.ip.as_deref(),
        via: None,
    };
    stop_environment(&state, &user, caller, &row).await?;
    let row = visible(&state, &user, &id).await?;
    Ok(Json(view(
        &state,
        row,
        &nodes_by_id(&state).await?,
        &user,
        &moonlight::Index::load(&state).await?,
    )))
}

/// Begins stopping `row`, which `user` may stop (the caller checked): records
/// it and has its node stop it in the background. One that is already stopping
/// or over is left alone.
pub(crate) async fn stop_environment(
    state: &AppState,
    user: &User,
    caller: Caller<'_>,
    row: &EnvironmentRow,
) -> ApiResult<()> {
    let id = &row.id;
    if db::transition_environment(&state.db, id, &["starting", "running"], "stopping", None).await?
    {
        db::audit(
            &state.db,
            Some(&user.id),
            "environment.stopped",
            Some(id),
            Some(caller.audit_details(json!({ "template": row.template_id }))),
            caller.ip,
        )
        .await?;
        if let Some(node_id) = row.node_id.clone() {
            tokio::spawn(stop_on_node(state.clone(), node_id, id.clone(), None));
        } else {
            db::transition_environment(&state.db, id, &["stopping"], "destroyed", None).await?;
        }
    }
    Ok(())
}

// The wire names are `webrtc`, `webtransport` and `websocket`.
#[allow(clippy::enum_variant_names)]
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum Transport {
    #[default]
    WebRtc,
    /// `cha-stream/1` over WebTransport: the browser connects to the streamer
    /// itself, with the URLs and certificate hash this returns.
    WebTransport,
    /// `cha-stream/1` over a WebSocket through the portal and the node
    /// (ADR 0022): the browser opens the one-use ticket this returns.
    WebSocket,
}

#[derive(Deserialize)]
pub(crate) struct ConnectRequest {
    /// `h264`, `hevc` or `av1`: what this browser decodes best.
    codec: String,
    #[serde(default)]
    transport: Transport,
    /// The browser's WebRTC offer (`{"type": "offer", "sdp": …}`).
    #[serde(default)]
    offer: serde_json::Value,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConnectResponse {
    codec: String,
    pub(crate) transport: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    answer: Option<serde_json::Value>,
    /// WebTransport: one URL per address of the node, best first, each with
    /// the media token; the browser takes the first that connects.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    urls: Vec<String>,
    /// WebTransport: the streamer's self-signed certificate (SHA-256, hex),
    /// for `serverCertificateHashes`.
    #[serde(skip_serializing_if = "Option::is_none")]
    cert_hash: Option<String>,
}

/// Brokers a WebRTC connection: the offer goes to the environment's streamer
/// through its node, with a short-lived media token only this portal can sign;
/// the answer comes back the same way. Media then flows node → browser.
/// From a streamer's `/info`: a WebTransport URL per address it has (best
/// first), carrying the media token, and its certificate's hash.
fn webtransport_urls(
    info: &serde_json::Value,
    codec: &str,
    token: &str,
) -> Option<(Vec<String>, String)> {
    let port = info["wt_port"].as_u64().filter(|p| *p > 0)?;
    let hash = info["cert_hash_hex"].as_str().filter(|h| !h.is_empty())?;
    let urls = info["addresses"]
        .as_array()?
        .iter()
        .filter_map(|a| a.as_str())
        .map(|addr| {
            let host = if addr.contains(':') {
                format!("[{addr}]")
            } else {
                addr.to_string()
            };
            format!("https://{host}:{port}/media?codec={codec}&token={token}")
        })
        .collect::<Vec<_>>();
    (!urls.is_empty()).then(|| (urls, hash.to_string()))
}

async fn connect(
    State(state): State<AppState>,
    PlayerUser(user): PlayerUser,
    client: ClientInfo,
    Path(id): Path<String>,
    Json(req): Json<ConnectRequest>,
) -> ApiResult<Json<ConnectResponse>> {
    let row = visible(&state, &user, &id).await?;
    let role = if row.owner_id == user.id {
        "owner"
    } else {
        "admin"
    };
    let guest = Guest {
        sub: user.id.clone(),
        role,
        slot: None,
    };
    let (codec, response) = broker(&state, &row, req, guest).await?;
    db::audit(
        &state.db,
        Some(&user.id),
        "environment.connected",
        Some(&id),
        Some(json!({ "codec": codec, "transport": response.transport })),
        client.ip.as_deref(),
    )
    .await?;
    Ok(Json(response))
}

/// Who a media token is for: its `sub`, `role` and, for a player, gamepad slot.
pub(crate) struct Guest {
    pub(crate) sub: String,
    pub(crate) role: &'static str,
    pub(crate) slot: Option<u8>,
}

/// Connects `guest` to a running environment, for the owner's or an admin's
/// own request and for a share link alike: checks the request, signs a media
/// token and has the environment's node broker the connection. Returns the
/// codec asked for too.
pub(crate) async fn broker(
    state: &AppState,
    row: &EnvironmentRow,
    req: ConnectRequest,
    guest: Guest,
) -> ApiResult<(String, ConnectResponse)> {
    let id = row.id.clone();
    if row.state != "running" {
        return Err(ApiError::conflict(
            "not_running",
            format!("the environment is {}", row.state),
        ));
    }
    let pyrowave = PYROWAVE_CODECS.contains(&req.codec.as_str());
    if !CODECS.contains(&req.codec.as_str()) && !pyrowave {
        return Err(ApiError::bad_request(
            "bad_codec",
            "codec must be h264, hevc, av1, pyrowave420 or pyrowave444",
        ));
    }
    if pyrowave && req.transport != Transport::WebTransport {
        return Err(ApiError::bad_request(
            "bad_codec",
            "PyroWave goes over WebTransport only",
        ));
    }
    if req.transport == Transport::WebRtc && req.offer.get("sdp").and_then(|s| s.as_str()).is_none()
    {
        return Err(ApiError::bad_request("bad_offer", "the offer has no SDP"));
    }
    let node_id = row
        .node_id
        .clone()
        .ok_or_else(|| ApiError::conflict("no_node", "the environment's node was removed"))?;
    if req.transport == Transport::WebSocket
        && !crate::relay::node_supports_relay(state, &node_id).await
    {
        return Err(ApiError::conflict(
            "node_outdated",
            "The node needs updating to stream over the internet",
        ));
    }
    let claims = MediaClaims {
        env: id.clone(),
        sub: guest.sub.clone(),
        role: guest.role.into(),
        slot: guest.slot,
        exp: db::now() + MEDIA_TOKEN_SECS,
    };
    let media_token = sign_media_token(&state.media_key, &claims);
    let response = match req.transport {
        Transport::WebRtc => {
            let reply = state
                .nodes
                .request(
                    &node_id,
                    NodeRequest::Connect {
                        environment_id: id.clone(),
                        codec: req.codec.clone(),
                        offer: req.offer,
                        media_token,
                    },
                )
                .await?;
            let NodeResponse::Answer { answer } = reply else {
                return Err(ApiError::conflict(
                    "node_error",
                    "the node answered something else",
                ));
            };
            ConnectResponse {
                codec: req.codec.clone(),
                transport: "webrtc",
                answer: Some(answer),
                urls: Vec::new(),
                cert_hash: None,
            }
        }
        Transport::WebTransport => {
            let reply = state
                .nodes
                .request(
                    &node_id,
                    NodeRequest::StreamerInfo {
                        environment_id: id.clone(),
                    },
                )
                .await?;
            let NodeResponse::StreamerInfo { info } = reply else {
                return Err(ApiError::conflict(
                    "node_error",
                    "the node answered something else",
                ));
            };
            let (urls, cert_hash) =
                webtransport_urls(&info, &req.codec, &media_token).ok_or_else(|| {
                    ApiError::conflict(
                        "no_webtransport",
                        "this environment's streamer doesn't offer WebTransport",
                    )
                })?;
            ConnectResponse {
                codec: req.codec.clone(),
                transport: "webtransport",
                answer: None,
                urls,
                cert_hash: Some(cert_hash),
            }
        }
        Transport::WebSocket => {
            let ticket = state.relays.issue(crate::relay::Ticket {
                environment_id: id.clone(),
                node_id,
                codec: req.codec.clone(),
                media_token,
                sub: guest.sub,
            })?;
            ConnectResponse {
                codec: req.codec.clone(),
                transport: "websocket",
                answer: None,
                urls: vec![format!("/api/media/{ticket}")],
                cert_hash: None,
            }
        }
    };
    Ok((req.codec, response))
}

/// Where a launch runs: the device the request names, or the best one
/// ([`placement`]), and the node it is on.
async fn place(
    state: &AppState,
    user_id: &str,
    template: &Template,
    req: &LaunchRequest,
) -> ApiResult<(NodeRow, Device)> {
    let nodes = placement::online_nodes(state).await?;
    let placements = placement::for_template(state, user_id, template, &nodes).await?;
    let wanted = match (&req.node, &req.device) {
        (None, None) => None,
        (None, Some(_)) => {
            return Err(ApiError::bad_request(
                "bad_placement",
                "a device is chosen with its node",
            ));
        }
        (Some(node), device) => Some((node, device.as_deref())),
    };
    let chosen = match wanted {
        None => placement::best(&placements.options).ok_or_else(|| {
            ApiError::conflict(
                "no_node",
                format!(
                    "no online node can run {}: {}",
                    template.name,
                    placement::nothing_allowed(&placements.options)
                ),
            )
        })?,
        Some((node, device)) => {
            // Best first, so a node alone is its best device.
            let chosen = placements
                .options
                .iter()
                .find(|o| o.node == *node && device.is_none_or(|d| o.device == d));
            let chosen = chosen.ok_or_else(|| {
                ApiError::bad_request(
                    "unknown_placement",
                    "that node or device isn't available (it may be offline)",
                )
            })?;
            if !chosen.allowed {
                return Err(ApiError::bad_request(
                    "placement_not_allowed",
                    format!(
                        "{} on {} can't run {}: {}",
                        chosen.label,
                        chosen.node_name,
                        template.name,
                        chosen.reason.as_deref().unwrap_or("not allowed")
                    ),
                ));
            }
            chosen
        }
    };
    let view = nodes
        .iter()
        .find(|n| n.id == chosen.node)
        .and_then(|n| n.devices.iter().find(|d| d.id == chosen.device))
        .cloned()
        .ok_or_else(|| ApiError::conflict("no_node", "the node changed; try again"))?;
    let node = db::node_by_id(&state.db, &chosen.node)
        .await?
        .ok_or_else(|| ApiError::conflict("no_node", "the node was removed"))?;
    Ok((node, view))
}

// ---- Talking to nodes ----

async fn start_on_node(state: AppState, node_id: String, spec: EnvironmentSpec) {
    let id = spec.id.clone();
    let reply = state
        .nodes
        .request_timeout(
            &node_id,
            NodeRequest::StartEnvironment { environment: spec },
            START_TIMEOUT,
        )
        .await;
    let result = match reply {
        Ok(NodeResponse::EnvironmentStarted {
            streamer, ports, ..
        }) => {
            if let Err(err) = db::set_environment_ports(&state.db, &id, &ports).await {
                warn!(%id, "recording the published ports: {err}");
            }
            match db::set_environment_running(
                &state.db,
                &id,
                streamer.http_port,
                streamer.webrtc_port,
            )
            .await
            {
                // Stopped while starting: the stop is already on its way to the node.
                Ok(_) => Ok(()),
                Err(err) => Err(err.to_string()),
            }
        }
        Ok(other) => Err(format!("the node answered {other:?}")),
        Err(err) => Err(err.to_string()),
    };
    state.nodes.clear_progress(&id);
    if let Err(reason) = result {
        warn!(%id, %node_id, "start failed: {reason}");
        let _ = db::transition_environment(&state.db, &id, &["starting"], "failed", Some(&reason))
            .await;
    }
}

/// Stops an environment on its node and records the end; `reason` is what the
/// list says about it afterwards (when it wasn't the owner who stopped it).
pub(crate) async fn stop_on_node(
    state: AppState,
    node_id: String,
    id: String,
    reason: Option<String>,
) {
    state.nodes.clear_progress(&id);
    let reply = state
        .nodes
        .request_timeout(
            &node_id,
            NodeRequest::StopEnvironment { id: id.clone() },
            STOP_TIMEOUT,
        )
        .await;
    let (to, detail) = match reply {
        Ok(_) => ("destroyed", reason),
        // It goes when the node reconnects and reports it (reconcile).
        Err(ApiError::Conflict("node_offline", _)) => (
            "destroyed",
            Some("its node was offline; the node removes it when it reconnects".to_string()),
        ),
        Err(err) => ("failed", Some(format!("stopping: {err}"))),
    };
    let _ = db::transition_environment(&state.db, &id, &["stopping"], to, detail.as_deref()).await;
}

/// A node reported what it runs (after every connect): environments it lost
/// fail, and ones the portal no longer wants are stopped. `expected` is what
/// the portal had for the node when the report arrived: anything started
/// since isn't in `running`, and mustn't count as lost.
pub async fn reconcile(
    state: AppState,
    node_id: String,
    expected: Vec<EnvironmentRow>,
    running: Vec<String>,
) {
    let wanted: Vec<&str> = expected
        .iter()
        .filter(|e| e.state == "starting" || e.state == "running")
        .map(|e| e.id.as_str())
        .collect();
    for env in &expected {
        let present = running.contains(&env.id);
        let moved = match (env.state.as_str(), present) {
            ("running", false) => {
                db::transition_environment(
                    &state.db,
                    &env.id,
                    &["running"],
                    "failed",
                    Some("its node no longer runs it"),
                )
                .await
            }
            ("stopping", false) => {
                db::transition_environment(&state.db, &env.id, &["stopping"], "destroyed", None)
                    .await
            }
            _ => Ok(false),
        };
        if let Err(err) = moved {
            warn!(id = %env.id, "reconciling: {err}");
        }
    }
    for id in running
        .into_iter()
        .filter(|id| !wanted.contains(&id.as_str()))
    {
        info!(%id, %node_id, "stopping an environment the portal no longer wants");
        let state = state.clone();
        let node_id = node_id.clone();
        tokio::spawn(async move {
            let _ = state
                .nodes
                .request_timeout(
                    &node_id,
                    NodeRequest::StopEnvironment { id: id.clone() },
                    STOP_TIMEOUT,
                )
                .await;
            let _ =
                db::transition_environment(&state.db, &id, &["stopping"], "destroyed", None).await;
        });
    }
}

/// An environment stopped on its own on its node.
pub async fn exited(state: AppState, id: String, detail: String, failed: bool, log: Vec<String>) {
    let detail: String = detail.chars().take(DETAIL_CHARS).collect();
    state.nodes.clear_progress(&id);
    let log = bounded_log(log);
    let log = (!log.is_empty()).then(|| serde_json::to_string(&log).unwrap_or_default());
    if let Err(err) = db::record_exit(&state.db, &id, failed, &detail, log.as_deref()).await {
        warn!(%id, "recording an exit: {err}");
    }
}

/// What the portal keeps of a detail, whatever a node sends.
const DETAIL_CHARS: usize = 300;
/// And of a log: its last lines, each cut short.
const LOG_LINES: usize = 60;
const LOG_LINE_CHARS: usize = 300;

fn bounded_log(log: Vec<String>) -> Vec<String> {
    let skip = log.len().saturating_sub(LOG_LINES);
    log.into_iter()
        .skip(skip)
        .map(|line| line.chars().take(LOG_LINE_CHARS).collect())
        .collect()
}

#[cfg(test)]
mod tests {
    use cha_wire::DeviceKind;

    use super::*;

    #[test]
    fn a_log_is_cut_to_its_last_lines_and_short_lines() {
        let log: Vec<String> = (0..100).map(|n| format!("line {n}")).collect();
        let kept = bounded_log(log);
        assert_eq!(kept.len(), LOG_LINES);
        assert_eq!(kept[0], "line 40");
        let long = bounded_log(vec!["x".repeat(1000)]);
        assert_eq!(long[0].chars().count(), LOG_LINE_CHARS);
    }

    #[test]
    fn the_catalog_parses() {
        let ids: Vec<&str> = catalog().iter().map(|t| t.id.as_str()).collect();
        assert!(ids.contains(&"chrome"));
        assert!(ids.contains(&"test-pattern"));
        let chrome = template("chrome").unwrap();
        assert_eq!(chrome.security, SecurityProfile::Browser);
        assert!(chrome.shm_mb >= 512);
        assert!(template("steam").unwrap().persistent);
        assert!(template("kde").unwrap().persistent);
        assert!(!chrome.persistent);
        // Games need a GPU; browsers and desktops make do without.
        assert!(template("steam").unwrap().needs_gpu);
        assert!(
            catalog()
                .iter()
                .filter(|t| t.id != "steam")
                .all(|t| !t.needs_gpu)
        );
    }

    const ONE: &str = r#"{"templates":[{"id":"x","name":"X","description":"d",
        "image":"IMAGE","class":"test","security":"standard","shmMb":64LOCAL}]}"#;

    fn one(image: &str, local: Option<&str>, version: Option<&str>) -> String {
        let local = local.map_or(String::new(), |l| format!(r#","localImage":"{l}""#));
        let doc = ONE.replace("IMAGE", image).replace("LOCAL", &local);
        match version {
            Some(v) => doc.replacen('{', &format!(r#"{{"version":{v},"#), 1),
            None => doc,
        }
    }

    #[test]
    fn the_built_in_catalog_names_published_images_and_local_builds() {
        let src = include_str!("../../../images/catalog.json");
        assert!(src.contains(r#""version": 1"#));
        for t in catalog() {
            assert_eq!(
                t.image,
                format!("ghcr.io/ban-red/cha-env-{}:{{version}}", t.id),
                "{}",
                t.id
            );
            assert_eq!(
                t.local_image.as_deref(),
                Some(format!("cha/env-{}:dev", t.id).as_str())
            );
            assert!(names_registry(&t.image));
            assert_eq!(
                t.image_candidates(),
                [t.local_image.clone().unwrap(), t.image.clone()]
            );
        }
    }

    #[test]
    fn a_catalog_reads_with_or_without_version_and_local_image() {
        for (doc, local) in [
            (one("cha/env-x:dev", None, None), None),
            (one("cha/env-x:dev", None, Some("1")), None),
            (
                one("ghcr.io/o/x:{version}", Some("cha/env-x:dev"), Some("1")),
                Some("cha/env-x:dev"),
            ),
        ] {
            let t = parse_catalog(&doc, CatalogSource::BuiltIn).unwrap();
            assert_eq!(t.len(), 1);
            assert_eq!(t[0].local_image.as_deref(), local);
            let want = local.map_or(1, |_| 2);
            assert_eq!(t[0].image_candidates().len(), want);
            assert_eq!(t[0].image_candidates().last().unwrap(), &t[0].image);
        }
        let err =
            parse_catalog(&one("a/b:1", None, Some("2")), CatalogSource::BuiltIn).unwrap_err();
        assert!(err.contains("version 2"), "{err}");
        assert!(parse_catalog("{", CatalogSource::BuiltIn).is_err());
    }

    #[test]
    fn an_external_catalog_must_name_registries_and_have_no_local_builds() {
        let ok = one("ghcr.io/o/x:1", None, Some("1"));
        assert!(parse_catalog(&ok, CatalogSource::External).is_ok());
        assert!(
            parse_catalog(
                &one("localhost:5000/x:1", None, None),
                CatalogSource::External
            )
            .is_ok()
        );
        for bare in ["cha/env-x:dev", "x", "ubuntu:24.04", "o/x"] {
            let err = parse_catalog(&one(bare, None, None), CatalogSource::External).unwrap_err();
            assert!(err.contains("registry"), "{bare}: {err}");
            // The built-in catalog may.
            assert!(parse_catalog(&one(bare, None, None), CatalogSource::BuiltIn).is_ok());
        }
        let err = parse_catalog(
            &one("ghcr.io/o/x:1", Some("cha/env-x:dev"), None),
            CatalogSource::External,
        )
        .unwrap_err();
        assert!(err.contains("localImage"), "{err}");
        // Repeated ids are refused for any source.
        let twice = ok.replace(r#"}]}"#, r#"},{"id":"x","name":"Y","description":"d","image":"ghcr.io/o/y:1","class":"test","security":"standard","shmMb":64}]}"#);
        assert!(parse_catalog(&twice, CatalogSource::External).is_err());
    }

    fn spec_of(template: &Template) -> EnvironmentSpec {
        environment_spec(
            "e1".into(),
            "u1",
            template,
            "key".into(),
            None,
            Settings {
                gamepad: GamepadKind::default(),
                fps: 60,
                device: Device {
                    id: "cpu".into(),
                    kind: DeviceKind::Cpu,
                    name: "CPU".into(),
                    render_node: None,
                    vendor: None,
                    codecs: vec![],
                    cores: None,
                },
                gateway: None,
            },
        )
    }

    #[test]
    fn the_launch_spec_carries_the_candidates_and_image_is_the_first() {
        let chrome = spec_of(template("chrome").unwrap());
        assert_eq!(
            chrome.image_candidates,
            [
                "cha/env-chrome:dev",
                "ghcr.io/ban-red/cha-env-chrome:{version}"
            ]
        );
        assert_eq!(chrome.image, "cha/env-chrome:dev");
        let mut bare = template("chrome").unwrap().clone();
        bare.local_image = None;
        let spec = spec_of(&bare);
        assert_eq!(spec.image_candidates, [bare.image.clone()]);
        assert_eq!(spec.image, bare.image);
    }

    #[test]
    fn every_catalog_icon_is_built_in_and_inert() {
        let named: Vec<&str> = catalog()
            .iter()
            .filter(|t| t.icon.is_some())
            .map(|t| t.id.as_str())
            .collect();
        let built: Vec<&str> = ICONS.iter().map(|(id, _)| *id).collect();
        for id in &named {
            assert!(built.contains(id), "{id}: icon not in ICONS");
        }
        for id in &built {
            assert!(named.contains(id), "{id}: in ICONS but not the catalog");
        }
        for (id, svg) in ICONS {
            let svg = std::str::from_utf8(svg).unwrap();
            assert!(svg.contains("<svg"), "{id}: not an SVG");
            assert!(
                svg.contains("viewBox"),
                "{id}: no viewBox, so it won't scale"
            );
            assert!(!svg.contains("<script"), "{id}: has a script");
        }
    }

    #[test]
    fn every_catalog_frame_rate_is_one_we_offer() {
        for t in catalog() {
            if let Some(fps) = t.fps {
                assert!(apps::FPS_CHOICES.contains(&fps), "{}: fps {fps}", t.id);
            }
        }
    }

    #[test]
    fn a_spec_carries_the_storage_and_never_the_old_home_volume() {
        let owner = "01a10527-f79f-761b-962f-4b26924a2e68";
        let settings = storage::Effective {
            persistent: true,
            shared_access: storage::SharedAccess::Write,
        };
        let steam = template("steam").unwrap();
        let spec = environment_spec(
            "e1".into(),
            owner,
            steam,
            "k".into(),
            storage::spec_storage(owner, steam, settings).map(Box::new),
            Settings {
                gamepad: GamepadKind::Xbox360,
                fps: 60,
                device: test_device(DeviceKind::Nvidia),
                gateway: None,
            },
        );
        assert_eq!(spec.home, None);
        assert_eq!(spec.fps, 60);
        assert_eq!(
            (spec.owner.as_str(), spec.template.as_str()),
            (owner, "steam")
        );
        let storage = spec.storage.expect("steam keeps and shares");
        assert_eq!(
            storage.home.as_deref(),
            Some("users/01a10527-f79f-761b-962f-4b26924a2e68/steam")
        );
        assert_eq!(storage.check(owner, "steam"), Ok(()));

        // Nothing kept or shared: nothing sent.
        let chrome = template("chrome").unwrap();
        let plain = environment_spec(
            "e2".into(),
            owner,
            chrome,
            "k".into(),
            None,
            Settings {
                gamepad: GamepadKind::Dualsense,
                fps: 120,
                device: test_device(DeviceKind::Vaapi),
                gateway: None,
            },
        );
        let device = plain.device.as_ref().expect("a device is always said");
        assert_eq!(device.kind, DeviceKind::Vaapi);
        assert_eq!(device.render_node.as_deref(), Some("/dev/dri/renderD129"));
        assert_eq!(spec.device.as_ref().unwrap().render_node, None);
        assert_eq!(plain.gamepad, Some(GamepadKind::Dualsense));
        assert_eq!(plain.fps, 120);
        assert_eq!(spec.gamepad, None);
        assert_eq!(plain.storage, None);
        assert_eq!(plain.home, None);
    }

    #[test]
    fn an_nvidia_device_offers_pyrowave_too() {
        assert_eq!(
            device_codecs(test_device(DeviceKind::Nvidia)),
            ["h264", "pyrowave420", "pyrowave444"]
        );
        let mut listed = test_device(DeviceKind::Nvidia);
        listed.codecs.push("pyrowave444".into());
        assert_eq!(
            device_codecs(listed),
            ["h264", "pyrowave444", "pyrowave420"]
        );
        assert_eq!(device_codecs(test_device(DeviceKind::Vaapi)), ["h264"]);
        assert_eq!(device_codecs(test_device(DeviceKind::Cpu)), ["h264"]);
    }

    fn test_device(kind: DeviceKind) -> Device {
        Device {
            id: format!("{}:0", kind.as_str()),
            kind,
            name: "d".into(),
            render_node: (kind == DeviceKind::Vaapi).then(|| "/dev/dri/renderD129".into()),
            vendor: None,
            codecs: vec!["h264".into()],
            cores: None,
        }
    }

    #[test]
    fn webtransport_urls_cover_every_address() {
        let info = json!({
            "wt_port": 7602,
            "cert_hash_hex": "ab12",
            "addresses": ["192.168.1.5", "100.64.0.7", "fd7a::1"],
        });
        let (urls, hash) = webtransport_urls(&info, "hevc", "tok").unwrap();
        assert_eq!(hash, "ab12");
        assert_eq!(
            urls[0],
            "https://192.168.1.5:7602/media?codec=hevc&token=tok"
        );
        assert_eq!(urls[2], "https://[fd7a::1]:7602/media?codec=hevc&token=tok");
        assert!(webtransport_urls(&json!({ "wt_port": 0 }), "hevc", "tok").is_none());
    }
}

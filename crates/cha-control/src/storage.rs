//! App data (Phase 3, persistent environments): what each app keeps between
//! launches and shares across users, who decides, and the API for it.
//!
//! - **Persistence** is per (user, app), on or off: a user who keeps their data
//!   for an app gets their directory on the node (`users/<user>/<app>`)
//!   as the app's home; otherwise the home goes with the environment. The
//!   user's own choice wins; until they make one, the app's default applies.
//! - **The app's default** and **shared access** (`none`, `read`, `write`:
//!   whether a directory shared by all users of the app is mounted, and how)
//!   are the admin's, per app. Until an admin sets one, the catalog's applies
//!   (`images/catalog.json`: Steam and KDE keep data, Steam also shares a
//!   library, the others do neither).
//! - A user can **reset** an app's data: the nodes delete their directory.
//!
//! None of this changes an environment that is running: its mounts were made
//! when it started. The user's settings and reset therefore refuse (409
//! `live`) while an environment of the app is live; an admin's defaults apply
//! to the next launch.
//!
//! The layout on the node, and what the node accepts, is `cha_wire::Storage`.

use std::collections::HashMap;
use std::time::Duration;

use axum::extract::{Path, State};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use cha_wire::{
    DEFAULT_DATA_ROOT, Inventory, NodeRequest, NodeResponse, Shared, SharedDirState,
    SharedDirStatus, Storage,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tracing::info;

use crate::AppState;
use crate::auth::{AdminUser, ClientInfo, CurrentUser, PlayerUser};
use crate::db::{self, AppStorageRow, NodeRow, Role, User};
use crate::environments::{self, Template};
#[cfg(test)]
use crate::environments::{catalog, template};
use crate::error::{ApiError, ApiResult};

/// Deleting a big home (a Steam library) takes a while.
const RESET_TIMEOUT: Duration = Duration::from_secs(600);

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/storage", get(list))
        .route("/storage/{template}", put(set))
        .route("/storage/{template}/reset", post(reset))
        .route("/admin/storage", get(admin_list))
        .route("/admin/storage/{template}", put(admin_set))
}

/// What an app shares across users.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SharedAccess {
    /// Not mounted.
    #[default]
    None,
    /// Mounted read-only.
    Read,
    /// Mounted read-write.
    Write,
}

impl SharedAccess {
    fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Read => "read",
            Self::Write => "write",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            "none" => Some(Self::None),
            "read" => Some(Self::Read),
            "write" => Some(Self::Write),
            _ => None,
        }
    }
}

/// What the catalog says an app shares, until an admin sets otherwise.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SharedDefaults {
    pub access: SharedAccess,
    /// Paths in the shared directory that every user keeps their own copy of
    /// (`cha_wire::Shared::per_user`): Steam's Proton prefixes and shader
    /// caches, which break when users share them. Whatever the access is.
    #[serde(default)]
    pub per_user: Vec<String>,
}

/// An app's settings from the admin (or the catalog, where they haven't).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AppSettings {
    /// Whether users who haven't chosen keep their data.
    pub default_persistent: bool,
    pub shared_access: SharedAccess,
}

pub fn app_settings(template: &Template, row: Option<&AppStorageRow>) -> AppSettings {
    let catalog_access = template
        .shared
        .as_ref()
        .map_or(SharedAccess::None, |s| s.access);
    AppSettings {
        default_persistent: row
            .and_then(|r| r.default_persistent)
            .unwrap_or(template.persistent),
        shared_access: row
            .and_then(|r| r.shared_access.as_deref())
            .and_then(SharedAccess::parse)
            .unwrap_or(catalog_access),
    }
}

/// What a launch of an app by a user gets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Effective {
    pub persistent: bool,
    pub shared_access: SharedAccess,
}

pub fn effective(app: AppSettings, user_choice: Option<bool>) -> Effective {
    Effective {
        persistent: user_choice.unwrap_or(app.default_persistent),
        shared_access: app.shared_access,
    }
}

/// The settings a launch of `template` by `user_id` runs with. A custom
/// environment that shares its base's data follows the base's settings.
pub async fn effective_for(
    state: &AppState,
    user_id: &str,
    template: &Template,
) -> ApiResult<Effective> {
    let rows = db::app_storage(&state.db).await?;
    let row = rows.iter().find(|r| r.template_id == template.data_id());
    let choice = db::user_storage(&state.db, user_id)
        .await?
        .get(template.data_id())
        .copied();
    Ok(effective(app_settings(template, row), choice))
}

/// What a node is told to mount for a launch: `None` when the app keeps and
/// shares nothing. `owner` is the user's id.
///
/// An app with parts of its shared directory that mustn't be shared (Steam's
/// Proton prefixes) gets the directory only when the user keeps their data:
/// their own copies of those parts live in their app directory. For a user who
/// keeps nothing there is nowhere to put them, and sharing the prefixes
/// instead would mix users. (An anonymous volume would do for the copies, but
/// Docker makes those root-owned, which an app running as uid 1000 can't use.)
pub fn spec_storage(owner: &str, template: &Template, effective: Effective) -> Option<Storage> {
    // A custom environment that shares its base's data uses the base's
    // directories, and tells the node whose they are.
    let data = template.data_id();
    let home = effective
        .persistent
        .then(|| cha_wire::user_dir(owner, data));
    let per_user = template
        .shared
        .as_ref()
        .map(|s| s.per_user.clone())
        .unwrap_or_default();
    let shared = (effective.shared_access != SharedAccess::None
        && (effective.persistent || per_user.is_empty()))
    .then(|| Shared {
        path: cha_wire::shared_dir(data),
        writable: effective.shared_access == SharedAccess::Write,
        per_user,
    });
    if home.is_none() && shared.is_none() {
        return None;
    }
    Some(Storage {
        // The node copies the user's old home volume in, if there is one and
        // it hasn't been taken (data used to live in volumes).
        legacy_volume: home
            .is_some()
            .then(|| cha_wire::home_volume_name(owner, data)),
        home,
        shared,
        data_template: (data != template.id).then(|| data.to_string()),
    })
}

pub(crate) fn node_inventory(node: &NodeRow) -> Option<Inventory> {
    node.inventory
        .as_deref()
        .and_then(|j| serde_json::from_str::<Inventory>(j).ok())
}

/// Whether the node reports a data root: its agent knows storage, so what the
/// portal sends is honoured and not silently dropped.
pub fn node_has_storage(node: &NodeRow) -> bool {
    node_inventory(node).is_some_and(|inv| inv.data_root.is_some())
}

/// What the nodes say about where they keep app data.
struct NodeStorage {
    /// The data root shown to users and admins: the one the first connected
    /// node reports (placement picks the first capable node, so nodes agree in
    /// practice: one root per node is the owner's to keep alike), else one a
    /// node last reported, else the default a node would use.
    root: String,
    /// Template id → where a node keeps its shared directory, for the
    /// templates whose directory its owner keeps outside the data root: from
    /// connected nodes first, else from ones that last said so. A template
    /// with none here keeps it under the data root.
    shared_paths: HashMap<String, String>,
    /// Every node that keeps app data, online or not, for the admin's
    /// per-node view of where each app's shared directory is.
    nodes: Vec<NodeShared>,
}

/// A node that keeps app data and what it says about its shared directories.
struct NodeShared {
    id: String,
    name: String,
    online: bool,
    data_root: String,
    /// Template (data) id → the external path.
    dirs: std::collections::BTreeMap<String, String>,
    status: std::collections::BTreeMap<String, SharedDirStatus>,
}

async fn node_storage(state: &AppState) -> ApiResult<NodeStorage> {
    let mut online_root = None;
    let mut offline_root = None;
    let mut online = HashMap::new();
    let mut offline = HashMap::new();
    let mut per_node = Vec::new();
    for node in db::list_nodes(&state.db).await? {
        let Some(inventory) = node_inventory(&node) else {
            continue;
        };
        let connected = state.nodes.connected_since(&node.id).await.is_some();
        let (root, paths) = if connected {
            (&mut online_root, &mut online)
        } else {
            (&mut offline_root, &mut offline)
        };
        if let Some(data_root) = &inventory.data_root {
            root.get_or_insert(data_root.clone());
            per_node.push(NodeShared {
                id: node.id.clone(),
                name: node.name.clone(),
                online: connected,
                data_root: data_root.clone(),
                dirs: inventory.shared_dirs.clone(),
                status: inventory.shared_status.clone(),
            });
        }
        for (template, path) in inventory.shared_dirs {
            paths.entry(template).or_insert(path);
        }
    }
    per_node.sort_by(|a, b| (&a.name, &a.id).cmp(&(&b.name, &b.id)));
    for (template, path) in offline {
        online.entry(template).or_insert(path);
    }
    Ok(NodeStorage {
        root: online_root
            .or(offline_root)
            .unwrap_or_else(|| DEFAULT_DATA_ROOT.to_string()),
        shared_paths: online,
        nodes: per_node,
    })
}

// ---- A user's apps ----

#[derive(Serialize)]
struct UserStorage {
    root: String,
    apps: Vec<UserApp>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct UserApp {
    template: String,
    name: String,
    /// The user's effective setting: their choice, else the app's default.
    persistent: bool,
    /// The app's default, for those who haven't chosen.
    default: bool,
    shared_access: SharedAccess,
    /// The user has a live environment of the app.
    live: bool,
    /// Where the node keeps what the app shares, when its owner keeps that
    /// outside the data root (a NAS share); absent when it is under the root.
    #[serde(skip_serializing_if = "Option::is_none")]
    shared_path: Option<String>,
}

fn user_app(
    template: &Template,
    rows: &[AppStorageRow],
    choices: &HashMap<String, bool>,
    live: &[String],
    shared_paths: &HashMap<String, String>,
) -> UserApp {
    let data = template.data_id();
    let app = app_settings(template, rows.iter().find(|r| r.template_id == data));
    let eff = effective(app, choices.get(data).copied());
    UserApp {
        template: template.id.clone(),
        name: template.name.clone(),
        persistent: eff.persistent,
        default: app.default_persistent,
        shared_access: eff.shared_access,
        live: live.iter().any(|l| l == data),
        shared_path: shared_paths.get(data).cloned(),
    }
}

/// `GET /api/storage`: the signed-in user's setting for every app they can
/// launch (guests can't launch any).
async fn list(
    State(state): State<AppState>,
    PlayerUser(user): PlayerUser,
) -> ApiResult<Json<UserStorage>> {
    let nodes = node_storage(&state).await?;
    if user.role == Role::Guest {
        return Ok(Json(UserStorage {
            root: nodes.root,
            apps: Vec::new(),
        }));
    }
    let rows = db::app_storage(&state.db).await?;
    let choices = db::user_storage(&state.db, &user.id).await?;
    let live = live_data_ids(&state, &user.id).await?;
    Ok(Json(UserStorage {
        root: nodes.root,
        apps: environments::all(&state)
            .iter()
            .map(|t| user_app(t, &rows, &choices, &live, &nodes.shared_paths))
            .collect(),
    }))
}

/// The app, for a user who may keep data for it: not a guest, and in the catalog.
fn launchable(state: &AppState, user: &User, id: &str) -> ApiResult<Template> {
    if user.role == Role::Guest {
        return Err(ApiError::forbidden(
            "guests_cannot_launch",
            "guests can't launch environments, so they keep no data",
        ));
    }
    environments::find(state, id).ok_or_else(|| ApiError::NotFound("no such template".into()))
}

/// The data ids (`Template::data_id`) of the user's live environments.
async fn live_data_ids(state: &AppState, user_id: &str) -> ApiResult<Vec<String>> {
    Ok(db::live_templates(&state.db, user_id)
        .await?
        .into_iter()
        .map(|id| match environments::find_any(state, &id) {
            Some(t) => t.data_id().to_string(),
            None => id,
        })
        .collect())
}

/// The user's live environment (starting, running or stopping) that uses
/// `data_id`'s app data: of that template, or of a custom one sharing it.
pub async fn live_on_data(
    state: &AppState,
    user_id: &str,
    data_id: &str,
) -> ApiResult<Option<db::EnvironmentRow>> {
    for id in db::live_templates(&state.db, user_id).await? {
        let data = match environments::find_any(state, &id) {
            Some(t) => t.data_id().to_string(),
            None => id.clone(),
        };
        if data == data_id
            && let Some(live) = db::live_environments_of(&state.db, user_id, &id)
                .await?
                .into_iter()
                .next()
        {
            return Ok(Some(live));
        }
    }
    Ok(None)
}

fn live_error() -> ApiError {
    ApiError::conflict(
        "live",
        "an environment of this app is running; stop it first",
    )
}

async fn show(state: &AppState, user: &User, template: &Template) -> ApiResult<UserApp> {
    let rows = db::app_storage(&state.db).await?;
    let choices = db::user_storage(&state.db, &user.id).await?;
    let live = live_data_ids(state, &user.id).await?;
    let nodes = node_storage(state).await?;
    Ok(user_app(
        template,
        &rows,
        &choices,
        &live,
        &nodes.shared_paths,
    ))
}

#[derive(Deserialize)]
struct SetPersistent {
    persistent: bool,
}

/// `PUT /api/storage/{template}`: keep (or stop keeping) the user's data for
/// the app. Not while an environment of it is live: its mounts are made.
async fn set(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    client: ClientInfo,
    Path(id): Path<String>,
    Json(req): Json<SetPersistent>,
) -> ApiResult<Json<UserApp>> {
    let template = launchable(&state, &user, &id)?;
    if live_on_data(&state, &user.id, template.data_id())
        .await?
        .is_some()
    {
        return Err(live_error());
    }
    db::set_user_persistent(&state.db, &user.id, template.data_id(), req.persistent).await?;
    db::audit(
        &state.db,
        Some(&user.id),
        "storage.persistence_set",
        Some(&template.id),
        Some(json!({ "persistent": req.persistent })),
        client.ip.as_deref(),
    )
    .await?;
    Ok(Json(show(&state, &user, &template).await?))
}

/// `POST /api/storage/{template}/reset`: deletes the user's data for the app on
/// every node that is connected (a user's data is on the node that ran their
/// app; today that is the only node, and placement keeps them on one while
/// there is one). A node that is offline isn't reached: reset again once it
/// is back. Any node's failure is a 502 with its message; the nodes before it
/// have already deleted theirs, and deleting is safe to repeat.
async fn reset(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    client: ClientInfo,
    Path(id): Path<String>,
) -> ApiResult<Json<UserApp>> {
    let template = launchable(&state, &user, &id)?;
    if live_on_data(&state, &user.id, template.data_id())
        .await?
        .is_some()
    {
        return Err(live_error());
    }
    let mut reached = Vec::new();
    for node in db::list_nodes(&state.db).await? {
        if state.nodes.connected_since(&node.id).await.is_none() {
            continue;
        }
        let reply = state
            .nodes
            .request_timeout(
                &node.id,
                NodeRequest::DeleteUserData {
                    user: user.id.clone(),
                    template: template.data_id().to_string(),
                },
                RESET_TIMEOUT,
            )
            .await
            .map_err(ApiError::node_failure)?;
        let NodeResponse::UserDataDeleted { .. } = reply else {
            return Err(ApiError::BadGateway(
                "node_error",
                format!("{} answered something else", node.name),
            ));
        };
        reached.push(node.name);
    }
    if reached.is_empty() {
        return Err(ApiError::BadGateway(
            "no_node",
            "no node is online to delete your data: try again when one is".into(),
        ));
    }
    info!(user = %user.username, template = %template.id, nodes = ?reached, "reset app data");
    db::audit(
        &state.db,
        Some(&user.id),
        "storage.reset",
        Some(&template.id),
        Some(json!({ "nodes": reached })),
        client.ip.as_deref(),
    )
    .await?;
    Ok(Json(show(&state, &user, &template).await?))
}

// ---- The admin's settings ----

#[derive(Serialize)]
struct AdminStorage {
    root: String,
    apps: Vec<AdminApp>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AdminApp {
    template: String,
    name: String,
    default_persistent: bool,
    shared_access: SharedAccess,
    /// As in [`UserApp`].
    #[serde(skip_serializing_if = "Option::is_none")]
    shared_path: Option<String>,
    /// Where each node that keeps app data holds the app's shared directory,
    /// by node name.
    nodes: Vec<AdminAppNode>,
}

#[derive(Serialize, PartialEq, Debug)]
#[serde(rename_all = "camelCase")]
struct AdminAppNode {
    node_id: String,
    node_name: String,
    online: bool,
    /// `external` (its owner keeps it outside the data root) or `local`.
    location: &'static str,
    /// The external path, or the directory under the node's data root.
    path: String,
    /// What the node's check of an external directory found; absent for a
    /// local one and from agents that report none.
    #[serde(skip_serializing_if = "Option::is_none")]
    state: Option<SharedDirState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    fs_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    checked_at: Option<i64>,
}

fn admin_app_nodes(data: &str, nodes: &[NodeShared]) -> Vec<AdminAppNode> {
    nodes
        .iter()
        .map(|n| {
            let status = n.status.get(data);
            let external = n.dirs.get(data);
            AdminAppNode {
                node_id: n.id.clone(),
                node_name: n.name.clone(),
                online: n.online,
                location: if external.is_some() {
                    "external"
                } else {
                    "local"
                },
                path: external.cloned().unwrap_or_else(|| {
                    format!(
                        "{}/{}",
                        n.data_root.trim_end_matches('/'),
                        cha_wire::shared_dir(data)
                    )
                }),
                state: status.filter(|_| external.is_some()).map(|s| s.state),
                fs_type: status
                    .filter(|_| external.is_some())
                    .and_then(|s| s.fs_type.clone()),
                source: status
                    .filter(|_| external.is_some())
                    .and_then(|s| s.source.clone()),
                detail: status
                    .filter(|_| external.is_some())
                    .and_then(|s| s.detail.clone()),
                checked_at: status.filter(|_| external.is_some()).map(|s| s.checked_at),
            }
        })
        .collect()
}

fn admin_app(template: &Template, rows: &[AppStorageRow], nodes: &NodeStorage) -> AdminApp {
    let data = template.data_id();
    let app = app_settings(template, rows.iter().find(|r| r.template_id == data));
    AdminApp {
        template: template.id.clone(),
        name: template.name.clone(),
        default_persistent: app.default_persistent,
        shared_access: app.shared_access,
        shared_path: nodes.shared_paths.get(data).cloned(),
        nodes: admin_app_nodes(data, &nodes.nodes),
    }
}

/// `GET /api/admin/storage`.
async fn admin_list(State(state): State<AppState>, _: AdminUser) -> ApiResult<Json<AdminStorage>> {
    let rows = db::app_storage(&state.db).await?;
    let nodes = node_storage(&state).await?;
    let apps = environments::all(&state)
        .iter()
        .map(|t| admin_app(t, &rows, &nodes))
        .collect();
    Ok(Json(AdminStorage {
        root: nodes.root,
        apps,
    }))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AdminUpdate {
    default_persistent: Option<bool>,
    shared_access: Option<SharedAccess>,
}

/// `PUT /api/admin/storage/{template}`: sets what is in the body; the rest
/// stays. Applies to launches from now on.
async fn admin_set(
    State(state): State<AppState>,
    AdminUser(admin): AdminUser,
    client: ClientInfo,
    Path(id): Path<String>,
    Json(req): Json<AdminUpdate>,
) -> ApiResult<Json<AdminApp>> {
    let template = environments::find(&state, &id)
        .ok_or_else(|| ApiError::NotFound("no such template".into()))?;
    let template = &template;
    if req.default_persistent.is_none() && req.shared_access.is_none() {
        return Err(ApiError::bad_request(
            "nothing_to_set",
            "send defaultPersistent, sharedAccess or both",
        ));
    }
    // What was asked, as it is audited: only the fields sent.
    let mut changes = serde_json::Map::new();
    if let Some(persistent) = req.default_persistent {
        changes.insert("defaultPersistent".into(), json!(persistent));
    }
    if let Some(access) = req.shared_access {
        changes.insert("sharedAccess".into(), json!(access));
    }
    let changes = serde_json::Value::Object(changes);
    db::set_app_storage(
        &state.db,
        template.data_id(),
        req.default_persistent,
        req.shared_access.map(SharedAccess::as_str),
    )
    .await?;
    db::audit(
        &state.db,
        Some(&admin.id),
        "storage.defaults_set",
        Some(&template.id),
        Some(changes),
        client.ip.as_deref(),
    )
    .await?;
    let rows = db::app_storage(&state.db).await?;
    let nodes = node_storage(&state).await?;
    Ok(Json(admin_app(template, &rows, &nodes)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const USER: &str = "01a10527-f79f-761b-962f-4b26924a2e68";

    fn row(persistent: Option<bool>, access: Option<&str>) -> AppStorageRow {
        AppStorageRow {
            template_id: "x".into(),
            default_persistent: persistent,
            shared_access: access.map(String::from),
        }
    }

    #[test]
    fn the_catalog_sets_what_nobody_has() {
        let steam = template("steam").unwrap();
        let chrome = template("chrome").unwrap();
        assert_eq!(
            app_settings(steam, None),
            AppSettings {
                default_persistent: true,
                shared_access: SharedAccess::Write
            }
        );
        assert_eq!(
            app_settings(chrome, None),
            AppSettings {
                default_persistent: false,
                shared_access: SharedAccess::None
            }
        );
        // A desktop keeps its home, and shares nothing.
        assert_eq!(
            app_settings(template("kde").unwrap(), None),
            AppSettings {
                default_persistent: true,
                shared_access: SharedAccess::None
            }
        );
        // An admin's settings win, each on its own.
        let only_access = row(None, Some("read"));
        assert_eq!(
            app_settings(steam, Some(&only_access)),
            AppSettings {
                default_persistent: true,
                shared_access: SharedAccess::Read
            }
        );
        let both = row(Some(true), Some("write"));
        assert_eq!(
            app_settings(chrome, Some(&both)),
            AppSettings {
                default_persistent: true,
                shared_access: SharedAccess::Write
            }
        );
        let off = row(Some(false), Some("none"));
        assert_eq!(
            app_settings(steam, Some(&off)),
            AppSettings {
                default_persistent: false,
                shared_access: SharedAccess::None
            }
        );
    }

    #[test]
    fn a_users_choice_wins_over_the_default() {
        let app = AppSettings {
            default_persistent: true,
            shared_access: SharedAccess::Read,
        };
        assert!(effective(app, None).persistent);
        assert!(!effective(app, Some(false)).persistent);
        let app = AppSettings {
            default_persistent: false,
            ..app
        };
        assert!(!effective(app, None).persistent);
        assert!(effective(app, Some(true)).persistent);
        // Sharing is the admin's alone.
        assert_eq!(effective(app, Some(true)).shared_access, SharedAccess::Read);
    }

    fn eff(persistent: bool, shared_access: SharedAccess) -> Effective {
        Effective {
            persistent,
            shared_access,
        }
    }

    #[test]
    fn the_node_is_told_what_to_mount_for_each_combination() {
        let steam = template("steam").unwrap();
        let chrome = template("chrome").unwrap();
        // Nothing kept, nothing shared: nothing sent.
        assert_eq!(
            spec_storage(USER, chrome, eff(false, SharedAccess::None)),
            None
        );

        // Kept, not shared.
        let kept = spec_storage(USER, chrome, eff(true, SharedAccess::None)).unwrap();
        assert_eq!(
            kept.home.as_deref(),
            Some("users/01a10527-f79f-761b-962f-4b26924a2e68/chrome")
        );
        assert_eq!(kept.shared, None);
        assert_eq!(
            kept.legacy_volume.as_deref(),
            Some("cha-home-01a10527-f79f-761b-962f-4b26924a2e68-chrome")
        );

        // Shared, not kept: no home, so no volume to copy in. (Nothing
        // per-user in chrome's: for Steam see below.)
        for (access, writable) in [(SharedAccess::Read, false), (SharedAccess::Write, true)] {
            let shared_only = spec_storage(USER, chrome, eff(false, access)).unwrap();
            assert_eq!(shared_only.home, None);
            assert_eq!(shared_only.legacy_volume, None);
            let shared = shared_only.shared.unwrap();
            assert_eq!(shared.path, "shared/chrome");
            assert_eq!(shared.writable, writable);
            assert!(shared.per_user.is_empty(), "chrome has none");
        }

        // Both, for an app with parts that mustn't be shared.
        let both = spec_storage(USER, steam, eff(true, SharedAccess::Write)).unwrap();
        assert!(both.home.is_some() && both.legacy_volume.is_some());
        let shared = both.shared.unwrap();
        assert_eq!(shared.path, "shared/steam");
        assert!(shared.writable);
        assert_eq!(
            shared.per_user,
            ["steamapps/compatdata", "steamapps/shadercache"]
        );
        // A user who keeps nothing has nowhere to keep their own copies of
        // those parts, so they aren't given the shared directory at all (and
        // so nothing at all).
        for access in [SharedAccess::Read, SharedAccess::Write] {
            assert_eq!(spec_storage(USER, steam, eff(false, access)), None);
        }
        let read = spec_storage(USER, steam, eff(true, SharedAccess::Read)).unwrap();
        assert!(!read.shared.unwrap().writable);
    }

    #[test]
    fn kde_gets_a_home_and_no_shared_directory() {
        let kde = template("kde").unwrap();
        let storage = spec_storage(USER, kde, eff(true, SharedAccess::None)).unwrap();
        assert_eq!(storage.home, Some(cha_wire::user_dir(USER, "kde")));
        assert_eq!(storage.shared, None);
        // Off, nothing is mounted: the home goes with the container.
        assert_eq!(
            spec_storage(USER, kde, eff(false, SharedAccess::None)),
            None
        );
    }

    #[test]
    fn everything_sent_is_what_a_node_accepts() {
        for template in catalog() {
            for persistent in [false, true] {
                for access in [SharedAccess::None, SharedAccess::Read, SharedAccess::Write] {
                    if let Some(storage) = spec_storage(USER, template, eff(persistent, access)) {
                        assert_eq!(
                            storage.check(USER, &template.id),
                            Ok(()),
                            "{} {persistent} {access:?}",
                            template.id
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn the_catalogs_shared_defaults_are_sane() {
        for template in catalog() {
            if let Some(shared) = &template.shared {
                for path in &shared.per_user {
                    assert!(
                        cha_wire::valid_relative_path(path),
                        "{}: {path}",
                        template.id
                    );
                }
            }
        }
        assert!(template("steam").unwrap().persistent);
        assert!(template("kde").unwrap().persistent);
        assert!(!template("chrome").unwrap().persistent);
    }

    #[test]
    fn access_levels_have_stable_names() {
        for (access, name) in [
            (SharedAccess::None, "none"),
            (SharedAccess::Read, "read"),
            (SharedAccess::Write, "write"),
        ] {
            assert_eq!(access.as_str(), name);
            assert_eq!(SharedAccess::parse(name), Some(access));
            assert_eq!(serde_json::to_value(access).unwrap(), name);
        }
        assert_eq!(SharedAccess::parse("rw"), None);
        assert!(serde_json::from_value::<SharedAccess>(json!("rw")).is_err());
    }
}

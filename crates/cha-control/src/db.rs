//! SQLite storage: the pool, migrations, and the queries the API needs.

use std::path::Path;
use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::Serialize;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{FromRow, SqlitePool};

/// Opens (creating if needed) the database at `path` and applies migrations.
pub async fn open(path: &Path) -> Result<SqlitePool> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let options = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))?
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(8)
        .connect_with(options)
        .await
        .with_context(|| format!("opening {}", path.display()))?;
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .context("migrating the database")?;
    Ok(pool)
}

/// Unix seconds.
pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

pub fn new_id() -> String {
    uuid::Uuid::now_v7().to_string()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, serde::Deserialize, sqlx::Type)]
#[serde(rename_all = "lowercase")]
#[sqlx(type_name = "TEXT", rename_all = "lowercase")]
pub enum Role {
    Admin,
    User,
    Guest,
}

#[derive(Clone, Debug, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub struct User {
    pub id: String,
    pub username: String,
    pub display_name: String,
    pub role: Role,
    pub disabled: bool,
    pub created_at: i64,
}

#[derive(FromRow)]
pub struct UserWithHash {
    #[sqlx(flatten)]
    pub user: User,
    pub password_hash: Option<String>,
}

const USER_COLUMNS: &str = "id, username, display_name, role, disabled, created_at";

pub async fn user_count(db: &SqlitePool) -> Result<i64> {
    Ok(sqlx::query_scalar("SELECT COUNT(*) FROM users")
        .fetch_one(db)
        .await?)
}

pub async fn insert_user(
    db: &SqlitePool,
    username: &str,
    display_name: &str,
    password_hash: &str,
    role: Role,
) -> Result<User, sqlx::Error> {
    let id = new_id();
    sqlx::query(
        "INSERT INTO users (id, username, display_name, password_hash, role, created_at) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(username)
    .bind(display_name)
    .bind(password_hash)
    .bind(role)
    .bind(now())
    .execute(db)
    .await?;
    user_by_id(db, &id).await?.ok_or(sqlx::Error::RowNotFound)
}

pub async fn user_by_id(db: &SqlitePool, id: &str) -> Result<Option<User>, sqlx::Error> {
    sqlx::query_as(&format!("SELECT {USER_COLUMNS} FROM users WHERE id = ?"))
        .bind(id)
        .fetch_optional(db)
        .await
}

pub async fn user_for_login(
    db: &SqlitePool,
    username: &str,
) -> Result<Option<UserWithHash>, sqlx::Error> {
    sqlx::query_as(&format!(
        "SELECT {USER_COLUMNS}, password_hash FROM users WHERE username = ?"
    ))
    .bind(username)
    .fetch_optional(db)
    .await
}

pub async fn list_users(db: &SqlitePool) -> Result<Vec<User>, sqlx::Error> {
    sqlx::query_as(&format!(
        "SELECT {USER_COLUMNS} FROM users ORDER BY created_at"
    ))
    .fetch_all(db)
    .await
}

pub async fn insert_session(
    db: &SqlitePool,
    token_hash: &str,
    user_id: &str,
    expires_at: i64,
    user_agent: Option<&str>,
    ip: Option<&str>,
) -> Result<(), sqlx::Error> {
    let now = now();
    sqlx::query(
        "INSERT INTO sessions (token_hash, user_id, created_at, expires_at, last_seen_at, user_agent, ip) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(token_hash)
    .bind(user_id)
    .bind(now)
    .bind(expires_at)
    .bind(now)
    .bind(user_agent)
    .bind(ip)
    .execute(db)
    .await?;
    Ok(())
}

/// The user behind a live session, if the session exists, hasn't expired and the
/// account isn't disabled. Touches `last_seen_at`.
pub async fn session_user(db: &SqlitePool, token_hash: &str) -> Result<Option<User>, sqlx::Error> {
    let now = now();
    let user: Option<User> = sqlx::query_as(
        "SELECT u.id, u.username, u.display_name, u.role, u.disabled, u.created_at \
         FROM sessions s JOIN users u ON u.id = s.user_id \
         WHERE s.token_hash = ? AND s.expires_at > ? AND u.disabled = 0",
    )
    .bind(token_hash)
    .bind(now)
    .fetch_optional(db)
    .await?;
    if user.is_some() {
        sqlx::query("UPDATE sessions SET last_seen_at = ? WHERE token_hash = ?")
            .bind(now)
            .bind(token_hash)
            .execute(db)
            .await?;
    }
    Ok(user)
}

pub async fn delete_session(db: &SqlitePool, token_hash: &str) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM sessions WHERE token_hash = ?")
        .bind(token_hash)
        .execute(db)
        .await?;
    Ok(())
}

pub async fn delete_expired_sessions(db: &SqlitePool) -> Result<u64, sqlx::Error> {
    Ok(sqlx::query("DELETE FROM sessions WHERE expires_at <= ?")
        .bind(now())
        .execute(db)
        .await?
        .rows_affected())
}

#[derive(Debug, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub struct AuditEntry {
    pub id: i64,
    pub at: i64,
    pub actor_id: Option<String>,
    pub action: String,
    pub target: Option<String>,
    pub detail: Option<String>,
    pub ip: Option<String>,
}

pub async fn audit(
    db: &SqlitePool,
    actor_id: Option<&str>,
    action: &str,
    target: Option<&str>,
    detail: Option<serde_json::Value>,
    ip: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO audit_log (at, actor_id, action, target, detail, ip) VALUES (?, ?, ?, ?, ?, ?)")
        .bind(now())
        .bind(actor_id)
        .bind(action)
        .bind(target)
        .bind(detail.map(|d| d.to_string()))
        .bind(ip)
        .execute(db)
        .await?;
    Ok(())
}

pub async fn recent_audit(db: &SqlitePool, limit: i64) -> Result<Vec<AuditEntry>, sqlx::Error> {
    sqlx::query_as("SELECT id, at, actor_id, action, target, detail, ip FROM audit_log ORDER BY id DESC LIMIT ?")
        .bind(limit)
        .fetch_all(db)
        .await
}

// ---- Nodes and join tokens (P1.2) ----

pub const JOIN_TOKEN_TTL_SECS: i64 = 3600;

pub async fn insert_join_token(
    db: &SqlitePool,
    token_hash: &str,
    label: Option<&str>,
    created_by: &str,
) -> Result<i64, sqlx::Error> {
    let now = now();
    let expires_at = now + JOIN_TOKEN_TTL_SECS;
    sqlx::query(
        "INSERT INTO join_tokens (token_hash, label, created_by, created_at, expires_at) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(token_hash)
    .bind(label)
    .bind(created_by)
    .bind(now)
    .bind(expires_at)
    .execute(db)
    .await?;
    Ok(expires_at)
}

/// Uses up a join token and records the node it enrolled, atomically. `false`
/// if the token is unknown, expired or already used.
pub async fn redeem_join_token(
    db: &SqlitePool,
    token_hash: &str,
    node_id: &str,
    name: &str,
    public_key: &str,
    agent_version: &str,
) -> Result<bool, sqlx::Error> {
    let now = now();
    let mut tx = db.begin().await?;
    let used = sqlx::query(
        "UPDATE join_tokens SET used_at = ?, node_id = ? \
         WHERE token_hash = ? AND used_at IS NULL AND expires_at > ?",
    )
    .bind(now)
    .bind(node_id)
    .bind(token_hash)
    .bind(now)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if used == 0 {
        return Ok(false);
    }
    sqlx::query(
        "INSERT INTO nodes (id, name, public_key, agent_version, enrolled_at) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(node_id)
    .bind(name)
    .bind(public_key)
    .bind(agent_version)
    .bind(now)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(true)
}

#[derive(Clone, Debug, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub struct NodeRow {
    pub id: String,
    pub name: String,
    #[serde(skip)]
    pub public_key: String,
    pub agent_version: Option<String>,
    pub enrolled_at: i64,
    pub last_seen_at: Option<i64>,
    #[serde(skip)]
    pub inventory: Option<String>,
}

const NODE_COLUMNS: &str =
    "id, name, public_key, agent_version, enrolled_at, last_seen_at, inventory";

pub async fn node_by_id(db: &SqlitePool, id: &str) -> Result<Option<NodeRow>, sqlx::Error> {
    sqlx::query_as(&format!("SELECT {NODE_COLUMNS} FROM nodes WHERE id = ?"))
        .bind(id)
        .fetch_optional(db)
        .await
}

pub async fn list_nodes(db: &SqlitePool) -> Result<Vec<NodeRow>, sqlx::Error> {
    sqlx::query_as(&format!(
        "SELECT {NODE_COLUMNS} FROM nodes ORDER BY enrolled_at"
    ))
    .fetch_all(db)
    .await
}

pub async fn touch_node(
    db: &SqlitePool,
    id: &str,
    agent_version: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE nodes SET last_seen_at = ?, agent_version = COALESCE(?, agent_version) WHERE id = ?")
        .bind(now())
        .bind(agent_version)
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}

pub async fn set_node_inventory(
    db: &SqlitePool,
    id: &str,
    inventory_json: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE nodes SET inventory = ?, last_seen_at = ? WHERE id = ?")
        .bind(inventory_json)
        .bind(now())
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}

pub async fn delete_node(db: &SqlitePool, id: &str) -> Result<bool, sqlx::Error> {
    Ok(sqlx::query("DELETE FROM nodes WHERE id = ?")
        .bind(id)
        .execute(db)
        .await?
        .rows_affected()
        > 0)
}

// ---- Environments ----

#[derive(Clone, Debug, FromRow)]
pub struct EnvironmentRow {
    pub id: String,
    pub owner_id: String,
    pub template_id: String,
    pub node_id: Option<String>,
    pub state: String,
    pub detail: Option<String>,
    /// Something the user should know about while it runs (from its node).
    pub warning: Option<String>,
    /// What its containers last logged when it died, as a JSON array of lines.
    pub log: Option<String>,
    pub http_port: Option<i64>,
    pub webrtc_port: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
}

const ENVIRONMENT_COLUMNS: &str = "id, owner_id, template_id, node_id, state, detail, warning, log, http_port, webrtc_port, created_at, updated_at";

pub async fn insert_environment(
    db: &SqlitePool,
    id: &str,
    owner_id: &str,
    template_id: &str,
    node_id: &str,
    state: &str,
) -> Result<(), sqlx::Error> {
    let now = now();
    sqlx::query(
        "INSERT INTO environments (id, owner_id, template_id, node_id, state, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(id)
    .bind(owner_id)
    .bind(template_id)
    .bind(node_id)
    .bind(state)
    .bind(now)
    .bind(now)
    .execute(db)
    .await?;
    Ok(())
}

pub async fn environment_by_id(
    db: &SqlitePool,
    id: &str,
) -> Result<Option<EnvironmentRow>, sqlx::Error> {
    sqlx::query_as(&format!(
        "SELECT {ENVIRONMENT_COLUMNS} FROM environments WHERE id = ?"
    ))
    .bind(id)
    .fetch_optional(db)
    .await
}

/// Live environments first, then the most recent ended ones; `owner` limits
/// the list to one user's.
pub async fn list_environments(
    db: &SqlitePool,
    owner: Option<&str>,
    limit: i64,
) -> Result<Vec<EnvironmentRow>, sqlx::Error> {
    sqlx::query_as(&format!(
        "SELECT {ENVIRONMENT_COLUMNS} FROM environments \
         WHERE (?1 IS NULL OR owner_id = ?1) \
         ORDER BY state IN ('destroyed', 'failed'), created_at DESC LIMIT ?2"
    ))
    .bind(owner)
    .bind(limit)
    .fetch_all(db)
    .await
}

/// Environments a node should be running (or starting, or stopping).
pub async fn node_environments(
    db: &SqlitePool,
    node_id: &str,
) -> Result<Vec<EnvironmentRow>, sqlx::Error> {
    sqlx::query_as(&format!(
        "SELECT {ENVIRONMENT_COLUMNS} FROM environments \
         WHERE node_id = ? AND state IN ('starting', 'running', 'stopping')"
    ))
    .bind(node_id)
    .fetch_all(db)
    .await
}

pub async fn count_live_environments(db: &SqlitePool, owner_id: &str) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM environments WHERE owner_id = ? AND state IN ('starting', 'running', 'stopping')",
    )
    .bind(owner_id)
    .fetch_one(db)
    .await
}

/// The user's live environment of `template_id`, if any.
pub async fn live_environment_of(
    db: &SqlitePool,
    owner_id: &str,
    template_id: &str,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT id FROM environments WHERE owner_id = ? AND template_id = ? \
         AND state IN ('starting', 'running', 'stopping') LIMIT 1",
    )
    .bind(owner_id)
    .bind(template_id)
    .fetch_optional(db)
    .await
}

/// Templates the user has a live environment of.
pub async fn live_templates(db: &SqlitePool, owner_id: &str) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT DISTINCT template_id FROM environments WHERE owner_id = ? \
         AND state IN ('starting', 'running', 'stopping')",
    )
    .bind(owner_id)
    .fetch_all(db)
    .await
}

/// What a node says a starting environment is doing, as its detail. Only the
/// environment's own node may, and only while it is starting.
pub async fn set_environment_progress(
    db: &SqlitePool,
    id: &str,
    node_id: &str,
    detail: &str,
) -> Result<bool, sqlx::Error> {
    Ok(sqlx::query(
        "UPDATE environments SET detail = ?, updated_at = ? \
         WHERE id = ? AND node_id = ? AND state = 'starting'",
    )
    .bind(detail)
    .bind(now())
    .bind(id)
    .bind(node_id)
    .execute(db)
    .await?
    .rows_affected()
        > 0)
}

/// What a node says is wrong with a running environment, or `None` when it
/// no longer is. Only the environment's own node may, and only while it runs.
pub async fn set_environment_warning(
    db: &SqlitePool,
    id: &str,
    node_id: &str,
    warning: Option<&str>,
) -> Result<bool, sqlx::Error> {
    Ok(sqlx::query(
        "UPDATE environments SET warning = ?, updated_at = ? \
         WHERE id = ? AND node_id = ? AND state = 'running'",
    )
    .bind(warning)
    .bind(now())
    .bind(id)
    .bind(node_id)
    .execute(db)
    .await?
    .rows_affected()
        > 0)
}

/// Moves an environment to `state`, but only from one of `from`; returns
/// whether it moved (so concurrent updates can't resurrect a stopped one).
pub async fn transition_environment(
    db: &SqlitePool,
    id: &str,
    from: &[&str],
    state: &str,
    detail: Option<&str>,
) -> Result<bool, sqlx::Error> {
    let placeholders = vec!["?"; from.len()].join(", ");
    let sql = format!(
        "UPDATE environments SET state = ?, detail = ?, warning = NULL, updated_at = ? \
         WHERE id = ? AND state IN ({placeholders})"
    );
    let mut query = sqlx::query(&sql)
        .bind(state)
        .bind(detail)
        .bind(now())
        .bind(id);
    for f in from {
        query = query.bind(*f);
    }
    Ok(query.execute(db).await?.rows_affected() > 0)
}

/// An environment that stopped on its own, as its node tells it: moves it to
/// `failed` or `destroyed` from starting or running, with what its containers
/// logged (a JSON array of lines). A failed start can reach the portal as the
/// node's error first and its exit (the better account, with the log) second,
/// so a failed environment with no log yet takes them too.
pub async fn record_exit(
    db: &SqlitePool,
    id: &str,
    failed: bool,
    detail: &str,
    log: Option<&str>,
) -> Result<bool, sqlx::Error> {
    let state = if failed { "failed" } else { "destroyed" };
    let moved = sqlx::query(
        "UPDATE environments SET state = ?, detail = ?, log = ?, warning = NULL, updated_at = ? \
         WHERE id = ? AND state IN ('starting', 'running')",
    )
    .bind(state)
    .bind(detail)
    .bind(log)
    .bind(now())
    .bind(id)
    .execute(db)
    .await?
    .rows_affected()
        > 0;
    if moved || !failed || log.is_none() {
        return Ok(moved);
    }
    Ok(sqlx::query(
        "UPDATE environments SET detail = ?, log = ?, updated_at = ? \
         WHERE id = ? AND state = 'failed' AND log IS NULL",
    )
    .bind(detail)
    .bind(log)
    .bind(now())
    .bind(id)
    .execute(db)
    .await?
    .rows_affected()
        > 0)
}

pub async fn set_environment_running(
    db: &SqlitePool,
    id: &str,
    http_port: u16,
    webrtc_port: u16,
) -> Result<bool, sqlx::Error> {
    Ok(sqlx::query(
        "UPDATE environments SET state = 'running', detail = NULL, http_port = ?, webrtc_port = ?, \
         updated_at = ? WHERE id = ? AND state = 'starting'",
    )
    .bind(i64::from(http_port))
    .bind(i64::from(webrtc_port))
    .bind(now())
    .bind(id)
    .execute(db)
    .await?
    .rows_affected()
        > 0)
}

/// Before a node is deleted: its live environments can't be reached any more.
pub async fn fail_node_environments(
    db: &SqlitePool,
    node_id: &str,
    detail: &str,
) -> Result<u64, sqlx::Error> {
    Ok(sqlx::query(
        "UPDATE environments SET state = 'failed', detail = ?, warning = NULL, updated_at = ? \
         WHERE node_id = ? AND state IN ('starting', 'running', 'stopping')",
    )
    .bind(detail)
    .bind(now())
    .bind(node_id)
    .execute(db)
    .await?
    .rows_affected())
}

// ---- App data settings (0004) ----

/// An admin's settings for one app; `None` where they haven't set it (the
/// catalog's value applies).
#[derive(Clone, Debug, FromRow)]
pub struct AppStorageRow {
    pub template_id: String,
    pub default_persistent: Option<bool>,
    pub shared_access: Option<String>,
}

pub async fn app_storage(db: &SqlitePool) -> Result<Vec<AppStorageRow>, sqlx::Error> {
    sqlx::query_as("SELECT template_id, default_persistent, shared_access FROM app_storage")
        .fetch_all(db)
        .await
}

/// Sets what is `Some`; what is `None` stays as it was.
pub async fn set_app_storage(
    db: &SqlitePool,
    template_id: &str,
    default_persistent: Option<bool>,
    shared_access: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO app_storage (template_id, default_persistent, shared_access) \
         VALUES (?1, ?2, ?3) \
         ON CONFLICT (template_id) DO UPDATE SET \
           default_persistent = COALESCE(?2, default_persistent), \
           shared_access = COALESCE(?3, shared_access)",
    )
    .bind(template_id)
    .bind(default_persistent)
    .bind(shared_access)
    .execute(db)
    .await?;
    Ok(())
}

/// The user's own choices: template id → whether they keep their data.
pub async fn user_storage(
    db: &SqlitePool,
    user_id: &str,
) -> Result<std::collections::HashMap<String, bool>, sqlx::Error> {
    let rows: Vec<(String, bool)> =
        sqlx::query_as("SELECT template_id, persistent FROM user_app_storage WHERE user_id = ?")
            .bind(user_id)
            .fetch_all(db)
            .await?;
    Ok(rows.into_iter().collect())
}

pub async fn set_user_persistent(
    db: &SqlitePool,
    user_id: &str,
    template_id: &str,
    persistent: bool,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO user_app_storage (user_id, template_id, persistent) VALUES (?1, ?2, ?3) \
         ON CONFLICT (user_id, template_id) DO UPDATE SET persistent = ?3",
    )
    .bind(user_id)
    .bind(template_id)
    .bind(persistent)
    .execute(db)
    .await?;
    Ok(())
}

/// The user's own controller choices: template id → a `cha_wire::GamepadKind`'s name.
pub async fn user_gamepads(
    db: &SqlitePool,
    user_id: &str,
) -> Result<std::collections::HashMap<String, String>, sqlx::Error> {
    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT template_id, kind FROM user_app_gamepad WHERE user_id = ?")
            .bind(user_id)
            .fetch_all(db)
            .await?;
    Ok(rows.into_iter().collect())
}

/// Sets the user's controller for an app, or clears it (`None`: the app's default).
pub async fn set_user_gamepad(
    db: &SqlitePool,
    user_id: &str,
    template_id: &str,
    kind: Option<&str>,
) -> Result<(), sqlx::Error> {
    match kind {
        Some(kind) => {
            sqlx::query(
                "INSERT INTO user_app_gamepad (user_id, template_id, kind) VALUES (?1, ?2, ?3) \
                 ON CONFLICT (user_id, template_id) DO UPDATE SET kind = ?3",
            )
            .bind(user_id)
            .bind(template_id)
            .bind(kind)
            .execute(db)
            .await?;
        }
        None => {
            sqlx::query("DELETE FROM user_app_gamepad WHERE user_id = ?1 AND template_id = ?2")
                .bind(user_id)
                .bind(template_id)
                .execute(db)
                .await?;
        }
    }
    Ok(())
}

/// The user's own frame rate choices: template id → fps.
pub async fn user_fps(
    db: &SqlitePool,
    user_id: &str,
) -> Result<std::collections::HashMap<String, u32>, sqlx::Error> {
    let rows: Vec<(String, u32)> =
        sqlx::query_as("SELECT template_id, fps FROM user_app_fps WHERE user_id = ?")
            .bind(user_id)
            .fetch_all(db)
            .await?;
    Ok(rows.into_iter().collect())
}

/// Sets the user's frame rate for an app, or clears it (`None`: the app's default).
pub async fn set_user_fps(
    db: &SqlitePool,
    user_id: &str,
    template_id: &str,
    fps: Option<u32>,
) -> Result<(), sqlx::Error> {
    match fps {
        Some(fps) => {
            sqlx::query(
                "INSERT INTO user_app_fps (user_id, template_id, fps) VALUES (?1, ?2, ?3) \
                 ON CONFLICT (user_id, template_id) DO UPDATE SET fps = ?3",
            )
            .bind(user_id)
            .bind(template_id)
            .bind(fps)
            .execute(db)
            .await?;
        }
        None => {
            sqlx::query("DELETE FROM user_app_fps WHERE user_id = ?1 AND template_id = ?2")
                .bind(user_id)
                .bind(template_id)
                .execute(db)
                .await?;
        }
    }
    Ok(())
}

// ---- Settings (the table is from 0001) ----

pub async fn setting(db: &SqlitePool, key: &str) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar("SELECT value FROM settings WHERE key = ?")
        .bind(key)
        .fetch_optional(db)
        .await
}

/// Stores `value` unless the key already has one; returns the value that won
/// (so two portals starting at once agree).
pub async fn setting_or_insert(
    db: &SqlitePool,
    key: &str,
    value: &str,
) -> Result<String, sqlx::Error> {
    sqlx::query("INSERT OR IGNORE INTO settings (key, value) VALUES (?, ?)")
        .bind(key)
        .bind(value)
        .execute(db)
        .await?;
    Ok(setting(db, key).await?.unwrap_or_else(|| value.to_string()))
}

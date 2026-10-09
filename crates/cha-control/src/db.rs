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
    pub email: Option<String>,
    /// Environments at once; `None` is the portal's default.
    pub max_instances: Option<i64>,
    pub node_restricted: bool,
}

#[derive(FromRow)]
pub struct UserWithHash {
    #[sqlx(flatten)]
    pub user: User,
    pub password_hash: Option<String>,
}

const USER_COLUMNS: &str =
    "id, username, display_name, role, disabled, created_at, email, max_instances, node_restricted";
/// [`USER_COLUMNS`] for a query that joins the users table as `u`.
const USER_COLUMNS_U: &str = "u.id, u.username, u.display_name, u.role, u.disabled, u.created_at, u.email, u.max_instances, u.node_restricted";

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
    insert_user_with_email(db, username, None, display_name, password_hash, role).await
}

/// [`insert_user`] with an email address (already lower-cased).
pub async fn insert_user_with_email(
    db: &SqlitePool,
    username: &str,
    email: Option<&str>,
    display_name: &str,
    password_hash: &str,
    role: Role,
) -> Result<User, sqlx::Error> {
    let id = new_id();
    sqlx::query(
        "INSERT INTO users (id, username, email, display_name, password_hash, role, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(username)
    .bind(email)
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

/// [`user_for_login`] by username or email, either case.
pub async fn user_for_login_or_email(
    db: &SqlitePool,
    name: &str,
) -> Result<Option<UserWithHash>, sqlx::Error> {
    sqlx::query_as(&format!(
        "SELECT {USER_COLUMNS}, password_hash FROM users \
         WHERE username = ?1 COLLATE NOCASE OR email = ?1 COLLATE NOCASE \
         ORDER BY username = ?1 COLLATE NOCASE DESC LIMIT 1"
    ))
    .bind(name)
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

/// Deletes a user; their sessions, devices, grants and the rest cascade.
pub async fn delete_user(db: &SqlitePool, id: &str) -> Result<bool, sqlx::Error> {
    Ok(sqlx::query("DELETE FROM users WHERE id = ?")
        .bind(id)
        .execute(db)
        .await?
        .rows_affected()
        > 0)
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

/// A signed-in session's user and, when an admin is viewing as them, that admin.
pub struct SessionActor {
    pub user: User,
    pub impersonator: Option<User>,
}

/// The user behind a live session, if the session exists, hasn't expired and the
/// account isn't disabled. Touches `last_seen_at`.
pub async fn session_user(db: &SqlitePool, token_hash: &str) -> Result<Option<User>, sqlx::Error> {
    Ok(session_actor(db, token_hash).await?.map(|a| a.user))
}

/// [`session_user`] with the admin behind a "switch user" session. The session
/// ends when either account is disabled.
pub async fn session_actor(
    db: &SqlitePool,
    token_hash: &str,
) -> Result<Option<SessionActor>, sqlx::Error> {
    let now = now();
    let user: Option<User> = sqlx::query_as(&format!(
        "SELECT {USER_COLUMNS_U} \
         FROM sessions s JOIN users u ON u.id = s.user_id \
         WHERE s.token_hash = ? AND s.expires_at > ? AND u.disabled = 0"
    ))
    .bind(token_hash)
    .bind(now)
    .fetch_optional(db)
    .await?;
    let Some(user) = user else {
        return Ok(None);
    };
    let impersonator_id: Option<String> =
        sqlx::query_scalar("SELECT impersonator_id FROM sessions WHERE token_hash = ?")
            .bind(token_hash)
            .fetch_one(db)
            .await?;
    let impersonator = match impersonator_id {
        Some(id) => match user_by_id(db, &id).await? {
            Some(admin) if !admin.disabled => Some(admin),
            _ => return Ok(None),
        },
        None => None,
    };
    sqlx::query("UPDATE sessions SET last_seen_at = ? WHERE token_hash = ?")
        .bind(now)
        .bind(token_hash)
        .execute(db)
        .await?;
    Ok(Some(SessionActor { user, impersonator }))
}

/// Starts a "switch user" session: `user_id`'s, on behalf of `impersonator_id`.
pub async fn insert_impersonation_session(
    db: &SqlitePool,
    token_hash: &str,
    user_id: &str,
    impersonator_id: &str,
    expires_at: i64,
    user_agent: Option<&str>,
    ip: Option<&str>,
) -> Result<(), sqlx::Error> {
    let now = now();
    sqlx::query(
        "INSERT INTO sessions (token_hash, user_id, created_at, expires_at, last_seen_at, user_agent, ip, impersonator_id) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(token_hash)
    .bind(user_id)
    .bind(now)
    .bind(expires_at)
    .bind(now)
    .bind(user_agent)
    .bind(ip)
    .bind(impersonator_id)
    .execute(db)
    .await?;
    Ok(())
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
    insert_node(&mut *tx, node_id, name, public_key, agent_version).await?;
    tx.commit().await?;
    Ok(true)
}

/// Records an enrolled node. A key that is already enrolled is a unique
/// violation.
pub async fn insert_node(
    executor: impl sqlx::Executor<'_, Database = sqlx::Sqlite>,
    node_id: &str,
    name: &str,
    public_key: &str,
    agent_version: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO nodes (id, name, public_key, agent_version, enrolled_at) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(node_id)
    .bind(name)
    .bind(public_key)
    .bind(agent_version)
    .bind(now())
    .execute(executor)
    .await?;
    Ok(())
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

pub async fn rename_node(db: &SqlitePool, id: &str, name: &str) -> Result<bool, sqlx::Error> {
    Ok(sqlx::query("UPDATE nodes SET name = ? WHERE id = ?")
        .bind(name)
        .bind(id)
        .execute(db)
        .await?
        .rows_affected()
        > 0)
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
    /// The id of the node's device it runs on; `None` is the NVIDIA GPU, from
    /// before devices.
    pub device: Option<String>,
    /// The host ports published for its host options, as a JSON array of
    /// `cha_wire::HostPort`; `None` without any.
    pub ports: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

const ENVIRONMENT_COLUMNS: &str = "id, owner_id, template_id, node_id, state, detail, warning, log, http_port, webrtc_port, device, ports, created_at, updated_at";

pub async fn insert_environment(
    db: &SqlitePool,
    id: &str,
    owner_id: &str,
    template_id: &str,
    node_id: &str,
    device: Option<&str>,
    state: &str,
) -> Result<(), sqlx::Error> {
    let now = now();
    sqlx::query(
        "INSERT INTO environments (id, owner_id, template_id, node_id, device, state, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(id)
    .bind(owner_id)
    .bind(template_id)
    .bind(node_id)
    .bind(device)
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

/// Every environment in state `running`, with no limit.
pub async fn running_environments(db: &SqlitePool) -> Result<Vec<EnvironmentRow>, sqlx::Error> {
    sqlx::query_as(&format!(
        "SELECT {ENVIRONMENT_COLUMNS} FROM environments WHERE state = 'running'"
    ))
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

/// Environments starting or running, per node and device (`None`: the
/// NVIDIA GPU, from before devices).
pub async fn running_by_device(
    db: &SqlitePool,
) -> Result<Vec<(String, Option<String>, i64)>, sqlx::Error> {
    sqlx::query_as(
        "SELECT node_id, device, COUNT(*) FROM environments \
         WHERE node_id IS NOT NULL AND state IN ('starting', 'running') \
         GROUP BY node_id, device",
    )
    .fetch_all(db)
    .await
}

/// `owner`'s live environments of a template (starting, running or stopping),
/// oldest first.
pub async fn live_environments_of(
    db: &SqlitePool,
    owner_id: &str,
    template_id: &str,
) -> Result<Vec<EnvironmentRow>, sqlx::Error> {
    sqlx::query_as(&format!(
        "SELECT {ENVIRONMENT_COLUMNS} FROM environments WHERE owner_id = ? AND template_id = ? \
         AND state IN ('starting', 'running', 'stopping') ORDER BY created_at, id"
    ))
    .bind(owner_id)
    .bind(template_id)
    .fetch_all(db)
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
    let moved = query.execute(db).await?.rows_affected() > 0;
    if moved && state != "running" {
        end_environment_shares(db, id).await?;
    }
    Ok(moved)
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
    if moved {
        end_environment_shares(db, id).await?;
    }
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

/// Records the host ports a starting environment published (call before
/// [`set_environment_running`], so the page never sees it running without).
pub async fn set_environment_ports(
    db: &SqlitePool,
    id: &str,
    ports: &[cha_wire::HostPort],
) -> Result<(), sqlx::Error> {
    if ports.is_empty() {
        return Ok(());
    }
    let json = serde_json::to_string(ports).unwrap_or_default();
    sqlx::query("UPDATE environments SET ports = ? WHERE id = ? AND state = 'starting'")
        .bind(json)
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}

/// Before a node is deleted: its live environments can't be reached any more.
pub async fn fail_node_environments(
    db: &SqlitePool,
    node_id: &str,
    detail: &str,
) -> Result<u64, sqlx::Error> {
    let ids: Vec<String> = sqlx::query_scalar(
        "SELECT id FROM environments \
         WHERE node_id = ? AND state IN ('starting', 'running', 'stopping')",
    )
    .bind(node_id)
    .fetch_all(db)
    .await?;
    let moved = sqlx::query(
        "UPDATE environments SET state = 'failed', detail = ?, warning = NULL, updated_at = ? \
         WHERE node_id = ? AND state IN ('starting', 'running', 'stopping')",
    )
    .bind(detail)
    .bind(now())
    .bind(node_id)
    .execute(db)
    .await?
    .rows_affected();
    for id in ids {
        end_environment_shares(db, &id).await?;
    }
    Ok(moved)
}

// ---- Share links (0014) ----

/// A share, without its token (only the hash is kept).
#[derive(Debug, FromRow)]
pub struct ShareRow {
    pub id: String,
    pub environment_id: String,
    pub created_by: String,
    /// `player`, `viewer` or `controller` (0015).
    pub role: String,
    /// A player's pad index (1 to 3); `None` for the other roles.
    pub slot: Option<i64>,
    pub created_at: i64,
    pub expires_at: i64,
    /// On the tunnel's hostname, for guests on the internet (0018).
    pub wan: bool,
}

const SHARE_COLUMNS: &str =
    "id, environment_id, created_by, role, slot, created_at, expires_at, wan";

/// Makes a share, revoking the live one it replaces: a player's on the same
/// slot, a controller's on the same environment; viewer links replace nothing.
/// Returns the revoked share's id, if there was one.
pub async fn replace_share(
    db: &SqlitePool,
    share: &ShareRow,
    token_hash: &str,
) -> Result<Option<String>, sqlx::Error> {
    let mut tx = db.begin().await?;
    let old: Option<String> = match share.role.as_str() {
        "viewer" => None,
        "controller" => {
            sqlx::query_scalar(
                "SELECT id FROM shares \
                 WHERE environment_id = ? AND role = 'controller' AND revoked_at IS NULL",
            )
            .bind(&share.environment_id)
            .fetch_optional(&mut *tx)
            .await?
        }
        _ => {
            sqlx::query_scalar(
                "SELECT id FROM shares \
                 WHERE environment_id = ? AND role = 'player' AND slot = ? AND revoked_at IS NULL",
            )
            .bind(&share.environment_id)
            .bind(share.slot)
            .fetch_optional(&mut *tx)
            .await?
        }
    };
    if let Some(old) = &old {
        sqlx::query("UPDATE shares SET revoked_at = ? WHERE id = ?")
            .bind(now())
            .bind(old)
            .execute(&mut *tx)
            .await?;
    }
    sqlx::query(
        "INSERT INTO shares (id, environment_id, created_by, role, slot, token_hash, created_at, expires_at, wan) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&share.id)
    .bind(&share.environment_id)
    .bind(&share.created_by)
    .bind(&share.role)
    .bind(share.slot)
    .bind(token_hash)
    .bind(share.created_at)
    .bind(share.expires_at)
    .bind(share.wan)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(old)
}

/// An environment's shares that haven't been revoked or expired.
pub async fn live_shares(
    db: &SqlitePool,
    environment_id: &str,
) -> Result<Vec<ShareRow>, sqlx::Error> {
    sqlx::query_as(&format!(
        "SELECT {SHARE_COLUMNS} FROM shares \
         WHERE environment_id = ? AND revoked_at IS NULL AND expires_at > ? \
         ORDER BY CASE role WHEN 'player' THEN 0 WHEN 'controller' THEN 1 ELSE 2 END, slot, created_at"
    ))
    .bind(environment_id)
    .bind(now())
    .fetch_all(db)
    .await
}

/// A live share of an environment, by id.
pub async fn live_share(
    db: &SqlitePool,
    environment_id: &str,
    id: &str,
) -> Result<Option<ShareRow>, sqlx::Error> {
    Ok(live_shares(db, environment_id)
        .await?
        .into_iter()
        .find(|s| s.id == id))
}

/// Revokes one live share of an environment; whether there was one.
pub async fn revoke_share(
    db: &SqlitePool,
    environment_id: &str,
    id: &str,
) -> Result<bool, sqlx::Error> {
    Ok(sqlx::query(
        "UPDATE shares SET revoked_at = ? \
         WHERE id = ? AND environment_id = ? AND revoked_at IS NULL",
    )
    .bind(now())
    .bind(id)
    .bind(environment_id)
    .execute(db)
    .await?
    .rows_affected()
        > 0)
}

/// The live share a token names, with its environment's state and owner.
#[derive(Debug, FromRow)]
pub struct SharedAccess {
    #[sqlx(flatten)]
    pub share: ShareRow,
    pub state: String,
    pub template_id: String,
    pub owner_name: String,
}

/// `None` for an unknown, revoked or expired token, or an environment that is
/// stopping or over.
pub async fn share_by_token_hash(
    db: &SqlitePool,
    token_hash: &str,
) -> Result<Option<SharedAccess>, sqlx::Error> {
    sqlx::query_as(
        "SELECT s.id, s.environment_id, s.created_by, s.role, s.slot, s.created_at, s.expires_at, s.wan, \
                e.state, e.template_id, u.display_name AS owner_name \
         FROM shares s \
         JOIN environments e ON e.id = s.environment_id \
         JOIN users u ON u.id = e.owner_id \
         WHERE s.token_hash = ? AND s.revoked_at IS NULL AND s.expires_at > ? \
           AND e.state IN ('starting', 'running')",
    )
    .bind(token_hash)
    .bind(now())
    .fetch_optional(db)
    .await
}

/// How many internet links are live: the tunnel stays up while there are any.
pub async fn live_wan_shares(db: &SqlitePool) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM shares WHERE wan = 1 AND revoked_at IS NULL AND expires_at > ?",
    )
    .bind(now())
    .fetch_one(db)
    .await
}

/// A quick tunnel started anew, so internet links made before `since` (Unix
/// seconds) point at a hostname that no longer exists: revokes them and
/// audits each (no actor: the portal did it).
pub async fn end_stale_wan_shares(db: &SqlitePool, since: i64) -> Result<(), sqlx::Error> {
    let ended: Vec<(String, String)> = sqlx::query_as(
        "UPDATE shares SET revoked_at = ? \
         WHERE wan = 1 AND revoked_at IS NULL AND created_at < ? \
         RETURNING id, environment_id",
    )
    .bind(now())
    .bind(since)
    .fetch_all(db)
    .await?;
    for (id, environment_id) in ended {
        audit(
            db,
            None,
            "share.revoked",
            Some(&environment_id),
            Some(serde_json::json!({ "share": id, "reason": "tunnel_restarted" })),
            None,
        )
        .await?;
    }
    Ok(())
}

/// An environment left `running`: every live share on it ends, and each is
/// audited (no actor: the portal did it).
pub async fn end_environment_shares(
    db: &SqlitePool,
    environment_id: &str,
) -> Result<(), sqlx::Error> {
    let ids: Vec<String> = sqlx::query_scalar(
        "UPDATE shares SET revoked_at = ? WHERE environment_id = ? AND revoked_at IS NULL \
         RETURNING id",
    )
    .bind(now())
    .bind(environment_id)
    .fetch_all(db)
    .await?;
    for id in ids {
        audit(
            db,
            None,
            "share.revoked",
            Some(environment_id),
            Some(serde_json::json!({ "share": id, "reason": "environment_ended" })),
            None,
        )
        .await?;
    }
    Ok(())
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

/// Stores `value` under `key`, replacing what was there.
pub async fn set_setting(db: &SqlitePool, key: &str, value: &str) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO settings (key, value) VALUES (?, ?) \
         ON CONFLICT (key) DO UPDATE SET value = excluded.value",
    )
    .bind(key)
    .bind(value)
    .execute(db)
    .await?;
    Ok(())
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

/// The user's stored preferences (a JSON object as text), if they've saved any.
pub async fn user_prefs(db: &SqlitePool, user_id: &str) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar("SELECT prefs FROM user_prefs WHERE user_id = ?")
        .bind(user_id)
        .fetch_optional(db)
        .await
}

/// Replaces the user's stored preferences.
pub async fn set_user_prefs(
    db: &SqlitePool,
    user_id: &str,
    prefs: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO user_prefs (user_id, prefs, updated_at) VALUES (?1, ?2, ?3) \
         ON CONFLICT (user_id) DO UPDATE SET prefs = ?2, updated_at = ?3",
    )
    .bind(user_id)
    .bind(prefs)
    .bind(now())
    .execute(db)
    .await?;
    Ok(())
}

// ---- Moonlight hosts (ADR 0008) ----

#[derive(Clone, Debug, FromRow)]
pub struct MoonlightHostRow {
    pub id: String,
    pub node_id: String,
    pub unique_id: String,
    pub name: String,
    pub address: Option<String>,
    pub http_port: Option<i64>,
    pub https_port: Option<i64>,
    /// A JSON array of codec names.
    pub codecs: Option<String>,
    /// A JSON array of `{id, name, hdr}`.
    pub apps: Option<String>,
    pub apps_at: Option<i64>,
    pub adopted_by: Option<String>,
    pub created_at: Option<i64>,
}

const MOONLIGHT_COLUMNS: &str = "id, node_id, unique_id, name, address, http_port, https_port, codecs, apps, apps_at, adopted_by, created_at";

pub async fn moonlight_hosts(db: &SqlitePool) -> Result<Vec<MoonlightHostRow>, sqlx::Error> {
    sqlx::query_as(&format!(
        "SELECT {MOONLIGHT_COLUMNS} FROM moonlight_hosts ORDER BY name, id"
    ))
    .fetch_all(db)
    .await
}

pub async fn moonlight_host(
    db: &SqlitePool,
    id: &str,
) -> Result<Option<MoonlightHostRow>, sqlx::Error> {
    sqlx::query_as(&format!(
        "SELECT {MOONLIGHT_COLUMNS} FROM moonlight_hosts WHERE id = ?"
    ))
    .bind(id)
    .fetch_optional(db)
    .await
}

pub async fn moonlight_host_of_node(
    db: &SqlitePool,
    node_id: &str,
    unique_id: &str,
) -> Result<Option<MoonlightHostRow>, sqlx::Error> {
    sqlx::query_as(&format!(
        "SELECT {MOONLIGHT_COLUMNS} FROM moonlight_hosts WHERE node_id = ? AND unique_id = ?"
    ))
    .bind(node_id)
    .bind(unique_id)
    .fetch_optional(db)
    .await
}

pub async fn insert_moonlight_host(
    db: &SqlitePool,
    row: &MoonlightHostRow,
) -> Result<(), sqlx::Error> {
    sqlx::query(&format!(
        "INSERT INTO moonlight_hosts ({MOONLIGHT_COLUMNS}) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
    ))
    .bind(&row.id)
    .bind(&row.node_id)
    .bind(&row.unique_id)
    .bind(&row.name)
    .bind(&row.address)
    .bind(row.http_port)
    .bind(row.https_port)
    .bind(&row.codecs)
    .bind(&row.apps)
    .bind(row.apps_at)
    .bind(&row.adopted_by)
    .bind(row.created_at)
    .execute(db)
    .await?;
    Ok(())
}

/// Where the node last saw the host and what it encodes.
pub async fn update_moonlight_host_found(
    db: &SqlitePool,
    id: &str,
    name: &str,
    address: &str,
    http_port: i64,
    https_port: i64,
    codecs: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE moonlight_hosts SET name = ?, address = ?, http_port = ?, https_port = ?, codecs = ? WHERE id = ?",
    )
    .bind(name)
    .bind(address)
    .bind(http_port)
    .bind(https_port)
    .bind(codecs)
    .bind(id)
    .execute(db)
    .await?;
    Ok(())
}

pub async fn set_moonlight_apps(db: &SqlitePool, id: &str, apps: &str) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE moonlight_hosts SET apps = ?, apps_at = ? WHERE id = ?")
        .bind(apps)
        .bind(now())
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}

pub async fn delete_moonlight_host(db: &SqlitePool, id: &str) -> Result<bool, sqlx::Error> {
    Ok(sqlx::query("DELETE FROM moonlight_hosts WHERE id = ?")
        .bind(id)
        .execute(db)
        .await?
        .rows_affected()
        > 0)
}

/// The live environment streaming one of a host's apps: its id and its
/// owner's display name.
pub async fn live_moonlight_environment(
    db: &SqlitePool,
    host_id: &str,
) -> Result<Option<(String, String)>, sqlx::Error> {
    sqlx::query_as(
        "SELECT e.id, u.display_name FROM environments e JOIN users u ON u.id = e.owner_id \
         WHERE e.template_id LIKE ? AND e.state IN ('starting', 'running', 'stopping') LIMIT 1",
    )
    .bind(format!("moonlight:{host_id}:%"))
    .fetch_optional(db)
    .await
}

// ---- GameStream devices (ADR 0009) ----

/// A paired Moonlight client, with the names the API shows.
#[derive(Clone, Debug, FromRow)]
pub struct GameStreamDeviceRow {
    pub id: String,
    pub user_id: String,
    pub node_id: String,
    pub fingerprint: String,
    pub unique_id: Option<String>,
    pub name: String,
    pub paired_at: i64,
    pub node_name: String,
    pub owner_name: String,
}

const GAMESTREAM_SELECT: &str = "SELECT d.id, d.user_id, d.node_id, d.fingerprint, d.unique_id, d.name, d.paired_at, \
     n.name AS node_name, u.display_name AS owner_name \
     FROM gamestream_devices d JOIN nodes n ON n.id = d.node_id JOIN users u ON u.id = d.user_id";

/// Every device, or one user's, oldest first.
pub async fn gamestream_devices(
    db: &SqlitePool,
    user_id: Option<&str>,
) -> Result<Vec<GameStreamDeviceRow>, sqlx::Error> {
    sqlx::query_as(&format!(
        "{GAMESTREAM_SELECT} WHERE (?1 IS NULL OR d.user_id = ?1) ORDER BY d.paired_at, d.id"
    ))
    .bind(user_id)
    .fetch_all(db)
    .await
}

pub async fn gamestream_devices_of_node(
    db: &SqlitePool,
    node_id: &str,
) -> Result<Vec<GameStreamDeviceRow>, sqlx::Error> {
    sqlx::query_as(&format!(
        "{GAMESTREAM_SELECT} WHERE d.node_id = ? ORDER BY d.paired_at, d.id"
    ))
    .bind(node_id)
    .fetch_all(db)
    .await
}

pub async fn gamestream_device(
    db: &SqlitePool,
    id: &str,
) -> Result<Option<GameStreamDeviceRow>, sqlx::Error> {
    sqlx::query_as(&format!("{GAMESTREAM_SELECT} WHERE d.id = ?"))
        .bind(id)
        .fetch_optional(db)
        .await
}

/// Records a pairing. A client that pairs again with the same node keeps its
/// row and takes the new owner and name.
pub async fn upsert_gamestream_device(
    db: &SqlitePool,
    user_id: &str,
    node_id: &str,
    fingerprint: &str,
    unique_id: Option<&str>,
    name: &str,
) -> Result<GameStreamDeviceRow, sqlx::Error> {
    let id: String = sqlx::query_scalar(
        "INSERT INTO gamestream_devices (id, user_id, node_id, fingerprint, unique_id, name, paired_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
         ON CONFLICT (node_id, fingerprint) DO UPDATE SET user_id = ?2, unique_id = ?5, name = ?6, paired_at = ?7 \
         RETURNING id",
    )
    .bind(new_id())
    .bind(user_id)
    .bind(node_id)
    .bind(fingerprint)
    .bind(unique_id)
    .bind(name)
    .bind(now())
    .fetch_one(db)
    .await?;
    gamestream_device(db, &id)
        .await?
        .ok_or(sqlx::Error::RowNotFound)
}

pub async fn delete_gamestream_device(db: &SqlitePool, id: &str) -> Result<bool, sqlx::Error> {
    Ok(sqlx::query("DELETE FROM gamestream_devices WHERE id = ?")
        .bind(id)
        .execute(db)
        .await?
        .rows_affected()
        > 0)
}

/// Forgets a device a client unpaired itself; the row it had, if any.
pub async fn delete_gamestream_device_of(
    db: &SqlitePool,
    node_id: &str,
    fingerprint: &str,
) -> Result<Option<GameStreamDeviceRow>, sqlx::Error> {
    let row: Option<GameStreamDeviceRow> = sqlx::query_as(&format!(
        "{GAMESTREAM_SELECT} WHERE d.node_id = ? AND d.fingerprint = ?"
    ))
    .bind(node_id)
    .bind(fingerprint)
    .fetch_optional(db)
    .await?;
    if let Some(row) = &row {
        delete_gamestream_device(db, &row.id).await?;
    }
    Ok(row)
}

// ---- Cha Player devices (C2.1, ADR 0013) ----

pub const TICKET_TTL_SECS: i64 = 60;
pub const DEVICE_CODE_TTL_SECS: i64 = 600;
/// How often `last_used_at` and `last_ip` are refreshed, at most.
pub const DEVICE_TOUCH_SECS: i64 = 60;

/// A signed-in player install, with its owner's username.
#[derive(Clone, Debug, FromRow)]
pub struct DeviceRow {
    pub id: String,
    pub user_id: String,
    pub name: String,
    pub created_at: i64,
    pub last_used_at: i64,
    pub last_ip: Option<String>,
    pub username: String,
}

const DEVICE_SELECT: &str = "SELECT d.id, d.user_id, d.name, d.created_at, d.last_used_at, d.last_ip, u.username \
     FROM devices d JOIN users u ON u.id = d.user_id";

/// Every device, or one user's, oldest first.
pub async fn list_devices(
    db: &SqlitePool,
    user_id: Option<&str>,
) -> Result<Vec<DeviceRow>, sqlx::Error> {
    sqlx::query_as(&format!(
        "{DEVICE_SELECT} WHERE (?1 IS NULL OR d.user_id = ?1) ORDER BY d.created_at, d.id"
    ))
    .bind(user_id)
    .fetch_all(db)
    .await
}

pub async fn device(db: &SqlitePool, id: &str) -> Result<Option<DeviceRow>, sqlx::Error> {
    sqlx::query_as(&format!("{DEVICE_SELECT} WHERE d.id = ?"))
        .bind(id)
        .fetch_optional(db)
        .await
}

pub async fn delete_device(db: &SqlitePool, id: &str) -> Result<bool, sqlx::Error> {
    Ok(sqlx::query("DELETE FROM devices WHERE id = ?")
        .bind(id)
        .execute(db)
        .await?
        .rows_affected()
        > 0)
}

/// The enabled user behind a device token, and the device's id. Refreshes
/// `last_used_at` and `last_ip` when the last refresh is over a minute old.
pub async fn device_user(
    db: &SqlitePool,
    token_hash: &str,
    ip: Option<&str>,
) -> Result<Option<User>, sqlx::Error> {
    let found: Option<(String, i64, User)> = sqlx::query(
        "SELECT d.id AS device_id, d.last_used_at AS last_used_at, \
         u.id, u.username, u.display_name, u.role, u.disabled, u.created_at, \
         u.email, u.max_instances, u.node_restricted \
         FROM devices d JOIN users u ON u.id = d.user_id \
         WHERE d.token_hash = ? AND u.disabled = 0",
    )
    .bind(token_hash)
    .try_map(|row: sqlx::sqlite::SqliteRow| {
        use sqlx::Row;
        Ok((
            row.try_get("device_id")?,
            row.try_get("last_used_at")?,
            User::from_row(&row)?,
        ))
    })
    .fetch_optional(db)
    .await?;
    let Some((device_id, last_used, user)) = found else {
        return Ok(None);
    };
    let now = now();
    if now - last_used >= DEVICE_TOUCH_SECS {
        sqlx::query("UPDATE devices SET last_used_at = ?, last_ip = ? WHERE id = ?")
            .bind(now)
            .bind(ip)
            .bind(device_id)
            .execute(db)
            .await?;
    }
    Ok(Some(user))
}

/// Signs `install_id` in as `user_id`: a new device row, or the existing one
/// for this (user, install) with its token replaced. Returns the device's id.
pub async fn upsert_device(
    db: &SqlitePool,
    user_id: &str,
    install_id: &str,
    name: &str,
    token_hash: &str,
    ip: Option<&str>,
) -> Result<String, sqlx::Error> {
    let now = now();
    sqlx::query_scalar(
        "INSERT INTO devices (id, user_id, install_id, name, token_hash, created_at, last_used_at, last_ip) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6, ?7) \
         ON CONFLICT (user_id, install_id) DO UPDATE \
         SET name = ?4, token_hash = ?5, last_used_at = ?6, last_ip = ?7 \
         RETURNING id",
    )
    .bind(new_id())
    .bind(user_id)
    .bind(install_id)
    .bind(name)
    .bind(token_hash)
    .bind(now)
    .bind(ip)
    .fetch_one(db)
    .await
}

pub async fn insert_ticket(
    db: &SqlitePool,
    ticket_hash: &str,
    user_id: &str,
) -> Result<(), sqlx::Error> {
    let now = now();
    sqlx::query("DELETE FROM device_tickets WHERE expires_at <= ?")
        .bind(now)
        .execute(db)
        .await?;
    sqlx::query(
        "INSERT INTO device_tickets (ticket_hash, user_id, created_at, expires_at) VALUES (?, ?, ?, ?)",
    )
    .bind(ticket_hash)
    .bind(user_id)
    .bind(now)
    .bind(now + TICKET_TTL_SECS)
    .execute(db)
    .await?;
    Ok(())
}

/// Spends a ticket: the user it was issued to, if it exists and hasn't
/// expired. A ticket works once.
pub async fn redeem_ticket(
    db: &SqlitePool,
    ticket_hash: &str,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar(
        "DELETE FROM device_tickets WHERE ticket_hash = ? AND expires_at > ? RETURNING user_id",
    )
    .bind(ticket_hash)
    .bind(now())
    .fetch_optional(db)
    .await
}

/// Codes waiting for a user, after dropping the expired ones.
pub async fn count_pending_device_codes(db: &SqlitePool) -> Result<i64, sqlx::Error> {
    sqlx::query("DELETE FROM device_codes WHERE expires_at <= ?")
        .bind(now())
        .execute(db)
        .await?;
    sqlx::query_scalar("SELECT COUNT(*) FROM device_codes")
        .fetch_one(db)
        .await
}

/// Stores a new pending code. `false` when `user_code` is already taken.
pub async fn insert_device_code(
    db: &SqlitePool,
    code_hash: &str,
    user_code: &str,
    install_id: &str,
    name: &str,
) -> Result<bool, sqlx::Error> {
    let now = now();
    Ok(sqlx::query(
        "INSERT INTO device_codes (code_hash, user_code, install_id, name, created_at, expires_at) \
         VALUES (?, ?, ?, ?, ?, ?) ON CONFLICT DO NOTHING",
    )
    .bind(code_hash)
    .bind(user_code)
    .bind(install_id)
    .bind(name)
    .bind(now)
    .bind(now + DEVICE_CODE_TTL_SECS)
    .execute(db)
    .await?
    .rows_affected()
        > 0)
}

#[derive(Clone, Debug, FromRow)]
pub struct DeviceCodeRow {
    pub code_hash: String,
    pub user_code: String,
    pub install_id: String,
    pub name: String,
    pub status: String,
    pub user_id: Option<String>,
    pub created_at: i64,
    pub expires_at: i64,
    pub last_poll_at: Option<i64>,
}

const DEVICE_CODE_SELECT: &str = "SELECT code_hash, user_code, install_id, name, status, user_id, created_at, expires_at, last_poll_at \
     FROM device_codes";

/// A code waiting for approval, by the user code (digits only, upper-case).
pub async fn pending_device_code(
    db: &SqlitePool,
    user_code: &str,
) -> Result<Option<DeviceCodeRow>, sqlx::Error> {
    sqlx::query_as(&format!(
        "{DEVICE_CODE_SELECT} WHERE user_code = ? AND status = 'pending' AND expires_at > ?"
    ))
    .bind(user_code)
    .bind(now())
    .fetch_optional(db)
    .await
}

/// A code by its secret, expired or not.
pub async fn device_code(
    db: &SqlitePool,
    code_hash: &str,
) -> Result<Option<DeviceCodeRow>, sqlx::Error> {
    sqlx::query_as(&format!("{DEVICE_CODE_SELECT} WHERE code_hash = ?"))
        .bind(code_hash)
        .fetch_optional(db)
        .await
}

/// Approves (with `user_id`) or denies (`None`) a pending, unexpired code.
/// `false` when there is no such code.
pub async fn resolve_device_code(
    db: &SqlitePool,
    user_code: &str,
    approve_for: Option<&str>,
) -> Result<bool, sqlx::Error> {
    Ok(sqlx::query(
        "UPDATE device_codes SET status = ?, user_id = ? \
         WHERE user_code = ? AND status = 'pending' AND expires_at > ?",
    )
    .bind(if approve_for.is_some() {
        "approved"
    } else {
        "denied"
    })
    .bind(approve_for)
    .bind(user_code)
    .bind(now())
    .execute(db)
    .await?
    .rows_affected()
        > 0)
}

pub async fn touch_device_code_poll(db: &SqlitePool, code_hash: &str) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE device_codes SET last_poll_at = ? WHERE code_hash = ?")
        .bind(now())
        .bind(code_hash)
        .execute(db)
        .await?;
    Ok(())
}

/// Spends an approved code: its user, once.
pub async fn redeem_device_code(
    db: &SqlitePool,
    code_hash: &str,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar(
        "DELETE FROM device_codes WHERE code_hash = ? AND status = 'approved' AND expires_at > ? \
         RETURNING user_id",
    )
    .bind(code_hash)
    .bind(now())
    .fetch_optional(db)
    .await
}

// ---- Per-user access: node restrictions, limits and grants ----

/// A template on a node that a restricted user may use anyway.
#[derive(Clone, Debug, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub struct UserGrant {
    pub id: String,
    pub node_id: String,
    pub template_id: String,
    pub created_at: i64,
}

/// The ids of the nodes `user_id` is limited to, when restricted.
pub async fn user_node_ids(db: &SqlitePool, user_id: &str) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar("SELECT node_id FROM user_nodes WHERE user_id = ? ORDER BY node_id")
        .bind(user_id)
        .fetch_all(db)
        .await
}

/// Replaces a user's restriction flag, node list and instance limit in one step.
/// The node ids must exist (a foreign key fails otherwise, changing nothing).
pub async fn set_user_access(
    db: &SqlitePool,
    user_id: &str,
    node_restricted: bool,
    node_ids: &[String],
    max_instances: Option<i64>,
) -> Result<(), sqlx::Error> {
    let mut tx = db.begin().await?;
    sqlx::query("UPDATE users SET node_restricted = ?, max_instances = ? WHERE id = ?")
        .bind(node_restricted)
        .bind(max_instances)
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM user_nodes WHERE user_id = ?")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
    for node_id in node_ids {
        sqlx::query("INSERT OR IGNORE INTO user_nodes (user_id, node_id) VALUES (?, ?)")
            .bind(user_id)
            .bind(node_id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await
}

pub async fn user_grants(db: &SqlitePool, user_id: &str) -> Result<Vec<UserGrant>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, node_id, template_id, created_at FROM user_grants \
         WHERE user_id = ? ORDER BY created_at, id",
    )
    .bind(user_id)
    .fetch_all(db)
    .await
}

/// Adds a grant; an existing one for the same node and template is returned as is.
pub async fn add_user_grant(
    db: &SqlitePool,
    user_id: &str,
    node_id: &str,
    template_id: &str,
    created_by: &str,
) -> Result<UserGrant, sqlx::Error> {
    sqlx::query(
        "INSERT OR IGNORE INTO user_grants (id, user_id, node_id, template_id, created_by, created_at) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(new_id())
    .bind(user_id)
    .bind(node_id)
    .bind(template_id)
    .bind(created_by)
    .bind(now())
    .execute(db)
    .await?;
    sqlx::query_as(
        "SELECT id, node_id, template_id, created_at FROM user_grants \
         WHERE user_id = ? AND node_id = ? AND template_id = ?",
    )
    .bind(user_id)
    .bind(node_id)
    .bind(template_id)
    .fetch_one(db)
    .await
}

/// Whether a grant of `user_id`'s was removed.
pub async fn remove_user_grant(
    db: &SqlitePool,
    user_id: &str,
    grant_id: &str,
) -> Result<bool, sqlx::Error> {
    Ok(
        sqlx::query("DELETE FROM user_grants WHERE id = ? AND user_id = ?")
            .bind(grant_id)
            .bind(user_id)
            .execute(db)
            .await?
            .rows_affected()
            > 0,
    )
}

/// The nodes `user_id` may use for `template_id`: `None` when unrestricted
/// (any node), else the nodes on their list plus those a grant names for the
/// template.
pub async fn allowed_nodes(
    db: &SqlitePool,
    user_id: &str,
    template_id: &str,
) -> Result<Option<Vec<String>>, sqlx::Error> {
    let restricted: Option<bool> =
        sqlx::query_scalar("SELECT node_restricted FROM users WHERE id = ?")
            .bind(user_id)
            .fetch_optional(db)
            .await?;
    if restricted != Some(true) {
        return Ok(None);
    }
    sqlx::query_scalar(
        "SELECT node_id FROM user_nodes WHERE user_id = ?1 \
         UNION SELECT node_id FROM user_grants WHERE user_id = ?1 AND template_id = ?2",
    )
    .bind(user_id)
    .bind(template_id)
    .fetch_all(db)
    .await
    .map(Some)
}

/// A user's grants with their nodes' names, for the dashboard.
pub async fn user_grants_with_nodes(
    db: &SqlitePool,
    user_id: &str,
) -> Result<Vec<(UserGrant, String)>, sqlx::Error> {
    let rows: Vec<(String, String, String, i64, String)> = sqlx::query_as(
        "SELECT g.id, g.node_id, g.template_id, g.created_at, n.name \
         FROM user_grants g JOIN nodes n ON n.id = g.node_id \
         WHERE g.user_id = ? ORDER BY g.created_at, g.id",
    )
    .bind(user_id)
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, node_id, template_id, created_at, name)| {
            (
                UserGrant {
                    id,
                    node_id,
                    template_id,
                    created_at,
                },
                name,
            )
        })
        .collect())
}

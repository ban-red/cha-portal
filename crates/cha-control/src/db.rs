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

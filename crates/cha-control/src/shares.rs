//! Share links (ADRs 0014 and 0015; the contract is `docs/plans/share-links.md`).
//!
//! The owner (or an admin) of a running environment makes a link,
//! `/s/<token>`, for a player (one gamepad slot, 1 to 3 is player 2 to 4), a
//! viewer (watch and listen) or a controller (keyboard, mouse and pads when
//! handed the controls). Whoever holds
//! the token can join with no account: they see what the link is for and
//! connect through the same brokering as the owner, with a media token whose
//! role is the link's (`player` also carries the slot). The token is 256 random bits,
//! shown once; only its SHA-256 is kept, and it is never logged or put in the
//! audit log. A link ends at its expiry (24 hours), when revoked, or when its
//! environment leaves `running` (`db::end_environment_shares`).

use std::collections::HashMap;
use std::sync::Mutex;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::AppState;
use crate::auth::{self, ClientInfo, CurrentUser, token_hash};
use crate::db::{self, ShareRow};
use crate::environments::{self, ConnectRequest, ConnectResponse, Guest};
use crate::error::{ApiError, ApiResult};

/// How long a link lasts at most.
const SHARE_TTL_SECS: i64 = 24 * 3600;
/// Requests per address per minute to the two unauthenticated routes.
const REQUESTS_PER_MINUTE: u32 = 30;
const WINDOW_SECS: i64 = 60;
/// Addresses the limiter remembers at once; when full, windows that have
/// passed are dropped first.
const MAX_TRACKED: usize = 10_000;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/environments/{id}/shares", get(list).post(create))
        .route("/environments/{id}/shares/{share}", delete(revoke))
        .route("/shares/{token}", get(info))
        .route("/shares/{token}/connect", post(connect))
}

/// A fixed-window counter per address, in memory (a restart forgets it, which
/// only lets a guesser start over, and tokens are 256 bits).
#[derive(Default)]
pub struct RateLimit {
    windows: Mutex<HashMap<String, (i64, u32)>>,
}

impl RateLimit {
    /// Counts a request from `key` at `now` (Unix seconds); `false` when it
    /// is over the limit for this minute.
    pub fn allow(&self, key: &str, now: i64) -> bool {
        let mut windows = self.windows.lock().unwrap_or_else(|e| e.into_inner());
        if windows.len() >= MAX_TRACKED && !windows.contains_key(key) {
            windows.retain(|_, (start, _)| now - *start < WINDOW_SECS);
            if windows.len() >= MAX_TRACKED {
                // Everyone is active: refuse a new address rather than grow.
                return false;
            }
        }
        let entry = windows.entry(key.to_string()).or_insert((now, 0));
        if now - entry.0 >= WINDOW_SECS {
            *entry = (now, 0);
        }
        entry.1 += 1;
        entry.1 <= REQUESTS_PER_MINUTE
    }
}

fn check_rate(state: &AppState, client: &ClientInfo) -> ApiResult<()> {
    let key = client.ip.as_deref().unwrap_or("unknown");
    if state.share_limit.allow(key, db::now()) {
        Ok(())
    } else {
        Err(ApiError::TooManyRequests(
            "rate_limited",
            "too many requests; wait a minute".into(),
        ))
    }
}

fn unknown_share() -> ApiError {
    ApiError::NotFoundCode(
        "unknown_share",
        "this link has expired or was revoked".into(),
    )
}

// ---- The owner (cookie) ----

#[derive(Deserialize)]
struct CreateRequest {
    role: String,
    /// A player's slot; left out (or null) for the other roles.
    #[serde(default)]
    slot: Option<i64>,
}

async fn create(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    client: ClientInfo,
    Path(env_id): Path<String>,
    Json(req): Json<CreateRequest>,
) -> ApiResult<Json<Value>> {
    let row = environments::visible(&state, &user, &env_id).await?;
    match (req.role.as_str(), req.slot) {
        ("player", Some(1..=3)) => {}
        ("player", _) => {
            return Err(ApiError::bad_request(
                "bad_slot",
                "the slot is 1 to 3 (player 2 to 4)",
            ));
        }
        ("viewer" | "controller", None) => {}
        ("viewer" | "controller", Some(_)) => {
            return Err(ApiError::bad_request(
                "bad_slot",
                "only a player link has a slot",
            ));
        }
        _ => {
            return Err(ApiError::bad_request(
                "bad_role",
                "the role is player, viewer or controller",
            ));
        }
    }
    if row.state != "running" {
        return Err(ApiError::conflict(
            "not_running",
            format!("the environment is {}", row.state),
        ));
    }
    let token = auth::random_token()?;
    let now = db::now();
    let share = ShareRow {
        id: db::new_id(),
        environment_id: env_id.clone(),
        created_by: user.id.clone(),
        role: req.role,
        slot: req.slot,
        created_at: now,
        expires_at: now + SHARE_TTL_SECS,
    };
    let replaced = db::replace_share(&state.db, &share, &token_hash(&token)).await?;
    if let Some(old) = replaced {
        db::audit(
            &state.db,
            Some(&user.id),
            "share.revoked",
            Some(&env_id),
            Some(json!({ "share": old, "slot": share.slot, "reason": "replaced" })),
            client.ip.as_deref(),
        )
        .await?;
    }
    db::audit(
        &state.db,
        Some(&user.id),
        "share.created",
        Some(&env_id),
        Some(json!({ "share": share.id, "role": share.role, "slot": share.slot })),
        client.ip.as_deref(),
    )
    .await?;
    Ok(Json(json!({
        "id": share.id,
        "url": format!("/s/{token}"),
        "role": share.role,
        "slot": share.slot,
        "expires_at": share.expires_at,
    })))
}

#[derive(Serialize)]
struct ShareView {
    id: String,
    role: String,
    slot: Option<i64>,
    created_at: i64,
    expires_at: i64,
}

async fn list(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(env_id): Path<String>,
) -> ApiResult<Json<Vec<ShareView>>> {
    environments::visible(&state, &user, &env_id).await?;
    let rows = db::live_shares(&state.db, &env_id).await?;
    Ok(Json(
        rows.into_iter()
            .map(|s| ShareView {
                id: s.id,
                role: s.role,
                slot: s.slot,
                created_at: s.created_at,
                expires_at: s.expires_at,
            })
            .collect(),
    ))
}

async fn revoke(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    client: ClientInfo,
    Path((env_id, share_id)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    environments::visible(&state, &user, &env_id).await?;
    let share = db::live_share(&state.db, &env_id, &share_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("no such link".into()))?;
    if db::revoke_share(&state.db, &env_id, &share.id).await? {
        db::audit(
            &state.db,
            Some(&user.id),
            "share.revoked",
            Some(&env_id),
            Some(json!({ "share": share.id, "slot": share.slot, "reason": "revoked" })),
            client.ip.as_deref(),
        )
        .await?;
    }
    Ok(StatusCode::NO_CONTENT)
}

// ---- The guest (the token is the credential) ----

/// The live share a token names; one 404 for every way it can be dead.
async fn access(state: &AppState, token: &str) -> ApiResult<db::SharedAccess> {
    db::share_by_token_hash(&state.db, &token_hash(token))
        .await?
        .ok_or_else(unknown_share)
}

async fn info(
    State(state): State<AppState>,
    client: ClientInfo,
    Path(token): Path<String>,
) -> ApiResult<Json<Value>> {
    check_rate(&state, &client)?;
    let share = access(&state, &token).await?;
    // What the guest's browser can pick from: the codec the owner plays
    // shares its encoder.
    let codecs = match db::environment_by_id(&state.db, &share.share.environment_id).await? {
        Some(row) => environments::codecs_of(&state, row).await?,
        None => None,
    };
    let app = environments::find_any(&state, &share.template_id)
        .map(|t| t.name)
        .unwrap_or(share.template_id);
    Ok(Json(json!({
        "app": app,
        "owner": share.owner_name,
        "role": share.share.role,
        "slot": share.share.slot,
        "state": share.state,
        "codecs": codecs,
    })))
}

async fn connect(
    State(state): State<AppState>,
    client: ClientInfo,
    Path(token): Path<String>,
    Json(req): Json<ConnectRequest>,
) -> ApiResult<Json<ConnectResponse>> {
    check_rate(&state, &client)?;
    let access = access(&state, &token).await?;
    let row = db::environment_by_id(&state.db, &access.share.environment_id)
        .await?
        .ok_or_else(unknown_share)?;
    let (role, slot) = match access.share.role.as_str() {
        "controller" => ("controller", None),
        "viewer" => ("viewer", None),
        _ => (
            "player",
            access.share.slot.and_then(|s| u8::try_from(s).ok()),
        ),
    };
    let guest = Guest {
        sub: format!("share:{}", access.share.id),
        role,
        slot,
    };
    let (codec, response) = environments::broker(&state, &row, req, guest).await?;
    db::audit(
        &state.db,
        None,
        "share.joined",
        Some(&row.id),
        Some(json!({
            "share": access.share.id,
            "role": access.share.role,
            "slot": access.share.slot,
            "codec": codec,
            "transport": response.transport,
        })),
        client.ip.as_deref(),
    )
    .await?;
    Ok(Json(response))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_address_gets_thirty_requests_a_minute() {
        let limit = RateLimit::default();
        for _ in 0..REQUESTS_PER_MINUTE {
            assert!(limit.allow("203.0.113.1", 1000));
        }
        assert!(!limit.allow("203.0.113.1", 1059));
        // Another address is unaffected, and the first starts over.
        assert!(limit.allow("203.0.113.2", 1059));
        assert!(limit.allow("203.0.113.1", 1060));
    }
}

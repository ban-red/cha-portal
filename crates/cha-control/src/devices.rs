//! Cha Player sign-in (C2.1, ADR 0013; the contract is `docs/plans/c2-device-signin.md`).
//!
//! A player becomes a *device*: a row holding the SHA-256 of a long-lived
//! bearer token that [`PlayerUser`](crate::auth::PlayerUser) accepts on the
//! player's routes. Two ways in. A signed-in browser asks for a one-use,
//! 60-second ticket and hands the player a `cha://connect` link; or the player
//! asks for a code, shows its short user code, and the user approves it on
//! `/link` while the player polls. Signing in again from the same install
//! replaces the token on its row. Every issue, approval, denial and
//! revocation goes in the audit log.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::AppState;
use crate::auth::{self, ClientInfo, CurrentUser, new_device_token, token_hash};
use crate::db::{self, Role, User};
use crate::error::{ApiError, ApiResult};

/// The seconds a player should wait between polls.
const POLL_INTERVAL_SECS: i64 = 5;
/// A poll this much early is still on time (clocks and network jitter).
const POLL_SLACK_SECS: i64 = 1;
/// Pending codes the portal holds at once; more are refused, so the
/// unauthenticated endpoint can't fill the database.
const MAX_PENDING_CODES: i64 = 500;
const MAX_NAME_CHARS: usize = 100;
const MAX_INSTALL_ID_CHARS: usize = 64;
/// User codes use these letters: no vowels and no look-alikes.
const USER_CODE_ALPHABET: &[u8] = b"BCDFGHJKLMNPQRSTVWXZ";
const USER_CODE_LEN: usize = 8;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/devices", get(list))
        .route("/devices/tickets", post(issue_ticket))
        .route("/devices/{id}", axum::routing::delete(revoke))
        .route("/devices/codes/{user_code}", get(show_code))
        .route("/devices/codes/{user_code}/approve", post(approve))
        .route("/devices/codes/{user_code}/deny", post(deny))
        .route("/device/ticket", post(swap_ticket))
        .route("/device/code", post(start_code))
        .route("/device/token", post(poll_code))
}

// ---- User codes ----

/// Upper-cases a typed user code and drops dashes and spaces; `None` unless
/// what is left is eight letters of the alphabet.
pub fn normalize_user_code(input: &str) -> Option<String> {
    let code: String = input
        .chars()
        .filter(|c| !matches!(c, '-' | ' '))
        .map(|c| c.to_ascii_uppercase())
        .collect();
    (code.len() == USER_CODE_LEN && code.bytes().all(|b| USER_CODE_ALPHABET.contains(&b)))
        .then_some(code)
}

/// `ABCDEFGH` as `ABCD-EFGH`.
fn display_user_code(code: &str) -> String {
    format!("{}-{}", &code[..4], &code[4..])
}

fn new_user_code() -> anyhow::Result<String> {
    let mut code = String::with_capacity(USER_CODE_LEN);
    while code.len() < USER_CODE_LEN {
        let mut byte = [0u8; 1];
        getrandom::fill(&mut byte).map_err(|e| anyhow::anyhow!("random source: {e}"))?;
        // 240 is a multiple of 20, so every letter is equally likely.
        if byte[0] < 240 {
            code.push(USER_CODE_ALPHABET[usize::from(byte[0]) % USER_CODE_ALPHABET.len()] as char);
        }
    }
    Ok(code)
}

fn unknown_code() -> ApiError {
    ApiError::NotFoundCode("unknown_code", "no such code, or it has expired".into())
}

// ---- Signed in (cookie) ----

#[derive(Deserialize, Default)]
struct TicketRequest {
    /// The template the link should launch; only echoed into the audit log.
    launch: Option<String>,
}

async fn issue_ticket(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    client: ClientInfo,
    body: Option<Json<TicketRequest>>,
) -> ApiResult<Json<Value>> {
    let req = body.map(|Json(b)| b).unwrap_or_default();
    if let Some(launch) = &req.launch
        && crate::environments::template(launch).is_none()
    {
        return Err(ApiError::bad_request(
            "unknown_template",
            "no such app to launch",
        ));
    }
    let ticket = auth::random_token()?;
    db::insert_ticket(&state.db, &token_hash(&ticket), &user.id).await?;
    db::audit(
        &state.db,
        Some(&user.id),
        "device.ticket_issued",
        None,
        req.launch.map(|l| json!({ "launch": l })),
        client.ip.as_deref(),
    )
    .await?;
    Ok(Json(
        json!({ "ticket": ticket, "expires_in": db::TICKET_TTL_SECS }),
    ))
}

async fn show_code(
    State(state): State<AppState>,
    _: CurrentUser,
    Path(user_code): Path<String>,
) -> ApiResult<Json<Value>> {
    let code = normalize_user_code(&user_code).ok_or_else(unknown_code)?;
    let row = db::pending_device_code(&state.db, &code)
        .await?
        .ok_or_else(unknown_code)?;
    Ok(Json(json!({
        "name": row.name,
        "created_at": row.created_at,
        "expires_at": row.expires_at,
    })))
}

async fn approve(
    state: State<AppState>,
    user: CurrentUser,
    client: ClientInfo,
    user_code: Path<String>,
) -> ApiResult<Json<Value>> {
    resolve(state, user, client, user_code, true).await
}

async fn deny(
    state: State<AppState>,
    user: CurrentUser,
    client: ClientInfo,
    user_code: Path<String>,
) -> ApiResult<Json<Value>> {
    resolve(state, user, client, user_code, false).await
}

async fn resolve(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    client: ClientInfo,
    Path(user_code): Path<String>,
    approve: bool,
) -> ApiResult<Json<Value>> {
    let code = normalize_user_code(&user_code).ok_or_else(unknown_code)?;
    let row = db::pending_device_code(&state.db, &code)
        .await?
        .ok_or_else(unknown_code)?;
    let changed =
        db::resolve_device_code(&state.db, &code, approve.then_some(user.id.as_str())).await?;
    if !changed {
        return Err(unknown_code());
    }
    db::audit(
        &state.db,
        Some(&user.id),
        if approve {
            "device.code_approved"
        } else {
            "device.code_denied"
        },
        None,
        Some(json!({ "name": row.name })),
        client.ip.as_deref(),
    )
    .await?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Serialize)]
struct DeviceView {
    id: String,
    name: String,
    created_at: i64,
    last_used_at: i64,
    last_ip: Option<String>,
    /// Only when an admin lists everyone's.
    #[serde(skip_serializing_if = "Option::is_none")]
    user: Option<String>,
}

#[derive(Deserialize)]
struct ListQuery {
    all: Option<String>,
}

async fn list(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Query(query): Query<ListQuery>,
) -> ApiResult<Json<Vec<DeviceView>>> {
    let all = matches!(query.all.as_deref(), Some("1" | "true"));
    if all && user.role != Role::Admin {
        return Err(ApiError::forbidden("admin_only", "this needs an admin"));
    }
    let rows = db::list_devices(&state.db, (!all).then_some(user.id.as_str())).await?;
    Ok(Json(
        rows.into_iter()
            .map(|d| DeviceView {
                id: d.id,
                name: d.name,
                created_at: d.created_at,
                last_used_at: d.last_used_at,
                last_ip: d.last_ip,
                user: all.then_some(d.username),
            })
            .collect(),
    ))
}

async fn revoke(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    client: ClientInfo,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    let device = db::device(&state.db, &id)
        .await?
        .filter(|d| d.user_id == user.id || user.role == Role::Admin)
        .ok_or_else(|| ApiError::NotFound("no such device".into()))?;
    db::delete_device(&state.db, &device.id).await?;
    db::audit(
        &state.db,
        Some(&user.id),
        "device.revoked",
        Some(&device.id),
        Some(json!({ "name": device.name, "owner": device.username })),
        client.ip.as_deref(),
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---- The player (no cookie) ----

fn valid_name(name: &str) -> ApiResult<&str> {
    let name = name.trim();
    if name.is_empty()
        || name.chars().count() > MAX_NAME_CHARS
        || name.chars().any(char::is_control)
    {
        return Err(ApiError::bad_request(
            "invalid_name",
            format!("a device name is 1 to {MAX_NAME_CHARS} characters"),
        ));
    }
    Ok(name)
}

fn valid_install_id(id: &str) -> ApiResult<&str> {
    let ok = !id.is_empty()
        && id.len() <= MAX_INSTALL_ID_CHARS
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
    if ok {
        Ok(id)
    } else {
        Err(ApiError::bad_request(
            "invalid_install_id",
            "the install id is up to 64 letters, digits and dashes",
        ))
    }
}

/// Signs `user_id`'s player in: a fresh token on the (user, install) row.
async fn sign_in(
    state: &AppState,
    user: &User,
    install_id: &str,
    name: &str,
    method: &str,
    client: &ClientInfo,
) -> ApiResult<Json<Value>> {
    let token = new_device_token()?;
    let device_id = db::upsert_device(
        &state.db,
        &user.id,
        install_id,
        name,
        &token_hash(&token),
        client.ip.as_deref(),
    )
    .await?;
    db::audit(
        &state.db,
        Some(&user.id),
        "device.token_issued",
        Some(&device_id),
        Some(json!({ "name": name, "via": method })),
        client.ip.as_deref(),
    )
    .await?;
    Ok(Json(json!({
        "token": token,
        "device_id": device_id,
        "user": { "id": user.id, "username": user.username, "role": user.role },
    })))
}

fn invalid_ticket() -> ApiError {
    ApiError::bad_request("invalid_ticket", "the ticket is unknown, used or expired")
}

#[derive(Deserialize)]
struct TicketSwap {
    ticket: String,
    install_id: String,
    name: String,
}

async fn swap_ticket(
    State(state): State<AppState>,
    client: ClientInfo,
    Json(req): Json<TicketSwap>,
) -> ApiResult<Json<Value>> {
    let install_id = valid_install_id(&req.install_id)?;
    let name = valid_name(&req.name)?;
    let user_id = db::redeem_ticket(&state.db, &token_hash(&req.ticket))
        .await?
        .ok_or_else(invalid_ticket)?;
    let user = db::user_by_id(&state.db, &user_id)
        .await?
        .filter(|u| !u.disabled)
        .ok_or_else(invalid_ticket)?;
    sign_in(&state, &user, install_id, name, "ticket", &client).await
}

#[derive(Deserialize)]
struct CodeRequest {
    install_id: String,
    name: String,
}

async fn start_code(
    State(state): State<AppState>,
    client: ClientInfo,
    Json(req): Json<CodeRequest>,
) -> ApiResult<Json<Value>> {
    let install_id = valid_install_id(&req.install_id)?;
    let name = valid_name(&req.name)?;
    if db::count_pending_device_codes(&state.db).await? >= MAX_PENDING_CODES {
        return Err(ApiError::conflict(
            "too_many_codes",
            "too many sign-ins are waiting; try again in a few minutes",
        ));
    }
    let device_code = auth::random_token()?;
    let mut user_code = None;
    for _ in 0..8 {
        let candidate = new_user_code()?;
        if db::insert_device_code(
            &state.db,
            &token_hash(&device_code),
            &candidate,
            install_id,
            name,
        )
        .await?
        {
            user_code = Some(candidate);
            break;
        }
    }
    let user_code = user_code.ok_or_else(|| anyhow::anyhow!("no free user code"))?;
    db::audit(
        &state.db,
        None,
        "device.code_requested",
        None,
        Some(json!({ "name": name })),
        client.ip.as_deref(),
    )
    .await?;
    Ok(Json(json!({
        "device_code": device_code,
        "user_code": display_user_code(&user_code),
        "verification_path": "/link",
        "expires_in": db::DEVICE_CODE_TTL_SECS,
        "interval": POLL_INTERVAL_SECS,
    })))
}

#[derive(Deserialize)]
struct PollRequest {
    device_code: String,
}

fn oauth(code: &'static str, message: &str) -> ApiError {
    ApiError::bad_request(code, message)
}

async fn poll_code(
    State(state): State<AppState>,
    client: ClientInfo,
    Json(req): Json<PollRequest>,
) -> ApiResult<Json<Value>> {
    let hash = token_hash(&req.device_code);
    let row = db::device_code(&state.db, &hash)
        .await?
        .filter(|r| r.expires_at > db::now())
        .ok_or_else(|| oauth("expired_token", "the code has expired; start again"))?;
    match row.status.as_str() {
        "denied" => Err(oauth("access_denied", "the sign-in was denied")),
        "approved" => {
            let user_id = db::redeem_device_code(&state.db, &hash)
                .await?
                .ok_or_else(|| oauth("expired_token", "the code has already been used"))?;
            let user = db::user_by_id(&state.db, &user_id)
                .await?
                .filter(|u| !u.disabled)
                .ok_or_else(|| oauth("access_denied", "the account is disabled"))?;
            sign_in(&state, &user, &row.install_id, &row.name, "code", &client).await
        }
        _ => {
            let too_fast = row
                .last_poll_at
                .is_some_and(|t| db::now() - t < POLL_INTERVAL_SECS - POLL_SLACK_SECS);
            db::touch_device_code_poll(&state.db, &hash).await?;
            if too_fast {
                Err(oauth("slow_down", "poll every 5 seconds"))
            } else {
                Err(oauth("authorization_pending", "waiting for approval"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_codes_are_typed_loosely() {
        assert_eq!(
            normalize_user_code("bcdf-ghjk").as_deref(),
            Some("BCDFGHJK")
        );
        assert_eq!(
            normalize_user_code(" BCDF GHJK ").as_deref(),
            Some("BCDFGHJK")
        );
        assert_eq!(normalize_user_code("BCDFGHJK").as_deref(), Some("BCDFGHJK"));
        assert_eq!(normalize_user_code("BCDF-GHJ"), None);
        assert_eq!(
            normalize_user_code("BCDF-GHJA"),
            None,
            "vowels aren't in the alphabet"
        );
        assert_eq!(display_user_code("BCDFGHJK"), "BCDF-GHJK");
    }

    #[test]
    fn generated_user_codes_normalise_to_themselves() {
        for _ in 0..50 {
            let code = new_user_code().unwrap();
            assert_eq!(normalize_user_code(&display_user_code(&code)), Some(code));
        }
    }
}

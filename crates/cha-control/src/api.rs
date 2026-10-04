//! `/api/*`: health, first-run setup, sign-in, and admin user management;
//! nodes live in [`crate::nodes`].

use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, Router};
use axum_extra::extract::CookieJar;
use serde::Deserialize;
use serde_json::{Value, json};
use tracing::info;

use crate::AppState;
use crate::auth::{
    self, AdminUser, ClientInfo, CurrentUser, SESSION_COOKIE, check_password_policy, hash_password,
};
use crate::db::{self, Role, User};
use crate::error::{ApiError, ApiResult};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/health", get(health))
        .route("/setup", get(setup_status).post(setup))
        .route("/auth/login", post(login))
        .route("/auth/logout", post(logout))
        .route("/me", get(me))
        .route("/users", get(list_users).post(create_user))
        .route("/audit", get(audit))
        .merge(crate::nodes::routes())
        .merge(crate::environments::routes())
        .merge(crate::ice::routes())
        .fallback(|| async { ApiError::NotFound("no such API".into()) })
}

async fn health() -> Json<Value> {
    Json(json!({ "status": "ok", "version": env!("CARGO_PKG_VERSION") }))
}

async fn setup_status(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    Ok(Json(
        json!({ "needed": db::user_count(&state.db).await? == 0 }),
    ))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SetupRequest {
    token: String,
    username: String,
    display_name: Option<String>,
    password: String,
}

/// Creates the first admin, with the setup token from the server log.
async fn setup(
    State(state): State<AppState>,
    client: ClientInfo,
    jar: CookieJar,
    Json(req): Json<SetupRequest>,
) -> ApiResult<(CookieJar, Json<User>)> {
    let mut setup_token = state.setup_token.lock().await;
    if db::user_count(&state.db).await? > 0 {
        return Err(ApiError::conflict(
            "already_set_up",
            "the portal already has accounts",
        ));
    }
    let expected = setup_token
        .as_deref()
        .ok_or_else(|| ApiError::conflict("already_set_up", "setup isn't open"))?;
    if !constant_time_eq(req.token.trim().as_bytes(), expected.as_bytes()) {
        db::audit(
            &state.db,
            None,
            "setup.bad_token",
            None,
            None,
            client.ip.as_deref(),
        )
        .await?;
        return Err(ApiError::forbidden(
            "bad_setup_token",
            "that setup token isn't the one in the server log",
        ));
    }
    let username = valid_username(&req.username)?;
    check_password_policy(&req.password)?;
    let display_name = req
        .display_name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(username);
    let user = db::insert_user(
        &state.db,
        username,
        display_name,
        &hash_password(&req.password)?,
        Role::Admin,
    )
    .await?;
    *setup_token = None;
    db::audit(
        &state.db,
        Some(&user.id),
        "setup.admin_created",
        Some(&user.id),
        None,
        client.ip.as_deref(),
    )
    .await?;
    info!(username = %user.username, "first admin created; setup is closed");
    let cookie = auth::start_session(&state, &user, &client).await?;
    Ok((jar.add(cookie), Json(user)))
}

#[derive(Deserialize)]
struct LoginRequest {
    username: String,
    password: String,
}

async fn login(
    State(state): State<AppState>,
    client: ClientInfo,
    jar: CookieJar,
    Json(req): Json<LoginRequest>,
) -> ApiResult<(CookieJar, Json<User>)> {
    let found = db::user_for_login(&state.db, req.username.trim()).await?;
    let user = match found {
        Some(row)
            if row
                .password_hash
                .as_deref()
                .is_some_and(|h| auth::verify_password(&req.password, h)) =>
        {
            row.user
        }
        other => {
            if other.is_none() {
                auth::verify_dummy(&req.password);
            }
            db::audit(
                &state.db,
                None,
                "login.failed",
                None,
                Some(json!({ "username": req.username.trim() })),
                client.ip.as_deref(),
            )
            .await?;
            return Err(ApiError::BadRequest(
                "invalid_credentials",
                "wrong username or password".into(),
            ));
        }
    };
    if user.disabled {
        return Err(ApiError::forbidden("disabled", "this account is disabled"));
    }
    let cookie = auth::start_session(&state, &user, &client).await?;
    db::audit(
        &state.db,
        Some(&user.id),
        "login.ok",
        None,
        None,
        client.ip.as_deref(),
    )
    .await?;
    Ok((jar.add(cookie), Json(user)))
}

async fn logout(
    State(state): State<AppState>,
    jar: CookieJar,
    Json(_): Json<Value>,
) -> ApiResult<CookieJar> {
    if let Some(cookie) = jar.get(SESSION_COOKIE) {
        db::delete_session(&state.db, &auth::token_hash(cookie.value())).await?;
    }
    Ok(jar.remove(auth::removal_cookie()))
}

async fn me(CurrentUser(user): CurrentUser) -> Json<User> {
    Json(user)
}

async fn list_users(State(state): State<AppState>, _: AdminUser) -> ApiResult<Json<Vec<User>>> {
    Ok(Json(db::list_users(&state.db).await?))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateUserRequest {
    username: String,
    display_name: Option<String>,
    password: String,
    role: Role,
}

async fn create_user(
    State(state): State<AppState>,
    AdminUser(admin): AdminUser,
    client: ClientInfo,
    Json(req): Json<CreateUserRequest>,
) -> ApiResult<Json<User>> {
    let username = valid_username(&req.username)?;
    check_password_policy(&req.password)?;
    let display_name = req
        .display_name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(username);
    let user = match db::insert_user(
        &state.db,
        username,
        display_name,
        &hash_password(&req.password)?,
        req.role,
    )
    .await
    {
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
            return Err(ApiError::conflict(
                "username_taken",
                "that username is taken",
            ));
        }
        other => other?,
    };
    db::audit(
        &state.db,
        Some(&admin.id),
        "user.created",
        Some(&user.id),
        Some(json!({ "username": user.username, "role": user.role })),
        client.ip.as_deref(),
    )
    .await?;
    Ok(Json(user))
}

async fn audit(
    State(state): State<AppState>,
    _: AdminUser,
) -> ApiResult<Json<Vec<db::AuditEntry>>> {
    Ok(Json(db::recent_audit(&state.db, 200).await?))
}

/// 3–32 characters: letters, digits, `.`, `_`, `-`; starts with a letter or digit.
fn valid_username(name: &str) -> ApiResult<&str> {
    let name = name.trim();
    let ok = (3..=32).contains(&name.len())
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if ok {
        Ok(name)
    } else {
        Err(ApiError::bad_request(
            "bad_username",
            "usernames are 3–32 letters, digits, '.', '_' or '-', starting with a letter or digit",
        ))
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

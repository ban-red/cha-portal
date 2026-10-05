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
        .route("/auth/dev-login", post(dev_login))
        .route("/auth/dev-accounts", get(dev_accounts))
        .route("/auth/logout", post(logout))
        .route("/me", get(me))
        .route("/users", get(list_users).post(create_user))
        .route("/audit", get(audit))
        .merge(crate::nodes::routes())
        .merge(crate::environments::routes())
        .merge(crate::ice::routes())
        .merge(crate::storage::routes())
        .merge(crate::controllers::routes())
        .merge(crate::apps::routes())
        .fallback(|| async { ApiError::NotFound("no such API".into()) })
}

async fn health() -> Json<Value> {
    Json(json!({ "status": "ok", "version": env!("CARGO_PKG_VERSION") }))
}

async fn setup_status(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    Ok(Json(json!({
        "needed": db::user_count(&state.db).await? == 0,
        "devLogin": state.config.dev_login,
    })))
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

const DEV_USERNAME: &str = "dev";

/// Dev login is off without `--dev-login`, and only for loopback clients.
fn require_dev_login(state: &AppState, client: &ClientInfo) -> ApiResult<()> {
    if !state.config.dev_login {
        return Err(ApiError::NotFound("no such API".into()));
    }
    let loopback = client
        .ip
        .as_deref()
        .and_then(|ip| ip.parse::<std::net::IpAddr>().ok())
        .is_some_and(|ip| ip.is_loopback());
    if !loopback {
        return Err(ApiError::forbidden(
            "dev_login_loopback_only",
            "dev login only works from this machine",
        ));
    }
    Ok(())
}

/// The enabled admins other than `dev`, for dev login to sign in as.
async fn dev_accounts(State(state): State<AppState>, client: ClientInfo) -> ApiResult<Json<Value>> {
    require_dev_login(&state, &client)?;
    let mut accounts: Vec<User> = db::list_users(&state.db)
        .await?
        .into_iter()
        .filter(|u| u.role == Role::Admin && !u.disabled && u.username != DEV_USERNAME)
        .collect();
    accounts.sort_by(|a, b| a.username.cmp(&b.username));
    let accounts: Vec<Value> = accounts
        .iter()
        .map(|u| json!({ "username": u.username, "displayName": u.display_name }))
        .collect();
    Ok(Json(json!({ "accounts": accounts })))
}

/// "Login as Local Dev": with `--dev-login`, signs a loopback client in as the
/// `dev` admin, creating it (with an unusable random password) on first use.
/// Closes first-run setup, since the portal then has an account. With a
/// `username`, signs in as that existing, enabled admin instead (never
/// creating it), so local development sees the owner's own data.
async fn dev_login(
    State(state): State<AppState>,
    client: ClientInfo,
    jar: CookieJar,
    Json(req): Json<Value>,
) -> ApiResult<(CookieJar, Json<User>)> {
    require_dev_login(&state, &client)?;
    let target = match req.get("username") {
        None | Some(Value::Null) => None,
        Some(Value::String(name)) => Some(name.trim().to_string()),
        Some(_) => {
            return Err(ApiError::bad_request(
                "invalid_username",
                "username must be a string",
            ));
        }
    };
    if let Some(username) = target.filter(|name| name != DEV_USERNAME) {
        let user = match db::user_for_login(&state.db, &username).await? {
            Some(row) => row.user,
            None => return Err(ApiError::NotFound(format!("no account named {username}"))),
        };
        if user.disabled {
            return Err(ApiError::forbidden("disabled", "this account is disabled"));
        }
        if user.role != Role::Admin {
            return Err(ApiError::forbidden(
                "dev_login_admin_only",
                "dev login only signs in as admins",
            ));
        }
        let cookie = auth::start_session(&state, &user, &client).await?;
        db::audit(
            &state.db,
            Some(&user.id),
            "dev.login_as",
            Some(&user.id),
            Some(json!({ "username": user.username })),
            client.ip.as_deref(),
        )
        .await?;
        db::audit(
            &state.db,
            Some(&user.id),
            "login.dev",
            None,
            None,
            client.ip.as_deref(),
        )
        .await?;
        return Ok((jar.add(cookie), Json(user)));
    }
    let mut setup_token = state.setup_token.lock().await;
    let user = match db::user_for_login(&state.db, DEV_USERNAME).await? {
        Some(row) => row.user,
        None => {
            let user = db::insert_user(
                &state.db,
                DEV_USERNAME,
                "Local Dev",
                &hash_password(&auth::random_token()?)?,
                Role::Admin,
            )
            .await?;
            db::audit(
                &state.db,
                Some(&user.id),
                "dev.user_created",
                Some(&user.id),
                None,
                client.ip.as_deref(),
            )
            .await?;
            info!("created the `dev` admin for dev login");
            user
        }
    };
    *setup_token = None;
    drop(setup_token);
    if user.disabled {
        return Err(ApiError::forbidden("disabled", "this account is disabled"));
    }
    let cookie = auth::start_session(&state, &user, &client).await?;
    db::audit(
        &state.db,
        Some(&user.id),
        "login.dev",
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

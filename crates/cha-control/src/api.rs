//! `/api/*`: health, first-run setup, sign-in, and admin user management;
//! nodes live in [`crate::nodes`].

use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, Router};
use axum_extra::extract::CookieJar;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tracing::info;

use crate::AppState;
use crate::auth::{
    self, Actor, AdminUser, ClientInfo, PlayerUser, SESSION_COOKIE, check_password_policy,
    hash_password,
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
        .route("/auth/switch", post(switch_user))
        .route("/auth/switch-back", post(switch_back))
        .route("/auth/switchable", get(switchable))
        .route("/me", get(me))
        .route("/users", get(list_users).post(create_user))
        .route("/users/{id}", axum::routing::delete(delete_user))
        .route("/audit", get(audit))
        .merge(crate::access::routes())
        .merge(crate::devices::routes())
        .merge(crate::nodes::routes())
        .merge(crate::environments::routes())
        .merge(crate::catalogs::routes())
        .merge(crate::custom::routes())
        .merge(crate::moonlight::routes())
        .merge(crate::gamestream::routes())
        .merge(crate::ice::routes())
        .merge(crate::storage::routes())
        .merge(crate::controllers::routes())
        .merge(crate::apps::routes())
        .merge(crate::prefs::routes())
        .merge(crate::settings::routes())
        .merge(crate::placement::routes())
        .merge(crate::shares::routes())
        .merge(crate::relay::routes())
        .merge(crate::tunnel::routes())
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
    username: String,
    display_name: Option<String>,
    password: String,
}

/// Claims a fresh portal: creates the first admin. Open only while the portal
/// has no accounts; the first request wins.
async fn setup(
    State(state): State<AppState>,
    client: ClientInfo,
    jar: CookieJar,
    Json(req): Json<SetupRequest>,
) -> ApiResult<(CookieJar, Json<User>)> {
    let _claim = state.setup_lock.lock().await;
    if db::user_count(&state.db).await? > 0 {
        return Err(ApiError::conflict(
            "already_set_up",
            "the portal already has accounts",
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
    let found = db::user_for_login_or_email(&state.db, req.username.trim()).await?;
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
/// A request through a proxy on this machine also arrives from loopback, so
/// one that carries a forwarding header is refused too.
fn require_dev_login(state: &AppState, client: &ClientInfo) -> ApiResult<()> {
    if !state.config.dev_login {
        return Err(ApiError::NotFound("no such API".into()));
    }
    let loopback = !client.forwarded
        && client
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
    let claim = state.setup_lock.lock().await;
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
    drop(claim);
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

/// The admin behind a "switch user" session.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Impersonator {
    id: String,
    username: String,
    display_name: String,
}

#[derive(Serialize)]
struct Me {
    #[serde(flatten)]
    user: User,
    impersonator: Option<Impersonator>,
}

/// The signed-in user, and the admin viewing as them if one is. A device
/// token gets the user alone.
async fn me(actor: Result<Actor, ApiError>, PlayerUser(user): PlayerUser) -> Json<Me> {
    let (user, admin) = match actor {
        Ok(actor) => (actor.user, actor.impersonator),
        Err(_) => (user, None),
    };
    Json(Me {
        user,
        impersonator: admin.map(|a| Impersonator {
            id: a.id,
            username: a.username,
            display_name: a.display_name,
        }),
    })
}

/// `POST /api/auth/switch`: an admin starts viewing as another user, for
/// [`auth::IMPERSONATION_HOURS`]. Works from inside such a session too, so
/// the admin moves from one user to the next without going back.
async fn switch_user(
    State(state): State<AppState>,
    actor: Actor,
    client: ClientInfo,
    jar: CookieJar,
    Json(req): Json<SwitchRequest>,
) -> ApiResult<(CookieJar, Json<User>)> {
    let admin = actor.real_admin()?.clone();
    let target = db::user_by_id(&state.db, &req.user_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("no such user".into()))?;
    if target.disabled {
        return Err(ApiError::bad_request(
            "disabled",
            "this account is disabled",
        ));
    }
    let switched = actor.impersonator.is_some();
    if target.id == admin.id && !switched {
        return Err(ApiError::bad_request(
            "already_you",
            "you are already signed in as this user",
        ));
    }
    // Back to the admin is a plain session, not one viewing as themself.
    let cookie = if target.id == admin.id {
        auth::start_session(&state, &target, &client).await?
    } else {
        auth::start_impersonation(&state, &target, &admin, &client).await?
    };
    if switched && let Some(old) = jar.get(SESSION_COOKIE) {
        db::delete_session(&state.db, &auth::token_hash(old.value())).await?;
    }
    db::audit(
        &state.db,
        Some(&admin.id),
        "user.switched",
        Some(&target.id),
        Some(json!({ "username": target.username })),
        client.ip.as_deref(),
    )
    .await?;
    Ok((jar.add(cookie), Json(target)))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SwitchRequest {
    user_id: String,
}

/// `POST /api/auth/switch-back`: ends the view-as session and signs the admin
/// in again.
async fn switch_back(
    State(state): State<AppState>,
    actor: Actor,
    client: ClientInfo,
    jar: CookieJar,
    Json(_): Json<Value>,
) -> ApiResult<(CookieJar, Json<User>)> {
    let Some(admin) = actor.impersonator else {
        return Err(ApiError::bad_request(
            "not_switched",
            "you are not viewing as another user",
        ));
    };
    let cookie = auth::start_session(&state, &admin, &client).await?;
    if let Some(old) = jar.get(SESSION_COOKIE) {
        db::delete_session(&state.db, &auth::token_hash(old.value())).await?;
    }
    db::audit(
        &state.db,
        Some(&admin.id),
        "user.switched_back",
        Some(&actor.user.id),
        Some(json!({ "username": actor.user.username })),
        client.ip.as_deref(),
    )
    .await?;
    Ok((jar.add(cookie), Json(admin)))
}

/// `GET /api/auth/switchable`: who an admin can view as (everyone enabled,
/// the admin included), by display name.
async fn switchable(State(state): State<AppState>, actor: Actor) -> ApiResult<Json<Vec<User>>> {
    actor.real_admin()?;
    let mut users: Vec<User> = db::list_users(&state.db)
        .await?
        .into_iter()
        .filter(|u| !u.disabled)
        .collect();
    users.sort_by_key(|u| u.display_name.to_lowercase());
    Ok(Json(users))
}

async fn list_users(State(state): State<AppState>, _: AdminUser) -> ApiResult<Json<Vec<User>>> {
    Ok(Json(db::list_users(&state.db).await?))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateUserRequest {
    email: String,
    username: Option<String>,
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
    let email = valid_email(&req.email)?;
    let username = valid_username(req.username.as_deref().unwrap_or(&email))?;
    check_password_policy(&req.password)?;
    let display_name = req
        .display_name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(username);
    let user = match db::insert_user_with_email(
        &state.db,
        username,
        Some(&email),
        display_name,
        &hash_password(&req.password)?,
        req.role,
    )
    .await
    {
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
            // The unique index that tripped says which.
            let code = if e.message().contains("email") {
                ("email_taken", "that email address is taken")
            } else {
                ("username_taken", "that username is taken")
            };
            return Err(ApiError::conflict(code.0, code.1));
        }
        other => other?,
    };
    db::audit(
        &state.db,
        Some(&admin.id),
        "user.created",
        Some(&user.id),
        Some(json!({ "username": user.username, "email": user.email, "role": user.role })),
        client.ip.as_deref(),
    )
    .await?;
    Ok(Json(user))
}

async fn delete_user(
    State(state): State<AppState>,
    AdminUser(admin): AdminUser,
    client: ClientInfo,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> ApiResult<axum::http::StatusCode> {
    let _claim = state.setup_lock.lock().await;
    let user = db::user_by_id(&state.db, &id)
        .await?
        .ok_or_else(|| ApiError::NotFound("no such user".into()))?;
    if user.id == admin.id {
        return Err(ApiError::conflict(
            "cannot_delete_self",
            "you can't delete your own account",
        ));
    }
    if user.role == Role::Admin
        && !db::list_users(&state.db)
            .await?
            .iter()
            .any(|u| u.role == Role::Admin && !u.disabled && u.id != user.id)
    {
        return Err(ApiError::conflict(
            "last_admin",
            "this is the only enabled admin",
        ));
    }
    let live = db::count_live_environments(&state.db, &user.id).await?;
    if live > 0 {
        return Err(ApiError::conflict(
            "user_has_environments",
            format!(
                "{} has {live} running environments; stop them first",
                user.username
            ),
        ));
    }
    db::delete_user(&state.db, &user.id).await?;
    db::audit(
        &state.db,
        Some(&admin.id),
        "user.deleted",
        Some(&user.id),
        Some(json!({ "username": user.username, "email": user.email, "role": user.role })),
        client.ip.as_deref(),
    )
    .await?;
    info!(username = %user.username, "user deleted");
    Ok(axum::http::StatusCode::NO_CONTENT)
}

async fn audit(
    State(state): State<AppState>,
    _: AdminUser,
) -> ApiResult<Json<Vec<db::AuditEntry>>> {
    Ok(Json(db::recent_audit(&state.db, 200).await?))
}

/// 3–64 characters, trimmed, no control characters.
fn valid_username(name: &str) -> ApiResult<&str> {
    let name = name.trim();
    let ok = (3..=64).contains(&name.chars().count()) && !name.chars().any(char::is_control);
    if ok {
        Ok(name)
    } else {
        Err(ApiError::bad_request(
            "bad_username",
            "usernames need at least 3 characters (and at most 64)",
        ))
    }
}

/// An address with one `@` and text on both sides, at most 254 characters, no
/// whitespace or control characters; lower-cased.
fn valid_email(email: &str) -> ApiResult<String> {
    let email = email.trim().to_lowercase();
    let ok = email.chars().count() <= 254
        && !email.chars().any(|c| c.is_control() || c.is_whitespace())
        && email.split_once('@').is_some_and(|(local, domain)| {
            !local.is_empty() && !domain.is_empty() && !domain.contains('@')
        });
    if ok {
        Ok(email)
    } else {
        Err(ApiError::bad_request(
            "bad_email",
            "that doesn't look like an email address",
        ))
    }
}

//! Controllers: which virtual gamepad an app gets (`docs/controllers.md`).
//!
//! The kind is the user's per-app choice; until they make one, the app's
//! default applies (`gamepad` in `images/catalog.json`, `xbox360` when the
//! catalog says nothing). A launch carries the effective kind to the node
//! ([`cha_wire::EnvironmentSpec::gamepad`]). Like the other launch-time
//! settings, a change reaches the next launch, not a running environment, so
//! there is no reason to refuse while one is live.

use axum::extract::{Path, State};
use axum::routing::{get, put};
use axum::{Json, Router};
use cha_wire::GamepadKind;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::AppState;
use crate::auth::{ClientInfo, CurrentUser};
use crate::db::{self, Role};
use crate::environments::{Template, catalog, template};
use crate::error::{ApiError, ApiResult};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/controllers/apps", get(list))
        .route("/controllers/apps/{template}", put(set))
}

/// What the catalog says an app gets until the user chooses.
pub fn app_default(template: &Template) -> GamepadKind {
    template.gamepad.unwrap_or_default()
}

/// The controller a launch of `template` by `user_id` gets: their choice, else
/// the app's default.
pub async fn effective_for(
    state: &AppState,
    user_id: &str,
    template: &Template,
) -> ApiResult<GamepadKind> {
    let choice = db::user_gamepads(&state.db, user_id)
        .await?
        .get(&template.id)
        .and_then(|k| GamepadKind::parse(k));
    Ok(choice.unwrap_or_else(|| app_default(template)))
}

#[derive(Serialize)]
struct UserApps {
    apps: Vec<UserApp>,
}

#[derive(Serialize)]
struct UserApp {
    template: String,
    name: String,
    /// The user's choice; `None` while they haven't made one.
    kind: Option<GamepadKind>,
    /// What the app gets for a user who hasn't chosen.
    default: GamepadKind,
}

fn user_app(template: &Template, choice: Option<&String>) -> UserApp {
    UserApp {
        template: template.id.clone(),
        name: template.name.clone(),
        kind: choice.and_then(|k| GamepadKind::parse(k)),
        default: app_default(template),
    }
}

/// `GET /api/controllers/apps`: the signed-in user's controller for every app
/// they can launch (guests can't launch any).
async fn list(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
) -> ApiResult<Json<UserApps>> {
    if user.role == Role::Guest {
        return Ok(Json(UserApps { apps: Vec::new() }));
    }
    let choices = db::user_gamepads(&state.db, &user.id).await?;
    Ok(Json(UserApps {
        apps: catalog()
            .iter()
            .map(|t| user_app(t, choices.get(&t.id)))
            .collect(),
    }))
}

#[derive(Deserialize)]
struct SetKind {
    /// A kind's name, or `null` to go back to the app's default. Required, so
    /// a body that forgot it isn't taken for a reset.
    kind: serde_json::Value,
}

/// `PUT /api/controllers/apps/{template}`: choose the app's controller, or
/// (`null`) clear the choice.
async fn set(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    client: ClientInfo,
    Path(id): Path<String>,
    Json(req): Json<SetKind>,
) -> ApiResult<Json<UserApp>> {
    if user.role == Role::Guest {
        return Err(ApiError::forbidden(
            "guests_cannot_launch",
            "guests can't launch environments, so they have no controller settings",
        ));
    }
    let template = template(&id).ok_or_else(|| ApiError::NotFound("no such template".into()))?;
    let kind = match &req.kind {
        serde_json::Value::Null => None,
        serde_json::Value::String(s) => Some(GamepadKind::parse(s).ok_or_else(|| {
            ApiError::bad_request("bad_kind", "kind is xbox360, dualsense or steam")
        })?),
        _ => {
            return Err(ApiError::bad_request(
                "bad_kind",
                "kind is xbox360, dualsense, steam or null",
            ));
        }
    };
    db::set_user_gamepad(
        &state.db,
        &user.id,
        &template.id,
        kind.map(GamepadKind::as_str),
    )
    .await?;
    db::audit(
        &state.db,
        Some(&user.id),
        "controllers.gamepad_set",
        Some(&template.id),
        Some(json!({ "kind": kind })),
        client.ip.as_deref(),
    )
    .await?;
    Ok(Json(user_app(
        template,
        kind.map(|k| k.as_str().to_string()).as_ref(),
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_catalog_default_is_xbox360_unless_it_says_otherwise() {
        assert_eq!(
            app_default(template("chrome").unwrap()),
            GamepadKind::Xbox360
        );
        let mut t = template("chrome").unwrap().clone();
        t.gamepad = Some(GamepadKind::Dualsense);
        assert_eq!(app_default(&t), GamepadKind::Dualsense);
    }
}

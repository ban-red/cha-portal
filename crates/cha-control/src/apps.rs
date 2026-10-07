//! Per-app settings that aren't about storage or controllers: the frame rate.
//!
//! The rate is the user's per-app choice (60, 90 or 120); until they make one,
//! the app's default applies (`fps` in `images/catalog.json`, 60 when the
//! catalog says nothing). A launch carries the effective rate to the node
//! ([`cha_wire::EnvironmentSpec::fps`]). Like the other launch-time settings,
//! a change reaches the next launch, not a running environment, so there is no
//! reason to refuse while one is live.

use axum::extract::{Path, State};
use axum::routing::{get, put};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::AppState;
use crate::auth::{ClientInfo, CurrentUser, PlayerUser};
use crate::db::{self, Role};
use crate::environments::{Template, catalog, template};
use crate::error::{ApiError, ApiResult};

/// The frame rates a user can choose.
pub const FPS_CHOICES: [u32; 3] = [60, 90, 120];
const DEFAULT_FPS: u32 = 60;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/apps/settings", get(list))
        .route("/apps/settings/{template}", put(set))
}

/// What the catalog says an app gets until the user chooses.
pub fn app_default(template: &Template) -> u32 {
    template
        .fps
        .filter(|f| FPS_CHOICES.contains(f))
        .unwrap_or(DEFAULT_FPS)
}

/// The frame rate a launch of `template` by `user_id` gets: their choice, else
/// the app's default.
pub async fn fps_for(state: &AppState, user_id: &str, template: &Template) -> ApiResult<u32> {
    let choice = db::user_fps(&state.db, user_id)
        .await?
        .get(&template.id)
        .copied()
        .filter(|f| FPS_CHOICES.contains(f));
    Ok(choice.unwrap_or_else(|| app_default(template)))
}

#[derive(Serialize)]
struct UserApps {
    apps: Vec<UserApp>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct UserApp {
    template: String,
    name: String,
    /// The user's choice; `None` while they haven't made one.
    fps: Option<u32>,
    /// What the app gets for a user who hasn't chosen.
    default_fps: u32,
}

fn user_app(template: &Template, choice: Option<u32>) -> UserApp {
    UserApp {
        template: template.id.clone(),
        name: template.name.clone(),
        fps: choice.filter(|f| FPS_CHOICES.contains(f)),
        default_fps: app_default(template),
    }
}

/// `GET /api/apps/settings`: the signed-in user's settings for every app they
/// can launch (guests can't launch any).
async fn list(
    State(state): State<AppState>,
    PlayerUser(user): PlayerUser,
) -> ApiResult<Json<UserApps>> {
    if user.role == Role::Guest {
        return Ok(Json(UserApps { apps: Vec::new() }));
    }
    let choices = db::user_fps(&state.db, &user.id).await?;
    Ok(Json(UserApps {
        apps: catalog()
            .iter()
            .map(|t| user_app(t, choices.get(&t.id).copied()))
            .collect(),
    }))
}

#[derive(Deserialize)]
struct SetFps {
    /// 60, 90 or 120, or `null` to go back to the app's default. Required, so
    /// a body that forgot it isn't taken for a reset.
    fps: serde_json::Value,
}

/// `PUT /api/apps/settings/{template}`: choose the app's frame rate, or
/// (`null`) clear the choice.
async fn set(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    client: ClientInfo,
    Path(id): Path<String>,
    Json(req): Json<SetFps>,
) -> ApiResult<Json<UserApp>> {
    if user.role == Role::Guest {
        return Err(ApiError::forbidden(
            "guests_cannot_launch",
            "guests can't launch environments, so they have no app settings",
        ));
    }
    let template = template(&id).ok_or_else(|| ApiError::NotFound("no such template".into()))?;
    let fps = match &req.fps {
        serde_json::Value::Null => None,
        v => Some(
            v.as_u64()
                .and_then(|n| u32::try_from(n).ok())
                .filter(|n| FPS_CHOICES.contains(n))
                .ok_or_else(|| ApiError::bad_request("bad_fps", "fps is 60, 90, 120 or null"))?,
        ),
    };
    db::set_user_fps(&state.db, &user.id, &template.id, fps).await?;
    db::audit(
        &state.db,
        Some(&user.id),
        "apps.fps_set",
        Some(&template.id),
        Some(json!({ "fps": fps })),
        client.ip.as_deref(),
    )
    .await?;
    Ok(Json(user_app(template, fps)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_catalog_default_is_60_unless_it_says_otherwise() {
        assert_eq!(app_default(template("chrome").unwrap()), 60);
        let mut t = template("chrome").unwrap().clone();
        t.fps = Some(120);
        assert_eq!(app_default(&t), 120);
        t.fps = Some(75);
        assert_eq!(app_default(&t), 60);
    }
}

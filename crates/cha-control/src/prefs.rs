//! Per-user interface preferences: one small JSON object per user.
//!
//! `GET /api/me/prefs` returns `{ "prefs": {…} }` (empty when nothing is
//! stored) and `PUT /api/me/prefs` replaces the object whole. Any signed-in
//! user may use them, guests included. They are cosmetic, so no audit entry.
//! Keys and values are checked here; the list of theme ids lives only in the
//! client, which falls back to its default on an id it doesn't know.
//! Besides the theme keys: `pinned` (the app templates the user pinned),
//! `envView` (grid or list) and `envSort` (name or recent) for the Environments page.

use axum::extract::{DefaultBodyLimit, State};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{Map, Value, json};

use crate::AppState;
use crate::auth::CurrentUser;
use crate::db;
use crate::error::{ApiError, ApiResult};

/// The most the stored object may take, as compact JSON.
pub const MAX_PREFS_BYTES: usize = 4 * 1024;
/// Hard cap on the request body, so a huge one is refused (413) unread.
const MAX_BODY_BYTES: usize = 16 * 1024;

pub fn routes() -> Router<AppState> {
    Router::new().route(
        "/me/prefs",
        get(read)
            .put(write)
            .layer(DefaultBodyLimit::max(MAX_BODY_BYTES)),
    )
}

async fn read(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
) -> ApiResult<Json<Value>> {
    let prefs = db::user_prefs(&state.db, &user.id)
        .await?
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}));
    Ok(Json(json!({ "prefs": prefs })))
}

async fn write(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Json(req): Json<Value>,
) -> ApiResult<Json<Value>> {
    let prefs = validate(req.get("prefs").unwrap_or(&Value::Null))?;
    let text = serde_json::to_string(&prefs).map_err(|e| ApiError::Internal(e.into()))?;
    if text.len() > MAX_PREFS_BYTES {
        return Err(ApiError::bad_request(
            "prefs_too_large",
            format!("preferences take at most {MAX_PREFS_BYTES} bytes"),
        ));
    }
    db::set_user_prefs(&state.db, &user.id, &text).await?;
    Ok(Json(json!({ "prefs": prefs })))
}

fn one_of(key: &str, v: &Value, allowed: &[&str]) -> ApiResult<()> {
    match v.as_str() {
        Some(s) if allowed.contains(&s) => Ok(()),
        _ => Err(ApiError::bad_request(
            "bad_pref",
            format!("{key} is one of: {}", allowed.join(", ")),
        )),
    }
}

/// A template id, as the catalog spells them: a-z, 0-9 and -, up to 40 characters.
fn is_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 40
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// The most apps a user may pin.
const MAX_PINNED: usize = 64;

/// Checks a preferences object and returns it as it will be stored.
fn validate(v: &Value) -> ApiResult<Map<String, Value>> {
    let Some(obj) = v.as_object() else {
        return Err(ApiError::bad_request(
            "bad_prefs",
            "prefs must be a JSON object",
        ));
    };
    for (key, value) in obj {
        match key.as_str() {
            "theme" => {
                if !value.as_str().is_some_and(is_id) {
                    return Err(ApiError::bad_request(
                        "bad_pref",
                        "theme is a string of a-z, 0-9 and - up to 40 characters",
                    ));
                }
            }
            "appearance" => one_of(key, value, &["system", "dark", "light"])?,
            "contrast" => one_of(key, value, &["system", "standard", "more"])?,
            "motion" => one_of(key, value, &["system", "reduced"])?,
            "transparency" => one_of(key, value, &["system", "reduced"])?,
            "envView" => one_of(key, value, &["grid", "list"])?,
            "envSort" => one_of(key, value, &["name", "recent"])?,
            "pinned" => {
                let ok = value.as_array().is_some_and(|a| {
                    let ids: Vec<&str> = a.iter().filter_map(Value::as_str).collect();
                    let unique = ids.iter().collect::<std::collections::HashSet<_>>().len();
                    a.len() <= MAX_PINNED
                        && ids.len() == a.len()
                        && unique == ids.len()
                        && ids.iter().all(|s| is_id(s))
                });
                if !ok {
                    return Err(ApiError::bad_request(
                        "bad_pref",
                        format!(
                            "pinned is a list of up to {MAX_PINNED} different app ids (a-z, 0-9 and -)"
                        ),
                    ));
                }
            }
            other => {
                return Err(ApiError::bad_request(
                    "unknown_pref",
                    format!(
                        "unknown preference: {}",
                        other.chars().take(40).collect::<String>()
                    ),
                ));
            }
        }
    }
    Ok(obj.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_keys_are_checked() {
        assert!(validate(&json!({})).is_ok());
        assert!(validate(&json!({"theme": "cha-jade", "appearance": "dark"})).is_ok());
        assert!(validate(&json!({"theme": "Cha Jade"})).is_err());
        assert!(validate(&json!({"theme": "a".repeat(41)})).is_err());
        assert!(validate(&json!({"contrast": "high"})).is_err());
        assert!(validate(&json!({"extra": 1})).is_err());
        assert!(validate(&json!([])).is_err());
    }

    #[test]
    fn environments_page_keys_are_checked() {
        assert!(
            validate(
                &json!({"envView": "list", "envSort": "recent", "pinned": ["chrome", "steam-2"]})
            )
            .is_ok()
        );
        assert!(validate(&json!({"pinned": []})).is_ok());
        assert!(validate(&json!({"envView": "table"})).is_err());
        assert!(validate(&json!({"envSort": "size"})).is_err());
        assert!(validate(&json!({"pinned": "chrome"})).is_err());
        assert!(validate(&json!({"pinned": ["Chrome"]})).is_err());
        assert!(validate(&json!({"pinned": ["a", "a"]})).is_err());
        assert!(validate(&json!({"pinned": [1]})).is_err());
        let many: Vec<String> = (0..65).map(|i| format!("app-{i}")).collect();
        assert!(validate(&json!({ "pinned": many })).is_err());
        assert!(validate(&json!({ "pinned": &many[..64] })).is_ok());
    }
}

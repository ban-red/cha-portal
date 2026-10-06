//! Per-user interface preferences: one small JSON object per user.
//!
//! `GET /api/me/prefs` returns `{ "prefs": {…} }` (empty when nothing is
//! stored) and `PUT /api/me/prefs` replaces the object whole. Any signed-in
//! user may use them, guests included. They are cosmetic, so no audit entry.
//! Keys and values are checked here; the list of theme ids lives only in the
//! client, which falls back to its default on an id it doesn't know.

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
                let ok = value.as_str().is_some_and(|s| {
                    !s.is_empty()
                        && s.len() <= 40
                        && s.bytes()
                            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
                });
                if !ok {
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
}

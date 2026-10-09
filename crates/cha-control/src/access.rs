//! What an admin lets a user do beyond their role: which nodes they launch on,
//! how many environments they run at once, and single templates on nodes they
//! otherwise can't use (grants). Enforced in [`crate::placement`] and
//! [`crate::environments`].

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{delete, get};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::AppState;
use crate::auth::{AdminUser, ClientInfo, CurrentUser};
use crate::db::{self, Role, UserGrant};
use crate::environments::MAX_LIVE_PER_USER;
use crate::error::{ApiError, ApiResult};

/// The most environments a user can be allowed at once.
const MAX_INSTANCES_LIMIT: i64 = 64;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/users/{id}/access", get(access).put(set_access))
        .route("/users/{id}/grants", axum::routing::post(add_grant))
        .route("/users/{id}/grants/{grant_id}", delete(remove_grant))
        .route("/me/grants", get(my_grants))
}

async fn find_user(state: &AppState, id: &str) -> ApiResult<db::User> {
    db::user_by_id(&state.db, id)
        .await?
        .ok_or_else(|| ApiError::NotFound("no such user".into()))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Access {
    node_restricted: bool,
    node_ids: Vec<String>,
    max_instances: Option<i64>,
    effective_max: i64,
    live: i64,
    grants: Vec<UserGrant>,
}

async fn access(
    State(state): State<AppState>,
    _: AdminUser,
    Path(id): Path<String>,
) -> ApiResult<Json<Access>> {
    let user = find_user(&state, &id).await?;
    Ok(Json(Access {
        node_restricted: user.node_restricted,
        node_ids: db::user_node_ids(&state.db, &id).await?,
        max_instances: user.max_instances,
        effective_max: user.max_instances.unwrap_or(MAX_LIVE_PER_USER),
        live: db::count_live_environments(&state.db, &id).await?,
        grants: db::user_grants(&state.db, &id).await?,
    }))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SetAccess {
    node_restricted: bool,
    node_ids: Vec<String>,
    max_instances: Option<i64>,
}

async fn set_access(
    State(state): State<AppState>,
    AdminUser(admin): AdminUser,
    client: ClientInfo,
    Path(id): Path<String>,
    Json(req): Json<SetAccess>,
) -> ApiResult<Json<Access>> {
    find_user(&state, &id).await?;
    if let Some(max) = req.max_instances
        && !(1..=MAX_INSTANCES_LIMIT).contains(&max)
    {
        return Err(ApiError::bad_request(
            "bad_max_instances",
            format!("the limit is between 1 and {MAX_INSTANCES_LIMIT} environments"),
        ));
    }
    let mut node_ids = req.node_ids;
    node_ids.sort();
    node_ids.dedup();
    for node_id in &node_ids {
        if db::node_by_id(&state.db, node_id).await?.is_none() {
            return Err(ApiError::bad_request(
                "unknown_node",
                format!("no such node: {node_id}"),
            ));
        }
    }
    db::set_user_access(
        &state.db,
        &id,
        req.node_restricted,
        &node_ids,
        req.max_instances,
    )
    .await?;
    db::audit(
        &state.db,
        Some(&admin.id),
        "user.access_set",
        Some(&id),
        Some(json!({
            "nodeRestricted": req.node_restricted,
            "nodeIds": node_ids,
            "maxInstances": req.max_instances,
        })),
        client.ip.as_deref(),
    )
    .await?;
    access(State(state), AdminUser(admin), Path(id)).await
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AddGrant {
    node_id: String,
    template_id: String,
}

async fn add_grant(
    State(state): State<AppState>,
    AdminUser(admin): AdminUser,
    client: ClientInfo,
    Path(id): Path<String>,
    Json(req): Json<AddGrant>,
) -> ApiResult<Json<UserGrant>> {
    find_user(&state, &id).await?;
    let template_id = req.template_id.trim();
    if template_id.is_empty() {
        return Err(ApiError::bad_request(
            "bad_template",
            "a grant needs a template",
        ));
    }
    if db::node_by_id(&state.db, &req.node_id).await?.is_none() {
        return Err(ApiError::bad_request("unknown_node", "no such node"));
    }
    let grant = db::add_user_grant(&state.db, &id, &req.node_id, template_id, &admin.id).await?;
    db::audit(
        &state.db,
        Some(&admin.id),
        "user.grant_added",
        Some(&id),
        Some(json!({ "nodeId": grant.node_id, "templateId": grant.template_id })),
        client.ip.as_deref(),
    )
    .await?;
    Ok(Json(grant))
}

async fn remove_grant(
    State(state): State<AppState>,
    AdminUser(admin): AdminUser,
    client: ClientInfo,
    Path((id, grant_id)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    find_user(&state, &id).await?;
    let grant = db::user_grants(&state.db, &id)
        .await?
        .into_iter()
        .find(|g| g.id == grant_id)
        .ok_or_else(|| ApiError::NotFound("no such grant".into()))?;
    db::remove_user_grant(&state.db, &id, &grant_id).await?;
    db::audit(
        &state.db,
        Some(&admin.id),
        "user.grant_removed",
        Some(&id),
        Some(json!({ "nodeId": grant.node_id, "templateId": grant.template_id })),
        client.ip.as_deref(),
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct MyGrant {
    id: String,
    node_id: String,
    node_name: String,
    online: bool,
    template_id: String,
}

/// `GET /api/me/grants`: the templates on nodes that an admin shared with the
/// signed-in user.
async fn my_grants(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
) -> ApiResult<Json<Vec<MyGrant>>> {
    if user.role == Role::Guest {
        return Ok(Json(Vec::new()));
    }
    let mut out = Vec::new();
    for (grant, node_name) in db::user_grants_with_nodes(&state.db, &user.id).await? {
        out.push(MyGrant {
            online: state.nodes.connected_since(&grant.node_id).await.is_some(),
            id: grant.id,
            node_id: grant.node_id,
            node_name,
            template_id: grant.template_id,
        });
    }
    Ok(Json(out))
}

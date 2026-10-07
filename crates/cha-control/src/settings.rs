//! Portal-wide settings an admin sets: `GET` and `PUT /api/admin/settings`.
//!
//! The idle shutoff: a running environment nobody is using is stopped after
//! `idleShutdownMinutes` (default 30; 0 turns it off). Once a minute the portal
//! asks each running environment's streamer for its `/info`. The streamer keeps
//! the clock (`idle_secs`: seconds since someone last used it, counted from its
//! start if nobody came), so a short visit between two checks still resets it
//! and a portal restart changes nothing. Someone using it is a browser tab
//! that is visible or playing sound (a hidden, silent tab doesn't count) or a
//! native or Moonlight client, which counts while connected.
//!
//! A streamer that predates `idle_secs` only reports `viewers` (every
//! connection, hidden tabs included); the portal times those by its own
//! samples, in memory. One that reports neither is never stopped, nor is an
//! environment whose node is offline or whose streamer doesn't answer: each is
//! logged once as a warning, again only after it was judged fine and fails
//! anew. External Moonlight hosts can't be asked and are left alone.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use cha_wire::{NodeRequest, NodeResponse};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tracing::{info, warn};

use crate::AppState;
use crate::auth::{AdminUser, ClientInfo};
use crate::db;
use crate::environments::stop_on_node;
use crate::error::{ApiError, ApiResult};

const IDLE_KEY: &str = "idle_shutdown_minutes";
pub const DEFAULT_IDLE_MINUTES: u32 = 30;
/// A week: past this "off" is the better setting.
const MAX_IDLE_MINUTES: u32 = 7 * 24 * 60;
const CHECK_EVERY: Duration = Duration::from_secs(60);
const INFO_TIMEOUT: Duration = Duration::from_secs(10);

pub fn routes() -> Router<AppState> {
    Router::new().route("/admin/settings", get(read).put(write))
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Settings {
    /// Minutes an environment nobody uses keeps running; 0: never stop it.
    idle_shutdown_minutes: u32,
}

/// The idle period in minutes (0: off).
pub async fn idle_minutes(db: &sqlx::SqlitePool) -> u32 {
    db::setting(db, IDLE_KEY)
        .await
        .ok()
        .flatten()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_IDLE_MINUTES)
}

async fn read(State(state): State<AppState>, _: AdminUser) -> ApiResult<Json<Settings>> {
    Ok(Json(Settings {
        idle_shutdown_minutes: idle_minutes(&state.db).await,
    }))
}

async fn write(
    State(state): State<AppState>,
    AdminUser(admin): AdminUser,
    client: ClientInfo,
    Json(req): Json<Settings>,
) -> ApiResult<Json<Settings>> {
    if req.idle_shutdown_minutes > MAX_IDLE_MINUTES {
        return Err(ApiError::bad_request(
            "bad_idle_shutdown",
            "the idle shutoff is at most a week (10080 minutes); 0 turns it off",
        ));
    }
    db::set_setting(&state.db, IDLE_KEY, &req.idle_shutdown_minutes.to_string()).await?;
    db::audit(
        &state.db,
        Some(&admin.id),
        "settings.updated",
        None,
        Some(json!({ "idleShutdownMinutes": req.idle_shutdown_minutes })),
        client.ip.as_deref(),
    )
    .await?;
    Ok(Json(req))
}

/// What one check decides about a running environment.
#[derive(Debug, PartialEq)]
enum Verdict {
    /// Someone is using it, or it hasn't been idle long enough. The portal's
    /// clock for it (if any) is dropped.
    Keep,
    /// Idle, but not for the whole period yet: keep (or start) the portal's clock.
    Timing,
    /// Idle for the whole period; the seconds are what we saw.
    Stop { idle_secs: u64 },
    /// The streamer reports nothing to judge by.
    Unknown,
}

/// Decides from a streamer's `/info`. A streamer that keeps its own clock
/// (`idle_secs`) is believed as is, so a portal restart changes nothing; an
/// older one that only counts connections (`viewers`) is timed by the portal's
/// samples (`clock`: when it was first seen with none); a very old one that
/// reports neither is never stopped.
fn verdict(
    info: &serde_json::Value,
    clock: Option<Instant>,
    now: Instant,
    limit: Duration,
) -> Verdict {
    if let Some(idle) = info.get("idle_secs").and_then(|v| v.as_u64()) {
        return if idle >= limit.as_secs() {
            Verdict::Stop { idle_secs: idle }
        } else {
            Verdict::Keep
        };
    }
    match info.get("viewers").and_then(|v| v.as_u64()) {
        None => Verdict::Unknown,
        Some(n) if n > 0 => Verdict::Keep,
        Some(_) => {
            let idle = now.saturating_duration_since(clock.unwrap_or(now));
            if idle >= limit {
                Verdict::Stop {
                    idle_secs: idle.as_secs(),
                }
            } else {
                Verdict::Timing
            }
        }
    }
}

/// Stops running environments nobody has used for the idle period.
pub async fn idle_loop(state: AppState) {
    let mut clocks = Clocks::default();
    loop {
        tokio::time::sleep(CHECK_EVERY).await;
        check_idle(&state, &mut clocks).await;
    }
}

/// What the idle shutoff remembers between checks.
#[derive(Default)]
pub struct Clocks {
    /// When each environment of an older streamer was first seen with no
    /// viewers.
    idle_since: HashMap<String, Instant>,
    /// Environments already warned about, so a problem is logged once, not
    /// every minute; cleared when one is judged again.
    warned: HashSet<String>,
}

impl Clocks {
    /// Logs why `id` can't be judged, the first time only.
    fn cannot_judge(&mut self, id: &str, why: &str) {
        if self.warned.insert(id.to_string()) {
            warn!(
                environment = id,
                "the idle shutoff can't stop this environment: {why}"
            );
        }
    }
}

/// One pass over the running environments.
pub async fn check_idle(state: &AppState, clocks: &mut Clocks) {
    let minutes = idle_minutes(&state.db).await;
    if minutes == 0 {
        *clocks = Clocks::default();
        return;
    }
    let limit = Duration::from_secs(u64::from(minutes) * 60);
    let running = match db::running_environments(&state.db).await {
        Ok(envs) => envs,
        Err(err) => {
            warn!("idle shutoff: listing environments: {err}");
            return;
        }
    };
    // External Moonlight hosts can't be asked.
    let running: Vec<_> = running
        .into_iter()
        .filter(|e| !e.template_id.starts_with("moonlight:"))
        .collect();
    clocks
        .idle_since
        .retain(|id, _| running.iter().any(|e| &e.id == id));
    clocks
        .warned
        .retain(|id| running.iter().any(|e| &e.id == id));
    for env in running {
        let Some(node_id) = env.node_id.clone() else {
            clocks.cannot_judge(&env.id, "it has no node");
            continue;
        };
        let reply = state
            .nodes
            .request_timeout(
                &node_id,
                NodeRequest::StreamerInfo {
                    environment_id: env.id.clone(),
                },
                INFO_TIMEOUT,
            )
            .await;
        let info = match reply {
            Ok(NodeResponse::StreamerInfo { info }) => info,
            Ok(_) => {
                clocks.cannot_judge(&env.id, "its node answered something else");
                continue;
            }
            Err(err) => {
                clocks.cannot_judge(&env.id, &format!("its streamer can't be asked: {err}"));
                continue;
            }
        };
        let now = Instant::now();
        match verdict(&info, clocks.idle_since.get(&env.id).copied(), now, limit) {
            Verdict::Unknown => {
                clocks.idle_since.remove(&env.id);
                clocks.cannot_judge(
                    &env.id,
                    "its streamer is too old to report viewers (update the streamer image)",
                );
            }
            Verdict::Keep => {
                clocks.idle_since.remove(&env.id);
                clocks.warned.remove(&env.id);
            }
            Verdict::Timing => {
                clocks.idle_since.entry(env.id.clone()).or_insert(now);
                clocks.warned.remove(&env.id);
            }
            Verdict::Stop { idle_secs } => {
                clocks.idle_since.remove(&env.id);
                clocks.warned.remove(&env.id);
                stop_idle(
                    state,
                    &env.id,
                    &env.template_id,
                    &node_id,
                    minutes,
                    idle_secs,
                )
                .await;
            }
        }
    }
}

async fn stop_idle(
    state: &AppState,
    id: &str,
    template: &str,
    node_id: &str,
    minutes: u32,
    idle_secs: u64,
) {
    let detail = format!("stopped: nobody used it for {minutes} min");
    match db::transition_environment(&state.db, id, &["running"], "stopping", Some(&detail)).await {
        Ok(true) => {}
        Ok(false) => return,
        Err(err) => {
            warn!("idle shutoff: {err}");
            return;
        }
    }
    info!(
        environment = id,
        minutes, idle_secs, "stopping an idle environment"
    );
    let _ = db::audit(
        &state.db,
        None,
        "environment.idle_stopped",
        Some(id),
        Some(json!({ "template": template, "idleMinutes": minutes, "idleSeconds": idle_secs })),
        None,
    )
    .await;
    tokio::spawn(stop_on_node(
        state.clone(),
        node_id.to_string(),
        id.to_string(),
        Some(detail),
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMIT: Duration = Duration::from_secs(30 * 60);

    fn at(now: Instant, ago_secs: u64) -> Option<Instant> {
        Some(now - Duration::from_secs(ago_secs))
    }

    #[test]
    fn idle_secs_decides_alone() {
        let now = Instant::now();
        let over = json!({ "idle_secs": 1800, "viewers": 2, "active_viewers": 0 });
        assert_eq!(
            verdict(&over, None, now, LIMIT),
            Verdict::Stop { idle_secs: 1800 }
        );
        let under = json!({ "idle_secs": 1799, "viewers": 0 });
        assert_eq!(verdict(&under, at(now, 99_999), now, LIMIT), Verdict::Keep);
        let active = json!({ "idle_secs": 0, "viewers": 1, "active_viewers": 1 });
        assert_eq!(verdict(&active, at(now, 99_999), now, LIMIT), Verdict::Keep);
    }

    #[test]
    fn viewers_alone_use_the_portals_clock() {
        let now = Instant::now();
        let none = json!({ "viewers": 0 });
        assert_eq!(verdict(&none, None, now, LIMIT), Verdict::Timing);
        assert_eq!(verdict(&none, at(now, 600), now, LIMIT), Verdict::Timing);
        assert_eq!(
            verdict(&none, at(now, 1800), now, LIMIT),
            Verdict::Stop { idle_secs: 1800 }
        );
        let some = json!({ "viewers": 1 });
        assert_eq!(verdict(&some, at(now, 99_999), now, LIMIT), Verdict::Keep);
    }

    #[test]
    fn a_streamer_that_reports_nothing_is_never_stopped() {
        let now = Instant::now();
        assert_eq!(
            verdict(&json!({}), at(now, 99_999), now, LIMIT),
            Verdict::Unknown
        );
        // Wrong types count as absent.
        let odd = json!({ "idle_secs": "soon", "viewers": null });
        assert_eq!(verdict(&odd, None, now, LIMIT), Verdict::Unknown);
    }

    #[test]
    fn a_problem_is_warned_about_once_until_it_clears() {
        let mut clocks = Clocks::default();
        clocks.cannot_judge("a", "x");
        assert!(clocks.warned.contains("a"));
        // The second time inserts nothing new.
        assert!(!clocks.warned.insert("a".into()));
    }
}

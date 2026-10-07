//! What a paired Moonlight client sees and launches: the running environments
//! of the portal user it belongs to on this node. Launching reserves nothing
//! new, since each environment's streamer already has its port block; the
//! media of a session is given to that streamer, which serves it.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

use bytes::Bytes;
use cha_gamestream::{
    App, ClientId, Directory, DirectoryError, HostHandle, LaunchRequest, MediaPorts,
    PairingAttempt, PinWaiter, ResumeRequest, SessionHandoff, SessionTarget,
};
use futures_util::future::BoxFuture;
use hyper::Method;
use tracing::{debug, info, warn};

use super::pairing::Pairings;
use super::streamer;
use crate::environments::{DockerRuntime, GameStreamAccess, RunningEnvironment};

/// What the host needs to know about this node's environments.
pub trait Environments: Send + Sync + 'static {
    /// The environments running now, with their owners.
    fn running(&self) -> Vec<RunningEnvironment>;
}

impl Environments for DockerRuntime {
    fn running(&self) -> Vec<RunningEnvironment> {
        self.running_environments()
    }
}

/// How often a started session is asked whether its media still runs.
pub const POLL: Duration = Duration::from_secs(2);

/// The app id Moonlight sees for an environment: stable for as long as it
/// runs (FNV-1a of its id), positive and never 0, which clients take to mean
/// "nothing".
pub fn app_id(environment_id: &str) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in environment_id.bytes() {
        hash = (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193);
    }
    (hash & 0x7fff_ffff).max(1)
}

/// Template id → its name in the catalog this agent was built with.
fn template_name(template: &str) -> String {
    static NAMES: OnceLock<HashMap<String, String>> = OnceLock::new();
    NAMES
        .get_or_init(|| {
            let catalog: serde_json::Value =
                serde_json::from_str(include_str!("../../../../images/catalog.json"))
                    .unwrap_or_default();
            catalog["templates"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|t| {
                    Some((
                        t["id"].as_str()?.to_string(),
                        t["name"].as_str()?.to_string(),
                    ))
                })
                .collect()
        })
        .get(template)
        .cloned()
        .unwrap_or_else(|| template.to_string())
}

/// A started session, for the host to stop.
struct Active {
    client: ClientId,
    access: GameStreamAccess,
}

pub struct NodeDirectory {
    /// Itself, for a session's watcher to hold.
    me: Weak<NodeDirectory>,
    environments: Arc<dyn Environments>,
    pairings: Arc<Pairings>,
    handle: OnceLock<HostHandle>,
    /// Session id → what runs it.
    active: Mutex<BTreeMap<u64, Active>>,
    poll: Duration,
}

impl NodeDirectory {
    pub fn new(
        environments: Arc<dyn Environments>,
        pairings: Arc<Pairings>,
        poll: Duration,
    ) -> Arc<Self> {
        Arc::new_cyclic(|me| Self {
            me: me.clone(),
            environments,
            pairings,
            handle: OnceLock::new(),
            active: Mutex::default(),
            poll,
        })
    }

    /// The running host, which is told when a session's media ends by itself.
    pub fn set_handle(&self, handle: HostHandle) {
        let _ = self.handle.set(handle);
    }

    /// The client's owner's running environments.
    fn of(&self, client: &ClientId) -> Vec<RunningEnvironment> {
        let Some(user) = self.pairings.user_of(client) else {
            return Vec::new();
        };
        self.environments
            .running()
            .into_iter()
            .filter(|e| !e.gateway && !e.owner.is_empty() && e.owner == user)
            .collect()
    }

    /// The environment `app_id` names, if the client's owner has it running.
    fn find(&self, client: &ClientId, app_id_: u32) -> Result<RunningEnvironment, DirectoryError> {
        self.of(client)
            .into_iter()
            .find(|e| app_id(&e.id) == app_id_)
            .ok_or(DirectoryError::NoSuchApp)
    }

    fn target(&self, client: &ClientId, app: u32) -> Result<SessionTarget, DirectoryError> {
        let environment = self.find(client, app)?;
        let access = environment.gamestream.ok_or_else(|| {
            DirectoryError::Refused("restart the environment so it can stream to Moonlight".into())
        })?;
        Ok(SessionTarget {
            media_ports: MediaPorts {
                video: access.ports.video,
                control: access.ports.control,
                audio: access.ports.audio,
            },
        })
    }

    /// Asks `access`'s streamer to stop `session_id`; one that is gone is fine.
    async fn stop(&self, session_id: u64, access: &GameStreamAccess) {
        let path = format!("/gamestream/session/{session_id}");
        match streamer::request(access, Method::DELETE, &path, None).await {
            Ok(reply) if matches!(reply.status, 200 | 404) => {
                debug!(session_id, "stopped a Moonlight session's media");
            }
            Ok(reply) => warn!(
                session_id,
                status = reply.status,
                "stopping media: {}",
                reply.body
            ),
            Err(err) => warn!(session_id, "stopping media: {err:#}"),
        }
    }

    /// Watches a session the streamer runs and tells the host when it ends
    /// (the client left, or the streamer did).
    fn watch(&self, session_id: u64, access: GameStreamAccess) {
        let poll = self.poll;
        let me = self.me.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(poll).await;
                let Some(this) = me.upgrade() else { return };
                if !this
                    .active
                    .lock()
                    .expect("active lock")
                    .contains_key(&session_id)
                {
                    return;
                }
                if streamer::running_session(&access).await.ok().flatten() != Some(session_id) {
                    info!(session_id, "a Moonlight session's media ended");
                    this.active.lock().expect("active lock").remove(&session_id);
                    if let Some(handle) = this.handle.get() {
                        handle.media_ended(session_id);
                    }
                    return;
                }
            }
        });
    }
}

impl Directory for NodeDirectory {
    fn apps(&self, client: &ClientId) -> BoxFuture<'_, Vec<App>> {
        let mut apps: Vec<App> = self
            .of(client)
            .into_iter()
            .map(|e| App {
                id: app_id(&e.id),
                title: template_name(&e.template),
                hdr: false,
            })
            .collect();
        apps.sort_by(|a, b| a.title.cmp(&b.title).then(a.id.cmp(&b.id)));
        Box::pin(async move { apps })
    }

    fn app_image(&self, _client: &ClientId, _app_id: u32) -> BoxFuture<'_, Option<Bytes>> {
        Box::pin(async { None })
    }

    fn launch(
        &self,
        client: &ClientId,
        request: LaunchRequest,
    ) -> BoxFuture<'_, Result<SessionTarget, DirectoryError>> {
        let target = self.target(client, request.app_id);
        Box::pin(async move { target })
    }

    fn resume(
        &self,
        client: &ClientId,
        request: ResumeRequest,
    ) -> BoxFuture<'_, Result<SessionTarget, DirectoryError>> {
        let target = self.target(client, request.app_id);
        Box::pin(async move { target })
    }

    fn start_media(&self, handoff: SessionHandoff) -> BoxFuture<'_, Result<(), DirectoryError>> {
        Box::pin(async move {
            let client = handoff.params.client.clone();
            let environment = self.find(&client, handoff.params.app_id)?;
            let access = environment.gamestream.ok_or_else(|| {
                DirectoryError::Refused("this environment has no Moonlight ports".into())
            })?;
            let id = handoff.session_id;
            let body =
                serde_json::to_vec(&handoff).map_err(|e| DirectoryError::Failed(e.to_string()))?;
            let reply = streamer::request(&access, Method::POST, "/gamestream/session", Some(body))
                .await
                .map_err(|e| DirectoryError::Failed(format!("{e:#}")))?;
            match reply.status {
                200 => {}
                401 => {
                    return Err(DirectoryError::Failed(
                        "the streamer refused the node's secret".into(),
                    ));
                }
                409 | 422 => return Err(DirectoryError::Refused(reply.body)),
                other => {
                    return Err(DirectoryError::Failed(format!(
                        "the streamer answered {other}: {}",
                        reply.body
                    )));
                }
            }
            info!(session_id = id, environment = %environment.id, "a Moonlight session's media started");
            self.active.lock().expect("active lock").insert(
                id,
                Active {
                    client,
                    access: access.clone(),
                },
            );
            self.watch(id, access);
            Ok(())
        })
    }

    fn stop_media(&self, session_id: u64) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            let active = self.active.lock().expect("active lock").remove(&session_id);
            if let Some(active) = active {
                self.stop(session_id, &active.access).await;
            }
        })
    }

    /// The client quit: its media stops. The environment is the user's and
    /// goes on running (they stop it in the portal).
    fn cancel(&self, client: &ClientId) -> BoxFuture<'_, ()> {
        let mine: Vec<(u64, Active)> = {
            let mut active = self.active.lock().expect("active lock");
            let ids: Vec<u64> = active
                .iter()
                .filter(|(_, a)| a.client == *client)
                .map(|(id, _)| *id)
                .collect();
            ids.into_iter()
                .filter_map(|id| active.remove(&id).map(|a| (id, a)))
                .collect()
        };
        Box::pin(async move {
            for (id, active) in mine {
                self.stop(id, &active.access).await;
            }
        })
    }

    fn pin_for(&self, attempt: PairingAttempt) -> PinWaiter {
        self.pairings.begin(attempt)
    }
}

/// A fake streamer API on localhost, for this module's tests and the
/// loopback test (`tests/gamestream.rs`).
#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use axum::extract::{Path, Request, State};
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    use axum::routing::{delete, get, post};
    use axum::{Json, Router};
    use cha_gamestream::handoff::Chroma;
    use cha_gamestream::{AudioParams, Encryption, SessionKeys, StreamParams, VideoCodec};
    use cha_wire::GameStreamDevice;

    use super::*;
    use crate::environments::GameStreamPorts;

    #[derive(Default)]
    struct Fake {
        /// (method, path, authorization, body)
        calls: Mutex<Vec<(String, String, String, String)>>,
        /// What a start answers.
        start: Mutex<Option<StatusCode>>,
        /// The session the streamer says it runs.
        running: Mutex<Option<u64>>,
    }

    impl Fake {
        fn called(&self, method: &str) -> Vec<(String, String)> {
            self.calls
                .lock()
                .unwrap()
                .iter()
                .filter(|c| c.0 == method)
                .map(|c| (c.1.clone(), c.2.clone()))
                .collect()
        }
    }

    async fn record(State(fake): State<Arc<Fake>>, request: Request) -> Request {
        let (parts, body) = request.into_parts();
        let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
        fake.calls.lock().unwrap().push((
            parts.method.to_string(),
            parts.uri.path().to_string(),
            parts
                .headers
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_string(),
            String::from_utf8_lossy(&bytes).into_owned(),
        ));
        Request::from_parts(parts, axum::body::Body::empty())
    }

    /// A streamer API that records what it is asked.
    async fn streamer(secret: &str) -> (GameStreamAccess, Arc<Fake>) {
        let fake = Arc::new(Fake::default());
        let app = Router::new()
            .route(
                "/gamestream/session",
                post(|State(fake): State<Arc<Fake>>, request: Request| async move {
                    record(State(fake.clone()), request).await;
                    let status = fake.start.lock().unwrap().unwrap_or(StatusCode::OK);
                    (status, Json(serde_json::json!({ "error": "refused for a reason" })))
                        .into_response()
                }),
            )
            .route(
                "/gamestream/session/{id}",
                delete(
                    |State(fake): State<Arc<Fake>>, Path(_id): Path<u64>, request: Request| async move {
                        record(State(fake), request).await;
                        StatusCode::OK
                    },
                ),
            )
            .route(
                "/gamestream/status",
                get(|State(fake): State<Arc<Fake>>, request: Request| async move {
                    record(State(fake.clone()), request).await;
                    Json(serde_json::json!({
                        "running": fake.running.lock().unwrap().is_some(),
                        "session_id": *fake.running.lock().unwrap(),
                    }))
                }),
            )
            .with_state(fake.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (
            GameStreamAccess {
                http_port: port,
                ports: GameStreamPorts {
                    video: 7700,
                    control: 7701,
                    audio: 7702,
                },
                secret: secret.into(),
            },
            fake,
        )
    }

    struct Environments(Mutex<Vec<RunningEnvironment>>);

    impl super::Environments for Environments {
        fn running(&self) -> Vec<RunningEnvironment> {
            self.0.lock().unwrap().clone()
        }
    }

    fn env(
        id: &str,
        owner: &str,
        template: &str,
        access: Option<&GameStreamAccess>,
    ) -> RunningEnvironment {
        RunningEnvironment {
            id: id.into(),
            owner: owner.into(),
            template: template.into(),
            gateway: false,
            gamestream: access.cloned(),
        }
    }

    fn client(n: u8) -> ClientId {
        ClientId(format!("{n:02x}").repeat(32))
    }

    struct Rig {
        directory: Arc<NodeDirectory>,
        environments: Arc<Environments>,
    }

    /// A directory where `bob` has client 1 and `alice` has client 2.
    fn rig(environments: Vec<RunningEnvironment>) -> Rig {
        let (out, _rx) = tokio::sync::broadcast::channel(8);
        let pairings = Pairings::new(out, None);
        let device = |n: u8, user: &str| GameStreamDevice {
            fingerprint: client(n).fingerprint().into(),
            unique_id: None,
            name: format!("device {n}"),
            user_id: user.into(),
        };
        pairings.set_devices(vec![device(1, "bob"), device(2, "alice")]);
        let environments = Arc::new(Environments(Mutex::new(environments)));
        Rig {
            directory: NodeDirectory::new(
                environments.clone(),
                pairings,
                Duration::from_millis(30),
            ),
            environments,
        }
    }

    fn launch_request(app_id: u32) -> LaunchRequest {
        LaunchRequest {
            app_id,
            width: 1920,
            height: 1080,
            fps: 60,
            hdr: false,
            surround_audio_info: 0x30002,
            local_audio: false,
            optimize_game_settings: false,
            gamepad_mask: 0,
        }
    }

    fn handoff(session_id: u64, client: &ClientId, app_id: u32) -> SessionHandoff {
        SessionHandoff {
            session_id,
            keys: SessionKeys {
                key: [7; 16],
                key_id: 1,
            },
            encryption: Encryption {
                control: true,
                video: false,
                audio: false,
            },
            control_connect_data: 5,
            ping_payload: [9; 16],
            params: StreamParams {
                client: client.clone(),
                client_ip: "192.168.1.9".parse().unwrap(),
                app_id,
                width: 1920,
                height: 1080,
                fps: 60,
                bitrate_bps: 20_000_000,
                codec: VideoCodec::H264,
                hdr: false,
                chroma: Chroma::Yuv420,
                full_range: false,
                max_ref_frames: 1,
                packet_size: 1392,
                fec_percent: 20,
                min_fec_packets: 0,
                audio: AudioParams {
                    channels: 2,
                    channel_mask: 3,
                    high_quality: false,
                    streams: 1,
                    coupled_streams: 1,
                    mapping: vec![0, 1],
                    packet_duration_ms: 5,
                    bitrate: 96_000,
                },
            },
        }
    }

    #[tokio::test]
    async fn a_client_sees_only_its_owners_running_environments() {
        let (access, _) = streamer("s").await;
        let mut gateway = env("e-gw", "bob", "moonlight:abc:1", None);
        gateway.gateway = true;
        let rig = rig(vec![
            env("e-chrome", "bob", "chrome", Some(&access)),
            env("e-xfce", "bob", "xfce", None),
            env("e-custom", "bob", "not-in-the-catalog", Some(&access)),
            env("e-other", "alice", "firefox", Some(&access)),
            env("e-nobody", "", "chrome", Some(&access)),
            gateway,
        ]);
        let directory = &rig.directory;
        let titles = |apps: Vec<App>| apps.into_iter().map(|a| a.title).collect::<Vec<_>>();
        assert_eq!(
            titles(directory.apps(&client(1)).await),
            ["Google Chrome", "XFCE desktop", "not-in-the-catalog"],
            "catalog names, the template id as the fallback; no gateways, no one else's"
        );
        assert_eq!(titles(directory.apps(&client(2)).await), ["Firefox"]);
        assert!(
            directory.apps(&client(3)).await.is_empty(),
            "an unpaired certificate sees nothing"
        );
        // Ids are stable, distinct and never 0; a stopped environment drops out.
        let apps = directory.apps(&client(1)).await;
        let ids: Vec<u32> = apps.iter().map(|a| a.id).collect();
        assert_eq!(ids.len(), 3);
        assert!(ids.iter().all(|&i| i >= 1 && i <= i32::MAX as u32));
        assert_eq!(
            ids.iter().collect::<std::collections::HashSet<_>>().len(),
            3
        );
        assert_eq!(ids[0], app_id("e-chrome"));
        rig.environments
            .0
            .lock()
            .unwrap()
            .retain(|e| e.id != "e-chrome");
        assert_eq!(
            titles(directory.apps(&client(1)).await),
            ["XFCE desktop", "not-in-the-catalog"]
        );
    }

    #[tokio::test]
    async fn app_ids_are_a_fixed_function_of_the_environment() {
        // Pinned: clients remember apps by id, so the function mustn't drift.
        assert_eq!(app_id(""), 0x011c_9dc5);
        assert_eq!(app_id("a"), 0xe40c_292c & 0x7fff_ffff);
        assert!(app_id("018f-anything") >= 1);
    }

    #[tokio::test]
    async fn launching_needs_the_clients_own_running_environment_with_a_block() {
        let (access, _) = streamer("s").await;
        let rig = rig(vec![
            env("e-mine", "bob", "chrome", Some(&access)),
            env("e-old", "bob", "xfce", None),
            env("e-hers", "alice", "firefox", Some(&access)),
        ]);
        let d = &rig.directory;
        let target = d
            .launch(&client(1), launch_request(app_id("e-mine")))
            .await
            .unwrap();
        assert_eq!(
            target.media_ports,
            MediaPorts {
                video: 7700,
                control: 7701,
                audio: 7702
            }
        );
        // A resume answers the same way.
        let again = d
            .resume(
                &client(1),
                ResumeRequest {
                    app_id: app_id("e-mine"),
                    surround_audio_info: 0,
                },
            )
            .await
            .unwrap();
        assert_eq!(again, target);
        // Not one that started before GameStream was on.
        match d.launch(&client(1), launch_request(app_id("e-old"))).await {
            Err(DirectoryError::Refused(why)) => {
                assert!(why.contains("restart the environment"), "{why}");
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
        // Not someone else's, an unknown one, or an unpaired client's.
        for (who, app) in [
            (1, app_id("e-hers")),
            (1, 999),
            (2, app_id("e-mine")),
            (3, app_id("e-mine")),
        ] {
            assert_eq!(
                d.launch(&client(who), launch_request(app)).await,
                Err(DirectoryError::NoSuchApp),
                "client {who}, app {app}"
            );
        }
        assert_eq!(
            d.resume(
                &client(2),
                ResumeRequest {
                    app_id: app_id("e-mine"),
                    surround_audio_info: 0
                }
            )
            .await,
            Err(DirectoryError::NoSuchApp)
        );
        // Gone once it stops.
        rig.environments.0.lock().unwrap().clear();
        assert_eq!(
            d.launch(&client(1), launch_request(app_id("e-mine"))).await,
            Err(DirectoryError::NoSuchApp)
        );
    }

    #[tokio::test]
    async fn media_goes_to_the_environments_streamer_with_its_secret_and_stops_there() {
        let (access, fake) = streamer("the-secret").await;
        let rig = rig(vec![env("e-mine", "bob", "chrome", Some(&access))]);
        let d = &rig.directory;
        let app = app_id("e-mine");

        d.start_media(handoff(7, &client(1), app)).await.unwrap();
        let posts = fake.called("POST");
        assert_eq!(
            posts,
            [(
                "/gamestream/session".to_string(),
                "Bearer the-secret".to_string()
            )]
        );
        let sent: SessionHandoff = serde_json::from_str(&fake.calls.lock().unwrap()[0].3).unwrap();
        assert_eq!(
            (sent.session_id, sent.params.client, sent.params.app_id),
            (7, client(1), app)
        );

        d.stop_media(7).await;
        assert_eq!(
            fake.called("DELETE"),
            [(
                "/gamestream/session/7".to_string(),
                "Bearer the-secret".to_string()
            )]
        );
        // Stopping what is stopped asks nobody.
        d.stop_media(7).await;
        assert_eq!(fake.called("DELETE").len(), 1);
    }

    #[tokio::test]
    async fn a_session_for_someone_elses_environment_is_refused_before_the_streamer_hears() {
        let (access, fake) = streamer("s").await;
        let rig = rig(vec![env("e-hers", "alice", "firefox", Some(&access))]);
        let refused = rig
            .directory
            .start_media(handoff(1, &client(1), app_id("e-hers")))
            .await;
        assert_eq!(refused, Err(DirectoryError::NoSuchApp));
        assert!(fake.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn the_streamers_refusals_are_told_as_they_are() {
        let (access, fake) = streamer("s").await;
        let rig = rig(vec![env("e-mine", "bob", "chrome", Some(&access))]);
        let app = app_id("e-mine");
        for (status, refused) in [
            (StatusCode::UNPROCESSABLE_ENTITY, true),
            (StatusCode::CONFLICT, true),
            (StatusCode::UNAUTHORIZED, false),
            (StatusCode::INTERNAL_SERVER_ERROR, false),
        ] {
            *fake.start.lock().unwrap() = Some(status);
            let result = rig.directory.start_media(handoff(1, &client(1), app)).await;
            match (result, refused) {
                (Err(DirectoryError::Refused(_)), true)
                | (Err(DirectoryError::Failed(_)), false) => {}
                (other, _) => panic!("{status}: {other:?}"),
            }
        }
        // None of them left a session to stop.
        rig.directory.stop_media(1).await;
        assert!(fake.called("DELETE").is_empty());
    }

    #[tokio::test]
    async fn cancelling_stops_that_clients_media_and_leaves_the_environment_running() {
        let (access, fake) = streamer("s").await;
        let (other_access, other_fake) = streamer("t").await;
        let rig = rig(vec![
            env("e-mine", "bob", "chrome", Some(&access)),
            env("e-hers", "alice", "firefox", Some(&other_access)),
        ]);
        let d = &rig.directory;
        d.start_media(handoff(1, &client(1), app_id("e-mine")))
            .await
            .unwrap();
        d.start_media(handoff(2, &client(2), app_id("e-hers")))
            .await
            .unwrap();

        d.cancel(&client(1)).await;
        assert_eq!(
            fake.called("DELETE"),
            [("/gamestream/session/1".to_string(), "Bearer s".to_string())]
        );
        assert!(
            other_fake.called("DELETE").is_empty(),
            "another client's media runs on"
        );
        // The environment is still there, and still launchable.
        assert_eq!(rig.environments.0.lock().unwrap().len(), 2);
        assert!(
            d.launch(&client(1), launch_request(app_id("e-mine")))
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn a_session_whose_media_ended_is_forgotten() {
        let (access, fake) = streamer("s").await;
        let rig = rig(vec![env("e-mine", "bob", "chrome", Some(&access))]);
        let d = &rig.directory;
        *fake.running.lock().unwrap() = Some(4);
        d.start_media(handoff(4, &client(1), app_id("e-mine")))
            .await
            .unwrap();
        // It runs while the streamer says so.
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(d.active.lock().unwrap().contains_key(&4));
        // The client leaves: the streamer's session is over.
        *fake.running.lock().unwrap() = None;
        for _ in 0..100 {
            if d.active.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
        assert!(d.active.lock().unwrap().is_empty());
        // Nothing left to stop.
        d.stop_media(4).await;
        assert!(fake.called("DELETE").is_empty());
    }
}

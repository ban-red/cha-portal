//! What a paired Moonlight client sees and launches, as with Sunshine: the
//! apps its owner can run on this node ([`super::catalog`]). Launching one
//! resumes the owner's running environment of that template, or has the portal
//! start one on this node and waits for it. The media of a session is given to
//! that environment's streamer, which already has its port block and serves it.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

use bytes::Bytes;
use cha_gamestream::{
    App, ClientId, Directory, DirectoryError, HostHandle, LaunchRequest, MediaPorts,
    PairingAttempt, PinWaiter, ResumeRequest, SessionHandoff, SessionTarget,
};
use cha_wire::{PortalRequest, PortalResponse};
use futures_util::future::BoxFuture;
use hyper::Method;
use tokio::time::Instant;
use tracing::{debug, info, warn};

use super::catalog;
pub use super::catalog::app_id;
use super::pairing::Pairings;
use super::portal::{Failure, PortalLink};
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
/// How long a launch waits for an environment to start: Moonlight's launch
/// request waits for it, and the portal gives up a little sooner so that its
/// reason arrives first.
pub const LAUNCH_WAIT: Duration = Duration::from_secs(180);
/// How often a launch looks for its environment among the running ones.
const LAUNCH_POLL: Duration = Duration::from_millis(250);
/// How long the portal has to take a stop.
const STOP_WAIT: Duration = Duration::from_secs(30);

/// What the directory is tuned by.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// How often a session's media is asked whether it still runs.
    pub poll: Duration,
    /// How long a launch of an app that isn't running waits for it.
    pub launch_wait: Duration,
    /// Quitting an app a Moonlight launch started also stops its environment
    /// (`CHA_GAMESTREAM_QUIT_STOPS`); otherwise a quit ends only the stream.
    pub quit_stops: bool,
    /// This node has a GPU, so apps that need one are offered.
    pub gpu: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            poll: POLL,
            launch_wait: LAUNCH_WAIT,
            quit_stops: false,
            gpu: true,
        }
    }
}

/// A started session, for the host to stop.
struct Active {
    client: ClientId,
    access: GameStreamAccess,
}

/// What the directory remembers of launches. In memory: an agent restart
/// forgets which environments Moonlight started, so quitting one of them
/// afterwards ends only the stream.
#[derive(Default)]
struct Launches {
    /// The environment each client's current app runs in.
    current: HashMap<ClientId, String>,
    /// Environments a Moonlight launch started (rather than found running):
    /// the only ones a quit may stop.
    started: HashSet<String>,
}

pub struct NodeDirectory {
    /// Itself, for a session's watcher and a launch to hold.
    me: Weak<NodeDirectory>,
    environments: Arc<dyn Environments>,
    pairings: Arc<Pairings>,
    portal: Arc<PortalLink>,
    handle: OnceLock<HostHandle>,
    /// Session id → what runs it.
    active: Mutex<BTreeMap<u64, Active>>,
    launches: Mutex<Launches>,
    options: Options,
}

impl NodeDirectory {
    pub fn new(
        environments: Arc<dyn Environments>,
        pairings: Arc<Pairings>,
        portal: Arc<PortalLink>,
        options: Options,
    ) -> Arc<Self> {
        Arc::new_cyclic(|me| Self {
            me: me.clone(),
            environments,
            pairings,
            portal,
            handle: OnceLock::new(),
            active: Mutex::default(),
            launches: Mutex::default(),
            options,
        })
    }

    /// The running host, which is told when a session's media ends by itself.
    pub fn set_handle(&self, handle: HostHandle) {
        let _ = self.handle.set(handle);
    }

    /// The portal user a paired client belongs to.
    fn user_of(&self, client: &ClientId) -> Result<String, DirectoryError> {
        self.pairings
            .user_of(client)
            .ok_or(DirectoryError::NoSuchApp)
    }

    /// `user`'s running environments, gateways and unowned ones aside.
    fn running_of(&self, user: &str) -> Vec<RunningEnvironment> {
        self.environments
            .running()
            .into_iter()
            .filter(|e| !e.gateway && !e.owner.is_empty() && e.owner == user)
            .collect()
    }

    /// The apps `user` can launch here, as (template id, name): what the
    /// node can run, and whatever of theirs runs now that the catalog (this
    /// agent's may be older) doesn't list.
    fn offered(&self, user: &str) -> Vec<(String, String)> {
        // A portal that can't start environments leaves only what runs.
        let mut apps: Vec<(String, String)> = if self.portal.launches() {
            catalog::offered(self.options.gpu)
                .map(|t| (t.id.clone(), t.name.clone()))
                .collect()
        } else {
            Vec::new()
        };
        for e in self.running_of(user) {
            if !apps.iter().any(|(id, _)| *id == e.template) {
                apps.push((e.template.clone(), catalog::name(&e.template)));
            }
        }
        apps
    }

    /// The template `app` names among what `user` is offered.
    fn template_of(&self, user: &str, app: u32) -> Result<String, DirectoryError> {
        self.offered(user)
            .into_iter()
            .map(|(id, _)| id)
            .find(|id| app_id(id) == app)
            .ok_or(DirectoryError::NoSuchApp)
    }

    /// The environment `user` runs `template` in: one with a Moonlight port
    /// block first, then the oldest (ids sort by time).
    fn running_template(&self, user: &str, template: &str) -> Option<RunningEnvironment> {
        self.running_of(user)
            .into_iter()
            .filter(|e| e.template == template)
            .min_by_key(|e| (e.gamestream.is_none(), e.id.clone()))
    }

    /// The running environment the client's `app` is, if there is one.
    fn find(
        &self,
        client: &ClientId,
        app: u32,
    ) -> Result<Option<RunningEnvironment>, DirectoryError> {
        let user = self.user_of(client)?;
        let template = self.template_of(&user, app)?;
        Ok(self.running_template(&user, &template))
    }

    /// Where `environment` serves a session's media.
    fn target_of(environment: &RunningEnvironment) -> Result<SessionTarget, DirectoryError> {
        let access = environment.gamestream.as_ref().ok_or_else(|| {
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

    /// Where `client`'s `app` streams from: its owner's running environment of
    /// it, started first when none runs.
    async fn target(&self, client: &ClientId, app: u32) -> Result<SessionTarget, DirectoryError> {
        let user = self.user_of(client)?;
        let template = self.template_of(&user, app)?;
        let environment = match self.running_template(&user, &template) {
            Some(running) => running,
            None => self.start(user, template).await?,
        };
        let target = Self::target_of(&environment)?;
        self.launches
            .lock()
            .expect("launches lock")
            .current
            .insert(client.clone(), environment.id);
        Ok(target)
    }

    /// Has the portal start `template` on this node for `user` and waits until
    /// it runs here with its Moonlight ports. The request goes on in its own
    /// task: a client that gives up waiting doesn't leave an environment the
    /// directory forgot it started.
    async fn start(
        &self,
        user: String,
        template: String,
    ) -> Result<RunningEnvironment, DirectoryError> {
        let this = self
            .me
            .upgrade()
            .ok_or_else(|| DirectoryError::Failed("the host is shutting down".into()))?;
        tokio::spawn(async move { this.start_and_wait(user, template).await })
            .await
            .map_err(|e| DirectoryError::Failed(e.to_string()))?
    }

    async fn start_and_wait(
        &self,
        user: String,
        template: String,
    ) -> Result<RunningEnvironment, DirectoryError> {
        let deadline = Instant::now() + self.options.launch_wait;
        self.forget_ended();
        info!(%user, %template, "a Moonlight launch asks the portal to start it");
        let request = PortalRequest::GameStreamLaunch {
            user_id: user.clone(),
            template_id: template,
        };
        let (id, created) = match self.portal.ask(request, self.options.launch_wait).await {
            Ok(PortalResponse::GameStreamLaunched {
                environment_id,
                created,
            }) => (environment_id, created),
            Ok(other) => {
                return Err(DirectoryError::Failed(format!(
                    "the portal answered something else: {other:?}"
                )));
            }
            Err(Failure::Said(why)) => return Err(DirectoryError::Refused(why)),
            Err(other) => return Err(DirectoryError::Failed(other.to_string())),
        };
        if created {
            self.launches
                .lock()
                .expect("launches lock")
                .started
                .insert(id.clone());
        }
        // The portal says it runs; this node's runtime has it as soon as its
        // own start finished, which it has.
        loop {
            if let Some(found) = self.running_of(&user).into_iter().find(|e| e.id == id) {
                return Ok(found);
            }
            if Instant::now() >= deadline {
                return Err(DirectoryError::Failed(
                    "the environment didn't come up in time; try again".into(),
                ));
            }
            tokio::time::sleep(LAUNCH_POLL).await;
        }
    }

    /// Drops what the launches remember of environments that no longer run.
    fn forget_ended(&self) {
        let running: HashSet<String> = self
            .environments
            .running()
            .into_iter()
            .map(|e| e.id)
            .collect();
        self.launches
            .lock()
            .expect("launches lock")
            .started
            .retain(|id| running.contains(id));
    }

    /// Asks the portal to stop an environment a Moonlight launch started.
    async fn stop_environment(&self, user: String, environment_id: String) {
        let request = PortalRequest::GameStreamStop {
            user_id: user,
            environment_id: environment_id.clone(),
        };
        match self.portal.ask(request, STOP_WAIT).await {
            Ok(_) => {
                info!(environment = %environment_id, "a Moonlight quit stopped its environment")
            }
            Err(err) => warn!(environment = %environment_id, "stopping it after a quit: {err}"),
        }
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
        let poll = self.options.poll;
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
            .user_of(client)
            .map(|user| self.offered(&user))
            .unwrap_or_default()
            .into_iter()
            .map(|(id, title)| App {
                id: app_id(&id),
                title,
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
        let client = client.clone();
        Box::pin(async move { self.target(&client, request.app_id).await })
    }

    fn resume(
        &self,
        client: &ClientId,
        request: ResumeRequest,
    ) -> BoxFuture<'_, Result<SessionTarget, DirectoryError>> {
        let client = client.clone();
        Box::pin(async move { self.target(&client, request.app_id).await })
    }

    fn start_media(&self, handoff: SessionHandoff) -> BoxFuture<'_, Result<(), DirectoryError>> {
        Box::pin(async move {
            let client = handoff.params.client.clone();
            let environment = self
                .find(&client, handoff.params.app_id)?
                .ok_or(DirectoryError::NoSuchApp)?;
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
    /// goes on running (they stop it in the portal), unless the node is set to
    /// stop what a Moonlight launch started ([`Options::quit_stops`]).
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
        // Its app is over either way; whether the environment goes with it.
        let quit = {
            let mut launches = self.launches.lock().expect("launches lock");
            let environment = launches.current.remove(client);
            environment.filter(|id| self.options.quit_stops && launches.started.remove(id))
        };
        let user = self.pairings.user_of(client);
        Box::pin(async move {
            for (id, active) in mine {
                self.stop(id, &active.access).await;
            }
            if let (Some(environment), Some(user)) = (quit, user) {
                self.stop_environment(user, environment).await;
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
    use cha_wire::{GameStreamDevice, ToPortal};

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
        portal: Arc<FakePortal>,
    }

    /// What the fake portal does with a launch request.
    #[derive(Clone)]
    enum Mode {
        /// Answers `Launched` for `env` and, `after` a moment, the node's
        /// runtime has it running.
        Start {
            env: RunningEnvironment,
            created: bool,
            after: Duration,
        },
        /// Answers `Launched` for an environment that never appears.
        Ghost,
        Refuse(String),
        Silent,
    }

    /// The portal on the other end of the host's channel: records what it is
    /// asked and answers as `mode` says (stops are always taken).
    struct FakePortal {
        asked: Mutex<Vec<PortalRequest>>,
        mode: Mutex<Mode>,
    }

    impl FakePortal {
        fn asked(&self) -> Vec<PortalRequest> {
            self.asked.lock().unwrap().clone()
        }

        fn stops(&self) -> Vec<(String, String)> {
            self.asked()
                .into_iter()
                .filter_map(|r| match r {
                    PortalRequest::GameStreamStop {
                        user_id,
                        environment_id,
                    } => Some((user_id, environment_id)),
                    _ => None,
                })
                .collect()
        }
    }

    fn rig(environments: Vec<RunningEnvironment>) -> Rig {
        rig_with(environments, Options::default())
    }

    /// A directory where `bob` has client 1 and `alice` has client 2, with a
    /// fake portal connected (until `Rig::portal_gone`).
    fn rig_with(environments: Vec<RunningEnvironment>, options: Options) -> Rig {
        let (out, mut rx) = tokio::sync::broadcast::channel(8);
        let pairings = Pairings::new(out.clone(), None);
        let device = |n: u8, user: &str| GameStreamDevice {
            fingerprint: client(n).fingerprint().into(),
            unique_id: None,
            name: format!("device {n}"),
            user_id: user.into(),
        };
        pairings.set_devices(vec![device(1, "bob"), device(2, "alice")]);
        let environments = Arc::new(Environments(Mutex::new(environments)));
        let link = Arc::new(PortalLink::new(out));
        link.set_launches(true);
        let portal = Arc::new(FakePortal {
            asked: Mutex::default(),
            mode: Mutex::new(Mode::Silent),
        });
        {
            let (link, portal, environments) = (link.clone(), portal.clone(), environments.clone());
            tokio::spawn(async move {
                while let Ok(msg) = rx.recv().await {
                    let ToPortal::Request { id, request } = msg else {
                        continue;
                    };
                    portal.asked.lock().unwrap().push(request.clone());
                    let mode = portal.mode.lock().unwrap().clone();
                    let answer = match (&request, mode) {
                        (PortalRequest::GameStreamStop { .. }, _) => {
                            Some(Ok(PortalResponse::GameStreamStopped))
                        }
                        (
                            PortalRequest::GameStreamLaunch { .. },
                            Mode::Start {
                                env,
                                created,
                                after,
                            },
                        ) => {
                            let environment_id = env.id.clone();
                            let environments = environments.clone();
                            tokio::spawn(async move {
                                tokio::time::sleep(after).await;
                                environments.0.lock().unwrap().push(env);
                            });
                            Some(Ok(PortalResponse::GameStreamLaunched {
                                environment_id,
                                created,
                            }))
                        }
                        (PortalRequest::GameStreamLaunch { .. }, Mode::Ghost) => {
                            Some(Ok(PortalResponse::GameStreamLaunched {
                                environment_id: "e-ghost".into(),
                                created: true,
                            }))
                        }
                        (PortalRequest::GameStreamLaunch { .. }, Mode::Refuse(why)) => {
                            Some(Err(why))
                        }
                        _ => None,
                    };
                    if let Some(answer) = answer {
                        link.answer(id, answer);
                    }
                }
            });
        }
        Rig {
            directory: NodeDirectory::new(environments.clone(), pairings, link, options),
            environments,
            portal,
        }
    }

    impl Rig {
        fn mode(&self, mode: Mode) {
            *self.portal.mode.lock().unwrap() = mode;
        }
    }

    /// Short waits and polls, for tests of what happens when time runs out.
    fn quick() -> Options {
        Options {
            poll: Duration::from_millis(30),
            launch_wait: Duration::from_millis(400),
            ..Options::default()
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

    fn titles(apps: Vec<App>) -> Vec<String> {
        apps.into_iter().map(|a| a.title).collect()
    }

    #[tokio::test]
    async fn a_client_sees_what_its_owner_can_run_here_running_or_not() {
        let (access, _) = streamer("s").await;
        let mut gateway = env("e-gw", "bob", "moonlight:abc:1", None);
        gateway.gateway = true;
        let rig = rig(vec![
            env("e-chrome", "bob", "chrome", Some(&access)),
            env("e-custom", "bob", "not-in-the-catalog", Some(&access)),
            env("e-other", "alice", "other-custom", Some(&access)),
            env("e-nobody", "", "nobodys-custom", Some(&access)),
            gateway,
        ]);
        let directory = &rig.directory;
        let bob = titles(directory.apps(&client(1)).await);
        let catalog: Vec<String> = catalog::offered(true).map(|t| t.name.clone()).collect();
        for name in &catalog {
            assert!(bob.contains(name), "{name} is missing from {bob:?}");
        }
        assert!(
            bob.contains(&"not-in-the-catalog".to_string()),
            "his own running custom one"
        );
        assert_eq!(
            bob.len(),
            catalog.len() + 1,
            "no gateways, no one else's: {bob:?}"
        );
        assert!(bob.windows(2).all(|w| w[0] <= w[1]), "sorted by title");
        let alice = titles(directory.apps(&client(2)).await);
        assert!(
            alice.contains(&"other-custom".to_string())
                && !bob.contains(&"other-custom".to_string())
        );
        assert!(
            directory.apps(&client(3)).await.is_empty(),
            "an unpaired certificate sees nothing"
        );
        // Ids belong to templates: a running environment is its template's app.
        let apps = directory.apps(&client(1)).await;
        let chrome = apps.iter().find(|a| a.title == "Google Chrome").unwrap();
        assert_eq!(chrome.id, app_id("chrome"));
        rig.environments.0.lock().unwrap().clear();
        let after = directory.apps(&client(1)).await;
        assert!(
            after.iter().any(|a| a.id == chrome.id),
            "stopped, still listed, same id"
        );
        assert_eq!(after.len(), catalog.len(), "only the custom one drops out");
    }

    #[tokio::test]
    async fn a_portal_that_cannot_launch_leaves_only_the_running_environments() {
        let (access, _) = streamer("s").await;
        let rig = rig(vec![env("e-chrome", "bob", "chrome", Some(&access))]);
        rig.directory.portal.set_launches(false);
        assert_eq!(
            titles(rig.directory.apps(&client(1)).await),
            ["Google Chrome"]
        );
        assert!(
            rig.directory
                .launch(&client(1), launch_request(app_id("chrome")))
                .await
                .is_ok()
        );
        // Anything else isn't an app, and the portal is never asked.
        assert_eq!(
            rig.directory
                .launch(&client(1), launch_request(app_id("firefox")))
                .await,
            Err(DirectoryError::NoSuchApp)
        );
        rig.directory.cancel(&client(1)).await;
        assert!(rig.portal.asked().is_empty());
    }

    #[tokio::test]
    async fn what_needs_a_gpu_is_not_listed_on_a_node_without_one() {
        let options = Options {
            gpu: false,
            ..Options::default()
        };
        let rig = rig_with(Vec::new(), options);
        let bob = titles(rig.directory.apps(&client(1)).await);
        assert!(!bob.contains(&"Steam".to_string()) && bob.contains(&"Google Chrome".to_string()));
        assert_eq!(
            rig.directory
                .launch(&client(1), launch_request(app_id("steam")))
                .await,
            Err(DirectoryError::NoSuchApp),
            "and can't be launched either"
        );
        assert!(rig.portal.asked().is_empty());
    }

    #[tokio::test]
    async fn launching_a_running_template_resumes_it_and_asks_nobody() {
        let (access, _) = streamer("s").await;
        let rig = rig(vec![
            env("e-mine", "bob", "chrome", Some(&access)),
            env("e-old", "bob", "xfce", None),
            env("e-hers", "alice", "other-custom", Some(&access)),
        ]);
        let d = &rig.directory;
        let target = d
            .launch(&client(1), launch_request(app_id("chrome")))
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
                    app_id: app_id("chrome"),
                    surround_audio_info: 0,
                },
            )
            .await
            .unwrap();
        assert_eq!(again, target);
        // Not one that started before GameStream was on.
        match d.launch(&client(1), launch_request(app_id("xfce"))).await {
            Err(DirectoryError::Refused(why)) => {
                assert!(why.contains("restart the environment"), "{why}");
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
        // Not an app that isn't offered: someone else's custom one, an
        // unknown id, or an unpaired client's.
        for (who, app) in [(1, app_id("other-custom")), (1, 999), (3, app_id("chrome"))] {
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
                    app_id: app_id("not-offered"),
                    surround_audio_info: 0
                }
            )
            .await,
            Err(DirectoryError::NoSuchApp)
        );
        assert!(rig.portal.asked().is_empty(), "nothing needed starting");
    }

    #[tokio::test]
    async fn launching_a_template_that_isnt_running_asks_the_portal_and_waits_for_it() {
        let (access, _) = streamer("s").await;
        let rig = rig(vec![env("e-hers", "alice", "chrome", Some(&access))]);
        // Alice's chrome is not bob's: the portal starts his, a moment later
        // than it says so.
        rig.mode(Mode::Start {
            env: env("e-new", "bob", "chrome", Some(&access)),
            created: false,
            after: Duration::from_millis(300),
        });
        let d = &rig.directory;
        let began = Instant::now();
        let target = d
            .launch(&client(1), launch_request(app_id("chrome")))
            .await
            .unwrap();
        assert!(
            began.elapsed() >= Duration::from_millis(250),
            "it waited for the environment"
        );
        assert_eq!(target.media_ports.video, 7700);
        assert_eq!(
            rig.portal.asked(),
            [PortalRequest::GameStreamLaunch {
                user_id: "bob".into(),
                template_id: "chrome".into()
            }]
        );
        // Now it runs: the next launch, and a resume, find it.
        d.launch(&client(1), launch_request(app_id("chrome")))
            .await
            .unwrap();
        d.resume(
            &client(1),
            ResumeRequest {
                app_id: app_id("chrome"),
                surround_audio_info: 0,
            },
        )
        .await
        .unwrap();
        assert_eq!(rig.portal.asked().len(), 1);
        // Its media goes to that environment.
        assert_eq!(
            d.find(&client(1), app_id("chrome")).unwrap().unwrap().id,
            "e-new"
        );
    }

    #[tokio::test]
    async fn the_portals_refusal_is_the_clients_reason() {
        let rig = rig(Vec::new());
        rig.mode(Mode::Refuse("your Steam is already running on box".into()));
        assert_eq!(
            rig.directory
                .launch(&client(1), launch_request(app_id("chrome")))
                .await,
            Err(DirectoryError::Refused(
                "your Steam is already running on box".into()
            ))
        );
        let resumed = rig
            .directory
            .resume(
                &client(1),
                ResumeRequest {
                    app_id: app_id("chrome"),
                    surround_audio_info: 0,
                },
            )
            .await;
        assert!(matches!(resumed, Err(DirectoryError::Refused(_))));
    }

    #[tokio::test]
    async fn a_launch_fails_when_there_is_no_portal_no_answer_or_no_environment() {
        // Nobody connected: nobody to ask.
        let (out, rx) = tokio::sync::broadcast::channel(8);
        drop(rx);
        let pairings = Pairings::new(out.clone(), None);
        pairings.set_devices(vec![GameStreamDevice {
            fingerprint: client(1).fingerprint().into(),
            unique_id: None,
            name: "d".into(),
            user_id: "bob".into(),
        }]);
        let link = Arc::new(PortalLink::new(out));
        link.set_launches(true);
        let alone = NodeDirectory::new(
            Arc::new(Environments(Mutex::default())),
            pairings,
            link,
            quick(),
        );
        match alone
            .launch(&client(1), launch_request(app_id("chrome")))
            .await
        {
            Err(DirectoryError::Failed(why)) => assert!(why.contains("isn't connected"), "{why}"),
            other => panic!("{other:?}"),
        }

        // A portal that never answers.
        let rig = rig_with(Vec::new(), quick());
        rig.mode(Mode::Silent);
        match rig
            .directory
            .launch(&client(1), launch_request(app_id("chrome")))
            .await
        {
            Err(DirectoryError::Failed(why)) => assert!(why.contains("didn't answer"), "{why}"),
            other => panic!("{other:?}"),
        }

        // One that says it runs, though this node never sees it.
        rig.mode(Mode::Ghost);
        match rig
            .directory
            .launch(&client(1), launch_request(app_id("chrome")))
            .await
        {
            Err(DirectoryError::Failed(why)) => assert!(why.contains("in time"), "{why}"),
            other => panic!("{other:?}"),
        }
    }

    /// Bob launches Chrome through the portal, which makes `created` say whether
    /// the launch started it; returns the rig, with the environment running.
    async fn launched(options: Options, created: bool) -> Rig {
        let (access, _) = streamer("s").await;
        let rig = rig_with(Vec::new(), options);
        rig.mode(Mode::Start {
            env: env("e-new", "bob", "chrome", Some(&access)),
            created,
            after: Duration::ZERO,
        });
        rig.directory
            .launch(&client(1), launch_request(app_id("chrome")))
            .await
            .unwrap();
        rig
    }

    #[tokio::test]
    async fn quitting_ends_only_the_stream_unless_the_node_says_otherwise() {
        let rig = launched(Options::default(), true).await;
        rig.directory.cancel(&client(1)).await;
        assert!(rig.portal.stops().is_empty(), "off by default");
        assert_eq!(rig.environments.0.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn quitting_stops_the_environment_a_moonlight_launch_started_when_set_to() {
        let stops = Options {
            quit_stops: true,
            ..Options::default()
        };
        let rig = launched(stops, true).await;
        rig.directory.cancel(&client(1)).await;
        assert_eq!(
            rig.portal.stops(),
            [("bob".to_string(), "e-new".to_string())]
        );
        // Once: another quit has nothing left of it to stop.
        rig.directory.cancel(&client(1)).await;
        assert_eq!(rig.portal.stops().len(), 1);
        // And a client that never launched anything stops nothing.
        rig.directory.cancel(&client(2)).await;
        assert_eq!(rig.portal.stops().len(), 1);
    }

    #[tokio::test]
    async fn a_resume_after_the_stream_dropped_still_quits_what_the_launch_started() {
        let stops = Options {
            quit_stops: true,
            ..Options::default()
        };
        let rig = launched(stops, true).await;
        rig.directory
            .resume(
                &client(1),
                ResumeRequest {
                    app_id: app_id("chrome"),
                    surround_audio_info: 0,
                },
            )
            .await
            .unwrap();
        rig.directory.cancel(&client(1)).await;
        assert_eq!(rig.portal.stops().len(), 1);
    }

    #[tokio::test]
    async fn quitting_never_stops_what_the_user_started_elsewhere() {
        let stops = Options {
            quit_stops: true,
            ..Options::default()
        };
        // Running already (started in the browser): the launch only finds it.
        let (access, _) = streamer("s").await;
        let rig = rig_with(
            vec![env("e-browser", "bob", "chrome", Some(&access))],
            stops,
        );
        rig.directory
            .launch(&client(1), launch_request(app_id("chrome")))
            .await
            .unwrap();
        rig.directory.cancel(&client(1)).await;
        // The portal says the launch joined an environment it didn't start.
        let joined = launched(stops, false).await;
        joined.directory.cancel(&client(1)).await;
        assert!(rig.portal.stops().is_empty() && joined.portal.stops().is_empty());
    }

    #[tokio::test]
    async fn media_goes_to_the_environments_streamer_with_its_secret_and_stops_there() {
        let (access, fake) = streamer("the-secret").await;
        let rig = rig(vec![env("e-mine", "bob", "chrome", Some(&access))]);
        let d = &rig.directory;
        let app = app_id("chrome");

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
            .start_media(handoff(1, &client(1), app_id("firefox")))
            .await;
        assert_eq!(refused, Err(DirectoryError::NoSuchApp));
        assert!(fake.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn the_streamers_refusals_are_told_as_they_are() {
        let (access, fake) = streamer("s").await;
        let rig = rig(vec![env("e-mine", "bob", "chrome", Some(&access))]);
        let app = app_id("chrome");
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
        d.start_media(handoff(1, &client(1), app_id("chrome")))
            .await
            .unwrap();
        d.start_media(handoff(2, &client(2), app_id("firefox")))
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
            d.launch(&client(1), launch_request(app_id("chrome")))
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
        d.start_media(handoff(4, &client(1), app_id("chrome")))
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

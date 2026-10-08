//! The native client's portal transport (ADR 0013): signs in to a Cha Portal
//! with a device token and lists its apps, as a [`cha_client::Transport`]
//! named "Cha Portal". The contract with the portal is
//! `docs/plans/c2-device-signin.md`.
//!
//! - **Hosts:** the portals this install knows, keyed by origin
//!   (`https://portal.example`). One is paired when it holds a token.
//!   [`Transport::add_host`] adds a portal by address (https unless the address
//!   says otherwise; plain http only to this machine or with
//!   `CHA_ALLOW_INSECURE_PORTAL=true`).
//! - **Signing in:** by a `cha://connect` link's one-use ticket
//!   ([`Portal::sign_in_with_ticket`]), or by device code: [`Transport::pair`]
//!   returns the code to approve at `<portal>/link` and polls until it is
//!   approved, denied or expires. [`Portal::sign_out`] forgets the token here
//!   (revoking it is done in the portal).
//! - **Tokens:** kept in `portals.json` under the directory given to
//!   [`Portal::open`] (0600), next to the install id. A token only ever goes
//!   to the origin it was issued by ([`PortalClient`] is bound to one), and a
//!   `401` clears it: the portal is then shown as not signed in.
//! - **Apps:** the portal's catalog. Catalog ids are strings and
//!   [`cha_client::App::id`] is a number, so an app's id is its position in
//!   the catalog the portal last sent (1-based); [`Portal::template_id`]
//!   maps it back. Each app's [`cha_client::AppState`] is the user's
//!   environment of that template (`GET /api/environments`), so apps running
//!   from a browser or another player show as running here; the list is a
//!   snapshot, to be listed again to stay current.
//! - **Launch:** [`Transport::launch`] runs the app's template on the portal
//!   and streams it over `cha-stream/1` on WebTransport
//!   (`cha-client-stream`), through the portal as the browser does. It
//!   reuses the user's starting or running environment of that template, or
//!   starts one (`POST /api/environments`), and polls it until it runs
//!   (minutes for Steam); picks the first of the player's codecs the
//!   environment encodes; asks `POST /api/environments/{id}/connect` for a
//!   media token (60 s) right before connecting. Stopping the session leaves
//!   the environment running, so launching again resumes it; stopping with
//!   `quit_app` also stops the environment (`DELETE /api/environments/{id}`),
//!   and [`Transport::quit_app`] stops it without streaming.
//!   A dropped connection ends the session; there is no reconnect yet.
//!
//! Needs a tokio runtime.

mod client;
mod link;
mod origin;
mod store;

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use cha_client::{
    App, AppState, BoxFuture, Codec, Host, Input, Pairing, Session, SessionControl, StreamConfig,
    Transport,
};
use cha_client_stream::Target;
use tracing::{info, warn};

pub use client::{
    DeviceCode, Environment, Grant, GrantUser, Me, PortalClient, PortalError, StreamerConnection,
    TokenPoll,
};
pub use link::{ConnectLink, parse_connect_link, parse_connect_link_with};
pub use origin::{ALLOW_INSECURE_ENV, host_label, normalize_origin, normalize_origin_with};
pub use store::{PortalEntry, PortalStore};

/// What the transport is called in the UI.
pub const TRANSPORT_NAME: &str = "Cha Portal";

/// The portal won't take a name longer than this.
const MAX_DEVICE_NAME: usize = 100;
/// Polls that fail to reach the portal in a row before a pairing gives up.
const MAX_POLL_FAILURES: u32 = 4;
/// How long a launch waits for its environment to run: the portal itself
/// gives up on a launch after 10 minutes.
const LAUNCH_TIMEOUT: Duration = Duration::from_secs(11 * 60);
/// How often a launch looks at its environment.
const LAUNCH_POLL: Duration = Duration::from_millis(500);
/// How long a launch waits for the app's last environment to finish
/// closing (Steam saving its state, the node removing containers).
const STOP_WAIT: Duration = Duration::from_secs(120);
/// Polls that fail to reach the portal in a row before a launch gives up.
const MAX_LAUNCH_POLL_FAILURES: u32 = 5;

/// How a device-code sign-in paces its polls.
#[derive(Clone, Copy, Debug)]
pub struct PollTiming {
    /// Added to the interval each time the portal says `slow_down`.
    pub slow_down_step: Duration,
    /// Never poll faster than this, whatever the portal says (the portal's
    /// `interval` is used when it is longer).
    pub min_interval: Duration,
}

impl Default for PollTiming {
    fn default() -> Self {
        Self {
            slow_down_step: Duration::from_secs(5),
            min_interval: Duration::from_secs(1),
        }
    }
}

struct Inner {
    store: PortalStore,
    device_name: String,
    timing: PollTiming,
    /// Catalog template ids by origin, in the order the apps were listed.
    templates: Mutex<HashMap<String, Vec<String>>>,
}

/// The portal transport. Cheap to clone; clones share the same state.
#[derive(Clone)]
pub struct Portal {
    inner: Arc<Inner>,
}

/// Who a sign-in made this install.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignedIn {
    pub portal: String,
    pub username: String,
    pub role: String,
}

impl Portal {
    /// Keeps tokens and the install id under `dir`. `device_name` is what the
    /// portal's Devices page calls this install ("Alex's MacBook Pro").
    pub fn open(dir: &Path, device_name: &str) -> Result<Self> {
        Self::open_with_timing(dir, device_name, PollTiming::default())
    }

    /// As [`Self::open`] with the polling pace given (for tests).
    #[doc(hidden)]
    pub fn open_with_timing(dir: &Path, device_name: &str, timing: PollTiming) -> Result<Self> {
        let device_name: String = device_name.trim().chars().take(MAX_DEVICE_NAME).collect();
        Ok(Self {
            inner: Arc::new(Inner {
                store: PortalStore::open(dir)?,
                device_name: if device_name.is_empty() {
                    "Cha Player".into()
                } else {
                    device_name
                },
                timing,
                templates: Mutex::default(),
            }),
        })
    }

    /// This install's id, as sent to portals.
    pub fn install_id(&self) -> String {
        self.inner.store.install_id()
    }

    /// Swaps a `cha://connect` link's ticket for a device token and keeps it.
    pub async fn sign_in_with_ticket(&self, link: &ConnectLink) -> Result<SignedIn> {
        let client = PortalClient::new(&link.portal, None)?;
        let grant = client
            .swap_ticket(&link.ticket, &self.inner.store.install_id(), &self.inner.device_name)
            .await
            .map_err(|e| match e.code() {
                Some("invalid_ticket") => anyhow!(
                    "the sign-in link has expired or was already used; open it again from the portal"
                ),
                _ => anyhow!(e),
            })?;
        self.inner.keep(client.origin(), grant)
    }

    /// Forgets the token for `portal` (an origin) on this install; the portal
    /// keeps listing the device until it is revoked there.
    pub fn sign_out(&self, portal: &str) -> Result<()> {
        self.inner.store.sign_out(portal)?;
        self.inner
            .templates
            .lock()
            .expect("templates lock")
            .remove(portal);
        Ok(())
    }

    /// Whether this install holds a token for `portal` (an origin).
    pub fn is_signed_in(&self, portal: &str) -> bool {
        self.inner.store.get(portal).is_some_and(|e| e.signed_in())
    }

    /// The catalog template id behind an app id from [`Transport::apps`].
    pub fn template_id(&self, portal: &str, app_id: u32) -> Option<String> {
        let templates = self.inner.templates.lock().expect("templates lock");
        let index = usize::try_from(app_id.checked_sub(1)?).ok()?;
        templates.get(portal)?.get(index).cloned()
    }

    /// Makes `portal` (an origin) known without signing in, as `add_host` does.
    pub fn add_portal(&self, address: &str) -> Result<Host> {
        let origin = normalize_origin(address)?;
        self.inner.store.add(&origin)?;
        Ok(self
            .inner
            .host(&origin, &self.inner.store.get(&origin).unwrap_or_default()))
    }
}

impl Inner {
    fn host(&self, origin: &str, entry: &PortalEntry) -> Host {
        let label = host_label(origin);
        Host {
            id: origin.to_string(),
            name: match &entry.username {
                Some(user) if entry.signed_in() => format!("{label} ({user})"),
                _ => label,
            },
            address: origin.to_string(),
            paired: entry.signed_in(),
            running_app: None,
        }
    }

    fn keep(&self, origin: &str, grant: Grant) -> Result<SignedIn> {
        let signed_in = SignedIn {
            portal: origin.to_string(),
            username: grant.user.username.clone(),
            role: grant.user.role.clone(),
        };
        self.store.set(
            origin,
            PortalEntry {
                token: Some(grant.token),
                device_id: Some(grant.device_id),
                username: Some(grant.user.username),
                role: Some(grant.user.role),
            },
        )?;
        info!(portal = origin, user = %signed_in.username, "signed in to the portal");
        Ok(signed_in)
    }

    /// A client for `origin` with its token, which must be one we hold.
    fn authed(&self, origin: &str) -> Result<PortalClient> {
        let entry = self
            .store
            .get(origin)
            .ok_or_else(|| anyhow!("{origin} isn't a portal this player knows"))?;
        let token = entry
            .token
            .ok_or_else(|| anyhow!("not signed in to {origin}; pair with it first"))?;
        PortalClient::new(origin, Some(token))
    }

    /// A `401` means the token is gone: forget it so the portal shows as signed out.
    fn on_error(&self, origin: &str, e: PortalError) -> anyhow::Error {
        if matches!(e, PortalError::SignedOut) {
            warn!(
                portal = origin,
                "the portal no longer accepts this device's token"
            );
            if let Err(err) = self.store.sign_out(origin) {
                warn!("forgetting the token: {err:#}");
            }
            self.templates
                .lock()
                .expect("templates lock")
                .remove(origin);
            return anyhow!(
                "signed out of {origin}: this device was removed there. Pair with it again"
            );
        }
        anyhow!(e)
    }

    async fn apps(&self, origin: &str) -> Result<Vec<App>> {
        let client = self.authed(origin)?;
        let templates = client
            .catalog()
            .await
            .map_err(|e| self.on_error(origin, e))?;
        // Without them every app reads as stopped, and launching still finds
        // a running one: not worth failing the list for.
        let environments = match client.environments().await {
            Ok(environments) => environments,
            Err(PortalError::SignedOut) => {
                return Err(self.on_error(origin, PortalError::SignedOut));
            }
            Err(e) => {
                warn!(portal = origin, "listing environments: {e}");
                Vec::new()
            }
        };
        let apps = templates
            .iter()
            .enumerate()
            .map(|(i, t)| App {
                id: i as u32 + 1,
                name: t.name.clone(),
                hdr: false,
                state: app_state(&environments, &t.id),
            })
            .collect();
        self.templates.lock().expect("templates lock").insert(
            origin.to_string(),
            templates.into_iter().map(|t| t.id).collect(),
        );
        Ok(apps)
    }

    async fn pair(self: &Arc<Self>, origin: &str) -> Result<Pairing> {
        if self.store.get(origin).is_none() {
            bail!("{origin} isn't a portal this player knows");
        }
        let client = PortalClient::new(origin, None)?;
        let code = client
            .start_code(&self.store.install_id(), &self.device_name)
            .await
            .map_err(|e| anyhow!(e))?;
        let url = format!("{origin}{}", code.verification_path);
        info!(
            portal = origin,
            "sign-in: waiting for the code to be approved"
        );
        let this = Arc::clone(self);
        let origin = origin.to_string();
        let user_code = code.user_code.clone();
        Ok(Pairing {
            pin: user_code,
            instructions: format!(
                "Open {url} in a browser where you are signed in, and approve this code."
            ),
            done: Box::pin(async move { this.await_approval(&client, &origin, &code).await }),
        })
    }

    /// Polls until the code is approved, denied or expires.
    async fn await_approval(
        &self,
        client: &PortalClient,
        origin: &str,
        code: &DeviceCode,
    ) -> Result<()> {
        let mut interval = Duration::from_secs(code.interval).max(self.timing.min_interval);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(code.expires_in);
        let mut failures = 0;
        loop {
            tokio::time::sleep(interval).await;
            if tokio::time::Instant::now() >= deadline {
                bail!("the code wasn't approved in time; pair again for a new one");
            }
            match client.poll_code(&code.device_code).await {
                Ok(TokenPoll::Pending) => failures = 0,
                Ok(TokenPoll::SlowDown) => {
                    failures = 0;
                    interval += self.timing.slow_down_step;
                }
                Ok(TokenPoll::Approved(grant)) => {
                    self.keep(origin, grant)?;
                    return Ok(());
                }
                Ok(TokenPoll::Denied) => bail!("the sign-in was denied in the portal"),
                Ok(TokenPoll::Expired) => {
                    bail!("the code expired before it was approved; pair again for a new one")
                }
                Err(e) => {
                    failures += 1;
                    if failures >= MAX_POLL_FAILURES {
                        return Err(anyhow!(e)).context("waiting for the portal");
                    }
                }
            }
        }
    }
}

impl Transport for Portal {
    fn name(&self) -> &str {
        TRANSPORT_NAME
    }

    fn hosts(&self) -> Vec<Host> {
        self.inner
            .store
            .portals()
            .iter()
            .map(|(origin, entry)| self.inner.host(origin, entry))
            .collect()
    }

    fn add_host(&self, address: &str) -> BoxFuture<'_, Result<Host>> {
        let address = address.to_string();
        Box::pin(async move { self.add_portal(&address) })
    }

    fn pair(&self, host_id: &str) -> BoxFuture<'_, Result<Pairing>> {
        let inner = Arc::clone(&self.inner);
        let id = host_id.to_string();
        Box::pin(async move { inner.pair(&id).await })
    }

    fn apps(&self, host_id: &str) -> BoxFuture<'_, Result<Vec<App>>> {
        let inner = Arc::clone(&self.inner);
        let id = host_id.to_string();
        Box::pin(async move { inner.apps(&id).await })
    }

    fn launch(
        &self,
        host_id: &str,
        app_id: u32,
        config: StreamConfig,
    ) -> BoxFuture<'_, Result<Session>> {
        let inner = Arc::clone(&self.inner);
        let host = host_id.to_string();
        Box::pin(async move { inner.launch(&host, app_id, config).await })
    }

    fn quit_app(
        &self,
        host_id: &str,
        app_id: u32,
        _config: StreamConfig,
    ) -> BoxFuture<'_, Result<()>> {
        let inner = Arc::clone(&self.inner);
        let host = host_id.to_string();
        Box::pin(async move { inner.quit_app(&host, app_id).await })
    }
}

/// Wraps a stream's control so that stopping it with `quit_app` also stops
/// the environment on the portal.
struct QuitsEnvironment {
    inner: Box<dyn SessionControl>,
    client: PortalClient,
    environment: String,
    runtime: tokio::runtime::Handle,
}

impl SessionControl for QuitsEnvironment {
    fn input(&self, input: Input) {
        self.inner.input(input);
    }

    fn release_all(&self) {
        self.inner.release_all();
    }

    fn request_keyframe(&self) {
        self.inner.request_keyframe();
    }

    fn stop(&self, quit_app: bool) {
        self.inner.stop(quit_app);
        if quit_app {
            let client = self.client.clone();
            let id = self.environment.clone();
            self.runtime.spawn(async move {
                match client.stop_environment(&id).await {
                    Ok(_) => info!(environment = %id, "environment stopped"),
                    Err(e) => warn!(environment = %id, "stopping the environment: {e}"),
                }
            });
        }
    }
}

/// The player's codecs the environment encodes, in the player's order. An
/// environment whose node didn't say what it encodes (`None`) is tried with
/// all of them; the portal refuses the ones it can't.
pub fn pick_codecs(wanted: &[Codec], offered: Option<&[String]>) -> Result<Vec<Codec>> {
    let name = |c: Codec| cha_client_stream::control::codec_name(c);
    let picked: Vec<Codec> = wanted
        .iter()
        .copied()
        .filter(|c| offered.is_none_or(|o| o.iter().any(|n| n == name(*c))))
        .collect();
    if picked.is_empty() {
        let offered = offered.map(|o| o.join(", ")).unwrap_or_default();
        let wanted: Vec<&str> = wanted.iter().map(|c| name(*c)).collect();
        bail!(
            "this environment encodes {offered}; this player decodes {}",
            wanted.join(", ")
        );
    }
    Ok(picked)
}

impl Inner {
    /// The catalog template behind an app id, listing the catalog if this
    /// run hasn't yet.
    async fn template_for(&self, origin: &str, app_id: u32) -> Result<String> {
        let known = |this: &Self| {
            let templates = this.templates.lock().expect("templates lock");
            let index = usize::try_from(app_id.checked_sub(1)?).ok()?;
            templates.get(origin)?.get(index).cloned()
        };
        if let Some(id) = known(self) {
            return Ok(id);
        }
        self.apps(origin).await?;
        known(self).ok_or_else(|| anyhow!("the portal's catalog has no app {app_id}"))
    }

    /// Stops the user's environment of the app's template, running or
    /// starting, without connecting to it.
    async fn quit_app(&self, origin: &str, app_id: u32) -> Result<()> {
        let client = self.authed(origin)?;
        let template = self.template_for(origin, app_id).await?;
        let environments = client
            .environments()
            .await
            .map_err(|e| self.on_error(origin, e))?;
        let env = pick_existing(&environments, &template)
            .ok_or_else(|| anyhow!("it isn't running any more"))?;
        client
            .stop_environment(&env.id)
            .await
            .map_err(|e| self.on_error(origin, e))?;
        info!(environment = %env.id, template = %template, "environment stopped");
        Ok(())
    }

    async fn launch(&self, origin: &str, app_id: u32, config: StreamConfig) -> Result<Session> {
        let client = self.authed(origin)?;
        let template = self.template_for(origin, app_id).await?;
        let env = self
            .environment_for(&client, &template)
            .await
            .map_err(|e| self.on_error(origin, e))?;
        info!(environment = %env.id, template = %template, "environment running");
        let codecs = pick_codecs(&config.codecs, env.codecs.as_deref())?;

        // The media token lives 60 s: ask for it as late as possible.
        let mut refused = None;
        for codec in codecs {
            let connection = match client
                .connect_environment(&env.id, cha_client_stream::control::codec_name(codec))
                .await
            {
                Ok(c) => c,
                // This streamer can't encode it after all: the next one.
                Err(PortalError::Api { code, message, .. }) if code == "bad_codec" => {
                    refused = Some(message);
                    continue;
                }
                Err(PortalError::Api { code, .. }) if code == "not_running" => {
                    bail!("the environment stopped while connecting; launch it again")
                }
                Err(e) => return Err(self.on_error(origin, e)),
            };
            let target = Target {
                urls: connection.urls,
                cert_hash: connection.cert_hash,
                codec,
            };
            let mut session = cha_client_stream::connect_at(
                &target,
                config.width,
                config.height,
                Some(config.fps),
            )
            .await
            .context("connecting to the streamer")?;
            session.control = Box::new(QuitsEnvironment {
                inner: session.control,
                client,
                environment: env.id,
                runtime: tokio::runtime::Handle::current(),
            });
            return Ok(session);
        }
        bail!(
            "{}",
            refused.unwrap_or_else(|| "the environment accepts none of the codecs".into())
        )
    }

    /// Finds the user's environment of `template` that is running (or still
    /// starting), or launches one, and waits until it runs.
    async fn environment_for(
        &self,
        client: &PortalClient,
        template: &str,
    ) -> Result<Environment, PortalError> {
        let mut existing = client.environments().await?;
        // One still closing down goes first: a second beside it would share
        // its app data (Steam's home) while it shuts.
        let closing = tokio::time::Instant::now() + STOP_WAIT;
        while pick_existing(&existing, template).is_none()
            && let Some(old) = stopping(&existing, template)
        {
            if tokio::time::Instant::now() >= closing {
                return Err(PortalError::Other(anyhow!(
                    "the last {template} is still closing after {} s; try again in a moment",
                    STOP_WAIT.as_secs()
                )));
            }
            info!(environment = %old.id, "waiting for the last one to close");
            tokio::time::sleep(LAUNCH_POLL).await;
            existing = client.environments().await?;
        }
        let found = pick_existing(&existing, template);
        let mut env = match found {
            Some(env) => env.clone(),
            None => client.launch_environment(template).await?,
        };
        let deadline = tokio::time::Instant::now() + LAUNCH_TIMEOUT;
        let mut failures = 0;
        loop {
            match env.state.as_str() {
                "running" => return Ok(env),
                "starting" => {}
                state => {
                    return Err(PortalError::Other(anyhow!(
                        "the environment {}: {}",
                        match state {
                            "failed" => "failed to start",
                            "stopping" | "destroyed" => "was stopped",
                            _ => "is in an unexpected state",
                        },
                        env.detail.clone().unwrap_or_else(|| state.to_string())
                    )));
                }
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(PortalError::Other(anyhow!(
                    "the environment didn't start in {} minutes",
                    LAUNCH_TIMEOUT.as_secs() / 60
                )));
            }
            tokio::time::sleep(LAUNCH_POLL).await;
            match client.environment(&env.id).await {
                Ok(next) => {
                    failures = 0;
                    env = next;
                }
                // The portal answering is the portal being there: only a
                // flaky network is waited out.
                Err(PortalError::Other(e)) => {
                    failures += 1;
                    if failures >= MAX_LAUNCH_POLL_FAILURES {
                        return Err(PortalError::Other(e));
                    }
                }
                Err(e) => return Err(e),
            }
        }
    }
}

/// The environment of `template` to reuse: one that runs, else one that is
/// starting; the portal lists the newest first.
pub fn pick_existing<'a>(
    environments: &'a [Environment],
    template: &str,
) -> Option<&'a Environment> {
    let of_template = || environments.iter().filter(|e| e.template_id == template);
    of_template()
        .find(|e| e.state == "running")
        .or_else(|| of_template().find(|e| e.state == "starting"))
}

/// How the user's environments of `template` make its app look: running if
/// one runs, starting if one starts, stopping if one is still closing down,
/// else stopped.
pub fn app_state(environments: &[Environment], template: &str) -> AppState {
    match pick_existing(environments, template).map(|e| e.state.as_str()) {
        Some("running") => AppState::Running,
        Some(_) => AppState::Starting,
        None if stopping(environments, template).is_some() => AppState::Stopping,
        None => AppState::Stopped,
    }
}

/// The user's environment of `template` that is still closing down, if any.
fn stopping<'a>(environments: &'a [Environment], template: &str) -> Option<&'a Environment> {
    environments
        .iter()
        .find(|e| e.template_id == template && e.state == "stopping")
}

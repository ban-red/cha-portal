//! The native client's GameStream transport (ADR 0010): plays Sunshine and
//! Apollo hosts, and our own nodes (ADR 0009), as a [`cha_client::Transport`]
//! on `moonlight-common-rust`.
//!
//! - **Discovery:** an mDNS browse for `_nvstream._tcp.local.` and hosts added
//!   by address. Each is read with `/serverinfo` (with this install's identity,
//!   so `paired` is right), every 30 s; one found only by the browse drops
//!   after two minutes unseen, one paired or added stays.
//! - **Identity:** one client certificate per install, and each paired host's
//!   certificate, under the directory given to [`GameStream::open`]
//!   (`~/Library/Application Support/Cha Player` in the app), files 0600; the
//!   hosts to show before they answer are kept there too.
//! - **Pairing:** [`Transport::pair`] returns a random four-digit PIN at once and
//!   goes on in the background until the host accepts it (typed on
//!   Sunshine's or Apollo's PIN page, or in the portal for our nodes), fails
//!   or five minutes pass.
//! - **Launch:** the codec is the first of `StreamConfig::codecs` the host can
//!   encode (HEVC, H.264; AV1 isn't in the library, so it is skipped); stereo
//!   audio only; no video encryption (the library can't decrypt it). A host
//!   already running the requested app is resumed; one running another app
//!   is refused, rather than quitting a game the user may be in.
//!
//! Needs a tokio runtime. The input mapping is `cha-moonlight-input`.

mod discovery;
mod hosts;
mod store;
mod stream;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use cha_client::{App, BoxFuture, Host, Pairing, Session, StreamConfig, Transport};
use futures_util::future::join_all;
use moonlight_common::crypto::rustcrypto::RustCryptoBackend;
use moonlight_common::high::tokio::MoonlightHost;
use moonlight_common::http::client::tokio_hyper::TokioHyperClient;
use moonlight_common::http::pair::PairPin;
use moonlight_common::http::{ClientIdentifier, ClientSecret};
use tokio::sync::{Notify, mpsc, watch};
use tracing::{debug, info, warn};

use crate::discovery::Seen;
use crate::hosts::{Endpoint, HostList, Probed};
use crate::store::{SavedHost, Store};

type Client = MoonlightHost<TokioHyperClient>;

/// What hosts call this client in their requests (they know it by certificate).
const CLIENT_UNIQUE_ID: &str = "cha-player";
/// The name a host lists this player under once paired. No space: the
/// library puts it in the query string as is.
const DEVICE_NAME: &str = "ChaPlayer";
/// How often each host is read again.
const REFRESH: Duration = Duration::from_secs(30);
/// One host's `/serverinfo`; an unreachable host mustn't hold up the rest.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
/// How long the PIN stays valid.
const PAIR_TIMEOUT: Duration = Duration::from_secs(300);

/// A pairing's end as every waiter sees it.
type PairOutcome = Option<Result<(), String>>;

struct Pending {
    pin: String,
    outcome: watch::Receiver<PairOutcome>,
}

struct State {
    list: HostList,
    /// What `hosts.json` holds.
    saved: Vec<SavedHost>,
}

struct Inner {
    store: Store,
    state: Mutex<State>,
    /// Pairings in progress, by host id.
    pairing: Mutex<HashMap<String, Pending>>,
    /// Look at every host now (after a pairing, a launch).
    rescan: Notify,
}

/// The GameStream transport. Dropping it stops the browse and the refresh.
pub struct GameStream {
    inner: Arc<Inner>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for GameStream {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl GameStream {
    /// Browses the LAN for hosts and keeps its identity and known hosts under
    /// `dir` (made, owner-only, if missing).
    pub fn open(dir: PathBuf) -> Result<Self> {
        Self::start(dir, true)
    }

    /// As [`Self::open`] without the browse: only the saved hosts and the
    /// ones added by address (for tests, and networks without mDNS).
    pub fn without_discovery(dir: PathBuf) -> Result<Self> {
        Self::start(dir, false)
    }

    fn start(dir: PathBuf, discover: bool) -> Result<Self> {
        // moonlight-common's HTTPS client builds rustls without a provider.
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let store = Store::new(dir);
        let list = HostList::from_saved(store.load_hosts(), Instant::now());
        let saved = list.saved();
        let seen = if discover {
            Some(discovery::browse()?)
        } else {
            None
        };
        let inner = Arc::new(Inner {
            store,
            state: Mutex::new(State { list, saved }),
            pairing: Mutex::default(),
            rescan: Notify::new(),
        });
        let task = tokio::spawn(maintain(Arc::clone(&inner), seen));
        Ok(Self { inner, task })
    }
}

impl Transport for GameStream {
    fn name(&self) -> &str {
        "Moonlight"
    }

    fn hosts(&self) -> Vec<Host> {
        self.inner.state.lock().expect("state lock").list.hosts()
    }

    fn add_host(&self, address: &str) -> BoxFuture<'_, Result<Host>> {
        let inner = Arc::clone(&self.inner);
        let address = address.to_string();
        Box::pin(async move { inner.add_host(&address).await })
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
        let id = host_id.to_string();
        Box::pin(async move {
            let (client, _) = inner.paired_client(&id).await?;
            let session = stream::start(client, app_id, config).await;
            // The host now runs (or no longer runs) the app: say so in the list.
            inner.rescan.notify_one();
            session
        })
    }
}

impl Inner {
    /// This install's identity (RSA key generation the first time).
    async fn identity(self: &Arc<Self>) -> Result<(ClientIdentifier, ClientSecret)> {
        let this = Arc::clone(self);
        tokio::task::spawn_blocking(move || this.store.client_identity()).await?
    }

    /// A client for the host at `endpoint`; with `server_id`, one that carries
    /// this install's identity and that host's certificate.
    async fn client(
        self: &Arc<Self>,
        endpoint: &Endpoint,
        server_id: Option<&str>,
    ) -> Result<Client> {
        let client = Client::new(
            endpoint.address.clone(),
            endpoint.port,
            Some(CLIENT_UNIQUE_ID.to_string()),
        )
        .map_err(|e| anyhow!("creating the Moonlight client: {e:?}"))?;
        if let Some(id) = server_id
            && let Some(server) = self.store.server_identity(id)?
        {
            let (cert, key) = self.identity().await?;
            client
                .set_identity(cert, key, server)
                .await
                .map_err(|e| anyhow!("the host didn't accept this player's identity: {e:?}"))?;
        }
        Ok(client)
    }

    fn lookup(&self, id: &str) -> Result<(Host, Endpoint)> {
        let state = self.state.lock().expect("state lock");
        state
            .list
            .get(id)
            .map(|(host, endpoint)| (host.clone(), endpoint.clone()))
            .ok_or_else(|| anyhow!("that host isn't known (any more); look for it again"))
    }

    /// The host `id` as a paired client, ready to list apps and stream.
    async fn paired_client(self: &Arc<Self>, id: &str) -> Result<(Client, Host)> {
        let (host, endpoint) = self.lookup(id)?;
        if self.store.server_identity(id)?.is_none() {
            bail!("this player isn't paired with {}; pair it first", host.name);
        }
        let client = self
            .client(&endpoint, Some(id))
            .await
            .with_context(|| format!("reaching {}", host.name))?;
        Ok((client, host))
    }

    /// Reads one host, over HTTPS too where this install holds its certificate.
    async fn probe(self: &Arc<Self>, endpoint: &Endpoint) -> Result<Probed> {
        let plain = self.client(endpoint, None).await?;
        let mut info = plain
            .server_info()
            .await
            .map_err(|e| anyhow!("reading /serverinfo from {}: {e:?}", endpoint.display()))?;
        // The host's `uniqueid` as we keep it: 32 uppercase hex digits.
        let id = info.unique_id.simple().to_string().to_uppercase();
        // Whether this install is paired is only visible over HTTPS with its
        // certificate, so a host we hold a certificate for is asked that way.
        let mut paired = false;
        if self.store.server_identity(&id)?.is_some() {
            match self.client(endpoint, Some(&id)).await {
                Ok(secure) => match secure.server_info().await {
                    Ok(secure_info) => {
                        paired = secure_info.paired;
                        info = secure_info;
                    }
                    Err(err) => debug!(%id, "not paired (or the host dropped us): {err:?}"),
                },
                Err(err) => debug!(%id, "not paired (or the host dropped us): {err:#}"),
            }
        }
        Ok(Probed {
            id,
            name: info.host_name,
            paired,
            running_app: (info.current_game != 0).then_some(info.current_game),
        })
    }

    fn record(&self, probed: Probed, endpoint: Endpoint, added: bool) {
        let mut state = self.state.lock().expect("state lock");
        state.list.seen(probed, endpoint, added, Instant::now());
        self.persist(&mut state);
    }

    /// Writes `hosts.json` if what should persist changed.
    fn persist(&self, state: &mut State) {
        let now = state.list.saved();
        if now != state.saved {
            match self.store.save_hosts(&now) {
                Ok(()) => state.saved = now,
                Err(err) => warn!("saving the host list: {err:#}"),
            }
        }
    }

    async fn add_host(self: &Arc<Self>, address: &str) -> Result<Host> {
        let endpoint = Endpoint::parse(address).ok_or_else(|| {
            anyhow!("{address:?} isn't a host address (try 192.168.1.20 or pc.lan:47989)")
        })?;
        let probed = tokio::time::timeout(PROBE_TIMEOUT, self.probe(&endpoint))
            .await
            .map_err(|_| anyhow!("{} didn't answer", endpoint.display()))??;
        let id = probed.id.clone();
        self.record(probed, endpoint, true);
        self.lookup(&id).map(|(host, _)| host)
    }

    /// Reads each endpoint (at once, each with its own time limit).
    async fn look(self: &Arc<Self>, endpoints: &[Endpoint]) {
        let probes = endpoints.iter().map(|endpoint| async move {
            match tokio::time::timeout(PROBE_TIMEOUT, self.probe(endpoint)).await {
                Ok(Ok(probed)) => Some((probed, endpoint.clone())),
                Ok(Err(err)) => {
                    debug!("{err:#}");
                    None
                }
                Err(_) => {
                    debug!("{} timed out", endpoint.display());
                    None
                }
            }
        });
        for (probed, endpoint) in join_all(probes).await.into_iter().flatten() {
            self.record(probed, endpoint, false);
        }
    }

    async fn apps(self: &Arc<Self>, id: &str) -> Result<Vec<App>> {
        let (client, host) = self.paired_client(id).await?;
        let apps = client
            .app_list()
            .await
            .map_err(|e| anyhow!("listing {}'s apps: {e:?}", host.name))?;
        Ok(apps
            .into_iter()
            .map(|a| App {
                id: a.id.0,
                name: a.title,
                hdr: a.is_hdr_supported,
            })
            .collect())
    }

    async fn pair(self: &Arc<Self>, id: &str) -> Result<Pairing> {
        let (host, endpoint) = self.lookup(id)?;
        if let Some(pending) = self.pairing.lock().expect("pairing lock").get(id) {
            return Ok(pending.pairing());
        }
        let (cert, key) = self.identity().await?;
        let pin = PairPin::new_random(&RustCryptoBackend).map_err(|e| anyhow!("{e:?}"))?;
        let pin_text = pin.to_string();
        let client = self.client(&endpoint, None).await?;
        let (outcome_tx, outcome) = watch::channel(None);
        {
            let mut pairing = self.pairing.lock().expect("pairing lock");
            // Another call got here while the identity was made.
            if let Some(pending) = pairing.get(id) {
                return Ok(pending.pairing());
            }
            pairing.insert(
                id.to_string(),
                Pending {
                    pin: pin_text.clone(),
                    outcome: outcome.clone(),
                },
            );
        }
        info!(host = %host.name, "pairing: waiting for the PIN on the host");
        let this = Arc::clone(self);
        let id = id.to_string();
        tokio::spawn(async move {
            let pairing = tokio::time::timeout(
                PAIR_TIMEOUT,
                client.pair(&cert, &key, DEVICE_NAME.into(), pin, RustCryptoBackend),
            )
            .await;
            let result: Result<()> = match pairing {
                Err(_) => Err(anyhow!(
                    "the PIN wasn't entered on the host within 5 minutes"
                )),
                Ok(Err(err)) => Err(anyhow!("pairing failed: {err:?}")),
                Ok(Ok(())) => match client.identity().await {
                    Some((_, _, server)) => this.store.save_server(&id, &server),
                    None => Err(anyhow!("paired, but the host's certificate is missing")),
                },
            };
            match &result {
                Ok(()) => {
                    info!(%id, "paired");
                    // Look again, so the list says paired before the player hears.
                    this.look(std::slice::from_ref(&endpoint)).await;
                }
                Err(err) => warn!(%id, "pairing failed: {err:#}"),
            }
            this.pairing.lock().expect("pairing lock").remove(&id);
            let _ = outcome_tx.send(Some(result.map_err(|e| format!("{e:#}"))));
        });
        Ok(Pairing {
            pin: pin_text,
            done: wait_for(outcome),
        })
    }
}

impl Pending {
    fn pairing(&self) -> Pairing {
        Pairing {
            pin: self.pin.clone(),
            done: wait_for(self.outcome.clone()),
        }
    }
}

/// Resolves with the pairing's result once it is in.
fn wait_for(mut outcome: watch::Receiver<PairOutcome>) -> BoxFuture<'static, Result<()>> {
    Box::pin(async move {
        let result = {
            let ended = outcome
                .wait_for(Option::is_some)
                .await
                .map_err(|_| anyhow!("pairing was abandoned"))?;
            ended.clone().expect("waited for a result")
        };
        result.map_err(|message| anyhow!(message))
    })
}

async fn next_seen(seen: &mut Option<mpsc::Receiver<Seen>>) -> Option<Seen> {
    match seen {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

/// Keeps the list fresh: reads what the browse finds, and every host again
/// each [`REFRESH`] (the first look is at once, at the saved hosts).
async fn maintain(inner: Arc<Inner>, mut seen: Option<mpsc::Receiver<Seen>>) {
    // Every endpoint the browse knows, by service name, to read again.
    let mut services: HashMap<String, Endpoint> = HashMap::new();
    let mut tick = tokio::time::interval(REFRESH);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            event = next_seen(&mut seen) => match event {
                // A resolve is news: read it now. A service lost is left to
                // the two-minute rule.
                Some(Seen::Resolved { name, address, port }) => {
                    let endpoint = Endpoint::from_ip(address, port);
                    services.insert(name, endpoint.clone());
                    inner.look(&[endpoint]).await;
                }
                Some(Seen::Removed { name }) => {
                    services.remove(&name);
                }
                None => seen = None,
            },
            _ = tick.tick() => refresh(&inner, &services).await,
            () = inner.rescan.notified() => refresh(&inner, &services).await,
        }
    }
}

/// Reads every known endpoint and drops the hosts gone silent.
async fn refresh(inner: &Arc<Inner>, services: &HashMap<String, Endpoint>) {
    let mut endpoints: HashSet<Endpoint> = services.values().cloned().collect();
    endpoints.extend(inner.state.lock().expect("state lock").list.endpoints());
    let endpoints: Vec<Endpoint> = endpoints.into_iter().collect();
    inner.look(&endpoints).await;
    inner
        .state
        .lock()
        .expect("state lock")
        .list
        .age(Instant::now());
}

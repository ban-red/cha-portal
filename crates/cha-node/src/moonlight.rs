//! Moonlight hosts on this node's network (ADR 0008).
//!
//! The node browses `_nvstream._tcp.local.` for Sunshine and Apollo hosts,
//! reads each one's `/serverinfo`, and publishes the list ([`Moonlight::hosts`])
//! for the agent to push to the portal. It is also the Moonlight *client*: one
//! identity per node and each paired host's certificate live under
//! `<data root>/node/moonlight/` (root, 0700; keys 0600), which a gateway
//! container gets read-only. Pairing runs in the background after the PIN is
//! handed out; the result comes through [`Moonlight::paired`].

use std::collections::HashMap;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use cha_wire::{MoonlightApp, MoonlightHost};
use futures_util::future::{BoxFuture, join_all};
use mdns_sd::{ServiceDaemon, ServiceEvent};
use moonlight_common::crypto::rustcrypto::RustCryptoBackend;
use moonlight_common::high::tokio::MoonlightHost as Client;
use moonlight_common::http::client::tokio_hyper::TokioHyperClient;
use moonlight_common::http::pair::{PairPin, PairingCryptoBackend};
use moonlight_common::http::{ClientIdentifier, ClientSecret, ServerIdentifier};
use moonlight_common::stream::video::ServerCodecModeSupport;
use pem::Pem;
use tokio::sync::{broadcast, mpsc, watch};
use tracing::{debug, info, warn};

/// What Sunshine and Apollo advertise.
pub const SERVICE: &str = "_nvstream._tcp.local.";
/// The name the host lists this node's client under.
const CLIENT_NAME: &str = "cha-node";
/// How often each host is read again.
const REFRESH: Duration = Duration::from_secs(60);
/// A host nobody has answered for this long is dropped.
const GONE_AFTER: Duration = Duration::from_secs(180);
/// One host's `/serverinfo`; an unreachable host mustn't hold up the rest.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
/// How long the PIN stays valid.
const PAIR_TIMEOUT: Duration = Duration::from_secs(300);

const CLIENT_CERT: &str = "client-cert.pem";
const CLIENT_KEY: &str = "client-key.pem";
const SERVER_CERT: &str = "server-cert.pem";

/// The identity directory under the data root.
pub fn dir(data_root: &Path) -> PathBuf {
    data_root.join("node").join("moonlight")
}

/// Whether `id` is a host's `uniqueid` as we write it (hex digits): it names a
/// directory and goes to Docker, so nothing else will do.
pub fn valid_unique_id(id: &str) -> bool {
    (1..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_hexdigit())
}

/// A service the browse resolved, or lost.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Seen {
    Resolved {
        /// Stable for the service: its mDNS full name.
        name: String,
        address: IpAddr,
        port: u16,
    },
    Removed {
        name: String,
    },
}

/// Reads one host. Real ones talk HTTP(S); tests answer from a table.
pub trait Probe: Send + Sync + 'static {
    fn probe(&self, address: IpAddr, port: u16) -> BoxFuture<'_, Result<MoonlightHost>>;
}

/// The node's Moonlight identity files.
#[derive(Debug, Clone)]
pub struct Store {
    dir: PathBuf,
}

impl Store {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn host_dir(&self, unique_id: &str) -> PathBuf {
        self.dir.join("hosts").join(unique_id)
    }

    /// Creates `path` and what's missing above it, owner-only.
    fn make_dir(path: &Path) -> Result<()> {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        builder
            .create(path)
            .with_context(|| format!("creating {}", path.display()))
    }

    /// Writes `pem` to `path` owner-only (0600), replacing what is there.
    fn write_private(path: &Path, pem: &Pem) -> Result<()> {
        use std::io::Write;
        let tmp = path.with_extension("tmp");
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut file = options
            .open(&tmp)
            .with_context(|| format!("writing {}", tmp.display()))?;
        file.write_all(pem.to_string().as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&tmp, path).with_context(|| format!("writing {}", path.display()))
    }

    fn read_pem(path: &Path) -> Result<Pem> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        Pem::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }

    /// This node's client identity, generated the first time it's needed.
    pub fn client_identity(&self) -> Result<(ClientIdentifier, ClientSecret)> {
        let cert = self.dir.join(CLIENT_CERT);
        let key = self.dir.join(CLIENT_KEY);
        if cert.exists() && key.exists() {
            return Ok((
                ClientIdentifier::from_pem(Self::read_pem(&cert)?),
                ClientSecret::from_pem(Self::read_pem(&key)?),
            ));
        }
        Self::make_dir(&self.dir)?;
        let (client, secret) = RustCryptoBackend
            .generate_client_identity()
            .map_err(|e| anyhow!("generating the Moonlight client identity: {e:?}"))?;
        Self::write_private(&key, &secret.to_pem())?;
        Self::write_private(&cert, &client.to_pem())?;
        info!(dir = %self.dir.display(), "made this node's Moonlight client identity");
        Ok((client, secret))
    }

    /// The certificate a paired host showed, if this node is paired with it.
    pub fn server_identity(&self, unique_id: &str) -> Result<Option<ServerIdentifier>> {
        if !valid_unique_id(unique_id) {
            bail!("{unique_id:?} isn't a Moonlight host id");
        }
        let path = self.host_dir(unique_id).join(SERVER_CERT);
        if !path.exists() {
            return Ok(None);
        }
        Ok(Some(ServerIdentifier::from_pem(Self::read_pem(&path)?)))
    }

    pub fn save_server(&self, unique_id: &str, server: &ServerIdentifier) -> Result<()> {
        if !valid_unique_id(unique_id) {
            bail!("{unique_id:?} isn't a Moonlight host id");
        }
        let dir = self.host_dir(unique_id);
        Self::make_dir(&dir)?;
        Self::write_private(&dir.join(SERVER_CERT), &server.to_pem())
    }

    /// How many hosts this node has a certificate for.
    pub fn paired_count(&self) -> usize {
        std::fs::read_dir(self.dir.join("hosts")).map_or(0, |entries| {
            entries
                .flatten()
                .filter(|e| e.path().join(SERVER_CERT).exists())
                .count()
        })
    }

    /// A client for the host at `address`, with this node's identity and the
    /// host's certificate when paired. `None` for the host's id means it
    /// answers as an unpaired client.
    async fn client(
        &self,
        address: IpAddr,
        port: u16,
        unique_id: Option<&str>,
    ) -> Result<Client<TokioHyperClient>> {
        let client = Client::<TokioHyperClient>::new(
            address.to_string(),
            port,
            Some(CLIENT_NAME.to_string()),
        )
        .map_err(|e| anyhow!("creating the Moonlight client: {e:?}"))?;
        if let Some(id) = unique_id
            && let Some(server) = self.server_identity(id)?
        {
            let (cert, key) = self.client_identity()?;
            client
                .set_identity(cert, key, server)
                .await
                .map_err(|e| anyhow!("{e:?}"))?;
        }
        Ok(client)
    }
}

/// Reads hosts over HTTP, and over HTTPS where this node is paired.
pub struct HttpProbe {
    store: Arc<Store>,
}

impl HttpProbe {
    pub fn new(store: Arc<Store>) -> Self {
        Self { store }
    }
}

impl Probe for HttpProbe {
    fn probe(&self, address: IpAddr, port: u16) -> BoxFuture<'_, Result<MoonlightHost>> {
        Box::pin(async move {
            let plain = self.store.client(address, port, None).await?;
            let info = plain
                .server_info()
                .await
                .map_err(|e| anyhow!("reading /serverinfo: {e:?}"))?;
            // The host's `uniqueid` as we keep it: 32 uppercase hex digits.
            let unique_id = info.unique_id.simple().to_string().to_uppercase();
            // Whether this node is paired is only visible over HTTPS with its
            // certificate, so a host we hold a certificate for is asked that way.
            let (info, paired) = match self.store.client(address, port, Some(&unique_id)).await {
                Ok(secure) => match secure.server_info().await {
                    Ok(secure_info) if self.store.server_identity(&unique_id)?.is_some() => {
                        let paired = secure_info.paired;
                        (secure_info, paired)
                    }
                    _ => (info, false),
                },
                Err(err) => {
                    debug!(%unique_id, "not paired (or the host dropped us): {err:#}");
                    (info, false)
                }
            };
            let mut codecs = Vec::new();
            let modes = info.server_codec_mode_support;
            if modes.contains(ServerCodecModeSupport::H264) {
                codecs.push("h264".to_string());
            }
            if modes.contains(ServerCodecModeSupport::HEVC) {
                codecs.push("hevc".to_string());
            }
            Ok(MoonlightHost {
                unique_id,
                name: info.host_name,
                address: address.to_string(),
                http_port: port,
                https_port: info.https_port,
                paired,
                codecs,
                app_version: Some(info.app_version.to_string()),
            })
        })
    }
}

/// What the browse knows about one advertised service.
struct Entry {
    address: IpAddr,
    port: u16,
    /// Last time the host answered.
    answered: tokio::time::Instant,
    host: Option<MoonlightHost>,
}

/// What the agent needs from the Moonlight side (a trait so tests can stand in
/// for a real host).
pub trait Control: Send + Sync + 'static {
    /// The hosts found now, changing as they come and go.
    fn hosts(&self) -> watch::Receiver<Vec<MoonlightHost>>;
    /// Pairings that ended.
    fn paired(&self) -> broadcast::Receiver<Paired>;
    /// Starts pairing; answers with the PIN once there is one.
    fn pair(self: Arc<Self>, unique_id: String) -> BoxFuture<'static, Result<String>>;
    /// The apps a paired host offers.
    fn apps(self: Arc<Self>, unique_id: String) -> BoxFuture<'static, Result<Vec<MoonlightApp>>>;
}

impl Control for Moonlight {
    fn hosts(&self) -> watch::Receiver<Vec<MoonlightHost>> {
        self.hosts.subscribe()
    }

    fn paired(&self) -> broadcast::Receiver<Paired> {
        self.paired.subscribe()
    }

    fn pair(self: Arc<Self>, unique_id: String) -> BoxFuture<'static, Result<String>> {
        Box::pin(async move { Moonlight::pair(&self, &unique_id).await })
    }

    fn apps(self: Arc<Self>, unique_id: String) -> BoxFuture<'static, Result<Vec<MoonlightApp>>> {
        Box::pin(async move { Moonlight::apps(&self, &unique_id).await })
    }
}

/// A pairing's end, for the agent to tell the portal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paired {
    pub unique_id: String,
    pub ok: bool,
    pub message: Option<String>,
}

pub struct Moonlight {
    store: Arc<Store>,
    probe: Arc<dyn Probe>,
    hosts: watch::Sender<Vec<MoonlightHost>>,
    paired: broadcast::Sender<Paired>,
    /// Pairings in progress: host id → its PIN.
    pairing: Mutex<HashMap<String, String>>,
    rescan: mpsc::UnboundedSender<()>,
}

impl Moonlight {
    /// Starts browsing the LAN. Returns once the browse is running.
    pub fn spawn(dir: PathBuf) -> Result<Arc<Self>> {
        let store = Arc::new(Store::new(dir));
        let (seen_tx, seen_rx) = mpsc::channel(64);
        let daemon = ServiceDaemon::new()?;
        let events = daemon.browse(SERVICE)?;
        info!("looking for Moonlight hosts on the LAN ({SERVICE})");
        tokio::spawn(async move {
            // The daemon stops when this is dropped, which is when the task ends.
            let _daemon = daemon;
            while let Ok(event) = events.recv_async().await {
                let seen = match event {
                    ServiceEvent::ServiceResolved(service) => {
                        let mut addresses: Vec<IpAddr> = service
                            .get_addresses()
                            .iter()
                            .map(|a| a.to_ip_addr())
                            .collect();
                        // IPv4 first: link-local IPv6 isn't reachable without a scope.
                        addresses.sort_by_key(|a| (a.is_ipv6(), *a));
                        match addresses.first() {
                            Some(&address) => Seen::Resolved {
                                name: service.get_fullname().to_string(),
                                address,
                                port: service.get_port(),
                            },
                            None => continue,
                        }
                    }
                    ServiceEvent::ServiceRemoved(_, name) => Seen::Removed { name },
                    _ => continue,
                };
                if seen_tx.send(seen).await.is_err() {
                    break;
                }
            }
            warn!("the Moonlight browse stopped; hosts won't be found until the agent restarts");
        });
        let probe = Arc::new(HttpProbe::new(Arc::clone(&store)));
        Ok(Self::with_parts(store, seen_rx, probe))
    }

    /// The manager over any source of sightings and any prober (tests).
    pub fn with_parts(
        store: Arc<Store>,
        seen: mpsc::Receiver<Seen>,
        probe: Arc<dyn Probe>,
    ) -> Arc<Self> {
        let (hosts, _) = watch::channel(Vec::new());
        let (paired, _) = broadcast::channel(16);
        let (rescan, rescan_rx) = mpsc::unbounded_channel();
        let this = Arc::new(Self {
            store,
            probe,
            hosts,
            paired,
            pairing: Mutex::default(),
            rescan,
        });
        tokio::spawn(Arc::clone(&this).run(seen, rescan_rx));
        this
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    fn found(&self, unique_id: &str) -> Result<MoonlightHost> {
        self.hosts
            .borrow()
            .iter()
            .find(|h| h.unique_id == unique_id)
            .cloned()
            .ok_or_else(|| anyhow!("this node doesn't see that Moonlight host right now"))
    }

    fn address_of(host: &MoonlightHost) -> Result<IpAddr> {
        host.address
            .parse()
            .with_context(|| format!("the host's address {:?}", host.address))
    }

    /// Starts pairing with a found host: returns the PIN to enter on it and
    /// goes on in the background, reporting through [`Self::paired`]. A pairing
    /// already waiting for that host gives its PIN again.
    pub async fn pair(self: &Arc<Self>, unique_id: &str) -> Result<String> {
        let host = self.found(unique_id)?;
        if let Some(pin) = self.pairing.lock().expect("pairing lock").get(unique_id) {
            return Ok(pin.clone());
        }
        let address = Self::address_of(&host)?;
        let store = Arc::clone(&self.store);
        let (cert, key) = tokio::task::spawn_blocking({
            let store = Arc::clone(&store);
            move || store.client_identity()
        })
        .await??;
        let crypto = RustCryptoBackend;
        let pin = PairPin::new_random(&crypto).map_err(|e| anyhow!("{e:?}"))?;
        let pin_text = pin.to_string();
        let client = store.client(address, host.http_port, None).await?;
        self.pairing
            .lock()
            .expect("pairing lock")
            .insert(unique_id.to_string(), pin_text.clone());
        let this = Arc::clone(self);
        let id = unique_id.to_string();
        info!(host = %host.name, "pairing: waiting for the PIN on the host");
        tokio::spawn(async move {
            let outcome = tokio::time::timeout(
                PAIR_TIMEOUT,
                client.pair(&cert, &key, CLIENT_NAME.into(), pin, crypto),
            )
            .await;
            let outcome = match outcome {
                Err(_) => Err(anyhow!(
                    "the PIN wasn't entered on the host within 5 minutes"
                )),
                Ok(Err(err)) => Err(anyhow!("{err:?}")),
                Ok(Ok(())) => match client.identity().await {
                    Some((_, _, server)) => store.save_server(&id, &server),
                    None => Err(anyhow!("paired, but the host's certificate is missing")),
                },
            };
            this.pairing.lock().expect("pairing lock").remove(&id);
            let paired = match outcome {
                Ok(()) => {
                    info!(%id, "paired");
                    Paired {
                        unique_id: id,
                        ok: true,
                        message: None,
                    }
                }
                Err(err) => {
                    warn!(%id, "pairing failed: {err:#}");
                    Paired {
                        unique_id: id,
                        ok: false,
                        message: Some(format!("{err:#}")),
                    }
                }
            };
            let _ = this.paired.send(paired);
            let _ = this.rescan.send(());
        });
        Ok(pin_text)
    }

    /// The apps a paired host offers.
    pub async fn apps(&self, unique_id: &str) -> Result<Vec<MoonlightApp>> {
        let host = self.found(unique_id)?;
        if self.store.server_identity(unique_id)?.is_none() {
            bail!("this node isn't paired with {}", host.name);
        }
        let client = self
            .store
            .client(Self::address_of(&host)?, host.http_port, Some(unique_id))
            .await?;
        let apps = client
            .app_list()
            .await
            .map_err(|e| anyhow!("listing {}'s apps: {e:?}", host.name))?;
        Ok(apps
            .into_iter()
            .map(|a| MoonlightApp {
                id: a.id.0,
                name: a.title,
                hdr: a.is_hdr_supported,
            })
            .collect())
    }

    async fn run(
        self: Arc<Self>,
        mut seen: mpsc::Receiver<Seen>,
        mut rescan: mpsc::UnboundedReceiver<()>,
    ) {
        let mut entries: HashMap<String, Entry> = HashMap::new();
        let mut tick = tokio::time::interval(REFRESH);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        tick.tick().await;
        loop {
            tokio::select! {
                event = seen.recv() => match event {
                    Some(Seen::Resolved { name, address, port }) => {
                        // A resolve is news: read it now. A service lost is left
                        // to the three-minute rule.
                        let entry = entries.entry(name.clone()).or_insert(Entry {
                            address,
                            port,
                            answered: tokio::time::Instant::now(),
                            host: None,
                        });
                        entry.address = address;
                        entry.port = port;
                        self.refresh(&mut entries, Some(&name)).await;
                    }
                    Some(Seen::Removed { .. }) => {}
                    None => return,
                },
                _ = tick.tick() => self.refresh(&mut entries, None).await,
                Some(()) = rescan.recv() => self.refresh(&mut entries, None).await,
            }
        }
    }

    /// Reads `only` (or every service), drops what has been silent too long,
    /// and publishes the list if it changed.
    async fn refresh(&self, entries: &mut HashMap<String, Entry>, only: Option<&str>) {
        let names: Vec<String> = entries
            .keys()
            .filter(|n| only.is_none_or(|o| o == n.as_str()))
            .cloned()
            .collect();
        let probes = names.iter().map(|name| {
            let entry = &entries[name];
            let (address, port) = (entry.address, entry.port);
            async move {
                let read =
                    tokio::time::timeout(PROBE_TIMEOUT, self.probe.probe(address, port)).await;
                match read {
                    Ok(Ok(host)) => Some(host),
                    Ok(Err(err)) => {
                        debug!(%address, "no /serverinfo: {err:#}");
                        None
                    }
                    Err(_) => {
                        debug!(%address, "/serverinfo timed out");
                        None
                    }
                }
            }
        });
        let results = join_all(probes).await;
        let now = tokio::time::Instant::now();
        for (name, host) in names.into_iter().zip(results) {
            if let Some(host) = host
                && let Some(entry) = entries.get_mut(&name)
            {
                entry.answered = now;
                entry.host = Some(host);
            }
        }
        entries.retain(|_, e| now.duration_since(e.answered) < GONE_AFTER);
        let list = list_of(entries);
        self.hosts.send_if_modified(|current| {
            if *current == list {
                false
            } else {
                *current = list;
                true
            }
        });
    }
}

/// The hosts the entries hold, once each, by name.
fn list_of(entries: &HashMap<String, Entry>) -> Vec<MoonlightHost> {
    let mut by_id: HashMap<&str, &MoonlightHost> = HashMap::new();
    for entry in entries.values() {
        if let Some(host) = &entry.host {
            by_id.insert(&host.unique_id, host);
        }
    }
    let mut list: Vec<MoonlightHost> = by_id.into_values().cloned().collect();
    list.sort_by(|a, b| {
        a.name
            .cmp(&b.name)
            .then_with(|| a.unique_id.cmp(&b.unique_id))
    });
    list
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap as Map;

    /// Answers from a table; a missing address is an unreachable host.
    #[derive(Default)]
    struct Fake {
        hosts: Mutex<Map<IpAddr, MoonlightHost>>,
    }

    impl Probe for Fake {
        fn probe(&self, address: IpAddr, _port: u16) -> BoxFuture<'_, Result<MoonlightHost>> {
            let host = self.hosts.lock().unwrap().get(&address).cloned();
            Box::pin(async move { host.ok_or_else(|| anyhow!("unreachable")) })
        }
    }

    fn host(id: &str, name: &str, ip: &str, paired: bool) -> MoonlightHost {
        MoonlightHost {
            unique_id: id.into(),
            name: name.into(),
            address: ip.into(),
            http_port: 47989,
            https_port: 47984,
            paired,
            codecs: vec!["h264".into()],
            app_version: None,
        }
    }

    fn resolved(name: &str, ip: &str) -> Seen {
        Seen::Resolved {
            name: name.into(),
            address: ip.parse().unwrap(),
            port: 47989,
        }
    }

    async fn settle() {
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test(start_paused = true)]
    async fn hosts_appear_change_and_go_after_three_silent_minutes() {
        let dir = tempfile::tempdir().unwrap();
        let fake = Arc::new(Fake::default());
        let a: IpAddr = "10.0.0.2".parse().unwrap();
        fake.hosts
            .lock()
            .unwrap()
            .insert(a, host("AA", "pc", "10.0.0.2", false));
        let (tx, rx) = mpsc::channel(8);
        let ml = Moonlight::with_parts(Arc::new(Store::new(dir.path().into())), rx, fake.clone());
        let mut hosts = Control::hosts(&*ml);
        assert!(hosts.borrow().is_empty());

        // A resolve is read at once and published.
        tx.send(resolved("pc._nvstream._tcp.local.", "10.0.0.2"))
            .await
            .unwrap();
        hosts.changed().await.unwrap();
        assert_eq!(
            hosts.borrow_and_update().clone(),
            vec![host("AA", "pc", "10.0.0.2", false)]
        );

        // Nothing changed: the minute's re-read publishes nothing.
        tokio::time::sleep(REFRESH + Duration::from_secs(1)).await;
        settle().await;
        assert!(!hosts.has_changed().unwrap());

        // It got paired: the next re-read publishes the new state.
        fake.hosts
            .lock()
            .unwrap()
            .insert(a, host("AA", "pc", "10.0.0.2", true));
        tokio::time::sleep(REFRESH).await;
        settle().await;
        assert!(hosts.has_changed().unwrap());
        assert!(hosts.borrow_and_update()[0].paired);

        // It stops answering; a goodbye alone doesn't drop it, three minutes do.
        fake.hosts.lock().unwrap().clear();
        tx.send(Seen::Removed {
            name: "pc._nvstream._tcp.local.".into(),
        })
        .await
        .unwrap();
        tokio::time::sleep(REFRESH * 2).await;
        settle().await;
        assert_eq!(hosts.borrow().len(), 1);
        tokio::time::sleep(REFRESH * 2).await;
        settle().await;
        assert!(hosts.borrow().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn one_host_seen_twice_is_listed_once_and_the_list_is_sorted() {
        let dir = tempfile::tempdir().unwrap();
        let fake = Arc::new(Fake::default());
        for (ip, h) in [
            ("10.0.0.2", host("AA", "zed", "10.0.0.2", false)),
            ("10.0.0.3", host("AA", "zed", "10.0.0.3", false)),
            ("10.0.0.4", host("BB", "alpha", "10.0.0.4", true)),
        ] {
            fake.hosts.lock().unwrap().insert(ip.parse().unwrap(), h);
        }
        let (tx, rx) = mpsc::channel(8);
        let ml = Moonlight::with_parts(Arc::new(Store::new(dir.path().into())), rx, fake);
        let mut hosts = Control::hosts(&*ml);
        for (n, ip) in [("a", "10.0.0.2"), ("b", "10.0.0.3"), ("c", "10.0.0.4")] {
            tx.send(resolved(n, ip)).await.unwrap();
        }
        settle().await;
        let _ = hosts.changed().await;
        settle().await;
        let names: Vec<String> = hosts.borrow().iter().map(|h| h.name.clone()).collect();
        assert_eq!(names, ["alpha", "zed"]);
    }

    #[cfg(unix)]
    #[test]
    fn the_identity_directory_is_private_and_the_client_key_persists() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let store = Store::new(dir(root.path()));
        let (cert, _) = store.client_identity().unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&root.path().join("node")), 0o700);
        assert_eq!(mode(store.dir()), 0o700);
        assert_eq!(mode(&store.dir().join(CLIENT_KEY)), 0o600);
        assert_eq!(mode(&store.dir().join(CLIENT_CERT)), 0o600);

        // The second call reads the same identity back.
        let (again, _) = store.client_identity().unwrap();
        assert_eq!(cert.to_pem().to_string(), again.to_pem().to_string());

        // A host's certificate lands in its own private directory.
        assert!(store.server_identity("AB12").unwrap().is_none());
        assert_eq!(store.paired_count(), 0);
        store
            .save_server(
                "AB12",
                &ServerIdentifier::from_pem(Pem::new("CERTIFICATE", vec![1, 2, 3])),
            )
            .unwrap();
        assert!(store.server_identity("AB12").unwrap().is_some());
        assert_eq!(store.paired_count(), 1);
        assert_eq!(mode(&store.dir().join("hosts/AB12")), 0o700);
        assert_eq!(
            mode(&store.dir().join("hosts/AB12").join(SERVER_CERT)),
            0o600
        );
        assert!(
            store
                .save_server(
                    "../x",
                    &ServerIdentifier::from_pem(Pem::new("CERTIFICATE", vec![1]))
                )
                .is_err()
        );
    }

    #[test]
    fn host_ids_are_hex() {
        assert!(valid_unique_id("0123ABCDEF"));
        assert!(!valid_unique_id(""));
        assert!(!valid_unique_id("../etc"));
        assert!(!valid_unique_id("a b"));
    }
}

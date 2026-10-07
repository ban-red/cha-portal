//! The front of a GameStream host: what a Moonlight client talks to before
//! a stream exists. nvhttp (server info, pairing, the app list, launch,
//! resume, cancel), RTSP (negotiating the stream) and the mDNS
//! advertisement. When the client says `PLAY`, the front gives a
//! [`SessionHandoff`](crate::handoff::SessionHandoff) to the [`Directory`], which runs the media half
//! wherever it likes.

use std::net::SocketAddr;
use std::sync::Arc;

use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio::task::JoinSet;

use crate::config::HostConfig;
use crate::directory::{Directory, PairingStore};

pub mod identity;
pub(crate) mod mdns;
pub(crate) mod nvhttp;
pub(crate) mod pairing;
pub(crate) mod rtsp;
pub(crate) mod session;

use identity::Identity;
use session::Sessions;

#[derive(Debug, thiserror::Error)]
pub enum HostError {
    #[error("binding {what}: {source}")]
    Bind {
        what: &'static str,
        source: std::io::Error,
    },
    #[error(transparent)]
    Identity(#[from] identity::IdentityError),
    #[error("the host key can't sign: {0}")]
    Key(String),
    #[error("missing {0}")]
    Missing(&'static str),
}

/// The assembled front, ready to start.
pub struct Host {
    config: HostConfig,
    identity: Identity,
    directory: Arc<dyn Directory>,
    store: Arc<dyn PairingStore>,
}

pub struct HostBuilder {
    config: HostConfig,
    identity: Option<Identity>,
    directory: Option<Arc<dyn Directory>>,
    store: Option<Arc<dyn PairingStore>>,
}

impl Host {
    pub fn builder(config: HostConfig) -> HostBuilder {
        HostBuilder {
            config,
            identity: None,
            directory: None,
            store: None,
        }
    }

    /// Binds the ports and serves until shut down.
    pub async fn run(self) -> Result<(), HostError> {
        self.start().await?.wait().await;
        Ok(())
    }

    /// Binds the ports and starts serving in the background.
    pub async fn start(self) -> Result<RunningHost, HostError> {
        let Host {
            mut config,
            identity,
            directory,
            store,
        } = self;
        let bind = |what, port| async move {
            TcpListener::bind(SocketAddr::new(config.bind, port))
                .await
                .map_err(|source| HostError::Bind { what, source })
        };
        let http = bind("HTTP", config.ports.http).await?;
        let https = bind("HTTPS", config.ports.https).await?;
        let rtsp = bind("RTSP", config.ports.rtsp).await?;
        let port = |l: &TcpListener| l.local_addr().map(|a| a.port()).unwrap_or(0);
        // A port of 0 asked for any; the clients are told the real ones.
        config.ports = crate::config::Ports {
            http: port(&http),
            https: port(&https),
            rtsp: port(&rtsp),
        };
        let addrs = HostAddrs {
            http: http.local_addr().map_err(|source| HostError::Bind {
                what: "HTTP",
                source,
            })?,
            https: https.local_addr().map_err(|source| HostError::Bind {
                what: "HTTPS",
                source,
            })?,
            rtsp: rtsp.local_addr().map_err(|source| HostError::Bind {
                what: "RTSP",
                source,
            })?,
        };

        let identity = Arc::new(identity);
        let config = Arc::new(config);
        let (stop, shutdown) = watch::channel(false);
        let sessions = Arc::new(Sessions::default());
        let pairing = pairing::Pairing::new(
            identity.clone(),
            store.clone(),
            directory.clone(),
            config.pin_timeout,
        )
        .map_err(|e| HostError::Key(e.to_string()))?;
        let acceptor = nvhttp::tls::acceptor(&identity)?;

        let nv = Arc::new(nvhttp::Context {
            config: config.clone(),
            directory: directory.clone(),
            store,
            pairing,
            sessions: sessions.clone(),
            shutdown: shutdown.clone(),
        });
        let rt = Arc::new(rtsp::Context {
            config: config.clone(),
            directory: directory.clone(),
            sessions: sessions.clone(),
            shutdown,
        });

        let mut tasks = JoinSet::new();
        tasks.spawn(nvhttp::serve(http, None, nv.clone()));
        tasks.spawn(nvhttp::serve(https, Some(acceptor), nv));
        tasks.spawn(rtsp::serve(rtsp, rt));

        let advertisement = if config.mdns {
            match mdns::Advertisement::start(&config.name, config.bind, config.ports.http) {
                Ok(a) => Some(a),
                // Discovery is a convenience: clients can add the host by address.
                Err(e) => {
                    tracing::warn!("mDNS advertisement failed: {e}");
                    None
                }
            }
        } else {
            None
        };
        Ok(RunningHost {
            addrs,
            stop,
            tasks,
            sessions,
            directory,
            identity,
            _advertisement: advertisement,
        })
    }
}

impl HostBuilder {
    pub fn directory(mut self, directory: Arc<dyn Directory>) -> Self {
        self.directory = Some(directory);
        self
    }
    pub fn pairing_store(mut self, store: Arc<dyn PairingStore>) -> Self {
        self.store = Some(store);
        self
    }
    /// The host's certificate and key; generated (and not kept) when not given.
    pub fn identity(mut self, identity: Identity) -> Self {
        self.identity = Some(identity);
        self
    }

    pub fn build(self) -> Result<Host, HostError> {
        Ok(Host {
            identity: match self.identity {
                Some(i) => i,
                None => Identity::generate()?,
            },
            directory: self.directory.ok_or(HostError::Missing("a Directory"))?,
            store: self.store.ok_or(HostError::Missing("a PairingStore"))?,
            config: self.config,
        })
    }
}

/// The addresses the front listens on.
#[derive(Clone, Copy, Debug)]
pub struct HostAddrs {
    pub http: SocketAddr,
    pub https: SocketAddr,
    pub rtsp: SocketAddr,
}

/// A started front. Dropping it stops the servers.
pub struct RunningHost {
    addrs: HostAddrs,
    stop: watch::Sender<bool>,
    tasks: JoinSet<()>,
    sessions: Arc<Sessions>,
    directory: Arc<dyn Directory>,
    identity: Arc<Identity>,
    _advertisement: Option<mdns::Advertisement>,
}

/// The embedder's grip on a running host.
#[derive(Clone)]
pub struct HostHandle {
    sessions: Arc<Sessions>,
    directory: Arc<dyn Directory>,
}

impl HostHandle {
    /// The media of stream `session_id` (from its [`SessionHandoff`](crate::handoff::SessionHandoff)) ended on
    /// its own: the client left, or the media half failed. The session stays,
    /// waiting for its client to resume.
    pub fn media_ended(&self, session_id: u64) {
        self.sessions.media_ended(session_id);
    }

    /// Ends the running session, whoever launched it: media stopped, app
    /// quit through the Directory.
    pub async fn cancel_session(&self) {
        self.sessions.cancel_any(self.directory.as_ref()).await;
    }

    /// The app of the running session, if any.
    pub fn running_app(&self) -> Option<u32> {
        Some(self.sessions.current_app()).filter(|&id| id != 0)
    }
}

impl RunningHost {
    pub fn addrs(&self) -> HostAddrs {
        self.addrs
    }

    pub fn handle(&self) -> HostHandle {
        HostHandle {
            sessions: self.sessions.clone(),
            directory: self.directory.clone(),
        }
    }

    pub fn identity(&self) -> &Identity {
        &self.identity
    }

    /// Stops the servers and waits for them. A running session is left to the
    /// Directory; call [`HostHandle::cancel_session`] first to end it.
    pub async fn shutdown(mut self) {
        self.stop.send_replace(true);
        while self.tasks.join_next().await.is_some() {}
    }

    /// Returns when the servers have stopped.
    pub async fn wait(mut self) {
        while self.tasks.join_next().await.is_some() {}
    }
}

impl Drop for RunningHost {
    fn drop(&mut self) {
        self.stop.send_replace(true);
    }
}

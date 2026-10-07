//! The node's GameStream host, run for real on loopback and driven by
//! cha-gamestream's client, with a fake portal on the other end of
//! the agent's channel: it answers pairing requests with a PIN, as the portal
//! does when a user types one, and sends the paired devices back.

#![cfg(feature = "gamestream")]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use cha_gamestream::VideoCodec;
use cha_gamestream::client::front::{ClientIdentity, HostClient, StreamRequest};
use cha_node::environments::{GameStreamAccess, GameStreamPorts, RunningEnvironment};
use cha_node::gamestream::{Config, Environments, GameStream, Ports, catalog};
use cha_wire::{GameStreamDevice, NodeRequest, PortalRequest, PortalResponse, ToPortal};
use tokio::sync::mpsc;

#[derive(Default)]
struct FakeEnvironments(Mutex<Vec<RunningEnvironment>>);

impl Environments for FakeEnvironments {
    fn running(&self) -> Vec<RunningEnvironment> {
        self.0.lock().unwrap().clone()
    }
}

fn environment(id: &str, owner: &str, template: &str, streamer: u16) -> RunningEnvironment {
    RunningEnvironment {
        id: id.into(),
        owner: owner.into(),
        template: template.into(),
        gateway: false,
        gamestream: Some(GameStreamAccess {
            http_port: streamer,
            ports: GameStreamPorts {
                video: 7700,
                control: 7701,
                audio: 7702,
            },
            secret: "unused".into(),
        }),
    }
}

/// An environment's streamer, as far as the host needs it: it takes a
/// session's media, reports it running and lets it go. Returns its port.
async fn fake_streamer() -> u16 {
    use axum::Json;
    use axum::Router;
    use axum::extract::{Path, State};
    use axum::http::StatusCode;
    use axum::routing::{delete, get, post};

    type Running = Arc<Mutex<Option<u64>>>;
    let running = Running::default();
    let app = Router::new()
        .route(
            "/gamestream/session",
            post(
                |State(running): State<Running>, Json(handoff): Json<serde_json::Value>| async move {
                    *running.lock().unwrap() = handoff["session_id"].as_u64();
                    StatusCode::OK
                },
            ),
        )
        .route(
            "/gamestream/session/{id}",
            delete(|State(running): State<Running>, Path(_): Path<u64>| async move {
                *running.lock().unwrap() = None;
                StatusCode::OK
            }),
        )
        .route(
            "/gamestream/status",
            get(|State(running): State<Running>| async move {
                Json(serde_json::json!({ "session_id": *running.lock().unwrap() }))
            }),
        )
        .with_state(running);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    port
}

struct Rig {
    host: Arc<GameStream>,
    environments: Arc<FakeEnvironments>,
    /// What the host said to the portal, after the fake portal acted on it.
    said: mpsc::UnboundedReceiver<ToPortal>,
    /// The PIN the fake portal's user types; "" leaves requests unanswered.
    typed: Arc<Mutex<String>>,
    /// When set, the fake portal refuses launches with this reason.
    refuse: Arc<Mutex<Option<String>>>,
    identity: ClientIdentity,
    /// The host's certificate, as a paired client pins it.
    server: String,
    _dir: tempfile::TempDir,
}

impl Rig {
    async fn start() -> Self {
        cha_node::init_tls();
        let dir = tempfile::tempdir().unwrap();
        let mut config = Config::new(
            "test node",
            dir.path().to_path_buf(),
            Ports {
                http: 0,
                https: 0,
                rtsp: 0,
            },
        );
        config.bind = "127.0.0.1".parse().unwrap();
        config.mdns = false;
        // A node without a GPU: what needs one isn't offered.
        config.directory.gpu = false;
        let streamer = fake_streamer().await;
        let environments = Arc::new(FakeEnvironments(Mutex::new(vec![
            environment("e-chrome", "bob", "chrome", streamer),
            environment("e-firefox", "alice", "firefox", streamer),
        ])));
        let host = GameStream::start(config, environments.clone())
            .await
            .unwrap();
        // As the welcome of a portal that answers launches says.
        host.set_portal_launches(true);
        let refuse = Arc::new(Mutex::new(None::<String>));
        let refusal = refuse.clone();
        let started_here = environments.clone();
        let mut outgoing = host.outgoing();
        let (tx, said) = mpsc::unbounded_channel();
        let typed = Arc::new(Mutex::new(String::new()));
        let portal = host.clone();
        let pin = typed.clone();
        tokio::spawn(async move {
            while let Ok(msg) = outgoing.recv().await {
                match &msg {
                    ToPortal::GameStreamPairRequest { attempt_id, .. } => {
                        let pin = pin.lock().unwrap().clone();
                        if !pin.is_empty() {
                            portal
                                .handle(NodeRequest::GameStreamPin {
                                    attempt_id: attempt_id.clone(),
                                    pin,
                                    user_id: "bob".into(),
                                })
                                .unwrap();
                        }
                    }
                    // The portal keeps the device and sends the node its list.
                    ToPortal::GameStreamPaired {
                        fingerprint,
                        unique_id,
                        name,
                        ..
                    } => {
                        portal
                            .handle(NodeRequest::GameStreamDevices {
                                devices: vec![GameStreamDevice {
                                    fingerprint: fingerprint.clone(),
                                    unique_id: Some(unique_id.clone()),
                                    name: name.clone(),
                                    user_id: "bob".into(),
                                }],
                            })
                            .unwrap();
                    }
                    // The portal starts what a client launched, on this node: the
                    // environment is running by the time it says so.
                    ToPortal::Request {
                        id,
                        request:
                            PortalRequest::GameStreamLaunch {
                                user_id,
                                template_id,
                            },
                    } => {
                        let answer = match refusal.lock().unwrap().clone() {
                            Some(why) => Err(why),
                            None => {
                                let started = format!("e-{user_id}-{template_id}");
                                started_here.0.lock().unwrap().push(environment(
                                    &started,
                                    user_id,
                                    template_id,
                                    streamer,
                                ));
                                Ok(PortalResponse::GameStreamLaunched {
                                    environment_id: started,
                                    created: true,
                                })
                            }
                        };
                        portal.answer(*id, answer);
                    }
                    ToPortal::GameStreamUnpaired { .. } => {
                        portal
                            .handle(NodeRequest::GameStreamDevices {
                                devices: Vec::new(),
                            })
                            .unwrap();
                    }
                    _ => {}
                }
                let _ = tx.send(msg);
            }
        });
        let identity = ClientIdentity::generate().unwrap();
        let cert_pem =
            std::fs::read_to_string(dir.path().join("node/gamestream/cert.pem")).unwrap();
        Rig {
            host,
            environments,
            said,
            typed,
            refuse,
            identity,
            server: cert_pem,
            _dir: dir,
        }
    }

    /// A client that doesn't hold the host's certificate yet.
    fn client(&self) -> HostClient {
        HostClient::new(
            &format!("127.0.0.1:{}", self.host.addrs().http.port()),
            self.identity.clone(),
        )
        .unwrap()
        .with_unique_id("client-one")
    }

    /// A client that has the host's certificate pinned, as after pairing.
    fn pinned_client(&self) -> HostClient {
        self.client().with_server_cert(&self.server).unwrap()
    }

    /// The client's identity, as the host names it.
    fn client_fingerprint(&self) -> String {
        self.identity.fingerprint()
    }

    async fn pair(&self, host: &HostClient, shown: &str) -> Result<(), String> {
        host.pair(shown, "TestDevice")
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// The next thing the host said, within `secs`.
    async fn next(&mut self, secs: u64) -> ToPortal {
        tokio::time::timeout(Duration::from_secs(secs), self.said.recv())
            .await
            .expect("the host said nothing in time")
            .unwrap()
    }
}

/// `/launch` and the RTSP negotiation, as a client does it (without
/// connecting the media).
async fn launch(host: &HostClient, app: u32) -> Result<(), String> {
    let mut request = StreamRequest::new(app, 1280, 720, 60);
    request.codecs = vec![VideoCodec::H264];
    host.launch(&request)
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_device_pairs_through_the_portal_lists_its_owners_apps_launches_and_unpairs() {
    let mut rig = Rig::start().await;
    *rig.typed.lock().unwrap() = "4321".into();
    let host = rig.client();

    // The client shows 4321; the portal's user types 4321.
    rig.pair(&host, "4321").await.unwrap();
    let ToPortal::GameStreamPairRequest {
        device_name,
        address,
        expires_in_secs,
        ..
    } = rig.next(5).await
    else {
        panic!("the portal was asked first");
    };
    assert_eq!(
        (device_name.as_str(), address.as_str()),
        ("TestDevice", "127.0.0.1")
    );
    assert_eq!(expires_in_secs, 120);
    let ToPortal::GameStreamPaired {
        fingerprint,
        unique_id,
        name,
        ..
    } = rig.next(5).await
    else {
        panic!("then told it paired");
    };
    assert_eq!(fingerprint, rig.client_fingerprint());
    assert_eq!(
        (unique_id.as_str(), name.as_str()),
        ("client-one", "TestDevice")
    );

    // The list the portal sent makes the client bob's: it sees what he can run
    // on this node (the catalog, less what needs a GPU), running or not.
    let host = rig.pinned_client();
    assert!(host.server_info().await.unwrap().paired);
    let apps = host.app_list().await.unwrap();
    let titles: Vec<&str> = apps.iter().map(|a| a.title.as_str()).collect();
    let offered: Vec<String> = catalog::offered(false).map(|t| t.name.clone()).collect();
    assert_eq!(titles.len(), offered.len(), "{titles:?}");
    assert!(offered.iter().all(|n| titles.contains(&n.as_str())));
    assert!(!titles.contains(&"Steam"), "it needs a GPU");
    let id_of = |title: &str| apps.iter().find(|a| a.title == title).unwrap().id;
    assert_eq!(id_of("Google Chrome"), catalog::app_id("chrome"));

    // Chrome runs already: launching it resumes that. Cancelling quits the
    // session; the environment is not the host's to stop.
    launch(&host, id_of("Google Chrome")).await.unwrap();
    host.cancel().await.unwrap();
    assert!(rig.environments.0.lock().unwrap().len() == 2);

    // Firefox runs for alice, not bob: the portal is asked to start his, and
    // the launch returns once it runs.
    launch(&host, id_of("Firefox")).await.unwrap();
    let ToPortal::Request {
        request:
            PortalRequest::GameStreamLaunch {
                user_id,
                template_id,
            },
        ..
    } = rig.next(5).await
    else {
        panic!("the portal was asked to start it");
    };
    assert_eq!((user_id.as_str(), template_id.as_str()), ("bob", "firefox"));
    assert!(
        rig.environments
            .0
            .lock()
            .unwrap()
            .iter()
            .any(|e| e.id == "e-bob-firefox")
    );
    host.cancel().await.unwrap();

    // A portal that refuses says why, and the client's launch fails with it.
    *rig.refuse.lock().unwrap() = Some("your KDE Plasma is already running on box".into());
    let refused = launch(&host, id_of("KDE Plasma")).await.unwrap_err();
    assert!(refused.contains("already running on box"), "{refused}");
    assert!(matches!(rig.next(5).await, ToPortal::Request { .. }));
    // What this node can't run isn't an app at all, and nobody is asked.
    assert!(launch(&host, catalog::app_id("steam")).await.is_err());
    *rig.refuse.lock().unwrap() = None;

    // The client unpairs itself over HTTPS: the portal hears, and it is out.
    host.unpair().await.unwrap();
    let ToPortal::GameStreamUnpaired { fingerprint } = rig.next(5).await else {
        panic!("the portal was told it unpaired");
    };
    assert_eq!(fingerprint, rig.client_fingerprint());
    assert!(
        host.app_list().await.is_err(),
        "an unpaired certificate lists nothing"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_wrong_pin_pairs_nobody_and_the_portal_is_told_it_failed() {
    let mut rig = Rig::start().await;
    *rig.typed.lock().unwrap() = "9999".into();
    let host = rig.client();

    assert!(
        rig.pair(&host, "1111").await.is_err(),
        "the client refuses a host whose PIN differs"
    );
    let ToPortal::GameStreamPairRequest { attempt_id, .. } = rig.next(5).await else {
        panic!("the portal was asked first");
    };
    // The host can't see the client give up, so it says so after a while.
    let ToPortal::GameStreamPairFailed {
        attempt_id: failed, ..
    } = rig.next(20).await
    else {
        panic!("then told it failed");
    };
    assert_eq!(failed, attempt_id);
    assert!(
        !rig.pinned_client().server_info().await.unwrap().paired,
        "nobody was paired"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn nobody_is_paired_until_the_portal_says_so() {
    let rig = Rig::start().await;
    // A client the portal never heard of, with the host's certificate pinned.
    let host = rig.pinned_client();
    assert!(
        !host.server_info().await.unwrap().paired,
        "an unlisted certificate is paired"
    );
    assert!(
        host.app_list().await.is_err(),
        "an unlisted certificate got the app list"
    );
    // The portal's list is what pairs: after it, the same certificate works.
    rig.host
        .handle(NodeRequest::GameStreamDevices {
            devices: vec![GameStreamDevice {
                fingerprint: rig.client_fingerprint(),
                unique_id: None,
                name: "TestDevice".into(),
                user_id: "alice".into(),
            }],
        })
        .unwrap();
    assert!(host.server_info().await.unwrap().paired);
    let apps = host.app_list().await.unwrap();
    assert!(
        apps.iter().any(|a| a.title == "Firefox"),
        "the owner in the list decides what it sees"
    );
    // And a list without it takes the access away again.
    rig.host
        .handle(NodeRequest::GameStreamDevices {
            devices: Vec::new(),
        })
        .unwrap();
    assert!(host.app_list().await.is_err());
}

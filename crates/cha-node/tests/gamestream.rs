//! The node's GameStream host, run for real on loopback and driven by
//! moonlight-common-rust as the client, with a fake portal on the other end of
//! the agent's channel: it answers pairing requests with a PIN, as the portal
//! does when a user types one, and sends the paired devices back.

#![cfg(feature = "gamestream")]

use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cha_gamestream::front::identity::fingerprint;
use cha_node::environments::{GameStreamAccess, GameStreamPorts, RunningEnvironment};
use cha_node::gamestream::{Config, Environments, GameStream, Ports};
use cha_node::moonlight::Store;
use cha_wire::{GameStreamDevice, NodeRequest, ToPortal};
use moonlight_common::AppId;
use moonlight_common::crypto::rustcrypto::RustCryptoBackend;
use moonlight_common::high::tokio::MoonlightHost;
use moonlight_common::http::client::tokio_hyper::TokioHyperClient;
use moonlight_common::http::pair::PairPin;
use moonlight_common::http::{ClientIdentifier, ClientSecret, ServerIdentifier};
use moonlight_common::stream::audio::AudioConfig;
use moonlight_common::stream::control::ActiveGamepads;
use moonlight_common::stream::proto::MoonlightStreamSetup;
use moonlight_common::stream::video::{ColorRange, ColorSpace, VideoFormats};
use moonlight_common::stream::{
    AesIv, AesKey, EncryptionFlags, MoonlightStreamSettings, StreamingConfig,
};
use pem::Pem;
use tokio::sync::mpsc;

struct FakeEnvironments(Vec<RunningEnvironment>);

impl Environments for FakeEnvironments {
    fn running(&self) -> Vec<RunningEnvironment> {
        self.0.clone()
    }
}

fn environment(id: &str, owner: &str, template: &str) -> RunningEnvironment {
    RunningEnvironment {
        id: id.into(),
        owner: owner.into(),
        template: template.into(),
        gateway: false,
        gamestream: Some(GameStreamAccess {
            http_port: 1,
            ports: GameStreamPorts {
                video: 7700,
                control: 7701,
                audio: 7702,
            },
            secret: "unused".into(),
        }),
    }
}

struct Rig {
    host: Arc<GameStream>,
    /// What the host said to the portal, after the fake portal acted on it.
    said: mpsc::UnboundedReceiver<ToPortal>,
    /// The PIN the fake portal's user types; "" leaves requests unanswered.
    typed: Arc<Mutex<String>>,
    client_cert: ClientIdentifier,
    client_key: ClientSecret,
    server: ServerIdentifier,
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
        let environments = FakeEnvironments(vec![
            environment("e-chrome", "bob", "chrome"),
            environment("e-firefox", "alice", "firefox"),
        ]);
        let host = GameStream::start(config, Arc::new(environments))
            .await
            .unwrap();
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
        let (client_cert, client_key) = Store::new(dir.path().join("client"))
            .client_identity()
            .unwrap();
        let cert_pem =
            std::fs::read_to_string(dir.path().join("node/gamestream/cert.pem")).unwrap();
        Rig {
            host,
            said,
            typed,
            client_cert,
            client_key,
            server: ServerIdentifier::from_pem(Pem::from_str(&cert_pem).unwrap()),
            _dir: dir,
        }
    }

    fn client(&self) -> MoonlightHost<TokioHyperClient> {
        MoonlightHost::<TokioHyperClient>::new(
            "127.0.0.1".into(),
            self.host.addrs().http.port(),
            Some("client-one".into()),
        )
        .unwrap()
    }

    /// The client's identity, as the host names it.
    fn client_fingerprint(&self) -> String {
        fingerprint(self.client_cert.to_pem().contents())
    }

    async fn pair(
        &self,
        host: &MoonlightHost<TokioHyperClient>,
        shown: &str,
    ) -> Result<(), String> {
        let d: Vec<u8> = shown.bytes().map(|b| b - b'0').collect();
        let pin = PairPin::new(d[0], d[1], d[2], d[3]).unwrap();
        host.pair(
            &self.client_cert,
            &self.client_key,
            "TestDevice".into(),
            pin,
            RustCryptoBackend,
        )
        .await
        .map_err(|e| format!("{e:?}"))
    }

    /// The next thing the host said, within `secs`.
    async fn next(&mut self, secs: u64) -> ToPortal {
        tokio::time::timeout(Duration::from_secs(secs), self.said.recv())
            .await
            .expect("the host said nothing in time")
            .unwrap()
    }
}

fn settings() -> MoonlightStreamSettings {
    MoonlightStreamSettings {
        width: 1280,
        height: 720,
        fps: 60,
        fps_x100: 6000,
        bitrate: 20_000,
        packet_size: 1392,
        encryption_flags: EncryptionFlags::empty(),
        streaming_remotely: StreamingConfig::Local,
        sops: false,
        hdr: false,
        supported_video_formats: VideoFormats::H264,
        color_space: ColorSpace::Rec709,
        color_range: ColorRange::Limited,
        local_audio_play_mode: false,
        audio_config: AudioConfig::STEREO,
        gamepads_attached: ActiveGamepads::empty(),
        gamepads_persist_after_disconnect: false,
        enable_mic: false,
    }
}

/// `/launch` as a client does it, without connecting the stream.
async fn launch(host: &MoonlightHost<TokioHyperClient>, app: u32) -> Result<(), String> {
    let crypto = RustCryptoBackend;
    let version = host.version().await.map_err(|e| format!("{e:?}"))?;
    let gfe = host.gfe_version().await.map_err(|e| format!("{e:?}"))?;
    let modes = host
        .server_codec_mode_support()
        .await
        .map_err(|e| format!("{e:?}"))?;
    let mut settings = settings();
    settings
        .adjust_for_server(version, &gfe, modes)
        .map_err(|e| format!("{e:?}"))?;
    host.start_stream(
        AppId(app),
        &settings,
        AesKey::new_random(&crypto).unwrap(),
        AesIv::new_random(&crypto).unwrap(),
        MoonlightStreamSetup::launch_query_parameters(),
    )
    .await
    .map(|_| ())
    .map_err(|e| format!("{e:?}"))
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

    // The list the portal sent makes the client bob's: it sees his
    // environments, and not alice's.
    host.set_identity(
        rig.client_cert.clone(),
        rig.client_key.clone(),
        rig.server.clone(),
    )
    .await
    .unwrap();
    let apps = host.app_list().await.unwrap();
    assert_eq!(
        apps.iter().map(|a| a.title.as_str()).collect::<Vec<_>>(),
        ["Google Chrome"]
    );
    let chrome = apps[0].id.0;
    assert_eq!(chrome, cha_node::gamestream::directory::app_id("e-chrome"));
    launch(&host, chrome).await.unwrap();
    // Cancelling quits the session; the environment is not the host's to stop.
    host.cancel().await.unwrap();
    assert_eq!(host.app_list().await.unwrap().len(), 1);
    let hers = cha_node::gamestream::directory::app_id("e-firefox");
    assert!(
        launch(&host, hers).await.is_err(),
        "alice's environment is not bob's to launch"
    );

    // The client unpairs itself over HTTPS: the portal hears, and it is out.
    host.unpair().await.unwrap();
    let ToPortal::GameStreamUnpaired { fingerprint } = rig.next(5).await else {
        panic!("the portal was told it unpaired");
    };
    assert_eq!(fingerprint, rig.client_fingerprint());
    assert!(
        host.update().await.is_err(),
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
    host.set_identity(
        rig.client_cert.clone(),
        rig.client_key.clone(),
        rig.server.clone(),
    )
    .await
    .expect_err("nobody was paired");
}

#[tokio::test(flavor = "multi_thread")]
async fn nobody_is_paired_until_the_portal_says_so() {
    let rig = Rig::start().await;
    let host = rig.client();
    // A client the portal never heard of, with the host's certificate pinned.
    let refused = host
        .set_identity(
            rig.client_cert.clone(),
            rig.client_key.clone(),
            rig.server.clone(),
        )
        .await;
    assert!(refused.is_err(), "an unlisted certificate got the app list");
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
    host.set_identity(
        rig.client_cert.clone(),
        rig.client_key.clone(),
        rig.server.clone(),
    )
    .await
    .unwrap();
    let apps = host.app_list().await.unwrap();
    assert_eq!(
        apps.iter().map(|a| a.title.as_str()).collect::<Vec<_>>(),
        ["Firefox"],
        "the owner in the list decides what it sees"
    );
    // And a list without it takes the access away again.
    rig.host
        .handle(NodeRequest::GameStreamDevices {
            devices: Vec::new(),
        })
        .unwrap();
    assert!(host.update().await.is_err());
}

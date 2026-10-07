//! The client's front (`client::front`) against our host's front on loopback:
//! serverinfo, pairing, apps, launch, RTSP, resume, cancel, unpair, and what
//! the host saw of each. The host side is the real `front::Host` with the
//! fake directory and pairing store of `tests/common`.

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use cha_gamestream::client::front::{
    ClientError, ClientIdentity, Encrypt, HostClient, PairingError, StreamRequest, Timeouts,
};
use cha_gamestream::handoff::Chroma;
use cha_gamestream::{
    App, Capabilities, ClientId, Directory, DirectoryError, Host, HostConfig, LaunchRequest,
    MemoryPairingStore, PairingAttempt, PairingStore, PinWaiter, ResumeRequest, RunningHost,
    SessionHandoff, SessionTarget, VideoCodec,
};
use common::{FakeBackend, FakeDirectory, LOOPBACK, Rig};
use futures_util::future::BoxFuture;

/// The fake directory, remembering what the front handed it.
struct Recording {
    inner: Arc<FakeDirectory>,
    handoffs: Mutex<Vec<SessionHandoff>>,
    targets: Mutex<Vec<SessionTarget>>,
}

impl Recording {
    fn handoffs(&self) -> Vec<SessionHandoff> {
        self.handoffs.lock().unwrap().clone()
    }
    fn targets(&self) -> Vec<SessionTarget> {
        self.targets.lock().unwrap().clone()
    }
}

impl Directory for Recording {
    fn apps(&self, client: &ClientId) -> BoxFuture<'_, Vec<App>> {
        self.inner.apps(client)
    }
    fn app_image(&self, client: &ClientId, app_id: u32) -> BoxFuture<'_, Option<Bytes>> {
        self.inner.app_image(client, app_id)
    }
    fn launch(
        &self,
        client: &ClientId,
        request: LaunchRequest,
    ) -> BoxFuture<'_, Result<SessionTarget, DirectoryError>> {
        let client = client.clone();
        Box::pin(async move {
            let target = self.inner.launch(&client, request).await?;
            self.targets.lock().unwrap().push(target);
            Ok(target)
        })
    }
    fn resume(
        &self,
        client: &ClientId,
        request: ResumeRequest,
    ) -> BoxFuture<'_, Result<SessionTarget, DirectoryError>> {
        let client = client.clone();
        Box::pin(async move {
            let target = self.inner.resume(&client, request).await?;
            self.targets.lock().unwrap().push(target);
            Ok(target)
        })
    }
    fn start_media(&self, handoff: SessionHandoff) -> BoxFuture<'_, Result<(), DirectoryError>> {
        self.handoffs.lock().unwrap().push(handoff.clone());
        self.inner.start_media(handoff)
    }
    fn stop_media(&self, session_id: u64) -> BoxFuture<'_, ()> {
        self.inner.stop_media(session_id)
    }
    fn cancel(&self, client: &ClientId) -> BoxFuture<'_, ()> {
        self.inner.cancel(client)
    }
    fn pin_for(&self, attempt: PairingAttempt) -> PinWaiter {
        self.inner.pin_for(attempt)
    }
}

struct Env {
    host: RunningHost,
    fake: Arc<FakeDirectory>,
    recording: Arc<Recording>,
    backend: Arc<FakeBackend>,
    store: Arc<MemoryPairingStore>,
}

async fn env_with(caps: Capabilities, tweak: impl FnOnce(&mut HostConfig)) -> Env {
    common::init();
    let backend = FakeBackend::with_caps(caps);
    let fake = FakeDirectory::new(backend.clone());
    let recording = Arc::new(Recording {
        inner: fake.clone(),
        handoffs: Mutex::default(),
        targets: Mutex::default(),
    });
    let store = Arc::new(MemoryPairingStore::default());
    let mut config = HostConfig::new("Test Host", "ABCDEF0123456789ABCDEF0123456789");
    config.bind = LOOPBACK;
    config.ports = cha_gamestream::Ports {
        http: 0,
        https: 0,
        rtsp: 0,
    };
    config.mdns = false;
    config.capabilities = backend.caps.clone();
    tweak(&mut config);
    let host = Host::builder(config)
        .directory(recording.clone())
        .pairing_store(store.clone())
        .build()
        .unwrap()
        .start()
        .await
        .unwrap();
    fake.set_handle(host.handle());
    Env {
        host,
        fake,
        recording,
        backend,
        store,
    }
}

fn caps(codecs: &[VideoCodec], hdr: bool, yuv444: bool) -> Capabilities {
    Capabilities {
        codecs: codecs.to_vec(),
        hdr,
        yuv444,
    }
}

fn all_codecs() -> Capabilities {
    caps(
        &[VideoCodec::H264, VideoCodec::Hevc, VideoCodec::Av1],
        true,
        true,
    )
}

impl Env {
    fn client(&self) -> HostClient {
        HostClient::new(
            &format!("127.0.0.1:{}", self.host.addrs().http.port()),
            ClientIdentity::generate().unwrap(),
        )
        .unwrap()
    }

    /// A client paired through the real five phases.
    async fn paired(&self) -> HostClient {
        self.fake.auto_pin("4321");
        let client = self.client();
        client.pair("4321", "Test Mac").await.unwrap();
        client
    }
}

/// Quits whatever runs, so the next launch can start.
async fn quit(client: &HostClient) {
    client.cancel().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_client_goes_from_unpaired_to_streaming_and_back() {
    let env = env_with(
        caps(&[VideoCodec::H264, VideoCodec::Hevc], false, false),
        |c| {
            c.video_encryption = true;
        },
    )
    .await;
    env.fake.auto_pin("4321");
    let client = env.client();

    // Unpaired: plain HTTP says who the host is, and that we aren't known.
    let info = client.server_info().await.unwrap();
    assert_eq!(info.name, "Test Host");
    assert_eq!(info.unique_id, "ABCDEF0123456789ABCDEF0123456789");
    assert_eq!(info.https_port, env.host.addrs().https.port());
    assert_eq!(info.http_port, env.host.addrs().http.port());
    assert!(!info.paired && !info.busy() && info.is_sunshine());
    assert_eq!(info.codecs(), [VideoCodec::Hevc, VideoCodec::H264]);
    assert!(matches!(
        client.app_list().await,
        Err(ClientError::NoPinnedCert)
    ));

    // Pairing: the host's user (the fake directory) supplies the PIN we show.
    let pem = client.pair("4321", "My Mac & Co").await.unwrap();
    assert_eq!(pem, env.host.identity().cert_pem());
    assert_eq!(client.server_cert_pem().as_deref(), Some(pem.as_str()));
    let paired = env.store.list().await.unwrap();
    assert_eq!(paired.len(), 1);
    assert_eq!(paired[0].client, client.identity().client_id());
    assert_eq!(paired[0].name, "My Mac & Co");
    assert_eq!(paired[0].unique_id, client.identity().default_unique_id());
    let attempts = env.fake.attempts();
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0].client_name.as_deref(), Some("My Mac & Co"));

    // Paired: HTTPS, pinned, with our certificate.
    assert!(client.server_info().await.unwrap().paired);
    let apps = client.app_list().await.unwrap();
    assert_eq!(
        apps,
        [
            App {
                id: 1,
                title: "Desktop".into(),
                hdr: false
            },
            App {
                id: 2,
                title: "Steam & Co".into(),
                hdr: true
            }
        ]
    );
    let art = Bytes::from_static(b"\x89PNG\r\n\x1a\nbox art");
    *env.fake.image.lock().unwrap() = Some(art.clone());
    assert_eq!(client.app_asset(1).await.unwrap(), art);
    assert!(matches!(
        client.app_asset(2).await,
        Err(ClientError::Host { code: 404, .. })
    ));

    // Launch: the host's handoff is the stream the client negotiated.
    let mut request = StreamRequest::new(2, 2560, 1440, 60);
    request.bitrate_kbps = 40_000;
    request.gamepad_mask = 0b101;
    request.optimize_game_settings = true;
    let setup = client.launch(&request).await.unwrap();
    let backend = env.backend.next_stream().await;
    let handoff = env.recording.handoffs().pop().unwrap();
    assert_eq!(setup.host, LOOPBACK);
    assert_eq!(setup.ports, env.recording.targets()[0].media_ports);
    assert_eq!(setup.keys, handoff.keys);
    assert_eq!(setup.encryption, handoff.encryption);
    assert!(setup.encryption.control && setup.encryption.video && setup.encryption.audio);
    assert_eq!(setup.control_connect_data, handoff.control_connect_data);
    assert_eq!(setup.ping_payload, handoff.ping_payload);
    assert_eq!(
        setup.codec,
        VideoCodec::Hevc,
        "HEVC over H.264 when both are offered"
    );
    assert_eq!(backend.params.codec, setup.codec);
    assert_eq!(
        (setup.width, setup.height, setup.fps),
        (
            backend.params.width,
            backend.params.height,
            backend.params.fps
        )
    );
    assert_eq!((setup.width, setup.height, setup.fps), (2560, 1440, 60));
    assert_eq!(setup.packet_size, backend.params.packet_size);
    assert_eq!(
        setup.packet_size,
        1392 - 32,
        "the encryption header is taken off"
    );
    assert_eq!(backend.params.bitrate_bps, 40_000_000);
    assert_eq!(setup.audio, backend.params.audio);
    assert_eq!((setup.hdr, setup.chroma), (false, Chroma::Yuv420));
    assert_eq!(backend.params.app_id, 2);
    assert_eq!(backend.params.client, client.identity().client_id());
    let (_, launched) = env.fake.launches().pop().unwrap();
    assert_eq!(
        (
            launched.app_id,
            launched.width,
            launched.height,
            launched.fps
        ),
        (2, 2560, 1440, 60)
    );
    assert_eq!(launched.gamepad_mask, 0b101);
    assert!(launched.optimize_game_settings && !launched.hdr && !launched.local_audio);
    assert_eq!(launched.surround_audio_info, 0x30002);
    // The key the client made is the one the host holds, and each launch makes its own.
    assert_ne!(setup.keys.key, [0; 16]);

    let info = client.server_info().await.unwrap();
    assert!(info.busy());
    assert_eq!(info.current_game, 2);

    // A second launch while one runs is the host's refusal, not a hang.
    match client.launch(&request).await {
        Err(ClientError::Host { code: 400, message }) => {
            assert!(message.contains("already"), "{message}")
        }
        other => panic!("{other:?}"),
    }

    // Resume: a new key and connection data, the old media stopped.
    let resumed = client.resume(&request).await.unwrap();
    assert_ne!(resumed.keys, setup.keys);
    let handoffs = env.recording.handoffs();
    assert_eq!(handoffs.len(), 2);
    assert_eq!(resumed.keys, handoffs[1].keys);
    assert_eq!(
        resumed.control_connect_data,
        handoffs[1].control_connect_data
    );
    assert_eq!(resumed.ping_payload, handoffs[1].ping_payload);
    assert_eq!(resumed.ports, env.recording.targets()[1].media_ports);
    assert_eq!(env.fake.resumes().len(), 1);
    assert_eq!(env.fake.stopped_media(), [handoff.session_id]);

    // Cancel: the app quits, the host is free.
    client.cancel().await.unwrap();
    assert_eq!(env.fake.cancels(), [client.identity().client_id()]);
    assert!(!client.server_info().await.unwrap().busy());
    // Resuming what isn't running is the host's answer.
    assert!(matches!(
        client.resume(&request).await,
        Err(ClientError::Host { code: 400, .. })
    ));

    // Unpair: the host forgets us and so do we.
    client.unpair().await.unwrap();
    assert!(env.store.list().await.unwrap().is_empty());
    assert!(client.server_cert_pem().is_none());
    assert!(!client.server_info().await.unwrap().paired);
    env.host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn device_names_with_spaces_and_symbols_arrive_as_they_were() {
    let env = env_with(caps(&[VideoCodec::H264], false, false), |_| {}).await;
    env.fake.auto_pin("1234");
    for (i, name) in ["My Mac", "Mac & Co = 100%", "Café ñ +1", "a b/c?d#e"]
        .into_iter()
        .enumerate()
    {
        let client = env.client();
        client.pair("1234", name).await.unwrap();
        assert_eq!(env.store.list().await.unwrap().len(), 1, "{name}");
        assert_eq!(env.store.list().await.unwrap()[0].name, name);
        assert_eq!(env.fake.attempts()[i].client_name.as_deref(), Some(name));
        client.unpair().await.unwrap();
    }
    env.host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_wrong_pin_pairs_nobody_and_says_so() {
    let env = env_with(caps(&[VideoCodec::H264], false, false), |_| {}).await;
    // The host's user types 1111; the client shows 9999.
    env.fake.auto_pin("1111");
    let client = env.client();
    let outcome = client.pair("9999", "Test").await;
    assert!(
        matches!(outcome, Err(ClientError::Pairing(PairingError::WrongPin))),
        "{outcome:?}"
    );
    assert!(env.store.list().await.unwrap().is_empty());
    assert!(client.server_cert_pem().is_none());
    assert!(matches!(
        client.launch(&StreamRequest::new(1, 1280, 720, 60)).await,
        Err(ClientError::NoPinnedCert)
    ));
    // And the right one still works afterwards.
    client.pair("1111", "Test").await.unwrap();
    assert_eq!(env.store.list().await.unwrap().len(), 1);
    env.host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn pairing_without_a_pin_is_declined_not_endless() {
    let env = env_with(caps(&[VideoCodec::H264], false, false), |c| {
        c.pin_timeout = Duration::from_millis(300);
    })
    .await;
    let client = env.client();
    let outcome = client.pair("1234", "Test").await;
    assert!(
        matches!(outcome, Err(ClientError::Pairing(PairingError::Declined))),
        "{outcome:?}"
    );
    assert!(env.store.list().await.unwrap().is_empty());

    // A client that gives up waiting says so.
    let env2 = env_with(caps(&[VideoCodec::H264], false, false), |_| {}).await;
    let impatient = env2.client().with_timeouts(Timeouts {
        pair: Duration::from_millis(300),
        ..Timeouts::default()
    });
    assert!(matches!(
        impatient.pair("1234", "Test").await,
        Err(ClientError::Timeout(_))
    ));
    env.host.shutdown().await;
    env2.host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_certificate_the_host_never_paired_gets_nothing() {
    let env = env_with(caps(&[VideoCodec::H264], false, false), |_| {}).await;
    let stranger = env
        .client()
        .with_server_cert(env.host.identity().cert_pem())
        .unwrap();
    // HTTPS answers serverinfo for anyone, and says we aren't paired.
    assert!(!stranger.server_info().await.unwrap().paired);
    assert!(matches!(
        stranger.app_list().await,
        Err(ClientError::NotPaired)
    ));
    assert!(matches!(
        stranger.launch(&StreamRequest::new(1, 1280, 720, 60)).await,
        Err(ClientError::NotPaired)
    ));
    assert!(matches!(
        stranger.cancel().await,
        Err(ClientError::NotPaired)
    ));
    assert!(env.fake.launches().is_empty());

    // A client that pinned another certificate than the host's refuses to talk.
    let impostor = env
        .client()
        .with_server_cert(ClientIdentity::generate().unwrap().cert_pem())
        .unwrap()
        .with_https_port(env.host.addrs().https.port());
    assert!(matches!(
        impostor.app_list().await,
        Err(ClientError::Tls(_))
    ));
    env.host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_codec_is_the_first_of_our_preference_the_host_offers() {
    let env = env_with(all_codecs(), |_| {}).await;
    let client = env.paired().await;

    let launch = |codecs: &[VideoCodec]| {
        let mut r = StreamRequest::new(1, 1920, 1080, 60);
        r.codecs = codecs.to_vec();
        let client = &client;
        async move { client.launch(&r).await }
    };
    for (prefer, expect) in [
        (&[VideoCodec::Hevc, VideoCodec::H264][..], VideoCodec::Hevc),
        (
            &[VideoCodec::Av1, VideoCodec::Hevc, VideoCodec::H264],
            VideoCodec::Av1,
        ),
        (&[VideoCodec::H264, VideoCodec::Hevc], VideoCodec::H264),
        (&[VideoCodec::Av1], VideoCodec::Av1),
    ] {
        let setup = launch(prefer).await.unwrap();
        assert_eq!(setup.codec, expect, "{prefer:?}");
        // The host agrees: it accepted this ANNOUNCE for exactly that codec.
        assert_eq!(env.backend.next_stream().await.params.codec, expect);
        quit(&client).await;
    }
    env.host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn what_the_host_cannot_encode_is_refused_before_anything_starts() {
    // H.264 only, no HDR, no 4:4:4.
    let env = env_with(caps(&[VideoCodec::H264], false, false), |_| {}).await;
    let client = env.paired().await;

    // HEVC first, H.264 second: the second.
    let mut r = StreamRequest::new(1, 1920, 1080, 60);
    assert_eq!(client.launch(&r).await.unwrap().codec, VideoCodec::H264);
    quit(&client).await;

    // Only AV1: nothing to use.
    r.codecs = vec![VideoCodec::Av1];
    assert!(matches!(
        client.launch(&r).await,
        Err(ClientError::Unsupported(_))
    ));
    // HDR, and 4:4:4: neither.
    let mut r = StreamRequest::new(1, 1920, 1080, 60);
    r.hdr = true;
    assert!(matches!(
        client.launch(&r).await,
        Err(ClientError::Unsupported(_))
    ));
    let mut r = StreamRequest::new(1, 1920, 1080, 60);
    r.chroma = Chroma::Yuv444;
    assert!(matches!(
        client.launch(&r).await,
        Err(ClientError::Unsupported(_))
    ));
    // Nothing was launched by any of those.
    assert_eq!(env.fake.launches().len(), 1);
    assert!(!client.server_info().await.unwrap().busy());
    env.host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn hdr_444_surround_and_reference_choices_reach_the_host() {
    let env = env_with(all_codecs(), |_| {}).await;
    let client = env.paired().await;

    let mut r = StreamRequest::new(2, 3840, 2160, 120);
    r.codecs = vec![VideoCodec::Hevc];
    r.hdr = true;
    r.chroma = Chroma::Yuv444;
    r.full_range = true;
    r.audio_channels = 6;
    r.bitrate_kbps = 80_000;
    r.reference_frame_invalidation = true;
    r.local_audio = true;
    let setup = client.launch(&r).await.unwrap();
    let backend = env.backend.next_stream().await;
    assert!(setup.hdr && setup.chroma == Chroma::Yuv444);
    assert!(backend.params.hdr && backend.params.full_range);
    assert_eq!(backend.params.chroma, Chroma::Yuv444);
    assert_eq!(backend.params.max_ref_frames, 0);
    assert_eq!(
        (
            backend.params.width,
            backend.params.height,
            backend.params.fps
        ),
        (3840, 2160, 120)
    );
    // 5.1 above 15 Mbps asks for the high-quality layout: six mono streams.
    assert_eq!(setup.audio, backend.params.audio);
    assert_eq!(setup.audio.channels, 6);
    assert!(setup.audio.high_quality);
    assert_eq!((setup.audio.streams, setup.audio.coupled_streams), (6, 0));
    let (_, launched) = env.fake.launches().pop().unwrap();
    assert!(launched.hdr && launched.local_audio);
    assert_eq!(launched.surround_audio_info, 0x3F0006);
    quit(&client).await;

    // Normal quality 5.1 below it, with the layout and channel order the host's table has.
    let mut r = StreamRequest::new(1, 1920, 1080, 60);
    r.audio_channels = 6;
    r.bitrate_kbps = 8_000;
    let setup = client.launch(&r).await.unwrap();
    assert!(!setup.audio.high_quality);
    assert_eq!((setup.audio.streams, setup.audio.coupled_streams), (4, 2));
    assert_eq!(setup.audio.mapping, [0, 1, 4, 5, 2, 3]);
    assert_eq!(env.backend.next_stream().await.params.audio, setup.audio);
    quit(&client).await;
    env.host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn video_encryption_is_negotiated_when_the_host_supports_it() {
    let on = env_with(caps(&[VideoCodec::Hevc], false, false), |c| {
        c.video_encryption = true
    })
    .await;
    let off = env_with(caps(&[VideoCodec::Hevc], false, false), |_| {}).await;
    let (on_client, off_client) = (on.paired().await, off.paired().await);
    let request = |video| {
        let mut r = StreamRequest::new(1, 1920, 1080, 60);
        r.video_encryption = video;
        r
    };

    // If supported: on where the host can, off where it can't.
    let setup = on_client
        .launch(&request(Encrypt::IfSupported))
        .await
        .unwrap();
    assert!(setup.encryption.video && setup.encryption.audio && setup.encryption.control);
    assert_eq!(setup.packet_size, 1360);
    let handoff = on.recording.handoffs().pop().unwrap();
    assert!(handoff.encryption.video);
    assert_eq!(handoff.params.packet_size, 1360);
    quit(&on_client).await;
    let setup = off_client
        .launch(&request(Encrypt::IfSupported))
        .await
        .unwrap();
    assert!(!setup.encryption.video && setup.encryption.audio);
    assert_eq!(setup.packet_size, 1392);
    assert!(!off.recording.handoffs().pop().unwrap().encryption.video);
    quit(&off_client).await;

    // Off stays off even where it could be on; required fails where it can't be.
    let setup = on_client.launch(&request(Encrypt::Off)).await.unwrap();
    assert!(!setup.encryption.video);
    assert_eq!(setup.packet_size, 1392);
    quit(&on_client).await;
    let outcome = off_client.launch(&request(Encrypt::Required)).await;
    assert!(
        matches!(outcome, Err(ClientError::Unsupported(_))),
        "{outcome:?}"
    );
    // The host's session was started by /launch; it can be ended.
    quit(&off_client).await;

    // Audio can be left clear.
    let mut r = request(Encrypt::Off);
    r.audio_encryption = Encrypt::Off;
    let setup = on_client.launch(&r).await.unwrap();
    assert!(!setup.encryption.audio && setup.encryption.control);
    quit(&on_client).await;
    on.host.shutdown().await;
    off.host.shutdown().await;
}

/// A host that answers every connection with fixed bytes, or with nothing at all.
async fn hostile_host(reply: Option<Vec<u8>>) -> u16 {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind((LOOPBACK, 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let reply = reply.clone();
            tokio::spawn(async move {
                let mut buf = [0u8; 2048];
                let _ = stream.read(&mut buf).await;
                match reply {
                    Some(bytes) => {
                        let _ = stream.write_all(&bytes).await;
                        let _ = stream.shutdown().await;
                    }
                    // Hold the connection open and say nothing.
                    None => tokio::time::sleep(Duration::from_secs(30)).await,
                }
            });
        }
    });
    port
}

fn quick() -> Timeouts {
    Timeouts {
        connect: Duration::from_secs(2),
        request: Duration::from_millis(400),
        launch: Duration::from_millis(400),
        cancel: Duration::from_millis(400),
        pair: Duration::from_millis(400),
        rtsp: Duration::from_millis(400),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_host_that_misbehaves_gives_errors_never_panics_or_hangs() {
    common::init();
    let http = |status: &str, body: &str| {
        format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nContent-Type: application/xml\r\nConnection: close\r\n\r\n{body}", body.len())
            .into_bytes()
    };
    let replies = [
        None,
        Some(Vec::new()),
        Some(b"garbage\r\n\r\n".to_vec()),
        Some(vec![0xFF; 5000]),
        Some(http("200 OK", "")),
        Some(http("200 OK", "not xml at all")),
        Some(http(
            "200 OK",
            "<root status_code=\"503\" status_message=\"busy\"></root>",
        )),
        Some(http("200 OK", "<root status_code=\"x\"></root>")),
        Some(http("200 OK", "<root status_code=\"200\"></root>")),
        Some(http("500 Internal Server Error", "boom")),
        Some(http("401 Unauthorized", "no")),
        Some(b"HTTP/1.1 200 OK\r\nContent-Length: 99999999999\r\n\r\nxx".to_vec()),
        Some(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nZZ\r\n".to_vec()),
    ];
    for reply in replies {
        let port = hostile_host(reply.clone()).await;
        let client = HostClient::new(
            &format!("127.0.0.1:{port}"),
            ClientIdentity::generate().unwrap(),
        )
        .unwrap()
        .with_timeouts(quick());
        let started = std::time::Instant::now();
        let info = client.server_info().await;
        assert!(info.is_err(), "{reply:?} gave {info:?}");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{reply:?} took too long"
        );
        // Pairing against the same host ends in an error as well.
        assert!(client.pair("1234", "x").await.is_err());
    }
    // A host nobody listens at.
    let gone = {
        let l = std::net::TcpListener::bind((LOOPBACK, 0)).unwrap();
        l.local_addr().unwrap().port()
    };
    let client = HostClient::new(
        &format!("127.0.0.1:{gone}"),
        ClientIdentity::generate().unwrap(),
    )
    .unwrap()
    .with_timeouts(quick());
    assert!(matches!(
        client.server_info().await,
        Err(ClientError::Connect { .. })
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn serverinfo_without_the_required_fields_is_malformed() {
    common::init();
    let body = "<root status_code=\"200\"><hostname>x</hostname></root>";
    let reply = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    let port = hostile_host(Some(reply.into_bytes())).await;
    let client = HostClient::new(
        &format!("127.0.0.1:{port}"),
        ClientIdentity::generate().unwrap(),
    )
    .unwrap()
    .with_timeouts(quick());
    assert!(matches!(
        client.server_info().await,
        Err(ClientError::Malformed(_))
    ));
}

/// The same session through moonlight-common-rust and through us: the host
/// ends up with the same stream parameters, so the two clients negotiated alike.
#[tokio::test(flavor = "multi_thread")]
async fn our_client_negotiates_what_moonlight_common_rust_does() {
    use common::{TestClient, connect_stream, settings};

    let rig = Rig::start().await;
    rig.directory.auto_pin("4321");

    // Ours.
    let ours = ClientIdentity::generate().unwrap();
    let client = HostClient::new(&format!("127.0.0.1:{}", rig.http_port()), ours).unwrap();
    client.pair("4321", "Ours").await.unwrap();
    let request = StreamRequest::new(1, 1920, 1080, 60);
    let setup = client.launch(&request).await.unwrap();
    let mine = rig.backend.next_stream().await.params.clone();
    client.cancel().await.unwrap();

    // Theirs.
    let theirs = TestClient::new("theirs");
    let host = theirs.paired_host(&rig).await;
    let stream = connect_stream(&host, 1, settings(VideoCodec::Hevc, 1920, 1080, 60)).await;
    let other = rig.backend.next_stream().await.params.clone();
    stream.send(common::ClientCommand::Disconnect);
    let _ = tokio::time::timeout(Duration::from_secs(5), stream.task).await;

    assert_eq!(setup.codec, VideoCodec::Hevc);
    assert_eq!(mine.codec, other.codec);
    assert_eq!(
        (mine.width, mine.height, mine.fps),
        (other.width, other.height, other.fps)
    );
    assert_eq!(mine.packet_size, other.packet_size);
    assert_eq!(mine.bitrate_bps, other.bitrate_bps);
    assert_eq!(mine.min_fec_packets, other.min_fec_packets);
    assert_eq!(mine.fec_percent, other.fec_percent);
    assert_eq!(
        (mine.hdr, mine.chroma, mine.app_id),
        (other.hdr, other.chroma, other.app_id)
    );
    assert_eq!(mine.audio, other.audio);
    assert_ne!(mine.client, other.client);
    rig.host.shutdown().await;
}

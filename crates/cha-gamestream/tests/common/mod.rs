//! Fakes and a real client for the loopback tests.
//!
//! `FakeDirectory` is the production seam in process: `launch` reserves
//! media sockets, `start_media` hands the handoff to `MediaSession::start`
//! with a `FakeBackend`, exactly as a node would across a process boundary.

#![allow(dead_code)]

use std::collections::VecDeque;
use std::net::IpAddr;
use std::str::FromStr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use bytes::Bytes;
use cha_gamestream::backend::BackendError;
use cha_gamestream::{
    App, Capabilities, ClientId, Directory, DirectoryError, EncodedVideo, Feedback, Host,
    HostConfig, HostHandle, Identity, InputEvent, LaunchRequest, MediaBackend, MediaConfig,
    MediaControl, MediaPorts, MediaSession, MediaSockets, MediaStreams, MemoryPairingStore,
    OpusPacket, PairingAttempt, PinSender, PinWaiter, ResumeRequest, RunningHost, SessionHandoff,
    SessionTarget, StreamParams, VideoCodec, pin_channel,
};
use futures_util::future::BoxFuture;
use moonlight_common::crypto::rustcrypto::RustCryptoBackend;
use moonlight_common::high::tokio::MoonlightHost;
use moonlight_common::http::client::tokio_hyper::TokioHyperClient;
use moonlight_common::http::pair::PairPin;
use moonlight_common::http::{ClientIdentifier, ClientSecret};
use tokio::sync::{Notify, mpsc};

pub const LOOPBACK: IpAddr = IpAddr::V4(std::net::Ipv4Addr::LOCALHOST);

pub fn init() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    });
}

/// What the control handle of a started stream was asked.
#[derive(Default)]
pub struct ControlLog {
    pub keyframes: usize,
    pub invalidations: Vec<(u64, u64)>,
    pub inputs: Vec<InputEvent>,
    pub bitrates: Vec<u32>,
    pub released: bool,
    pub stopped: bool,
}

pub struct BackendStream {
    pub params: StreamParams,
    pub video: mpsc::Sender<EncodedVideo>,
    pub audio: mpsc::Sender<OpusPacket>,
    pub feedback: mpsc::Sender<Feedback>,
    pub log: Arc<Mutex<ControlLog>>,
    changed: Arc<Notify>,
    end_video: Arc<Notify>,
}

impl BackendStream {
    /// The backend stops producing video: the host's video channel closes.
    pub fn end_video(&self) {
        self.end_video.notify_one();
    }

    /// Waits until `check` holds of the log, or panics after a few seconds.
    pub async fn wait_for(&self, what: &str, check: impl Fn(&ControlLog) -> bool) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            let notified = self.changed.notified();
            if check(&self.log.lock().unwrap()) {
                return;
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                panic!("timed out waiting for {what}");
            }
        }
    }
}

struct FakeControl {
    log: Arc<Mutex<ControlLog>>,
    changed: Arc<Notify>,
}

impl FakeControl {
    fn touch<R>(&self, f: impl FnOnce(&mut ControlLog) -> R) -> R {
        let r = f(&mut self.log.lock().unwrap());
        self.changed.notify_waiters();
        r
    }
}

impl MediaControl for FakeControl {
    fn request_keyframe(&self) {
        self.touch(|l| l.keyframes += 1);
    }
    fn invalidate(&self, first: u64, last: u64) {
        self.touch(|l| l.invalidations.push((first, last)));
    }
    fn set_bitrate(&self, bps: u32) {
        self.touch(|l| l.bitrates.push(bps));
    }
    fn input(&self, event: InputEvent) {
        self.touch(|l| l.inputs.push(event));
    }
    fn release_input(&self) {
        self.touch(|l| l.released = true);
    }
    fn stop(&self) -> BoxFuture<'_, ()> {
        self.touch(|l| l.stopped = true);
        Box::pin(async {})
    }
}

pub struct FakeBackend {
    pub caps: Capabilities,
    started: Mutex<VecDeque<Arc<BackendStream>>>,
    announce: Notify,
    pub starts: AtomicUsize,
}

impl FakeBackend {
    pub fn with_caps(caps: Capabilities) -> Arc<Self> {
        Arc::new(Self {
            caps,
            started: Mutex::default(),
            announce: Notify::new(),
            starts: AtomicUsize::new(0),
        })
    }

    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            caps: Capabilities {
                codecs: vec![VideoCodec::H264, VideoCodec::Hevc],
                hdr: false,
                yuv444: false,
            },
            started: Mutex::default(),
            announce: Notify::new(),
            starts: AtomicUsize::new(0),
        })
    }

    /// The next stream the backend was started for.
    pub async fn next_stream(&self) -> Arc<BackendStream> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            let notified = self.announce.notified();
            if let Some(s) = self.started.lock().unwrap().pop_front() {
                return s;
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                panic!("the backend was never started");
            }
        }
    }
}

impl MediaBackend for FakeBackend {
    fn capabilities(&self) -> Capabilities {
        self.caps.clone()
    }

    fn start(&self, params: StreamParams) -> BoxFuture<'_, Result<MediaStreams, BackendError>> {
        self.starts.fetch_add(1, Ordering::SeqCst);
        let (vtx, mut vforward) = mpsc::channel::<EncodedVideo>(64);
        let (host_vtx, vrx) = mpsc::channel(64);
        let end_video = Arc::new(Notify::new());
        tokio::spawn({
            let end = end_video.clone();
            async move {
                loop {
                    tokio::select! {
                        () = end.notified() => break,
                        frame = vforward.recv() => match frame {
                            Some(f) => if host_vtx.send(f).await.is_err() { break },
                            None => break,
                        },
                    }
                }
            }
        });
        let (atx, arx) = mpsc::channel(64);
        let (ftx, frx) = mpsc::channel(64);
        let log = Arc::new(Mutex::new(ControlLog::default()));
        let changed = Arc::new(Notify::new());
        let stream = Arc::new(BackendStream {
            params,
            video: vtx,
            audio: atx,
            feedback: ftx,
            log: log.clone(),
            changed: changed.clone(),
            end_video,
        });
        self.started.lock().unwrap().push_back(stream);
        self.announce.notify_waiters();
        Box::pin(async move {
            Ok(MediaStreams {
                video: vrx,
                audio: arx,
                feedback: frx,
                control: Arc::new(FakeControl { log, changed }),
            })
        })
    }
}

#[derive(Default)]
struct DirectoryState {
    attempts: Vec<PairingAttempt>,
    pending_pins: VecDeque<PinSender>,
    launches: Vec<(ClientId, LaunchRequest)>,
    resumes: Vec<ClientId>,
    cancels: Vec<ClientId>,
    reserved: Option<MediaSockets>,
    media: std::collections::HashMap<u64, Arc<MediaSession>>,
    stopped_media: Vec<u64>,
}

pub struct FakeDirectory {
    backend: Arc<FakeBackend>,
    pub media_config: MediaConfig,
    state: Mutex<DirectoryState>,
    auto_pin: Mutex<Option<String>>,
    pin_arrived: Notify,
    handle: OnceLock<HostHandle>,
    pub image: Mutex<Option<Bytes>>,
}

impl FakeDirectory {
    pub fn new(backend: Arc<FakeBackend>) -> Arc<Self> {
        Arc::new(Self {
            backend,
            media_config: MediaConfig::default(),
            state: Mutex::default(),
            auto_pin: Mutex::new(None),
            pin_arrived: Notify::new(),
            handle: OnceLock::new(),
            image: Mutex::new(None),
        })
    }

    pub fn set_handle(&self, handle: HostHandle) {
        let _ = self.handle.set(handle);
    }

    /// The PIN to resolve every pairing attempt with.
    pub fn auto_pin(&self, pin: &str) {
        *self.auto_pin.lock().unwrap() = Some(pin.to_owned());
    }

    /// Resolves the next waiting pairing attempt, waiting for it to arrive.
    pub async fn answer_next_pin(&self, pin: &str) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
        loop {
            let notified = self.pin_arrived.notified();
            if let Some(tx) = self.state.lock().unwrap().pending_pins.pop_front() {
                tx.send(pin.to_owned());
                return;
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                panic!("no pairing attempt asked for a PIN");
            }
        }
    }

    pub fn attempts(&self) -> Vec<PairingAttempt> {
        self.state.lock().unwrap().attempts.clone()
    }
    pub fn launches(&self) -> Vec<(ClientId, LaunchRequest)> {
        self.state.lock().unwrap().launches.clone()
    }
    pub fn cancels(&self) -> Vec<ClientId> {
        self.state.lock().unwrap().cancels.clone()
    }
    pub fn resumes(&self) -> Vec<ClientId> {
        self.state.lock().unwrap().resumes.clone()
    }
    pub fn stopped_media(&self) -> Vec<u64> {
        self.state.lock().unwrap().stopped_media.clone()
    }
    pub fn media(&self, id: u64) -> Option<Arc<MediaSession>> {
        self.state.lock().unwrap().media.get(&id).cloned()
    }

    async fn reserve(&self) -> Result<SessionTarget, DirectoryError> {
        let sockets = MediaSockets::bind(
            LOOPBACK,
            MediaPorts {
                video: 0,
                control: 0,
                audio: 0,
            },
        )
        .await
        .map_err(|e| DirectoryError::Failed(e.to_string()))?;
        let media_ports = sockets.ports();
        self.state.lock().unwrap().reserved = Some(sockets);
        Ok(SessionTarget { media_ports })
    }
}

impl Directory for FakeDirectory {
    fn apps(&self, _client: &ClientId) -> BoxFuture<'_, Vec<App>> {
        Box::pin(async {
            vec![
                App {
                    id: 1,
                    title: "Desktop".into(),
                    hdr: false,
                },
                App {
                    id: 2,
                    title: "Steam & Co".into(),
                    hdr: true,
                },
            ]
        })
    }

    fn app_image(&self, _client: &ClientId, app_id: u32) -> BoxFuture<'_, Option<Bytes>> {
        let image = if app_id == 1 {
            self.image.lock().unwrap().clone()
        } else {
            None
        };
        Box::pin(async move { image })
    }

    fn launch(
        &self,
        client: &ClientId,
        request: LaunchRequest,
    ) -> BoxFuture<'_, Result<SessionTarget, DirectoryError>> {
        let client = client.clone();
        Box::pin(async move {
            if request.app_id != 1 && request.app_id != 2 {
                return Err(DirectoryError::NoSuchApp);
            }
            self.state.lock().unwrap().launches.push((client, request));
            self.reserve().await
        })
    }

    fn resume(
        &self,
        client: &ClientId,
        _request: ResumeRequest,
    ) -> BoxFuture<'_, Result<SessionTarget, DirectoryError>> {
        let client = client.clone();
        Box::pin(async move {
            self.state.lock().unwrap().resumes.push(client);
            self.reserve().await
        })
    }

    fn start_media(&self, handoff: SessionHandoff) -> BoxFuture<'_, Result<(), DirectoryError>> {
        Box::pin(async move {
            let sockets = self
                .state
                .lock()
                .unwrap()
                .reserved
                .take()
                .ok_or_else(|| DirectoryError::Failed("no ports reserved".into()))?;
            let id = handoff.session_id;
            let session = MediaSession::start(
                handoff,
                self.backend.clone(),
                sockets,
                self.media_config.clone(),
            )
            .await
            .map_err(|e| DirectoryError::Failed(e.to_string()))?;
            let session = Arc::new(session);
            self.state.lock().unwrap().media.insert(id, session.clone());
            // Tell the front when the media ends by itself, as a node's streamer would.
            if let Some(handle) = self.handle.get().cloned() {
                tokio::spawn(async move {
                    session.closed().await;
                    session.join().await;
                    handle.media_ended(id);
                });
            }
            Ok(())
        })
    }

    fn stop_media(&self, session_id: u64) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            let session = {
                let mut state = self.state.lock().unwrap();
                state.stopped_media.push(session_id);
                state.media.remove(&session_id)
            };
            if let Some(session) = session {
                session.stop().await;
            }
        })
    }

    fn cancel(&self, client: &ClientId) -> BoxFuture<'_, ()> {
        self.state.lock().unwrap().cancels.push(client.clone());
        Box::pin(async {})
    }

    fn pin_for(&self, attempt: PairingAttempt) -> PinWaiter {
        let (tx, waiter) = pin_channel();
        self.state.lock().unwrap().attempts.push(attempt);
        if let Some(pin) = self.auto_pin.lock().unwrap().clone() {
            tx.send(pin);
        } else {
            self.state.lock().unwrap().pending_pins.push_back(tx);
            self.pin_arrived.notify_waiters();
        }
        waiter
    }
}

pub struct Rig {
    pub host: RunningHost,
    pub directory: Arc<FakeDirectory>,
    pub store: Arc<MemoryPairingStore>,
    pub backend: Arc<FakeBackend>,
    pub config: HostConfig,
}

impl Rig {
    pub async fn start() -> Self {
        Self::with(|_| {}).await
    }

    pub async fn with(tweak: impl FnOnce(&mut HostConfig)) -> Self {
        init();
        let backend = FakeBackend::new();
        let directory = FakeDirectory::new(backend.clone());
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
        let host = Host::builder(config.clone())
            .directory(directory.clone())
            .pairing_store(store.clone())
            .build()
            .unwrap()
            .start()
            .await
            .unwrap();
        directory.set_handle(host.handle());
        Rig {
            host,
            directory,
            store,
            backend,
            config,
        }
    }

    pub fn http_port(&self) -> u16 {
        self.host.addrs().http.port()
    }
}

/// A Moonlight client with an identity of its own.
pub struct TestClient {
    pub identity: Identity,
    pub unique_id: String,
}

impl TestClient {
    pub fn new(unique_id: &str) -> Self {
        init();
        Self {
            identity: Identity::generate().unwrap(),
            unique_id: unique_id.to_owned(),
        }
    }

    pub fn id(&self) -> ClientId {
        ClientId(self.identity.fingerprint())
    }

    pub fn ids(&self) -> (ClientIdentifier, ClientSecret) {
        (
            ClientIdentifier::from_pem(pem::Pem::from_str(self.identity.cert_pem()).unwrap()),
            ClientSecret::from_pem(pem::Pem::from_str(self.identity.key_pem()).unwrap()),
        )
    }

    pub fn host(&self, rig: &Rig) -> MoonlightHost<TokioHyperClient> {
        MoonlightHost::<TokioHyperClient>::new(
            "127.0.0.1".into(),
            rig.http_port(),
            Some(self.unique_id.clone()),
        )
        .unwrap()
    }

    /// Pairs with the PIN the client shows; `typed` is what the user types
    /// into the directory side.
    pub async fn pair(
        &self,
        rig: &Rig,
        host: &MoonlightHost<TokioHyperClient>,
        shown: &str,
        typed: &str,
    ) -> Result<(), String> {
        let (cert, key) = self.ids();
        let crypto = RustCryptoBackend;
        let d: Vec<u8> = shown.bytes().map(|b| b - b'0').collect();
        let pin = PairPin::new(d[0], d[1], d[2], d[3]).ok_or("bad PIN")?;
        let pairing = host.pair(&cert, &key, "TestDevice".into(), pin, crypto);
        tokio::pin!(pairing);
        let user = rig.directory.answer_next_pin(typed);
        tokio::pin!(user);
        let mut typed_it = false;
        let outcome = loop {
            tokio::select! {
                outcome = &mut pairing => break outcome,
                () = &mut user, if !typed_it => typed_it = true,
            }
        };
        outcome.map_err(|e| format!("{e:?}"))
    }
}

/// One raw HTTP request to the plain port; returns the response head and body.
pub async fn raw_http(port: u16, request: &str) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut out = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut out)).await;
    String::from_utf8_lossy(&out).into_owned()
}

/// The host's certificate as a client pins it.
pub fn server_identifier(rig: &Rig) -> moonlight_common::http::ServerIdentifier {
    moonlight_common::http::ServerIdentifier::from_pem(
        pem::Pem::from_str(rig.host.identity().cert_pem()).unwrap(),
    )
}

impl TestClient {
    /// A client the host already counts as paired, without running the
    /// pairing (whose client-side maths is slow in a debug build): the store
    /// gets its certificate, and the client pins the host's.
    pub async fn paired_host(&self, rig: &Rig) -> MoonlightHost<TokioHyperClient> {
        use cha_gamestream::{PairedClient, PairingStore};
        rig.store
            .add(PairedClient {
                client: self.id(),
                unique_id: self.unique_id.clone(),
                name: "TestDevice".into(),
            })
            .await
            .unwrap();
        let host = self.host(rig);
        let (cert, key) = self.ids();
        host.set_identity(cert, key, server_identifier(rig))
            .await
            .unwrap();
        host
    }
}

// ---------------------------------------------------------------------------
// A streaming client: moonlight-common-rust's stream, driven in a task.

use moonlight_common::AppId;
use moonlight_common::stream::audio::AudioConfig;
use moonlight_common::stream::control::ActiveGamepads;
use moonlight_common::stream::proto::MoonlightStreamSetup;
use moonlight_common::stream::proto::audio::AudioStreamEvent;
use moonlight_common::stream::proto::control::ControlStreamEvent;
use moonlight_common::stream::proto::control::input_batcher::ClientInputEvent;
use moonlight_common::stream::proto::control::packet::ControlPacket;
use moonlight_common::stream::proto::video::VideoStreamEvent;
use moonlight_common::stream::tokio::{MoonlightStream, MoonlightStreamEvent};
use moonlight_common::stream::video::{ColorRange, ColorSpace, VideoCapabilities, VideoFormats};
use moonlight_common::stream::{
    AesIv, AesKey, EncryptionFlags, MoonlightStreamSettings, StreamingConfig,
};

#[derive(Debug)]
pub enum ClientEvent {
    Video {
        data: Vec<u8>,
        key: bool,
        index: u32,
    },
    Audio {
        data: Bytes,
    },
    Rumble {
        pad: u16,
        low: u16,
        high: u16,
    },
    Disconnected,
    Other(String),
}

pub enum ClientCommand {
    Input(ClientInputEvent),
    Raw(ControlPacket),
    Disconnect,
}

pub struct StreamClient {
    pub events: mpsc::UnboundedReceiver<ClientEvent>,
    pub commands: mpsc::UnboundedSender<ClientCommand>,
    pub task: tokio::task::JoinHandle<()>,
}

impl StreamClient {
    pub fn send(&self, command: ClientCommand) {
        let _ = self.commands.send(command);
    }

    /// The next event matching `pick`, ignoring others; panics after `secs`.
    pub async fn next<T>(
        &mut self,
        secs: u64,
        mut pick: impl FnMut(ClientEvent) -> Result<T, ClientEvent>,
    ) -> T {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
        loop {
            match tokio::time::timeout_at(deadline, self.events.recv()).await {
                Ok(Some(event)) => match pick(event) {
                    Ok(t) => return t,
                    Err(_) => continue,
                },
                Ok(None) => panic!("the client stream ended"),
                Err(_) => panic!("timed out waiting for a client event"),
            }
        }
    }
}

pub fn settings(codec: VideoCodec, width: u32, height: u32, fps: u32) -> MoonlightStreamSettings {
    settings_encrypting(codec, width, height, fps, EncryptionFlags::empty())
}

pub fn settings_encrypting(
    codec: VideoCodec,
    width: u32,
    height: u32,
    fps: u32,
    flags: EncryptionFlags,
) -> MoonlightStreamSettings {
    MoonlightStreamSettings {
        width,
        height,
        fps,
        fps_x100: fps * 100,
        bitrate: 20_000,
        packet_size: 1392,
        // moonlight-common-rust can't decrypt video: the tests keep video unencrypted.
        encryption_flags: flags,
        streaming_remotely: StreamingConfig::Local,
        sops: false,
        hdr: false,
        supported_video_formats: if codec == VideoCodec::Hevc {
            VideoFormats::H265
        } else {
            VideoFormats::H264
        },
        color_space: ColorSpace::Rec709,
        color_range: ColorRange::Limited,
        local_audio_play_mode: false,
        audio_config: AudioConfig::STEREO,
        gamepads_attached: ActiveGamepads::empty(),
        gamepads_persist_after_disconnect: false,
        enable_mic: false,
    }
}

/// Launches (or resumes) `app_id`, connects the stream and drives it in a task.
pub async fn connect_stream(
    host: &MoonlightHost<TokioHyperClient>,
    app_id: u32,
    settings: MoonlightStreamSettings,
) -> StreamClient {
    let crypto = RustCryptoBackend;
    let version = host.version().await.unwrap();
    let gfe = host.gfe_version().await.unwrap();
    let modes = host.server_codec_mode_support().await.unwrap();
    let mut settings = settings;
    settings.adjust_for_server(version, &gfe, modes).unwrap();
    let config = host
        .start_stream(
            AppId(app_id),
            &settings,
            AesKey::new_random(&crypto).unwrap(),
            AesIv::new_random(&crypto).unwrap(),
            MoonlightStreamSetup::launch_query_parameters(),
        )
        .await
        .expect("launch");
    let mut stream = MoonlightStream::connect(
        config,
        settings,
        Arc::new(crypto),
        VideoCapabilities::default(),
    )
    .await
    .expect("stream connects");

    let (etx, events) = mpsc::unbounded_channel();
    let (commands, mut crx) = mpsc::unbounded_channel();
    let task = tokio::spawn(async move {
        loop {
            tokio::select! {
                command = crx.recv() => match command {
                    Some(ClientCommand::Input(e)) => { let _ = stream.send_input(e); }
                    Some(ClientCommand::Raw(p)) => { let _ = stream.send_raw(p); }
                    Some(ClientCommand::Disconnect) => {
                        let _ = stream.disconnect();
                        let _ = tokio::time::timeout(Duration::from_millis(500), async {
                            while stream.is_alive() {
                                if stream.drive().await.is_err() { break; }
                            }
                        }).await;
                        let _ = etx.send(ClientEvent::Disconnected);
                        return;
                    }
                    None => return,
                },
                event = stream.drive() => {
                    let event = match event {
                        Err(e) => { let _ = etx.send(ClientEvent::Other(format!("error: {e:?}"))); return; }
                        Ok(MoonlightStreamEvent::Video(VideoStreamEvent::OnFrame(frame))) => {
                            let md = frame.metadata();
                            ClientEvent::Video {
                                key: format!("{:?}", md.frame_type) == "Idr",
                                index: md.frame_index.0,
                                data: frame.raw().to_vec(),
                            }
                        }
                        Ok(MoonlightStreamEvent::Video(VideoStreamEvent::SignalIdr)) => {
                            let _ = stream.send_raw(ControlPacket::RequestIdr);
                            continue;
                        }
                        Ok(MoonlightStreamEvent::Audio(AudioStreamEvent::OnFrame(frame))) => ClientEvent::Audio { data: frame.buffer },
                        Ok(MoonlightStreamEvent::Control(ControlStreamEvent::Packet(ControlPacket::ControllerRumbleData {
                            controller_number, low_frequency, high_frequency, ..
                        }))) => ClientEvent::Rumble { pad: controller_number, low: low_frequency, high: high_frequency },
                        Ok(MoonlightStreamEvent::Control(ControlStreamEvent::Disconnect)) => ClientEvent::Disconnected,
                        Ok(other) => ClientEvent::Other(format!("{other:?}")),
                    };
                    if etx.send(event).is_err() { return; }
                }
            }
        }
    });
    StreamClient {
        events,
        commands,
        task,
    }
}

/// An access unit that looks like one: start codes and NAL headers, filler.
pub fn access_unit(codec: VideoCodec, key: bool, len: usize, seed: u8) -> Vec<u8> {
    let mut au = Vec::with_capacity(len);
    let nal_headers: &[&[u8]] = match (codec, key) {
        (VideoCodec::H264, true) => &[&[0x67], &[0x68], &[0x65]],
        (VideoCodec::H264, false) => &[&[0x41]],
        (_, true) => &[&[0x40, 0x01], &[0x42, 0x01], &[0x44, 0x01], &[0x26, 0x01]],
        (_, false) => &[&[0x02, 0x01]],
    };
    for h in nal_headers {
        au.extend_from_slice(&[0, 0, 0, 1]);
        au.extend_from_slice(h);
        au.extend_from_slice(&[0xAA, seed, 0x55]);
    }
    while au.len() < len {
        // No zero runs: nothing in the filler looks like a start code.
        au.push(1 + (au.len() as u8 ^ seed) % 250);
    }
    au.truncate(len);
    au
}

// ---------------------------------------------------------------------------
// The media half on its own: a handoff built by hand, and a hand-rolled control client.

use std::net::SocketAddr;

use aes_gcm::aead::{AeadInPlace, KeyInit};
use aes_gcm::{Aes128Gcm, Key, Nonce};
use cha_gamestream::{AudioParams, Encryption, SessionKeys};
use tokio_enet::{
    Event as EnetEvent, Host as EnetHost, HostConfig as EnetConfig, Packet, PacketMode, PeerId,
};

pub const KEY: [u8; 16] = *b"0123456789abcdef";
pub const CONNECT_DATA: u32 = 0xC0FFEE;
pub const PING_PAYLOAD: [u8; 16] = *b"PINGPAYLOAD12345";

pub fn stream_params(client_ip: IpAddr) -> StreamParams {
    StreamParams {
        client: ClientId("ab".repeat(32)),
        client_ip,
        app_id: 1,
        width: 1280,
        height: 720,
        fps: 60,
        bitrate_bps: 10_000_000,
        codec: VideoCodec::H264,
        hdr: false,
        chroma: cha_gamestream::handoff::Chroma::Yuv420,
        full_range: false,
        max_ref_frames: 1,
        packet_size: 1024,
        fec_percent: 20,
        min_fec_packets: 0,
        audio: AudioParams::select(2, 3, true, 5),
    }
}

pub fn handoff(client_ip: IpAddr) -> SessionHandoff {
    SessionHandoff {
        session_id: 77,
        keys: SessionKeys {
            key: KEY,
            key_id: 5,
        },
        encryption: Encryption {
            control: true,
            video: false,
            audio: false,
        },
        control_connect_data: CONNECT_DATA,
        ping_payload: PING_PAYLOAD,
        params: stream_params(client_ip),
    }
}

pub struct MediaRig {
    pub session: MediaSession,
    pub stream: Arc<BackendStream>,
    pub ports: MediaPorts,
    pub handoff: SessionHandoff,
}

impl MediaRig {
    pub async fn start(bind: IpAddr, handoff: SessionHandoff, config: MediaConfig) -> Self {
        Self::start_with(FakeBackend::new(), bind, handoff, config).await
    }

    pub async fn start_with(
        backend: Arc<FakeBackend>,
        bind: IpAddr,
        handoff: SessionHandoff,
        config: MediaConfig,
    ) -> Self {
        let sockets = MediaSockets::bind(
            bind,
            MediaPorts {
                video: 0,
                control: 0,
                audio: 0,
            },
        )
        .await
        .unwrap();
        let ports = sockets.ports();
        let session = MediaSession::start(handoff.clone(), backend.clone(), sockets, config)
            .await
            .unwrap();
        let stream = backend.next_stream().await;
        Self {
            session,
            stream,
            ports,
            handoff,
        }
    }
}

/// A control-stream client by hand: ENet plus the AES-GCM wrapper, written
/// independently of the crate's own so it checks it.
pub struct ControlClient {
    pub host: EnetHost,
    pub peer: PeerId,
    pub key: [u8; 16],
    pub sequence: u32,
}

impl ControlClient {
    pub async fn connect(local: SocketAddr, server: SocketAddr, data: u32, key: [u8; 16]) -> Self {
        let mut host = EnetHost::new(EnetConfig {
            address: Some(local),
            peer_count: 1,
            channel_limit: 0x30,
            ..Default::default()
        })
        .unwrap();
        let peer = host.connect(server, 0x30, data).unwrap();
        host.flush().await.unwrap();
        Self {
            host,
            peer,
            key,
            sequence: 0,
        }
    }

    /// Services the host for up to `dur`, returning whether a connect arrived.
    pub async fn wait_connected(&mut self, dur: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + dur;
        while tokio::time::Instant::now() < deadline {
            if let Ok(Some(EnetEvent::Connect { .. })) =
                self.host.service(Duration::from_millis(20)).await
            {
                let _ = self.host.flush().await;
                return true;
            }
        }
        false
    }

    pub async fn wait_disconnected(&mut self, dur: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + dur;
        while tokio::time::Instant::now() < deadline {
            if let Ok(Some(EnetEvent::Disconnect { .. })) =
                self.host.service(Duration::from_millis(20)).await
            {
                return true;
            }
        }
        false
    }

    pub async fn send_raw(&mut self, bytes: &[u8]) {
        self.host
            .peer_mut(self.peer)
            .unwrap()
            .send(0, Packet::new(bytes, PacketMode::ReliableSequenced))
            .unwrap();
        let _ = self.host.flush().await;
    }

    pub fn seal(&mut self, inner: &[u8]) -> Vec<u8> {
        let seq = self.sequence;
        self.sequence += 1;
        seal_with(&self.key, seq, inner, b'C')
    }

    /// An encrypted message `ty` with `body`.
    pub async fn send(&mut self, ty: u16, body: &[u8]) {
        let wire = self.seal(&frame(ty, body));
        self.send_raw(&wire).await;
    }

    pub async fn send_input(&mut self, packet: &[u8]) {
        let mut body = (packet.len() as u32).to_be_bytes().to_vec();
        body.extend_from_slice(packet);
        self.send(0x0206, &body).await;
    }

    /// Services the host for `dur`, returning host-to-client messages decrypted: `(type, body)`.
    pub async fn receive_for(&mut self, dur: Duration) -> Vec<(u16, Vec<u8>)> {
        let deadline = tokio::time::Instant::now() + dur;
        let mut out = Vec::new();
        while tokio::time::Instant::now() < deadline {
            if let Ok(Some(EnetEvent::Receive { packet, .. })) =
                self.host.service(Duration::from_millis(10)).await
            {
                let wire = packet.data();
                assert_eq!(
                    u16::from_le_bytes([wire[0], wire[1]]),
                    1,
                    "host messages are encrypted"
                );
                let seq = u32::from_le_bytes(wire[4..8].try_into().unwrap());
                let tag: [u8; 16] = wire[8..24].try_into().unwrap();
                let mut plain = wire[24..].to_vec();
                let mut iv = [0u8; 12];
                iv[..4].copy_from_slice(&seq.to_le_bytes());
                iv[10] = b'H';
                iv[11] = b'C';
                Aes128Gcm::new(Key::<Aes128Gcm>::from_slice(&self.key))
                    .decrypt_in_place_detached(
                        Nonce::from_slice(&iv),
                        b"",
                        &mut plain,
                        (&tag).into(),
                    )
                    .expect("the host's message authenticates");
                let ty = u16::from_le_bytes([plain[0], plain[1]]);
                out.push((ty, plain[4..].to_vec()));
            }
        }
        out
    }
}

pub fn frame(ty: u16, body: &[u8]) -> Vec<u8> {
    let mut m = ty.to_le_bytes().to_vec();
    m.extend((body.len() as u16).to_le_bytes());
    m.extend_from_slice(body);
    m
}

pub fn seal_with(key: &[u8; 16], seq: u32, inner: &[u8], direction: u8) -> Vec<u8> {
    let mut iv = [0u8; 12];
    iv[..4].copy_from_slice(&seq.to_le_bytes());
    iv[10] = direction;
    iv[11] = b'C';
    let mut ct = inner.to_vec();
    let tag = Aes128Gcm::new(Key::<Aes128Gcm>::from_slice(key))
        .encrypt_in_place_detached(Nonce::from_slice(&iv), b"", &mut ct)
        .unwrap();
    let mut body = seq.to_le_bytes().to_vec();
    body.extend_from_slice(&tag);
    body.extend_from_slice(&ct);
    frame(1, &body)
}

/// A key-down packet for virtual key `vk`.
pub fn key_packet(vk: u8, down: bool) -> Vec<u8> {
    let mut p = (if down { 3u32 } else { 4u32 }).to_le_bytes().to_vec();
    p.extend([0, vk, 0x80, 0, 0, 0]);
    p
}

/// Sends `bytes`, then reads until the peer closes or `wait` passes.
/// Returns what came back and whether the peer closed.
pub async fn raw_tcp(port: u16, bytes: &[u8], wait: Duration) -> (Vec<u8>, bool) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    let _ = stream.write_all(bytes).await;
    let mut out = Vec::new();
    let closed = tokio::time::timeout(wait, stream.read_to_end(&mut out))
        .await
        .is_ok();
    (out, closed)
}

/// One RTSP request on its own connection; the response text.
pub async fn rtsp(port: u16, request: &str) -> String {
    let (out, _) = raw_tcp(port, request.as_bytes(), Duration::from_secs(5)).await;
    String::from_utf8_lossy(&out).into_owned()
}

/// The value of header `name` in an RTSP or HTTP response.
pub fn header(response: &str, name: &str) -> Option<String> {
    response.lines().find_map(|l| {
        l.split_once(':')
            .filter(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.trim().to_owned())
    })
}

/// Launches `app_id` over HTTPS without connecting the stream, as a client's first step.
pub async fn launch_only(host: &MoonlightHost<TokioHyperClient>, app_id: u32) -> String {
    let crypto = RustCryptoBackend;
    let version = host.version().await.unwrap();
    let gfe = host.gfe_version().await.unwrap();
    let modes = host.server_codec_mode_support().await.unwrap();
    let mut s = settings(VideoCodec::H264, 1280, 720, 60);
    s.adjust_for_server(version, &gfe, modes).unwrap();
    host.start_stream(
        AppId(app_id),
        &s,
        AesKey::new_random(&crypto).unwrap(),
        AesIv::new_random(&crypto).unwrap(),
        MoonlightStreamSetup::launch_query_parameters(),
    )
    .await
    .expect("launch")
    .rtsp_session_url
    .expect("the launch answer has an RTSP URL")
    .to_string()
}

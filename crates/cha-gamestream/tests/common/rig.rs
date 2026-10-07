//! The host rig of the loopback tests: a fake directory and backend, and a running host.
//! Nothing here depends on moonlight-common, so crates that use our own client can share it.
//!
//! `FakeDirectory` is the production seam in process: `launch` reserves
//! media sockets, `start_media` hands the handoff to `MediaSession::start`
//! with a `FakeBackend`, exactly as a node would across a process boundary.

#![allow(dead_code)]

use std::collections::VecDeque;
use std::net::IpAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use bytes::Bytes;
use cha_gamestream::backend::BackendError;
use cha_gamestream::{
    App, Capabilities, ClientId, Directory, DirectoryError, EncodedVideo, Feedback, Host,
    HostConfig, HostHandle, InputEvent, LaunchRequest, MediaBackend, MediaConfig, MediaControl,
    MediaPorts, MediaSession, MediaSockets, MediaStreams, MemoryPairingStore, OpusPacket,
    PairingAttempt, PinSender, PinWaiter, ResumeRequest, RunningHost, SessionHandoff,
    SessionTarget, StreamParams, VideoCodec, pin_channel,
};
use futures_util::future::BoxFuture;
use tokio::sync::{Notify, mpsc};

pub const LOOPBACK: IpAddr = IpAddr::V4(std::net::Ipv4Addr::LOCALHOST);

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

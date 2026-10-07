//! The media half of a GameStream host: one session's control (ENet), video
//! and audio streams on their own ports, fed by a [`MediaBackend`].
//!
//! It knows nothing of nvhttp, pairing or RTSP. It is given a
//! [`SessionHandoff`] (the client's address, the session key, the negotiated
//! stream) and bound [`MediaSockets`], and serves that one session until the
//! client leaves, goes quiet, the backend ends or [`MediaSession::stop`] is
//! called.

use std::io;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::net::UdpSocket;
use tokio::sync::watch;

use crate::backend::{BackendError, HdrMetadata, MediaBackend};
use crate::handoff::{MediaPorts, SessionHandoff};

pub(crate) mod audio;
pub mod control;
mod ping;
pub(crate) mod video;

use video::FrameMap;

/// The ports of a session, bound and held until the session starts. The
/// front reports the ports to the client in RTSP `SETUP`, before the stream is
/// negotiated, so whoever runs the media half binds them first.
pub struct MediaSockets {
    video: UdpSocket,
    audio: UdpSocket,
    control: tokio_enet::Host,
    ports: MediaPorts,
}

impl MediaSockets {
    /// Binds the three ports on `ip`; a port of 0 takes any free one.
    pub async fn bind(ip: IpAddr, ports: MediaPorts) -> io::Result<Self> {
        let video = UdpSocket::bind(SocketAddr::new(ip, ports.video)).await?;
        let audio = UdpSocket::bind(SocketAddr::new(ip, ports.audio)).await?;
        let control = tokio_enet::Host::new(tokio_enet::HostConfig {
            address: Some(SocketAddr::new(ip, ports.control)),
            peer_count: 4,
            channel_limit: 0x30,
            ..Default::default()
        })
        .map_err(|e| io::Error::other(format!("ENet host: {e}")))?;
        let bound = MediaPorts {
            video: video.local_addr()?.port(),
            audio: audio.local_addr()?.port(),
            control: control
                .local_addr()
                .map_err(|e| io::Error::other(e.to_string()))?
                .port(),
        };
        Ok(Self {
            video,
            audio,
            control,
            ports: bound,
        })
    }

    /// The ports actually bound.
    pub fn ports(&self) -> MediaPorts {
        self.ports
    }
}

#[derive(Clone, Debug)]
pub struct MediaConfig {
    /// How long the control stream may go without a ping before the session
    /// is taken as lost.
    pub stream_timeout: Duration,
}

impl Default for MediaConfig {
    fn default() -> Self {
        Self {
            stream_timeout: Duration::from_secs(60),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EndReason {
    /// [`MediaSession::stop`] was called.
    Stopped,
    /// The client's control connection ended.
    ClientLeft,
    /// No ping within the stream timeout.
    TimedOut,
    /// The backend stopped producing video.
    BackendEnded,
}

#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    #[error(transparent)]
    Backend(#[from] BackendError),
    #[error("setting up the sockets: {0}")]
    Io(#[from] io::Error),
}

/// What the session dropped or refused, and what it did.
#[derive(Default)]
pub(crate) struct MediaStats {
    malformed_control: AtomicU64,
    rejected_control: AtomicU64,
    rejected_pings: AtomicU64,
    rejected_peers: AtomicU64,
    input_events: AtomicU64,
    video_frames: AtomicU64,
    video_dropped: AtomicU64,
    audio_packets: AtomicU64,
}

macro_rules! counter {
    ($($name:ident),*) => {$(
        pub(crate) fn $name(&self) { self.$name.fetch_add(1, Ordering::Relaxed); }
    )*};
}

impl MediaStats {
    counter!(
        malformed_control,
        rejected_control,
        rejected_pings,
        rejected_peers,
        input_events
    );

    pub(crate) fn video_sent(&self) {
        self.video_frames.fetch_add(1, Ordering::Relaxed);
    }
    pub(crate) fn video_dropped(&self) {
        self.video_dropped.fetch_add(1, Ordering::Relaxed);
    }
    pub(crate) fn audio_sent(&self) {
        self.audio_packets.fetch_add(1, Ordering::Relaxed);
    }

    fn snapshot(&self) -> MediaStatsSnapshot {
        let get = |a: &AtomicU64| a.load(Ordering::Relaxed);
        MediaStatsSnapshot {
            malformed_control_packets: get(&self.malformed_control),
            rejected_control_packets: get(&self.rejected_control),
            rejected_pings: get(&self.rejected_pings),
            rejected_control_peers: get(&self.rejected_peers),
            input_events: get(&self.input_events),
            video_frames_sent: get(&self.video_frames),
            video_frames_dropped: get(&self.video_dropped),
            audio_packets_sent: get(&self.audio_packets),
        }
    }
}

/// A point-in-time copy of a session's counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MediaStatsSnapshot {
    /// Control packets that did not parse; dropped, the session went on.
    pub malformed_control_packets: u64,
    /// Control packets refused for no encryption or failing authentication.
    pub rejected_control_packets: u64,
    /// Video and audio datagrams that were not the client's `PING`.
    pub rejected_pings: u64,
    /// ENet connections refused (wrong address, wrong connect data, second peer).
    pub rejected_control_peers: u64,
    pub input_events: u64,
    pub video_frames_sent: u64,
    pub video_frames_dropped: u64,
    pub audio_packets_sent: u64,
}

/// State the three stream tasks share.
pub(crate) struct Shared {
    pub stats: MediaStats,
    started: AtomicBool,
    pub frames: Mutex<FrameMap>,
    stop: watch::Sender<bool>,
    end: watch::Sender<Option<EndReason>>,
    pub hdr: watch::Sender<Option<HdrMetadata>>,
}

impl Shared {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            stats: MediaStats::default(),
            started: AtomicBool::new(false),
            frames: Mutex::default(),
            stop: watch::channel(false).0,
            end: watch::channel(None).0,
            hdr: watch::channel(None).0,
        })
    }

    /// The client said `StartB`: video and audio may flow.
    pub fn start(&self) {
        self.started.store(true, Ordering::Release);
    }
    pub fn is_started(&self) -> bool {
        self.started.load(Ordering::Acquire)
    }
    pub fn stop_rx(&self) -> watch::Receiver<bool> {
        self.stop.subscribe()
    }
    pub fn end_reason(&self) -> Option<EndReason> {
        *self.end.borrow()
    }
    /// Ends the session with `reason`, the first one given.
    pub fn end(&self, reason: EndReason) {
        self.end
            .send_if_modified(|r| r.is_none().then(|| *r = Some(reason)).is_some());
        self.stop.send_replace(true);
    }
}

/// Resolves when the session is stopping (or its sender is gone).
pub(crate) async fn stopped(rx: &mut watch::Receiver<bool>) {
    crate::net::flagged(rx).await;
}

/// A running session's media. Dropping it stops the streams without the
/// goodbye [`stop`](Self::stop) sends.
pub struct MediaSession {
    shared: Arc<Shared>,
    end: watch::Receiver<Option<EndReason>>,
    /// True once the streams and the backend are done.
    done: watch::Receiver<bool>,
}

impl MediaSession {
    /// Starts the backend for `handoff.params` and serves the session on
    /// `sockets`. Video and audio stay quiet until the client starts them.
    pub async fn start(
        handoff: SessionHandoff,
        backend: Arc<dyn MediaBackend>,
        sockets: MediaSockets,
        config: MediaConfig,
    ) -> Result<Self, MediaError> {
        if let Some(what) = backend.capabilities().unsupported(&handoff.params) {
            return Err(BackendError::Unsupported(what).into());
        }
        let streams = backend.start(handoff.params.clone()).await?;
        let MediaSockets {
            video,
            audio,
            control: enet,
            ..
        } = sockets;
        let video = video::sender::VideoSocket::new(video)?;

        let shared = Shared::new();
        let end = shared.end.subscribe();
        let control = streams.control.clone();

        let video_task = tokio::spawn(video::run(video::VideoTask {
            socket: video,
            handoff: handoff.clone(),
            frames: streams.video,
            control: control.clone(),
            shared: shared.clone(),
            hdr: shared.hdr.subscribe(),
        }));
        let audio_task = tokio::spawn(audio::run(audio::AudioTask {
            socket: audio,
            handoff: handoff.clone(),
            packets: streams.audio,
            shared: shared.clone(),
        }));
        let control_task = tokio::spawn(control::run(control::ControlTask {
            host: enet,
            handoff,
            feedback: streams.feedback,
            control: control.clone(),
            shared: shared.clone(),
            stream_timeout: config.stream_timeout,
        }));

        let (done_tx, done) = watch::channel(false);
        tokio::spawn({
            let shared = shared.clone();
            async move {
                // The control task ends first, having said goodbye; then the
                // rest, then the backend.
                let _ = control_task.await;
                shared.end(EndReason::ClientLeft);
                let _ = tokio::join!(video_task, audio_task);
                control.release_input();
                control.stop().await;
                done_tx.send_replace(true);
            }
        });
        Ok(Self { shared, end, done })
    }

    /// Why the session ended, once it has.
    pub async fn closed(&self) -> EndReason {
        let mut end = self.end.clone();
        match end.wait_for(Option::is_some).await {
            Ok(r) => r.expect("waited for Some"),
            Err(_) => EndReason::Stopped,
        }
    }

    pub fn stats(&self) -> MediaStatsSnapshot {
        self.shared.stats.snapshot()
    }

    /// Ends the session: the client is told, the streams stop, the backend is
    /// stopped. Returns when all that is done.
    pub async fn stop(&self) {
        self.shared.end(EndReason::Stopped);
        self.join().await;
    }

    /// Waits until the streams and the backend are done.
    pub async fn join(&self) {
        let mut done = self.done.clone();
        let _ = done.wait_for(|d| *d).await;
    }
}

impl Drop for MediaSession {
    fn drop(&mut self) {
        self.shared.end(EndReason::Stopped);
    }
}

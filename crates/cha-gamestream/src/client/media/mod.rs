//! One stream's media as a Moonlight client receives it (ADR 0011): the
//! ENet control channel, video with full-packet Reed-Solomon recovery and
//! AES-GCM decryption, audio with RS(4,2) and AES-CBC, input encoding and
//! feedback decoding, out as plain channels.
//!
//! It is the client end of `crate::media`, and shares its packet layouts,
//! framing and ciphers. Wire formats and message ids are interop constants
//! (per moonlight-common-c's `ControlStream.c`, `InputStream.c`, `Input.h`,
//! `Video.h`); the receivers and the tasks are written from the behaviour.
//!
//! ```ignore
//! let Media { mut video, audio, feedback, ended, control } =
//!     MediaClient::start(&setup).await?;
//! control.input(InputEvent::MouseMoveRelative { dx: 3, dy: -1 });
//! while let Some(frame) = video.recv().await { /* decode */ }
//! ```
//!
//! A stream lives as long as a [`MediaHandle`] does: the last clone dropped
//! stops it. A frame the network lost is never fatal: it is dropped, the host
//! is asked for a keyframe (or a reference-frame invalidation) and the stream
//! resumes at the next picture it can start from.

use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::net::UdpSocket;
use tokio::sync::{Notify, mpsc, oneshot, watch};
use tokio_enet::{Event, Host, HostConfig, Packet, PacketMode, PeerId};

use crate::backend::Feedback;
use crate::input::InputEvent;
use crate::net::{flagged, same_ip};

use super::{AudioPacket, StreamSetup, VideoFrame};

mod audio;
mod control;
mod input;
mod video;

pub use audio::AudioStats;
pub use video::{VideoStats, VideoTiming};

use audio::{AudioConfig, AudioReceiver};
use control::{HostMessage, Sealer};
use input::InputQueue;
use video::{VideoConfig, VideoEvent, VideoReceiver};

/// ENet channels (numbers per moonlight-common-c's `CTRL_CHANNEL_*`): `0` for
/// everything generic, `1` for the urgent requests.
const CHANNEL_GENERIC: u8 = 0;
const CHANNEL_URGENT: u8 = 1;
/// `CTRL_CHANNEL_COUNT`: the channels the client asks the host for.
const CHANNEL_COUNT: usize = 0x30;
/// How often video and audio `PING`s go out (`moonlight-common-c`: 500 ms).
const PING_INTERVAL: Duration = Duration::from_millis(500);
/// How often the control ping goes out (`PERIODIC_PING_INTERVAL_MS`).
const CONTROL_PING_INTERVAL: Duration = Duration::from_millis(100);
/// The receivers' clock for giving up on missing packets.
const RECEIVER_TICK: Duration = Duration::from_millis(5);
/// How long the control loop waits for the network before it looks at its
/// queues again.
const SERVICE_TICK: Duration = Duration::from_millis(5);
/// Relative mouse motion goes out at most this often (`MOUSE_BATCHING_INTERVAL_MS`).
const MOUSE_BATCHING_INTERVAL: Duration = Duration::from_millis(1);
/// How long a stopping client waits for the host to acknowledge its goodbye.
const LINGER: Duration = Duration::from_secs(1);
/// The least time a control connection lives before its goodbye. ENet throws
/// away a connect and a disconnect that arrive together, before the host has
/// dispatched the connect, so the host would never see the client leave and
/// its media would run on until the ping timeout.
const SETTLE: Duration = Duration::from_millis(100);
/// ENet peer timeouts as Moonlight sets them: limit 2, 10 s.
const PEER_TIMEOUT_MS: u32 = 10_000;
/// Receive buffer asked of the kernel for video: a whole keyframe's burst.
const VIDEO_RECV_BUFFER: usize = 4 << 20;
const AUDIO_RECV_BUFFER: usize = 256 << 10;

/// Knobs of a stream that [`StreamSetup`] doesn't carry.
#[derive(Clone, Debug)]
pub struct MediaOptions {
    /// After a loss, ask the host to invalidate the lost frames' references
    /// (reference frame invalidation) instead of asking for a keyframe. Only
    /// for hosts that support it; the default is the keyframe, which every
    /// host answers.
    pub invalidate_refs: bool,
    /// How long to wait for the control connection.
    pub connect_timeout: Duration,
    /// How long the host may send no video at all before the stream is taken
    /// as failed.
    pub first_video_timeout: Duration,
    /// How long the video receiver waits on missing packets before it gives
    /// a frame up, and between its requests to the host.
    pub video_timing: VideoTiming,
    /// How long audio waits on a missing packet once later ones are in.
    pub audio_reorder_window: Duration,
    /// Frames queued for a slow consumer before the receiver drops them (and
    /// asks the host for a keyframe).
    pub video_queue: usize,
    pub audio_queue: usize,
    pub feedback_queue: usize,
}

impl Default for MediaOptions {
    fn default() -> Self {
        Self {
            invalidate_refs: false,
            connect_timeout: Duration::from_secs(10),
            first_video_timeout: Duration::from_secs(10),
            video_timing: VideoTiming::default(),
            audio_reorder_window: Duration::from_millis(10),
            video_queue: 8,
            audio_queue: 64,
            feedback_queue: 64,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    #[error("setting up the sockets: {0}")]
    Io(#[from] io::Error),
    #[error("the control connection: {0}")]
    Control(String),
    #[error("the stream setup is unusable: {0}")]
    Setup(String),
}

/// How the stream ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ended {
    /// [`MediaHandle::stop`] (or the last handle dropped).
    Stopped,
    /// The host ended it and said why: `code` is its status (Moonlight's
    /// `NVST_DISCONN_*`, big-endian in the message); `graceful` is the host
    /// closing on purpose (`0x80030023`), which is not an error.
    Terminated { code: u32, graceful: bool },
    /// The connection or the host went away.
    Failed(String),
}

/// What the client dropped, refused and did, over the stream's life.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MediaStats {
    pub video: VideoStats,
    pub audio: AudioStats,
    /// Control messages that didn't parse; dropped, the stream went on.
    pub malformed_control: u64,
    /// Control messages refused for no encryption or failing authentication.
    pub rejected_control: u64,
    /// Input events dropped because the queue was full.
    pub input_dropped: u64,
    /// Frames dropped because the consumer was too slow.
    pub video_dropped_slow: u64,
    /// Audio packets dropped because the consumer was too slow.
    pub audio_dropped_slow: u64,
    /// Feedback messages dropped because the consumer was too slow.
    pub feedback_dropped_slow: u64,
}

/// A running stream: the media out of it, and the handle to steer it.
pub struct Media {
    /// Whole access units in frame order. Closes when the stream ends.
    pub video: mpsc::Receiver<VideoFrame>,
    /// Opus packets in order, gaps skipped.
    pub audio: mpsc::Receiver<AudioPacket>,
    /// Rumble, triggers, LEDs, motion enable and HDR mode from the host.
    pub feedback: mpsc::Receiver<Feedback>,
    /// Why the stream ended, once it has.
    pub ended: oneshot::Receiver<Ended>,
    pub control: MediaHandle,
}

/// What a request for the host is.
enum Command {
    Idr,
    Invalidate { first: u32, last: u32 },
}

struct Shared {
    stop: watch::Sender<bool>,
    ended: Mutex<Option<oneshot::Sender<Ended>>>,
    /// Why the stream ended, the first reason given.
    reason: Mutex<Option<Ended>>,
    commands: mpsc::UnboundedSender<Command>,
    input: Mutex<InputQueue>,
    input_ready: Notify,
    stats: Mutex<MediaStats>,
}

impl Shared {
    /// Ends the stream with `reason`, the first one given.
    fn finish(&self, reason: Ended) {
        if let Some(tx) = self.ended.lock().expect("ended").take() {
            *self.reason.lock().expect("reason") = Some(reason.clone());
            let _ = tx.send(reason);
        }
        self.stop.send_replace(true);
    }

    fn ask(&self, command: Command) {
        let _ = self.commands.send(command);
    }

    fn update(&self, f: impl FnOnce(&mut MediaStats)) {
        f(&mut self.stats.lock().expect("stats"));
    }
}

/// Stops the stream when the last [`MediaHandle`] is gone.
struct StopOnDrop(Arc<Shared>);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        self.0.finish(Ended::Stopped);
    }
}

/// Steers a running stream. Cheap to clone; the stream stops when the last
/// clone is dropped.
#[derive(Clone)]
pub struct MediaHandle {
    shared: Arc<Shared>,
    _guard: Arc<StopOnDrop>,
}

impl MediaHandle {
    /// Sends an input event over the control stream, encoded and batched as
    /// Moonlight does (relative mouse motion adds up and goes out at most once
    /// a millisecond; a position, a pad's axes and a sensor keep the latest).
    pub fn input(&self, event: InputEvent) {
        let queued = self.shared.input.lock().expect("input").push(event);
        if queued {
            self.shared.input_ready.notify_one();
        }
    }

    /// Asks the host for a keyframe.
    pub fn request_idr(&self) {
        self.shared.ask(Command::Idr);
    }

    /// Tells the host frames `first..=last` (wire numbers) were lost, so it
    /// can stop referencing them: reference frame invalidation, for hosts
    /// that support it. The client calls it itself after a loss when
    /// [`MediaOptions::invalidate_refs`] is on.
    pub fn invalidate(&self, first_frame: u32, last_frame: u32) {
        self.shared.ask(Command::Invalidate {
            first: first_frame,
            last: last_frame,
        });
    }

    /// Ends the stream: the host is told, the tasks stop and [`Media::ended`]
    /// resolves with [`Ended::Stopped`].
    pub fn stop(&self) {
        self.shared.finish(Ended::Stopped);
    }

    /// What the stream has done and dropped so far.
    pub fn stats(&self) -> MediaStats {
        let mut s = *self.shared.stats.lock().expect("stats");
        s.input_dropped = self.shared.input.lock().expect("input").dropped();
        s
    }
}

/// Starts media streams.
pub struct MediaClient;

impl MediaClient {
    /// Starts the stream `setup` describes with the default [`MediaOptions`].
    pub async fn start(setup: &StreamSetup) -> Result<Media, MediaError> {
        Self::start_with(setup, MediaOptions::default()).await
    }

    /// Binds the video and audio sockets and pings the host from them,
    /// connects the control peer, sends the start messages and runs until
    /// stopped or the host ends the stream.
    pub async fn start_with(
        setup: &StreamSetup,
        options: MediaOptions,
    ) -> Result<Media, MediaError> {
        if setup.packet_size < 17 || setup.packet_size > 8192 {
            return Err(MediaError::Setup(format!(
                "a video packet size of {}",
                setup.packet_size
            )));
        }
        let video_addr = SocketAddr::new(setup.host, setup.ports.video);
        let audio_addr = SocketAddr::new(setup.host, setup.ports.audio);
        let control_addr = SocketAddr::new(setup.host, setup.ports.control);
        let local = unspecified(setup.host);
        let video_socket = bind_udp(local, VIDEO_RECV_BUFFER)?;
        let audio_socket = bind_udp(local, AUDIO_RECV_BUFFER)?;
        let mut enet = Host::new(HostConfig {
            address: Some(local),
            peer_count: 1,
            channel_limit: CHANNEL_COUNT,
            ..Default::default()
        })
        .map_err(|e| MediaError::Control(e.to_string()))?;

        let (stop, _) = watch::channel(false);
        let (ended_tx, ended_rx) = oneshot::channel();
        let (commands_tx, commands_rx) = mpsc::unbounded_channel();
        let shared = Arc::new(Shared {
            stop,
            ended: Mutex::new(Some(ended_tx)),
            reason: Mutex::new(None),
            commands: commands_tx,
            input: Mutex::default(),
            input_ready: Notify::new(),
            stats: Mutex::default(),
        });
        let (video_tx, video_rx) = mpsc::channel(options.video_queue.max(1));
        let (audio_tx, audio_rx) = mpsc::channel(options.audio_queue.max(1));
        let (feedback_tx, feedback_rx) = mpsc::channel(options.feedback_queue.max(1));
        let media = Media {
            video: video_rx,
            audio: audio_rx,
            feedback: feedback_rx,
            ended: ended_rx,
            control: MediaHandle {
                shared: shared.clone(),
                _guard: Arc::new(StopOnDrop(shared.clone())),
            },
        };

        // Video and audio ping first: some hosts (and ours, for the keyframe it
        // starts the stream with) only send to an address they have heard from.
        let ping_payload = setup.ping_payload;
        tokio::spawn(video_task(VideoTask {
            socket: video_socket,
            host: setup.host,
            to: video_addr,
            ping_payload,
            receiver: VideoReceiver::new(
                VideoConfig {
                    timing: options.video_timing,
                    packet_size: setup.packet_size,
                    key: setup.encryption.video.then_some(setup.keys.key),
                    codec: setup.codec,
                    invalidate_refs: options.invalidate_refs,
                },
                Instant::now(),
            ),
            frames: video_tx,
            shared: shared.clone(),
            first_video_timeout: options.first_video_timeout,
        }));
        tokio::spawn(audio_task(AudioTask {
            socket: audio_socket,
            host: setup.host,
            to: audio_addr,
            ping_payload,
            receiver: AudioReceiver::new(AudioConfig {
                reorder_window: options.audio_reorder_window,
                key: setup
                    .encryption
                    .audio
                    .then_some((setup.keys.key, setup.keys.key_id)),
                packet_duration_ms: setup.audio.packet_duration_ms.max(1),
            }),
            packets: audio_tx,
            shared: shared.clone(),
        }));

        let peer = match connect_control(&mut enet, control_addr, setup, &options).await {
            Ok(peer) => peer,
            Err(e) => {
                shared.finish(Ended::Stopped);
                return Err(e);
            }
        };
        tokio::spawn(control_task(ControlTask {
            enet,
            peer,
            host: setup.host,
            key: setup.keys.key,
            commands: commands_rx,
            feedback: feedback_tx,
            shared,
        }));
        Ok(media)
    }
}

fn unspecified(like: IpAddr) -> SocketAddr {
    match like {
        IpAddr::V4(_) => SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), 0),
        IpAddr::V6(_) => SocketAddr::new(Ipv6Addr::UNSPECIFIED.into(), 0),
    }
}

/// A UDP socket with a receive buffer of the size asked, when the system
/// grants it.
fn bind_udp(local: SocketAddr, recv_buffer: usize) -> io::Result<UdpSocket> {
    let socket = socket2::Socket::new(
        socket2::Domain::for_address(local),
        socket2::Type::DGRAM,
        Some(socket2::Protocol::UDP),
    )?;
    if let Err(e) = socket.set_recv_buffer_size(recv_buffer) {
        tracing::debug!("a receive buffer of {recv_buffer} bytes was refused: {e}");
    }
    socket.bind(&local.into())?;
    socket.set_nonblocking(true)?;
    UdpSocket::from_std(socket.into())
}

/// The datagram a video or audio `PING` is: Sunshine's (the session's ping
/// payload and a counter, big-endian) when the host gave a payload, the
/// legacy four bytes when it didn't.
fn ping_datagram(payload: &[u8; 16], counter: u32) -> Vec<u8> {
    if payload[0] == 0 {
        return b"PING".to_vec();
    }
    let mut d = payload.to_vec();
    d.extend_from_slice(&counter.to_be_bytes());
    d
}

// ---------------------------------------------------------------------------
// Video

struct VideoTask {
    socket: UdpSocket,
    host: IpAddr,
    to: SocketAddr,
    ping_payload: [u8; 16],
    receiver: VideoReceiver,
    frames: mpsc::Sender<VideoFrame>,
    shared: Arc<Shared>,
    first_video_timeout: Duration,
}

async fn video_task(task: VideoTask) {
    let VideoTask {
        socket,
        host,
        to,
        ping_payload,
        mut receiver,
        frames,
        shared,
        first_video_timeout,
    } = task;
    let mut stop = shared.stop.subscribe();
    let mut buf = vec![0u8; 65_536];
    let mut ping = tokio::time::interval(PING_INTERVAL);
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut tick = tokio::time::interval(RECEIVER_TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut pings = 0u32;
    let started = Instant::now();
    let mut heard = false;
    let mut slow_dropped = 0u64;

    loop {
        tokio::select! {
            () = flagged(&mut stop) => break,
            _ = ping.tick() => {
                pings += 1;
                // Errors surface on the read side: the host may not have bound its port yet.
                let _ = socket.send_to(&ping_datagram(&ping_payload, pings), to).await;
                if !heard && started.elapsed() >= first_video_timeout {
                    tracing::warn!("no video from {to}");
                    shared.finish(Ended::Failed("no video traffic from the host".into()));
                    break;
                }
            }
            _ = tick.tick() => {
                // What is already in the socket comes first: a busy client must not
                // take its own backlog for the network going quiet.
                while let Ok((n, from)) = socket.try_recv_from(&mut buf) {
                    if same_ip(from.ip(), host) {
                        heard = true;
                        let began = Instant::now();
                        receiver.datagram(&buf[..n], began);
                        receiver.processing_took(began.elapsed());
                    }
                }
                receiver.tick(Instant::now());
            }
            received = socket.recv_from(&mut buf) => {
                let (n, from) = match received {
                    Ok(r) => r,
                    Err(e) if matches!(
                        e.kind(),
                        io::ErrorKind::ConnectionReset | io::ErrorKind::ConnectionRefused
                    ) => continue,
                    Err(e) => {
                        shared.finish(Ended::Failed(format!("video socket: {e}")));
                        break;
                    }
                };
                if !same_ip(from.ip(), host) {
                    continue;
                }
                heard = true;
                let began = Instant::now();
                receiver.datagram(&buf[..n], began);
                let spent = began.elapsed();
                if spent >= Duration::from_millis(1) {
                    receiver.processing_took(spent);
                }
            }
        }
        for event in receiver.take_events() {
            match event {
                VideoEvent::Frame(frame) => {
                    let number = frame.number;
                    match frames.try_send(frame) {
                        Ok(()) => {}
                        Err(mpsc::error::TrySendError::Full(_)) => {
                            // The consumer can't keep up: what it never got is a loss.
                            slow_dropped += 1;
                            receiver.dropped_downstream(number, Instant::now());
                        }
                        // Nobody is reading video; carry on for the rest of the stream.
                        Err(mpsc::error::TrySendError::Closed(_)) => {}
                    }
                }
                VideoEvent::RequestIdr => shared.ask(Command::Idr),
                VideoEvent::Invalidate { first, last } => {
                    shared.ask(Command::Invalidate { first, last });
                }
            }
        }
        let stats = receiver.stats;
        shared.update(|s| {
            s.video = stats;
            s.video_dropped_slow = slow_dropped;
        });
    }
    tracing::debug!("video stream stopped: {:?}", receiver.stats);
}

// ---------------------------------------------------------------------------
// Audio

struct AudioTask {
    socket: UdpSocket,
    host: IpAddr,
    to: SocketAddr,
    ping_payload: [u8; 16],
    receiver: AudioReceiver,
    packets: mpsc::Sender<AudioPacket>,
    shared: Arc<Shared>,
}

async fn audio_task(task: AudioTask) {
    let AudioTask {
        socket,
        host,
        to,
        ping_payload,
        mut receiver,
        packets,
        shared,
    } = task;
    let mut stop = shared.stop.subscribe();
    let mut buf = vec![0u8; 4096];
    let mut ping = tokio::time::interval(PING_INTERVAL);
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut tick = tokio::time::interval(RECEIVER_TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut pings = 0u32;
    let mut slow_dropped = 0u64;

    loop {
        tokio::select! {
            () = flagged(&mut stop) => break,
            _ = ping.tick() => {
                pings += 1;
                let _ = socket.send_to(&ping_datagram(&ping_payload, pings), to).await;
            }
            _ = tick.tick() => {
                while let Ok((n, from)) = socket.try_recv_from(&mut buf) {
                    if same_ip(from.ip(), host) {
                        receiver.datagram(&buf[..n], Instant::now());
                    }
                }
                receiver.tick(Instant::now());
            }
            received = socket.recv_from(&mut buf) => {
                let (n, from) = match received {
                    Ok(r) => r,
                    Err(e) if matches!(
                        e.kind(),
                        io::ErrorKind::ConnectionReset | io::ErrorKind::ConnectionRefused
                    ) => continue,
                    Err(e) => {
                        // Audio is not worth the stream: carry on without it.
                        tracing::warn!("audio socket: {e}");
                        break;
                    }
                };
                if !same_ip(from.ip(), host) {
                    continue;
                }
                receiver.datagram(&buf[..n], Instant::now());
            }
        }
        for packet in receiver.take_packets() {
            if let Err(mpsc::error::TrySendError::Full(_)) = packets.try_send(packet) {
                slow_dropped += 1;
            }
        }
        let stats = receiver.stats;
        shared.update(|s| {
            s.audio = stats;
            s.audio_dropped_slow = slow_dropped;
        });
    }
    tracing::debug!("audio stream stopped: {:?}", receiver.stats);
}

// ---------------------------------------------------------------------------
// Control

/// Connects the control peer with the connect data the host gave, and waits
/// for the connection.
async fn connect_control(
    enet: &mut Host,
    to: SocketAddr,
    setup: &StreamSetup,
    options: &MediaOptions,
) -> Result<PeerId, MediaError> {
    let peer = enet
        .connect(to, CHANNEL_COUNT, setup.control_connect_data)
        .map_err(|e| MediaError::Control(e.to_string()))?;
    let deadline = tokio::time::Instant::now() + options.connect_timeout;
    loop {
        let wait = deadline.saturating_duration_since(tokio::time::Instant::now());
        if wait.is_zero() {
            return Err(MediaError::Control(format!("no answer from {to}")));
        }
        match enet.service(wait.min(Duration::from_millis(50))).await {
            Ok(Some(Event::Connect { peer_id, .. })) if peer_id == peer => {
                if let Some(p) = enet.peer_mut(peer) {
                    // Moonlight: limit 2, minimum and maximum of 10 seconds.
                    p.set_timeout(2, PEER_TIMEOUT_MS, PEER_TIMEOUT_MS);
                }
                return Ok(peer);
            }
            Ok(Some(Event::Disconnect { .. })) => {
                return Err(MediaError::Control(format!("{to} refused the connection")));
            }
            Ok(_) => {}
            Err(e) => return Err(MediaError::Control(e.to_string())),
        }
    }
}

struct ControlTask {
    enet: Host,
    peer: PeerId,
    host: IpAddr,
    key: [u8; 16],
    commands: mpsc::UnboundedReceiver<Command>,
    feedback: mpsc::Sender<Feedback>,
    shared: Arc<Shared>,
}

/// The connection to send on.
struct Link {
    enet: Host,
    peer: PeerId,
    sealer: Sealer,
}

impl Link {
    /// Seals and queues a message, on `channel` or the generic one if the
    /// peer has fewer channels.
    fn send(&mut self, inner: &[u8], channel: u8, mode: PacketMode) -> Result<(), String> {
        let wire = self.sealer.seal(inner);
        let peer = self
            .enet
            .peer_mut(self.peer)
            .ok_or_else(|| "the control peer is gone".to_owned())?;
        let channel = if usize::from(channel) < peer.channel_count() {
            channel
        } else {
            CHANNEL_GENERIC
        };
        peer.send(channel, Packet::new(&wire, mode))
            .map_err(|e| e.to_string())
    }

    fn reliable(&mut self, inner: &[u8], channel: u8) -> Result<(), String> {
        self.send(inner, channel, PacketMode::ReliableSequenced)
    }
}

async fn control_task(task: ControlTask) {
    let ControlTask {
        enet,
        peer,
        host,
        key,
        mut commands,
        feedback,
        shared,
    } = task;
    let mut link = Link {
        enet,
        peer,
        sealer: Sealer::new(key),
    };
    let connected = tokio::time::Instant::now();
    let mut stop = shared.stop.subscribe();
    let mut ping = tokio::time::interval(CONTROL_PING_INTERVAL);
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_mouse = Instant::now() - Duration::from_secs(1);
    let mut malformed = 0u64;
    let mut rejected = 0u64;
    let mut feedback_dropped = 0u64;

    // What Moonlight sends on connecting: "start A" (a keyframe request, with an
    // encrypted stream) then "start B", which tells the host to begin.
    let started = link
        .reliable(&control::start_a(), CHANNEL_GENERIC)
        .and_then(|()| link.reliable(&control::start_b(), CHANNEL_GENERIC));
    if let Err(e) = started {
        shared.finish(Ended::Failed(format!("starting the stream: {e}")));
    }

    'run: loop {
        let sent = tokio::select! {
            () = flagged(&mut stop) => break 'run,
            Some(command) = commands.recv() => match command {
                Command::Idr => link.reliable(&control::request_idr(), CHANNEL_URGENT),
                Command::Invalidate { first, last } => {
                    link.reliable(&control::invalidate_refs(first, last), CHANNEL_URGENT)
                }
            },
            () = shared.input_ready.notified() => {
                let wait = {
                    let queue = shared.input.lock().expect("input");
                    (queue.has_mouse_motion())
                        .then(|| MOUSE_BATCHING_INTERVAL.saturating_sub(last_mouse.elapsed()))
                };
                if let Some(wait) = wait.filter(|w| !w.is_zero()) {
                    tokio::time::sleep(wait).await;
                }
                let (packets, motion) = {
                    let mut queue = shared.input.lock().expect("input");
                    let motion = queue.has_mouse_motion();
                    (queue.drain(), motion)
                };
                if motion {
                    last_mouse = Instant::now();
                }
                packets.iter().try_for_each(|p| {
                    let mode = if p.reliable {
                        PacketMode::ReliableSequenced
                    } else {
                        PacketMode::UnreliableSequenced
                    };
                    link.send(&control::input(&p.data), p.channel, mode)
                })
            },
            _ = ping.tick() => link.reliable(&control::ping(), CHANNEL_GENERIC),
            event = link.enet.service(SERVICE_TICK) => {
                match event {
                    Ok(Some(Event::Receive { peer_id, packet, .. })) if peer_id == link.peer => {
                        match handle_host_packet(&key, packet.data()) {
                            Incoming::Feedback(fb) => {
                                if feedback.try_send(fb).is_err() {
                                    feedback_dropped += 1;
                                }
                            }
                            Incoming::Ended(reason) => {
                                shared.finish(reason);
                                break 'run;
                            }
                            Incoming::Malformed => malformed += 1,
                            Incoming::Rejected => rejected += 1,
                            Incoming::Ignored => {}
                        }
                        Ok(())
                    }
                    Ok(Some(Event::Disconnect { peer_id, .. })) if peer_id == link.peer => {
                        shared.finish(Ended::Failed("the host closed the control connection".into()));
                        break 'run;
                    }
                    Ok(_) => Ok(()),
                    Err(e) => Err(e.to_string()),
                }
            },
        };
        if let Err(e) = sent {
            tracing::warn!("control stream: {e}");
            shared.finish(Ended::Failed(format!("control stream: {e}")));
            break;
        }
        shared.update(|s| {
            s.malformed_control = malformed;
            s.rejected_control = rejected;
            s.feedback_dropped_slow = feedback_dropped;
        });
        let _ = host;
    }

    // A client that stopped says goodbye, so the host lets go of held keys and
    // the pad; one the host ended has nobody left to tell.
    let graceful = matches!(
        *shared.reason.lock().expect("reason"),
        None | Some(Ended::Stopped)
    );
    if graceful {
        // The host's next service rounds must see the connection first.
        let settled = connected + SETTLE;
        while tokio::time::Instant::now() < settled {
            if link.enet.service(Duration::from_millis(10)).await.is_err() {
                break;
            }
        }
        if let Some(p) = link.enet.peer_mut(link.peer) {
            p.disconnect(0);
        }
        let deadline = tokio::time::Instant::now() + LINGER;
        while tokio::time::Instant::now() < deadline {
            match link.enet.service(Duration::from_millis(20)).await {
                Ok(Some(Event::Disconnect { .. })) | Err(_) => break,
                _ => {}
            }
        }
    }
    link.enet.disconnect_now(link.peer, 0);
    shared.finish(Ended::Stopped);
    tracing::debug!("control stream stopped");
}

enum Incoming {
    Feedback(Feedback),
    Ended(Ended),
    Malformed,
    Rejected,
    Ignored,
}

/// What one ENet packet from the host is: opened, parsed, and dropped
/// without a word if it is neither.
fn handle_host_packet(key: &[u8; 16], packet: &[u8]) -> Incoming {
    use crate::media::control::crypto;
    use crate::media::control::messages::{Outer, parse_outer};
    let (sequence, tag, ciphertext) = match parse_outer(packet) {
        Err(_) => return Incoming::Malformed,
        // Anyone could have sent a message that isn't encrypted.
        Ok(Outer::Plain { .. }) => return Incoming::Rejected,
        Ok(Outer::Encrypted {
            sequence,
            tag,
            ciphertext,
        }) => (sequence, tag, ciphertext),
    };
    let Ok(inner) = crypto::open_from_host(key, sequence, &tag, ciphertext) else {
        return Incoming::Rejected;
    };
    match control::parse_host(&inner) {
        Ok(HostMessage::Feedback(fb)) => Incoming::Feedback(fb),
        Ok(HostMessage::Terminated { code, graceful }) => {
            Incoming::Ended(Ended::Terminated { code, graceful })
        }
        Ok(HostMessage::Ignored(_)) => Incoming::Ignored,
        Err(_) => Incoming::Malformed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::control::{crypto, feedback, messages};

    const KEY: [u8; 16] = *b"0123456789abcdef";

    /// A host packet that is garbage, plain, forged or cut short is dropped and
    /// counted by kind; the next good one is acted on.
    #[test]
    fn a_bad_host_packet_is_dropped_never_fatal() {
        let good = crypto::seal(&KEY, 7, &feedback::termination(0x8003_0023)).unwrap();
        assert!(matches!(
            handle_host_packet(&KEY, &good),
            Incoming::Ended(Ended::Terminated { graceful: true, .. })
        ));
        let rumble = crypto::seal(
            &KEY,
            8,
            &feedback::encode(&Feedback::Rumble {
                pad: 0,
                low: 1,
                high: 2,
            })
            .unwrap(),
        )
        .unwrap();
        assert!(matches!(
            handle_host_packet(&KEY, &rumble),
            Incoming::Feedback(_)
        ));
        // Not framed at all, an unencrypted message, a forged one, a short rumble.
        assert!(matches!(
            handle_host_packet(&KEY, &[1, 2, 3]),
            Incoming::Malformed
        ));
        let plain = messages::frame(0x0109, &[0, 0, 0, 1]);
        assert!(matches!(
            handle_host_packet(&KEY, &plain),
            Incoming::Rejected
        ));
        let forged = crypto::seal(&[9; 16], 1, &feedback::termination(1)).unwrap();
        assert!(matches!(
            handle_host_packet(&KEY, &forged),
            Incoming::Rejected
        ));
        let short = crypto::seal(&KEY, 9, &messages::frame(0x010b, &[1, 2])).unwrap();
        assert!(matches!(
            handle_host_packet(&KEY, &short),
            Incoming::Malformed
        ));
        let unknown = crypto::seal(&KEY, 10, &messages::frame(0x7777, &[1])).unwrap();
        assert!(matches!(
            handle_host_packet(&KEY, &unknown),
            Incoming::Ignored
        ));
    }

    #[test]
    fn pings_are_sunshines_or_the_legacy_four_bytes() {
        let d = ping_datagram(b"0123456789ABCDEF", 3);
        assert_eq!(&d[..16], b"0123456789ABCDEF");
        assert_eq!(&d[16..], &3u32.to_be_bytes());
        assert_eq!(ping_datagram(&[0; 16], 9), b"PING");
    }
}

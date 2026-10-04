//! Spike S1d: a replayed H.264/HEVC/AV1 stream as a real WebRTC video track,
//! the Phase 1 baseline path. Frames go out as RTP with playout-delay 0/0, so
//! Chrome renders each one as soon as it decodes. A reliable `control`
//! DataChannel carries clock-sync pings and the send time of every frame.

use std::io::ErrorKind;
use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result, bail};
use str0m::change::{SdpAnswer, SdpOffer};
use str0m::channel::ChannelId;
use str0m::format::Codec;
use str0m::media::{Frequency, MediaKind, MediaTime, Mid};
use str0m::net::{Protocol, Receive};
use str0m::rtp::{AbsCaptureTime, Extension, ExtensionMap};
use str0m::{Candidate, Event, IceConnectionState, Input, Output, Rtc};
use tracing::{info, warn};

use crate::stream::Stream;
use crate::traffic::{ClientMsg, SenderStats, ServerMsg, TrafficConfig, TransportStats};

const STATS_INTERVAL: Duration = Duration::from_millis(500);
const DRAIN: Duration = Duration::from_secs(1);
const RTP_CLOCK: u64 = 90_000;

pub struct MediaParams {
    pub config: TrafficConfig,
    pub host: IpAddr,
    pub port: u16,
    pub stream: Arc<Stream>,
}

pub fn start(params: MediaParams, offer: SdpOffer) -> Result<SdpAnswer> {
    let codec = match params.stream.header["codec"].as_str() {
        Some("h264") => Codec::H264,
        Some("hevc") => Codec::H265,
        Some("av1") => Codec::Av1,
        other => bail!("stream codec {other:?} cannot be sent as RTP"),
    };
    let socket = UdpSocket::bind(SocketAddr::new(params.host, params.port))
        .or_else(|_| UdpSocket::bind(SocketAddr::new(params.host, 0)))
        .with_context(|| format!("binding a UDP socket on {}", params.host))?;
    let local = socket.local_addr()?;

    let mut exts = ExtensionMap::standard();
    exts.set(5, Extension::PlayoutDelay);
    exts.set(6, Extension::AbsoluteCaptureTime);
    let builder = Rtc::builder()
        .set_ice_lite(true)
        .clear_codecs()
        .set_extension_map(exts);
    let builder = match codec {
        Codec::H264 => builder.enable_h264(true),
        Codec::H265 => builder.enable_h265(true),
        _ => builder.enable_av1(true),
    };
    let mut rtc = builder.build(Instant::now());
    rtc.add_local_candidate(Candidate::host(local, "udp")?)
        .context("adding host candidate")?;
    let answer = rtc.sdp_api().accept_offer(offer)?;

    info!(%local, ?codec, config = ?params.config, "webrtc media session");
    std::thread::Builder::new()
        .name(format!("webrtc-media-{}", local.port()))
        .spawn(move || {
            if let Err(err) =
                MediaSession::new(rtc, socket, codec, params).and_then(MediaSession::run)
            {
                warn!("webrtc media session ended: {err:#}");
            }
        })?;
    Ok(answer)
}

struct MediaSession {
    rtc: Rtc,
    socket: UdpSocket,
    local: SocketAddr,
    epoch: Instant,
    codec: Codec,
    config: TrafficConfig,
    stream: Arc<Stream>,
    mid: Option<Mid>,
    control: Option<ChannelId>,
    connected: bool,
    frame_id: u32,
    stats: SenderStats,
    keyframe_requests: u64,
    next_frame_at: Option<Instant>,
    next_stats_at: Instant,
    finished_at: Option<Instant>,
}

impl MediaSession {
    fn new(rtc: Rtc, socket: UdpSocket, codec: Codec, params: MediaParams) -> Result<Self> {
        let epoch = Instant::now();
        Ok(Self {
            rtc,
            local: socket.local_addr()?,
            socket,
            epoch,
            codec,
            config: params.config,
            stream: params.stream,
            mid: None,
            control: None,
            connected: false,
            frame_id: 0,
            stats: SenderStats::default(),
            keyframe_requests: 0,
            next_frame_at: None,
            next_stats_at: epoch + STATS_INTERVAL,
            finished_at: None,
        })
    }

    fn run(mut self) -> Result<()> {
        let mut buf = vec![0u8; 2048];
        loop {
            let now = Instant::now();
            if let Some(done) = self.finished_at
                && now >= done + DRAIN
            {
                return Ok(());
            }
            self.maybe_send_frame(now)?;
            if now >= self.next_stats_at {
                self.next_stats_at = now + STATS_INTERVAL;
                let elapsed_ms = self.epoch.elapsed().as_millis() as u64;
                self.send_control(&ServerMsg::Stats {
                    elapsed_ms,
                    sender: self.stats,
                    transport: TransportStats::default(),
                });
            }

            let Some(rtc_deadline) = self.drive()? else {
                return Ok(());
            };
            let mut deadline = rtc_deadline.min(self.next_stats_at);
            if let Some(at) = self.next_frame_at {
                deadline = deadline.min(at);
            }
            let wait = deadline.saturating_duration_since(Instant::now());
            if wait.is_zero() {
                self.rtc.handle_input(Input::Timeout(Instant::now()))?;
                continue;
            }
            self.socket.set_read_timeout(Some(wait))?;
            match self.socket.recv_from(&mut buf) {
                Ok((n, source)) => self.receive(&buf[..n], source)?,
                Err(err) if matches!(err.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                    self.rtc.handle_input(Input::Timeout(Instant::now()))?;
                }
                Err(err) => return Err(err.into()),
            }
        }
    }

    fn maybe_send_frame(&mut self, now: Instant) -> Result<()> {
        let (Some(at), Some(mid)) = (self.next_frame_at, self.mid) else {
            return Ok(());
        };
        if now < at || self.finished_at.is_some() {
            return Ok(());
        }
        let interval = self.config.frame_interval();
        let mut next = at + interval;
        while next <= now {
            next += interval;
        }
        self.next_frame_at = Some(next);

        let id = self.frame_id;
        self.frame_id += 1;
        self.stats.frames_generated += 1;
        let frame = &self.stream.frames[id as usize % self.stream.frames.len()];
        let Some(writer) = self.rtc.writer(mid) else {
            bail!("video media {mid:?} disappeared");
        };
        let Some(pt) = writer
            .payload_params()
            .find(|p| p.spec().codec == self.codec)
            .map(|p| p.pt())
        else {
            bail!("browser did not negotiate {:?}", self.codec);
        };
        let rtp_time = MediaTime::new(
            id as u64 * RTP_CLOCK / self.config.fps as u64,
            Frequency::NINETY_KHZ,
        );
        let s_us = self.epoch.elapsed().as_micros() as u64;
        writer
            .playout_delay(MediaTime::ZERO, MediaTime::ZERO)
            .abs_capture_time(AbsCaptureTime {
                capture_time: SystemTime::now(),
                clock_offset: None,
            })
            .write(pt, now, rtp_time, frame.data.as_slice())?;
        self.stats.frames_sent += 1;
        self.stats.bytes_sent += frame.data.len() as u64;
        self.send_control(&ServerMsg::Sent {
            id,
            rtp: rtp_time.numer() as u32,
            s_us,
        });

        if self.stats.frames_generated >= self.config.total_frames() {
            info!(local = %self.local, stats = ?self.stats, keyframe_requests = self.keyframe_requests, "webrtc media run complete");
            self.finished_at = Some(now);
            self.send_control(&ServerMsg::Done { sender: self.stats });
        }
        Ok(())
    }

    /// Polls output until str0m wants to wait; `None` ends the session.
    fn drive(&mut self) -> Result<Option<Instant>> {
        loop {
            match self.rtc.poll_output()? {
                Output::Timeout(at) => return Ok(Some(at)),
                Output::Transmit(t) => {
                    if let Err(err) = self.socket.send_to(&t.contents, t.destination)
                        && err.kind() != ErrorKind::WouldBlock
                    {
                        return Err(err.into());
                    }
                }
                Output::Event(event) => {
                    if !self.handle_event(event) {
                        return Ok(None);
                    }
                }
            }
        }
    }

    fn handle_event(&mut self, event: Event) -> bool {
        match event {
            Event::IceConnectionStateChange(IceConnectionState::Disconnected) => return false,
            Event::Connected => self.connected = true,
            Event::MediaAdded(m) if m.kind == MediaKind::Video => self.mid = Some(m.mid),
            Event::KeyframeRequest(_) => self.keyframe_requests += 1,
            Event::ChannelOpen(id, label) if label == "control" => {
                self.control = Some(id);
                self.send_control(&ServerMsg::Hello {
                    config: self.config,
                    frame_bytes: self
                        .stream
                        .frames
                        .iter()
                        .map(|f| f.data.len())
                        .max()
                        .unwrap_or(0),
                    fragments_per_frame: 0,
                    max_datagram: 0,
                    total_frames: self.config.total_frames(),
                    stream: Some(self.stream.header.clone()),
                });
            }
            Event::ChannelData(data) if Some(data.id) == self.control => {
                for line in String::from_utf8_lossy(&data.data).lines() {
                    if let Ok(ClientMsg::Ping { c }) = serde_json::from_str::<ClientMsg>(line) {
                        let s_us = self.epoch.elapsed().as_micros() as u64;
                        self.send_control(&ServerMsg::Pong { c, s_us });
                    }
                }
            }
            _ => {}
        }
        // Start the clock once the track, the control channel and ICE are up.
        if self.next_frame_at.is_none()
            && self.connected
            && self.mid.is_some()
            && self.control.is_some()
        {
            self.next_frame_at = Some(Instant::now());
        }
        true
    }

    fn receive(&mut self, contents: &[u8], source: SocketAddr) -> Result<()> {
        let input = Input::Receive(
            Instant::now(),
            Receive {
                proto: Protocol::Udp,
                source,
                destination: self.local,
                contents: contents.try_into()?,
            },
        );
        self.rtc.handle_input(input)?;
        Ok(())
    }

    fn send_control(&mut self, msg: &ServerMsg) {
        let Some(id) = self.control else { return };
        let Some(mut channel) = self.rtc.channel(id) else {
            return;
        };
        let mut line = serde_json::to_string(msg).expect("control messages serialize");
        line.push('\n');
        if !matches!(channel.write(false, line.as_bytes()), Ok(true)) {
            warn!("control channel refused a message");
        }
    }
}

//! The browser side: one WebRTC session that sends the host's frames as an RTP
//! video track with playout-delay 0, exactly as received. A reliable `control`
//! DataChannel carries clock-sync pings, the send time of every frame, and the
//! gateway's own numbers (S1d's protocol, so the S1c/S1d page measures it).

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use str0m::change::{SdpAnswer, SdpOffer};
use str0m::channel::ChannelId;
use str0m::format::Codec as RtpCodec;
use str0m::media::{Frequency, MediaKind, MediaTime, Mid};
use str0m::net::{Protocol, Receive};
use str0m::rtp::{AbsCaptureTime, Extension, ExtensionMap};
use str0m::{Candidate, Event, IceConnectionState, Input, Output, Rtc};
use tokio::net::UdpSocket;
use tokio::time::sleep_until;
use tracing::{info, warn};

use crate::host::{Codec, Gateway, HostFrame, StreamHandle, StreamRequest};

const STATS_INTERVAL: Duration = Duration::from_millis(500);
const DRAIN: Duration = Duration::from_secs(1);
/// Give up if the host sends nothing for this long after launch.
const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(30);

pub struct SessionParams {
    pub request: StreamRequest,
    pub secs: u32,
    pub host: IpAddr,
    pub port: u16,
    pub gateway: Arc<Gateway>,
}

pub async fn start(params: SessionParams, offer: SdpOffer) -> Result<SdpAnswer> {
    let socket = match UdpSocket::bind(SocketAddr::new(params.host, params.port)).await {
        Ok(socket) => socket,
        Err(_) => UdpSocket::bind(SocketAddr::new(params.host, 0))
            .await
            .with_context(|| format!("binding a UDP socket on {}", params.host))?,
    };
    let local = socket.local_addr()?;

    let mut exts = ExtensionMap::standard();
    exts.set(5, Extension::PlayoutDelay);
    exts.set(6, Extension::AbsoluteCaptureTime);
    let builder = Rtc::builder()
        .set_ice_lite(true)
        .clear_codecs()
        .set_extension_map(exts);
    let (builder, rtp_codec) = match params.request.codec {
        Codec::H264 => (builder.enable_h264(true), RtpCodec::H264),
        Codec::Hevc => (builder.enable_h265(true), RtpCodec::H265),
        Codec::Av1 => (builder.enable_av1(true), RtpCodec::Av1),
    };
    let mut rtc = builder.build(Instant::now());
    rtc.add_local_candidate(Candidate::host(local, "udp")?)
        .context("adding host candidate")?;
    let answer = rtc.sdp_api().accept_offer(offer)?;

    info!(%local, request = ?params.request, secs = params.secs, "webrtc session");
    tokio::spawn(async move {
        if let Err(err) = Session::new(rtc, socket, rtp_codec, params).run().await {
            warn!("webrtc session ended: {err:#}");
        }
    });
    Ok(answer)
}

#[derive(Debug, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
enum ClientMsg {
    Ping { c: f64 },
}

#[derive(Debug, Serialize)]
#[serde(tag = "t", rename_all = "snake_case")]
enum ServerMsg {
    Hello {
        stream: serde_json::Value,
    },
    Pong {
        c: f64,
        s_us: u64,
    },
    /// Frame `id` went out with RTP timestamp `rtp` at `s_us` (session clock).
    Sent {
        id: u32,
        rtp: u32,
        s_us: u64,
    },
    Stats {
        elapsed_ms: u64,
        #[serde(flatten)]
        stats: GatewayStats,
    },
    Done {
        #[serde(flatten)]
        stats: GatewayStats,
    },
}

#[derive(Clone, Debug, Default, Serialize)]
struct GatewayStats {
    frames_generated: u64,
    frames_sent: u64,
    bytes_sent: u64,
    keyframes: u64,
    /// Browser PLI/FIR, each forwarded to the host as an IDR request.
    keyframe_requests: u64,
    /// From launch request to the first host frame.
    first_frame_ms: Option<u64>,
    /// Gateway hop: frame complete from the host → handed to str0m.
    hop_us_p50: Option<u64>,
    hop_us_p99: Option<u64>,
    /// The host's own capture → encoded time, when it reports one.
    host_latency_ms_p50: Option<f64>,
    host_latency_ms_p99: Option<f64>,
    /// Frame numbers the host sent that never arrived complete (FEC couldn't recover them).
    host_frames_missing: u64,
    /// Spacing of frame arrivals from the host: the rate it actually produced.
    frame_interval_ms_p50: Option<f64>,
    frame_interval_ms_p99: Option<f64>,
}

struct Session {
    rtc: Rtc,
    socket: UdpSocket,
    local: SocketAddr,
    epoch: Instant,
    rtp_codec: RtpCodec,
    params: SessionParams,
    mid: Option<Mid>,
    control: Option<ChannelId>,
    connected: bool,
    stream: Option<StreamHandle>,
    launched_at: Option<Instant>,
    sending_until: Option<Instant>,
    finished_at: Option<Instant>,
    next_stats_at: Instant,
    frame_id: u32,
    stats: GatewayStats,
    hops_us: Vec<u64>,
    host_latencies_ms: Vec<f64>,
    last_host_frame: Option<(u32, Instant)>,
    frame_intervals_ms: Vec<f64>,
}

impl Session {
    fn new(rtc: Rtc, socket: UdpSocket, rtp_codec: RtpCodec, params: SessionParams) -> Self {
        let epoch = Instant::now();
        Self {
            rtc,
            local: socket.local_addr().expect("bound socket has an address"),
            socket,
            epoch,
            rtp_codec,
            params,
            mid: None,
            control: None,
            connected: false,
            stream: None,
            launched_at: None,
            sending_until: None,
            finished_at: None,
            next_stats_at: epoch + STATS_INTERVAL,
            frame_id: 0,
            stats: GatewayStats::default(),
            hops_us: Vec::new(),
            host_latencies_ms: Vec::new(),
            last_host_frame: None,
            frame_intervals_ms: Vec::new(),
        }
    }

    async fn run(mut self) -> Result<()> {
        let mut buf = vec![0u8; 2048];
        loop {
            let now = Instant::now();
            if let Some(done) = self.finished_at
                && now >= done + DRAIN
            {
                return Ok(());
            }
            if let Some(launched) = self.launched_at
                && self.stats.frames_sent == 0
                && now >= launched + FIRST_FRAME_TIMEOUT
            {
                bail!("no frame from the host {FIRST_FRAME_TIMEOUT:?} after launch");
            }
            if let Some(until) = self.sending_until
                && now >= until
                && self.finished_at.is_none()
            {
                self.finish(now);
            }
            if now >= self.next_stats_at {
                self.next_stats_at = now + STATS_INTERVAL;
                let elapsed_ms = self.epoch.elapsed().as_millis() as u64;
                let stats = self.summary();
                self.send_control(&ServerMsg::Stats { elapsed_ms, stats });
            }

            let Some(rtc_deadline) = self.drive().await? else {
                return Ok(());
            };
            self.maybe_launch()?;
            let mut deadline = rtc_deadline.min(self.next_stats_at);
            if let Some(until) = self.sending_until.filter(|_| self.finished_at.is_none()) {
                deadline = deadline.min(until);
            }

            tokio::select! {
                received = self.socket.recv_from(&mut buf) => {
                    let (n, source) = received?;
                    self.receive(&buf[..n], source)?;
                }
                frame = next_frame(&mut self.stream) => match frame {
                    Some(frame) => self.send_frame(frame)?,
                    None => {
                        info!("host stream ended");
                        self.stream = None;
                        if self.finished_at.is_none() {
                            self.finish(Instant::now());
                        }
                    }
                },
                _ = sleep_until(deadline.into()) => {
                    self.rtc.handle_input(Input::Timeout(Instant::now()))?;
                }
            }
        }
    }

    /// Launches the app on the host once ICE, the track and the control channel are up.
    fn maybe_launch(&mut self) -> Result<()> {
        if self.launched_at.is_some()
            || !self.connected
            || self.mid.is_none()
            || self.control.is_none()
        {
            return Ok(());
        }
        self.launched_at = Some(Instant::now());
        self.stream = Some(self.params.gateway.start(self.params.request.clone())?);
        Ok(())
    }

    fn send_frame(&mut self, frame: HostFrame) -> Result<()> {
        let now = Instant::now();
        if self.finished_at.is_some() {
            return Ok(());
        }
        let Some(mid) = self.mid else { return Ok(()) };
        if self.stats.frames_generated == 0 {
            let first = self
                .launched_at
                .map(|t| now.duration_since(t).as_millis() as u64);
            self.stats.first_frame_ms = first;
            info!(first_frame_ms = ?first, bytes = frame.data.len(), key = frame.key, "first frame to the browser");
            self.sending_until = Some(now + Duration::from_secs(self.params.secs.into()));
        }
        self.stats.frames_generated += 1;
        let Some(writer) = self.rtc.writer(mid) else {
            bail!("video media {mid:?} disappeared");
        };
        let Some(pt) = writer
            .payload_params()
            .find(|p| p.spec().codec == self.rtp_codec)
            .map(|p| p.pt())
        else {
            bail!("browser did not negotiate {:?}", self.rtp_codec);
        };
        // RTP time from when the frame reached the gateway. Wolf stamps every video
        // packet with RTP timestamp 0 (Moonlight clients ignore it), so the host's
        // clock can't be passed through.
        let rtp_time = MediaTime::new(
            (frame
                .received
                .saturating_duration_since(self.epoch)
                .as_micros()
                * 9
                / 100) as u64,
            Frequency::NINETY_KHZ,
        );
        let s_us = self.epoch.elapsed().as_micros() as u64;
        writer
            .playout_delay(MediaTime::ZERO, MediaTime::ZERO)
            .abs_capture_time(AbsCaptureTime {
                capture_time: SystemTime::now(),
                clock_offset: None,
            })
            .write(pt, now, rtp_time, &frame.data[..])?;

        let id = self.frame_id;
        self.frame_id += 1;
        self.stats.frames_sent += 1;
        self.stats.bytes_sent += frame.data.len() as u64;
        self.stats.keyframes += u64::from(frame.key);
        self.hops_us
            .push(now.duration_since(frame.received).as_micros() as u64);
        if let Some((index, received)) = self.last_host_frame {
            self.stats.host_frames_missing +=
                u64::from(frame.index.wrapping_sub(index).saturating_sub(1));
            self.frame_intervals_ms
                .push(frame.received.duration_since(received).as_secs_f64() * 1000.0);
        }
        self.last_host_frame = Some((frame.index, frame.received));
        if let Some(latency) = frame.host_latency {
            self.host_latencies_ms.push(latency.as_secs_f64() * 1000.0);
        }
        self.send_control(&ServerMsg::Sent {
            id,
            rtp: rtp_time.numer() as u32,
            s_us,
        });
        Ok(())
    }

    fn finish(&mut self, now: Instant) {
        self.finished_at = Some(now);
        // Dropping the handle stops the host stream and closes the app.
        self.stream = None;
        let stats = self.summary();
        info!(local = %self.local, ?stats, "gateway run complete");
        self.send_control(&ServerMsg::Done { stats });
    }

    fn summary(&self) -> GatewayStats {
        let mut stats = self.stats.clone();
        let mut hops = self.hops_us.clone();
        hops.sort_unstable();
        stats.hop_us_p50 = percentile(&hops, 0.5);
        stats.hop_us_p99 = percentile(&hops, 0.99);
        let mut host: Vec<f64> = self.host_latencies_ms.clone();
        host.sort_by(f64::total_cmp);
        stats.host_latency_ms_p50 = percentile(&host, 0.5);
        stats.host_latency_ms_p99 = percentile(&host, 0.99);
        let mut intervals = self.frame_intervals_ms.clone();
        intervals.sort_by(f64::total_cmp);
        stats.frame_interval_ms_p50 = percentile(&intervals, 0.5);
        stats.frame_interval_ms_p99 = percentile(&intervals, 0.99);
        stats
    }

    /// Polls output until str0m wants to wait; `None` ends the session.
    async fn drive(&mut self) -> Result<Option<Instant>> {
        loop {
            match self.rtc.poll_output()? {
                Output::Timeout(at) => return Ok(Some(at)),
                Output::Transmit(t) => {
                    if let Err(err) = self.socket.try_send_to(&t.contents, t.destination)
                        && err.kind() != std::io::ErrorKind::WouldBlock
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
            Event::KeyframeRequest(_) => {
                self.stats.keyframe_requests += 1;
                if let Some(stream) = &self.stream {
                    let _ = stream.idr.send(());
                }
            }
            Event::ChannelOpen(id, label) if label == "control" => {
                self.control = Some(id);
                let request = &self.params.request;
                let stream = serde_json::json!({
                    "codec": request.codec.name(),
                    "width": request.width,
                    "height": request.height,
                    "fps": request.fps,
                    "bitrateKbps": request.bitrate_kbps,
                    "app": request.app.title,
                });
                self.send_control(&ServerMsg::Hello { stream });
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

/// The next host frame, or never while no stream is running.
async fn next_frame(stream: &mut Option<StreamHandle>) -> Option<HostFrame> {
    match stream {
        Some(stream) => stream.frames.recv().await,
        None => std::future::pending().await,
    }
}

fn percentile<T: Copy>(sorted: &[T], q: f64) -> Option<T> {
    if sorted.is_empty() {
        return None;
    }
    let i = ((sorted.len() as f64 * q) as usize).min(sorted.len() - 1);
    Some(sorted[i])
}

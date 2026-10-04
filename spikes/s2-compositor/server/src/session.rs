//! One WebRTC session: the compositor's encoded frames out as an RTP video
//! track with playout-delay 0, input back in on the `control` DataChannel.
//! The control protocol is S1d's (pings, per-frame send times, stats), so the
//! S1c/S1d page measures it, plus `{"t":"input",...}` messages from the page.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result, bail};
use serde::Serialize;
use str0m::change::{SdpAnswer, SdpOffer};
use str0m::channel::ChannelId;
use str0m::format::Codec as RtpCodec;
use str0m::media::{Frequency, MediaKind, MediaTime, Mid};
use str0m::net::{Protocol, Receive};
use str0m::rtp::{AbsCaptureTime, Extension, ExtensionMap};
use str0m::{Candidate, Event, IceConnectionState, Input, Output, Rtc};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::time::sleep_until;
use tracing::{info, warn};

use crate::input::BrowserInput;
use crate::pipeline::{Codec, EncodedFrame, Media};

const STATS_INTERVAL: Duration = Duration::from_millis(500);
const DRAIN: Duration = Duration::from_secs(1);
const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(10);

pub struct SessionParams {
    pub codec: Codec,
    pub secs: u32,
    pub host: IpAddr,
    pub port: u16,
    pub media: Arc<Media>,
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
    let (builder, rtp_codec) = match params.codec {
        Codec::H264 => (builder.enable_h264(true), RtpCodec::H264),
        Codec::Hevc => (builder.enable_h265(true), RtpCodec::H265),
    };
    let mut rtc = builder.build(Instant::now());
    rtc.add_local_candidate(Candidate::host(local, "udp")?)
        .context("adding host candidate")?;
    let answer = rtc.sdp_api().accept_offer(offer)?;

    info!(%local, codec = params.codec.name(), secs = params.secs, "webrtc session");
    tokio::spawn(async move {
        if let Err(err) = Session::new(rtc, socket, rtp_codec, params).run().await {
            warn!("webrtc session ended: {err:#}");
        }
    });
    Ok(answer)
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
    /// `c_us`/`e_us`: when it was composited and encoded, same clock.
    Sent {
        id: u32,
        rtp: u32,
        s_us: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        c_us: Option<u64>,
        e_us: u64,
    },
    /// Input carrying a `probe` id reached the streamer at `s_us`.
    Probe {
        id: u64,
        s_us: u64,
    },
    /// The output was resized to `w`×`h` at `s_us` (as asked, rounded to fit).
    Resized {
        w: u32,
        h: u32,
        s_us: u64,
    },
    Stats {
        elapsed_ms: u64,
        #[serde(flatten)]
        stats: StreamerStats,
    },
    Done {
        #[serde(flatten)]
        stats: StreamerStats,
    },
}

#[derive(Clone, Debug, Default, Serialize)]
struct StreamerStats {
    frames_generated: u64,
    frames_sent: u64,
    bytes_sent: u64,
    keyframes: u64,
    /// Browser PLI/FIR, each turned into a force-key-unit request.
    keyframe_requests: u64,
    /// From the session being ready to the first frame.
    first_frame_ms: Option<u64>,
    /// Compositor buffer out → encoded frame at the app sink.
    composite_to_encoded_ms_p50: Option<f64>,
    composite_to_encoded_ms_p99: Option<f64>,
    /// Encoded frame at the app sink → handed to str0m.
    encoded_to_sent_us_p50: Option<u64>,
    encoded_to_sent_us_p99: Option<u64>,
    frame_interval_ms_p50: Option<f64>,
    frame_interval_ms_p99: Option<f64>,
    inputs: u64,
    inputs_unmapped: u64,
    resizes: u64,
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
    frames: Option<mpsc::Receiver<EncodedFrame>>,
    subscribed_at: Option<Instant>,
    sending_until: Option<Instant>,
    finished_at: Option<Instant>,
    next_stats_at: Instant,
    frame_id: u32,
    stats: StreamerStats,
    encode_ms: Vec<f64>,
    hop_us: Vec<u64>,
    last_encoded: Option<Instant>,
    intervals_ms: Vec<f64>,
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
            frames: None,
            subscribed_at: None,
            sending_until: None,
            finished_at: None,
            next_stats_at: epoch + STATS_INTERVAL,
            frame_id: 0,
            stats: StreamerStats::default(),
            encode_ms: Vec::new(),
            hop_us: Vec::new(),
            last_encoded: None,
            intervals_ms: Vec::new(),
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
            if let Some(at) = self.subscribed_at
                && self.stats.frames_sent == 0
                && now >= at + FIRST_FRAME_TIMEOUT
            {
                bail!("no frame from the compositor {FIRST_FRAME_TIMEOUT:?} after subscribing");
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

            let Some(rtc_deadline) = self.drive()? else {
                return Ok(());
            };
            self.maybe_subscribe()?;
            let mut deadline = rtc_deadline.min(self.next_stats_at);
            if let Some(until) = self.sending_until.filter(|_| self.finished_at.is_none()) {
                deadline = deadline.min(until);
            }

            tokio::select! {
                received = self.socket.recv_from(&mut buf) => {
                    let (n, source) = received?;
                    self.receive(&buf[..n], source)?;
                }
                frame = next_frame(&mut self.frames) => match frame {
                    Some(frame) => self.send_frame(frame)?,
                    None => {
                        info!("frame source ended");
                        self.frames = None;
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

    /// Starts taking frames once ICE, the track and the control channel are up.
    fn maybe_subscribe(&mut self) -> Result<()> {
        if self.subscribed_at.is_some()
            || !self.connected
            || self.mid.is_none()
            || self.control.is_none()
        {
            return Ok(());
        }
        self.subscribed_at = Some(Instant::now());
        self.frames = Some(self.params.media.subscribe(self.params.codec)?);
        Ok(())
    }

    fn send_frame(&mut self, frame: EncodedFrame) -> Result<()> {
        let now = Instant::now();
        if self.finished_at.is_some() {
            return Ok(());
        }
        let Some(mid) = self.mid else { return Ok(()) };
        if self.stats.frames_generated == 0 {
            if !frame.key {
                // Wait for the keyframe the subscription asked for.
                return Ok(());
            }
            let first = self
                .subscribed_at
                .map(|t| now.duration_since(t).as_millis() as u64);
            self.stats.first_frame_ms = first;
            info!(first_frame_ms = ?first, bytes = frame.data.len(), "first frame to the browser");
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
        let rtp_time = MediaTime::new(
            (frame
                .encoded
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
        if let Some(composited) = frame.composited {
            self.encode_ms
                .push(frame.encoded.duration_since(composited).as_secs_f64() * 1000.0);
        }
        self.hop_us
            .push(now.duration_since(frame.encoded).as_micros() as u64);
        if let Some(last) = self.last_encoded {
            self.intervals_ms
                .push(frame.encoded.duration_since(last).as_secs_f64() * 1000.0);
        }
        self.last_encoded = Some(frame.encoded);
        let since_epoch = |t: Instant| t.saturating_duration_since(self.epoch).as_micros() as u64;
        let c_us = frame.composited.map(since_epoch);
        let e_us = since_epoch(frame.encoded);
        self.send_control(&ServerMsg::Sent {
            id,
            rtp: rtp_time.numer() as u32,
            s_us,
            c_us,
            e_us,
        });
        Ok(())
    }

    fn finish(&mut self, now: Instant) {
        self.finished_at = Some(now);
        self.frames = None;
        let stats = self.summary();
        info!(local = %self.local, ?stats, "stream complete");
        self.send_control(&ServerMsg::Done { stats });
    }

    fn summary(&self) -> StreamerStats {
        let mut stats = self.stats.clone();
        let mut encode = self.encode_ms.clone();
        encode.sort_by(f64::total_cmp);
        stats.composite_to_encoded_ms_p50 = percentile(&encode, 0.5);
        stats.composite_to_encoded_ms_p99 = percentile(&encode, 0.99);
        let mut hop = self.hop_us.clone();
        hop.sort_unstable();
        stats.encoded_to_sent_us_p50 = percentile(&hop, 0.5);
        stats.encoded_to_sent_us_p99 = percentile(&hop, 0.99);
        let mut intervals = self.intervals_ms.clone();
        intervals.sort_by(f64::total_cmp);
        stats.frame_interval_ms_p50 = percentile(&intervals, 0.5);
        stats.frame_interval_ms_p99 = percentile(&intervals, 0.99);
        stats
    }

    /// Polls output until str0m wants to wait; `None` ends the session.
    fn drive(&mut self) -> Result<Option<Instant>> {
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
                self.params.media.request_keyframe(self.params.codec);
            }
            Event::ChannelOpen(id, label) if label == "control" => {
                self.control = Some(id);
                let media = &self.params.media;
                let stream = serde_json::json!({
                    "codec": self.params.codec.name(),
                    "width": media.size().0,
                    "height": media.size().1,
                    "input": true,
                });
                self.send_control(&ServerMsg::Hello { stream });
            }
            Event::ChannelData(data) if Some(data.id) == self.control => {
                for line in String::from_utf8_lossy(&data.data).lines() {
                    self.handle_client_line(line);
                }
            }
            _ => {}
        }
        true
    }

    fn handle_client_line(&mut self, line: &str) {
        let Ok(msg) = serde_json::from_str::<serde_json::Value>(line) else {
            return;
        };
        match msg.get("t").and_then(|t| t.as_str()) {
            Some("ping") => {
                if let Some(c) = msg.get("c").and_then(|c| c.as_f64()) {
                    let s_us = self.epoch.elapsed().as_micros() as u64;
                    self.send_control(&ServerMsg::Pong { c, s_us });
                }
            }
            Some("resize") => {
                let dim = |k: &str| msg.get(k).and_then(|v| v.as_u64()).map(|v| v as u32);
                if let (Some(w), Some(h)) = (dim("w"), dim("h")) {
                    let (w, h) = self.params.media.resize(w, h);
                    self.stats.resizes += 1;
                    let s_us = self.epoch.elapsed().as_micros() as u64;
                    self.send_control(&ServerMsg::Resized { w, h, s_us });
                }
            }
            Some("input") => {
                let received_us = self.epoch.elapsed().as_micros() as u64;
                self.stats.inputs += 1;
                let probe = msg.get("probe").and_then(|p| p.as_u64());
                let media = &self.params.media;
                match serde_json::from_value::<BrowserInput>(msg)
                    .ok()
                    .and_then(|i| {
                        let (w, h) = media.size();
                        i.to_compositor(w, h)
                    }) {
                    Some(input) => media.input(input),
                    None => self.stats.inputs_unmapped += 1,
                }
                // The page's latency probe: echo when its click arrived.
                if let Some(id) = probe {
                    self.send_control(&ServerMsg::Probe {
                        id,
                        s_us: received_us,
                    });
                }
            }
            _ => {}
        }
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

/// The next encoded frame, or never while not subscribed.
async fn next_frame(frames: &mut Option<mpsc::Receiver<EncodedFrame>>) -> Option<EncodedFrame> {
    match frames {
        Some(rx) => rx.recv().await,
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

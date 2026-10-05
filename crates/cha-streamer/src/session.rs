//! One WebRTC session: the compositor's encoded frames out as an RTP video
//! track with playout-delay 0, the mixer's Opus frames as an audio track (its
//! own MediaStream, so the browser doesn't hold video back for lip sync), input
//! back in on the `control` DataChannel.
//! The control protocol is S1d's (pings, per-frame send times, stats), so the
//! S1c/S1d page measures it, plus `{"t":"input",...}` messages from the page.
//! Carried over from the S2 spike's streamer.

use std::collections::HashSet;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result, bail};
use cha_nvenc::Codec;
use str0m::change::{SdpAnswer, SdpOffer};
use str0m::channel::ChannelId;
use str0m::format::Codec as RtpCodec;
use str0m::media::{Frequency, MediaKind, MediaTime, Mid};
use str0m::net::{Protocol, Receive};
use str0m::rtp::{AbsCaptureTime, Extension, ExtensionMap};
use str0m::{Candidate, Event, IceConnectionState, Input, Output, Rtc};
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::sleep_until;
use tracing::{info, warn};

use crate::audio::{Audio, AudioPacket};
use crate::codec::VideoCodec;
use crate::compositor::{ClipboardWatch, CursorWatch, PointerWatch};
use crate::control::{
    Control, RumbleFeed, ServerMsg, StreamerStats, cursor_msg, floor_msg, next_clipboard,
    next_cursor, next_pointer, next_status, percentile,
};
use crate::gamepad::Gamepads;
use crate::media::{EncodedFrame, Media, Pace};
use crate::rate::{MIN_BPS, RateControl, Report, Sample, VIDEO_SHARE, Verdict, parse_report};
use crate::status::StatusWatch;
use crate::viewers::Seat;

const STATS_INTERVAL: Duration = Duration::from_millis(500);
const DRAIN: Duration = Duration::from_secs(1);
const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(10);

pub struct SessionParams {
    pub codec: Codec,
    /// Stop sending after this long (benchmarks); 0 runs until the browser
    /// leaves or another connection takes over.
    pub secs: u32,
    /// Where the browser may reach us: a host candidate on `port` for each.
    pub hosts: Vec<IpAddr>,
    /// Port-forwarded public addresses, announced on the first socket's port.
    pub public: Vec<IpAddr>,
    pub port: u16,
    pub media: Arc<Media>,
    /// Sound, if the browser asks for it (an audio m-line in its offer).
    pub audio: Option<Arc<Audio>>,
    pub gamepads: Option<Arc<Gamepads>>,
}

/// A running session; dropping `stop` (or sending on it) ends it.
pub struct Running {
    pub stop: oneshot::Sender<()>,
    pub handle: JoinHandle<()>,
}

/// Starts a session for `seat`'s viewer; it leaves when the session ends.
pub async fn start(
    params: SessionParams,
    seat: Seat,
    offer: SdpOffer,
) -> Result<(SdpAnswer, Running)> {
    let mut sockets = Vec::new();
    for host in &params.hosts {
        let socket = match UdpSocket::bind(SocketAddr::new(*host, params.port)).await {
            Ok(socket) => socket,
            Err(_) => match UdpSocket::bind(SocketAddr::new(*host, 0)).await {
                Ok(socket) => socket,
                Err(err) => {
                    warn!(%host, "no UDP socket: {err}");
                    continue;
                }
            },
        };
        sockets.push(socket);
    }
    if sockets.is_empty() {
        bail!("no address to receive WebRTC on");
    }
    let locals = sockets
        .iter()
        .map(UdpSocket::local_addr)
        .collect::<std::io::Result<Vec<_>>>()?;

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
        Codec::Av1 => (builder.enable_av1(true), RtpCodec::Av1),
    };
    let builder = builder.enable_opus(params.audio.is_some(), false);
    let mut rtc = builder.build(Instant::now());
    for local in &locals {
        rtc.add_local_candidate(Candidate::host(*local, "udp")?)
            .context("adding host candidate")?;
    }
    // A port-forward's public address: announced as a host candidate on the
    // first socket's port (ICE-lite takes host candidates only). The
    // browser's checks arrive, NAT-translated, on that socket.
    for ip in &params.public {
        let announced = SocketAddr::new(*ip, locals[0].port());
        rtc.add_local_candidate(Candidate::host(announced, "udp")?)
            .context("adding the public address")?;
    }
    let answer = rtc.sdp_api().accept_offer(offer)?;

    info!(
        ?locals,
        codec = params.codec.name(),
        secs = params.secs,
        "webrtc session"
    );
    let (stop, stopped) = oneshot::channel();
    let handle = tokio::spawn(async move {
        if let Err(err) = Session::new(rtc, sockets, locals, rtp_codec, params, seat, stopped)
            .run()
            .await
        {
            warn!("webrtc session ended: {err:#}");
        }
    });
    Ok((answer, Running { stop, handle }))
}

struct Session {
    rtc: Rtc,
    /// One per host candidate, all on the same port where possible.
    sockets: Vec<UdpSocket>,
    locals: Vec<SocketAddr>,
    epoch: Instant,
    rtp_codec: RtpCodec,
    params: SessionParams,
    mid: Option<Mid>,
    audio_mid: Option<Mid>,
    control: Option<ChannelId>,
    /// What the page's control messages do (shared with WebTransport).
    handler: Control,
    connected: bool,
    frames: Option<mpsc::Receiver<EncodedFrame>>,
    /// Rate control (P2.5) from the page's reports, as over WebTransport,
    /// and the encoder's rate it sets; bytes sent at the last report.
    rate: RateControl,
    pace: Option<Arc<Pace>>,
    rate_mark: Option<(Instant, u64)>,
    rate_logged: Instant,
    audio: Option<mpsc::Receiver<AudioPacket>>,
    /// What apps copy, for the page.
    clipboard: Option<ClipboardWatch>,
    /// The cursor's shape, once the control channel is open, and the image
    /// ids sent so far.
    cursor: Option<CursorWatch>,
    cursor_ids: HashSet<u64>,
    /// Where the pointer is, for this page when it only watches.
    pointer: Option<PointerWatch>,
    /// What the app's setup is doing, once the control channel is open.
    status: Option<StatusWatch>,
    rumble: RumbleFeed,
    subscribed_at: Option<Instant>,
    sending_until: Option<Instant>,
    finished_at: Option<Instant>,
    stopped: oneshot::Receiver<()>,
    next_stats_at: Instant,
    frame_id: u32,
    stats: StreamerStats,
    encode_ms: Vec<f64>,
    hop_us: Vec<u64>,
    last_encoded: Option<Instant>,
    intervals_ms: Vec<f64>,
}

impl Session {
    fn new(
        rtc: Rtc,
        sockets: Vec<UdpSocket>,
        locals: Vec<SocketAddr>,
        rtp_codec: RtpCodec,
        params: SessionParams,
        seat: Seat,
        stopped: oneshot::Receiver<()>,
    ) -> Self {
        let epoch = Instant::now();
        let clipboard = Some(params.media.clipboard());
        let rumble = RumbleFeed::new(params.gamepads.as_deref());
        let rate = RateControl::new(MIN_BPS, params.media.bitrate_bps());
        let handler = Control {
            epoch,
            codec: VideoCodec::Hw(params.codec),
            media: Arc::clone(&params.media),
            gamepads: params.gamepads.clone(),
            seat,
        };
        Self {
            rtc,
            sockets,
            locals,
            epoch,
            rtp_codec,
            params,
            mid: None,
            audio_mid: None,
            control: None,
            handler,
            connected: false,
            frames: None,
            rate,
            pace: None,
            rate_mark: None,
            rate_logged: epoch,
            audio: None,
            clipboard,
            cursor: None,
            cursor_ids: HashSet::new(),
            pointer: None,
            status: None,
            rumble,
            subscribed_at: None,
            sending_until: None,
            finished_at: None,
            stopped,
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
                received = recv_any(&self.sockets, &mut buf) => {
                    let (n, source, socket) = received?;
                    self.receive(&buf[..n], source, self.locals[socket])?;
                }
                packet = next_packet(&mut self.audio) => match packet {
                    Some(packet) => self.send_audio(packet)?,
                    None => self.audio = None,
                },
                // The apps' clipboard is the controller's alone.
                text = next_clipboard(&mut self.clipboard) => {
                    if let Some(text) = text && self.handler.seat.has_control() {
                        self.send_control(&ServerMsg::Clipboard { text: text.to_string() });
                    }
                }
                spot = next_pointer(&mut self.pointer) => {
                    if let Some(spot) = spot && !self.handler.seat.has_control() {
                        self.send_control(&ServerMsg::Pointer { x: spot.x, y: spot.y, drawn: spot.drawn });
                    }
                }
                () = self.handler.seat.changed() => {
                    let msg = floor_msg(&self.handler.seat);
                    self.send_control(&msg);
                }
                // The apps' rumble is the controller's alone.
                msg = self.rumble.next() => {
                    if self.handler.seat.has_control() {
                        self.send_control(&msg);
                    }
                }
                // Every viewer sees what the app is setting up, not only the controller.
                msg = next_status(&mut self.status) => {
                    if let Some(msg) = msg {
                        self.send_control(&msg);
                    }
                }
                shape = next_cursor(&mut self.cursor) => {
                    if let Some(shape) = shape {
                        let msg = cursor_msg(&shape, &mut self.cursor_ids);
                        self.send_control(&msg);
                    }
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
                _ = &mut self.stopped => {
                    // Taken over by a newer connection: free the port now.
                    info!(locals = ?self.locals, "session replaced");
                    self.rtc.disconnect();
                    return Ok(());
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
        let subscription = self
            .params
            .media
            .subscribe(VideoCodec::Hw(self.params.codec))?;
        self.frames = Some(subscription.frames);
        self.pace = Some(subscription.pace);
        if let (Some(audio), Some(_)) = (&self.params.audio, self.audio_mid) {
            self.audio = Some(audio.subscribe());
        }
        Ok(())
    }

    fn send_audio(&mut self, packet: AudioPacket) -> Result<()> {
        if self.finished_at.is_some() {
            return Ok(());
        }
        let Some(mid) = self.audio_mid else {
            return Ok(());
        };
        let Some(writer) = self.rtc.writer(mid) else {
            bail!("audio media {mid:?} disappeared");
        };
        let Some(pt) = writer
            .payload_params()
            .find(|p| p.spec().codec == RtpCodec::Opus)
            .map(|p| p.pt())
        else {
            // The browser offered audio without Opus: video only.
            self.audio = None;
            return Ok(());
        };
        let rtp_time = MediaTime::new(packet.samples, Frequency::FORTY_EIGHT_KHZ);
        writer.write(pt, packet.at, rtp_time, &packet.data[..])?;
        self.stats.audio_packets += 1;
        self.stats.audio_bytes += packet.data.len() as u64;
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
            if self.params.secs > 0 {
                self.sending_until = Some(now + Duration::from_secs(self.params.secs.into()));
            }
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
        self.encode_ms
            .push(frame.encoded.duration_since(frame.composited).as_secs_f64() * 1000.0);
        self.hop_us
            .push(now.duration_since(frame.encoded).as_micros() as u64);
        if let Some(last) = self.last_encoded {
            self.intervals_ms
                .push(frame.encoded.duration_since(last).as_secs_f64() * 1000.0);
        }
        self.last_encoded = Some(frame.encoded);
        let since_epoch = |t: Instant| t.saturating_duration_since(self.epoch).as_micros() as u64;
        let c_us = Some(since_epoch(frame.composited));
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
        self.audio = None;
        let stats = self.summary();
        info!(locals = ?self.locals, ?stats, "stream complete");
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
                    // From the candidate str0m chose.
                    let socket = self.locals.iter().position(|l| *l == t.source).unwrap_or(0);
                    if let Err(err) = self.sockets[socket].try_send_to(&t.contents, t.destination)
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
            Event::MediaAdded(m) if m.kind == MediaKind::Audio => self.audio_mid = Some(m.mid),
            Event::KeyframeRequest(_) => {
                self.stats.keyframe_requests += 1;
                self.params
                    .media
                    .request_keyframe(VideoCodec::Hw(self.params.codec));
            }
            Event::ChannelOpen(id, label) if label == "control" => {
                self.control = Some(id);
                self.cursor = Some(self.params.media.cursor());
                self.pointer = Some(self.params.media.pointer());
                self.status = Some(self.params.media.status());
                let media = &self.params.media;
                let stream = serde_json::json!({
                    "codec": self.params.codec.name(),
                    "width": media.size().0,
                    "height": media.size().1,
                    "input": true,
                    "audio": self.audio_mid.is_some() && self.params.audio.is_some(),
                    "gamepads": self.params.gamepads.is_some(),
                });
                self.send_control(&ServerMsg::Hello { stream });
                let floor = floor_msg(&self.handler.seat);
                self.send_control(&floor);
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
        if let Some(report) = parse_report(line) {
            self.on_report(report);
            return;
        }
        let mut replies = Vec::new();
        self.handler
            .handle_line(line, &mut self.stats, &mut |m| replies.push(m));
        for reply in replies {
            self.send_control(&reply);
        }
    }

    /// A report from the page: one rate-control update. Only one with a
    /// delay counts: a page that shows no frames (hidden) reports none, and
    /// the rate holds until it does.
    fn on_report(&mut self, report: Report) {
        let now = Instant::now();
        let sent = self.stats.bytes_sent;
        let mark = self.rate_mark.replace((now, sent));
        let (Some(delivery_ms), Some(rx_bps), Some((then, before))) =
            (report.delivery_ms, report.rx_bps, mark)
        else {
            return;
        };
        let secs = now.duration_since(then).as_secs_f64().max(1e-3);
        let sample = Sample {
            rtt: Duration::ZERO,
            delivery_ms: Some(delivery_ms),
            rx_bps: Some(rx_bps),
            backlog_ms: 0.0,
            loss: report.loss.unwrap_or(0.0),
            tx_bps: sent.saturating_sub(before) as f64 * 8.0 / secs,
        };
        let verdict = self.rate.update(now, &sample);
        let target = f64::from(self.rate.target()) * VIDEO_SHARE;
        if let Some(pace) = &self.pace {
            pace.set_target(target as u32);
        }
        self.stats.target_mbps = Some(target / 1e6);
        self.stats.queue_ms = Some(self.rate.queue_ms());
        if verdict == Verdict::Overuse
            || now.duration_since(self.rate_logged) >= Duration::from_secs(5)
        {
            self.rate_logged = now;
            info!(
                ?verdict,
                target_mbps = format!("{:.1}", f64::from(self.rate.target()) / 1e6),
                tx_mbps = format!("{:.1}", sample.tx_bps / 1e6),
                rx_mbps = format!("{:.1}", rx_bps / 1e6),
                queue_ms = format!("{:.0}", self.rate.queue_ms()),
                loss = format!("{:.3}", sample.loss),
                "webrtc rate"
            );
        }
    }

    fn receive(
        &mut self,
        contents: &[u8],
        source: SocketAddr,
        destination: SocketAddr,
    ) -> Result<()> {
        let input = Input::Receive(
            Instant::now(),
            Receive {
                proto: Protocol::Udp,
                source,
                destination,
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

/// The next datagram on any of `sockets`: its length, sender and socket.
async fn recv_any(
    sockets: &[UdpSocket],
    buf: &mut [u8],
) -> std::io::Result<(usize, SocketAddr, usize)> {
    std::future::poll_fn(|cx| {
        for (i, socket) in sockets.iter().enumerate() {
            let mut read = tokio::io::ReadBuf::new(buf);
            if let std::task::Poll::Ready(result) = socket.poll_recv_from(cx, &mut read) {
                return std::task::Poll::Ready(result.map(|from| (read.filled().len(), from, i)));
            }
        }
        std::task::Poll::Pending
    })
    .await
}

/// The next Opus frame, or never while not subscribed.
async fn next_packet(audio: &mut Option<mpsc::Receiver<AudioPacket>>) -> Option<AudioPacket> {
    match audio {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

/// The next encoded frame, or never while not subscribed.
async fn next_frame(frames: &mut Option<mpsc::Receiver<EncodedFrame>>) -> Option<EncodedFrame> {
    match frames {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

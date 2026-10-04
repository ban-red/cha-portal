//! The WebTransport path (plan §3.1, `cha-stream/1`), Chromium's fast path:
//! video and audio as datagrams with `cha-proto` framing, the control channel
//! on the session's first bidirectional stream with the same JSON lines as
//! WebRTC's DataChannel.
//!
//! §3.1's rules, as far as P2.3 goes: no silent eviction (a frame that doesn't
//! fit QUIC's send buffer isn't sent), nothing that depends on a frame that
//! wasn't sent (wait for the keyframe asked for instead), and QUIC paces.
//! The browser asks for a keyframe when it loses one.
//!
//! The page can switch codecs within the session (`{"t":"codec"}`): the
//! session subscribes to the other encoder, keeps sending the current stream
//! until that encoder's first frame, then sends its frames as the next
//! `stream` number, which the page starts afresh.
//!
//! The certificate is self-signed for 13 days (browsers accept such a one by
//! its hash, `serverCertificateHashes`); the portal hands the hash out.

use std::collections::{HashSet, VecDeque};
use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use cha_proto::{DatagramHeader, Flags, Fragmenter, HEADER_LEN, Kind, fec};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::{mpsc, oneshot};
use tokio::time::{MissedTickBehavior, interval_at};
use tracing::{debug, info, warn};
use wtransport::endpoint::IncomingSession;
use wtransport::endpoint::endpoint_side::Server;
use wtransport::quinn::TransportConfig;
use wtransport::{Connection, Endpoint, Identity, ServerConfig, VarInt};

use crate::audio::{Audio, AudioPacket};
use crate::codec::VideoCodec;
use crate::congestion::MediaWindowFactory;
use crate::control::{
    Control, ServerMsg, StreamerStats, cursor_msg, floor_msg, next_clipboard, next_cursor,
    next_pointer, percentile,
};
use crate::gamepad::Gamepads;
use crate::media::{EncodedFrame, Media, Pace, Subscription};
use crate::rate::{MIN_BPS, RateControl, Reports, Sample, VIDEO_SHARE, Verdict, parse_report};
use crate::session::Running;
use crate::viewers::{Seat, Viewer, Viewers};

const STATS_INTERVAL: Duration = Duration::from_millis(500);
/// QUIC's datagram send buffer. Our own backlog rules (below) keep it nearly
/// empty; the size only has to take a PyroWave frame or two.
const SEND_BUFFER: usize = 8 << 20;
/// How often the send queue is checked (hold or not) and the rate updated.
const PACE_INTERVAL: Duration = Duration::from_millis(50);
/// Hold the encoder while more than this many frames wait to go out.
const HOLD_FRAMES: f64 = 1.5;
/// A dropped frame's keyframe, asked for at most this often.
const KEYFRAME_RETRY: Duration = Duration::from_millis(300);
/// The loss a keyframe's parity is sized for, at least.
const KEYFRAME_LOSS: f64 = 0.003;
/// What wtransport adds to each datagram (the session id), rounded up.
const DATAGRAM_OVERHEAD: usize = 8;

/// The endpoint and the certificate's SHA-256, as hex.
pub fn bind(port: u16, names: &[IpAddr]) -> Result<(Endpoint<Server>, String)> {
    // serverCertificateHashes takes ECDSA P-256, valid at most 14 days.
    let mut sans: Vec<String> = names.iter().map(ToString::to_string).collect();
    sans.push("localhost".into());
    let identity = Identity::self_signed_builder()
        .subject_alt_names(&sans)
        .from_now_utc()
        .validity_days(13)
        .build()
        .context("generating the WebTransport certificate")?;
    let hash = identity.certificate_chain().as_slice()[0]
        .hash()
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let mut transport = TransportConfig::default();
    transport
        // A few frames' worth: more would only hide a stalled path.
        .datagram_send_buffer_size(SEND_BUFFER)
        .datagram_receive_buffer_size(Some(1 << 20))
        .max_idle_timeout(Some(Duration::from_secs(10).try_into()?))
        .keep_alive_interval(Some(Duration::from_secs(2)))
        // Our rate control sets the pace, not QUIC's window (plan §3.1 rule 9).
        .congestion_controller_factory(Arc::new(MediaWindowFactory));
    let config = ServerConfig::builder()
        .with_bind_default(port)
        .with_custom_transport(identity, transport)
        .build();
    let endpoint = Endpoint::server(config).context("binding the WebTransport endpoint")?;
    Ok((endpoint, hash))
}

/// Checks a presented token; the error says why not.
pub type Authorize = Box<dyn Fn(&str) -> std::result::Result<Viewer, String> + Send + Sync>;

/// What a session needs from the streamer.
pub struct Sessions {
    pub media: Arc<Media>,
    pub audio: Option<Arc<Audio>>,
    pub gamepads: Option<Arc<Gamepads>>,
    pub authorize: Authorize,
    pub viewers: Arc<Viewers>,
}

pub async fn serve(endpoint: Endpoint<Server>, sessions: Arc<Sessions>) {
    loop {
        let incoming = endpoint.accept().await;
        let sessions = Arc::clone(&sessions);
        tokio::spawn(async move {
            if let Err(err) = accept(incoming, sessions).await {
                warn!("webtransport: {err:#}");
            }
        });
    }
}

async fn accept(incoming: IncomingSession, sessions: Arc<Sessions>) -> Result<()> {
    let request = incoming.await?;
    let path = request.path().to_owned();
    let query = path.split_once('?').map_or("", |(_, q)| q);
    let value = |key: &str| {
        query
            .split('&')
            .filter_map(|kv| kv.split_once('='))
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v.to_string())
    };
    let viewer = match (sessions.authorize)(&value("token").unwrap_or_default()) {
        Ok(viewer) => viewer,
        Err(why) => {
            warn!("rejected a WebTransport session: {why}");
            request.forbidden().await;
            return Ok(());
        }
    };
    let Some(codec) = value("codec")
        .as_deref()
        .and_then(VideoCodec::from_name)
        .filter(|c| sessions.media.codecs().contains(c))
    else {
        request.not_found().await;
        bail!("no ?codec=, or one this streamer doesn't offer");
    };
    // WebTransport sessions coexist (one QUIC endpoint serves them all).
    let seat = match sessions.viewers.join(viewer, false).await {
        Ok(seat) => seat,
        Err(why) => {
            request.too_many_requests().await;
            bail!(why);
        }
    };
    let id = seat.id;
    let remote = request.remote_address();
    let conn = request.accept().await?;
    info!(%remote, codec = codec.name(), id, "webtransport session");
    let (stop, stopped) = oneshot::channel();
    let session = Arc::clone(&sessions);
    let handle = tokio::spawn(async move {
        if let Err(err) = run(conn, codec, seat, session, stopped).await {
            info!(%remote, "webtransport session ended: {err:#}");
        }
    });
    sessions.viewers.attach(id, Running { stop, handle });
    Ok(())
}

async fn run(
    conn: Connection,
    codec: VideoCodec,
    seat: Seat,
    sessions: Arc<Sessions>,
    mut stopped: oneshot::Receiver<()>,
) -> Result<()> {
    let epoch = Instant::now();
    let (mut ctl_send, ctl_recv) = tokio::select! {
        bi = conn.accept_bi() => bi.context("waiting for the page's control stream")?,
        _ = &mut stopped => return Ok(()),
    };
    let max_datagram = conn
        .max_datagram_size()
        .context("the browser doesn't take datagrams")?;
    let fragmenter = Fragmenter::new(max_datagram);

    // The control stream: lines in through a task, replies out through one.
    let (lines_tx, mut lines) = mpsc::channel::<String>(256);
    let reader = tokio::spawn(async move {
        let mut r = BufReader::new(ctl_recv).lines();
        while let Ok(Some(line)) = r.next_line().await {
            if lines_tx.send(line).await.is_err() {
                break;
            }
        }
    });
    let (out, mut outbox) = mpsc::unbounded_channel::<ServerMsg>();
    let writer = tokio::spawn(async move {
        while let Some(msg) = outbox.recv().await {
            let mut line = serde_json::to_vec(&msg).expect("control messages serialize");
            line.push(b'\n');
            if ctl_send.write_all(&line).await.is_err() {
                break;
            }
        }
    });

    let mut handler = Control {
        epoch,
        codec,
        media: Arc::clone(&sessions.media),
        gamepads: sessions.gamepads.clone(),
        seat,
    };
    let (w, h) = sessions.media.size();
    let _ = out.send(ServerMsg::Hello {
        stream: serde_json::json!({
            "codec": codec.name(),
            "width": w,
            "height": h,
            "input": true,
            "audio": sessions.audio.is_some(),
            "gamepads": sessions.gamepads.is_some(),
            "transport": "webtransport",
            "maxDatagram": max_datagram,
        }),
    });

    let subscribed = Instant::now();
    let mut video = Video::new(codec, sessions.media.subscribe(codec)?, fragmenter);
    let mut pacing = Pacing::new(video.pace.target());
    let mut pace_tick = interval_at(tokio::time::Instant::now() + PACE_INTERVAL, PACE_INTERVAL);
    pace_tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut audio = sessions.audio.as_ref().map(|a| a.subscribe());
    let mut clipboard = Some(sessions.media.clipboard());
    let mut cursor = Some(sessions.media.cursor());
    let mut cursor_ids = HashSet::new();
    let mut pointer = Some(sessions.media.pointer());
    let _ = out.send(floor_msg(&handler.seat));
    let mut stats = StreamerStats::default();
    let mut report = interval_at(tokio::time::Instant::now() + STATS_INTERVAL, STATS_INTERVAL);
    report.set_missed_tick_behavior(MissedTickBehavior::Skip);

    let result = loop {
        tokio::select! {
            frame = video.frames.recv() => {
                let Some(frame) = frame else {
                    let reason = format!("the {} encoder stopped", video.codec.name());
                    conn.close(VarInt::from_u32(2), reason.as_bytes());
                    break Err(anyhow::anyhow!(reason));
                };
                if video.send(&conn, &frame, &sessions.media, epoch, &mut stats) {
                    stats.first_frame_ms.get_or_insert(subscribed.elapsed().as_millis() as u64);
                }
            }
            // A switch: the other encoder's first frame, so take over from here.
            frame = next_frame(&mut video.next) => {
                let (next, frames) = video.next.take().expect("polled only while switching");
                let Some(frame) = frame else {
                    let _ = out.send(ServerMsg::Codec {
                        codec: video.codec.name(),
                        stream: video.stream,
                        error: Some(format!("the {} encoder stopped", next.name())),
                    });
                    continue;
                };
                video.take_over(next, frames, pacing.encoder_target());
                handler.codec = next;
                info!(codec = next.name(), stream = video.stream, "switched codec");
                let _ = out.send(ServerMsg::Codec { codec: next.name(), stream: video.stream, error: None });
                video.send(&conn, &frame, &sessions.media, epoch, &mut stats);
            }
            packet = next_packet(&mut audio) => match packet {
                Some(packet) => send_audio(&conn, &packet, epoch, &mut stats),
                None => audio = None,
            },
            // The apps' clipboard is the controller's alone.
            text = next_clipboard(&mut clipboard) => {
                if let Some(text) = text && handler.seat.has_control() {
                    let _ = out.send(ServerMsg::Clipboard { text: text.to_string() });
                }
            }
            shape = next_cursor(&mut cursor) => {
                if let Some(shape) = shape {
                    let _ = out.send(cursor_msg(&shape, &mut cursor_ids));
                }
            }
            spot = next_pointer(&mut pointer) => {
                if let Some(spot) = spot && !handler.seat.has_control() {
                    let _ = out.send(ServerMsg::Pointer { x: spot.x, y: spot.y, drawn: spot.drawn });
                }
            }
            () = handler.seat.changed() => {
                let _ = out.send(floor_msg(&handler.seat));
            }
            line = lines.recv() => {
                let Some(line) = line else { break Ok(()) };
                if let Some(report) = parse_report(&line) {
                    pacing.reports.record(report, Instant::now());
                    continue;
                }
                if let Some(id) = rfi_request(&line) {
                    video.page_lost(&sessions.media, id);
                    continue;
                }
                let Some(name) = codec_request(&line) else {
                    handler.handle_line(&line, &mut stats, &mut |m| { let _ = out.send(m); });
                    continue;
                };
                // The current stream goes on until the other encoder's first
                // frame (a cold one takes ~0.2 s to start), answered then.
                let error = match VideoCodec::from_name(&name)
                    .filter(|c| sessions.media.codecs().contains(c))
                {
                    None => Some(format!("{name} isn't offered here")),
                    Some(next) if next == video.codec => {
                        video.next = None;
                        None
                    }
                    Some(next) => match sessions.media.subscribe(next) {
                        Ok(subscription) => {
                            video.next = Some((next, subscription));
                            continue;
                        }
                        Err(err) => Some(format!("{err:#}")),
                    },
                };
                let _ = out.send(ServerMsg::Codec { codec: video.codec.name(), stream: video.stream, error });
            }
            _ = pace_tick.tick() => {
                pacing.tick(&conn, &mut video, &sessions.media, &mut stats);
            }
            _ = report.tick() => {
                video.report(&mut stats);
                let _ = out.send(ServerMsg::Stats {
                    elapsed_ms: epoch.elapsed().as_millis() as u64,
                    stats: stats.clone(),
                });
            }
            err = conn.closed() => break Err(anyhow::anyhow!("{err}")),
            _ = &mut stopped => {
                info!("webtransport session replaced");
                conn.close(VarInt::from_u32(1), b"replaced");
                break Ok(());
            }
        }
    };
    reader.abort();
    writer.abort();
    result
}

/// A session's video: the encoder it takes frames from, and the one it's
/// switching to.
struct Video {
    codec: VideoCodec,
    frames: mpsc::Receiver<EncodedFrame>,
    /// How this session paces its encoder (rate, and hold while backed up).
    pace: Arc<Pace>,
    /// Switching: the other encoder's frames, taken over at the first.
    next: Option<(VideoCodec, Subscription)>,
    /// Bumped on each switch and carried in every video datagram, so the
    /// page knows which decoder a frame is for and starts that stream afresh.
    stream: u8,
    frame_id: u32,
    /// Until a keyframe goes out, no frame may (the first, or after a
    /// drop), or a recovery frame referring around the frames dropped.
    resync: bool,
    /// The encoder index of the first frame the page doesn't have (dropped
    /// here, or reported lost by the page): a recovery frame referring only
    /// to frames before it resyncs the page.
    lost: Option<u64>,
    /// Frames sent lately: their ids and encoder indices.
    sent: VecDeque<(u32, u64)>,
    fragmenter: Fragmenter,
    /// Composited → encoded (ms) and encoded → sent (µs) since the last report.
    encode_ms: Vec<f64>,
    hop_us: Vec<u64>,
    /// The loss rate parity is sized for (0: no FEC); the pacing sets it.
    fec_loss: f64,
    /// Recent frame size (bytes, smoothed), to count the send queue in frames.
    frame_bytes: f64,
    keyframe_asked: Option<Instant>,
}

impl Video {
    fn new(codec: VideoCodec, subscription: Subscription, fragmenter: Fragmenter) -> Self {
        Self {
            codec,
            frames: subscription.frames,
            pace: subscription.pace,
            next: None,
            stream: 0,
            fec_loss: 0.0,
            frame_id: 0,
            resync: true,
            lost: None,
            sent: VecDeque::new(),
            fragmenter,
            encode_ms: Vec::new(),
            hop_us: Vec::new(),
            frame_bytes: 0.0,
            keyframe_asked: None,
        }
    }

    /// Asks the encoder to refer around the frames the page doesn't have
    /// (reference invalidation; it sends a keyframe where it can't), at most
    /// every `KEYFRAME_RETRY`.
    fn ask_resync(&mut self, media: &Media) {
        if self
            .keyframe_asked
            .is_none_or(|at| at.elapsed() >= KEYFRAME_RETRY)
        {
            self.keyframe_asked = Some(Instant::now());
            match self.lost {
                Some(from) => media.request_invalidate(self.codec, from),
                None => media.request_keyframe(self.codec),
            }
        }
    }

    /// The page lost frame `id`, and drops what follows until a recovery
    /// frame or a keyframe.
    fn page_lost(&mut self, media: &Media, id: u32) {
        match self.sent.iter().find(|(sent, _)| *sent == id) {
            Some(&(_, index)) => {
                self.lost = Some(self.lost.map_or(index, |lost| lost.min(index)));
                self.keyframe_asked = None;
                self.ask_resync(media);
            }
            // Too long ago to name.
            None => media.request_keyframe(self.codec),
        }
    }

    /// Sends a frame, or drops it (and, for the hardware codecs, all until a
    /// keyframe or a frame referring around them, which is asked for). True
    /// if sent.
    fn send(
        &mut self,
        conn: &Connection,
        frame: &EncodedFrame,
        media: &Media,
        epoch: Instant,
        stats: &mut StreamerStats,
    ) -> bool {
        stats.frames_generated += 1;
        // PyroWave: packets that each decode on their own, and every frame
        // stands alone.
        let intra = !frame.packets.is_empty();
        // Refers only to frames the page has: it resyncs the page.
        let recovers = matches!(
            (frame.recovery, self.lost),
            (Some(from), Some(lost)) if from <= lost
        );
        if self.resync && !frame.key && !recovers && !intra {
            return false;
        }
        let sent = if intra {
            send_packets(conn, frame, self.frame_id, self.stream, epoch, stats)
        } else {
            self.send_frame(conn, frame, recovers, epoch, stats)
        };
        if !sent {
            stats.frames_dropped += 1;
            if !intra {
                // Its dependents would be garbage: start over from a frame
                // referring around it (asked for now, and again once the
                // queue drains).
                self.lost = Some(self.lost.map_or(frame.index, |lost| lost.min(frame.index)));
                self.resync = true;
                self.ask_resync(media);
            }
            return false;
        }
        if !intra {
            self.sent.push_back((self.frame_id, frame.index));
            if self.sent.len() > 256 {
                self.sent.pop_front();
            }
        }
        if frame.key || recovers {
            self.lost = None;
        }
        let size = frame.data.len() as f64;
        self.frame_bytes = if self.frame_bytes == 0.0 {
            size
        } else {
            self.frame_bytes * 0.9 + size * 0.1
        };
        self.encode_ms
            .push(frame.encoded.duration_since(frame.composited).as_secs_f64() * 1e3);
        self.hop_us.push(frame.encoded.elapsed().as_micros() as u64);
        self.frame_id = self.frame_id.wrapping_add(1);
        stats.frames_sent += 1;
        stats.keyframes += u64::from(frame.key);
        self.resync = false;
        true
    }

    /// Parity fragments per block for a frame of `len` bytes: enough that,
    /// at the loss expected, a frame is lost under once in 10⁴, a resync
    /// point (a keyframe, or a recovery frame) once in 10⁶, but never more
    /// than half its data. Resync points always have some: everything after
    /// one needs it, and one is mostly sent because something was lost, when
    /// parity may not have caught up yet.
    fn fec_for(&self, len: usize, resync: bool) -> u8 {
        let k = len
            .div_ceil(self.fragmenter.shard_len())
            .clamp(1, fec::BLOCK);
        let (loss, failure) = if resync {
            (self.fec_loss.max(KEYFRAME_LOSS), 1e-6)
        } else {
            (self.fec_loss, 1e-4)
        };
        fec::parity_for(k, loss, failure).min(k.div_ceil(2)) as u8
    }

    /// Parity's share of a typical frame's data.
    fn fec_overhead(&self) -> f64 {
        let len = self.frame_bytes.max(1.0) as usize;
        let k = len
            .div_ceil(self.fragmenter.shard_len())
            .clamp(1, fec::BLOCK);
        f64::from(self.fec_for(len, false)) / k as f64
    }

    /// Sends one encoded frame as datagrams (with its parity), or nothing if
    /// QUIC can't take all of it now. True if sent.
    fn send_frame(
        &mut self,
        conn: &Connection,
        frame: &EncodedFrame,
        recovers: bool,
        epoch: Instant,
        stats: &mut StreamerStats,
    ) -> bool {
        // QUIC's path MTU can shrink mid-session (its black-hole detection,
        // after losses): cut to what it takes now, or every datagram is
        // refused.
        if let Some(max) = conn.max_datagram_size()
            && max != self.fragmenter.max_datagram()
            && max > HEADER_LEN + 4
        {
            info!(
                from = self.fragmenter.max_datagram(),
                to = max,
                "datagram size changed"
            );
            self.fragmenter = Fragmenter::new(max);
        }
        let fec = self.fec_for(frame.data.len(), frame.key || recovers);
        let datagrams = self.fragmenter.datagram_count(frame.data.len(), fec);
        let budget =
            datagrams * (HEADER_LEN + DATAGRAM_OVERHEAD + self.fragmenter.payload_capacity());
        if conn.quic_connection().datagram_send_buffer_space() < budget {
            return false;
        }
        let header = DatagramHeader {
            kind: Kind::Video,
            flags: Flags(if frame.key {
                Flags::KEYFRAME
            } else if recovers {
                Flags::RECOVERY
            } else {
                0
            }),
            stream: self.stream,
            fec,
            frame_id: self.frame_id,
            frag_index: 0,
            frag_count: 0,
            send_ts_us: epoch.elapsed().as_micros() as u32,
        };
        let mut refused = None;
        let sent = self
            .fragmenter
            .fragment_fec(header, &frame.data, fec, |datagram| {
                match conn.send_datagram(datagram) {
                    Ok(()) => stats.bytes_sent += datagram.len() as u64,
                    Err(err) => refused = Some(err),
                }
            });
        if let Some(err) = refused {
            debug!(len = frame.data.len(), "video datagram refused: {err}");
            return false;
        }
        sent.is_ok()
    }

    /// From now on, `codec`'s frames, as the next stream. The old encoder
    /// idles once nobody subscribes.
    fn take_over(&mut self, codec: VideoCodec, subscription: Subscription, target_bps: u32) {
        self.codec = codec;
        self.frames = subscription.frames;
        self.pace = subscription.pace;
        self.pace.set_target(target_bps);
        self.frame_bytes = 0.0;
        self.stream = self.stream.wrapping_add(1);
        self.resync = true;
        // Another encoder's indices.
        self.lost = None;
        self.sent.clear();
        self.encode_ms.clear();
        self.hop_us.clear();
    }

    /// The delays since the last report into `stats`.
    fn report(&mut self, stats: &mut StreamerStats) {
        self.encode_ms.sort_by(f64::total_cmp);
        self.hop_us.sort_unstable();
        stats.composite_to_encoded_ms_p50 = percentile(&self.encode_ms, 0.5);
        stats.composite_to_encoded_ms_p99 = percentile(&self.encode_ms, 0.99);
        stats.encoded_to_sent_us_p50 = percentile(&self.hop_us, 0.5);
        stats.encoded_to_sent_us_p99 = percentile(&self.hop_us, 0.99);
        self.encode_ms.clear();
        self.hop_us.clear();
    }
}

/// A session's rate control (P2.5): its send queue and the path's delay
/// set the encoder's rate, and hold it while the queue drains.
struct Pacing {
    rate: RateControl,
    /// The page's send → decoded reports, the delay signal.
    reports: Reports,
    /// QUIC's packets sent and lost per update, the last 2 s: the loss
    /// until the page reports.
    losses: VecDeque<(Instant, u64, u64)>,
    /// FEC follows the loss (plan §3.1 rule 6) seen while the path was
    /// calm, and when there last was some; and when it last had a queue.
    calm_loss: f64,
    last_loss: Option<Instant>,
    congested_at: Option<Instant>,
    /// Parity's share of a typical frame, as last sized.
    fec_overhead: f64,
    /// QUIC's counters at the last rate update.
    last: Option<(Instant, u64, u64, u64)>,
    logged: Instant,
    /// The last verdict was a stall (logged as one starts).
    stalled: bool,
}

impl Pacing {
    fn new(max_bps: u32) -> Self {
        Self {
            rate: RateControl::new(MIN_BPS, max_bps),
            reports: Reports::default(),
            losses: VecDeque::new(),
            calm_loss: 0.0,
            last_loss: None,
            congested_at: None,
            fec_overhead: 0.0,
            last: None,
            logged: Instant::now(),
            stalled: false,
        }
    }

    /// The encoder's rate: video's share, less what its parity takes.
    fn encoder_target(&self) -> u32 {
        (f64::from(self.rate.target()) * VIDEO_SHARE / (1.0 + self.fec_overhead)) as u32
    }

    /// The loss now: the page's count over the last second, QUIC's over
    /// 2 s until it reports.
    fn loss(&mut self, now: Instant, sent: u64, lost: u64, reported: Option<f64>) -> f64 {
        self.losses.push_back((now, sent, lost));
        while self
            .losses
            .front()
            .is_some_and(|(at, _, _)| now.duration_since(*at) > Duration::from_secs(2))
        {
            self.losses.pop_front();
        }
        let (sent, lost) = self
            .losses
            .iter()
            .fold((0, 0), |(s, l), (_, ds, dl)| (s + ds, l + dl));
        let quic = if sent >= 20 {
            lost as f64 / sent as f64
        } else {
            0.0
        };
        // The page's count when it reports: QUIC's also has packets that
        // came, reordered, after it gave up on them.
        reported.unwrap_or(quic)
    }

    /// The loss to size parity for. Only what's lost on a calm path counts
    /// (no queue for longer than the loss is counted over): a queue's
    /// overflow is the rate's to fix, and parity would only add to it. Half
    /// again (one second's count is noisy), at least 0.3 %, kept for 5 s
    /// after the last loss; none on a clean link.
    fn update_fec(&mut self, now: Instant, loss: f64, congested: bool) -> f64 {
        if congested {
            self.congested_at = Some(now);
        }
        let calm = self
            .congested_at
            .is_none_or(|at| now.duration_since(at) > Duration::from_millis(1200));
        if calm {
            self.calm_loss = loss;
            if loss > 0.001 {
                self.last_loss = Some(now);
            }
        }
        let recent = self
            .last_loss
            .is_some_and(|at| now.duration_since(at) < Duration::from_secs(5));
        if recent {
            (self.calm_loss * 1.5).max(0.003)
        } else {
            0.0
        }
    }

    fn tick(
        &mut self,
        conn: &Connection,
        video: &mut Video,
        media: &Media,
        stats: &mut StreamerStats,
    ) {
        let quic = conn.quic_connection();
        let backlog = SEND_BUFFER.saturating_sub(quic.datagram_send_buffer_space());
        // The queue in frames: hold the encoder (skip frames, none dropped)
        // while it's more than a frame and a half.
        let frame_bytes = video
            .frame_bytes
            .max(f64::from(video.pace.target()) / 8.0 / 60.0)
            .max(1.0);
        let frames_queued = backlog as f64 / frame_bytes;
        let hold = frames_queued > HOLD_FRAMES;
        video.pace.set_hold(hold);
        if video.resync && !hold {
            video.ask_resync(media);
        }
        let now = Instant::now();
        let s = quic.stats();
        let counters = (
            now,
            s.udp_tx.bytes,
            s.path.sent_packets,
            s.path.lost_packets,
        );
        let Some((then, bytes, sent, lost)) = self.last.replace(counters) else {
            return;
        };
        let secs = now.duration_since(then).as_secs_f64().max(1e-3);
        let report = self.reports.latest(now);
        let loss = self.loss(
            now,
            counters.2.saturating_sub(sent),
            counters.3.saturating_sub(lost),
            report.loss,
        );
        let sample = Sample {
            rtt: s.path.rtt,
            delivery_ms: report.delivery_ms,
            rx_bps: report.rx_bps,
            backlog_ms: frames_queued * 1000.0 / 60.0,
            loss,
            tx_bps: counters.1.saturating_sub(bytes) as f64 * 8.0 / secs,
        };
        let verdict = self.rate.update(now, &sample);
        let queue_ms = self.rate.queue_ms();
        let congested = queue_ms > 15.0
            || matches!(
                verdict,
                Verdict::Overuse | Verdict::Draining | Verdict::Backoff
            );
        video.fec_loss = self.update_fec(now, loss, congested);
        self.fec_overhead = video.fec_overhead();
        video.pace.set_target(self.encoder_target());
        stats.target_mbps = Some(f64::from(self.rate.target()) / 1e6);
        stats.queue_ms = Some(queue_ms);
        let stall_starts = verdict == Verdict::Stall && !self.stalled;
        self.stalled = verdict == Verdict::Stall;
        if verdict == Verdict::Overuse
            || stall_starts
            || now.duration_since(self.logged) >= Duration::from_secs(5)
        {
            self.logged = now;
            info!(
                ?verdict,
                target_mbps = format!("{:.1}", f64::from(self.rate.target()) / 1e6),
                tx_mbps = format!("{:.1}", sample.tx_bps / 1e6),
                rtt_ms = s.path.rtt.as_millis() as u64,
                queue_ms = format!("{queue_ms:.0}"),
                backlog_frames = format!("{frames_queued:.1}"),
                rx_mbps = sample.rx_bps.map(|r| format!("{:.1}", r / 1e6)),
                loss = format!("{:.3}", sample.loss),
                fec = format!("{:.2}", self.fec_overhead),
                "rate"
            );
        }
    }
}

async fn next_frame(next: &mut Option<(VideoCodec, Subscription)>) -> Option<EncodedFrame> {
    match next {
        Some((_, subscription)) => subscription.frames.recv().await,
        None => std::future::pending().await,
    }
}

/// Sends a PyroWave frame: a datagram per packet, or several for a packet
/// bigger than a datagram (a busy 32×32 block can be), flagged `CONTINUES`
/// and `CONTINUED` so the page uses it only whole. Nothing if QUIC can't take the whole
/// frame now. True if sent.
fn send_packets(
    conn: &Connection,
    frame: &EncodedFrame,
    frame_id: u32,
    stream: u8,
    epoch: Instant,
    stats: &mut StreamerStats,
) -> bool {
    let Some(max_payload) = conn
        .max_datagram_size()
        .and_then(|m| m.checked_sub(HEADER_LEN))
        .filter(|m| *m > 0)
    else {
        return false;
    };
    let count: usize = frame
        .packets
        .iter()
        .map(|p| (p.len as usize).div_ceil(max_payload).max(1))
        .sum();
    let budget = frame.data.len() + count * (HEADER_LEN + DATAGRAM_OVERHEAD);
    let space = conn.quic_connection().datagram_send_buffer_space();
    if space < budget {
        debug!(space, budget, "pyrowave frame doesn't fit the send buffer");
        return false;
    }
    let Ok(frag_count) = u16::try_from(count) else {
        return false;
    };
    let send_ts_us = epoch.elapsed().as_micros() as u32;
    let mut datagram = Vec::with_capacity(HEADER_LEN + max_payload);
    let mut index: u16 = 0;
    for packet in &frame.packets {
        let start = packet.offset as usize;
        let unit = &frame.data[start..start + packet.len as usize];
        let mut chunks = unit.chunks(max_payload).enumerate().peekable();
        while let Some((part, chunk)) = chunks.next() {
            let mut flags = Flags::KEYFRAME | Flags::INTRA;
            if chunks.peek().is_some() {
                flags |= Flags::CONTINUES;
            }
            if part > 0 {
                flags |= Flags::CONTINUED;
            }
            let header = DatagramHeader {
                kind: Kind::Video,
                flags: Flags(flags),
                stream,
                fec: 0,
                frame_id,
                frag_index: index,
                frag_count,
                send_ts_us,
            };
            let mut head = [0u8; HEADER_LEN];
            header.encode(&mut head);
            datagram.clear();
            datagram.extend_from_slice(&head);
            datagram.extend_from_slice(chunk);
            if let Err(err) = conn.send_datagram(&datagram) {
                debug!(
                    index,
                    count,
                    len = datagram.len(),
                    "pyrowave datagram refused: {err}"
                );
                return false;
            }
            stats.bytes_sent += datagram.len() as u64;
            index += 1;
        }
    }
    true
}

fn send_audio(conn: &Connection, packet: &AudioPacket, epoch: Instant, stats: &mut StreamerStats) {
    let header = DatagramHeader {
        kind: Kind::Audio,
        flags: Flags(0),
        stream: 0,
        fec: 0,
        // 10 ms frames: the mixer's sample clock in frames.
        frame_id: (packet.samples / crate::audio::FRAME as u64) as u32,
        frag_index: 0,
        frag_count: 1,
        send_ts_us: epoch.elapsed().as_micros() as u32,
    };
    let mut datagram = vec![0u8; HEADER_LEN + packet.data.len()];
    header.encode(
        (&mut datagram[..HEADER_LEN])
            .try_into()
            .expect("header length"),
    );
    datagram[HEADER_LEN..].copy_from_slice(&packet.data);
    if conn.send_datagram(datagram).is_ok() {
        stats.audio_packets += 1;
        stats.audio_bytes += packet.data.len() as u64;
    }
}

/// The codec a `{"t":"codec","codec":…}` line asks for (WebTransport only:
/// the session switches its own subscription).
/// The frame a `{"t":"rfi","id":…}` line says the page lost.
fn rfi_request(line: &str) -> Option<u32> {
    if !line.contains("\"rfi\"") {
        return None;
    }
    let msg: serde_json::Value = serde_json::from_str(line).ok()?;
    if msg.get("t")?.as_str()? != "rfi" {
        return None;
    }
    u32::try_from(msg.get("id")?.as_u64()?).ok()
}

fn codec_request(line: &str) -> Option<String> {
    let msg: serde_json::Value = serde_json::from_str(line).ok()?;
    if msg.get("t")?.as_str()? != "codec" {
        return None;
    }
    Some(msg.get("codec")?.as_str()?.to_string())
}

async fn next_packet(audio: &mut Option<mpsc::Receiver<AudioPacket>>) -> Option<AudioPacket> {
    match audio {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

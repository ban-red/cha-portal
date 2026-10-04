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

use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use cha_proto::{DatagramHeader, Flags, Fragmenter, HEADER_LEN, Kind};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::{mpsc, oneshot};
use tokio::time::{MissedTickBehavior, interval_at};
use tracing::{debug, info, warn};
use wtransport::endpoint::IncomingSession;
use wtransport::endpoint::endpoint_side::Server;
use wtransport::quinn::TransportConfig;
use wtransport::quinn::congestion::CubicConfig;
use wtransport::{Connection, Endpoint, Identity, ServerConfig, VarInt};

use crate::audio::{Audio, AudioPacket};
use crate::codec::VideoCodec;
use crate::control::{Control, ServerMsg, StreamerStats, percentile};
use crate::gamepad::Gamepads;
use crate::media::{EncodedFrame, Media};
use crate::session::Running;

const STATS_INTERVAL: Duration = Duration::from_millis(500);
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
        .datagram_send_buffer_size(8 << 20)
        .datagram_receive_buffer_size(Some(1 << 20))
        .max_idle_timeout(Some(Duration::from_secs(10).try_into()?))
        .keep_alive_interval(Some(Duration::from_secs(2)))
        // S1: Cubic keeps up on a LAN, quinn's BBR stalls (plan §3.1 rule 9).
        .congestion_controller_factory(Arc::new(CubicConfig::default()));
    let config = ServerConfig::builder()
        .with_bind_default(port)
        .with_custom_transport(identity, transport)
        .build();
    let endpoint = Endpoint::server(config).context("binding the WebTransport endpoint")?;
    Ok((endpoint, hash))
}

/// Checks a presented token; the error says why not.
pub type Authorize = Box<dyn Fn(&str) -> std::result::Result<(), String> + Send + Sync>;
/// Stops the running session (any transport) and records this one.
pub type TakeOver = Box<
    dyn Fn(Running) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
        + Send
        + Sync,
>;

/// What a session needs from the streamer.
pub struct Sessions {
    pub media: Arc<Media>,
    pub audio: Option<Arc<Audio>>,
    pub gamepads: Option<Arc<Gamepads>>,
    pub authorize: Authorize,
    pub take_over: TakeOver,
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
    if let Err(why) = (sessions.authorize)(&value("token").unwrap_or_default()) {
        warn!("rejected a WebTransport session: {why}");
        request.forbidden().await;
        return Ok(());
    }
    let Some(codec) = value("codec")
        .as_deref()
        .and_then(VideoCodec::from_name)
        .filter(|c| sessions.media.codecs().contains(c))
    else {
        request.not_found().await;
        bail!("no ?codec=, or one this streamer doesn't offer");
    };
    let remote = request.remote_address();
    let conn = request.accept().await?;
    info!(%remote, codec = codec.name(), "webtransport session");
    let (stop, stopped) = oneshot::channel();
    let session = Arc::clone(&sessions);
    let handle = tokio::spawn(async move {
        if let Err(err) = run(conn, codec, session, stopped).await {
            info!(%remote, "webtransport session ended: {err:#}");
        }
    });
    (sessions.take_over)(Running { stop, handle }).await;
    Ok(())
}

async fn run(
    conn: Connection,
    codec: VideoCodec,
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
    let mut audio = sessions.audio.as_ref().map(|a| a.subscribe());
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
                video.take_over(next, frames);
                handler.codec = next;
                info!(codec = next.name(), stream = video.stream, "switched codec");
                let _ = out.send(ServerMsg::Codec { codec: next.name(), stream: video.stream, error: None });
                video.send(&conn, &frame, &sessions.media, epoch, &mut stats);
            }
            packet = next_packet(&mut audio) => match packet {
                Some(packet) => send_audio(&conn, &packet, epoch, &mut stats),
                None => audio = None,
            },
            line = lines.recv() => {
                let Some(line) = line else { break Ok(()) };
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
                        Ok(frames) => {
                            video.next = Some((next, frames));
                            continue;
                        }
                        Err(err) => Some(format!("{err:#}")),
                    },
                };
                let _ = out.send(ServerMsg::Codec { codec: video.codec.name(), stream: video.stream, error });
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
    /// Switching: the other encoder's frames, taken over at the first.
    next: Option<(VideoCodec, mpsc::Receiver<EncodedFrame>)>,
    /// Bumped on each switch and carried in every video datagram, so the
    /// page knows which decoder a frame is for and starts that stream afresh.
    stream: u8,
    frame_id: u32,
    /// Until a keyframe goes out, no frame may (the first, or after a drop).
    resync: bool,
    fragmenter: Fragmenter,
    /// Composited → encoded (ms) and encoded → sent (µs) since the last report.
    encode_ms: Vec<f64>,
    hop_us: Vec<u64>,
}

impl Video {
    fn new(
        codec: VideoCodec,
        frames: mpsc::Receiver<EncodedFrame>,
        fragmenter: Fragmenter,
    ) -> Self {
        Self {
            codec,
            frames,
            next: None,
            stream: 0,
            frame_id: 0,
            resync: true,
            fragmenter,
            encode_ms: Vec::new(),
            hop_us: Vec::new(),
        }
    }

    /// Sends a frame, or drops it (and, for the hardware codecs, all until
    /// a keyframe, which is asked for). True if sent.
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
        if self.resync && !frame.key && !intra {
            return false;
        }
        let sent = if intra {
            send_packets(conn, frame, self.frame_id, self.stream, epoch, stats)
        } else {
            send_frame(
                conn,
                &mut self.fragmenter,
                frame,
                self.frame_id,
                self.stream,
                epoch,
                stats,
            )
        };
        if !sent {
            stats.frames_dropped += 1;
            if !intra {
                // Its dependents would be garbage: start over from a keyframe.
                self.resync = true;
                media.request_keyframe(self.codec);
            }
            return false;
        }
        self.encode_ms
            .push(frame.encoded.duration_since(frame.composited).as_secs_f64() * 1e3);
        self.hop_us.push(frame.encoded.elapsed().as_micros() as u64);
        self.frame_id = self.frame_id.wrapping_add(1);
        stats.frames_sent += 1;
        stats.keyframes += u64::from(frame.key);
        self.resync = false;
        true
    }

    /// From now on, `codec`'s frames, as the next stream. The old encoder
    /// idles once nobody subscribes.
    fn take_over(&mut self, codec: VideoCodec, frames: mpsc::Receiver<EncodedFrame>) {
        self.codec = codec;
        self.frames = frames;
        self.stream = self.stream.wrapping_add(1);
        self.resync = true;
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

async fn next_frame(
    next: &mut Option<(VideoCodec, mpsc::Receiver<EncodedFrame>)>,
) -> Option<EncodedFrame> {
    match next {
        Some((_, frames)) => frames.recv().await,
        None => std::future::pending().await,
    }
}

/// Sends one encoded frame as datagrams, or nothing if QUIC can't take all
/// of it now. True if sent.
fn send_frame(
    conn: &Connection,
    fragmenter: &mut Fragmenter,
    frame: &EncodedFrame,
    frame_id: u32,
    stream: u8,
    epoch: Instant,
    stats: &mut StreamerStats,
) -> bool {
    let fragments = fragmenter.fragment_count(frame.data.len());
    let budget = frame.data.len() + fragments * (HEADER_LEN + DATAGRAM_OVERHEAD);
    if conn.quic_connection().datagram_send_buffer_space() < budget {
        return false;
    }
    let header = DatagramHeader {
        kind: Kind::Video,
        flags: Flags(if frame.key { Flags::KEYFRAME } else { 0 }),
        stream,
        frame_id,
        frag_index: 0,
        frag_count: 0,
        send_ts_us: epoch.elapsed().as_micros() as u32,
    };
    let mut ok = true;
    let sent = fragmenter.fragment(header, &frame.data, |datagram| {
        if conn.send_datagram(datagram).is_err() {
            ok = false;
        } else {
            stats.bytes_sent += datagram.len() as u64;
        }
    });
    ok && sent.is_ok()
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

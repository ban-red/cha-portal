//! The WebSocket path (ADR 0022): `cha-stream/1` over one WebSocket, for
//! viewers who can only reach the node through an HTTPS tunnel (a share link
//! over the internet). The portal and the node pass its messages through
//! untouched.
//!
//! Text messages are the control lines of the WebTransport control stream, one
//! per message, both ways. Binary messages (streamer to page only) are
//! `cha-proto` datagrams: the 16-byte header and the payload, no FEC, fragments
//! of at most `MAX_FRAGMENT` bytes; audio is one message per Opus packet.
//! PyroWave isn't offered. TCP loses nothing, so the page never asks for
//! retransmission; it can still miss a whole frame, dropped here, and the
//! resync rules are `wt.rs`'s (the same `Video`).
//!
//! Rate control reads our own send queue instead of QUIC's: a writer task
//! owns the socket's sink and drains a queue, and the bytes queued and not yet
//! written are the backlog. The session loop never waits on the socket.
//! While the backlog is deeper than `HOLD_MS` of the current rate the encoder
//! is held; a frame that doesn't fit the queue's budget is dropped, and the
//! stream resyncs with a recovery or key frame.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use anyhow::Result;
use axum::extract::ws::{CloseFrame, Message, WebSocket};
use cha_proto::{DatagramHeader, Flags, Fragmenter, HEADER_LEN, Kind};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::{mpsc, oneshot};
use tokio::time::{MissedTickBehavior, interval_at};
use tracing::{debug, info};

use crate::audio::{Audio, AudioPacket};
use crate::codec::VideoCodec;
use crate::control::{
    Control, PadFeed, ServerMsg, StreamerStats, cursor_msg, floor_msgs, next_clipboard,
    next_cursor, next_pointer, next_status, pad_audience,
};
use crate::gamepad::Gamepads;
use crate::media::{EncodedFrame, Media};
use crate::rate::{MIN_BPS, RateControl, Reports, Sample, VIDEO_SHARE, Verdict, parse_report};
use crate::system::Sampler;
use crate::viewers::Seat;
use crate::wt::{
    HOLD_MS, PACE_INTERVAL, STATS_INTERVAL, SYSTEM_INTERVAL, Video, ceiling_bps, codec_request,
    next_frame, next_packet, rfi_request,
};

/// The biggest message of media, header included.
pub const MAX_FRAGMENT: usize = 65536;
/// What the queue holds at least (a big keyframe at a low rate), and for how
/// long at the current rate otherwise: whichever is more. A frame that would
/// pass it is dropped.
const BUDGET_FLOOR: usize = 2 << 20;
const BUDGET_MS: f64 = 250.0;
/// A queue with bytes in it and nothing written for this long: the path is
/// dead, so the session ends and frees its seat.
const STALL_LIMIT: Duration = Duration::from_secs(15);
/// How long a closing session waits for the close frame to go out.
const CLOSE_GRACE: Duration = Duration::from_secs(1);

/// What a session needs from the streamer (the token and the seat are
/// checked before the upgrade).
pub struct Sessions {
    pub media: Arc<Media>,
    pub audio: Option<Arc<Audio>>,
    pub gamepads: Option<Arc<Gamepads>>,
}

/// What the writer is told to send.
enum Out {
    Text(String),
    Binary(Vec<u8>),
    Close(u16, String),
}

/// The send queue's counters, shared with the writer task.
struct Counters {
    epoch: Instant,
    /// Media bytes queued and not yet written.
    queued: AtomicUsize,
    /// Media bytes written, ever.
    written: AtomicU64,
    /// When a write last finished, or the queue last went from empty (ms
    /// after `epoch`).
    progress_ms: AtomicU64,
}

impl Counters {
    fn new() -> Self {
        Self {
            epoch: Instant::now(),
            queued: AtomicUsize::new(0),
            written: AtomicU64::new(0),
            progress_ms: AtomicU64::new(0),
        }
    }

    fn now_ms(&self) -> u64 {
        self.epoch.elapsed().as_millis() as u64
    }

    fn queue(&self, bytes: usize) {
        if self.queued.fetch_add(bytes, Ordering::Relaxed) == 0 {
            self.progress_ms.store(self.now_ms(), Ordering::Relaxed);
        }
    }

    fn wrote(&self, bytes: usize) {
        self.queued.fetch_sub(bytes, Ordering::Relaxed);
        self.written.fetch_add(bytes as u64, Ordering::Relaxed);
        self.progress_ms.store(self.now_ms(), Ordering::Relaxed);
    }

    fn backlog(&self) -> usize {
        self.queued.load(Ordering::Relaxed)
    }

    /// Bytes waiting, and nothing written for `limit`.
    fn stalled(&self, limit: Duration) -> bool {
        self.backlog() > 0
            && self
                .now_ms()
                .saturating_sub(self.progress_ms.load(Ordering::Relaxed))
                > limit.as_millis() as u64
    }
}

/// The session's way to the socket.
struct Queue {
    tx: mpsc::UnboundedSender<Out>,
    counters: Arc<Counters>,
}

impl Queue {
    fn backlog(&self) -> usize {
        self.counters.backlog()
    }

    fn text(&self, line: String) {
        let _ = self.tx.send(Out::Text(line));
    }

    /// Queues a frame's messages (the caller checked they fit).
    fn media(&self, messages: Vec<Vec<u8>>) {
        for message in messages {
            self.counters.queue(message.len());
            let _ = self.tx.send(Out::Binary(message));
        }
    }

    fn close(&self, code: u16, reason: &str) {
        let _ = self.tx.send(Out::Close(code, reason.to_owned()));
    }
}

/// How many bytes the queue may hold at `rate_bps`.
fn budget_bytes(rate_bps: u32) -> usize {
    let at_rate = f64::from(rate_bps) / 8.0 * BUDGET_MS / 1000.0;
    BUDGET_FLOOR.max(at_rate as usize)
}

/// Whether a frame of `wire_len` bytes goes into a queue holding `backlog`:
/// within the budget, or into an empty queue (or a frame bigger than the
/// budget could never be sent).
fn fits(backlog: usize, wire_len: usize, budget: usize) -> bool {
    backlog == 0 || backlog + wire_len <= budget
}

/// The backlog in milliseconds of sending at `rate_bps`.
fn backlog_ms(backlog: usize, rate_bps: u32) -> f64 {
    backlog as f64 * 8000.0 / f64::from(rate_bps.max(MIN_BPS))
}

/// A frame as messages: header and payload each, in order.
fn frame_messages(
    fragmenter: &mut Fragmenter,
    header: DatagramHeader,
    data: &[u8],
) -> Option<Vec<Vec<u8>>> {
    let mut messages = Vec::with_capacity(fragmenter.fragment_count(data.len()));
    fragmenter
        .fragment(header, data, |datagram| messages.push(datagram.to_vec()))
        .ok()?;
    Some(messages)
}

/// Queues one encoded frame, or nothing if the queue can't take all of it.
/// True if queued.
fn send_frame(
    video: &mut Video,
    queue: &Queue,
    budget: usize,
    frame: &EncodedFrame,
    recovers: bool,
    epoch: Instant,
    stats: &mut StreamerStats,
) -> bool {
    let len = frame.data.len();
    let wire = len + video.fragmenter.fragment_count(len) * HEADER_LEN;
    if !fits(queue.backlog(), wire, budget) {
        debug!(
            backlog = queue.backlog(),
            wire, budget, "video frame doesn't fit the send queue"
        );
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
        stream: video.stream,
        fec: 0,
        frame_id: video.frame_id,
        frag_index: 0,
        frag_count: 0,
        send_ts_us: epoch.elapsed().as_micros() as u32,
    };
    let Some(messages) = frame_messages(&mut video.fragmenter, header, &frame.data) else {
        return false;
    };
    stats.bytes_sent += wire as u64;
    queue.media(messages);
    true
}

fn send_audio(
    queue: &Queue,
    budget: usize,
    packet: &AudioPacket,
    epoch: Instant,
    stats: &mut StreamerStats,
) {
    // Late audio is worse than none: past the budget it's skipped.
    if queue.backlog() > budget {
        return;
    }
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
    let mut message = vec![0u8; HEADER_LEN + packet.data.len()];
    header.encode(
        (&mut message[..HEADER_LEN])
            .try_into()
            .expect("header length"),
    );
    message[HEADER_LEN..].copy_from_slice(&packet.data);
    queue.media(vec![message]);
    stats.audio_packets += 1;
    stats.audio_bytes += packet.data.len() as u64;
}

/// A session's rate control: its send queue and the page's reports set the
/// encoder's rate, and hold it while the queue drains.
struct Pacing {
    rate: RateControl,
    reports: Reports,
    /// When the rate was last updated and the bytes written then.
    last: Option<(Instant, u64)>,
    logged: Instant,
    stalled: bool,
    /// What the queue may hold now.
    budget: usize,
}

impl Pacing {
    fn new(max_bps: u32) -> Self {
        Self {
            rate: RateControl::new(MIN_BPS, max_bps),
            reports: Reports::default(),
            last: None,
            logged: Instant::now(),
            stalled: false,
            budget: BUDGET_FLOOR,
        }
    }

    /// The encoder's rate: video's share (no parity to take from it).
    fn encoder_target(&self) -> u32 {
        (f64::from(self.rate.target()) * VIDEO_SHARE) as u32
    }

    fn tick(&mut self, queue: &Queue, video: &mut Video, media: &Media, stats: &mut StreamerStats) {
        let fps = media.fps();
        if fps != video.fps {
            video.fps = fps;
            self.rate.rescale(ceiling_bps(media, video.codec));
            info!(
                fps,
                ceiling_mbps = ceiling_bps(media, video.codec) / 1_000_000,
                "frame rate"
            );
        }
        let backlog = queue.backlog();
        let backlog_ms = backlog_ms(backlog, self.rate.target());
        let hold = backlog_ms > HOLD_MS;
        video.pace.set_hold(hold);
        if video.resync && !hold {
            video.ask_resync(media);
        }
        let now = Instant::now();
        let written = queue.counters.written.load(Ordering::Relaxed);
        let Some((then, before)) = self.last.replace((now, written)) else {
            return;
        };
        let secs = now.duration_since(then).as_secs_f64().max(1e-3);
        let report = self.reports.latest(now);
        let sample = Sample {
            // The path's round trip isn't ours to see on TCP; the page's
            // delay reports are the signal.
            rtt: Duration::ZERO,
            delivery_ms: report.delivery_ms,
            rx_bps: report.rx_bps,
            backlog_ms,
            loss: report.loss.unwrap_or(0.0),
            tx_bps: written.saturating_sub(before) as f64 * 8.0 / secs,
        };
        let verdict = self.rate.update(now, &sample);
        let queue_ms = self.rate.queue_ms();
        self.budget = budget_bytes(self.rate.target());
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
                queue_ms = format!("{queue_ms:.0}"),
                backlog_kb = backlog / 1000,
                rx_mbps = sample.rx_bps.map(|r| format!("{:.1}", r / 1e6)),
                "rate (websocket)"
            );
        }
    }
}

/// Runs a session on an upgraded socket until it or the page ends.
pub async fn run(
    socket: WebSocket,
    codec: VideoCodec,
    seat: Seat,
    sessions: Arc<Sessions>,
    mut stopped: oneshot::Receiver<()>,
) -> Result<()> {
    let epoch = Instant::now();
    let (mut sink, mut stream) = socket.split();

    // The socket: messages in through a task, out through another.
    let (lines_tx, mut lines) = mpsc::channel::<String>(256);
    let reader = tokio::spawn(async move {
        while let Some(Ok(message)) = stream.next().await {
            match message {
                Message::Text(text) => {
                    if lines_tx.send(text.as_str().to_owned()).await.is_err() {
                        break;
                    }
                }
                Message::Close(_) => break,
                // Pings are answered by the socket; binary isn't ours.
                _ => {}
            }
        }
    });
    let counters = Arc::new(Counters::new());
    let (tx, mut to_write) = mpsc::unbounded_channel::<Out>();
    let queue = Queue {
        tx,
        counters: Arc::clone(&counters),
    };
    let mut writer = tokio::spawn(async move {
        while let Some(out) = to_write.recv().await {
            let sent = match out {
                Out::Text(line) => sink.send(Message::Text(line.into())).await,
                Out::Binary(bytes) => {
                    let len = bytes.len();
                    let sent = sink.send(Message::Binary(bytes.into())).await;
                    counters.wrote(len);
                    sent
                }
                Out::Close(code, reason) => {
                    let frame = CloseFrame {
                        code,
                        reason: reason.into(),
                    };
                    let _ = sink.send(Message::Close(Some(frame))).await;
                    break;
                }
            };
            if sent.is_err() {
                break;
            }
        }
    });
    // Control replies: serialized into text messages.
    let (out, mut outbox) = mpsc::unbounded_channel::<ServerMsg>();
    let text_queue = Queue {
        tx: queue.tx.clone(),
        counters: Arc::clone(&queue.counters),
    };
    let control = tokio::spawn(async move {
        while let Some(msg) = outbox.recv().await {
            text_queue.text(serde_json::to_string(&msg).expect("control messages serialize"));
        }
    });

    let mut handler = Control::new(
        epoch,
        codec,
        Arc::clone(&sessions.media),
        sessions.gamepads.clone(),
        seat,
    );
    let (w, h) = sessions.media.size();
    let _ = out.send(ServerMsg::Hello {
        stream: serde_json::json!({
            "codec": codec.name(),
            "width": w,
            "height": h,
            "input": true,
            "audio": sessions.audio.is_some(),
            "gamepads": sessions.gamepads.is_some(),
            "fps": sessions.media.fps(),
            "overlay": sessions.media.overlay(),
            "transport": "websocket",
            "maxDatagram": MAX_FRAGMENT,
        }),
    });

    let subscribed = Instant::now();
    let mut video = Video::new(
        codec,
        sessions.media.subscribe(codec)?,
        Fragmenter::new(MAX_FRAGMENT),
        sessions.media.fps(),
    );
    let mut pacing = Pacing::new(ceiling_bps(&sessions.media, video.codec));
    let mut pace_tick = interval_at(tokio::time::Instant::now() + PACE_INTERVAL, PACE_INTERVAL);
    pace_tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut audio = sessions.audio.as_ref().map(|a| a.subscribe());
    let mut clipboard = Some(sessions.media.clipboard());
    let mut cursor = Some(sessions.media.cursor());
    let mut cursor_ids = std::collections::HashSet::new();
    let mut pointer = Some(sessions.media.pointer());
    let mut status = Some(sessions.media.status());
    let mut rumble = PadFeed::new(sessions.gamepads.as_deref());
    for msg in floor_msgs(&handler.seat) {
        let _ = out.send(msg);
    }
    for msg in rumble.replay_for(&pad_audience(&handler.seat)) {
        let _ = out.send(msg);
    }
    let mut stats = StreamerStats::default();
    let mut report = interval_at(tokio::time::Instant::now() + STATS_INTERVAL, STATS_INTERVAL);
    report.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut system = Sampler::new();
    let mut system_tick = interval_at(tokio::time::Instant::now(), SYSTEM_INTERVAL);
    system_tick.set_missed_tick_behavior(MissedTickBehavior::Skip);

    let (result, close) = loop {
        tokio::select! {
            frame = video.frames.recv() => {
                let Some(frame) = frame else {
                    let reason = format!("the {} encoder stopped", video.codec.name());
                    break (Err(anyhow::anyhow!(reason.clone())), Some((1011, reason)));
                };
                if send_video(&mut video, &queue, pacing.budget, &frame, &sessions.media, epoch, &mut stats) {
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
                send_video(&mut video, &queue, pacing.budget, &frame, &sessions.media, epoch, &mut stats);
            }
            packet = next_packet(&mut audio) => match packet {
                Some(packet) => send_audio(&queue, pacing.budget, &packet, epoch, &mut stats),
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
                for msg in floor_msgs(&handler.seat) {
                    let _ = out.send(msg);
                }
                for msg in rumble.replay_for(&pad_audience(&handler.seat)) {
                    let _ = out.send(msg);
                }
            }
            // What the apps do to the pads (rumble, lightbar, ...) is the
            // controller's, or for a player's slot the player's alone.
            event = rumble.next() => {
                if let Some(msg) = pad_audience(&handler.seat).message(&event) {
                    let _ = out.send(msg);
                }
            }
            // Every viewer sees what the app is setting up, not only the controller.
            msg = next_status(&mut status) => {
                if let Some(msg) = msg {
                    let _ = out.send(msg);
                }
            }
            line = lines.recv() => {
                let Some(line) = line else { break (Ok(()), None) };
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
                    Some(VideoCodec::PyroWave(_)) => Some(format!("{name} isn't offered over WebSocket")),
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
                if queue.counters.stalled(STALL_LIMIT) {
                    break (
                        Err(anyhow::anyhow!("nothing written to the socket for {STALL_LIMIT:?}")),
                        Some((1011, "stalled".into())),
                    );
                }
                pacing.tick(&queue, &mut video, &sessions.media, &mut stats);
            }
            _ = report.tick() => {
                video.report(&mut stats);
                stats.fps = Some(sessions.media.fps());
                stats.overlay = sessions.media.overlay();
                let _ = out.send(ServerMsg::Stats {
                    elapsed_ms: epoch.elapsed().as_millis() as u64,
                    stats: stats.clone(),
                });
            }
            _ = system_tick.tick() => {
                if let Some(sample) = system.sample() {
                    let _ = out.send(ServerMsg::System(sample));
                }
            }
            _ = &mut writer => break (Err(anyhow::anyhow!("the socket closed")), None),
            _ = &mut stopped => {
                info!("websocket session replaced");
                break (Ok(()), Some((1000, "replaced".into())));
            }
        }
    };
    reader.abort();
    control.abort();
    if let Some((code, reason)) = close {
        // Let the close frame out (if the writer is still there to send it).
        queue.close(code, &reason);
        let _ = tokio::time::timeout(CLOSE_GRACE, &mut writer).await;
    }
    writer.abort();
    result
}

/// Sends a frame through the queue, or drops it (see `Video::send_with`).
/// True if queued.
fn send_video(
    video: &mut Video,
    queue: &Queue,
    budget: usize,
    frame: &EncodedFrame,
    media: &Media,
    epoch: Instant,
    stats: &mut StreamerStats,
) -> bool {
    video.send_with(frame, media, stats, |video, intra, recovers, stats| {
        // PyroWave isn't offered, so no frame stands alone.
        !intra && send_frame(video, queue, budget, frame, recovers, epoch, stats)
    })
}

/// The codec in a request's query, as this transport offers it: the hardware
/// ones. The error is the HTTP status and why.
pub fn codec_for(
    name: Option<&str>,
    offered: &[VideoCodec],
) -> std::result::Result<VideoCodec, (u16, String)> {
    let Some(codec) = name.and_then(VideoCodec::from_name) else {
        return Err((404, "no ?codec=, or one this streamer doesn't offer".into()));
    };
    if matches!(codec, VideoCodec::PyroWave(_)) {
        return Err((
            400,
            "bad_codec: PyroWave isn't offered over WebSocket".into(),
        ));
    }
    if !offered.contains(&codec) {
        return Err((404, "no ?codec=, or one this streamer doesn't offer".into()));
    }
    Ok(codec)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header() -> DatagramHeader {
        DatagramHeader {
            kind: Kind::Video,
            flags: Flags(Flags::KEYFRAME),
            stream: 0,
            fec: 0,
            frame_id: 7,
            frag_index: 0,
            frag_count: 0,
            send_ts_us: 0,
        }
    }

    #[test]
    fn a_frame_becomes_ordered_messages_without_parity() {
        let data: Vec<u8> = (0..150_000u32).map(|i| i as u8).collect();
        let mut fragmenter = Fragmenter::new(MAX_FRAGMENT);
        let messages = frame_messages(&mut fragmenter, header(), &data).unwrap();
        assert_eq!(messages.len(), 3);
        let mut joined = Vec::new();
        for (i, message) in messages.iter().enumerate() {
            assert!(message.len() <= MAX_FRAGMENT);
            let (head, payload) = DatagramHeader::decode(message).unwrap();
            assert_eq!(head.fec, 0);
            assert!(!head.flags.has(Flags::PARITY));
            assert_eq!((head.frag_index as usize, head.frag_count), (i, 3));
            assert_eq!(head.frame_id, 7);
            joined.extend_from_slice(payload);
        }
        assert_eq!(joined, data);
        // A small frame is one message; the count the budget check uses
        // matches what is made.
        let small = frame_messages(&mut fragmenter, header(), &[1, 2, 3]).unwrap();
        assert_eq!(small.len(), 1);
        assert_eq!(fragmenter.fragment_count(data.len()), messages.len());
    }

    #[test]
    fn the_budget_is_a_floor_or_a_quarter_second() {
        assert_eq!(budget_bytes(10_000_000), BUDGET_FLOOR);
        // 200 Mbit/s for 250 ms is 6.25 MB.
        assert_eq!(budget_bytes(200_000_000), 6_250_000);
    }

    #[test]
    fn a_frame_that_passes_the_budget_is_dropped_unless_the_queue_is_empty() {
        let budget = 2_000_000;
        assert!(fits(0, 100_000, budget));
        assert!(fits(1_000_000, 900_000, budget));
        assert!(!fits(1_000_000, 1_100_000, budget));
        // A keyframe bigger than the budget goes out once the queue is empty.
        assert!(fits(0, 3_000_000, budget));
        assert!(!fits(1, 3_000_000, budget));
    }

    #[test]
    fn the_backlog_is_time_at_the_current_rate() {
        // 125 kB at 40 Mbit/s: 25 ms.
        assert!((backlog_ms(125_000, 40_000_000) - 25.0).abs() < 1e-9);
        // Never divides by less than the minimum rate.
        assert!(backlog_ms(1_000, 0).is_finite());
    }

    #[test]
    fn queued_and_written_bytes_balance() {
        let counters = Counters::new();
        counters.queue(1000);
        counters.queue(500);
        assert_eq!(counters.backlog(), 1500);
        counters.wrote(1000);
        assert_eq!(counters.backlog(), 500);
        assert_eq!(counters.written.load(Ordering::Relaxed), 1000);
        counters.wrote(500);
        assert_eq!(counters.backlog(), 0);
        // Nothing waiting is never a stall.
        assert!(!counters.stalled(Duration::ZERO));
    }

    #[test]
    fn a_queue_with_no_progress_stalls() {
        let counters = Counters::new();
        counters.queue(10);
        assert!(!counters.stalled(Duration::from_secs(60)));
        std::thread::sleep(Duration::from_millis(20));
        assert!(counters.stalled(Duration::from_millis(5)));
        counters.wrote(5);
        assert!(!counters.stalled(Duration::from_millis(5)));
    }

    #[test]
    fn pyrowave_is_refused_and_unknown_codecs_are_not_found() {
        let offered = [VideoCodec::Hw(cha_nvenc::Codec::H264)];
        assert_eq!(codec_for(Some("h264"), &offered).unwrap(), offered[0]);
        assert_eq!(codec_for(Some("nope"), &offered).unwrap_err().0, 404);
        assert_eq!(codec_for(None, &offered).unwrap_err().0, 404);
        assert_eq!(codec_for(Some("hevc"), &offered).unwrap_err().0, 404);
        assert_eq!(codec_for(Some("pyrowave420"), &offered).unwrap_err().0, 400);
    }
}

//! From composited frames to encoded ones.
//!
//! The compositor publishes each frame to a [`FrameHub`]; every active codec has
//! an encoder thread with a one-frame mailbox (newest frame wins, so a slow
//! encoder skips frames instead of queueing them). Encoded frames go to that
//! codec's subscribers, the WebRTC sessions.

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use bytes::Bytes;
use cha_nvenc::{Codec, CudaContext, Encoder, EncoderConfig};
use smithay::reexports::calloop::channel;
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::compositor::{Command, MAX_SIZE, Slot, fit_size};
use crate::input::Input;

/// One composited frame, sitting in an output buffer until every encoder that
/// got it lets go.
#[derive(Clone)]
pub struct Frame {
    pub slot: Arc<Slot>,
    pub width: u32,
    pub height: u32,
    /// The output pool's generation: buffers are reallocated on resize.
    pub generation: u64,
    /// When the GPU finished compositing it.
    pub rendered: Instant,
}

/// One encoded access unit (Annex-B for H.264/HEVC, OBUs for AV1).
pub struct EncodedFrame {
    pub data: Bytes,
    pub key: bool,
    /// When the composited frame was ready.
    pub composited: Instant,
    /// When encoding finished.
    pub encoded: Instant,
}

#[derive(Default)]
struct MailboxState {
    frame: Option<Frame>,
    keyframe: bool,
    closed: bool,
    /// Frames replaced before the encoder took them.
    skipped: u64,
}

#[derive(Default)]
struct Mailbox {
    state: Mutex<MailboxState>,
    ready: Condvar,
}

enum Wake {
    Frame(Frame, bool),
    Keyframe,
    Idle,
    Closed,
}

impl Mailbox {
    fn put(&self, frame: Frame) {
        let mut state = self.state.lock().expect("mailbox lock");
        if state.frame.replace(frame).is_some() {
            state.skipped += 1;
        }
        self.ready.notify_one();
    }

    fn request_keyframe(&self) {
        self.state.lock().expect("mailbox lock").keyframe = true;
        self.ready.notify_one();
    }

    fn wait(&self, timeout: Duration) -> Wake {
        let state = self.state.lock().expect("mailbox lock");
        let (mut state, _) = self
            .ready
            .wait_timeout_while(state, timeout, |s| {
                s.frame.is_none() && !s.keyframe && !s.closed
            })
            .expect("mailbox lock");
        if state.closed {
            return Wake::Closed;
        }
        let key = std::mem::take(&mut state.keyframe);
        match state.frame.take() {
            Some(frame) => Wake::Frame(frame, key),
            None if key => Wake::Keyframe,
            None => Wake::Idle,
        }
    }
}

/// Where the compositor publishes frames.
#[derive(Default)]
pub struct FrameHub {
    mailboxes: Mutex<Vec<Arc<Mailbox>>>,
}

impl FrameHub {
    pub fn publish(&self, frame: Frame) {
        for mailbox in self.mailboxes.lock().expect("hub lock").iter() {
            mailbox.put(frame.clone());
        }
    }

    /// Whether anyone is encoding; if not, the compositor doesn't composite.
    pub fn has_listeners(&self) -> bool {
        !self.mailboxes.lock().expect("hub lock").is_empty()
    }

    fn attach(&self, mailbox: Arc<Mailbox>) {
        self.mailboxes.lock().expect("hub lock").push(mailbox);
    }
}

type Subscribers = Arc<Mutex<Vec<mpsc::Sender<EncodedFrame>>>>;

struct EncoderThread {
    mailbox: Arc<Mailbox>,
    subscribers: Subscribers,
}

#[derive(Clone, Copy, Debug)]
pub struct EncodeSettings {
    pub fps: u32,
    pub bitrate_bps: u32,
}

pub struct Media {
    hub: Arc<FrameHub>,
    compositor: channel::Sender<Command>,
    cuda: Arc<CudaContext>,
    settings: EncodeSettings,
    codecs: Vec<Codec>,
    encoders: Mutex<HashMap<Codec, EncoderThread>>,
    size: Mutex<(u32, u32)>,
}

impl Media {
    pub fn new(
        hub: Arc<FrameHub>,
        compositor: channel::Sender<Command>,
        cuda: Arc<CudaContext>,
        settings: EncodeSettings,
        codecs: Vec<Codec>,
        size: (u32, u32),
    ) -> Self {
        Self {
            hub,
            compositor,
            cuda,
            settings,
            codecs,
            encoders: Mutex::default(),
            size: Mutex::new(size),
        }
    }

    pub fn codecs(&self) -> &[Codec] {
        &self.codecs
    }

    /// The output's current size.
    pub fn size(&self) -> (u32, u32) {
        *self.size.lock().expect("size lock")
    }

    /// Frames of `codec` from now on, starting with a keyframe.
    pub fn subscribe(&self, codec: Codec) -> Result<mpsc::Receiver<EncodedFrame>> {
        anyhow::ensure!(
            self.codecs.contains(&codec),
            "{} isn't offered here",
            codec.name()
        );
        let (tx, rx) = mpsc::channel(8);
        let mut encoders = self.encoders.lock().expect("encoders lock");
        let encoder = encoders
            .entry(codec)
            .or_insert_with(|| self.start_encoder(codec));
        encoder
            .subscribers
            .lock()
            .expect("subscribers lock")
            .push(tx);
        encoder.mailbox.request_keyframe();
        let _ = self.compositor.send(Command::ForceFrame);
        Ok(rx)
    }

    pub fn request_keyframe(&self, codec: Codec) {
        if let Some(encoder) = self.encoders.lock().expect("encoders lock").get(&codec) {
            encoder.mailbox.request_keyframe();
        }
    }

    /// Resizes the output (and the app's window); returns the size applied.
    pub fn resize(&self, width: u32, height: u32) -> (u32, u32) {
        let size = fit_size(width, height);
        *self.size.lock().expect("size lock") = size;
        let _ = self.compositor.send(Command::Resize {
            width: size.0,
            height: size.1,
        });
        size
    }

    pub fn input(&self, input: Input) {
        let _ = self.compositor.send(Command::Input(input));
    }

    fn start_encoder(&self, codec: Codec) -> EncoderThread {
        let mailbox = Arc::new(Mailbox::default());
        let subscribers: Subscribers = Arc::default();
        let worker = EncoderWorker {
            codec,
            cuda: Arc::clone(&self.cuda),
            settings: self.settings,
            mailbox: Arc::clone(&mailbox),
            subscribers: Arc::clone(&subscribers),
        };
        std::thread::Builder::new()
            .name(format!("encode-{}", codec.name()))
            .spawn(move || worker.run())
            .expect("spawning an encoder thread");
        self.hub.attach(Arc::clone(&mailbox));
        EncoderThread {
            mailbox,
            subscribers,
        }
    }
}

struct EncoderWorker {
    codec: Codec,
    cuda: Arc<CudaContext>,
    settings: EncodeSettings,
    mailbox: Arc<Mailbox>,
    subscribers: Subscribers,
}

/// Per-codec encoder counters, logged every few seconds.
#[derive(Default)]
struct EncodeStats {
    frames: u64,
    bytes: u64,
    /// Composited → the encoder picked the frame up.
    queue_us: Vec<u64>,
    /// The CUDA view of the buffer.
    surface_us: Vec<u64>,
    map_us: Vec<u64>,
    submit_us: Vec<u64>,
    wait_us: Vec<u64>,
    encode_us: Vec<u64>,
    last_log: Option<Instant>,
}

fn percentile(values: &mut [u64], q: f64) -> Option<u64> {
    values.sort_unstable();
    values
        .get(((values.len() as f64 * q) as usize).min(values.len().saturating_sub(1)))
        .copied()
}

impl EncoderWorker {
    fn run(self) {
        let mut encoder: Option<Encoder> = None;
        let mut generation = None;
        let mut last: Option<Frame> = None;
        let mut out = Vec::with_capacity(1 << 20);
        let mut stats = EncodeStats::default();
        let mut keyframe_pending = true;
        loop {
            let (frame, key) = match self.mailbox.wait(Duration::from_millis(100)) {
                Wake::Closed => return,
                Wake::Frame(frame, key) => (frame, key || keyframe_pending),
                // Nothing changed on screen: re-encode the last frame as a keyframe.
                Wake::Keyframe => match last.clone() {
                    Some(frame) => (frame, true),
                    None => {
                        keyframe_pending = true;
                        continue;
                    }
                },
                Wake::Idle => continue,
            };
            last = Some(frame.clone());
            if !self.has_subscribers() {
                // Keep the latest frame for whoever subscribes next.
                keyframe_pending = true;
                continue;
            }
            keyframe_pending = false;

            let started = Instant::now();
            let result = self.encode(
                &mut encoder,
                &mut generation,
                &frame,
                key,
                &mut out,
                &mut stats,
            );
            let encoded = Instant::now();
            let key = match result {
                Ok(key) => key,
                Err(err) => {
                    warn!(codec = self.codec.name(), "encoding failed: {err:#}");
                    encoder = None;
                    keyframe_pending = true;
                    continue;
                }
            };
            stats.frames += 1;
            stats.bytes += out.len() as u64;
            stats
                .queue_us
                .push(started.duration_since(frame.rendered).as_micros() as u64);
            stats
                .encode_us
                .push(encoded.duration_since(started).as_micros() as u64);
            if let Some(encoder) = &encoder {
                let t = encoder.last_timings();
                stats.map_us.push(t.map.as_micros() as u64);
                stats.submit_us.push(t.submit.as_micros() as u64);
                stats.wait_us.push(t.wait.as_micros() as u64);
            }
            self.deliver(EncodedFrame {
                data: Bytes::copy_from_slice(&out),
                key,
                composited: frame.rendered,
                encoded,
            });
            self.log(&mut stats);
        }
    }

    fn encode(
        &self,
        encoder: &mut Option<Encoder>,
        generation: &mut Option<u64>,
        frame: &Frame,
        key: bool,
        out: &mut Vec<u8>,
        stats: &mut EncodeStats,
    ) -> Result<bool> {
        let encoder = match encoder {
            Some(encoder) => encoder,
            None => {
                let started = Instant::now();
                let created = Encoder::new(
                    Arc::clone(&self.cuda),
                    EncoderConfig {
                        codec: self.codec,
                        input: cha_nvenc::InputFormat::Argb,
                        width: frame.width,
                        height: frame.height,
                        max_width: MAX_SIZE.0,
                        max_height: MAX_SIZE.1,
                        fps: self.settings.fps,
                        bitrate_bps: self.settings.bitrate_bps,
                    },
                )
                .map_err(|e| anyhow::anyhow!("{e}"))?;
                info!(
                    codec = self.codec.name(),
                    width = frame.width,
                    height = frame.height,
                    init_ms = started.elapsed().as_millis() as u64,
                    "encoder ready"
                );
                *generation = Some(frame.generation);
                encoder.insert(created)
            }
        };
        if *generation != Some(frame.generation) {
            // New output buffers: drop registrations of the old ones.
            encoder.forget_surfaces();
            *generation = Some(frame.generation);
        }
        let mut key = key;
        if (encoder.config().width, encoder.config().height) != (frame.width, frame.height) {
            encoder
                .resize(frame.width, frame.height)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            key = true;
        }
        let surface_started = Instant::now();
        let (surface, _, _) = frame
            .slot
            .image
            .surface()
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        stats
            .surface_us
            .push(surface_started.elapsed().as_micros() as u64);
        encoder
            .encode(surface, key, out)
            .map_err(|e| anyhow::anyhow!("{e}"))
    }

    fn has_subscribers(&self) -> bool {
        let mut subscribers = self.subscribers.lock().expect("subscribers lock");
        subscribers.retain(|s| !s.is_closed());
        !subscribers.is_empty()
    }

    fn deliver(&self, frame: EncodedFrame) {
        let mut subscribers = self.subscribers.lock().expect("subscribers lock");
        let Some((last, rest)) = subscribers.split_last() else {
            return;
        };
        for subscriber in rest {
            let _ = subscriber.try_send(EncodedFrame {
                data: frame.data.clone(),
                ..frame
            });
        }
        let _ = last.try_send(frame);
        subscribers.retain(|s| !s.is_closed());
    }

    fn log(&self, stats: &mut EncodeStats) {
        let now = Instant::now();
        let last = *stats.last_log.get_or_insert(now);
        if now.duration_since(last) < Duration::from_secs(10) {
            return;
        }
        let secs = now.duration_since(last).as_secs_f64();
        let skipped = std::mem::take(&mut self.mailbox.state.lock().expect("mailbox lock").skipped);
        let p = |v: &mut Vec<u64>| {
            format!(
                "{}/{}",
                percentile(v, 0.5).unwrap_or(0),
                percentile(v, 0.99).unwrap_or(0)
            )
        };
        info!(
            codec = self.codec.name(),
            fps = format!("{:.1}", stats.frames as f64 / secs),
            mbps = format!("{:.1}", stats.bytes as f64 * 8.0 / secs / 1e6),
            skipped,
            "encoder µs p50/p99: queue {} surface {} map {} submit {} wait {} total {}",
            p(&mut stats.queue_us),
            p(&mut stats.surface_us),
            p(&mut stats.map_us),
            p(&mut stats.submit_us),
            p(&mut stats.wait_us),
            p(&mut stats.encode_us),
        );
        *stats = EncodeStats {
            last_log: Some(now),
            ..EncodeStats::default()
        };
    }
}

//! From composited frames to encoded ones.
//!
//! The compositor publishes each frame to a [`FrameHub`]; every active codec has
//! an encoder thread with a one-frame mailbox (newest frame wins, so a slow
//! encoder skips frames instead of queueing them). Encoded frames go to that
//! codec's subscribers, the sessions.
//!
//! Each subscriber paces its codec (P2.5, plan §3.1 rules 1 and 2): the
//! encoder runs at the lowest bitrate its subscribers' links take, changed in
//! place, and skips a frame while any subscriber's send queue is backed up.
//! A frame that isn't encoded never has to be dropped, so nothing that later
//! frames depend on goes missing.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use bytes::Bytes;
use cha_nvenc::Codec;
use smithay::reexports::calloop::channel;
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::codec::VideoCodec;
use crate::compositor::{
    CLIPBOARD_MAX_BYTES, ClipboardWatch, Command, CursorWatch, Handle, PointerWatch, Slot, fit_size,
};
use crate::device::Device;
use crate::encoder::{Params, VideoEncoder};
use crate::framerate;
use crate::input::Input;
use crate::overlay::{Level, Overlay};
use crate::pyro::{PyroSettings, PyroWorker};
use crate::status::{SetupStatus, StatusWatch};
use crate::x11_clipboard::X11Clipboard;

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

/// One encoded access unit (Annex-B for H.264/HEVC, OBUs for AV1, a
/// PyroWave frame).
pub struct EncodedFrame {
    pub data: Bytes,
    /// PyroWave: the packets `data` splits into, one datagram each. Empty
    /// for the hardware codecs (any fragmentation does).
    pub packets: Vec<cha_pyrowave::Packet>,
    pub key: bool,
    /// The encoder's index for it (what reference invalidation names).
    pub index: u64,
    /// Encoded after a reference invalidation: it refers only to frames
    /// before this index, so a page that lost none of those resumes here.
    pub recovery: Option<u64>,
    /// When the composited frame was ready.
    pub composited: Instant,
    /// When encoding finished.
    pub encoded: Instant,
}

#[derive(Default)]
struct MailboxState {
    frame: Option<Frame>,
    keyframe: bool,
    /// Frames from this index on were lost (reference invalidation asked).
    invalidate: Option<u64>,
    closed: bool,
    /// Frames replaced before the encoder took them.
    skipped: u64,
}

#[derive(Default)]
pub struct Mailbox {
    state: Mutex<MailboxState>,
    ready: Condvar,
}

pub enum Wake {
    Frame(Frame, bool),
    Keyframe,
    /// Nothing new on screen, but frames from this index on were lost:
    /// encode the last one again, referring to what came before them.
    Refresh(u64),
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

    fn request_invalidate(&self, from: u64) {
        let mut state = self.state.lock().expect("mailbox lock");
        state.invalidate = Some(state.invalidate.map_or(from, |at| at.min(from)));
        self.ready.notify_one();
    }

    /// Frames lost since the last look (with a `Frame` or `Keyframe` wake).
    fn take_invalidate(&self) -> Option<u64> {
        self.state.lock().expect("mailbox lock").invalidate.take()
    }

    pub fn wait(&self, timeout: Duration) -> Wake {
        let state = self.state.lock().expect("mailbox lock");
        let (mut state, _) = self
            .ready
            .wait_timeout_while(state, timeout, |s| {
                s.frame.is_none() && !s.keyframe && s.invalidate.is_none() && !s.closed
            })
            .expect("mailbox lock");
        if state.closed {
            return Wake::Closed;
        }
        let key = std::mem::take(&mut state.keyframe);
        match state.frame.take() {
            Some(frame) => Wake::Frame(frame, key),
            None if key => Wake::Keyframe,
            None => match state.invalidate.take() {
                Some(from) => Wake::Refresh(from),
                None => Wake::Idle,
            },
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

    fn detach(&self, mailbox: &Arc<Mailbox>) {
        self.mailboxes
            .lock()
            .expect("hub lock")
            .retain(|m| !Arc::ptr_eq(m, mailbox));
    }
}

/// How a subscriber wants its codec's frames.
#[derive(Debug)]
pub struct Pace {
    /// The bitrate its link takes now.
    target_bps: AtomicU32,
    /// Its send queue is backed up: skip encoding until it drains.
    hold: AtomicBool,
    /// Since when it has held, if it does (ms after `epoch`).
    hold_since_ms: AtomicU64,
    epoch: Instant,
}

/// A subscriber holding longer than this is stuck (a viewer that left
/// without closing, a dead link): the others don't wait for it, nor run at
/// its rate; it drops frames and resyncs on its own.
const STUCK: Duration = Duration::from_secs(1);

impl Pace {
    pub fn new(target_bps: u32) -> Arc<Self> {
        Arc::new(Self {
            target_bps: AtomicU32::new(target_bps),
            hold: AtomicBool::new(false),
            hold_since_ms: AtomicU64::new(0),
            epoch: Instant::now(),
        })
    }

    fn now_ms(&self) -> u64 {
        self.epoch.elapsed().as_millis() as u64
    }

    /// Holding for longer than `STUCK`.
    fn stuck(&self) -> bool {
        self.held()
            && self.now_ms() - self.hold_since_ms.load(Ordering::Relaxed) > STUCK.as_millis() as u64
    }

    pub fn set_target(&self, bps: u32) {
        self.target_bps.store(bps, Ordering::Relaxed);
    }

    pub fn target(&self) -> u32 {
        self.target_bps.load(Ordering::Relaxed)
    }

    pub fn set_hold(&self, hold: bool) {
        if hold && !self.hold.swap(true, Ordering::Relaxed) {
            self.hold_since_ms.store(self.now_ms(), Ordering::Relaxed);
        } else if !hold {
            self.hold.store(false, Ordering::Relaxed);
        }
    }

    fn held(&self) -> bool {
        self.hold.load(Ordering::Relaxed)
    }
}

pub struct Subscriber {
    tx: mpsc::Sender<EncodedFrame>,
    pace: Arc<Pace>,
}

pub type Subscribers = Arc<Mutex<Vec<Subscriber>>>;

/// A codec's frames for one session, and how it paces them.
pub struct Subscription {
    pub frames: mpsc::Receiver<EncodedFrame>,
    pub pace: Arc<Pace>,
}

/// The pace an encoder keeps for its subscribers: hold if any is backed up,
/// at the lowest rate any takes; stuck ones don't count. `None` with no
/// subscribers left.
pub fn pace_of(subscribers: &Subscribers) -> Option<(bool, u32)> {
    let mut subscribers = subscribers.lock().expect("subscribers lock");
    subscribers.retain(|s| !s.tx.is_closed());
    if subscribers.is_empty() {
        return None;
    }
    let live = || subscribers.iter().filter(|s| !s.pace.stuck());
    let held = live().any(|s| s.pace.held());
    let target = live()
        .map(|s| s.pace.target())
        .min()
        .or_else(|| subscribers.iter().map(|s| s.pace.target()).max())?;
    Some((held, target))
}

/// Hands `frame` to every subscriber that's still there.
pub fn deliver(subscribers: &Subscribers, frame: EncodedFrame) {
    let mut subscribers = subscribers.lock().expect("subscribers lock");
    for subscriber in subscribers.iter() {
        let _ = subscriber.tx.try_send(EncodedFrame {
            data: frame.data.clone(),
            packets: frame.packets.clone(),
            ..frame
        });
    }
    subscribers.retain(|s| !s.tx.is_closed());
}

struct EncoderThread {
    mailbox: Arc<Mailbox>,
    subscribers: Subscribers,
    thread: std::thread::JoinHandle<()>,
}

/// What the streamer starts with.
#[derive(Clone, Copy, Debug)]
pub struct EncodeSettings {
    /// The frame rate at the start; a page can change it.
    pub fps: u32,
    /// The NVENC codecs' bitrate at 60 fps; at another rate it is scaled
    /// (`framerate::nvenc_bps`).
    pub bitrate_bps: u32,
}

pub struct Media {
    hub: Arc<FrameHub>,
    compositor: channel::Sender<Command>,
    device: Arc<Device>,
    settings: EncodeSettings,
    /// The current frame rate: the compositor, every encoder and every
    /// session follow it.
    fps: Arc<AtomicU32>,
    codecs: Vec<VideoCodec>,
    /// PyroWave's, if it's offered.
    pyrowave: Option<PyroSettings>,
    encoders: Mutex<HashMap<VideoCodec, EncoderThread>>,
    size: Mutex<(u32, u32)>,
    clipboard: ClipboardWatch,
    /// The bridge to X11 apps' clipboard, if there's one to start.
    x11_clipboard: Option<Arc<X11Clipboard>>,
    cursor: CursorWatch,
    pointer: PointerWatch,
    /// The app's setup status, if there's a file to follow.
    setup_status: SetupStatus,
    /// The app's performance overlay config, if the app has one.
    overlay: Overlay,
}

impl Media {
    pub fn new(
        hub: Arc<FrameHub>,
        compositor: &Handle,
        device: Arc<Device>,
        settings: EncodeSettings,
        codecs: Vec<VideoCodec>,
        pyrowave: Option<PyroSettings>,
        size: (u32, u32),
    ) -> Self {
        let fps = Arc::new(AtomicU32::new(settings.fps));
        Self {
            hub,
            compositor: compositor.commands.clone(),
            device,
            settings,
            pyrowave: pyrowave.map(|p| p.following(Arc::clone(&fps))),
            fps,
            codecs,
            encoders: Mutex::default(),
            size: Mutex::new(size),
            clipboard: compositor.clipboard.clone(),
            x11_clipboard: None,
            cursor: compositor.cursor.clone(),
            pointer: compositor.pointer.clone(),
            setup_status: SetupStatus::default(),
            overlay: Overlay::default(),
        }
    }

    /// Also tells the pages what the app's long setup is doing (Steam's
    /// download), from the file it writes.
    pub fn with_setup_status(mut self, status: SetupStatus) -> Self {
        self.setup_status = status;
        self
    }

    /// Also lets the pages set the app's performance overlay, through its
    /// config file (see `overlay`).
    pub fn with_overlay(mut self, overlay: Overlay) -> Self {
        self.overlay = overlay;
        self
    }

    /// Also hands the browser's clipboard to X11 apps (XFCE) through `bridge`.
    pub fn with_x11_clipboard(mut self, bridge: Option<Arc<X11Clipboard>>) -> Self {
        self.x11_clipboard = bridge;
        self
    }

    pub fn codecs(&self) -> &[VideoCodec] {
        &self.codecs
    }

    /// The current frame rate.
    pub fn fps(&self) -> u32 {
        self.fps.load(Ordering::Relaxed)
    }

    /// The session's ceiling: what rate control starts from and climbs to,
    /// for the NVENC codecs, at the current frame rate.
    pub fn bitrate_bps(&self) -> u32 {
        framerate::nvenc_bps(self.settings.bitrate_bps, self.fps())
    }

    /// Changes the frame rate (one of `framerate::CHOICES`) for everyone
    /// watching: the compositor composites at it, the encoders are
    /// reconfigured in place on their next frame (no gap, no keyframe), and
    /// the sessions rescale their rate control when they see it
    /// (`RateControl::rescale`). Returns the rate now.
    pub fn set_fps(&self, fps: u32) -> Result<u32, String> {
        let fps = framerate::validate(fps)?;
        let before = self.fps.swap(fps, Ordering::Relaxed);
        if before != fps {
            info!(from = before, to = fps, "frame rate");
            let _ = self.compositor.send(Command::SetFps(fps));
        }
        Ok(fps)
    }

    /// The output's current size.
    pub fn size(&self) -> (u32, u32) {
        *self.size.lock().expect("size lock")
    }

    /// Frames of `codec` from now on, starting with a keyframe, at the
    /// configured bitrate until the session paces them.
    pub fn subscribe(&self, codec: VideoCodec) -> Result<Subscription> {
        anyhow::ensure!(
            self.codecs.contains(&codec),
            "{} isn't offered here",
            codec.name()
        );
        let (tx, rx) = mpsc::channel(8);
        let pace = Pace::new(self.bitrate_bps());
        let mut encoders = self.encoders.lock().expect("encoders lock");
        // An encoder that gave up (it logged why) gets another go.
        if let Some(dead) = encoders.remove(&codec) {
            if dead.thread.is_finished() {
                self.hub.detach(&dead.mailbox);
            } else {
                encoders.insert(codec, dead);
            }
        }
        let encoder = encoders
            .entry(codec)
            .or_insert_with(|| self.start_encoder(codec));
        encoder
            .subscribers
            .lock()
            .expect("subscribers lock")
            .push(Subscriber {
                tx,
                pace: Arc::clone(&pace),
            });
        encoder.mailbox.request_keyframe();
        let _ = self.compositor.send(Command::ForceFrame);
        Ok(Subscription { frames: rx, pace })
    }

    pub fn request_keyframe(&self, codec: VideoCodec) {
        if let Some(encoder) = self.encoders.lock().expect("encoders lock").get(&codec) {
            encoder.mailbox.request_keyframe();
        }
    }

    /// Frames from encoder index `from` on were lost on the way: the next
    /// frame refers only to older ones (flagged `recovery`), or is a
    /// keyframe where that can't be.
    pub fn request_invalidate(&self, codec: VideoCodec, from: u64) {
        if let Some(encoder) = self.encoders.lock().expect("encoders lock").get(&codec) {
            encoder.mailbox.request_invalidate(from);
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

    /// What apps copy from now on (the current text counts as seen).
    pub fn clipboard(&self) -> ClipboardWatch {
        let mut watch = self.clipboard.clone();
        watch.mark_unchanged();
        watch
    }

    /// The cursor's shape, from now on (the current one counts as new).
    pub fn cursor(&self) -> CursorWatch {
        let mut watch = self.cursor.clone();
        watch.mark_changed();
        watch
    }

    /// The app's setup status, from now on (the current one counts as new).
    /// The overlay level now (None: this app has no overlay).
    pub fn overlay(&self) -> Option<Level> {
        self.overlay.level()
    }

    /// Sets the overlay level; refused when the app has no overlay.
    pub fn set_overlay(&self, level: u8) -> Result<u8, String> {
        self.overlay.set(level)
    }

    pub fn status(&self) -> StatusWatch {
        self.setup_status.subscribe()
    }

    /// Where the pointer is, from now on (the current spot counts as new).
    pub fn pointer(&self) -> PointerWatch {
        let mut watch = self.pointer.clone();
        watch.mark_changed();
        watch
    }

    /// Whether the page draws the cursor (desktop mode) or the picture has it.
    pub fn set_client_cursor(&self, on: bool) {
        let _ = self.compositor.send(Command::ClientCursor(on));
    }

    /// The browser's clipboard, for apps to paste; false if it's too big.
    pub fn set_clipboard(&self, text: &str) -> bool {
        if text.len() > CLIPBOARD_MAX_BYTES {
            return false;
        }
        let _ = self.compositor.send(Command::SetClipboard(text.into()));
        // X11 apps: wait until the helper owns the X clipboard, so the paste
        // keys after this text find it. The session's task waits, never the
        // compositor.
        if let Some(bridge) = &self.x11_clipboard {
            bridge.set(text);
        }
        true
    }

    pub fn input(&self, input: Input) {
        let _ = self.compositor.send(Command::Input(input));
    }

    fn start_encoder(&self, codec: VideoCodec) -> EncoderThread {
        let mailbox = Arc::new(Mailbox::default());
        let subscribers: Subscribers = Arc::default();
        let builder = std::thread::Builder::new().name(format!("encode-{}", codec.name()));
        let thread = match (codec, self.pyrowave.clone()) {
            (VideoCodec::Hw(codec), _) => {
                let worker = EncoderWorker {
                    codec,
                    device: Arc::clone(&self.device),
                    fps: Arc::clone(&self.fps),
                    mailbox: Arc::clone(&mailbox),
                    subscribers: Arc::clone(&subscribers),
                };
                builder.spawn(move || worker.run())
            }
            (VideoCodec::PyroWave(chroma), Some(settings)) => {
                let worker = PyroWorker {
                    chroma,
                    settings,
                    mailbox: Arc::clone(&mailbox),
                    subscribers: Arc::clone(&subscribers),
                };
                builder.spawn(move || worker.run())
            }
            // Not offered: subscribe() refused it already.
            (VideoCodec::PyroWave(_), None) => builder.spawn(|| ()),
        }
        .expect("spawning an encoder thread");
        self.hub.attach(Arc::clone(&mailbox));
        EncoderThread {
            mailbox,
            subscribers,
            thread,
        }
    }
}

struct EncoderWorker {
    codec: Codec,
    device: Arc<Device>,
    fps: Arc<AtomicU32>,
    mailbox: Arc<Mailbox>,
    subscribers: Subscribers,
}

/// Per-codec encoder counters, logged every few seconds.
#[derive(Default)]
struct EncodeStats {
    frames: u64,
    bytes: u64,
    /// Frames not encoded while a subscriber's queue drained.
    held: u64,
    rate_changes: u64,
    /// Frames encoded around lost ones (reference invalidation), and the
    /// keyframes sent where that couldn't be.
    recoveries: u64,
    rfi_keyframes: u64,
    /// Composited → the encoder picked the frame up.
    queue_us: Vec<u64>,
    map_us: Vec<u64>,
    submit_us: Vec<u64>,
    wait_us: Vec<u64>,
    encode_us: Vec<u64>,
    last_log: Option<Instant>,
}

/// What the next encode is asked to be.
#[derive(Clone, Copy)]
struct Want {
    key: bool,
    /// Frames from this encoder index on were lost: refer around them.
    invalidate: Option<u64>,
}

/// Spike S8: `CHA_DUMP_RFI=<dir>,<at>,<lost>` writes the first frames each
/// encoder makes to `<dir>/<codec>/` (`NNNNN.bin`, and `index.jsonl`), as
/// if frames `at` to `at + lost - 1` were lost: the frame after them refers
/// around them. A page then decodes the frames with and without the lost
/// ones and compares.
struct RfiDump {
    dir: std::path::PathBuf,
    at: u64,
    lost: u64,
    index: std::fs::File,
}

impl RfiDump {
    fn from_env(codec: Codec) -> Option<Self> {
        let spec = std::env::var("CHA_DUMP_RFI").ok()?;
        let mut parts = spec.split(',');
        let dir = std::path::Path::new(parts.next()?).join(codec.name());
        let at = parts.next()?.parse().ok()?;
        let lost = parts.next()?.parse().ok()?;
        std::fs::create_dir_all(&dir).ok()?;
        let index = std::fs::File::create(dir.join("index.jsonl")).ok()?;
        info!(dir = %dir.display(), at, lost, "dumping frames (spike S8)");
        Some(Self {
            dir,
            at,
            lost,
            index,
        })
    }

    /// The loss to refer around before encoding frame `next`.
    fn loss_before(&self, next: u64) -> Option<u64> {
        (next == self.at + self.lost).then_some(self.at)
    }

    fn write(&mut self, index: u64, key: bool, recovery: bool, data: &[u8]) {
        use std::io::Write;
        if index >= self.at + self.lost + 60 {
            return;
        }
        let lost = (self.at..self.at + self.lost).contains(&index);
        let _ = std::fs::write(self.dir.join(format!("{index:05}.bin")), data);
        let _ = writeln!(
            self.index,
            r#"{{"index":{index},"key":{key},"recovery":{recovery},"lost":{lost}}}"#
        );
    }
}

fn percentile(values: &mut [u64], q: f64) -> Option<u64> {
    values.sort_unstable();
    values
        .get(((values.len() as f64 * q) as usize).min(values.len().saturating_sub(1)))
        .copied()
}

impl EncoderWorker {
    fn run(self) {
        // The encoder, and the output buffers' generation it registered.
        let mut encoder: Option<(Box<dyn VideoEncoder>, u64)> = None;
        let mut last: Option<Frame> = None;
        let mut out = Vec::with_capacity(1 << 20);
        let mut stats = EncodeStats::default();
        let mut keyframe_pending = true;
        // Lost frames to refer around at the next encode (from this index on).
        let mut invalidate: Option<u64> = None;
        let mut dump = RfiDump::from_env(self.codec);
        let mut last_encode: Option<Instant> = None;
        loop {
            // The frame interval follows the rate: a re-encode (a keyframe
            // or recovery asked for with nothing new on screen) comes at most
            // once per frame.
            let interval = framerate::interval(self.fps.load(Ordering::Relaxed));
            let (frame, key, reencode) = match self.mailbox.wait(Duration::from_millis(100)) {
                Wake::Closed => return,
                Wake::Frame(frame, key) => (frame, key || keyframe_pending, false),
                // Nothing new on screen, but frames were lost: encode the
                // last one again around them (at most once a frame interval).
                Wake::Refresh(from) => {
                    invalidate = Some(invalidate.map_or(from, |at| at.min(from)));
                    match last.clone() {
                        Some(frame) if last_encode.is_none_or(|at| at.elapsed() >= interval) => {
                            (frame, keyframe_pending, true)
                        }
                        _ => continue,
                    }
                }
                // Nothing changed on screen: re-encode the last frame as a
                // keyframe, at most once a frame interval (requests can come
                // from every dropped frame of a congested session).
                Wake::Keyframe => match last.clone() {
                    Some(frame) if last_encode.is_none_or(|at| at.elapsed() >= interval) => {
                        (frame, true, true)
                    }
                    _ => {
                        keyframe_pending = true;
                        continue;
                    }
                },
                Wake::Idle => continue,
            };
            if let Some(from) = self.mailbox.take_invalidate() {
                invalidate = Some(invalidate.map_or(from, |at| at.min(from)));
            }
            last = Some(frame.clone());
            let Some((held, target_bps)) = pace_of(&self.subscribers) else {
                // Keep the latest frame for whoever subscribes next.
                keyframe_pending = true;
                continue;
            };
            if held {
                // A subscriber's queue is backed up: this frame isn't encoded,
                // so the next one depends only on frames that went out.
                keyframe_pending |= key;
                stats.held += 1;
                continue;
            }
            keyframe_pending = false;
            last_encode = Some(Instant::now());

            if let Some(d) = &dump {
                let next = encoder.as_ref().map_or(0, |(e, _)| e.next_index());
                invalidate = d.loss_before(next).or(invalidate);
            }
            let started = Instant::now();
            let want = Want {
                key,
                invalidate: invalidate.take(),
            };
            let result = self.encode(&mut encoder, &frame, want, target_bps, &mut out, &mut stats);
            let encoded = Instant::now();
            let (key, recovery): (bool, Option<u64>) = match result {
                Ok(done) => done,
                Err(err) => {
                    warn!(codec = self.codec.name(), "encoding failed: {err:#}");
                    encoder = None;
                    keyframe_pending = true;
                    continue;
                }
            };
            stats.frames += 1;
            stats.bytes += out.len() as u64;
            // A re-encoded frame was composited long ago; it isn't queueing.
            let composited = if reencode { started } else { frame.rendered };
            stats
                .queue_us
                .push(started.duration_since(composited).as_micros() as u64);
            stats
                .encode_us
                .push(encoded.duration_since(started).as_micros() as u64);
            if let Some((encoder, _)) = &encoder {
                let t = encoder.last_timings();
                stats.map_us.push(t.map.as_micros() as u64);
                stats.submit_us.push(t.submit.as_micros() as u64);
                stats.wait_us.push(t.wait.as_micros() as u64);
            }
            let index = encoder.as_ref().map_or(0, |(e, _)| e.last_index());
            if let Some(d) = &mut dump {
                d.write(index, key, recovery.is_some(), &out);
            }
            deliver(
                &self.subscribers,
                EncodedFrame {
                    data: Bytes::copy_from_slice(&out),
                    packets: Vec::new(),
                    key,
                    index,
                    recovery,
                    composited,
                    encoded,
                },
            );
            self.log(&mut stats);
        }
    }

    fn encode(
        &self,
        encoder: &mut Option<(Box<dyn VideoEncoder>, u64)>,
        frame: &Frame,
        want: Want,
        target_bps: u32,
        out: &mut Vec<u8>,
        stats: &mut EncodeStats,
    ) -> Result<(bool, Option<u64>)> {
        let fps = self.fps.load(Ordering::Relaxed);
        let (encoder, generation) = match encoder {
            Some((encoder, generation)) => (encoder, generation),
            None => {
                let started = Instant::now();
                let created = self.device.encoder(Params {
                    codec: self.codec,
                    width: frame.width,
                    height: frame.height,
                    fps,
                    bitrate_bps: target_bps,
                })?;
                info!(
                    codec = self.codec.name(),
                    width = frame.width,
                    height = frame.height,
                    init_ms = started.elapsed().as_millis() as u64,
                    "encoder ready"
                );
                let (encoder, generation) = encoder.insert((created, frame.generation));
                (encoder, generation)
            }
        };
        if *generation != frame.generation {
            // New output buffers: drop registrations of the old ones.
            encoder.forget_surfaces();
            *generation = frame.generation;
        }
        let mut key = want.key;
        // Follow the frame rate (with the rate asked for: its VBV and timing
        // are per frame), and the subscribers' bitrate; small wobbles aren't
        // worth a change.
        let current = encoder.params().bitrate_bps;
        if encoder.params().fps != fps {
            encoder.set_frame_rate(fps, target_bps)?;
            stats.rate_changes += 1;
        } else if target_bps.abs_diff(current) > current / 32 {
            encoder.set_bitrate(target_bps)?;
            stats.rate_changes += 1;
        }
        let params = encoder.params();
        if (params.width, params.height) != (frame.width, frame.height) {
            encoder.resize(frame.width, frame.height)?;
            key = true;
        }
        // Frames were lost: refer around them, or start over where that
        // can't be.
        let mut recovery = None;
        if let Some(from) = want.invalidate
            && !key
        {
            if encoder.invalidate_from(from)? {
                recovery = Some(from);
                stats.recoveries += 1;
            } else {
                key = true;
                stats.rfi_keyframes += 1;
            }
        }
        let key = encoder.encode(frame, key, out)?;
        Ok((key, recovery.filter(|_| !key)))
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
            held = stats.held,
            rate_changes = stats.rate_changes,
            recoveries = stats.recoveries,
            rfi_keyframes = stats.rfi_keyframes,
            "encoder µs p50/p99: queue {} map {} submit {} wait {} total {}",
            p(&mut stats.queue_us),
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

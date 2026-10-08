//! Drives a running [`cha_client::Session`]: video → decoder → the frame the
//! presenter picks up, audio → audio out, rumble → the pads. Each runs on its
//! own thread, so a slow decode never holds up audio.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cha_client::{Codec, Ended, Feedback, Session, SessionControl, VideoFrame};
use tokio::runtime::Handle;

use crate::audio::AudioOut;
use crate::input::pads::PadService;
use crate::ui::StatsSnapshot;
use crate::video::{self, DecodedFrame};

/// Don't ask the host for key frames more often than this while decoding
/// keeps failing.
const KEYFRAME_RETRY: Duration = Duration::from_millis(250);

/// Counters the threads bump and the UI reads. All monotonic.
#[derive(Default)]
pub struct Stats {
    pub decoded: AtomicU64,
    pub decode_us: AtomicU64,
    /// Decoded frames replaced by a newer one before being shown.
    pub dropped: AtomicU64,
    pub presented: AtomicU64,
    /// Of those, drawn offscreen because the window could not present (smoke runs).
    pub offscreen: AtomicU64,
    pub latency_us: AtomicU64,
    pub decode_errors: AtomicU64,
    /// PyroWave frames decoded from only the packets that arrived.
    pub partial: AtomicU64,
    pub audio_underruns: AtomicU64,
    pub audio_dropped_frames: AtomicU64,
}

/// What the video thread and the window share.
pub struct Shared {
    /// The newest decoded frame, if the window hasn't taken it yet: never more
    /// than one waits.
    pub latest: Mutex<Option<DecodedFrame>>,
    /// PyroWave: the newest raw frame, if the window hasn't taken it yet. It
    /// is decoded on the window's GPU device when drawn, not on a thread here.
    pub latest_pyro: Mutex<Option<VideoFrame>>,
    pub stats: Stats,
    /// What to show over the picture while the transport brings a dropped
    /// stream back (`Feedback::Reconnecting`), until it is back.
    pub reconnecting: Mutex<Option<String>>,
    /// Recent video arrival times (ms since the session began), for the AWDL check.
    arrivals: Mutex<VecDeque<u64>>,
    began: Instant,
    redraw_requested: AtomicBool,
    wake: Box<dyn Fn() + Send + Sync>,
}

impl Shared {
    fn new(wake: Box<dyn Fn() + Send + Sync>) -> Self {
        Self {
            latest: Mutex::new(None),
            latest_pyro: Mutex::new(None),
            stats: Stats::default(),
            reconnecting: Mutex::new(None),
            arrivals: Mutex::new(VecDeque::new()),
            began: Instant::now(),
            redraw_requested: AtomicBool::new(false),
            wake,
        }
    }

    /// Shows or clears the reconnecting note, and draws it at once (no frame
    /// is coming to wake the window).
    fn set_reconnecting(&self, note: Option<String>) {
        *self.reconnecting.lock().unwrap() = note;
        // The gaps of a drop aren't AWDL's.
        self.arrivals.lock().unwrap().clear();
        self.redraw_requested.store(true, Ordering::Release);
        (self.wake)();
    }

    /// Called after storing a frame; wakes the window once until it redraws.
    fn frame_ready(&self) {
        if !self.redraw_requested.swap(true, Ordering::AcqRel) {
            (self.wake)();
        }
    }

    fn note_arrival(&self, at: Instant) {
        let ms = at.saturating_duration_since(self.began).as_millis() as u64;
        let mut log = self.arrivals.lock().unwrap();
        log.push_back(ms);
        while log.front().is_some_and(|first| ms - first > 10_000) {
            log.pop_front();
        }
    }

    fn awdl_suspected(&self) -> bool {
        let log: Vec<u64> = self.arrivals.lock().unwrap().iter().copied().collect();
        // The timing alone isn't enough: only with AWDL actually up.
        crate::awdl::looks_like_awdl(&log) && crate::awdl::interface_active()
    }

    /// The window is about to draw: the next frame may wake it again.
    pub fn redraw_started(&self) {
        self.redraw_requested.store(false, Ordering::Release);
    }

    /// The window put a frame on screen.
    pub fn presented(&self, received: Instant) {
        let s = &self.stats;
        s.presented.fetch_add(1, Ordering::Relaxed);
        s.latency_us
            .fetch_add(received.elapsed().as_micros() as u64, Ordering::Relaxed);
    }
}

/// A started session, from the window's side.
pub struct Running {
    pub control: Arc<dyn SessionControl>,
    pub shared: Arc<Shared>,
    pub codec: Codec,
    pub width: u32,
    pub height: u32,
}

/// Start the threads for `session`. `wake` is called (from a thread) when a
/// frame is ready; `on_end` once when the session ends.
pub fn start(
    session: Session,
    runtime: &Handle,
    pads: Arc<PadService>,
    wake: impl Fn() + Send + Sync + 'static,
    on_end: impl FnOnce(Ended) + Send + 'static,
) -> anyhow::Result<Running> {
    let Session {
        video,
        audio,
        feedback,
        ended,
        codec,
        width,
        height,
        control,
    } = session;
    let control: Arc<dyn SessionControl> = Arc::from(control);
    let shared = Arc::new(Shared::new(Box::new(wake)));

    if codec.is_pyrowave() {
        // Decoded by the window on its own device; frames stand alone, so
        // nothing here ever asks for a keyframe.
        spawn("video", {
            let shared = shared.clone();
            move || pyrowave_loop(video, &shared)
        })?;
    } else {
        let decoder = video::new_decoder(codec)?;
        spawn("video", {
            let (shared, control) = (shared.clone(), control.clone());
            move || video_loop(video, decoder, &shared, &*control)
        })?;
    }
    spawn("audio", {
        let shared = shared.clone();
        move || audio_loop(audio, &shared)
    })?;
    spawn("feedback", {
        let pads = pads.clone();
        let shared = shared.clone();
        let mut feedback = feedback;
        move || {
            while let Some(f) = feedback.blocking_recv() {
                match f {
                    Feedback::Reconnecting { attempt, reason } => {
                        tracing::info!(attempt, %reason, "the stream dropped; reconnecting");
                        shared.set_reconnecting(Some(if attempt > 1 {
                            format!("Reconnecting… (try {attempt})")
                        } else {
                            "Reconnecting…".to_string()
                        }));
                    }
                    Feedback::Reconnected => shared.set_reconnecting(None),
                    f => pads.feedback(f),
                }
            }
        }
    })?;
    pads.attach(control.clone());

    runtime.spawn(async move {
        let ended = ended
            .await
            .unwrap_or_else(|_| Ended::Failed("the transport went away".into()));
        on_end(ended);
    });

    Ok(Running {
        control,
        shared,
        codec,
        width,
        height,
    })
}

fn spawn(name: &str, f: impl FnOnce() + Send + 'static) -> std::io::Result<()> {
    std::thread::Builder::new().name(name.into()).spawn(f)?;
    Ok(())
}

fn video_loop(
    mut video: tokio::sync::mpsc::Receiver<cha_client::VideoFrame>,
    mut decoder: Box<dyn video::VideoDecoder>,
    shared: &Shared,
    control: &dyn SessionControl,
) {
    let mut last_request: Option<Instant> = None;
    while let Some(frame) = video.blocking_recv() {
        shared.note_arrival(frame.received);
        match decoder.decode(frame) {
            Ok(Some(decoded)) => {
                let s = &shared.stats;
                s.decoded.fetch_add(1, Ordering::Relaxed);
                s.decode_us
                    .fetch_add((decoded.decode_ms * 1000.0) as u64, Ordering::Relaxed);
                let replaced = shared.latest.lock().unwrap().replace(decoded);
                if replaced.is_some() {
                    s.dropped.fetch_add(1, Ordering::Relaxed);
                }
                shared.frame_ready();
            }
            Ok(None) => {}
            Err(e) => {
                shared.stats.decode_errors.fetch_add(1, Ordering::Relaxed);
                tracing::debug!("decode: {e:#}");
                if last_request.is_none_or(|t| t.elapsed() >= KEYFRAME_RETRY) {
                    last_request = Some(Instant::now());
                    control.request_keyframe();
                }
            }
        }
    }
}

/// PyroWave: keep only the newest frame for the window to decode. A frame
/// replaced before it was taken is dropped, as for the other codecs.
fn pyrowave_loop(mut video: tokio::sync::mpsc::Receiver<VideoFrame>, shared: &Shared) {
    while let Some(frame) = video.blocking_recv() {
        shared.note_arrival(frame.received);
        let replaced = shared.latest_pyro.lock().unwrap().replace(frame);
        if replaced.is_some() {
            shared.stats.dropped.fetch_add(1, Ordering::Relaxed);
        }
        shared.frame_ready();
    }
}

fn audio_loop(mut audio: tokio::sync::mpsc::Receiver<cha_client::AudioPacket>, shared: &Shared) {
    let mut out: Option<AudioOut> = None;
    let mut failed = false;
    let (mut seen_underruns, mut seen_dropped) = (0, 0);
    while let Some(packet) = audio.blocking_recv() {
        if failed {
            continue;
        }
        if out.is_none() {
            match AudioOut::open(packet.sample_rate, packet.channels) {
                Ok(o) => out = Some(o),
                Err(e) => {
                    // Video keeps going without sound.
                    tracing::warn!("no audio: {e:#}");
                    failed = true;
                    continue;
                }
            }
        }
        let Some(o) = out.as_mut() else { continue };
        if let Err(e) = o.play(&packet) {
            tracing::debug!("audio packet: {e:#}");
            continue;
        }
        let stats = o.stats();
        let s = &shared.stats;
        s.audio_underruns
            .fetch_add(stats.underruns - seen_underruns, Ordering::Relaxed);
        s.audio_dropped_frames
            .fetch_add(stats.dropped_frames - seen_dropped, Ordering::Relaxed);
        (seen_underruns, seen_dropped) = (stats.underruns, stats.dropped_frames);
    }
}

/// Turns the monotonic counters into per-second readings for the overlay.
pub struct Rates {
    since: Instant,
    last: Totals,
    shown: StatsSnapshot,
}

#[derive(Clone, Copy, Default)]
struct Totals {
    decoded: u64,
    decode_us: u64,
    presented: u64,
    latency_us: u64,
}

impl Rates {
    pub fn new(running: &Running) -> Self {
        Self {
            since: Instant::now(),
            last: Totals::default(),
            shown: StatsSnapshot {
                width: running.width,
                height: running.height,
                codec: format!("{:?}", running.codec),
                ..Default::default()
            },
        }
    }

    /// The latest reading, refreshed about once a second.
    pub fn snapshot(&mut self, shared: &Shared) -> &StatsSnapshot {
        let stats = &shared.stats;
        let elapsed = self.since.elapsed();
        if elapsed >= Duration::from_secs(1) {
            let now = Totals {
                decoded: stats.decoded.load(Ordering::Relaxed),
                decode_us: stats.decode_us.load(Ordering::Relaxed),
                presented: stats.presented.load(Ordering::Relaxed),
                latency_us: stats.latency_us.load(Ordering::Relaxed),
            };
            let secs = elapsed.as_secs_f32();
            let decoded = now.decoded - self.last.decoded;
            let presented = now.presented - self.last.presented;
            self.shown.decode_fps = decoded as f32 / secs;
            self.shown.present_fps = presented as f32 / secs;
            self.shown.decode_ms = if decoded > 0 {
                (now.decode_us - self.last.decode_us) as f32 / decoded as f32 / 1000.0
            } else {
                0.0
            };
            self.shown.latency_ms = if presented > 0 {
                (now.latency_us - self.last.latency_us) as f32 / presented as f32 / 1000.0
            } else {
                0.0
            };
            self.last = now;
            self.shown.awdl_suspected = shared.awdl_suspected();
            self.since = Instant::now();
        }
        self.shown.dropped = stats.dropped.load(Ordering::Relaxed);
        self.shown.decode_errors = stats.decode_errors.load(Ordering::Relaxed);
        self.shown.partial = stats.partial.load(Ordering::Relaxed);
        self.shown.audio_underruns = stats.audio_underruns.load(Ordering::Relaxed);
        self.shown.audio_dropped_ms =
            stats.audio_dropped_frames.load(Ordering::Relaxed) * 1000 / 48_000;
        &self.shown
    }
}

/// One line for logs and the `--frames` smoke test: whole-session averages.
pub fn summary(stats: &Stats) -> String {
    let decoded = stats.decoded.load(Ordering::Relaxed);
    let presented = stats.presented.load(Ordering::Relaxed);
    let avg = |total: &AtomicU64, n: u64| {
        if n == 0 {
            0.0
        } else {
            total.load(Ordering::Relaxed) as f64 / n as f64 / 1000.0
        }
    };
    format!(
        "decoded={decoded} presented={presented} (offscreen {}) dropped={} decode_errors={} partial={} \
         decode_ms_avg={:.2} received_to_presented_ms_avg={:.2} \
         audio_underruns={} audio_dropped_frames={}",
        stats.offscreen.load(Ordering::Relaxed),
        stats.dropped.load(Ordering::Relaxed),
        stats.decode_errors.load(Ordering::Relaxed),
        stats.partial.load(Ordering::Relaxed),
        avg(&stats.decode_us, decoded),
        avg(&stats.latency_us, presented),
        stats.audio_underruns.load(Ordering::Relaxed),
        stats.audio_dropped_frames.load(Ordering::Relaxed),
    )
}

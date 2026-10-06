//! Sound. Apps play into our PulseAudio-protocol server (every Linux audio
//! stack speaks it: libpulse, PipeWire's and SDL's backends, Chrome, Firefox);
//! a mixer on a 10 ms clock sums the streams at 48 kHz stereo and encodes
//! each tick as one Opus frame for the sessions' audio tracks.
//!
//! The clock runs whether or not anyone listens: apps (and media players that
//! pace video by audio) need a sink that consumes.

mod convert;
mod opus;
mod pulse;

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use bytes::Bytes;
use tokio::sync::mpsc;
use tracing::{info, warn};

/// The mixer's (and Opus's) sample rate.
pub const RATE: u32 = 48_000;
/// Frames per tick: 10 ms, one Opus frame.
pub const FRAME: usize = 480;
const TICK: Duration = Duration::from_millis(10);
const BITRATE_BPS: i32 = 128_000;
/// While someone listens, how often the mix's level is logged (ticks).
const LEVEL_LOG_TICKS: u32 = 1000;

/// One Opus frame.
pub struct AudioPacket {
    pub data: Bytes,
    /// Its first sample's position on the mixer's 48 kHz clock.
    pub samples: u64,
    /// When it was encoded.
    pub at: Instant,
}

pub struct Audio {
    sink: Arc<pulse::Sink>,
    subscribers: Mutex<Vec<mpsc::Sender<AudioPacket>>>,
}

impl Audio {
    /// Opens the server's socket at `socket` (for `app_uid`) and starts the
    /// mixer. Connections are served on the current tokio runtime.
    pub fn start(socket: &Path, app_uid: Option<u32>) -> Result<Arc<Self>> {
        let sink = Arc::new(pulse::Sink::default());
        let listener = pulse::bind(socket, app_uid)?;
        tokio::spawn(pulse::serve(Arc::clone(&sink), listener));
        let audio = Arc::new(Self {
            sink,
            subscribers: Mutex::default(),
        });
        let encoder = opus::Encoder::new(2, BITRATE_BPS)?;
        let mixer = Arc::clone(&audio);
        std::thread::Builder::new()
            .name("audio-mixer".into())
            .spawn(move || mixer.run(encoder))
            .context("starting the mixer")?;
        info!(socket = %socket.display(), "audio: PulseAudio server up, Opus 48 kHz stereo, 10 ms frames");
        Ok(audio)
    }

    /// Opus frames from now on, until the receiver is dropped.
    pub fn subscribe(&self) -> mpsc::Receiver<AudioPacket> {
        let (tx, rx) = mpsc::channel(32);
        self.subscribers.lock().expect("subscribers lock").push(tx);
        rx
    }

    fn run(&self, mut encoder: opus::Encoder) {
        let mut mix = vec![0f32; FRAME * 2];
        let mut packet = vec![0u8; 1500];
        let mut samples: u64 = 0;
        let mut next = Instant::now();
        let mut listening = false;
        let mut level = Level::default();
        loop {
            next += TICK;
            let now = Instant::now();
            if next > now {
                std::thread::sleep(next - now);
            } else if now - next > 10 * TICK {
                // Stalled (suspended, overloaded): resume from now rather
                // than racing through the backlog.
                warn!(
                    behind_ms = (now - next).as_millis() as u64,
                    "audio clock fell behind"
                );
                next = now;
            }
            mix.fill(0.0);
            self.sink.mix(&mut mix);
            // An app's NaN or infinity would stay NaN through the clamp and
            // can leave Opus encoding nothing but silence: it is silence here.
            for v in &mut mix {
                if !v.is_finite() {
                    *v = 0.0;
                    level.invalid += 1;
                }
                *v = v.clamp(-1.0, 1.0);
                level.peak = level.peak.max(v.abs());
            }
            samples += FRAME as u64;

            let mut subscribers = self.subscribers.lock().expect("subscribers lock");
            subscribers.retain(|tx| !tx.is_closed());
            if subscribers.is_empty() {
                listening = false;
                continue;
            }
            if !listening {
                encoder.reset();
                listening = true;
            }
            let n = match encoder.encode(&mix, &mut packet) {
                Ok(n) => n,
                Err(err) => {
                    warn!("audio: {err:#}");
                    continue;
                }
            };
            level.ticks += 1;
            level.bytes += n as u64;
            if level.ticks >= LEVEL_LOG_TICKS {
                // What the sessions get, so a silent stream shows where it went quiet.
                info!(
                    peak = level.peak,
                    invalid_samples = level.invalid,
                    packet_bytes_avg = level.bytes / u64::from(level.ticks),
                    "audio: mix level"
                );
                level = Level::default();
            }
            let data = Bytes::copy_from_slice(&packet[..n]);
            let at = Instant::now();
            for tx in subscribers.iter() {
                // A full queue means a stalled session: it loses audio, not us.
                let _ = tx.try_send(AudioPacket {
                    data: data.clone(),
                    samples: samples - FRAME as u64,
                    at,
                });
            }
        }
    }
}

/// The mix over the last few seconds, for the log.
#[derive(Default)]
struct Level {
    ticks: u32,
    peak: f32,
    invalid: u64,
    bytes: u64,
}

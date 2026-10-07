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

/// Half a tick: the 5 ms frames some subscribers ask for.
const HALF: usize = FRAME / 2;

struct Subscriber {
    tx: mpsc::Sender<AudioPacket>,
    /// Wants 5 ms frames (two per tick, from a second encoder) instead of the
    /// tick's one 10 ms frame.
    half_frames: bool,
}

pub struct Audio {
    sink: Arc<pulse::Sink>,
    subscribers: Mutex<Vec<Subscriber>>,
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
        self.subscribe_with(false)
    }

    /// Like [`subscribe`](Self::subscribe), with frames of `ms` milliseconds:
    /// 10 (the mixer's) or 5, two per tick from a second encoder (what
    /// Moonlight asks for). What other subscribers get doesn't change.
    #[cfg_attr(not(feature = "gamestream"), allow(dead_code))]
    pub fn subscribe_frames(&self, ms: u32) -> Result<mpsc::Receiver<AudioPacket>> {
        match ms {
            10 => Ok(self.subscribe_with(false)),
            5 => Ok(self.subscribe_with(true)),
            _ => anyhow::bail!("Opus frames of {ms} ms: only 5 and 10 are made"),
        }
    }

    fn subscribe_with(&self, half_frames: bool) -> mpsc::Receiver<AudioPacket> {
        let (tx, rx) = mpsc::channel(32);
        self.subscribers
            .lock()
            .expect("subscribers lock")
            .push(Subscriber { tx, half_frames });
        rx
    }

    fn run(&self, mut encoder: opus::Encoder) {
        let mut mix = vec![0f32; FRAME * 2];
        let mut packet = vec![0u8; 1500];
        let mut samples: u64 = 0;
        let mut next = Instant::now();
        let mut listening = false;
        // Made when the first 5 ms subscriber comes.
        let mut half_encoder: Option<opus::Encoder> = None;
        let mut half_listening = false;
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
            subscribers.retain(|s| !s.tx.is_closed());
            if subscribers.is_empty() {
                listening = false;
                half_listening = false;
                continue;
            }
            let first_sample = samples - FRAME as u64;
            let at = Instant::now();
            if subscribers.iter().any(|s| s.half_frames) {
                if half_encoder.is_none() {
                    half_encoder = opus::Encoder::new(2, BITRATE_BPS)
                        .map_err(|err| warn!("audio: no 5 ms encoder: {err:#}"))
                        .ok();
                }
                if let Some(half) = &mut half_encoder {
                    if !half_listening {
                        half.reset();
                        half_listening = true;
                    }
                    for (i, pcm) in mix.chunks(HALF * 2).enumerate() {
                        let Ok(n) = half.encode(pcm, &mut packet) else {
                            continue;
                        };
                        let data = Bytes::copy_from_slice(&packet[..n]);
                        for s in subscribers.iter().filter(|s| s.half_frames) {
                            let _ = s.tx.try_send(AudioPacket {
                                data: data.clone(),
                                samples: first_sample + (i * HALF) as u64,
                                at,
                            });
                        }
                    }
                }
            } else {
                half_listening = false;
            }
            if subscribers.iter().all(|s| s.half_frames) {
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
            for s in subscribers.iter().filter(|s| !s.half_frames) {
                // A full queue means a stalled session: it loses audio, not us.
                let _ = s.tx.try_send(AudioPacket {
                    data: data.clone(),
                    samples: first_sample,
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

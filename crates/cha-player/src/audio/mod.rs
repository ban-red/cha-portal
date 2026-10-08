//! Audio out: Opus packets → PCM → a small playout buffer → the default
//! output device (CoreAudio on macOS, through `cpal`).

mod jitter;

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow};
use cha_client::AudioPacket;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use jitter::JitterBuffer;
pub use jitter::JitterStats;

/// Buffered audio the player aims to keep (adds to the device's own buffer).
const TARGET_MS: u32 = 30;

/// The toolbar's sound controls, shared with the thread that plays and the
/// audio callback: mute, volume (0 to 100, linear as the browser's media
/// element is), and a request to open the output device again.
#[derive(Clone)]
pub struct Volume(Arc<VolumeState>);

struct VolumeState {
    muted: AtomicBool,
    percent: AtomicU8,
    restart: AtomicBool,
}

impl Volume {
    pub fn new(muted: bool, percent: u8) -> Self {
        Self(Arc::new(VolumeState {
            muted: AtomicBool::new(muted),
            percent: AtomicU8::new(percent.min(100)),
            restart: AtomicBool::new(false),
        }))
    }

    pub fn muted(&self) -> bool {
        self.0.muted.load(Ordering::Relaxed)
    }

    pub fn set_muted(&self, muted: bool) {
        self.0.muted.store(muted, Ordering::Relaxed);
    }

    pub fn percent(&self) -> u8 {
        self.0.percent.load(Ordering::Relaxed)
    }

    pub fn set_percent(&self, percent: u8) {
        self.0.percent.store(percent.min(100), Ordering::Relaxed);
    }

    /// The factor the output is multiplied by now.
    pub fn gain(&self) -> f32 {
        if self.muted() {
            0.0
        } else {
            f32::from(self.percent()) / 100.0
        }
    }

    /// Ask the playing thread to open the output device again.
    pub fn request_restart(&self) {
        self.0.restart.store(true, Ordering::Release);
    }

    /// True once per request.
    pub fn take_restart(&self) -> bool {
        self.0.restart.swap(false, Ordering::AcqRel)
    }
}

/// Multiply `data` by a gain that moves from `*current` to `target` across the
/// buffer, so a change is a short ramp and not a click.
fn apply_gain(data: &mut [f32], current: &mut f32, target: f32) {
    if *current == target {
        if target != 1.0 {
            data.iter_mut().for_each(|s| *s *= target);
        }
        return;
    }
    let step = (target - *current) / data.len().max(1) as f32;
    let mut gain = *current;
    for s in data.iter_mut() {
        gain += step;
        *s *= gain;
    }
    *current = target;
}

/// Plays a stream of [`AudioPacket`]s.
pub struct AudioOut {
    decoder: opus::Decoder,
    channels: u8,
    sample_rate: u32,
    pcm: Vec<f32>,
    buffer: Arc<Mutex<JitterBuffer>>,
    volume: Volume,
    // Dropping the stream stops playback.
    _stream: cpal::Stream,
}

impl AudioOut {
    /// Opens the default output device for `sample_rate` Hz, `channels`
    /// (1 or 2) channels.
    pub fn open(sample_rate: u32, channels: u8, volume: Volume) -> Result<Self> {
        let decoder = new_decoder(sample_rate, channels)?;
        let buffer = Arc::new(Mutex::new(JitterBuffer::new(
            sample_rate,
            channels as usize,
            TARGET_MS,
        )));

        let device = cpal::default_host()
            .default_output_device()
            .ok_or_else(|| anyhow!("no audio output device"))?;
        let mut config = cpal::StreamConfig {
            channels: channels as u16,
            sample_rate,
            buffer_size: cpal::BufferSize::Fixed(256),
        };
        let stream = match build(&device, &config, &buffer, &volume) {
            Ok(stream) => stream,
            Err(e) => {
                tracing::debug!("fixed audio buffer refused ({e}), using the default size");
                config.buffer_size = cpal::BufferSize::Default;
                build(&device, &config, &buffer, &volume).context("opening the audio output")?
            }
        };
        stream.play().context("starting audio output")?;
        tracing::info!(sample_rate, channels, "audio output started");
        Ok(Self {
            decoder,
            channels,
            sample_rate,
            pcm: Vec::new(),
            buffer,
            volume,
            _stream: stream,
        })
    }

    /// Decode one packet and queue it for playback.
    pub fn play(&mut self, packet: &AudioPacket) -> Result<()> {
        if packet.channels != self.channels || packet.sample_rate != self.sample_rate {
            // The host changed format mid-stream; rebuild at the new one.
            *self = Self::open(packet.sample_rate, packet.channels, self.volume.clone())?;
        }
        // Opus frames are at most 120 ms.
        let max = (self.sample_rate as usize / 1000) * 120 * self.channels as usize;
        self.pcm.resize(max, 0.0);
        let frames = self
            .decoder
            .decode_float(&packet.data, &mut self.pcm, false)
            .map_err(|e| anyhow!("opus: {e}"))?;
        let mut buffer = self.buffer.lock().unwrap();
        buffer.push(&self.pcm[..frames * self.channels as usize]);
        Ok(())
    }

    /// Audio waiting to be played, in milliseconds.
    pub fn buffered_ms(&self) -> u64 {
        let frames = self.buffer.lock().unwrap().buffered_frames() as u64;
        frames * 1000 / u64::from(self.sample_rate)
    }

    pub fn stats(&self) -> JitterStats {
        self.buffer.lock().unwrap().stats
    }
}

fn new_decoder(sample_rate: u32, channels: u8) -> Result<opus::Decoder> {
    let layout = match channels {
        1 => opus::Channels::Mono,
        2 => opus::Channels::Stereo,
        n => anyhow::bail!("{n}-channel audio is not played yet"),
    };
    opus::Decoder::new(sample_rate, layout).map_err(|e| anyhow!("opus decoder: {e}"))
}

fn build(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    buffer: &Arc<Mutex<JitterBuffer>>,
    volume: &Volume,
) -> Result<cpal::Stream> {
    let buffer = buffer.clone();
    let volume = volume.clone();
    let mut current = volume.gain();
    device
        .build_output_stream(
            *config,
            move |data: &mut [f32], _| {
                buffer.lock().unwrap().pull(data);
                apply_gain(data, &mut current, volume.gain());
            },
            |e| tracing::warn!("audio stream: {e}"),
            None,
        )
        .map_err(|e| anyhow!("{e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_is_linear_and_mute_wins() {
        let v = Volume::new(false, 50);
        assert_eq!(v.gain(), 0.5);
        v.set_muted(true);
        assert_eq!(v.gain(), 0.0);
        assert_eq!(v.percent(), 50, "muting keeps the volume");
        v.set_muted(false);
        v.set_percent(250);
        assert_eq!(v.gain(), 1.0);
        assert!(!v.take_restart());
        v.request_restart();
        assert!(v.take_restart());
        assert!(!v.take_restart());
    }

    #[test]
    fn a_steady_gain_scales_and_unity_leaves_the_samples_alone() {
        let mut data = [1.0, -1.0, 0.5, 0.5];
        let mut current = 1.0;
        apply_gain(&mut data, &mut current, 1.0);
        assert_eq!(data, [1.0, -1.0, 0.5, 0.5]);
        let mut current = 0.5;
        apply_gain(&mut data, &mut current, 0.5);
        assert_eq!(data, [0.5, -0.5, 0.25, 0.25]);
    }

    #[test]
    fn a_change_ramps_across_the_buffer() {
        let mut data = [1.0; 4];
        let mut current = 1.0;
        apply_gain(&mut data, &mut current, 0.0);
        assert_eq!(current, 0.0);
        assert_eq!(data, [0.75, 0.5, 0.25, 0.0]);
        // And back up from silence.
        let mut data = [1.0; 4];
        apply_gain(&mut data, &mut current, 1.0);
        assert_eq!(data, [0.25, 0.5, 0.75, 1.0]);
    }
}

//! Audio out: Opus packets → PCM → a small playout buffer → the default
//! output device (CoreAudio on macOS, through `cpal`).

mod jitter;

use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow};
use cha_client::AudioPacket;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use jitter::JitterBuffer;
pub use jitter::JitterStats;

/// Buffered audio the player aims to keep (adds to the device's own buffer).
const TARGET_MS: u32 = 30;

/// Plays a stream of [`AudioPacket`]s.
pub struct AudioOut {
    decoder: opus::Decoder,
    channels: u8,
    sample_rate: u32,
    pcm: Vec<f32>,
    buffer: Arc<Mutex<JitterBuffer>>,
    // Dropping the stream stops playback.
    _stream: cpal::Stream,
}

impl AudioOut {
    /// Opens the default output device for `sample_rate` Hz, `channels`
    /// (1 or 2) channels.
    pub fn open(sample_rate: u32, channels: u8) -> Result<Self> {
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
        let stream = match build(&device, &config, &buffer) {
            Ok(stream) => stream,
            Err(e) => {
                tracing::debug!("fixed audio buffer refused ({e}), using the default size");
                config.buffer_size = cpal::BufferSize::Default;
                build(&device, &config, &buffer).context("opening the audio output")?
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
            _stream: stream,
        })
    }

    /// Decode one packet and queue it for playback.
    pub fn play(&mut self, packet: &AudioPacket) -> Result<()> {
        if packet.channels != self.channels || packet.sample_rate != self.sample_rate {
            // The host changed format mid-stream; rebuild at the new one.
            *self = Self::open(packet.sample_rate, packet.channels)?;
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
) -> Result<cpal::Stream> {
    let buffer = buffer.clone();
    device
        .build_output_stream(
            *config,
            move |data: &mut [f32], _| buffer.lock().unwrap().pull(data),
            |e| tracing::warn!("audio stream: {e}"),
            None,
        )
        .map_err(|e| anyhow!("{e}"))
}

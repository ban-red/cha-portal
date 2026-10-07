//! A small playout buffer between the network-paced Opus decoder and the
//! device-paced output callback.
//!
//! It holds about `target` of audio. Early packets pile up: past `max` the
//! oldest audio is dropped, so latency stays bounded when the sender's clock
//! runs fast. Late packets run it dry: it plays silence and refills to
//! `target` before resuming, so one gap is one glitch, not a stutter.

use std::collections::VecDeque;

pub struct JitterBuffer {
    channels: usize,
    samples: VecDeque<f32>,
    target_frames: usize,
    max_frames: usize,
    playing: bool,
    pub stats: JitterStats,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct JitterStats {
    /// Times the buffer ran dry while playing.
    pub underruns: u64,
    /// Frames (per channel) thrown away to bound latency.
    pub dropped_frames: u64,
}

impl JitterBuffer {
    pub fn new(sample_rate: u32, channels: usize, target_ms: u32) -> Self {
        let target_frames = (sample_rate as usize * target_ms as usize) / 1000;
        Self {
            channels,
            samples: VecDeque::new(),
            target_frames,
            max_frames: target_frames * 2 + sample_rate as usize / 100,
            playing: false,
            stats: JitterStats::default(),
        }
    }

    pub fn buffered_frames(&self) -> usize {
        self.samples.len() / self.channels
    }

    /// Add decoded, interleaved audio.
    pub fn push(&mut self, interleaved: &[f32]) {
        self.samples.extend(interleaved);
        let excess = self.buffered_frames().saturating_sub(self.max_frames);
        if excess > 0 {
            // Keep the newest `target` of audio.
            let drop = self.buffered_frames() - self.target_frames;
            self.samples.drain(..drop * self.channels);
            self.stats.dropped_frames += drop as u64;
        }
    }

    /// Fill `out` (interleaved) for the device.
    pub fn pull(&mut self, out: &mut [f32]) {
        if !self.playing {
            if self.buffered_frames() >= self.target_frames {
                self.playing = true;
            } else {
                out.fill(0.0);
                return;
            }
        }
        let have = out.len().min(self.samples.len());
        for (slot, sample) in out.iter_mut().zip(self.samples.drain(..have)) {
            *slot = sample;
        }
        if have < out.len() {
            out[have..].fill(0.0);
            self.playing = false;
            self.stats.underruns += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frames(n: usize) -> Vec<f32> {
        vec![0.5; n * 2]
    }

    #[test]
    fn waits_for_the_target_before_playing() {
        let mut j = JitterBuffer::new(48_000, 2, 20); // 960 frames
        j.push(&frames(480));
        let mut out = vec![1.0; 200];
        j.pull(&mut out);
        assert!(out.iter().all(|s| *s == 0.0), "still priming");
        j.push(&frames(480));
        j.pull(&mut out);
        assert!(out.iter().all(|s| *s == 0.5));
    }

    #[test]
    fn underrun_plays_silence_then_refills() {
        let mut j = JitterBuffer::new(48_000, 2, 20);
        j.push(&frames(960));
        let mut out = vec![0.0; 2000]; // 1000 frames > 960 buffered
        j.pull(&mut out);
        assert_eq!(out[1919], 0.5);
        assert_eq!(out[1920], 0.0);
        assert_eq!(j.stats.underruns, 1);
        // Not playing again until it holds the target.
        j.push(&frames(480));
        j.pull(&mut out);
        assert!(out.iter().all(|s| *s == 0.0));
    }

    #[test]
    fn overflow_drops_the_oldest_down_to_the_target() {
        let mut j = JitterBuffer::new(48_000, 2, 20);
        j.push(&frames(960));
        let mut marker = frames(3000);
        marker[..2].fill(0.1);
        j.push(&marker);
        assert_eq!(j.buffered_frames(), 960);
        assert!(j.stats.dropped_frames >= 3000);
    }
}

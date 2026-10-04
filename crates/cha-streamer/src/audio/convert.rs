//! From whatever an app plays to the mixer's format, 48 kHz stereo float:
//! sample decoding, a downmix by channel position, and a polyphase
//! windowed-sinc resampler.

use std::collections::VecDeque;

use pulseaudio::protocol::{ChannelMap, ChannelPosition, SampleFormat, SampleSpec};

use super::RATE;

pub struct Converter {
    format: SampleFormat,
    channels: usize,
    frame_bytes: usize,
    /// Each input channel's share of left and right.
    weights: Vec<[f32; 2]>,
    resampler: Option<Resampler>,
}

impl Converter {
    pub fn new(spec: &SampleSpec, map: &ChannelMap, by_index: bool) -> Self {
        let channels = usize::from(spec.channels.max(1));
        Self {
            format: spec.format,
            channels,
            frame_bytes: spec.format.bytes_per_sample() * channels,
            weights: downmix(map, channels, by_index),
            resampler: (spec.sample_rate != RATE).then(|| Resampler::new(spec.sample_rate)),
        }
    }

    pub fn set_rate(&mut self, rate: u32) {
        self.resampler = (rate != RATE).then(|| Resampler::new(rate));
    }

    /// Adds up to `out.len() / 2` frames from `queue` into `out` (interleaved
    /// stereo) at `gain`, consuming the bytes used. Returns the frames added
    /// and the bytes consumed.
    pub fn mix(
        &mut self,
        queue: &mut VecDeque<u8>,
        out: &mut [f32],
        gain: [f32; 2],
    ) -> (usize, usize) {
        if self.frame_bytes == 0 {
            return (0, 0);
        }
        let bytes = queue.make_contiguous();
        let mut decoder = Decoder {
            bytes,
            at: 0,
            format: self.format,
            channels: self.channels,
            frame_bytes: self.frame_bytes,
            weights: &self.weights,
        };
        let frames = match &mut self.resampler {
            None => {
                let mut n = 0;
                for o in out.chunks_exact_mut(2) {
                    let Some([l, r]) = decoder.next() else { break };
                    o[0] += l * gain[0];
                    o[1] += r * gain[1];
                    n += 1;
                }
                n
            }
            Some(resampler) => resampler.mix(&mut decoder, out, gain),
        };
        let consumed = decoder.at;
        queue.drain(..consumed);
        (frames, consumed)
    }
}

/// Reads frames from raw bytes, already downmixed to stereo.
struct Decoder<'a> {
    bytes: &'a [u8],
    at: usize,
    format: SampleFormat,
    channels: usize,
    frame_bytes: usize,
    weights: &'a [[f32; 2]],
}

impl Decoder<'_> {
    fn next(&mut self) -> Option<[f32; 2]> {
        let frame = self.bytes.get(self.at..self.at + self.frame_bytes)?;
        self.at += self.frame_bytes;
        let width = self.frame_bytes / self.channels;
        let (mut l, mut r) = (0.0, 0.0);
        for (c, w) in self.weights.iter().enumerate() {
            let v = sample(self.format, &frame[c * width..(c + 1) * width]);
            l += v * w[0];
            r += v * w[1];
        }
        Some([l, r])
    }
}

fn sample(format: SampleFormat, b: &[u8]) -> f32 {
    use SampleFormat::*;
    const I16: f32 = 1.0 / 32768.0;
    const I24: f32 = 1.0 / 8_388_608.0;
    const I32: f32 = 1.0 / 2_147_483_648.0;
    match format {
        S16Le => i16::from_le_bytes([b[0], b[1]]) as f32 * I16,
        S16Be => i16::from_be_bytes([b[0], b[1]]) as f32 * I16,
        Float32Le => f32::from_le_bytes([b[0], b[1], b[2], b[3]]),
        Float32Be => f32::from_be_bytes([b[0], b[1], b[2], b[3]]),
        S32Le => i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f32 * I32,
        S32Be => i32::from_be_bytes([b[0], b[1], b[2], b[3]]) as f32 * I32,
        S24Le => (i32::from_le_bytes([0, b[0], b[1], b[2]]) >> 8) as f32 * I24,
        S24Be => (i32::from_be_bytes([b[0], b[1], b[2], 0]) >> 8) as f32 * I24,
        S24In32Le => ((i32::from_le_bytes([b[0], b[1], b[2], b[3]]) << 8) >> 8) as f32 * I24,
        S24In32Be => ((i32::from_be_bytes([b[0], b[1], b[2], b[3]]) << 8) >> 8) as f32 * I24,
        U8 => (f32::from(b[0]) - 128.0) / 128.0,
        Ulaw => ulaw(b[0]),
        Alaw => alaw(b[0]),
        Invalid => 0.0,
    }
}

fn ulaw(v: u8) -> f32 {
    let v = !v;
    let exponent = (v >> 4) & 7;
    let magnitude = ((i32::from(v & 0x0f) << 3) + 0x84) << exponent;
    let linear = magnitude - 0x84;
    (if v & 0x80 != 0 { -linear } else { linear }) as f32 / 32768.0
}

fn alaw(v: u8) -> f32 {
    let v = v ^ 0x55;
    let exponent = (v >> 4) & 7;
    let mantissa = i32::from(v & 0x0f);
    let magnitude = if exponent == 0 {
        (mantissa << 4) + 8
    } else {
        ((mantissa << 4) + 0x108) << (exponent - 1)
    };
    (if v & 0x80 != 0 { magnitude } else { -magnitude }) as f32 / 32768.0
}

/// Each channel's (left, right) weights for a stereo downmix, by position:
/// sides and rears at -3 dB, centres to both at -3 dB, LFE dropped (as
/// PulseAudio does by default), each output normalised so it can't clip more
/// than its loudest input.
fn downmix(map: &ChannelMap, channels: usize, by_index: bool) -> Vec<[f32; 2]> {
    use ChannelPosition::*;
    const H: f32 = std::f32::consts::FRAC_1_SQRT_2;
    let positions: Vec<ChannelPosition> = map.into_iter().collect();
    if channels == 1 {
        return vec![[1.0, 1.0]];
    }
    // Only auxiliary positions (or none): go by index.
    let aux = |p: &ChannelPosition| (Aux0 as u32..=Aux31 as u32).contains(&(*p as u32));
    let known = positions.len() == channels && !positions.iter().all(aux);
    let mut weights: Vec<[f32; 2]> = (0..channels)
        .map(|c| {
            if by_index || !known {
                return match c {
                    0 => [1.0, 0.0],
                    1 => [0.0, 1.0],
                    _ => [0.0, 0.0],
                };
            }
            match positions[c] {
                Mono => [1.0, 1.0],
                FrontLeft => [1.0, 0.0],
                FrontRight => [0.0, 1.0],
                FrontLeftOfCenter => [1.0, 0.0],
                FrontRightOfCenter => [0.0, 1.0],
                RearLeft | SideLeft | TopFrontLeft | TopRearLeft => [H, 0.0],
                RearRight | SideRight | TopFrontRight | TopRearRight => [0.0, H],
                FrontCenter | RearCenter | TopCenter | TopFrontCenter | TopRearCenter => [H, H],
                _ => [0.0, 0.0],
            }
        })
        .collect();
    for side in 0..2 {
        let sum: f32 = weights.iter().map(|w| w[side]).sum();
        if sum > 1.0 {
            weights.iter_mut().for_each(|w| w[side] /= sum);
        }
    }
    weights
}

/// Polyphase windowed-sinc resampling to 48 kHz: 32 taps (16 a side), 256
/// phases interpolated linearly, a Kaiser window. 0.33 ms of delay at 48 kHz.
struct Resampler {
    /// Input frames per output frame.
    step: f64,
    /// The next output frame's position in `history`.
    pos: f64,
    history: Vec<[f32; 2]>,
    /// (PHASES + 1) rows of TAPS coefficients.
    table: Vec<f32>,
}

const HALF: usize = 16;
const TAPS: usize = 2 * HALF;
const PHASES: usize = 256;

impl Resampler {
    fn new(rate: u32) -> Self {
        let step = f64::from(rate) / f64::from(RATE);
        // Downsampling: cut below the output's Nyquist frequency.
        let cutoff = 0.95 * (1.0 / step).min(1.0);
        let beta = 8.0;
        let mut table = Vec::with_capacity((PHASES + 1) * TAPS);
        for p in 0..=PHASES {
            let frac = p as f64 / PHASES as f64;
            let row: Vec<f64> = (0..TAPS)
                .map(|j| {
                    let x = j as f64 - (HALF as f64 - 1.0) - frac;
                    let t = x / HALF as f64;
                    let window = if t.abs() >= 1.0 {
                        0.0
                    } else {
                        bessel_i0(beta * (1.0 - t * t).sqrt()) / bessel_i0(beta)
                    };
                    cutoff * sinc(cutoff * x) * window
                })
                .collect();
            let sum: f64 = row.iter().sum();
            table.extend(row.iter().map(|v| (v / sum) as f32));
        }
        Self {
            step,
            pos: (HALF - 1) as f64,
            history: vec![[0.0; 2]; HALF - 1],
            table,
        }
    }

    fn mix(&mut self, decoder: &mut Decoder<'_>, out: &mut [f32], gain: [f32; 2]) -> usize {
        let mut n = 0;
        for o in out.chunks_exact_mut(2) {
            let base = self.pos as usize;
            // Taps reach from base - (HALF - 1) to base + HALF.
            while self.history.len() <= base + HALF {
                match decoder.next() {
                    Some(frame) => self.history.push(frame),
                    None => break,
                }
            }
            if self.history.len() <= base + HALF {
                break;
            }
            let frac = self.pos - base as f64;
            let phase = frac * PHASES as f64;
            let p = (phase as usize).min(PHASES - 1);
            let mu = (phase - p as f64) as f32;
            let a = &self.table[p * TAPS..(p + 1) * TAPS];
            let b = &self.table[(p + 1) * TAPS..(p + 2) * TAPS];
            let x = &self.history[base + 1 - HALF..=base + HALF];
            let (mut l, mut r) = (0.0, 0.0);
            for j in 0..TAPS {
                let h = a[j] + (b[j] - a[j]) * mu;
                l += x[j][0] * h;
                r += x[j][1] * h;
            }
            o[0] += l * gain[0];
            o[1] += r * gain[1];
            n += 1;
            self.pos += self.step;
        }
        // Drop what no later output frame needs.
        let keep_from = (self.pos as usize + 1)
            .saturating_sub(HALF)
            .min(self.history.len());
        if keep_from > 0 {
            self.history.drain(..keep_from);
            self.pos -= keep_from as f64;
        }
        n
    }
}

fn sinc(x: f64) -> f64 {
    if x.abs() < 1e-9 {
        1.0
    } else {
        let px = std::f64::consts::PI * x;
        px.sin() / px
    }
}

fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let q = x * x / 4.0;
    for k in 1..50 {
        term *= q / (k * k) as f64;
        sum += term;
        if term < sum * 1e-12 {
            break;
        }
    }
    sum
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(format: SampleFormat, channels: u8, rate: u32) -> SampleSpec {
        SampleSpec {
            format,
            channels,
            sample_rate: rate,
        }
    }

    #[test]
    fn s16_stereo_passes_through() {
        let mut c = Converter::new(
            &spec(SampleFormat::S16Le, 2, 48_000),
            &ChannelMap::stereo(),
            false,
        );
        let mut q: VecDeque<u8> = [16384i16, -16384, 8192, 0]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        let mut out = [0.0; 6];
        assert_eq!(c.mix(&mut q, &mut out, [1.0, 1.0]), (2, 8));
        assert_eq!(&out[..4], &[0.5, -0.5, 0.25, 0.0]);
        assert!(q.is_empty());
    }

    #[test]
    fn surround_downmix_does_not_clip() {
        use ChannelPosition::*;
        let map = ChannelMap::new([FrontLeft, FrontRight, FrontCenter, Lfe, RearLeft, RearRight]);
        let w = downmix(&map, 6, false);
        assert!(w.iter().map(|w| w[0]).sum::<f32>() <= 1.0 + 1e-6);
        assert_eq!(w[3], [0.0, 0.0]);
        assert!(w[0][0] > w[4][0]);
    }

    #[test]
    fn resampled_tone_keeps_its_level() {
        // 1 kHz at 44.1 kHz in, 48 kHz out: same amplitude, ~48/44.1 as many frames.
        let mut c = Converter::new(
            &spec(SampleFormat::Float32Le, 1, 44_100),
            &ChannelMap::mono(),
            false,
        );
        let mut q: VecDeque<u8> = (0..4410)
            .flat_map(|i| {
                let v = (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / 44_100.0).sin() * 0.5;
                v.to_le_bytes()
            })
            .collect();
        let mut out = vec![0.0; 2 * 4800];
        let (frames, _) = c.mix(&mut q, &mut out, [1.0, 1.0]);
        assert!((4780..=4800).contains(&frames), "{frames}");
        let peak = out[2 * 200..2 * frames]
            .iter()
            .fold(0f32, |m, v| m.max(v.abs()));
        assert!((0.49..0.51).contains(&peak), "{peak}");
    }

    #[test]
    fn companded_formats_decode() {
        assert!(ulaw(0xff).abs() < 1e-3);
        assert!(alaw(0xd5).abs() < 1e-3);
        assert!(ulaw(0x00) < -0.9);
    }
}

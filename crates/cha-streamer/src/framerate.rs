//! The frame rate (60 to 120 fps) and what rides on it: the rates a page may
//! switch to, and the bitrates that follow. The baseline is 1440p60 on
//! 1 GbE; 90 and 120 are options (plan §3.1).
//!
//! Everything that depends on the rate goes through here, so it is stated
//! once: the NVENC codecs' bitrate and PyroWave's per-frame budget. What rate
//! control does per frame, and what it holds fixed in time, is in
//! `rate.rs`, `wt.rs` and the README.

use std::time::Duration;

use cha_pyrowave::Chroma;

/// What the defaults (`--mbps`, `--pyrowave-mbps`) are tuned for.
pub const BASE_FPS: u32 = 60;
/// The rates a page may ask for.
pub const CHOICES: [u32; 3] = [60, 90, 120];
/// The most PyroWave may take (Mbit/s): what 1 GbE carries with headroom
/// for audio, framing and the rest of the LAN. Past it, rate control (the
/// frames it holds back, the page's reports) trims.
pub const PYROWAVE_CAP_MBPS: f64 = 600.0;

/// How the NVENC codecs' bitrate grows with the frame rate: a frame at
/// twice the rate is much cheaper (less changes between frames, and
/// inter-frame prediction does better), so the bitrate grows with the
/// rate to the power 0.75, not linearly: ×1.68 at 120 fps, ×1.36 at 90.
const NVENC_EXPONENT: f64 = 0.75;

/// Checks a requested rate.
pub fn validate(fps: u32) -> Result<u32, String> {
    if CHOICES.contains(&fps) {
        Ok(fps)
    } else {
        Err(format!("{fps} fps isn't one of 60, 90, 120"))
    }
}

/// The time between frames at `fps`.
pub fn interval(fps: u32) -> Duration {
    Duration::from_secs(1) / fps.max(1)
}

/// The NVENC codecs' bitrate at `fps`, from `base_bps` at 60 fps (`--mbps`):
/// `base × (fps / 60)^0.75`.
pub fn nvenc_bps(base_bps: u32, fps: u32) -> u32 {
    let ratio = f64::from(fps.max(1)) / f64::from(BASE_FPS);
    (f64::from(base_bps) * ratio.powf(NVENC_EXPONENT)).round() as u32
}

/// What a PyroWave frame may take (bytes), and whether the cap on the total
/// cut it. PyroWave codes every frame on its own, so its budget is per frame:
/// `mbps_420` (at 60 fps, 1440p and 4:2:0; 4:4:4 gets twice) divided by 60,
/// scaled with the picture's `area` (relative to 1440p). The same bytes per
/// frame at every rate keeps the picture's quality, so the total grows
/// linearly with `fps`, up to [`PYROWAVE_CAP_MBPS`].
pub fn pyrowave_frame_bytes(mbps_420: u32, chroma: Chroma, area: f64, fps: u32) -> (usize, bool) {
    let fps = f64::from(fps.max(1));
    let per_frame_mbps =
        f64::from(mbps_420) * if chroma == Chroma::Yuv444 { 2.0 } else { 1.0 } * area
            / f64::from(BASE_FPS);
    let total = per_frame_mbps * fps;
    let capped = total > PYROWAVE_CAP_MBPS;
    let total = total.min(PYROWAVE_CAP_MBPS);
    let bytes = (total * 1e6 / 8.0 / fps) as usize;
    (bytes.max(64 * 1024), capped)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_offered_rates_are_valid() {
        for fps in CHOICES {
            assert_eq!(validate(fps), Ok(fps));
        }
        for fps in [0, 30, 59, 144, 240] {
            assert!(validate(fps).is_err());
        }
    }

    #[test]
    fn nvenc_bitrate_grows_with_the_rate_to_the_power_three_quarters() {
        assert_eq!(nvenc_bps(40_000_000, 60), 40_000_000);
        let at_90 = nvenc_bps(40_000_000, 90);
        let at_120 = nvenc_bps(40_000_000, 120);
        // 1.5^0.75 = 1.355 and 2^0.75 = 1.682.
        assert!((54_100_000..54_300_000).contains(&at_90), "{at_90}");
        assert!((67_200_000..67_400_000).contains(&at_120), "{at_120}");
        // Less per frame, more per second.
        assert!(at_120 / 120 < 40_000_000 / 60);
    }

    #[test]
    fn pyrowave_keeps_its_frame_budget_and_the_total_grows_with_the_rate() {
        let area = 1.0;
        let (at_60, capped) = pyrowave_frame_bytes(290, Chroma::Yuv420, area, 60);
        assert!(!capped);
        // 290 Mbit/s over 60 frames.
        assert_eq!(at_60, 290_000_000 / 8 / 60);
        let (at_120, capped) = pyrowave_frame_bytes(290, Chroma::Yuv420, area, 120);
        assert!(!capped);
        assert_eq!(at_120, at_60);
        // The total is 580 Mbit/s, under the cap.
        assert!(at_120 as f64 * 8.0 * 120.0 / 1e6 < PYROWAVE_CAP_MBPS);
    }

    #[test]
    fn pyrowave_is_capped_to_what_the_link_carries() {
        // 4:4:4 at 60 fps is 580 Mbit/s: under the cap.
        let (at_60, capped) = pyrowave_frame_bytes(290, Chroma::Yuv444, 1.0, 60);
        assert!(!capped);
        assert_eq!(at_60, 580_000_000 / 8 / 60);
        // At 120 it would take 1160: held to 600, frames get half the bytes.
        let (at_120, capped) = pyrowave_frame_bytes(290, Chroma::Yuv444, 1.0, 120);
        assert!(capped);
        assert_eq!(at_120, 600_000_000 / 8 / 120);
        assert!(at_120 < at_60);
        // A bigger picture hits the cap sooner.
        let (_, capped) = pyrowave_frame_bytes(290, Chroma::Yuv420, 2.25, 60);
        assert!(capped);
        // A tiny picture keeps its floor.
        let (tiny, _) = pyrowave_frame_bytes(290, Chroma::Yuv420, 0.001, 60);
        assert_eq!(tiny, 64 * 1024);
    }
}

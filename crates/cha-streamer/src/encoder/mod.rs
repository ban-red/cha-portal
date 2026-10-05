//! What turns a composited frame into an access unit, behind one trait, so the
//! media code is the same on every device (plan: `docs/devices.md`).
//!
//! - [`nvenc`]: NVENC through `cha-nvenc`, zero-copy from the output buffer
//!   (H.264, HEVC, AV1), the NVIDIA device's;
//! - [`x264`]: x264 loaded at run time, from frames read back to memory
//!   (H.264), the CPU device's;
//! - [`svtav1`]: SVT-AV1 loaded at run time, from the same frames (AV1), the
//!   CPU device's other codec;
//! - [`vaapi`]: libva (Intel, AMD): the capability probe works, the encoder
//!   isn't built yet.
//!
//! An encoder owns its codec's state for one stream. The media code asks it
//! for a frame at a time, keyframes on request, and reconfigures it in place
//! as the session's pace changes. What an encoder can't do it says so (no
//! reference invalidation: the caller sends a keyframe instead).

use anyhow::Result;
pub use cha_nvenc::{Codec, Timings};

use crate::media::Frame;

pub mod dl;
pub mod i420;
pub mod nvenc;
pub mod svtav1;
pub mod vaapi;
pub mod x264;

/// What an encoder starts with.
#[derive(Clone, Debug)]
pub struct Params {
    pub codec: Codec,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_bps: u32,
}

/// One encoded stream.
pub trait VideoEncoder: Send {
    /// What it is configured with now.
    fn params(&self) -> &Params;

    /// Encodes `frame` into `out` (cleared first); `keyframe` forces an IDR.
    /// Returns whether the result is a keyframe (an encoder may make one
    /// unasked, after a reconfiguration that restarted it).
    fn encode(&mut self, frame: &Frame, keyframe: bool, out: &mut Vec<u8>) -> Result<bool>;

    /// Switches to another picture size (the frame sizes follow the output).
    fn resize(&mut self, width: u32, height: u32) -> Result<()>;

    /// A new target bitrate, in place where the encoder can.
    fn set_bitrate(&mut self, bitrate_bps: u32) -> Result<()>;

    /// A new frame rate, with the bitrate scaled for it: the rate control
    /// and timing are per frame. Encoders that can't change it live restart
    /// from a keyframe.
    fn set_frame_rate(&mut self, fps: u32, bitrate_bps: u32) -> Result<()>;

    /// The index of the frame the next `encode` makes (counting from 0).
    fn next_index(&self) -> u64;

    /// The index of the last frame encoded.
    fn last_index(&self) -> u64 {
        self.next_index().saturating_sub(1)
    }

    /// Reference frame invalidation: frames from `from` on were lost, so the
    /// next frame refers around them. False when that can't be (not
    /// supported, too far back): the caller sends a keyframe instead.
    fn invalidate_from(&mut self, _from: u64) -> Result<bool> {
        Ok(false)
    }

    /// The output buffers were reallocated: drop what was registered for the
    /// old ones.
    fn forget_surfaces(&mut self) {}

    /// Where the last frame spent its time.
    fn last_timings(&self) -> Timings {
        Timings::default()
    }
}

/// Which encoder makes a codec on a kind of device: a pure choice, so it is
/// tested without any hardware.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    Nvenc,
    X264,
    SvtAv1,
    Vaapi,
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Counting {
        params: Params,
        next: u64,
    }

    impl VideoEncoder for Counting {
        fn params(&self) -> &Params {
            &self.params
        }
        fn encode(&mut self, _: &Frame, _: bool, _: &mut Vec<u8>) -> Result<bool> {
            self.next += 1;
            Ok(true)
        }
        fn resize(&mut self, _: u32, _: u32) -> Result<()> {
            Ok(())
        }
        fn set_bitrate(&mut self, _: u32) -> Result<()> {
            Ok(())
        }
        fn set_frame_rate(&mut self, _: u32, _: u32) -> Result<()> {
            Ok(())
        }
        fn next_index(&self) -> u64 {
            self.next
        }
    }

    #[test]
    fn what_an_encoder_leaves_out_has_safe_defaults() {
        let mut e = Counting {
            params: Params {
                codec: Codec::H264,
                width: 64,
                height: 64,
                fps: 60,
                bitrate_bps: 1,
            },
            next: 0,
        };
        // No frames yet: no "last" one to underflow to.
        assert_eq!(e.last_index(), 0);
        e.next = 5;
        assert_eq!(e.last_index(), 4);
        // No reference invalidation: the caller falls back to a keyframe.
        assert!(!e.invalidate_from(3).unwrap());
        e.forget_surfaces();
        assert_eq!(e.last_timings().submit.as_nanos(), 0);
    }
}

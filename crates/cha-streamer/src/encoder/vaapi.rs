//! VA-API (libva, loaded at run time): Intel's iHD and Mesa's radeonsi.
//!
//! - [`probe`]: which codecs the driver can *encode* (an `EncSlice` or
//!   `EncSliceLP` entrypoint on a profile of that codec), for the node's
//!   inventory.
//! - [`open`]: the encoder, H.264 only so far ([`BUILT`]); HEVC and AV1 take
//!   other parameter buffers (`VAEncSequenceParameterBufferHEVC`, …`AV1`) and
//!   headers, and are not written.
//!
//! The encoder (`session.rs`) imports the compositor's dmabuf as a VA surface
//! with no copy, converts it to NV12 with the driver's video processor, and
//! encodes it in low-latency CBR with packed headers we write ourselves
//! (`bitstream.rs`). The output pool asks [`accepts_import`] while it picks
//! the buffers' format and modifier, so it only allocates what the driver can
//! import. The module tree:
//! - `ffi.rs`: libva's functions and structs, and the display;
//! - `h264.rs`: what the H.264 parameter buffers say (pure, tested anywhere);
//! - `session.rs`: the encoder, and the self-test.
//!
//! Written from the VA-API headers and documentation, and the H.264
//! specification; no FFmpeg, GStreamer or other project's source.

mod ffi;
mod h264;
mod session;
#[allow(
    dead_code,
    non_camel_case_types,
    non_snake_case,
    non_upper_case_globals,
    unsafe_op_in_unsafe_fn,
    clippy::all
)]
mod sys;

pub use session::{accepts_import, self_test};

use std::path::Path;

use anyhow::Result;
use cha_nvenc::Codec;

use self::ffi::{
    ENTRYPOINT_ENC_SLICE, ENTRYPOINT_ENC_SLICE_LP, PROFILE_H264_CONSTRAINED_BASELINE,
    PROFILE_H264_HIGH, PROFILE_H264_MAIN,
};
use super::{Params, VideoEncoder};

// va.h's VAProfile values for the codecs we have no encoder for yet.
const PROFILE_HEVC_MAIN: i32 = 17;
const PROFILE_AV1_PROFILE0: i32 = 32;
/// Whether [`open`] makes an encoder.
pub const ENCODER_BUILT: bool = true;
/// The codecs the encoder makes: what the driver offers beyond them stays
/// unused (and unoffered) until they are written.
pub const BUILT: [Codec; 1] = [Codec::H264];

/// What a device's driver offers.
#[derive(Debug, PartialEq, Eq)]
pub struct Caps {
    /// The driver's own description (`Intel iHD driver for …`).
    pub vendor: String,
    /// The codecs it can encode, in `Codec::ALL` order.
    pub codecs: Vec<Codec>,
}

/// The codecs with an encode entrypoint, from each profile's entrypoints.
pub fn encodable(profiles: &[(i32, Vec<i32>)]) -> Vec<Codec> {
    let encodes = |wanted: &[i32]| {
        profiles.iter().any(|(profile, entrypoints)| {
            wanted.contains(profile)
                && entrypoints
                    .iter()
                    .any(|e| matches!(*e, ENTRYPOINT_ENC_SLICE | ENTRYPOINT_ENC_SLICE_LP))
        })
    };
    let mut codecs = Vec::new();
    if encodes(&[
        PROFILE_H264_MAIN,
        PROFILE_H264_HIGH,
        PROFILE_H264_CONSTRAINED_BASELINE,
    ]) {
        codecs.push(Codec::H264);
    }
    if encodes(&[PROFILE_HEVC_MAIN]) {
        codecs.push(Codec::Hevc);
    }
    if encodes(&[PROFILE_AV1_PROFILE0]) {
        codecs.push(Codec::Av1);
    }
    codecs
}

/// Of `codecs`, those the encoder is written for.
pub fn built(codecs: Vec<Codec>) -> Vec<Codec> {
    codecs.into_iter().filter(|c| BUILT.contains(c)).collect()
}

/// Asks the VA-API driver on `render_node` what it can encode.
pub fn probe(render_node: &Path) -> Result<Caps> {
    let display = ffi::Display::open(render_node)?;
    let mut found = Vec::new();
    for profile in display.profiles()? {
        // A profile that can't be queried has none for us.
        found.push((profile, display.entrypoints(profile)));
    }
    Ok(Caps {
        vendor: display.vendor(),
        codecs: encodable(&found),
    })
}

/// A new VA-API encoder for `params.codec` on `render_node`.
pub fn open(render_node: &Path, params: Params) -> Result<Box<dyn VideoEncoder>> {
    Ok(Box::new(session::Vaapi::new(render_node, params)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_codec_counts_with_an_encode_entrypoint_on_a_profile_of_it() {
        // VLD is 1: decode only.
        let decode_only = [(PROFILE_H264_HIGH, vec![1]), (PROFILE_HEVC_MAIN, vec![1])];
        assert!(encodable(&decode_only).is_empty());
        let intel = [
            (PROFILE_H264_HIGH, vec![1, ENTRYPOINT_ENC_SLICE_LP]),
            (PROFILE_HEVC_MAIN, vec![1, ENTRYPOINT_ENC_SLICE]),
            (PROFILE_AV1_PROFILE0, vec![1, ENTRYPOINT_ENC_SLICE_LP]),
            // Not a profile we offer.
            (19, vec![1, ENTRYPOINT_ENC_SLICE]),
        ];
        assert_eq!(encodable(&intel), [Codec::H264, Codec::Hevc, Codec::Av1]);
        let amd = [
            (
                PROFILE_H264_CONSTRAINED_BASELINE,
                vec![ENTRYPOINT_ENC_SLICE],
            ),
            (PROFILE_HEVC_MAIN, vec![ENTRYPOINT_ENC_SLICE]),
        ];
        assert_eq!(encodable(&amd), [Codec::H264, Codec::Hevc]);
    }

    #[test]
    fn only_the_codecs_that_are_written_are_offered() {
        assert_eq!(
            built(vec![Codec::H264, Codec::Hevc, Codec::Av1]),
            [Codec::H264]
        );
        assert!(built(vec![Codec::Av1]).is_empty());
    }

    #[test]
    fn hevc_and_av1_are_refused_before_the_device_is_touched() {
        for codec in [Codec::Hevc, Codec::Av1] {
            let err = open(
                Path::new("/dev/dri/none"),
                Params {
                    codec,
                    width: 1280,
                    height: 720,
                    fps: 60,
                    bitrate_bps: 1,
                },
            )
            .err()
            .unwrap();
            assert!(err.to_string().contains("H.264 only"), "{err}");
        }
    }
}

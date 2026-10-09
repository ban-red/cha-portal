//! VA-API (libva, loaded at run time): Intel's iHD and Mesa's radeonsi.
//!
//! - [`probe`]: which codecs the driver can *encode* (an `EncSlice` or
//!   `EncSliceLP` entrypoint on a profile of that codec), for the node's
//!   inventory.
//! - [`open`]: the encoder: H.264, HEVC Main 8-bit and AV1 Profile 0 8-bit
//!   ([`BUILT`]).
//!
//! The encoder (`session.rs`) imports the compositor's dmabuf as a VA surface
//! with no copy, converts it to NV12 with the driver's video processor, and
//! encodes it in low-latency CBR with packed headers we write ourselves
//! (`bitstream.rs`; AV1's are OBUs, `bitstream/av1.rs`). The output pool asks [`accepts_import`] while it picks
//! the buffers' format and modifier, so it only allocates what the driver can
//! import. The module tree:
//! - `ffi.rs`: libva's functions and structs, and the display;
//! - `h264.rs`, `hevc.rs`, `av1.rs`: what the H.264, HEVC and AV1 parameter
//!   buffers say (pure, tested anywhere);
//! - `session.rs`: the encoder, and the self-test.
//!
//! Written from the VA-API headers and documentation, and the H.264, H.265 and
//! AV1 specifications; for how a driver reads the buffers, Mesa's source (MIT)
//! too. No FFmpeg, GStreamer or other project's source.

mod av1;
mod ffi;
mod h264;
mod hevc;
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
    ENTRYPOINT_ENC_SLICE, ENTRYPOINT_ENC_SLICE_LP, PROFILE_AV1_PROFILE0,
    PROFILE_H264_CONSTRAINED_BASELINE, PROFILE_H264_HIGH, PROFILE_H264_MAIN, PROFILE_HEVC_MAIN,
};
use super::{Params, VideoEncoder};

/// Whether [`open`] makes an encoder.
pub const ENCODER_BUILT: bool = true;
/// The codecs the encoder makes: what the driver offers beyond them stays
/// unused (and unoffered).
pub const BUILT: [Codec; 3] = [Codec::H264, Codec::Hevc, Codec::Av1];

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
            [Codec::H264, Codec::Hevc, Codec::Av1]
        );
        assert_eq!(built(vec![Codec::Av1]), [Codec::Av1]);
        assert!(built(vec![]).is_empty());
    }
}

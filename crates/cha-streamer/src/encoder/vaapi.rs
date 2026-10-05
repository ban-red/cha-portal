//! VA-API (libva, loaded at run time): Intel's iHD and Mesa's radeonsi.
//!
//! This round has the capability probe, which is what the node needs to offer
//! the device: it asks the driver which codecs it can *encode* (an
//! `EncSlice` or `EncSliceLP` entrypoint on a profile of that codec). The
//! encoder isn't built yet: [`open`] says so.
//!
//! What the encoder needs, for whoever builds it:
//! - The compositor already renders on the Mesa render node and keeps the
//!   output buffers as GBM dmabufs (`SlotBuffer::Dmabuf`), the thing to import
//!   as a VA surface (`vaCreateSurfaces` with `VASurfaceAttribExternalBuffers`,
//!   or `vaCreateSurfaces` from a PRIME fd) and encode with no copy. The pool
//!   picks the first renderable format; VA may want linear or the driver's own
//!   tiling, so the pool's format choice will take the device's wishes.
//! - Low-latency CBR: `VAConfigAttribRateControl = VA_RC_CBR`, a one-frame HRD
//!   buffer, no B-frames, infinite GOP with an IDR on request, `EncSliceLP`
//!   (the fixed-function low-power path) where the driver has it.
//! - H.264, HEVC and AV1 take different sequence/picture/slice parameter
//!   buffers (`VAEncSequenceParameterBufferH264`, …`HEVC`, …`AV1`), with the
//!   packed headers made by us (SPS/PPS/VPS, or the AV1 sequence OBU).

use std::ffi::{CStr, c_char, c_int, c_void};
use std::fs::File;
use std::os::fd::AsRawFd;
use std::path::Path;

use anyhow::{Context, Result, bail, ensure};
use cha_nvenc::Codec;

use super::dl::Library;
use super::{Params, VideoEncoder};

// va.h's VAProfile values (the ones that encode).
const PROFILE_H264_MAIN: i32 = 6;
const PROFILE_H264_HIGH: i32 = 7;
const PROFILE_H264_CONSTRAINED_BASELINE: i32 = 13;
const PROFILE_HEVC_MAIN: i32 = 17;
const PROFILE_AV1_PROFILE0: i32 = 32;
/// Whether [`open`] makes an encoder.
pub const ENCODER_BUILT: bool = false;

// VAEntrypointEncSlice, and the low-power variant.
const ENTRYPOINT_ENC_SLICE: i32 = 6;
const ENTRYPOINT_ENC_SLICE_LP: i32 = 8;

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

type GetDisplayDrm = unsafe extern "C" fn(c_int) -> *mut c_void;
type Initialize = unsafe extern "C" fn(*mut c_void, *mut c_int, *mut c_int) -> c_int;
type Terminate = unsafe extern "C" fn(*mut c_void) -> c_int;
type QueryVendorString = unsafe extern "C" fn(*mut c_void) -> *const c_char;
type MaxNum = unsafe extern "C" fn(*mut c_void) -> c_int;
type QueryConfigProfiles = unsafe extern "C" fn(*mut c_void, *mut c_int, *mut c_int) -> c_int;
type QueryConfigEntrypoints =
    unsafe extern "C" fn(*mut c_void, c_int, *mut c_int, *mut c_int) -> c_int;

/// Asks the VA-API driver on `render_node` what it can encode.
pub fn probe(render_node: &Path) -> Result<Caps> {
    let va = Library::open(&["libva.so.2"]).context("VA-API isn't installed (libva2)")?;
    let drm = Library::open(&["libva-drm.so.2"]).context("VA-API isn't installed (libva-drm2)")?;
    let file = File::options()
        .read(true)
        .write(true)
        .open(render_node)
        .with_context(|| format!("opening {}", render_node.display()))?;
    // SAFETY: the signatures are va.h's and va_drm.h's. The display belongs
    // to the fd, which stays open until after `vaTerminate`.
    unsafe {
        let get_display: GetDisplayDrm = drm.symbol(c"vaGetDisplayDRM")?;
        let initialize: Initialize = va.symbol(c"vaInitialize")?;
        let terminate: Terminate = va.symbol(c"vaTerminate")?;
        let vendor_string: QueryVendorString = va.symbol(c"vaQueryVendorString")?;
        let max_profiles: MaxNum = va.symbol(c"vaMaxNumProfiles")?;
        let query_profiles: QueryConfigProfiles = va.symbol(c"vaQueryConfigProfiles")?;
        let max_entrypoints: MaxNum = va.symbol(c"vaMaxNumEntrypoints")?;
        let query_entrypoints: QueryConfigEntrypoints = va.symbol(c"vaQueryConfigEntrypoints")?;

        let display = get_display(file.as_raw_fd());
        ensure!(
            !display.is_null(),
            "libva couldn't use {}",
            render_node.display()
        );
        let (mut major, mut minor) = (0, 0);
        let status = initialize(display, &mut major, &mut minor);
        ensure!(
            status == 0,
            "vaInitialize on {} failed (status {status}): no VA-API driver for this device",
            render_node.display()
        );
        let result = (|| {
            let vendor = CStr::from_ptr(vendor_string(display))
                .to_string_lossy()
                .into_owned();
            let mut profiles = vec![0; max_profiles(display).max(0) as usize];
            let mut count = 0;
            let status = query_profiles(display, profiles.as_mut_ptr(), &mut count);
            ensure!(
                status == 0,
                "vaQueryConfigProfiles failed (status {status})"
            );
            profiles.truncate(count.max(0) as usize);
            let mut found = Vec::new();
            for profile in profiles {
                let mut entrypoints = vec![0; max_entrypoints(display).max(0) as usize];
                let mut count = 0;
                // A profile that can't be queried has none for us.
                if query_entrypoints(display, profile, entrypoints.as_mut_ptr(), &mut count) == 0 {
                    entrypoints.truncate(count.max(0) as usize);
                    found.push((profile, entrypoints));
                }
            }
            Ok(Caps {
                vendor,
                codecs: encodable(&found),
            })
        })();
        terminate(display);
        result
    }
}

/// The VA-API encoder: not built yet.
pub fn open(_params: Params) -> Result<Box<dyn VideoEncoder>> {
    bail!(
        "VA-API encoding isn't built yet (the device probe works: \
         cha-streamer --probe-device vaapi:<render node>)"
    )
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
    fn the_encoder_says_it_isnt_built() {
        let err = open(Params {
            codec: Codec::H264,
            width: 1280,
            height: 720,
            fps: 60,
            bitrate_bps: 1,
        })
        .err()
        .unwrap();
        assert!(err.to_string().contains("isn't built yet"));
    }
}

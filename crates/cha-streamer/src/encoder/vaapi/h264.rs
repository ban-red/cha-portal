//! H.264 on VA-API: what the parameter buffers say, from the stream's
//! settings. Pure code (no libva calls), so what it chooses is tested without a
//! GPU.
//!
//! The stream is built for the lowest latency a decoder can show:
//! - one slice per picture, every picture a reference, no B-frames, POC type
//!   0 with one reference (`P` follows the picture before it), an IDR when
//!   asked and at the start;
//! - CBR with a one-frame HRD buffer (the VBV x264 gets too);
//! - the SPS and PPS are ours (`bitstream::h264`), BT.709 limited range, with
//!   a VUI that says there is no reordering.
//!
//! The VA structs are `va_enc_h264.h`'s, from the libva API reference.

use anyhow::{Result, ensure};

use super::ffi;
use crate::encoder::Params;
use crate::encoder::bitstream::{
    self,
    h264::{
        self, CONSTRAINT_SET1, NAL_AUD, NAL_IDR, NAL_PPS, NAL_SEI, NAL_SLICE, NAL_SPS,
        PROFILE_BASELINE, PROFILE_HIGH, PROFILE_MAIN, Pps, SliceHeader, SliceKind, Sps, Vui,
    },
};

/// `log2_max_frame_num`: frame numbers wrap every 256 pictures.
pub const LOG2_MAX_FRAME_NUM: u32 = 8;
/// POC type 0 with a 12-bit LSB; the POC counts in twos (frame pictures).
pub const LOG2_MAX_POC_LSB: u32 = 12;
/// The POC counter wraps here, a multiple of the LSB range.
const POC_WRAP: u32 = 1 << 16;
/// The GOP length told to the driver. We choose every picture's type, so it
/// never ends one; a rate controller wants a number.
pub const ADVERTISED_GOP: u32 = 3600;
/// Pictures the stream refers to.
pub const REFERENCES: u32 = 1;

/// The H.264 profile the stream is made in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Profile {
    /// The VAProfile.
    pub va: i32,
    pub idc: u8,
    pub constraint_flags: u8,
    /// CABAC (Main and High) or CAVLC (Constrained Baseline).
    pub cabac: bool,
    /// The 8×8 transform (High).
    pub transform_8x8: bool,
    pub name: &'static str,
}

impl Profile {
    pub const HIGH: Profile = Profile {
        va: ffi::PROFILE_H264_HIGH,
        idc: PROFILE_HIGH,
        constraint_flags: 0,
        cabac: true,
        transform_8x8: true,
        name: "High",
    };
    pub const MAIN: Profile = Profile {
        va: ffi::PROFILE_H264_MAIN,
        idc: PROFILE_MAIN,
        constraint_flags: 0,
        cabac: true,
        transform_8x8: false,
        name: "Main",
    };
    pub const CONSTRAINED_BASELINE: Profile = Profile {
        va: ffi::PROFILE_H264_CONSTRAINED_BASELINE,
        idc: PROFILE_BASELINE,
        constraint_flags: CONSTRAINT_SET1,
        cabac: false,
        transform_8x8: false,
        name: "Constrained Baseline",
    };

    /// Best first: High compresses best, Baseline decodes anywhere.
    pub const PREFERENCE: [Profile; 3] = [Self::HIGH, Self::MAIN, Self::CONSTRAINED_BASELINE];
}

/// The SPS and PPS of a stream, and the picture size in macroblocks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    pub profile: Profile,
    pub sps: Sps,
    pub pps: Pps,
}

impl Layout {
    pub fn new(profile: Profile, params: &Params) -> Result<Self> {
        let (width, height) = (params.width, params.height);
        ensure!(
            width >= 16 && height >= 16 && width % 2 == 0 && height % 2 == 0,
            "{width}×{height} isn't a size H.264 4:2:0 takes"
        );
        let (width_mbs, height_mbs) = (width.div_ceil(16), height.div_ceil(16));
        // Cropping is in units of two pixels (4:2:0, frames).
        let crop_right = (width_mbs * 16 - width) / 2;
        let crop_bottom = (height_mbs * 16 - height) / 2;
        let sps = Sps {
            profile_idc: profile.idc,
            constraint_flags: profile.constraint_flags,
            level_idc: h264::level_for(width, height, params.fps),
            sps_id: 0,
            log2_max_frame_num: LOG2_MAX_FRAME_NUM,
            poc_type: 0,
            log2_max_poc_lsb: LOG2_MAX_POC_LSB,
            max_num_ref_frames: REFERENCES,
            width_mbs,
            height_mbs,
            direct_8x8_inference: true,
            crop: [0, crop_right, 0, crop_bottom],
            vui: Some(Vui::bt709_low_latency(params.fps, REFERENCES)),
        };
        let pps = Pps {
            pps_id: 0,
            sps_id: 0,
            cabac: profile.cabac,
            pic_init_qp: 26,
            chroma_qp_index_offset: 0,
            deblocking_filter_control_present: true,
            constrained_intra_pred: false,
            transform_8x8_mode: profile.transform_8x8.then_some(true),
        };
        Ok(Self { profile, sps, pps })
    }

    /// The macroblocks in a picture.
    pub fn macroblocks(&self) -> u32 {
        self.sps.width_mbs * self.sps.height_mbs
    }

    /// The coded (macroblock-aligned) size in pixels.
    pub fn coded_size(&self) -> (u32, u32) {
        (self.sps.width_mbs * 16, self.sps.height_mbs * 16)
    }

    /// The same stream at another frame rate: the level and the VUI's
    /// timing follow.
    pub fn with_fps(&mut self, params: &Params) {
        self.sps.level_idc = h264::level_for(params.width, params.height, params.fps);
        self.sps.vui = Some(Vui::bt709_low_latency(params.fps, REFERENCES));
    }
}

/// The rate control settings for a bitrate and frame rate: CBR whose buffer
/// holds one frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rate {
    pub bits_per_second: u32,
    /// The rate control window, in ms: one frame.
    pub window_ms: u32,
    /// The HRD buffer, in bits: one frame.
    pub buffer_bits: u32,
}

impl Rate {
    #[cfg(test)]
    pub fn new(bitrate_bps: u32, fps: u32) -> Self {
        Self::with_buffer(bitrate_bps, fps, 1)
    }

    /// A buffer of `frames` frames' worth of bits (1: the low-latency
    /// default; more lets a big frame, an IDR, borrow from the next ones).
    pub fn with_buffer(bitrate_bps: u32, fps: u32, frames: u32) -> Self {
        let (fps, frames) = (fps.max(1), frames.max(1));
        Self {
            bits_per_second: bitrate_bps.max(1),
            window_ms: (1000 * frames / fps).max(1),
            buffer_bits: (bitrate_bps / fps).saturating_mul(frames).max(1),
        }
    }
}

/// One picture's place in the stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Picture {
    pub idr: bool,
    pub frame_num: u32,
    /// The picture order count (twice the frames since the IDR, wrapped).
    pub poc: u32,
    pub idr_pic_id: u32,
}

impl Picture {
    pub fn poc_lsb(&self) -> u32 {
        self.poc % (1 << LOG2_MAX_POC_LSB)
    }

    pub fn kind(&self) -> SliceKind {
        if self.idr { SliceKind::I } else { SliceKind::P }
    }
}

/// Numbers the pictures: frame_num and POC count up from each IDR.
#[derive(Debug, Default)]
pub struct Gop {
    last: Option<Picture>,
    idr_pic_id: u32,
}

impl Gop {
    /// The next picture; a non-IDR one follows the last (an IDR if there is
    /// no last).
    pub fn next(&mut self, idr: bool) -> Picture {
        let picture = match (idr, self.last) {
            (false, Some(last)) => Picture {
                idr: false,
                frame_num: (last.frame_num + 1) % (1 << LOG2_MAX_FRAME_NUM),
                poc: (last.poc + 2) % POC_WRAP,
                idr_pic_id: last.idr_pic_id,
            },
            _ => {
                self.idr_pic_id = (self.idr_pic_id + 1) % 0x1_0000;
                Picture {
                    idr: true,
                    frame_num: 0,
                    poc: 0,
                    idr_pic_id: self.idr_pic_id,
                }
            }
        };
        self.last = Some(picture);
        picture
    }

    /// Forget the pictures: the next one is an IDR.
    pub fn reset(&mut self) {
        self.last = None;
    }
}

/// The slice header fields for `picture` (the experiment behind
/// `CHA_VAAPI_PACKED_HEADERS=slice`): with rate control the driver picks the
/// QP itself, so `slice_qp_delta` here is a guess.
pub fn slice_header(picture: &Picture) -> SliceHeader {
    SliceHeader {
        kind: picture.kind(),
        idr: picture.idr,
        frame_num: picture.frame_num,
        idr_pic_id: picture.idr_pic_id,
        poc_lsb: picture.poc_lsb(),
        qp_delta: 0,
        disable_deblocking_filter_idc: 0,
        alpha_c0_offset_div2: 0,
        beta_offset_div2: 0,
    }
}

// ---- VA parameter buffers (the structs are `sys.rs`'s, from libva's headers) ----

use super::sys;
pub use super::sys::VAPictureH264 as VaPicture;

/// A zeroed `T`, for the plain-data structs of `sys.rs` (integers, arrays
/// and unions of them).
///
/// # Safety
/// Every bit pattern of zeros must be a valid `T`.
pub(super) unsafe fn zeroed<T>() -> Box<T> {
    // SAFETY: the caller vouches for `T`.
    Box::new(unsafe { std::mem::zeroed() })
}

fn invalid_picture() -> VaPicture {
    VaPicture {
        picture_id: ffi::INVALID_SURFACE,
        frame_idx: 0,
        flags: ffi::PICTURE_H264_INVALID,
        TopFieldOrderCnt: 0,
        BottomFieldOrderCnt: 0,
        va_reserved: [0; 4],
    }
}

/// A short-term reference frame in `surface`.
fn frame_picture(surface: u32, picture: &Picture) -> VaPicture {
    VaPicture {
        picture_id: surface,
        frame_idx: picture.frame_num,
        flags: ffi::PICTURE_H264_SHORT_TERM_REFERENCE,
        TopFieldOrderCnt: picture.poc as i32,
        BottomFieldOrderCnt: picture.poc as i32,
        va_reserved: [0; 4],
    }
}

pub fn sequence_params(layout: &Layout, rate: &Rate) -> Box<sys::VAEncSequenceParameterBufferH264> {
    let sps = &layout.sps;
    // SAFETY: integers, arrays and unions of them.
    let mut p = unsafe { zeroed::<sys::VAEncSequenceParameterBufferH264>() };
    p.seq_parameter_set_id = sps.sps_id as u8;
    p.level_idc = sps.level_idc;
    p.intra_period = ADVERTISED_GOP;
    p.intra_idr_period = ADVERTISED_GOP;
    p.ip_period = 1; // no B-frames: a P every picture
    p.bits_per_second = rate.bits_per_second;
    p.max_num_ref_frames = sps.max_num_ref_frames;
    p.picture_width_in_mbs = sps.width_mbs as u16;
    p.picture_height_in_mbs = sps.height_mbs as u16;
    // SAFETY: a union of a u32 and bit-fields over it, zero-initialised.
    unsafe {
        let f = &mut p.seq_fields.bits;
        f.set_chroma_format_idc(1); // 4:2:0
        f.set_frame_mbs_only_flag(1);
        f.set_direct_8x8_inference_flag(u32::from(sps.direct_8x8_inference));
        f.set_log2_max_frame_num_minus4(sps.log2_max_frame_num - 4);
        f.set_pic_order_cnt_type(sps.poc_type);
        f.set_log2_max_pic_order_cnt_lsb_minus4(sps.log2_max_poc_lsb.saturating_sub(4));
    }
    let [left, right, top, bottom] = sps.crop;
    p.frame_cropping_flag = u8::from(sps.crop != [0; 4]);
    p.frame_crop_left_offset = left;
    p.frame_crop_right_offset = right;
    p.frame_crop_top_offset = top;
    p.frame_crop_bottom_offset = bottom;
    if let Some(vui) = &sps.vui {
        p.vui_parameters_present_flag = 1;
        // SAFETY: as above.
        unsafe {
            let f = &mut p.vui_fields.bits;
            f.set_timing_info_present_flag(u32::from(vui.timing.is_some()));
            f.set_bitstream_restriction_flag(1);
            f.set_log2_max_mv_length_horizontal(15);
            f.set_log2_max_mv_length_vertical(15);
        }
        if let Some((units, scale)) = vui.timing {
            p.num_units_in_tick = units;
            p.time_scale = scale;
        }
    }
    p
}

/// `current` is the surface the reconstruction goes to, `reference` the
/// previous picture's (for a P picture).
pub fn picture_params(
    layout: &Layout,
    picture: &Picture,
    current: u32,
    reference: Option<(u32, Picture)>,
    coded_buf: u32,
) -> Box<sys::VAEncPictureParameterBufferH264> {
    // SAFETY: integers, arrays and unions of them.
    let mut p = unsafe { zeroed::<sys::VAEncPictureParameterBufferH264>() };
    p.CurrPic = frame_picture(current, picture);
    p.ReferenceFrames = [invalid_picture(); 16];
    if let (false, Some((surface, last))) = (picture.idr, reference) {
        p.ReferenceFrames[0] = frame_picture(surface, &last);
    }
    p.coded_buf = coded_buf;
    p.pic_parameter_set_id = layout.pps.pps_id as u8;
    p.seq_parameter_set_id = layout.pps.sps_id as u8;
    p.frame_num = picture.frame_num as u16;
    p.pic_init_qp = layout.pps.pic_init_qp as u8;
    p.chroma_qp_index_offset = layout.pps.chroma_qp_index_offset as i8;
    p.second_chroma_qp_index_offset = layout.pps.chroma_qp_index_offset as i8;
    // SAFETY: a union of a u32 and bit-fields over it, zero-initialised.
    unsafe {
        let f = &mut p.pic_fields.bits;
        f.set_idr_pic_flag(u32::from(picture.idr));
        f.set_reference_pic_flag(1); // every picture is one
        f.set_entropy_coding_mode_flag(u32::from(layout.pps.cabac));
        f.set_constrained_intra_pred_flag(u32::from(layout.pps.constrained_intra_pred));
        f.set_transform_8x8_mode_flag(u32::from(layout.pps.transform_8x8_mode == Some(true)));
        f.set_deblocking_filter_control_present_flag(u32::from(
            layout.pps.deblocking_filter_control_present,
        ));
    }
    p
}

/// H.264 `slice_type` values the driver takes (0 = P, 2 = I).
const SLICE_P: u8 = 0;
const SLICE_I: u8 = 2;

pub fn slice_params(
    layout: &Layout,
    picture: &Picture,
    reference: Option<(u32, Picture)>,
) -> Box<sys::VAEncSliceParameterBufferH264> {
    // SAFETY: integers and arrays of them.
    let mut p = unsafe { zeroed::<sys::VAEncSliceParameterBufferH264>() };
    p.macroblock_address = 0;
    // No per-macroblock info buffer: zero would name buffer 0, which the
    // driver may read as one.
    p.macroblock_info = ffi::INVALID_ID;
    p.num_macroblocks = layout.macroblocks();
    p.slice_type = if picture.idr { SLICE_I } else { SLICE_P };
    p.pic_parameter_set_id = layout.pps.pps_id as u8;
    p.idr_pic_id = picture.idr_pic_id as u16;
    p.pic_order_cnt_lsb = picture.poc_lsb() as u16;
    p.RefPicList0 = [invalid_picture(); 32];
    p.RefPicList1 = [invalid_picture(); 32];
    if let (false, Some((surface, last))) = (picture.idr, reference) {
        p.RefPicList0[0] = frame_picture(surface, &last);
    }
    p.disable_deblocking_filter_idc = 0;
    p
}

/// A `VAEncMiscParameterBuffer` (its `type`) followed by the parameter
/// struct it announces: one buffer, as libva reads it.
#[repr(C)]
pub struct Misc<T> {
    pub kind: u32,
    pub body: T,
}

/// CBR at `rate`. `reset` tells the driver the rate control changed (not a
/// new one): it restarts its estimates from the new target.
pub fn misc_rate_control(rate: &Rate, reset: bool) -> Misc<sys::VAEncMiscParameterRateControl> {
    // SAFETY: integers and a union of them.
    let mut body = unsafe { zeroed::<sys::VAEncMiscParameterRateControl>() };
    body.bits_per_second = rate.bits_per_second;
    body.target_percentage = 100;
    body.window_size = rate.window_ms;
    // SAFETY: a union of a u32 and bit-fields over it, zero-initialised.
    unsafe {
        let f = &mut body.rc_flags.bits;
        f.set_reset(u32::from(reset));
        // Not a rate controller that skips frames or pads: every picture
        // we hand over comes out, and it takes only the bits it needs.
        f.set_disable_frame_skip(1);
        f.set_disable_bit_stuffing(1);
    }
    Misc {
        kind: ffi::MISC_RATE_CONTROL,
        body: *body,
    }
}

pub fn misc_hrd(rate: &Rate) -> Misc<sys::VAEncMiscParameterHRD> {
    Misc {
        kind: ffi::MISC_HRD,
        body: sys::VAEncMiscParameterHRD {
            initial_buffer_fullness: rate.buffer_bits,
            buffer_size: rate.buffer_bits,
            va_reserved: [0; 4],
        },
    }
}

pub fn misc_frame_rate(fps: u32) -> Misc<sys::VAEncMiscParameterFrameRate> {
    // SAFETY: integers and a union of them.
    let mut body = unsafe { zeroed::<sys::VAEncMiscParameterFrameRate>() };
    // Numerator in the low 16 bits, denominator (0 = 1) in the high 16.
    body.framerate = fps;
    Misc {
        kind: ffi::MISC_FRAME_RATE,
        body: *body,
    }
}

pub fn packed_header_params(
    kind: u32,
    bit_length: u32,
    has_emulation_bytes: bool,
) -> sys::VAEncPackedHeaderParameterBuffer {
    sys::VAEncPackedHeaderParameterBuffer {
        type_: kind,
        bit_length,
        has_emulation_bytes: u8::from(has_emulation_bytes),
        va_reserved: [0; 4],
    }
}

/// What a checked access unit holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccessUnit {
    /// NAL unit types in order.
    pub nals: Vec<u8>,
    pub idr: bool,
    /// The SPS, if there is one, parsed (the size is the cropped one).
    pub size: Option<(u32, u32)>,
}

/// Checks that `stream` is Annex-B H.264 shaped as the players expect: NAL
/// headers with the forbidden bit clear, an IDR access unit starting with
/// SPS then PPS then the IDR slice, and only slices otherwise.
pub fn check_access_unit(stream: &[u8]) -> Result<AccessUnit> {
    let units = h264::nal_units(stream);
    ensure!(!units.is_empty(), "no NAL units (no start code)");
    ensure!(
        units[0].start == 0,
        "{} bytes before the first start code",
        units[0].start
    );
    let mut nals = Vec::new();
    for unit in &units {
        let header = stream[unit.header];
        ensure!(
            header & 0x80 == 0,
            "forbidden_zero_bit set at {}",
            unit.header
        );
        ensure!(unit.end > unit.header + 1, "an empty NAL unit");
        nals.push(header & 0x1f);
    }
    let idr = nals.contains(&NAL_IDR);
    let mut size = None;
    if idr {
        ensure!(
            nals.starts_with(&[NAL_SPS, NAL_PPS, NAL_IDR])
                || nals.starts_with(&[NAL_AUD, NAL_SPS, NAL_PPS, NAL_IDR]),
            "an IDR picture should start with SPS, PPS, the IDR slice; the NAL types are {nals:?}"
        );
        let sps = units
            .iter()
            .find(|u| u.kind(stream) == NAL_SPS)
            .expect("checked above");
        let rbsp = bitstream::unescape(&stream[sps.header + 1..sps.end]);
        let parsed = h264::parse_sps(&rbsp).map_err(|e| anyhow::anyhow!("the SPS: {e}"))?;
        size = Some(parsed.picture_size());
    } else {
        ensure!(
            nals.iter()
                .all(|&n| matches!(n, NAL_SLICE | NAL_SEI | NAL_AUD)),
            "a non-IDR picture should hold slices only; the NAL types are {nals:?}"
        );
        ensure!(nals.contains(&NAL_SLICE), "no slice in the picture");
    }
    Ok(AccessUnit { nals, idr, size })
}

#[cfg(test)]
mod tests {
    use super::*;
    use cha_nvenc::Codec;

    fn params(width: u32, height: u32, fps: u32) -> Params {
        Params {
            codec: Codec::H264,
            width,
            height,
            fps,
            bitrate_bps: 40_000_000,
        }
    }

    #[test]
    fn sizes_that_arent_a_multiple_of_16_are_cropped() {
        let layout = Layout::new(Profile::HIGH, &params(1920, 1080, 60)).unwrap();
        assert_eq!((layout.sps.width_mbs, layout.sps.height_mbs), (120, 68));
        assert_eq!(layout.sps.crop, [0, 0, 0, 4]);
        assert_eq!(layout.sps.picture_size(), (1920, 1080));
        assert_eq!(layout.coded_size(), (1920, 1088));
        assert_eq!(layout.macroblocks(), 120 * 68);
        let layout = Layout::new(Profile::HIGH, &params(2560, 1440, 60)).unwrap();
        assert_eq!(layout.sps.crop, [0; 4]);
        assert_eq!(layout.sps.level_idc, 51);
        // A width of 1366: 86 MBs, 10 pixels of crop (5 units).
        let layout = Layout::new(Profile::MAIN, &params(1366, 768, 60)).unwrap();
        assert_eq!(layout.sps.crop, [0, 5, 0, 0]);
        assert_eq!(layout.sps.picture_size(), (1366, 768));
        assert!(Layout::new(Profile::HIGH, &params(1365, 768, 60)).is_err());
    }

    #[test]
    fn the_stream_is_low_latency_by_construction() {
        let layout = Layout::new(Profile::HIGH, &params(1920, 1080, 60)).unwrap();
        let vui = layout.sps.vui.as_ref().unwrap();
        assert_eq!(vui.max_num_reorder_frames, 0);
        assert_eq!(vui.max_dec_frame_buffering, 1);
        assert!(!vui.full_range);
        assert_eq!(
            (
                vui.colour_primaries,
                vui.transfer_characteristics,
                vui.matrix_coefficients
            ),
            (1, 1, 1)
        );
        assert_eq!(layout.sps.max_num_ref_frames, 1);
        assert_eq!(layout.pps.transform_8x8_mode, Some(true));
        let baseline = Layout::new(Profile::CONSTRAINED_BASELINE, &params(1920, 1080, 60)).unwrap();
        assert!(!baseline.pps.cabac);
        assert_eq!(baseline.pps.transform_8x8_mode, None);
        assert_eq!(baseline.sps.constraint_flags, CONSTRAINT_SET1);
    }

    #[test]
    fn the_frame_rate_changes_the_level_and_the_timing() {
        let mut layout = Layout::new(Profile::HIGH, &params(2560, 1440, 60)).unwrap();
        layout.with_fps(&params(2560, 1440, 120));
        assert_eq!(layout.sps.level_idc, 52);
        assert_eq!(layout.sps.vui.as_ref().unwrap().timing, Some((1, 240)));
    }

    #[test]
    fn the_hrd_buffer_is_one_frame() {
        let wide = Rate::with_buffer(24_000_000, 60, 4);
        assert_eq!((wide.buffer_bits, wide.window_ms), (1_600_000, 66));
        let rate = Rate::new(24_000_000, 60);
        assert_eq!(rate.bits_per_second, 24_000_000);
        assert_eq!(rate.buffer_bits, 400_000);
        assert_eq!(rate.window_ms, 16);
        let rate = Rate::new(24_000_000, 120);
        assert_eq!(rate.buffer_bits, 200_000);
        assert_eq!(rate.window_ms, 8);
        // Never zero.
        let rate = Rate::new(0, 120);
        assert!(rate.bits_per_second >= 1 && rate.buffer_bits >= 1);
    }

    #[test]
    fn pictures_are_numbered_from_each_idr() {
        let mut gop = Gop::default();
        // Nothing to refer to: the first picture is an IDR whatever is asked.
        let first = gop.next(false);
        assert!(first.idr);
        assert_eq!((first.frame_num, first.poc, first.idr_pic_id), (0, 0, 1));
        let second = gop.next(false);
        assert!(!second.idr);
        assert_eq!((second.frame_num, second.poc), (1, 2));
        assert_eq!(second.idr_pic_id, 1);
        let third = gop.next(false);
        assert_eq!((third.frame_num, third.poc), (2, 4));
        // A new IDR restarts the numbers and changes idr_pic_id.
        let idr = gop.next(true);
        assert!(idr.idr);
        assert_eq!((idr.frame_num, idr.poc, idr.idr_pic_id), (0, 0, 2));
        gop.reset();
        assert!(gop.next(false).idr);
    }

    #[test]
    fn frame_numbers_and_poc_wrap() {
        let mut gop = Gop::default();
        gop.next(true);
        let mut last = gop.next(false);
        for _ in 0..300 {
            let next = gop.next(false);
            assert_eq!(next.frame_num, (last.frame_num + 1) % 256);
            // The LSB the slice header carries moves by two each time.
            assert_eq!(next.poc_lsb(), (last.poc_lsb() + 2) % 4096);
            last = next;
        }
        assert!(last.poc < POC_WRAP);
    }

    #[test]
    fn sequence_params_say_what_the_sps_says() {
        let layout = Layout::new(Profile::HIGH, &params(1920, 1080, 60)).unwrap();
        let rate = Rate::new(40_000_000, 60);
        let seq = sequence_params(&layout, &rate);
        assert_eq!(seq.level_idc, 42);
        assert_eq!(seq.bits_per_second, 40_000_000);
        assert_eq!(
            (seq.picture_width_in_mbs, seq.picture_height_in_mbs),
            (120, 68)
        );
        assert_eq!(seq.max_num_ref_frames, 1);
        assert_eq!(seq.ip_period, 1);
        // chroma 4:2:0, frame_mbs_only, direct_8x8, log2_max_frame_num_minus4 4,
        // POC type 0, log2_max_poc_lsb_minus4 8.
        assert_eq!(unsafe { seq.seq_fields.value } & 3, 1);
        assert_eq!((unsafe { seq.seq_fields.value } >> 2) & 1, 1);
        assert_eq!((unsafe { seq.seq_fields.value } >> 3) & 1, 0);
        assert_eq!((unsafe { seq.seq_fields.value } >> 5) & 1, 1);
        assert_eq!((unsafe { seq.seq_fields.value } >> 6) & 0xf, 4);
        assert_eq!((unsafe { seq.seq_fields.value } >> 10) & 3, 0);
        assert_eq!((unsafe { seq.seq_fields.value } >> 12) & 0xf, 8);
        assert_eq!(seq.frame_cropping_flag, 1);
        assert_eq!(seq.frame_crop_bottom_offset, 4);
        assert_eq!(seq.vui_parameters_present_flag, 1);
        assert_eq!((unsafe { seq.vui_fields.value } >> 1) & 1, 1, "timing");
        assert_eq!(
            (unsafe { seq.vui_fields.value } >> 2) & 1,
            1,
            "bitstream restriction"
        );
        assert_eq!((seq.num_units_in_tick, seq.time_scale), (1, 120));
    }

    #[test]
    fn picture_params_refer_to_the_previous_picture() {
        let layout = Layout::new(Profile::HIGH, &params(1920, 1080, 60)).unwrap();
        let mut gop = Gop::default();
        let idr = gop.next(true);
        let p = picture_params(&layout, &idr, 10, None, 99);
        assert_eq!(p.CurrPic.picture_id, 10);
        assert_eq!(unsafe { p.pic_fields.value } & 1, 1, "idr_pic_flag");
        assert_eq!((unsafe { p.pic_fields.value } >> 3) & 1, 1, "cabac");
        assert_eq!((unsafe { p.pic_fields.value } >> 8) & 1, 1, "8x8 transform");
        assert_eq!(
            (unsafe { p.pic_fields.value } >> 9) & 1,
            1,
            "deblocking control"
        );
        assert!(p.ReferenceFrames.iter().all(|r| r.flags & 1 == 1));
        assert_eq!(p.coded_buf, 99);
        let next = gop.next(false);
        let p = picture_params(&layout, &next, 11, Some((10, idr)), 99);
        assert_eq!(unsafe { p.pic_fields.value } & 1, 0);
        assert_eq!(p.ReferenceFrames[0].picture_id, 10);
        assert_eq!(
            p.ReferenceFrames[0].flags,
            ffi::PICTURE_H264_SHORT_TERM_REFERENCE
        );
        assert!(
            p.ReferenceFrames[1..]
                .iter()
                .all(|r| r.picture_id == ffi::INVALID_SURFACE)
        );
        assert_eq!(p.frame_num, 1);
        assert_eq!(p.CurrPic.TopFieldOrderCnt, 2);
        // An IDR refers to nothing even if a reference is passed.
        let again = gop.next(true);
        let p = picture_params(&layout, &again, 10, Some((11, next)), 99);
        assert!(
            p.ReferenceFrames
                .iter()
                .all(|r| r.picture_id == ffi::INVALID_SURFACE)
        );
    }

    #[test]
    fn slice_params_cover_the_picture() {
        let layout = Layout::new(Profile::HIGH, &params(1920, 1080, 60)).unwrap();
        let mut gop = Gop::default();
        let idr = gop.next(true);
        let s = slice_params(&layout, &idr, None);
        assert_eq!(s.num_macroblocks, 120 * 68);
        assert_eq!(s.macroblock_info, ffi::INVALID_ID);
        assert_eq!(s.slice_type, 2);
        assert_eq!(s.idr_pic_id, 1);
        assert!(s.RefPicList0.iter().all(|r| r.flags & 1 == 1));
        let next = gop.next(false);
        let s = slice_params(&layout, &next, Some((10, idr)));
        assert_eq!(s.slice_type, 0);
        assert_eq!(s.RefPicList0[0].picture_id, 10);
        assert_eq!(s.pic_order_cnt_lsb, 2);
        assert!(
            s.RefPicList1
                .iter()
                .all(|r| r.picture_id == ffi::INVALID_SURFACE)
        );
    }

    #[test]
    fn rate_control_is_cbr_without_skipping_or_padding() {
        let rate = Rate::new(40_000_000, 60);
        let first = misc_rate_control(&rate, false);
        assert_eq!(first.kind, ffi::MISC_RATE_CONTROL);
        assert_eq!(first.body.target_percentage, 100);
        assert_eq!(unsafe { first.body.rc_flags.value }, 0b110);
        let changed = misc_rate_control(&rate, true);
        assert_eq!(unsafe { changed.body.rc_flags.value }, 0b111);
        let hrd = misc_hrd(&rate);
        assert_eq!(hrd.body.buffer_size, hrd.body.initial_buffer_fullness);
        assert_eq!(misc_frame_rate(90).body.framerate, 90);
    }

    #[test]
    fn va_struct_sizes_match_libvas_headers() {
        // The sizes of libva 2.22's structs on x86_64 Linux (measured by the
        // bindgen layout checks in sys.rs).
        assert_eq!(size_of::<VaPicture>(), 36);
        assert_eq!(size_of::<sys::VAEncSequenceParameterBufferH264>(), 1132);
        assert_eq!(size_of::<sys::VAEncPictureParameterBufferH264>(), 648);
        assert_eq!(size_of::<sys::VAEncSliceParameterBufferH264>(), 3140);
        assert_eq!(size_of::<sys::VAEncPackedHeaderParameterBuffer>(), 28);
        // A misc buffer is its 4-byte header and the struct.
        assert_eq!(size_of::<Misc<sys::VAEncMiscParameterRateControl>>(), 64);
        assert_eq!(size_of::<Misc<sys::VAEncMiscParameterHRD>>(), 28);
        assert_eq!(size_of::<Misc<sys::VAEncMiscParameterFrameRate>>(), 28);
        assert_eq!(size_of::<sys::VAProcPipelineParameterBuffer>(), 224);
        // Our own, for the debug read-back (not from sys.rs).
        assert_eq!(size_of::<ffi::Image>(), 120);
        assert_eq!(size_of::<ffi::ImageFormat>(), 48);
    }

    #[test]
    fn the_numbers_libva_defines_are_the_ones_we_use() {
        assert_eq!(ffi::RT_FORMAT_RGB32, 0x20000);
        assert_eq!(ffi::RT_FORMAT_YUV420, 1);
        assert_eq!(ffi::PICTURE_H264_INVALID, 1);
        assert_eq!(ffi::PICTURE_H264_SHORT_TERM_REFERENCE, 8);
        assert_eq!(ffi::FOURCC_NV12, ffi::fourcc(b'N', b'V', b'1', b'2'));
        assert_eq!(ffi::MISC_HRD, 5);
    }

    fn annexb(units: &[(u8, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        for (kind, body) in units {
            out.extend_from_slice(&[0, 0, 0, 1, 0x60 | kind]);
            out.extend_from_slice(body);
        }
        out
    }

    #[test]
    fn an_idr_access_unit_checks_out() {
        let layout = Layout::new(Profile::HIGH, &params(1920, 1080, 60)).unwrap();
        let mut stream = h264::sps_nal(&layout.sps);
        stream.extend_from_slice(&h264::pps_nal(&layout.pps));
        stream.extend_from_slice(&[0, 0, 0, 1, 0x65, 0x88, 1, 2]);
        let au = check_access_unit(&stream).unwrap();
        assert!(au.idr);
        assert_eq!(au.nals, [7, 8, 5]);
        assert_eq!(au.size, Some((1920, 1080)));
        let p = annexb(&[(1, &[0x9a, 1, 2])]);
        let au = check_access_unit(&p).unwrap();
        assert!(!au.idr);
        assert_eq!(au.size, None);
    }

    #[test]
    fn broken_access_units_are_refused() {
        // No start code.
        assert!(check_access_unit(&[1, 2, 3, 4]).is_err());
        // An IDR slice without the parameter sets in front.
        assert!(check_access_unit(&annexb(&[(5, &[0x88, 1])])).is_err());
        // PPS before SPS.
        let layout = Layout::new(Profile::HIGH, &params(1920, 1080, 60)).unwrap();
        let mut stream = h264::pps_nal(&layout.pps);
        stream.extend_from_slice(&h264::sps_nal(&layout.sps));
        stream.extend_from_slice(&[0, 0, 0, 1, 0x65, 0x88]);
        assert!(check_access_unit(&stream).is_err());
        // Forbidden bit.
        assert!(check_access_unit(&[0, 0, 0, 1, 0xe1, 9]).is_err());
        // Parameter sets without a slice, in a non-IDR unit.
        assert!(check_access_unit(&annexb(&[(7, &[1, 2])])).is_err());
    }
}

//! HEVC (H.265 Main, 8-bit) on VA-API: what the parameter buffers say, from the
//! stream's settings. Pure code (no libva calls), so what it chooses is tested
//! without a GPU. The counterpart of `h264.rs`; the rate control buffers
//! (`Rate`, `misc_*`) and the packed header parameters are that file's.
//!
//! The stream is built for the lowest latency a decoder can show, like the
//! H.264 one:
//! - one slice per picture, every picture a reference, no B-pictures, one
//!   reference (the picture before), an IDR when asked and at the start;
//! - CBR with the same HRD buffer;
//! - the VPS, SPS and PPS are ours (`bitstream::h265`), BT.709 limited range,
//!   and so is the slice header, because HEVC's depends on the SPS in ways the
//!   driver is not told (the POC bits).
//!
//! The VA structs are `va_enc_hevc.h`'s, from the libva API reference.

use anyhow::{Result, ensure};

use super::ffi;
use super::h264::{Rate, zeroed};
use super::sys;
use crate::encoder::Params;
use crate::encoder::bitstream::h265::{
    self, NAL_IDR_N_LP, NAL_TRAIL_R, PROFILE_MAIN, Pps, SLICE_I, SLICE_P, SliceHeader, Sps, Vui,
};

/// `log2_max_pic_order_cnt_lsb`: the slice header carries the POC modulo 256.
pub const LOG2_MAX_POC_LSB: u32 = 8;
/// The POC grows by one a picture and is passed to the driver as it is; at
/// this count the next picture is an IDR instead (about 200 days at 60 fps).
const POC_LIMIT: u32 = 1 << 30;
/// The GOP length told to the driver. We choose every picture's type, so it
/// never ends one; a rate controller wants a number.
pub const ADVERTISED_GOP: u32 = 3600;
/// Pictures the stream refers to.
pub const REFERENCES: u32 = 1;
/// `MaxNumMergeCand`.
const MAX_MERGE_CANDIDATES: u32 = 5;
/// The CTB size the sizes are fitted to when the driver doesn't say.
const CTB_LOG2: u32 = 5;

/// `VA_FEATURE_REQUIRED`: a value in `VAConfigAttribEncHEVCFeatures`.
const FEATURE_REQUIRED: u32 = 2;
const FEATURE_SUPPORTED: u32 = 1;

/// Block sizes of the coding tree, as the SPS says them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Blocks {
    pub log2_ctb: u32,
    pub log2_min_cb: u32,
    pub log2_min_tb: u32,
    pub log2_max_tb: u32,
    pub depth_inter: u32,
    pub depth_intra: u32,
}

impl Blocks {
    /// 32×32 coding tree blocks over 8×8 coding blocks, 4×4 to 32×32
    /// transforms, depth 2: what Intel's Gen9 encoder takes, and a size every
    /// decoder handles.
    pub const DEFAULT: Blocks = Blocks {
        log2_ctb: CTB_LOG2,
        log2_min_cb: 3,
        log2_min_tb: 2,
        log2_max_tb: 5,
        depth_inter: 2,
        depth_intra: 2,
    };

    /// The defaults fitted to `VAConfigAttribEncHEVCBlockSizes`; the defaults
    /// themselves if the driver doesn't say.
    pub fn from_attribute(value: Option<u32>) -> Self {
        let mut b = Self::DEFAULT;
        let Some(value) = value else { return b };
        let field = |shift: u32| (value >> shift) & 3;
        let (max_ctb, min_ctb) = (field(0) + 3, field(2) + 3);
        let min_cb = field(4) + 3;
        let max_tb = field(6) + 2;
        let min_tb = field(8) + 2;
        let (max_depth_inter, min_depth_inter) = (field(10), field(12));
        let (max_depth_intra, min_depth_intra) = (field(14), field(16));
        b.log2_ctb = b.log2_ctb.clamp(min_ctb.min(max_ctb), max_ctb);
        b.log2_min_cb = b.log2_min_cb.max(min_cb).min(b.log2_ctb);
        b.log2_max_tb = b.log2_max_tb.min(max_tb).min(b.log2_ctb);
        b.log2_min_tb = b
            .log2_min_tb
            .max(min_tb)
            .min(b.log2_min_cb - 1)
            .min(b.log2_max_tb);
        b.depth_inter = b
            .depth_inter
            .clamp(min_depth_inter.min(max_depth_inter), max_depth_inter);
        b.depth_intra = b
            .depth_intra
            .clamp(min_depth_intra.min(max_depth_intra), max_depth_intra);
        b
    }
}

/// The coding tools the stream uses: ours to leave off, the driver's to
/// insist on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tools {
    pub amp: bool,
    pub sao: bool,
    pub temporal_mvp: bool,
    pub strong_intra_smoothing: bool,
    pub sign_data_hiding: bool,
    pub transform_skip: bool,
    pub cu_qp_delta: bool,
}

impl Tools {
    /// Temporal MVP, strong intra smoothing and QP deltas in coding units
    /// when the driver allows them (or says nothing: Intel's iHD doesn't
    /// answer, and its rate control needs `cu_qp_delta` to change the QP;
    /// without it the slice header's QP is wrong and the picture is garbage,
    /// measured on a UHD 630); the rest off unless the driver *requires*
    /// them. `features` is `VAConfigAttribEncHEVCFeatures`.
    pub fn from_attribute(features: Option<u32>) -> Self {
        let Some(value) = features else {
            return Self {
                amp: false,
                sao: false,
                temporal_mvp: true,
                strong_intra_smoothing: true,
                sign_data_hiding: false,
                transform_skip: false,
                cu_qp_delta: true,
            };
        };
        // Two bits a feature, in the header's order.
        let level = |index: u32| (value >> (2 * index)) & 3;
        let required = |index| level(index) == FEATURE_REQUIRED;
        let allowed = |index| level(index) >= FEATURE_SUPPORTED;
        Self {
            amp: required(2),
            sao: required(3),
            temporal_mvp: allowed(5),
            strong_intra_smoothing: allowed(6),
            sign_data_hiding: required(8),
            transform_skip: required(10),
            cu_qp_delta: allowed(11),
        }
    }
}

/// The VPS/SPS/PPS inputs of a stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    pub blocks: Blocks,
    pub tools: Tools,
    pub sps: Sps,
    pub pps: Pps,
}

impl Layout {
    pub fn new(blocks: Blocks, tools: Tools, params: &Params) -> Result<Self> {
        let (width, height) = (params.width, params.height);
        ensure!(
            width >= 16 && height >= 16 && width % 2 == 0 && height % 2 == 0,
            "{width}×{height} isn't a size H.265 4:2:0 takes"
        );
        // The coded size is a multiple of 16 (more than the 8 the coding
        // blocks need: the driver's surfaces are laid out that way), cropped
        // in units of two pixels.
        let (coded_width, coded_height) = (width.next_multiple_of(16), height.next_multiple_of(16));
        let crop_right = (coded_width - width) / 2;
        let crop_bottom = (coded_height - height) / 2;
        let sps = Sps {
            level_idc: h265::level_for(coded_width, coded_height, params.fps),
            sps_id: 0,
            width: coded_width,
            height: coded_height,
            crop: [0, crop_right, 0, crop_bottom],
            log2_max_poc_lsb: LOG2_MAX_POC_LSB,
            max_dec_pic_buffering: REFERENCES + 1,
            log2_min_cb: blocks.log2_min_cb,
            log2_ctb: blocks.log2_ctb,
            log2_min_tb: blocks.log2_min_tb,
            log2_max_tb: blocks.log2_max_tb,
            max_transform_depth_inter: blocks.depth_inter,
            max_transform_depth_intra: blocks.depth_intra,
            amp: tools.amp,
            sao: tools.sao,
            temporal_mvp: tools.temporal_mvp,
            strong_intra_smoothing: tools.strong_intra_smoothing,
            vui: Some(Vui::bt709(params.fps)),
        };
        let pps = Pps {
            pps_id: 0,
            sps_id: 0,
            init_qp: 26,
            chroma_qp_offset: 0,
            cu_qp_delta_depth: tools.cu_qp_delta.then_some(0),
            sign_data_hiding: tools.sign_data_hiding,
            transform_skip: tools.transform_skip,
        };
        Ok(Self {
            blocks,
            tools,
            sps,
            pps,
        })
    }

    /// The coded (16-aligned) size in pixels.
    pub fn coded_size(&self) -> (u32, u32) {
        (self.sps.width, self.sps.height)
    }

    /// The coding tree blocks in a picture.
    pub fn ctus(&self) -> u32 {
        let ctb = 1 << self.blocks.log2_ctb;
        self.sps.width.div_ceil(ctb) * self.sps.height.div_ceil(ctb)
    }

    /// The same stream at another frame rate: the level and the VUI's
    /// timing follow.
    pub fn with_fps(&mut self, params: &Params) {
        let (width, height) = self.coded_size();
        self.sps.level_idc = h265::level_for(width, height, params.fps);
        self.sps.vui = Some(Vui::bt709(params.fps));
    }
}

/// One picture's place in the stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Picture {
    pub idr: bool,
    /// The picture order count: the pictures since the IDR.
    pub poc: u32,
}

impl Picture {
    pub fn poc_lsb(&self) -> u32 {
        self.poc % (1 << LOG2_MAX_POC_LSB)
    }

    pub fn nal_unit_type(&self) -> u8 {
        if self.idr { NAL_IDR_N_LP } else { NAL_TRAIL_R }
    }
}

/// Numbers the pictures: the POC counts up from each IDR.
#[derive(Debug, Default)]
pub struct Gop {
    last: Option<Picture>,
}

impl Gop {
    /// The next picture; a non-IDR one follows the last (an IDR if there is
    /// no last).
    pub fn next(&mut self, idr: bool) -> Picture {
        let picture = match self.last {
            Some(last) if !idr && last.poc + 1 < POC_LIMIT => Picture {
                idr: false,
                poc: last.poc + 1,
            },
            _ => Picture { idr: true, poc: 0 },
        };
        self.last = Some(picture);
        picture
    }

    /// Forget the pictures: the next one is an IDR.
    pub fn reset(&mut self) {
        self.last = None;
    }
}

/// The slice header fields for `picture`. The slice QP is the PPS's (delta
/// 0): under rate control the driver moves the QP per coding tree block with
/// `cu_qp_delta`, which is why that tool is on (see [`Tools`]).
pub fn slice_header(picture: &Picture) -> SliceHeader {
    SliceHeader {
        idr: picture.idr,
        poc_lsb: picture.poc_lsb(),
        qp_delta: 0,
        max_merge_candidates: MAX_MERGE_CANDIDATES,
    }
}

// ---- VA parameter buffers (the structs are `sys.rs`'s, from libva's headers) ----

pub use super::sys::VAPictureHEVC as VaPicture;

fn invalid_picture() -> VaPicture {
    VaPicture {
        picture_id: ffi::INVALID_SURFACE,
        pic_order_cnt: 0,
        flags: ffi::PICTURE_HEVC_INVALID,
        va_reserved: [0; 4],
    }
}

/// The picture being coded, in `surface`.
fn current_picture(surface: u32, picture: &Picture) -> VaPicture {
    VaPicture {
        picture_id: surface,
        pic_order_cnt: picture.poc as i32,
        flags: 0,
        va_reserved: [0; 4],
    }
}

/// A reference picture in `surface`, one the current picture refers to and
/// that comes before it.
fn reference_picture(surface: u32, picture: &Picture) -> VaPicture {
    VaPicture {
        picture_id: surface,
        pic_order_cnt: picture.poc as i32,
        flags: ffi::PICTURE_HEVC_RPS_ST_CURR_BEFORE,
        va_reserved: [0; 4],
    }
}

pub fn sequence_params(layout: &Layout, rate: &Rate) -> Box<sys::VAEncSequenceParameterBufferHEVC> {
    let sps = &layout.sps;
    // SAFETY: integers, arrays and unions of them.
    let mut p = unsafe { zeroed::<sys::VAEncSequenceParameterBufferHEVC>() };
    p.general_profile_idc = PROFILE_MAIN;
    p.general_level_idc = sps.level_idc;
    p.general_tier_flag = 0;
    p.intra_period = ADVERTISED_GOP;
    p.intra_idr_period = ADVERTISED_GOP;
    p.ip_period = 1; // no B-pictures: a P every picture
    p.bits_per_second = rate.bits_per_second;
    p.pic_width_in_luma_samples = sps.width as u16;
    p.pic_height_in_luma_samples = sps.height as u16;
    // SAFETY: a union of a u32 and bit-fields over it, zero-initialised.
    unsafe {
        let f = &mut p.seq_fields.bits;
        f.set_chroma_format_idc(1); // 4:2:0
        f.set_amp_enabled_flag(u32::from(sps.amp));
        f.set_sample_adaptive_offset_enabled_flag(u32::from(sps.sao));
        f.set_sps_temporal_mvp_enabled_flag(u32::from(sps.temporal_mvp));
        f.set_strong_intra_smoothing_enabled_flag(u32::from(sps.strong_intra_smoothing));
        f.set_low_delay_seq(1); // I and P only
    }
    p.log2_min_luma_coding_block_size_minus3 = (sps.log2_min_cb - 3) as u8;
    p.log2_diff_max_min_luma_coding_block_size = (sps.log2_ctb - sps.log2_min_cb) as u8;
    p.log2_min_transform_block_size_minus2 = (sps.log2_min_tb - 2) as u8;
    p.log2_diff_max_min_transform_block_size = (sps.log2_max_tb - sps.log2_min_tb) as u8;
    p.max_transform_hierarchy_depth_inter = sps.max_transform_depth_inter as u8;
    p.max_transform_hierarchy_depth_intra = sps.max_transform_depth_intra as u8;
    if let Some(vui) = &sps.vui {
        p.vui_parameters_present_flag = 1;
        // SAFETY: as above.
        unsafe {
            let f = &mut p.vui_fields.bits;
            f.set_vui_timing_info_present_flag(u32::from(vui.timing.is_some()));
            f.set_bitstream_restriction_flag(1);
            f.set_motion_vectors_over_pic_boundaries_flag(1);
            f.set_restricted_ref_pic_lists_flag(1);
            f.set_log2_max_mv_length_horizontal(15);
            f.set_log2_max_mv_length_vertical(15);
        }
        if let Some((units, scale)) = vui.timing {
            p.vui_num_units_in_tick = units;
            p.vui_time_scale = scale;
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
) -> Box<sys::VAEncPictureParameterBufferHEVC> {
    // SAFETY: integers, arrays and unions of them.
    let mut p = unsafe { zeroed::<sys::VAEncPictureParameterBufferHEVC>() };
    p.decoded_curr_pic = current_picture(current, picture);
    p.reference_frames = [invalid_picture(); 15];
    let reference = reference.filter(|_| !picture.idr);
    if let Some((surface, last)) = reference {
        p.reference_frames[0] = reference_picture(surface, &last);
    }
    p.coded_buf = coded_buf;
    // Index into `reference_frames`; 0xff when no picture uses a collocated one.
    p.collocated_ref_pic_index = if reference.is_some() && layout.tools.temporal_mvp {
        0
    } else {
        0xff
    };
    p.pic_init_qp = layout.pps.init_qp as u8;
    p.diff_cu_qp_delta_depth = layout.pps.cu_qp_delta_depth.unwrap_or(0) as u8;
    p.pps_cb_qp_offset = layout.pps.chroma_qp_offset as i8;
    p.pps_cr_qp_offset = layout.pps.chroma_qp_offset as i8;
    p.slice_pic_parameter_set_id = layout.pps.pps_id as u8;
    p.nal_unit_type = picture.nal_unit_type();
    // SAFETY: a union of a u32 and bit-fields over it, zero-initialised.
    unsafe {
        let f = &mut p.pic_fields.bits;
        f.set_idr_pic_flag(u32::from(picture.idr));
        f.set_coding_type(if picture.idr { 1 } else { 2 }); // I, P
        f.set_reference_pic_flag(1); // every picture is one
        f.set_sign_data_hiding_enabled_flag(u32::from(layout.pps.sign_data_hiding));
        f.set_transform_skip_enabled_flag(u32::from(layout.pps.transform_skip));
        f.set_cu_qp_delta_enabled_flag(u32::from(layout.pps.cu_qp_delta_depth.is_some()));
    }
    p
}

pub fn slice_params(
    layout: &Layout,
    picture: &Picture,
    reference: Option<(u32, Picture)>,
) -> Box<sys::VAEncSliceParameterBufferHEVC> {
    // SAFETY: integers, arrays and unions of them.
    let mut p = unsafe { zeroed::<sys::VAEncSliceParameterBufferHEVC>() };
    p.slice_segment_address = 0;
    p.num_ctu_in_slice = layout.ctus();
    p.slice_type = if picture.idr { SLICE_I } else { SLICE_P };
    p.slice_pic_parameter_set_id = layout.pps.pps_id as u8;
    p.ref_pic_list0 = [invalid_picture(); 15];
    p.ref_pic_list1 = [invalid_picture(); 15];
    let reference = reference.filter(|_| !picture.idr);
    if let Some((surface, last)) = reference {
        p.ref_pic_list0[0] = reference_picture(surface, &last);
    }
    p.max_num_merge_cand = MAX_MERGE_CANDIDATES as u8;
    // SAFETY: a union of a u32 and bit-fields over it, zero-initialised.
    unsafe {
        let f = &mut p.slice_fields.bits;
        f.set_last_slice_of_pic_flag(1);
        f.set_slice_temporal_mvp_enabled_flag(u32::from(
            reference.is_some() && layout.tools.temporal_mvp,
        ));
        f.set_slice_sao_luma_flag(u32::from(layout.tools.sao));
        f.set_slice_sao_chroma_flag(u32::from(layout.tools.sao));
        f.set_collocated_from_l0_flag(1);
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;
    use cha_nvenc::Codec;

    fn params(width: u32, height: u32, fps: u32) -> Params {
        Params {
            codec: Codec::Hevc,
            width,
            height,
            fps,
            bitrate_bps: 40_000_000,
        }
    }

    fn layout(width: u32, height: u32) -> Layout {
        Layout::new(
            Blocks::DEFAULT,
            Tools::from_attribute(None),
            &params(width, height, 60),
        )
        .unwrap()
    }

    #[test]
    fn sizes_are_coded_in_16s_and_cropped() {
        let layout = layout(1920, 1080);
        assert_eq!(layout.coded_size(), (1920, 1088));
        assert_eq!(layout.sps.crop, [0, 0, 0, 4]);
        assert_eq!(layout.sps.picture_size(), (1920, 1080));
        // 32×32 CTBs: 60 × 34.
        assert_eq!(layout.ctus(), 60 * 34);
        let layout = self::layout(1366, 768);
        assert_eq!(layout.coded_size(), (1376, 768));
        assert_eq!(layout.sps.crop, [0, 5, 0, 0]);
        assert_eq!(layout.sps.picture_size(), (1366, 768));
        assert!(
            Layout::new(
                Blocks::DEFAULT,
                Tools::from_attribute(None),
                &params(1365, 768, 60)
            )
            .is_err()
        );
        // 1440p60 needs level 5 (Table A.8: 221184000 samples a second).
        assert_eq!(self::layout(2560, 1440).sps.level_idc, 150);
    }

    #[test]
    fn the_stream_is_low_latency_by_construction() {
        let layout = layout(1280, 720);
        assert_eq!(layout.sps.max_dec_pic_buffering, 2);
        let vui = layout.sps.vui.as_ref().unwrap();
        assert!(!vui.full_range);
        assert_eq!(
            (
                vui.colour_primaries,
                vui.transfer_characteristics,
                vui.matrix_coefficients
            ),
            (1, 1, 1)
        );
        assert_eq!(vui.timing, Some((1, 60)));
        let rate = Rate::with_buffer(40_000_000, 60, 1);
        let seq = sequence_params(&layout, &rate);
        assert_eq!((seq.general_profile_idc, seq.ip_period), (1, 1));
        assert_eq!(seq.general_tier_flag, 0);
        // SAFETY: a plain integer union.
        unsafe {
            assert_eq!(seq.seq_fields.bits.chroma_format_idc(), 1);
            assert_eq!(seq.seq_fields.bits.bit_depth_luma_minus8(), 0);
            assert_eq!(seq.seq_fields.bits.low_delay_seq(), 1);
        }
        assert_eq!(seq.log2_diff_max_min_luma_coding_block_size, 2);
        assert_eq!(seq.log2_diff_max_min_transform_block_size, 3);
        assert_eq!(seq.vui_time_scale, 60);
    }

    #[test]
    fn pictures_count_up_from_each_idr() {
        let mut gop = Gop::default();
        let first = gop.next(false);
        assert_eq!(first, Picture { idr: true, poc: 0 });
        assert_eq!(gop.next(false), Picture { idr: false, poc: 1 });
        assert_eq!(gop.next(false).poc, 2);
        assert_eq!(gop.next(true), Picture { idr: true, poc: 0 });
        gop.reset();
        assert!(gop.next(false).idr);
        // The lsb wraps, the POC passed to the driver does not.
        let wrapped = Picture {
            idr: false,
            poc: 300,
        };
        assert_eq!(wrapped.poc_lsb(), 44);
        assert_eq!(wrapped.nal_unit_type(), NAL_TRAIL_R);
        assert_eq!(first.nal_unit_type(), NAL_IDR_N_LP);
    }

    #[test]
    fn a_p_picture_refers_to_the_one_before_and_an_idr_to_none() {
        let layout = layout(1280, 720);
        let idr = Picture { idr: true, poc: 0 };
        let p = Picture { idr: false, poc: 1 };
        let pic = picture_params(&layout, &p, 7, Some((6, idr)), 9);
        assert_eq!(pic.decoded_curr_pic.picture_id, 7);
        assert_eq!(pic.decoded_curr_pic.pic_order_cnt, 1);
        assert_eq!(pic.reference_frames[0].picture_id, 6);
        assert_eq!(
            pic.reference_frames[0].flags,
            ffi::PICTURE_HEVC_RPS_ST_CURR_BEFORE
        );
        assert_eq!(pic.reference_frames[1].flags, ffi::PICTURE_HEVC_INVALID);
        assert_eq!(pic.collocated_ref_pic_index, 0);
        assert_eq!(pic.nal_unit_type, NAL_TRAIL_R);
        // SAFETY: a plain integer union.
        unsafe {
            assert_eq!(pic.pic_fields.bits.coding_type(), 2);
            assert_eq!(pic.pic_fields.bits.idr_pic_flag(), 0);
        }
        let slice = slice_params(&layout, &p, Some((6, idr)));
        assert_eq!((slice.slice_type, slice.num_ctu_in_slice), (1, 40 * 23));
        assert_eq!(slice.ref_pic_list0[0].picture_id, 6);
        assert_eq!(slice.ref_pic_list1[0].flags, ffi::PICTURE_HEVC_INVALID);
        let pic = picture_params(&layout, &idr, 6, Some((7, p)), 9);
        assert_eq!(pic.reference_frames[0].flags, ffi::PICTURE_HEVC_INVALID);
        assert_eq!(pic.collocated_ref_pic_index, 0xff);
        assert_eq!(pic.nal_unit_type, NAL_IDR_N_LP);
        let slice = slice_params(&layout, &idr, Some((7, p)));
        assert_eq!(slice.slice_type, 2);
        assert_eq!(slice.ref_pic_list0[0].flags, ffi::PICTURE_HEVC_INVALID);
    }

    #[test]
    fn block_sizes_follow_what_the_driver_says() {
        // Nothing said: the defaults.
        assert_eq!(Blocks::from_attribute(None), Blocks::DEFAULT);
        // CTB 16 to 64 (minus3: 1, 3), min CB 8, TB 4 to 32 (minus2: 3, 0),
        // depth 0 to 3 for both.
        let wide = 3 | (1 << 2) | (3 << 6) | (3 << 10) | (3 << 14);
        assert_eq!(Blocks::from_attribute(Some(wide)), Blocks::DEFAULT);
        // Only 16×16 and 64×64 CTBs... 64 only: the CTB moves up to it.
        let only_64 = 3 | (3 << 2) | (3 << 6) | (3 << 10) | (3 << 14);
        let b = Blocks::from_attribute(Some(only_64));
        assert_eq!((b.log2_ctb, b.log2_max_tb), (6, 5));
        // Only 16×16: the CTB moves down, the transform follows it.
        let only_16 = 1 | (1 << 2) | (3 << 6) | (3 << 10) | (3 << 14);
        let b = Blocks::from_attribute(Some(only_16));
        assert_eq!((b.log2_ctb, b.log2_max_tb), (4, 4));
        // A minimum coding block of 16: the transforms stay below it.
        let cb16 = wide | (1 << 4);
        let b = Blocks::from_attribute(Some(cb16));
        assert_eq!((b.log2_min_cb, b.log2_min_tb), (4, 2));
    }

    #[test]
    fn tools_are_off_unless_the_driver_requires_them() {
        let none = Tools::from_attribute(None);
        assert!(none.temporal_mvp && none.strong_intra_smoothing);
        assert!(none.cu_qp_delta);
        assert!(!none.sao && !none.amp);
        // Every feature supported: still only the cheap ones.
        let supported = 0x5555_5555 & 0x3fff_ffff;
        let tools = Tools::from_attribute(Some(supported));
        assert!(tools.temporal_mvp && tools.strong_intra_smoothing);
        assert!(tools.cu_qp_delta);
        assert!(!tools.sao && !tools.amp && !tools.sign_data_hiding);
        // SAO (index 3) and cu_qp_delta (index 11) required, temporal MVP
        // (5) not supported, strong intra smoothing (6) supported.
        let value = (2 << 6) | (2 << 22) | (1 << 12);
        let tools = Tools::from_attribute(Some(value));
        assert!(tools.sao && tools.cu_qp_delta);
        assert!(!tools.temporal_mvp && tools.strong_intra_smoothing);
    }
}

//! H.265 (HEVC) Main, 8-bit 4:2:0 bitstream syntax, written by us: the VPS, SPS
//! and PPS the VA-API encoder packs itself, the slice segment header, and the
//! Annex-B helpers to check what a driver returns. The bit writer and the
//! emulation prevention are [`super`]'s, shared with H.264.
//!
//! The stream is built for the lowest latency a decoder can show, like the
//! H.264 one: one slice per picture, IP only, one reference picture (the one
//! before), `sps_max_num_reorder_pics = 0`, BT.709 limited range in the VUI.
//!
//! Written from ITU-T H.265 (7.3.1.1 NAL unit header, 7.3.2.1 VPS, 7.3.2.2.1
//! SPS, 7.3.2.3.1 PPS, 7.3.3 profile/tier/level, 7.3.6.1 slice segment header,
//! 7.3.7 short-term reference picture set, E.2.1 VUI, A.4 levels). Nothing in
//! it comes from another project's code.

use super::{BitReader, BitWriter, escape, unescape};

pub const NAL_TRAIL_R: u8 = 1;
pub const NAL_IDR_W_RADL: u8 = 19;
pub const NAL_IDR_N_LP: u8 = 20;
pub const NAL_VPS: u8 = 32;
pub const NAL_SPS: u8 = 33;
pub const NAL_PPS: u8 = 34;
pub const NAL_AUD: u8 = 35;
pub const NAL_SEI_PREFIX: u8 = 39;
pub const NAL_SEI_SUFFIX: u8 = 40;

/// The bytes of a NAL unit header (the start code is not counted).
pub const NAL_HEADER_LEN: usize = 2;

/// `general_profile_idc` of Main.
pub const PROFILE_MAIN: u8 = 1;

/// Appends one NAL unit with a four-byte start code, a header for layer 0 and
/// temporal id 0, and `payload` (the RBSP; emulation prevention is added here).
pub fn write_nal(out: &mut Vec<u8>, kind: u8, payload: &[u8]) {
    // forbidden_zero_bit, nal_unit_type (6), nuh_layer_id (6), and
    // nuh_temporal_id_plus1 (3) = 1.
    out.extend_from_slice(&[0, 0, 0, 1, (kind & 0x3f) << 1, 1]);
    out.extend_from_slice(&escape(payload));
}

/// The `nal_unit_type` of a NAL unit from its first header byte.
pub fn nal_kind(first_header_byte: u8) -> u8 {
    (first_header_byte >> 1) & 0x3f
}

/// Whether `kind` is an IDR slice.
pub fn is_idr(kind: u8) -> bool {
    matches!(kind, NAL_IDR_W_RADL | NAL_IDR_N_LP)
}

/// The VUI we write: colour description and timing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Vui {
    pub full_range: bool,
    /// H.273 code points (1 = BT.709).
    pub colour_primaries: u8,
    pub transfer_characteristics: u8,
    pub matrix_coefficients: u8,
    /// `(num_units_in_tick, time_scale)`.
    pub timing: Option<(u32, u32)>,
}

impl Vui {
    /// BT.709 limited range at `fps` (one tick a frame).
    pub fn bt709(fps: u32) -> Self {
        Self {
            full_range: false,
            colour_primaries: 1,
            transfer_characteristics: 1,
            matrix_coefficients: 1,
            timing: Some((1, fps.max(1))),
        }
    }
}

/// A sequence parameter set for progressive 4:2:0 8-bit Main video with one
/// short-term reference picture set (the picture before) and no reordering.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sps {
    /// `general_level_idc`: 30 times the level.
    pub level_idc: u8,
    pub sps_id: u32,
    /// The coded size, a multiple of the minimum coding block.
    pub width: u32,
    pub height: u32,
    /// The conformance window, in chroma units (two luma pixels): left,
    /// right, top, bottom.
    pub crop: [u32; 4],
    pub log2_max_poc_lsb: u32,
    /// The decoded picture buffer, the current picture included.
    pub max_dec_pic_buffering: u32,
    pub log2_min_cb: u32,
    pub log2_ctb: u32,
    pub log2_min_tb: u32,
    pub log2_max_tb: u32,
    pub max_transform_depth_inter: u32,
    pub max_transform_depth_intra: u32,
    /// Asymmetric motion partitions.
    pub amp: bool,
    /// Sample adaptive offset (the slice headers then say it is on).
    pub sao: bool,
    pub temporal_mvp: bool,
    pub strong_intra_smoothing: bool,
    pub vui: Option<Vui>,
}

impl Sps {
    /// The cropped picture size in pixels.
    #[cfg(test)]
    pub fn picture_size(&self) -> (u32, u32) {
        (
            self.width - 2 * (self.crop[0] + self.crop[1]),
            self.height - 2 * (self.crop[2] + self.crop[3]),
        )
    }
}

/// `profile_tier_level(1, 0)`: Main profile, Main tier, progressive frames
/// only, no sub-layers. 96 bits.
fn put_profile_tier_level(w: &mut BitWriter, level_idc: u8) {
    w.put_bits(0, 2); // general_profile_space
    w.put_flag(false); // general_tier_flag
    w.put_bits(u32::from(PROFILE_MAIN), 5);
    // general_profile_compatibility_flag[j]: Main (1) and Main 10 (2) decode it.
    w.put_bits(0x6000_0000, 32);
    w.put_flag(true); // general_progressive_source_flag
    w.put_flag(false); // general_interlaced_source_flag
    w.put_flag(false); // general_non_packed_constraint_flag
    w.put_flag(true); // general_frame_only_constraint_flag
    w.put_bits(0, 32); // general_reserved_zero_43bits
    w.put_bits(0, 11);
    w.put_flag(false); // general_inbld_flag (reserved_zero_bit)
    w.put_bits(u32::from(level_idc), 8);
}

/// `video_parameter_set_rbsp()` bytes (no NAL header) for the stream of `sps`.
pub fn vps_rbsp(sps: &Sps) -> Vec<u8> {
    let mut w = BitWriter::new();
    w.put_bits(0, 4); // vps_video_parameter_set_id
    w.put_flag(true); // vps_base_layer_internal_flag
    w.put_flag(true); // vps_base_layer_available_flag
    w.put_bits(0, 6); // vps_max_layers_minus1
    w.put_bits(0, 3); // vps_max_sub_layers_minus1
    w.put_flag(true); // vps_temporal_id_nesting_flag
    w.put_bits(0xffff, 16); // vps_reserved_0xffff_16bits
    put_profile_tier_level(&mut w, sps.level_idc);
    w.put_flag(true); // vps_sub_layer_ordering_info_present_flag
    w.put_ue(sps.max_dec_pic_buffering - 1);
    w.put_ue(0); // vps_max_num_reorder_pics
    w.put_ue(0); // vps_max_latency_increase_plus1
    w.put_bits(0, 6); // vps_max_layer_id
    w.put_ue(0); // vps_num_layer_sets_minus1
    w.put_flag(false); // vps_timing_info_present_flag (the VUI has it)
    w.put_flag(false); // vps_extension_flag
    w.trailing_bits();
    w.into_bytes()
}

pub fn vps_nal(sps: &Sps) -> Vec<u8> {
    let mut out = Vec::new();
    write_nal(&mut out, NAL_VPS, &vps_rbsp(sps));
    out
}

/// `seq_parameter_set_rbsp()` bytes (no NAL header).
pub fn sps_rbsp(sps: &Sps) -> Vec<u8> {
    let mut w = BitWriter::new();
    w.put_bits(0, 4); // sps_video_parameter_set_id
    w.put_bits(0, 3); // sps_max_sub_layers_minus1
    w.put_flag(true); // sps_temporal_id_nesting_flag
    put_profile_tier_level(&mut w, sps.level_idc);
    w.put_ue(sps.sps_id);
    w.put_ue(1); // chroma_format_idc: 4:2:0
    w.put_ue(sps.width);
    w.put_ue(sps.height);
    let cropped = sps.crop != [0; 4];
    w.put_flag(cropped); // conformance_window_flag
    if cropped {
        for offset in sps.crop {
            w.put_ue(offset);
        }
    }
    w.put_ue(0); // bit_depth_luma_minus8
    w.put_ue(0); // bit_depth_chroma_minus8
    w.put_ue(sps.log2_max_poc_lsb - 4);
    w.put_flag(true); // sps_sub_layer_ordering_info_present_flag
    w.put_ue(sps.max_dec_pic_buffering - 1);
    w.put_ue(0); // sps_max_num_reorder_pics
    w.put_ue(0); // sps_max_latency_increase_plus1
    w.put_ue(sps.log2_min_cb - 3);
    w.put_ue(sps.log2_ctb - sps.log2_min_cb);
    w.put_ue(sps.log2_min_tb - 2);
    w.put_ue(sps.log2_max_tb - sps.log2_min_tb);
    w.put_ue(sps.max_transform_depth_inter);
    w.put_ue(sps.max_transform_depth_intra);
    w.put_flag(false); // scaling_list_enabled_flag
    w.put_flag(sps.amp);
    w.put_flag(sps.sao);
    w.put_flag(false); // pcm_enabled_flag
    w.put_ue(1); // num_short_term_ref_pic_sets
    // st_ref_pic_set(0): the picture before, used by the current picture.
    w.put_ue(1); // num_negative_pics
    w.put_ue(0); // num_positive_pics
    w.put_ue(0); // delta_poc_s0_minus1[0]
    w.put_flag(true); // used_by_curr_pic_s0_flag[0]
    w.put_flag(false); // long_term_ref_pics_present_flag
    w.put_flag(sps.temporal_mvp);
    w.put_flag(sps.strong_intra_smoothing);
    w.put_flag(sps.vui.is_some());
    if let Some(vui) = &sps.vui {
        write_vui(&mut w, vui);
    }
    w.put_flag(false); // sps_extension_present_flag
    w.trailing_bits();
    w.into_bytes()
}

fn write_vui(w: &mut BitWriter, vui: &Vui) {
    w.put_flag(false); // aspect_ratio_info_present_flag
    w.put_flag(false); // overscan_info_present_flag
    w.put_flag(true); // video_signal_type_present_flag
    w.put_bits(5, 3); // video_format: unspecified
    w.put_flag(vui.full_range);
    w.put_flag(true); // colour_description_present_flag
    w.put_bits(u32::from(vui.colour_primaries), 8);
    w.put_bits(u32::from(vui.transfer_characteristics), 8);
    w.put_bits(u32::from(vui.matrix_coefficients), 8);
    w.put_flag(false); // chroma_loc_info_present_flag
    w.put_flag(false); // neutral_chroma_indication_flag
    w.put_flag(false); // field_seq_flag
    w.put_flag(false); // frame_field_info_present_flag
    w.put_flag(false); // default_display_window_flag
    w.put_flag(vui.timing.is_some());
    if let Some((units, scale)) = vui.timing {
        w.put_bits(units, 32);
        w.put_bits(scale, 32);
        w.put_flag(false); // vui_poc_proportional_to_timing_flag
        w.put_flag(false); // vui_hrd_parameters_present_flag
    }
    w.put_flag(true); // bitstream_restriction_flag
    w.put_flag(false); // tiles_fixed_structure_flag
    w.put_flag(true); // motion_vectors_over_pic_boundaries_flag
    w.put_flag(true); // restricted_ref_pic_lists_flag
    w.put_ue(0); // min_spatial_segmentation_idc
    w.put_ue(0); // max_bytes_per_pic_denom
    w.put_ue(0); // max_bits_per_min_cu_denom
    w.put_ue(15); // log2_max_mv_length_horizontal
    w.put_ue(15); // log2_max_mv_length_vertical
}

pub fn sps_nal(sps: &Sps) -> Vec<u8> {
    let mut out = Vec::new();
    write_nal(&mut out, NAL_SPS, &sps_rbsp(sps));
    out
}

/// The cropped picture size an SPS says (the RBSP, emulation prevention
/// already removed). Reads only what ours has: no sub-layers, 4:2:0.
pub fn parse_sps_size(rbsp: &[u8]) -> Result<(u32, u32), &'static str> {
    let mut r = BitReader::new(rbsp);
    r.bits(4)?; // sps_video_parameter_set_id
    if r.bits(3)? != 0 {
        return Err("sub-layers");
    }
    r.bit()?; // sps_temporal_id_nesting_flag
    // profile_tier_level: 96 bits without sub-layers.
    for _ in 0..3 {
        r.bits(32)?;
    }
    r.ue()?; // sps_seq_parameter_set_id
    if r.ue()? != 1 {
        return Err("not 4:2:0");
    }
    let (width, height) = (r.ue()?, r.ue()?);
    let mut crop = [0; 4];
    if r.bit()? {
        for offset in &mut crop {
            *offset = r.ue()?;
        }
    }
    let cropped = |size: u32, a: u32, b: u32| size.checked_sub(2 * (a + b)).ok_or("bad crop");
    Ok((
        cropped(width, crop[0], crop[1])?,
        cropped(height, crop[2], crop[3])?,
    ))
}

/// A picture parameter set: one slice, no tiles, one reference in list 0.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pps {
    pub pps_id: u32,
    pub sps_id: u32,
    pub init_qp: i32,
    /// `pps_cb_qp_offset` and `pps_cr_qp_offset`.
    pub chroma_qp_offset: i32,
    /// `diff_cu_qp_delta_depth` if `cu_qp_delta_enabled_flag`.
    pub cu_qp_delta_depth: Option<u32>,
    pub sign_data_hiding: bool,
    pub transform_skip: bool,
}

pub fn pps_rbsp(pps: &Pps) -> Vec<u8> {
    let mut w = BitWriter::new();
    w.put_ue(pps.pps_id);
    w.put_ue(pps.sps_id);
    w.put_flag(false); // dependent_slice_segments_enabled_flag
    w.put_flag(false); // output_flag_present_flag
    w.put_bits(0, 3); // num_extra_slice_header_bits
    w.put_flag(pps.sign_data_hiding);
    w.put_flag(false); // cabac_init_present_flag
    w.put_ue(0); // num_ref_idx_l0_default_active_minus1
    w.put_ue(0); // num_ref_idx_l1_default_active_minus1
    w.put_se(pps.init_qp - 26);
    w.put_flag(false); // constrained_intra_pred_flag
    w.put_flag(pps.transform_skip);
    w.put_flag(pps.cu_qp_delta_depth.is_some());
    if let Some(depth) = pps.cu_qp_delta_depth {
        w.put_ue(depth);
    }
    w.put_se(pps.chroma_qp_offset); // pps_cb_qp_offset
    w.put_se(pps.chroma_qp_offset); // pps_cr_qp_offset
    w.put_flag(false); // pps_slice_chroma_qp_offsets_present_flag
    w.put_flag(false); // weighted_pred_flag
    w.put_flag(false); // weighted_bipred_flag
    w.put_flag(false); // transquant_bypass_enabled_flag
    w.put_flag(false); // tiles_enabled_flag
    w.put_flag(false); // entropy_coding_sync_enabled_flag
    w.put_flag(false); // pps_loop_filter_across_slices_enabled_flag
    w.put_flag(false); // deblocking_filter_control_present_flag
    w.put_flag(false); // pps_scaling_list_data_present_flag
    w.put_flag(false); // lists_modification_present_flag
    w.put_ue(0); // log2_parallel_merge_level_minus2
    w.put_flag(false); // slice_segment_header_extension_present_flag
    w.put_flag(false); // pps_extension_present_flag
    w.trailing_bits();
    w.into_bytes()
}

pub fn pps_nal(pps: &Pps) -> Vec<u8> {
    let mut out = Vec::new();
    write_nal(&mut out, NAL_PPS, &pps_rbsp(pps));
    out
}

/// `slice_type` values (Table 7-7; we make no B slices).
pub const SLICE_P: u8 = 1;
pub const SLICE_I: u8 = 2;

/// What differs from slice to slice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SliceHeader {
    pub idr: bool,
    pub poc_lsb: u32,
    pub qp_delta: i32,
    /// `MaxNumMergeCand`, 1 to 5.
    pub max_merge_candidates: u32,
}

/// A slice segment's header as packed header data: the NAL unit with its start
/// code, the header and its `byte_alignment()`, and no slice data. Returns the
/// bytes and their length in bits (what a VA packed header buffer states).
pub fn slice_header_nal(sps: &Sps, pps: &Pps, h: &SliceHeader) -> (Vec<u8>, u32) {
    let mut w = BitWriter::new();
    w.put_flag(true); // first_slice_segment_in_pic_flag
    if h.idr {
        w.put_flag(false); // no_output_of_prior_pics_flag
    }
    w.put_ue(pps.pps_id);
    w.put_ue(u32::from(if h.idr { SLICE_I } else { SLICE_P }));
    if !h.idr {
        w.put_bits(h.poc_lsb, sps.log2_max_poc_lsb);
        w.put_flag(true); // short_term_ref_pic_set_sps_flag (the only set: no index)
        if sps.temporal_mvp {
            w.put_flag(true); // slice_temporal_mvp_enabled_flag
        }
    }
    if sps.sao {
        w.put_flag(true); // slice_sao_luma_flag
        w.put_flag(true); // slice_sao_chroma_flag
    }
    if !h.idr {
        w.put_flag(false); // num_ref_idx_active_override_flag
        // (With one reference the collocated picture is index 0: not coded.)
        w.put_ue(5 - h.max_merge_candidates); // five_minus_max_num_merge_cand
    }
    w.put_se(h.qp_delta);
    // byte_alignment(): a one, then zeros.
    w.trailing_bits();
    let mut out = Vec::new();
    write_nal(
        &mut out,
        if h.idr { NAL_IDR_N_LP } else { NAL_TRAIL_R },
        w.bytes(),
    );
    let bits = out.len() * 8;
    (out, bits as u32)
}

/// A NAL unit made by [`write_nal`] without its emulation prevention bytes
/// (start code and header kept).
pub fn unescaped_nal(nal: &[u8]) -> Vec<u8> {
    let split = nal.len().min(4 + NAL_HEADER_LEN);
    let mut out = nal[..split].to_vec();
    out.extend_from_slice(&unescape(&nal[split..]));
    out
}

pub use super::h264::{NalRange, nal_units};

/// The `nal_unit_type` of `nal` in `stream`.
pub fn kind_of(stream: &[u8], nal: &NalRange) -> u8 {
    nal_kind(stream[nal.header])
}

/// Whether the stream has an IDR slice.
pub fn has_idr(stream: &[u8]) -> bool {
    nal_units(stream).iter().any(|n| is_idr(kind_of(stream, n)))
}

/// Makes an IDR access unit carry the VPS, SPS and PPS: the ones the encoder
/// returned are kept as they are, and any that are missing are added in front
/// of the first slice from `sps` and `pps`. Everything else is copied as it
/// is, and a stream without an IDR slice unchanged. Returns how many headers
/// were added.
pub fn fix_headers(stream: &[u8], sps: &Sps, pps: &Pps, out: &mut Vec<u8>) -> u32 {
    let units = nal_units(stream);
    out.clear();
    if !units.iter().any(|n| is_idr(kind_of(stream, n))) {
        out.extend_from_slice(stream);
        return 0;
    }
    let has = |kind| units.iter().any(|n| kind_of(stream, n) == kind);
    let (have_vps, have_sps, have_pps) = (has(NAL_VPS), has(NAL_SPS), has(NAL_PPS));
    let mut added = 0;
    if let Some(first) = units.first() {
        out.extend_from_slice(&stream[..first.start]);
    }
    let mut inserted = false;
    for nal in &units {
        let kind = kind_of(stream, nal);
        if !inserted && kind < NAL_VPS {
            inserted = true;
            for (have, bytes) in [
                (have_vps, vps_nal as fn(&Sps) -> Vec<u8>),
                (have_sps, sps_nal),
            ] {
                if !have {
                    out.extend_from_slice(&bytes(sps));
                    added += 1;
                }
            }
            if !have_pps {
                out.extend_from_slice(&pps_nal(pps));
                added += 1;
            }
        }
        out.extend_from_slice(&stream[nal.start..nal.end]);
    }
    added
}

/// What a checked access unit holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccessUnit {
    /// `nal_unit_type`s in order.
    pub nals: Vec<u8>,
    pub idr: bool,
    /// The SPS, if there is one, parsed (the size is the cropped one).
    pub size: Option<(u32, u32)>,
}

/// Checks that `stream` is Annex-B HEVC shaped as the players expect: NAL
/// headers with the forbidden bit clear and `nuh_temporal_id_plus1` 1, an IDR
/// access unit starting with VPS, SPS, PPS and then the IDR slice, and only
/// slices otherwise.
pub fn check_access_unit(stream: &[u8]) -> Result<AccessUnit, String> {
    let units = nal_units(stream);
    if units.is_empty() {
        return Err("no NAL units (no start code)".into());
    }
    if units[0].start != 0 {
        return Err(format!(
            "{} bytes before the first start code",
            units[0].start
        ));
    }
    let mut nals = Vec::new();
    for unit in &units {
        if unit.end < unit.header + NAL_HEADER_LEN + 1 {
            return Err("an empty NAL unit".into());
        }
        let (first, second) = (stream[unit.header], stream[unit.header + 1]);
        if first & 0x80 != 0 {
            return Err(format!("forbidden_zero_bit set at {}", unit.header));
        }
        if second & 7 != 1 {
            return Err(format!(
                "nuh_temporal_id_plus1 is {} at {}",
                second & 7,
                unit.header
            ));
        }
        nals.push(nal_kind(first));
    }
    let idr = nals.iter().any(|&n| is_idr(n));
    let mut size = None;
    if idr {
        let first_slice = nals.iter().position(|&n| is_idr(n)).expect("found above");
        let before = &nals[..first_slice];
        if before != [NAL_VPS, NAL_SPS, NAL_PPS] && before != [NAL_AUD, NAL_VPS, NAL_SPS, NAL_PPS] {
            return Err(format!(
                "an IDR picture should start with VPS, SPS, PPS, the IDR slice; the NAL types are {nals:?}"
            ));
        }
        let sps = units
            .iter()
            .find(|u| kind_of(stream, u) == NAL_SPS)
            .expect("checked above");
        let rbsp = unescape(&stream[sps.header + NAL_HEADER_LEN..sps.end]);
        size = Some(parse_sps_size(&rbsp).map_err(|e| format!("the SPS: {e}"))?);
    } else {
        if !nals
            .iter()
            .all(|&n| n <= NAL_TRAIL_R || matches!(n, NAL_SEI_PREFIX | NAL_SEI_SUFFIX | NAL_AUD))
        {
            return Err(format!(
                "a non-IDR picture should hold trailing slices only; the NAL types are {nals:?}"
            ));
        }
        if !nals.iter().any(|&n| n <= NAL_TRAIL_R) {
            return Err("no slice in the picture".into());
        }
    }
    Ok(AccessUnit { nals, idr, size })
}

/// The lowest `general_level_idc` that takes `width`×`height` at `fps` in
/// Main tier (Table A.8: MaxLumaPs and MaxLumaSr), or the highest one if none
/// does.
pub fn level_for(width: u32, height: u32, fps: u32) -> u8 {
    // (general_level_idc, MaxLumaSr, MaxLumaPs)
    const LEVELS: [(u8, u64, u64); 13] = [
        (30, 552_960, 36_864),
        (60, 3_686_400, 122_880),
        (63, 7_372_800, 245_760),
        (90, 16_588_800, 552_960),
        (93, 33_177_600, 983_040),
        (120, 66_846_720, 2_228_224),
        (123, 133_693_440, 2_228_224),
        (150, 267_386_880, 8_912_896),
        (153, 534_773_760, 8_912_896),
        (156, 1_069_547_520, 8_912_896),
        (180, 1_069_547_520, 35_651_584),
        (183, 2_139_095_040, 35_651_584),
        (186, 4_278_190_080, 35_651_584),
    ];
    let samples = u64::from(width) * u64::from(height);
    let rate = samples * u64::from(fps);
    LEVELS
        .iter()
        .find(|&&(_, max_rate, max_size)| max_rate >= rate && max_size >= samples)
        .map_or(186, |l| l.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// 1280×720 at 60: level 4, a 32×32 CTB over 8×8 coding blocks.
    fn sample_sps() -> Sps {
        Sps {
            level_idc: 120,
            sps_id: 0,
            width: 1280,
            height: 720,
            crop: [0; 4],
            log2_max_poc_lsb: 8,
            max_dec_pic_buffering: 2,
            log2_min_cb: 3,
            log2_ctb: 5,
            log2_min_tb: 2,
            log2_max_tb: 5,
            max_transform_depth_inter: 2,
            max_transform_depth_intra: 2,
            amp: false,
            sao: false,
            temporal_mvp: true,
            strong_intra_smoothing: true,
            vui: Some(Vui::bt709(60)),
        }
    }

    fn sample_pps() -> Pps {
        Pps {
            pps_id: 0,
            sps_id: 0,
            init_qp: 26,
            chroma_qp_offset: 0,
            cu_qp_delta_depth: None,
            sign_data_hiding: false,
            transform_skip: false,
        }
    }

    /// The profile_tier_level of Main, level 4 (120 = 0x78): the profile
    /// (1), the compatibility flags for Main and Main 10, progressive and
    /// frame-only, zeros, the level.
    const PTL: &str = "016000000090000000000078";

    /// A string of '0' and '1' as bytes, padded with zeros to a byte.
    fn from_bits(text: &str) -> Vec<u8> {
        let mut text = text.to_string();
        while !text.len().is_multiple_of(8) {
            text.push('0');
        }
        text.as_bytes()
            .chunks(8)
            .map(|b| u8::from_str_radix(std::str::from_utf8(b).unwrap(), 2).unwrap())
            .collect()
    }

    #[test]
    fn a_vps_for_720p_has_known_bytes() {
        // vps id 0, base layer internal + available, 0 layers, 0 sub-layers,
        // nesting: 0000 1100 0000 0001 = 0c 01; 0xffff; the PTL; ordering
        // info present, dpb 2 (ue 1 = 010), reorder 0 (1), latency 0 (1),
        // max layer id 0 (000000), layer sets 1 (ue 0 = 1), no timing, no
        // extension, trailing one: 1 010 1 1 000000 1 0 0 1 = ac 09.
        let rbsp = vps_rbsp(&sample_sps());
        assert_eq!(hex(&rbsp), format!("0c01ffff{PTL}ac09"));
        // On the wire: a start code, the NAL header (type 32: 40 01), and
        // the PTL's zero runs escaped: 00 00 00 → 00 00 03 00, and
        // 00 00 00 00 00 → 00 00 03 00 00 03 00.
        assert_eq!(
            hex(&vps_nal(&sample_sps())),
            "00000001400\
             10c01ffff0160000003009000000300000300 78ac09"
                .replace(' ', "")
        );
    }

    #[test]
    fn an_sps_for_720p_has_known_bytes() {
        // Derived field by field, the bits of each in order:
        let ptl: String = PTL
            .as_bytes()
            .chunks(2)
            .map(|b| {
                format!(
                    "{:08b}",
                    u8::from_str_radix(std::str::from_utf8(b).unwrap(), 16).unwrap()
                )
            })
            .collect();
        let fields = [
            "00000001", // vps id 0, no sub-layers, nesting
            &ptl,
            "1",           // sps id 0
            "010",         // chroma_format_idc 1
            "0000000000",  /* */
            "10100000001", // width 1280 (ue: 1281 in 11 bits)
            "000000000",
            "1011010001", // height 720 (ue: 721 in 10 bits)
            "0",          // no conformance window
            "11",         // bit depths 8 and 8
            "00101",      // log2_max_pic_order_cnt_lsb_minus4 = 4
            "1",
            "010",
            "1",
            "1", // ordering info: dpb 2, no reorder, no latency
            "1",
            "011",
            "1",
            "00100",
            "011",
            "011",  // CB 8, CTB 32, TB 4..32, depth 2 and 2
            "0000", // no scaling lists, AMP, SAO, PCM
            "010",  // one short-term reference picture set
            "010",
            "1",
            "1",
            "1", // one negative picture (-1), used
            "0", // no long-term pictures
            "1",
            "1",
            "1", // temporal MVP, strong smoothing, VUI follows
            // VUI:
            "0",
            "0", // no aspect ratio, no overscan
            "1",
            "101",
            "0",
            "1", // signal type: unspecified, limited range, colour description
            "00000001",
            "00000001",
            "00000001", // BT.709
            "0",
            "0",
            "0",
            "0",
            "0", // no chroma loc, neutral, field seq, field info, window
            "1", // timing
            "00000000000000000000000000000001", // one unit in a tick
            "00000000000000000000000000111100", // 60 ticks a second
            "0",
            "0", // not POC-proportional, no HRD
            "1",
            "0",
            "1",
            "1", // restriction: no tiles, MVs over boundaries, restricted lists
            "1",
            "1",
            "1", // no spatial segmentation, byte and bit limits
            "000010000",
            "000010000", // longest MV 2^15
            "0",         // no SPS extension
            "1",         // trailing bit; zeros to the byte
        ]
        .concat();
        assert_eq!(hex(&sps_rbsp(&sample_sps())), hex(&from_bits(&fields)));
    }

    #[test]
    fn a_pps_for_720p_has_known_bytes() {
        let fields = [
            "1", "1", // pps id 0, sps id 0
            "0", "0", "000", "0",
            "0", // no dependent slices, output flag, extra bits, sign hiding, cabac init
            "1", "1", // one default reference per list
            "1", // init_qp_minus26 = 0
            "0", "0", "0", // no constrained intra, transform skip, cu_qp_delta
            "1", "1", // cb and cr offsets 0
            "0", "0", "0", "0", "0",
            "0", // no slice chroma offsets, weighting x2, bypass, tiles, WPP
            "0", "0", "0",
            "0", // no loop filter across slices, deblocking control, scaling list, list mods
            "1", // log2_parallel_merge_level_minus2 = 0
            "0", "0", // no slice header extension, no PPS extension
            "1", // trailing bit
        ]
        .concat();
        assert_eq!(hex(&pps_rbsp(&sample_pps())), hex(&from_bits(&fields)));
        assert_eq!(hex(&pps_nal(&sample_pps())), "000000014401c0718012");
        // With QP deltas in coding units, cu_qp_delta_enabled_flag is 1 and
        // diff_cu_qp_delta_depth (0 = ue 1) follows it.
        let with_delta = Pps {
            cu_qp_delta_depth: Some(0),
            ..sample_pps()
        };
        let fields = [
            "1", "1", "0", "0", "000", "0", "0", "1", "1", "1", "0", "0", "1", "1", "1", "1", "0",
            "0", "0", "0", "0", "0", "0", "0", "0", "0", "1", "0", "0", "1",
        ]
        .concat();
        assert_eq!(hex(&pps_rbsp(&with_delta)), hex(&from_bits(&fields)));
    }

    #[test]
    fn slice_headers_have_known_bytes() {
        let (sps, pps) = (sample_sps(), sample_pps());
        // A P picture, POC 1: first slice in picture (1), PPS 0 (1), slice
        // type 1 (ue: 010), POC lsb 1 (00000001), RPS from the SPS (1),
        // temporal MVP (1), no reference override (0), five minus five
        // merge candidates (ue 0: 1), slice_qp_delta 0 (1), byte alignment
        // (a one, then zeros).
        let header = SliceHeader {
            idr: false,
            poc_lsb: 1,
            qp_delta: 0,
            max_merge_candidates: 5,
        };
        let (nal, bits) = slice_header_nal(&sps, &pps, &header);
        let fields = ["1", "1", "010", "00000001", "1", "1", "0", "1", "1", "1"].concat();
        // NAL type 1 (TRAIL_R): 02 01.
        assert_eq!(
            hex(&nal),
            format!("000000010201{}", hex(&from_bits(&fields)))
        );
        assert_eq!(bits, nal.len() as u32 * 8);
        // An IDR: first slice (1), no_output_of_prior_pics (0), PPS 0 (1),
        // slice type 2 (ue: 011), slice_qp_delta 0 (1), alignment (1):
        // 1010 1111. NAL type 20 (IDR_N_LP): 28 01.
        let header = SliceHeader {
            idr: true,
            poc_lsb: 0,
            ..header
        };
        let (nal, bits) = slice_header_nal(&sps, &pps, &header);
        assert_eq!(hex(&nal), "000000012801af");
        assert_eq!(bits, 56);
        // A negative delta is se(v): -3 is ue 6 (00111).
        let (nal, _) = slice_header_nal(
            &sps,
            &pps,
            &SliceHeader {
                qp_delta: -3,
                ..header
            },
        );
        let fields = ["1", "0", "1", "011", "00111", "1"].concat();
        assert_eq!(
            hex(&nal),
            format!("000000012801{}", hex(&from_bits(&fields)))
        );
        // With SAO on, the two slice flags follow.
        let sao = Sps { sao: true, ..sps };
        let (nal, _) = slice_header_nal(&sao, &pps, &header);
        let fields = ["1", "0", "1", "011", "11", "1", "1"].concat();
        assert_eq!(
            hex(&nal),
            format!("000000012801{}", hex(&from_bits(&fields)))
        );
    }

    #[test]
    fn nal_headers_and_the_unescaped_form() {
        let mut nal = Vec::new();
        write_nal(&mut nal, NAL_SPS, &[0, 0, 1, 0, 0]);
        // Header 42 01; 00 00 01 → 00 00 03 01; a trailing 00 00 gets a 03.
        assert_eq!(hex(&nal), "00000001420100000301000003");
        assert_eq!(unescaped_nal(&nal), [0, 0, 0, 1, 0x42, 1, 0, 0, 1, 0, 0]);
        assert_eq!(nal_kind(0x42), NAL_SPS);
        assert_eq!(nal_kind(0x26), NAL_IDR_W_RADL);
        assert!(is_idr(NAL_IDR_N_LP) && !is_idr(NAL_TRAIL_R) && !is_idr(NAL_PPS));
    }

    fn idr_picture(sps: &Sps, pps: &Pps) -> Vec<u8> {
        let mut stream = vps_nal(sps);
        stream.extend(sps_nal(sps));
        stream.extend(pps_nal(pps));
        let header = SliceHeader {
            idr: true,
            poc_lsb: 0,
            qp_delta: 0,
            max_merge_candidates: 5,
        };
        stream.extend(slice_header_nal(sps, pps, &header).0);
        stream
    }

    #[test]
    fn an_access_unit_is_checked_for_its_shape() {
        let (sps, pps) = (sample_sps(), sample_pps());
        let idr = idr_picture(&sps, &pps);
        let unit = check_access_unit(&idr).unwrap();
        assert!(unit.idr);
        assert_eq!(unit.nals, [NAL_VPS, NAL_SPS, NAL_PPS, NAL_IDR_N_LP]);
        assert_eq!(unit.size, Some((1280, 720)));
        assert!(has_idr(&idr));
        // The size is the cropped one.
        let cropped = Sps {
            width: 1920,
            height: 1088,
            crop: [0, 0, 0, 4],
            ..sps.clone()
        };
        let unit = check_access_unit(&idr_picture(&cropped, &pps)).unwrap();
        assert_eq!(unit.size, Some((1920, 1080)));
        // A P picture is a trailing slice only.
        let p = slice_header_nal(
            &sps,
            &pps,
            &SliceHeader {
                idr: false,
                poc_lsb: 1,
                qp_delta: 0,
                max_merge_candidates: 5,
            },
        )
        .0;
        let unit = check_access_unit(&p).unwrap();
        assert_eq!(
            (unit.idr, unit.nals.as_slice(), unit.size),
            (false, &[NAL_TRAIL_R][..], None)
        );
        assert!(!has_idr(&p));
        // What isn't: no start code, a missing PPS, an IDR without headers, a
        // P picture with parameter sets, a wrong temporal id.
        assert!(check_access_unit(&[1, 2, 3]).is_err());
        let mut no_pps = vps_nal(&sps);
        no_pps.extend(sps_nal(&sps));
        no_pps.extend(&idr[idr.len() - 7..]);
        assert!(
            check_access_unit(&no_pps)
                .unwrap_err()
                .contains("VPS, SPS, PPS")
        );
        assert!(check_access_unit(&idr[idr.len() - 7..]).is_err());
        let mut with_sps = sps_nal(&sps);
        with_sps.extend(&p);
        assert!(check_access_unit(&with_sps).is_err());
        let mut bad_layer = p.clone();
        bad_layer[5] = 0;
        assert!(
            check_access_unit(&bad_layer)
                .unwrap_err()
                .contains("temporal")
        );
    }

    #[test]
    fn the_sps_size_reads_back_as_written() {
        let mut sps = sample_sps();
        assert_eq!(parse_sps_size(&sps_rbsp(&sps)), Ok((1280, 720)));
        sps.width = 1376;
        sps.crop = [0, 5, 0, 0];
        assert_eq!(parse_sps_size(&sps_rbsp(&sps)), Ok((1366, 720)));
        assert_eq!(sps.picture_size(), (1366, 720));
        assert!(parse_sps_size(&sps_rbsp(&sps)[..5]).is_err());
    }

    #[test]
    fn missing_headers_are_added_before_the_idr_slice() {
        let (sps, pps) = (sample_sps(), sample_pps());
        let full = idr_picture(&sps, &pps);
        let slice = full[full.len() - 7..].to_vec();
        let mut out = Vec::new();
        // A bare slice gets all three.
        assert_eq!(fix_headers(&slice, &sps, &pps, &mut out), 3);
        assert_eq!(out, full);
        // A driver's own VPS and SPS are kept; the PPS is added.
        let mut partial = vps_nal(&sps);
        partial.extend(sps_nal(&sps));
        partial.extend(&slice);
        assert_eq!(fix_headers(&partial, &sps, &pps, &mut out), 1);
        assert_eq!(out, full);
        // A complete access unit, and a picture that isn't an IDR, are copied.
        assert_eq!(fix_headers(&full, &sps, &pps, &mut out), 0);
        assert_eq!(out, full);
        let p = [0, 0, 0, 1, 0x02, 0x01, 0xaa, 0xbb];
        assert_eq!(fix_headers(&p, &sps, &pps, &mut out), 0);
        assert_eq!(out, p);
    }

    #[test]
    fn levels_follow_the_picture_size_and_rate() {
        // (width, height, fps, general_level_idc = 30 × level)
        for (width, height, fps, level) in [
            (640, 360, 30, 63),
            (1280, 720, 30, 93),
            (1280, 720, 60, 120),
            (1920, 1080, 30, 120),
            (1920, 1080, 60, 123),
            (2560, 1440, 60, 150),
            (3840, 2160, 60, 153),
            (3840, 2160, 120, 156),
            (8192, 4320, 60, 183),
        ] {
            assert_eq!(
                level_for(width, height, fps),
                level,
                "{width}×{height}@{fps}"
            );
        }
        assert_eq!(level_for(16384, 8704, 240), 186);
    }
}

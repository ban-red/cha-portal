//! AV1 (Profile 0, 8-bit 4:2:0) bitstream syntax, written by us: OBU framing,
//! the sequence header and the frame header the VA-API encoder packs itself,
//! and a parser to check (and, if a driver leaves them out, repair) what it
//! returns. The bit writer is [`super`]'s, shared with H.264 and HEVC.
//!
//! The stream is built for the lowest latency a decoder can show, like the
//! other codecs' ones: one tile, every frame a reference, no B-frames, one
//! reference (the frame before, always slot 0), a key frame when asked, and
//! the output shaped like NVENC's and SVT-AV1's: each packet is a temporal
//! unit in the low-overhead format (temporal delimiter OBU, the sequence
//! header on a key frame, then one frame OBU), which is what WebCodecs'
//! AV1 decoder takes.
//!
//! Written from the AOM AV1 Bitstream & Decoding Process Specification
//! (5.3 OBU syntax, 5.5 sequence header, 5.9 frame header, 4.10.5 leb128,
//! 7.5 temporal units) and libva's `va_enc_av1.h` for what the packed
//! headers and their bit offsets mean. Nothing in it comes from another
//! project's code.

use super::BitWriter;

pub const OBU_SEQUENCE_HEADER: u8 = 1;
pub const OBU_TEMPORAL_DELIMITER: u8 = 2;
pub const OBU_FRAME_HEADER: u8 = 3;
pub const OBU_TILE_GROUP: u8 = 4;
pub const OBU_FRAME: u8 = 6;

pub const KEY_FRAME: u8 = 0;
pub const INTER_FRAME: u8 = 1;
/// `primary_ref_frame` that loads nothing.
pub const PRIMARY_REF_NONE: u8 = 7;
/// `seq_level_idx` 13: level 5.1, what `/streams` announces (`av01.0.13M.08`).
pub const LEVEL_5_1: u8 = 13;

/// `interpolation_filter` values (EIGHTTAP, EIGHTTAP_SMOOTH, EIGHTTAP_SHARP,
/// BILINEAR, SWITCHABLE).
pub const FILTER_SWITCHABLE: u8 = 4;

/// An unsigned LEB128 number (4.10.5), at least `min_len` bytes long: a longer
/// code than needed pads with continuation bytes, which every decoder reads
/// (the size field may be up to eight bytes) and which a driver patching a
/// size in later needs.
pub fn leb128(mut value: u32, min_len: usize) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 && out.len() + 1 >= min_len {
            out.push(byte);
            return out;
        }
        out.push(byte | 0x80);
    }
}

/// An OBU with a size field, no extension: header byte, `obu_size` (at least
/// `size_bytes` long), payload (5.3.1).
pub fn obu(kind: u8, payload: &[u8], size_bytes: usize) -> Vec<u8> {
    let mut out = vec![(kind & 15) << 3 | 2];
    out.extend_from_slice(&leb128(payload.len() as u32, size_bytes));
    out.extend_from_slice(payload);
    out
}

/// The temporal delimiter OBU: two bytes, `12 00`.
pub fn temporal_delimiter() -> Vec<u8> {
    obu(OBU_TEMPORAL_DELIMITER, &[], 1)
}

/// What the sequence header says. Everything not here is fixed: Profile 0,
/// 8-bit, 4:2:0, BT.709 limited range signalled, one operating point, no
/// timing info, no frame ids, 64×64 superblocks unless `use_128x128`, order
/// hints on, no film grain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Seq {
    pub level_idx: u8,
    pub tier: u8,
    /// `max_frame_width` and `max_frame_height`.
    pub width: u32,
    pub height: u32,
    pub use_128x128: bool,
    pub filter_intra: bool,
    pub intra_edge_filter: bool,
    pub interintra_compound: bool,
    pub masked_compound: bool,
    pub warped_motion: bool,
    pub dual_filter: bool,
    pub jnt_comp: bool,
    pub ref_frame_mvs: bool,
    pub superres: bool,
    pub cdef: bool,
    pub restoration: bool,
    pub order_hint_bits: u32,
}

impl Seq {
    /// All the optional coding tools off, CDEF on.
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            level_idx: LEVEL_5_1,
            tier: 0,
            width,
            height,
            use_128x128: false,
            filter_intra: false,
            intra_edge_filter: false,
            interintra_compound: false,
            masked_compound: false,
            warped_motion: false,
            dual_filter: false,
            jnt_comp: false,
            ref_frame_mvs: false,
            superres: false,
            cdef: true,
            restoration: false,
            order_hint_bits: 7,
        }
    }
}

/// The bits `frame_width_bits_minus_1` + 1 and the like are coded in: enough
/// for the value (`floor(log2 v) + 1`).
fn size_bits(value: u32) -> u32 {
    32 - value.max(1).leading_zeros()
}

/// `sequence_header_obu()`'s payload (5.5.1), with its trailing bits.
pub fn sequence_header_payload(seq: &Seq) -> Vec<u8> {
    let mut w = BitWriter::new();
    w.put_bits(0, 3); // seq_profile
    w.put_bit(false); // still_picture
    w.put_bit(false); // reduced_still_picture_header
    w.put_bit(false); // timing_info_present_flag
    w.put_bit(false); // initial_display_delay_present_flag
    w.put_bits(0, 5); // operating_points_cnt_minus_1
    w.put_bits(0, 12); // operating_point_idc[0]
    w.put_bits(u32::from(seq.level_idx), 5);
    if seq.level_idx > 7 {
        w.put_bits(u32::from(seq.tier), 1);
    }
    let (width_bits, height_bits) = (size_bits(seq.width), size_bits(seq.height));
    w.put_bits(width_bits - 1, 4);
    w.put_bits(height_bits - 1, 4);
    w.put_bits(seq.width - 1, width_bits);
    w.put_bits(seq.height - 1, height_bits);
    w.put_bit(false); // frame_id_numbers_present_flag
    w.put_bit(seq.use_128x128);
    w.put_bit(seq.filter_intra);
    w.put_bit(seq.intra_edge_filter);
    w.put_bit(seq.interintra_compound);
    w.put_bit(seq.masked_compound);
    w.put_bit(seq.warped_motion);
    w.put_bit(seq.dual_filter);
    w.put_bit(true); // enable_order_hint
    w.put_bit(seq.jnt_comp);
    w.put_bit(seq.ref_frame_mvs);
    // The frame header says whether it uses screen content tools and integer
    // motion vectors (seq_choose_screen_content_tools, seq_choose_integer_mv).
    w.put_bit(true);
    w.put_bit(true);
    w.put_bits(seq.order_hint_bits - 1, 3);
    w.put_bit(seq.superres);
    w.put_bit(seq.cdef);
    w.put_bit(seq.restoration);
    // color_config()
    w.put_bit(false); // high_bitdepth
    w.put_bit(false); // mono_chrome
    w.put_bit(true); // color_description_present_flag
    w.put_bits(1, 8); // color_primaries: BT.709
    w.put_bits(1, 8); // transfer_characteristics: BT.709
    w.put_bits(1, 8); // matrix_coefficients: BT.709
    w.put_bit(false); // color_range: limited
    w.put_bits(0, 2); // chroma_sample_position: unknown
    w.put_bit(false); // separate_uv_delta_q
    w.put_bit(false); // film_grain_params_present
    w.trailing_bits();
    w.into_bytes()
}

/// The sequence header OBU, complete.
pub fn sequence_header_obu(seq: &Seq) -> Vec<u8> {
    obu(OBU_SEQUENCE_HEADER, &sequence_header_payload(seq), 1)
}

/// CDEF strengths of the frame header (5.9.19): the damping, the number of
/// strength pairs as a power of two, and each pair's luma and chroma
/// strengths as `primary << 2 | secondary` (a primary strength is 0 to 15,
/// a secondary one 0 to 3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cdef {
    pub damping_minus_3: u8,
    pub bits: u8,
    pub y: [u8; 8],
    pub uv: [u8; 8],
}

impl Cdef {
    /// One pair of strengths: `y` and `uv`.
    pub fn single(damping_minus_3: u8, y: u8, uv: u8) -> Self {
        let mut c = Self {
            damping_minus_3,
            bits: 0,
            y: [0; 8],
            uv: [0; 8],
        };
        c.y[0] = y;
        c.uv[0] = uv;
        c
    }
}

/// What the frame header of one frame says (the stream shows every frame, uses
/// one tile and no segmentation, deltas, loop filter deltas, quantiser
/// matrices, loop restoration, skip mode, compound references, global motion
/// or film grain).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub key: bool,
    pub order_hint: u32,
    pub primary_ref_frame: u8,
    pub refresh_frame_flags: u8,
    pub ref_frame_idx: [u8; 7],
    pub allow_screen_content_tools: bool,
    /// Only written when screen content tools are on.
    pub force_integer_mv: bool,
    pub disable_cdf_update: bool,
    pub disable_frame_end_update_cdf: bool,
    pub base_qindex: u8,
    pub filter_level: [u8; 2],
    pub filter_level_uv: [u8; 2],
    pub sharpness: u8,
    pub cdef: Cdef,
    pub tx_mode_select: bool,
    pub reduced_tx_set: bool,
    pub allow_high_precision_mv: bool,
    /// `interpolation_filter`, [`FILTER_SWITCHABLE`] for a per-block choice.
    pub interpolation_filter: u8,
    /// Where the frame size and the display size differ: `render_width`,
    /// `render_height`.
    pub render: Option<(u32, u32)>,
    /// The header goes in an `OBU_FRAME` (with the tile group after it)
    /// rather than an `OBU_FRAME_HEADER`.
    pub frame_obu: bool,
    /// The least length of `obu_size`, so that a driver can patch the
    /// real size in.
    pub size_bytes: usize,
}

/// Where fields are in a packed frame header, which a driver that rewrites them
/// (`VAEncPictureParameterBufferAV1`'s `bit_offset_*`) is told. All from the
/// start of the OBU's bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Offsets {
    pub qindex_bit: u32,
    pub segmentation_bit: u32,
    pub loop_filter_bit: u32,
    pub cdef_bit: u32,
    /// What `cdef_params()` takes in bits.
    pub cdef_bits: u32,
    /// Byte of `obu_size`.
    pub size_byte: u32,
    /// Bits to the end of `frame_header_obu()`: with the trailing bit if it
    /// is an `OBU_FRAME_HEADER`, without the byte alignment of an `OBU_FRAME`.
    pub total_bits: u32,
}

/// A packed frame header: the whole OBU, and where things are in it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Header {
    pub bytes: Vec<u8>,
    pub offsets: Offsets,
}

/// `tile_log2(blkSize, target)` (5.9.15): the least `k` with `blkSize << k >=
/// target`.
fn tile_log2(block: u32, target: u32) -> u32 {
    let mut k = 0;
    while (u64::from(block) << k) < u64::from(target) {
        k += 1;
    }
    k
}

/// The tile columns and rows (as log2) a frame of this size needs at least:
/// zero for both up to 4096 wide and 4096×2304 (5.9.15, with 64×64
/// superblocks).
pub fn min_tiles_log2(width: u32, height: u32, use_128x128: bool) -> (u32, u32) {
    let mi_cols = 2 * ((width - 1 + 8) >> 3);
    let mi_rows = 2 * ((height - 1 + 8) >> 3);
    let (sb_cols, sb_rows, sb_shift) = if use_128x128 {
        ((mi_cols + 31) >> 5, (mi_rows + 31) >> 5, 7)
    } else {
        ((mi_cols + 15) >> 4, (mi_rows + 15) >> 4, 6)
    };
    let max_tile_width_sb = 4096 >> sb_shift;
    let max_tile_area_sb = (4096 * 2304) >> (2 * sb_shift);
    let min_cols = tile_log2(max_tile_width_sb, sb_cols);
    let min_tiles = min_cols.max(tile_log2(max_tile_area_sb, sb_rows * sb_cols));
    (min_cols, min_tiles - min_cols)
}

/// `uncompressed_header()` (5.9.2) of a shown frame of one tile, as a packed
/// header OBU (`frame_header_obu()` with its OBU header and size).
pub fn frame_header_obu(seq: &Seq, f: &Frame) -> Header {
    let intra = f.key;
    let mut w = BitWriter::new();
    let mut at = Offsets::default();
    w.put_bit(false); // show_existing_frame
    w.put_bits(u32::from(if f.key { KEY_FRAME } else { INTER_FRAME }), 2);
    w.put_bit(true); // show_frame
    if !f.key {
        w.put_bit(false); // error_resilient_mode (a shown key frame's is 1)
    }
    w.put_bit(f.disable_cdf_update);
    w.put_bit(f.allow_screen_content_tools); // seq_force_screen_content_tools is SELECT
    if f.allow_screen_content_tools {
        w.put_bit(f.force_integer_mv); // seq_force_integer_mv is SELECT
    }
    w.put_bit(false); // frame_size_override_flag
    w.put_bits(f.order_hint, seq.order_hint_bits);
    if !intra {
        w.put_bits(u32::from(f.primary_ref_frame), 3);
        w.put_bits(u32::from(f.refresh_frame_flags), 8);
        if seq.order_hint_bits > 0 {
            w.put_bit(false); // frame_refs_short_signaling
        }
        for idx in f.ref_frame_idx {
            w.put_bits(u32::from(idx), 3);
        }
    }
    // (A shown key frame refreshes every slot without saying so.)
    // frame_size(): no override, no superres; render_size():
    put_render_size(&mut w, f.render);
    if intra {
        if f.allow_screen_content_tools {
            w.put_bit(false); // allow_intrabc
        }
    } else {
        let integer_mv = f.allow_screen_content_tools && f.force_integer_mv;
        if !integer_mv {
            w.put_bit(f.allow_high_precision_mv);
        }
        // read_interpolation_filter()
        let switchable = f.interpolation_filter == FILTER_SWITCHABLE;
        w.put_bit(switchable);
        if !switchable {
            w.put_bits(u32::from(f.interpolation_filter), 2);
        }
        w.put_bit(false); // is_motion_mode_switchable
        if seq.ref_frame_mvs {
            w.put_bit(false); // use_ref_frame_mvs
        }
    }
    if !f.disable_cdf_update {
        w.put_bit(f.disable_frame_end_update_cdf);
    }
    // tile_info(): uniform spacing, as few tiles as the size allows.
    w.put_bit(true); // uniform_tile_spacing_flag
    let (min_cols, min_rows) = min_tiles_log2(seq.width, seq.height, seq.use_128x128);
    debug_assert_eq!((min_cols, min_rows), (0, 0), "one tile only");
    let sb_size = if seq.use_128x128 { 5 } else { 4 };
    let mi_cols = 2 * ((seq.width - 1 + 8) >> 3);
    let mi_rows = 2 * ((seq.height - 1 + 8) >> 3);
    let sb_cols = (mi_cols + (1 << sb_size) - 1) >> sb_size;
    let sb_rows = (mi_rows + (1 << sb_size) - 1) >> sb_size;
    let max_log2_cols = tile_log2(1, sb_cols.min(64));
    let max_log2_rows = tile_log2(1, sb_rows.min(64));
    if min_cols < max_log2_cols {
        w.put_bit(false); // increment_tile_cols_log2: stop
    }
    if min_rows < max_log2_rows {
        w.put_bit(false); // increment_tile_rows_log2: stop
    }
    // quantization_params()
    at.qindex_bit = w.bit_len() as u32;
    w.put_bits(u32::from(f.base_qindex), 8);
    w.put_bit(false); // DeltaQYDc: delta_coded
    w.put_bit(false); // DeltaQUDc
    w.put_bit(false); // DeltaQUAc (V's are U's: separate_uv_delta_q is 0)
    w.put_bit(false); // using_qmatrix
    // segmentation_params()
    at.segmentation_bit = w.bit_len() as u32;
    w.put_bit(false); // segmentation_enabled
    // delta_q_params(), delta_lf_params()
    if f.base_qindex > 0 {
        w.put_bit(false); // delta_q_present
    }
    // loop_filter_params() (this stream is never lossless)
    at.loop_filter_bit = w.bit_len() as u32;
    w.put_bits(u32::from(f.filter_level[0]), 6);
    w.put_bits(u32::from(f.filter_level[1]), 6);
    if f.filter_level[0] != 0 || f.filter_level[1] != 0 {
        w.put_bits(u32::from(f.filter_level_uv[0]), 6);
        w.put_bits(u32::from(f.filter_level_uv[1]), 6);
    }
    w.put_bits(u32::from(f.sharpness), 3);
    w.put_bit(false); // loop_filter_delta_enabled
    // cdef_params()
    at.cdef_bit = w.bit_len() as u32;
    if seq.cdef {
        w.put_bits(u32::from(f.cdef.damping_minus_3), 2);
        w.put_bits(u32::from(f.cdef.bits), 2);
        for i in 0..1usize << f.cdef.bits {
            w.put_bits(u32::from(f.cdef.y[i] >> 2), 4);
            w.put_bits(u32::from(f.cdef.y[i] & 3), 2);
            w.put_bits(u32::from(f.cdef.uv[i] >> 2), 4);
            w.put_bits(u32::from(f.cdef.uv[i] & 3), 2);
        }
    }
    at.cdef_bits = w.bit_len() as u32 - at.cdef_bit;
    // lr_params(): enable_restoration is off (or the stream isn't ours to write).
    debug_assert!(!seq.restoration);
    // read_tx_mode()
    w.put_bit(f.tx_mode_select);
    // frame_reference_mode(): reference_select
    if !intra {
        w.put_bit(false);
    }
    // skip_mode_params(): no compound references, so nothing.
    if !intra && seq.warped_motion {
        w.put_bit(false); // allow_warped_motion
    }
    w.put_bit(f.reduced_tx_set);
    if !intra {
        for _ in 0..7 {
            w.put_bit(false); // is_global
        }
    }
    // film_grain_params(): film_grain_params_present is 0.
    at.total_bits = w.bit_len() as u32;

    let kind = if f.frame_obu {
        w.align_with(false); // byte_alignment()
        OBU_FRAME
    } else {
        w.trailing_bits();
        at.total_bits = w.bit_len() as u32; // up to and with the trailing bit
        OBU_FRAME_HEADER
    };
    let payload = w.into_bytes();
    let bytes = obu(kind, &payload, f.size_bytes.max(1));
    let prefix = (bytes.len() - payload.len()) as u32;
    at.size_byte = 1;
    for bit in [
        &mut at.qindex_bit,
        &mut at.segmentation_bit,
        &mut at.loop_filter_bit,
        &mut at.cdef_bit,
        &mut at.total_bits,
    ] {
        *bit += prefix * 8;
    }
    Header { bytes, offsets: at }
}

fn put_render_size(w: &mut BitWriter, render: Option<(u32, u32)>) {
    w.put_bit(render.is_some()); // render_and_frame_size_different
    if let Some((width, height)) = render {
        w.put_bits(width - 1, 16);
        w.put_bits(height - 1, 16);
    }
}

// ---- reading what a driver returns ----

/// One OBU of a stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Obu<'a> {
    pub kind: u8,
    /// `obu_extension_flag`'s byte, if there is one.
    pub extension: Option<u8>,
    pub payload: &'a [u8],
}

impl Obu<'_> {
    /// The OBU again, with a size field of the least length (an OBU without
    /// one, which may only end a stream, gains it).
    pub fn serialize(&self, out: &mut Vec<u8>) {
        out.push((self.kind & 15) << 3 | u8::from(self.extension.is_some()) << 2 | 2);
        out.extend(self.extension);
        out.extend_from_slice(&leb128(self.payload.len() as u32, 1));
        out.extend_from_slice(self.payload);
    }
}

/// The OBUs of `data` in order (5.3.1). The last may lack a size field, and
/// then runs to the end.
pub fn parse(data: &[u8]) -> Result<Vec<Obu<'_>>, String> {
    let mut at = 0;
    let mut obus = Vec::new();
    while at < data.len() {
        let head = data[at];
        if head & 0x80 != 0 {
            return Err(format!("obu_forbidden_bit set at byte {at}"));
        }
        let (kind, has_extension, has_size) = ((head >> 3) & 15, head & 4 != 0, head & 2 != 0);
        at += 1;
        let extension = if has_extension {
            let byte = *data.get(at).ok_or("an OBU header cut short")?;
            at += 1;
            Some(byte)
        } else {
            None
        };
        let size = if has_size {
            let (mut size, mut shift) = (0u64, 0);
            loop {
                let byte = *data.get(at).ok_or("an obu_size cut short")?;
                at += 1;
                size |= u64::from(byte & 0x7f) << shift;
                shift += 7;
                if byte & 0x80 == 0 {
                    break;
                }
                if shift >= 56 {
                    return Err("an obu_size longer than 8 bytes".into());
                }
            }
            size as usize
        } else {
            data.len() - at
        };
        let end = at
            .checked_add(size)
            .filter(|&end| end <= data.len())
            .ok_or_else(|| format!("an OBU of {size} bytes at {at} runs past the end"))?;
        obus.push(Obu {
            kind,
            extension,
            payload: &data[at..end],
        });
        at = end;
    }
    Ok(obus)
}

/// What a sequence header says that we check.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SeqInfo {
    pub profile: u32,
    pub level_idx: u32,
    pub tier: u32,
    pub width: u32,
    pub height: u32,
}

/// Reads the front of a sequence header (5.5.1) as far as the frame size.
pub fn parse_sequence(payload: &[u8]) -> Result<SeqInfo, String> {
    let mut r = super::BitReader::new(payload);
    let profile = r.bits(3)?;
    let _still = r.bit()?;
    if r.bit()? {
        return Err("a reduced still picture header".into());
    }
    if r.bit()? {
        return Err("timing info (not expected from this encoder)".into());
    }
    let initial_display_delay_present = r.bit()?;
    let operating_points = r.bits(5)? + 1;
    let (mut level_idx, mut tier) = (0, 0);
    for i in 0..operating_points {
        let _idc = r.bits(12)?;
        let level = r.bits(5)?;
        let t = if level > 7 { r.bits(1)? } else { 0 };
        if initial_display_delay_present && r.bit()? {
            r.bits(4)?;
        }
        if i == 0 {
            (level_idx, tier) = (level, t);
        }
    }
    let width_bits = r.bits(4)? + 1;
    let height_bits = r.bits(4)? + 1;
    let width = r.bits(width_bits)? + 1;
    let height = r.bits(height_bits)? + 1;
    Ok(SeqInfo {
        profile,
        level_idx,
        tier,
        width,
        height,
    })
}

/// A frame header's first fields (5.9.2): `(frame_type, show_frame,
/// show_existing_frame)`; the frame type is `u32::MAX` for a repeated frame.
pub fn frame_type(payload: &[u8]) -> Result<(u32, bool, bool), String> {
    let mut r = super::BitReader::new(payload);
    if r.bit()? {
        return Ok((u32::MAX, true, true));
    }
    let frame_type = r.bits(2)?;
    Ok((frame_type, r.bit()?, false))
}

/// What a checked temporal unit holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TemporalUnit {
    /// OBU types in order.
    pub obus: Vec<u8>,
    pub key: bool,
    /// The sequence header's frame size, if there is one.
    pub size: Option<(u32, u32)>,
    pub sequence: Option<SeqInfo>,
}

/// Checks that `data` is a temporal unit shaped as the players expect: a
/// temporal delimiter first, the sequence header on (and only on) a key
/// frame, and a single shown frame (an `OBU_FRAME`, or a frame header and
/// its tile group).
pub fn check_temporal_unit(data: &[u8]) -> Result<TemporalUnit, String> {
    let obus = parse(data)?;
    let kinds: Vec<u8> = obus.iter().map(|o| o.kind).collect();
    if kinds.first() != Some(&OBU_TEMPORAL_DELIMITER) {
        return Err(format!(
            "no temporal delimiter first; the OBU types are {kinds:?}"
        ));
    }
    if !obus[0].payload.is_empty() {
        return Err("a temporal delimiter with a payload".into());
    }
    let count = |kind| kinds.iter().filter(|&&k| k == kind).count();
    if count(OBU_TEMPORAL_DELIMITER) != 1 {
        return Err(format!(
            "{} temporal delimiters",
            count(OBU_TEMPORAL_DELIMITER)
        ));
    }
    let frame = obus
        .iter()
        .find(|o| matches!(o.kind, OBU_FRAME | OBU_FRAME_HEADER))
        .ok_or_else(|| format!("no frame OBU; the OBU types are {kinds:?}"))?;
    let (frame_type, shown, existing) = frame_type(frame.payload)?;
    if existing || !shown {
        return Err("the frame isn't shown".into());
    }
    let key = frame_type == u32::from(KEY_FRAME);
    let has_seq = count(OBU_SEQUENCE_HEADER);
    if key != (has_seq == 1) || has_seq > 1 {
        return Err(format!(
            "a {} frame with {has_seq} sequence headers",
            if key { "key" } else { "non-key" }
        ));
    }
    let frames = count(OBU_FRAME) + count(OBU_FRAME_HEADER);
    if frames != 1 {
        return Err(format!("{frames} frames in one temporal unit"));
    }
    if count(OBU_FRAME_HEADER) == 1 && count(OBU_TILE_GROUP) == 0 {
        return Err("a frame header with no tile group".into());
    }
    if let Some(pos) = kinds.iter().position(|&k| k == OBU_SEQUENCE_HEADER)
        && pos != 1
    {
        return Err(format!(
            "the sequence header isn't right after the delimiter: {kinds:?}"
        ));
    }
    let sequence = obus
        .iter()
        .find(|o| o.kind == OBU_SEQUENCE_HEADER)
        .map(|o| parse_sequence(o.payload))
        .transpose()?;
    Ok(TemporalUnit {
        obus: kinds,
        key,
        size: sequence.map(|s| (s.width, s.height)),
        sequence,
    })
}

/// Whether the coded OBUs hold a key frame: the first frame header's type.
pub fn has_key_frame(coded: &[u8]) -> Result<bool, String> {
    let obus = parse(coded)?;
    let frame = obus
        .iter()
        .find(|o| matches!(o.kind, OBU_FRAME | OBU_FRAME_HEADER))
        .ok_or("the driver's output has no frame OBU")?;
    Ok(frame_type(frame.payload)?.0 == u32::from(KEY_FRAME))
}

/// Shapes a driver's output into a temporal unit: our temporal delimiter
/// first (a driver's own is dropped, or there was none), the sequence
/// header on a key frame (the driver's if it has one, else `sequence`), then
/// everything else as it came, size fields tidied. Returns whether the unit
/// is a key frame and whether the driver's sequence header was kept.
pub fn shape_temporal_unit(
    coded: &[u8],
    sequence: &[u8],
    out: &mut Vec<u8>,
) -> Result<(bool, bool), String> {
    let obus = parse(coded)?;
    let key = has_key_frame(coded)?;
    let has_sequence = obus.iter().any(|o| o.kind == OBU_SEQUENCE_HEADER);
    out.clear();
    out.extend_from_slice(&temporal_delimiter());
    if key && !has_sequence {
        out.extend_from_slice(sequence);
    }
    for o in &obus {
        match o.kind {
            OBU_TEMPORAL_DELIMITER => {}
            // A sequence header repeated on a non-key frame is allowed but
            // useless; the unit is checked as the players expect.
            OBU_SEQUENCE_HEADER if !key => {}
            _ => o.serialize(out),
        }
    }
    Ok((key, has_sequence))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(key: bool) -> Frame {
        Frame {
            key,
            order_hint: if key { 0 } else { 5 },
            primary_ref_frame: if key { PRIMARY_REF_NONE } else { 0 },
            refresh_frame_flags: if key { 0xff } else { 1 },
            ref_frame_idx: [0; 7],
            allow_screen_content_tools: false,
            force_integer_mv: false,
            disable_cdf_update: false,
            disable_frame_end_update_cdf: false,
            base_qindex: 100,
            filter_level: [0, 0],
            filter_level_uv: [0, 0],
            sharpness: 0,
            cdef: Cdef::single(3, 5, 0),
            tx_mode_select: true,
            reduced_tx_set: false,
            allow_high_precision_mv: false,
            interpolation_filter: FILTER_SWITCHABLE,
            render: None,
            frame_obu: true,
            size_bytes: 4,
        }
    }

    #[test]
    fn leb128_is_the_least_length_unless_padded() {
        assert_eq!(leb128(0, 1), [0]);
        assert_eq!(leb128(127, 1), [0x7f]);
        assert_eq!(leb128(128, 1), [0x80, 0x01]);
        assert_eq!(leb128(300, 1), [0xac, 0x02]);
        // Padded: continuation bytes, the value the same.
        assert_eq!(leb128(5, 4), [0x85, 0x80, 0x80, 0x00]);
        assert_eq!(leb128(200, 2), [0xc8, 0x01]);
    }

    #[test]
    fn a_temporal_delimiter_is_two_bytes() {
        assert_eq!(temporal_delimiter(), [0x12, 0x00]);
        assert_eq!(obu(OBU_SEQUENCE_HEADER, &[1, 2, 3], 1), [0x0a, 3, 1, 2, 3]);
    }

    #[test]
    fn the_sequence_header_reads_back() {
        let seq = Seq::new(1280, 720);
        let obus = sequence_header_obu(&seq);
        let parsed = parse(&obus).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].kind, OBU_SEQUENCE_HEADER);
        let info = parse_sequence(parsed[0].payload).unwrap();
        assert_eq!(
            info,
            SeqInfo {
                profile: 0,
                level_idx: 13,
                tier: 0,
                width: 1280,
                height: 720
            }
        );
        // 1280: 11 bits (the value minus one, 1279, needs 11); 720: 10.
        let seq = Seq::new(2560, 1440);
        let payload = sequence_header_payload(&seq);
        let info = parse_sequence(&payload).unwrap();
        assert_eq!((info.width, info.height), (2560, 1440));
        // The payload ends with the trailing bit and zeros.
        assert_ne!(*payload.last().unwrap(), 0);
    }

    #[test]
    fn the_sequence_header_has_the_av1c_the_stream_announces() {
        // `av01.0.13M.08`: profile 0, level index 13, main tier, 8 bit.
        let payload = sequence_header_payload(&Seq::new(1920, 1080));
        let info = parse_sequence(&payload).unwrap();
        assert_eq!((info.profile, info.level_idx, info.tier), (0, 13, 0));
    }

    #[test]
    fn a_key_frame_header_says_key_and_a_p_frame_inter() {
        let seq = Seq::new(1280, 720);
        let key = frame_header_obu(&seq, &frame(true));
        let obus = parse(&key.bytes).unwrap();
        assert_eq!(obus[0].kind, OBU_FRAME);
        assert_eq!(frame_type(obus[0].payload).unwrap(), (0, true, false));
        let inter = frame_header_obu(&seq, &frame(false));
        let obus = parse(&inter.bytes).unwrap();
        assert_eq!(frame_type(obus[0].payload).unwrap(), (1, true, false));
        // An inter frame has more to say (references, motion vectors).
        assert!(inter.offsets.total_bits > key.offsets.total_bits);
        // A frame header OBU is another type, with its trailing bit.
        let mut f = frame(false);
        f.frame_obu = false;
        let header = frame_header_obu(&seq, &f);
        assert_eq!(parse(&header.bytes).unwrap()[0].kind, OBU_FRAME_HEADER);
    }

    #[test]
    fn the_offsets_point_at_the_fields() {
        let seq = Seq::new(1280, 720);
        let mut f = frame(true);
        f.base_qindex = 0xA5;
        f.filter_level = [0x2a, 0x15];
        f.filter_level_uv = [0x3f, 0x01];
        let header = frame_header_obu(&seq, &f);
        let bits = |from: u32, count: u32| {
            (from..from + count).fold(0, |acc, at| {
                let byte = header.bytes[at as usize / 8];
                acc << 1 | u32::from(byte >> (7 - at % 8) & 1)
            })
        };
        assert_eq!(bits(header.offsets.qindex_bit, 8), 0xA5);
        assert_eq!(bits(header.offsets.loop_filter_bit, 6), 0x2a);
        assert_eq!(bits(header.offsets.loop_filter_bit + 6, 6), 0x15);
        assert_eq!(bits(header.offsets.loop_filter_bit + 12, 6), 0x3f);
        // damping_minus_3 3, one strength pair: 2 + 2 + 4 + 2 + 4 + 2 bits.
        assert_eq!(header.offsets.cdef_bits, 16);
        assert_eq!(bits(header.offsets.cdef_bit, 2), 3);
        assert_eq!(bits(header.offsets.cdef_bit + 4, 4), 5 >> 2);
        // The size field is the byte after the header byte, four long.
        assert_eq!(header.offsets.size_byte, 1);
        assert_eq!(
            &header.bytes[1..5],
            &[0x80 | (header.bytes.len() as u8 - 5), 0x80, 0x80, 0x00][..]
        );
    }

    #[test]
    fn the_size_field_is_as_long_as_asked() {
        let seq = Seq::new(1280, 720);
        let mut f = frame(false);
        f.size_bytes = 1;
        let short = frame_header_obu(&seq, &f);
        f.size_bytes = 4;
        let long = frame_header_obu(&seq, &f);
        assert_eq!(long.bytes.len(), short.bytes.len() + 3);
        let a = parse(&short.bytes).unwrap();
        let b = parse(&long.bytes).unwrap();
        assert_eq!(a[0].payload, b[0].payload);
    }

    #[test]
    fn render_size_is_written_when_it_differs() {
        let seq = Seq::new(1920, 1082);
        let mut f = frame(true);
        let plain = frame_header_obu(&seq, &f);
        f.render = Some((1920, 1080));
        let with = frame_header_obu(&seq, &f);
        assert_eq!(with.offsets.total_bits, plain.offsets.total_bits + 32);
    }

    #[test]
    fn small_frames_take_one_tile() {
        assert_eq!(min_tiles_log2(1280, 720, false), (0, 0));
        assert_eq!(min_tiles_log2(3840, 2160, false), (0, 0));
        assert_eq!(min_tiles_log2(4096, 2304, false), (0, 0));
        // Wider than 4096: two columns at least.
        assert_eq!(min_tiles_log2(4098, 1080, false).0, 1);
    }

    fn unit(key: bool, extra: &[u8]) -> Vec<u8> {
        let seq = Seq::new(1280, 720);
        let mut data = temporal_delimiter();
        if key {
            data.extend(sequence_header_obu(&seq));
        }
        data.extend(extra);
        data.extend(frame_header_obu(&seq, &frame(key)).bytes);
        data
    }

    #[test]
    fn a_checked_unit_is_delimiter_sequence_frame() {
        let key = check_temporal_unit(&unit(true, &[])).unwrap();
        assert_eq!(key.obus, [2, 1, 6]);
        assert!(key.key);
        assert_eq!(key.size, Some((1280, 720)));
        let inter = check_temporal_unit(&unit(false, &[])).unwrap();
        assert_eq!(inter.obus, [2, 6]);
        assert!(!inter.key);
        assert_eq!(inter.size, None);
    }

    #[test]
    fn a_unit_not_shaped_for_the_players_is_refused() {
        // No delimiter.
        let seq = Seq::new(1280, 720);
        let mut no_td = sequence_header_obu(&seq);
        no_td.extend(frame_header_obu(&seq, &frame(true)).bytes);
        assert!(
            check_temporal_unit(&no_td)
                .unwrap_err()
                .contains("delimiter")
        );
        // A key frame without the sequence header; an inter frame with one.
        let mut bare = temporal_delimiter();
        bare.extend(frame_header_obu(&seq, &frame(true)).bytes);
        assert!(
            check_temporal_unit(&bare)
                .unwrap_err()
                .contains("sequence header")
        );
        let mut p = temporal_delimiter();
        p.extend(sequence_header_obu(&seq));
        p.extend(frame_header_obu(&seq, &frame(false)).bytes);
        assert!(
            check_temporal_unit(&p)
                .unwrap_err()
                .contains("sequence header")
        );
        // Two frames.
        let mut two = unit(false, &[]);
        two.extend(frame_header_obu(&seq, &frame(false)).bytes);
        assert!(check_temporal_unit(&two).is_err());
        // A truncated OBU.
        let mut cut = unit(true, &[]);
        cut.truncate(cut.len() - 3);
        assert!(check_temporal_unit(&cut).is_err());
    }

    #[test]
    fn an_obu_without_a_size_runs_to_the_end() {
        let obus = parse(&[0x12, 0x00, 0x30, 1, 2, 3]).unwrap();
        assert_eq!(obus.len(), 2);
        assert_eq!((obus[1].kind, obus[1].payload), (6, &[1, 2, 3][..]));
        let mut out = Vec::new();
        obus[1].serialize(&mut out);
        assert_eq!(out, [0x32, 3, 1, 2, 3]);
    }

    #[test]
    fn a_drivers_output_is_shaped_into_a_unit() {
        let seq = Seq::new(1280, 720);
        let ours = sequence_header_obu(&seq);
        let frame_obu = frame_header_obu(&seq, &frame(true)).bytes;
        // The driver wrote only the frame: we add the delimiter and sequence.
        let mut out = Vec::new();
        let (key, kept) = shape_temporal_unit(&frame_obu, &ours, &mut out).unwrap();
        assert!(key && !kept);
        assert_eq!(check_temporal_unit(&out).unwrap().obus, [2, 1, 6]);
        // It wrote its own delimiter (twice) and sequence header: kept once.
        let mut theirs = temporal_delimiter();
        theirs.extend(sequence_header_obu(&Seq::new(1280, 722)));
        theirs.extend(&frame_obu);
        let (key, kept) = shape_temporal_unit(&theirs, &ours, &mut out).unwrap();
        assert!(key && kept);
        let unit = check_temporal_unit(&out).unwrap();
        assert_eq!(unit.size, Some((1280, 722)));
        // A frame that isn't a key one loses a stray sequence header.
        let p = frame_header_obu(&seq, &frame(false)).bytes;
        let mut stray = ours.clone();
        stray.extend(&p);
        let (key, _) = shape_temporal_unit(&stray, &ours, &mut out).unwrap();
        assert!(!key);
        assert_eq!(check_temporal_unit(&out).unwrap().obus, [2, 6]);
        // Nothing to shape without a frame.
        assert!(shape_temporal_unit(&ours, &ours, &mut out).is_err());
    }
}

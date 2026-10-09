//! H.264 bitstream syntax, written by us: the bit writer (exp-Golomb), the
//! Annex-B NAL framing, and the SPS, PPS and slice header the VA-API encoder
//! packs itself (`vaapi`).
//!
//! Why we write them: the output has to match what NVENC and x264 make (Annex-B,
//! SPS and PPS in front of every IDR, so a viewer can join at any one) and has
//! to say what plan §3.1 rule 8 needs: a VUI with `bitstream_restriction`,
//! `max_num_reorder_frames = 0`, so a decoder shows a frame the moment it has
//! decoded it instead of waiting for reordering that never comes. A driver's
//! own headers can't be relied on for that, so they are replaced
//! ([`fix_headers`]).
//!
//! Written from ITU-T H.264 (clauses 7.3.2.1.1 SPS, 7.3.2.2 PPS, 7.3.3 slice
//! header, E.1.1 VUI, 7.4 and B.1 for the NAL framing and emulation
//! prevention). Nothing in it comes from another project's code. Pure Rust, no
//! dependencies, so its tests run anywhere.

/// Writes bits, most significant first.
#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub struct BitWriter {
    bytes: Vec<u8>,
    /// Bits used in the last byte (0 = the last byte is full or there is none).
    used: u32,
}

impl BitWriter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Bits written so far.
    pub fn bit_len(&self) -> usize {
        if self.used == 0 {
            self.bytes.len() * 8
        } else {
            (self.bytes.len() - 1) * 8 + self.used as usize
        }
    }

    pub fn is_byte_aligned(&self) -> bool {
        self.used == 0
    }

    pub fn put_bit(&mut self, bit: bool) {
        if self.used == 0 {
            self.bytes.push(0);
        }
        if bit {
            *self.bytes.last_mut().expect("a byte was pushed") |= 0x80 >> self.used;
        }
        self.used = (self.used + 1) % 8;
    }

    /// The low `count` bits of `value` (at most 32), `u(n)` in the spec.
    pub fn put_bits(&mut self, value: u32, count: u32) {
        assert!(count <= 32);
        for shift in (0..count).rev() {
            self.put_bit((value >> shift) & 1 == 1);
        }
    }

    pub fn put_flag(&mut self, flag: bool) {
        self.put_bit(flag);
    }

    /// `ue(v)`: unsigned exp-Golomb.
    pub fn put_ue(&mut self, value: u32) {
        let code = u64::from(value) + 1;
        let len = 64 - code.leading_zeros();
        for _ in 0..len - 1 {
            self.put_bit(false);
        }
        for shift in (0..len).rev() {
            self.put_bit((code >> shift) & 1 == 1);
        }
    }

    /// `se(v)`: signed exp-Golomb (1 → 1, -1 → 2, 2 → 3, …).
    pub fn put_se(&mut self, value: i32) {
        let mapped = if value > 0 {
            (value as u32) * 2 - 1
        } else {
            value.unsigned_abs() * 2
        };
        self.put_ue(mapped);
    }

    /// `rbsp_trailing_bits()`: a one, then zeros up to the byte boundary.
    pub fn trailing_bits(&mut self) {
        self.put_bit(true);
        while !self.is_byte_aligned() {
            self.put_bit(false);
        }
    }

    /// Fills up to the byte boundary with `bit`.
    pub fn align_with(&mut self, bit: bool) {
        while !self.is_byte_aligned() {
            self.put_bit(bit);
        }
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

/// Reads bits, most significant first.
pub struct BitReader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> BitReader<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    pub fn bit(&mut self) -> Result<bool, &'static str> {
        let byte = self.bytes.get(self.pos / 8).ok_or("ran out of bits")?;
        let bit = (byte >> (7 - self.pos % 8)) & 1 == 1;
        self.pos += 1;
        Ok(bit)
    }

    pub fn bits(&mut self, count: u32) -> Result<u32, &'static str> {
        if count > 32 {
            return Err("more than 32 bits at once");
        }
        let mut value = 0u32;
        for _ in 0..count {
            value = (value << 1) | u32::from(self.bit()?);
        }
        Ok(value)
    }

    pub fn ue(&mut self) -> Result<u32, &'static str> {
        let mut zeros = 0;
        while !self.bit()? {
            zeros += 1;
            if zeros > 32 {
                return Err("exp-Golomb code too long");
            }
        }
        let rest = if zeros == 0 { 0 } else { self.bits(zeros)? };
        let code = (1u64 << zeros) | u64::from(rest);
        u32::try_from(code - 1).map_err(|_| "exp-Golomb value too big")
    }

    #[cfg(test)]
    pub fn se(&mut self) -> Result<i32, &'static str> {
        let k = self.ue()?;
        let magnitude = k.div_ceil(2) as i32;
        Ok(if k % 2 == 1 { magnitude } else { -magnitude })
    }
}

/// Inserts emulation prevention bytes: `00 00 0x` (x ≤ 3) in a payload becomes
/// `00 00 03 0x`, so no start code appears inside a NAL.
pub fn escape(rbsp: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rbsp.len() + rbsp.len() / 64 + 1);
    let mut zeros = 0;
    for &byte in rbsp {
        if zeros >= 2 && byte <= 3 {
            out.push(3);
            zeros = 0;
        }
        out.push(byte);
        zeros = if byte == 0 { zeros + 1 } else { 0 };
    }
    // A payload may not end in a zero byte.
    if zeros > 0 {
        out.push(3);
    }
    out
}

/// Takes the emulation prevention bytes out again.
pub fn unescape(payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len());
    let mut zeros = 0;
    for &byte in payload {
        if zeros >= 2 && byte == 3 {
            zeros = 0;
            continue;
        }
        out.push(byte);
        zeros = if byte == 0 { zeros + 1 } else { 0 };
    }
    out
}

/// Appends one NAL unit with a four-byte start code. `payload` is the RBSP
/// (emulation prevention is added here).
pub fn write_nal(out: &mut Vec<u8>, ref_idc: u8, kind: u8, payload: &[u8]) {
    out.extend_from_slice(&[0, 0, 0, 1, ((ref_idc & 3) << 5) | (kind & 0x1f)]);
    out.extend_from_slice(&escape(payload));
}

/// A NAL unit made by [`write_nal`] without its emulation prevention bytes
/// (start code and header kept).
pub fn unescaped_nal(nal: &[u8]) -> Vec<u8> {
    let split = (nal.len()).min(5);
    let mut out = nal[..split].to_vec();
    out.extend_from_slice(&unescape(&nal[split..]));
    out
}

pub mod av1;
pub mod h265;

pub mod h264 {
    use super::*;

    pub const NAL_SLICE: u8 = 1;
    pub const NAL_IDR: u8 = 5;
    pub const NAL_SEI: u8 = 6;
    pub const NAL_SPS: u8 = 7;
    pub const NAL_PPS: u8 = 8;
    pub const NAL_AUD: u8 = 9;

    pub const PROFILE_BASELINE: u8 = 66;
    pub const PROFILE_MAIN: u8 = 77;
    pub const PROFILE_HIGH: u8 = 100;

    /// `constraint_set1_flag`: with profile 66, Constrained Baseline.
    pub const CONSTRAINT_SET1: u8 = 0x40;

    /// Profiles whose SPS carries the chroma and bit depth fields.
    fn is_high_family(profile_idc: u8) -> bool {
        matches!(
            profile_idc,
            100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135
        )
    }

    /// The VUI we write: colour description, timing, and the bitstream
    /// restriction that says there is no reordering.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Vui {
        pub full_range: bool,
        /// H.273 code points (1 = BT.709).
        pub colour_primaries: u8,
        pub transfer_characteristics: u8,
        pub matrix_coefficients: u8,
        /// `(num_units_in_tick, time_scale)`.
        pub timing: Option<(u32, u32)>,
        pub max_num_reorder_frames: u32,
        pub max_dec_frame_buffering: u32,
    }

    impl Vui {
        /// BT.709 limited range at `fps` (field-based clock: two ticks a
        /// frame, as x264 writes it), no reordering.
        pub fn bt709_low_latency(fps: u32, max_dec_frame_buffering: u32) -> Self {
            Self {
                full_range: false,
                colour_primaries: 1,
                transfer_characteristics: 1,
                matrix_coefficients: 1,
                timing: Some((1, fps.max(1) * 2)),
                max_num_reorder_frames: 0,
                max_dec_frame_buffering,
            }
        }
    }

    /// A sequence parameter set for progressive 4:2:0 8-bit video (the only
    /// kind we make), with POC type 0 or 2.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Sps {
        pub profile_idc: u8,
        /// The eight bits after `profile_idc`: constraint_set0..5 flags and
        /// two reserved zero bits.
        pub constraint_flags: u8,
        pub level_idc: u8,
        pub sps_id: u32,
        pub log2_max_frame_num: u32,
        pub poc_type: u32,
        /// Used when `poc_type` is 0.
        pub log2_max_poc_lsb: u32,
        pub max_num_ref_frames: u32,
        pub width_mbs: u32,
        pub height_mbs: u32,
        pub direct_8x8_inference: bool,
        /// Frame cropping, in crop units (2 luma pixels for 4:2:0): left,
        /// right, top, bottom.
        pub crop: [u32; 4],
        pub vui: Option<Vui>,
    }

    impl Sps {
        /// The cropped picture size in pixels.
        pub fn picture_size(&self) -> (u32, u32) {
            (
                self.width_mbs * 16 - 2 * (self.crop[0] + self.crop[1]),
                self.height_mbs * 16 - 2 * (self.crop[2] + self.crop[3]),
            )
        }
    }

    /// `seq_parameter_set_rbsp()` bytes (no NAL header).
    pub fn sps_rbsp(sps: &Sps) -> Vec<u8> {
        let mut w = BitWriter::new();
        w.put_bits(u32::from(sps.profile_idc), 8);
        w.put_bits(u32::from(sps.constraint_flags), 8);
        w.put_bits(u32::from(sps.level_idc), 8);
        w.put_ue(sps.sps_id);
        if is_high_family(sps.profile_idc) {
            w.put_ue(1); // chroma_format_idc: 4:2:0
            w.put_ue(0); // bit_depth_luma_minus8
            w.put_ue(0); // bit_depth_chroma_minus8
            w.put_flag(false); // qpprime_y_zero_transform_bypass_flag
            w.put_flag(false); // seq_scaling_matrix_present_flag
        }
        w.put_ue(sps.log2_max_frame_num - 4);
        w.put_ue(sps.poc_type);
        if sps.poc_type == 0 {
            w.put_ue(sps.log2_max_poc_lsb - 4);
        }
        w.put_ue(sps.max_num_ref_frames);
        w.put_flag(false); // gaps_in_frame_num_value_allowed_flag
        w.put_ue(sps.width_mbs - 1);
        w.put_ue(sps.height_mbs - 1); // pic_height_in_map_units_minus1
        w.put_flag(true); // frame_mbs_only_flag
        w.put_flag(sps.direct_8x8_inference);
        let cropped = sps.crop != [0; 4];
        w.put_flag(cropped);
        if cropped {
            for offset in sps.crop {
                w.put_ue(offset);
            }
        }
        w.put_flag(sps.vui.is_some());
        if let Some(vui) = &sps.vui {
            write_vui(&mut w, vui);
        }
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
        w.put_flag(vui.timing.is_some());
        if let Some((units, scale)) = vui.timing {
            w.put_bits(units, 32);
            w.put_bits(scale, 32);
            w.put_flag(true); // fixed_frame_rate_flag
        }
        w.put_flag(false); // nal_hrd_parameters_present_flag
        w.put_flag(false); // vcl_hrd_parameters_present_flag
        w.put_flag(false); // pic_struct_present_flag
        w.put_flag(true); // bitstream_restriction_flag
        w.put_flag(true); // motion_vectors_over_pic_boundaries_flag
        w.put_ue(0); // max_bytes_per_pic_denom
        w.put_ue(0); // max_bits_per_mb_denom
        w.put_ue(15); // log2_max_mv_length_horizontal
        w.put_ue(15); // log2_max_mv_length_vertical
        w.put_ue(vui.max_num_reorder_frames);
        w.put_ue(vui.max_dec_frame_buffering);
    }

    /// The SPS as a NAL unit with a start code.
    pub fn sps_nal(sps: &Sps) -> Vec<u8> {
        let mut out = Vec::new();
        write_nal(&mut out, 3, NAL_SPS, &sps_rbsp(sps));
        out
    }

    /// Reads an SPS (the RBSP, emulation prevention already removed) far
    /// enough to write it again with a different VUI. Errors for what we
    /// don't make: interlaced video, POC type 1, scaling matrices, other
    /// chroma formats or bit depths.
    pub fn parse_sps(rbsp: &[u8]) -> Result<Sps, &'static str> {
        let mut r = BitReader::new(rbsp);
        let profile_idc = r.bits(8)? as u8;
        let constraint_flags = r.bits(8)? as u8;
        let level_idc = r.bits(8)? as u8;
        let sps_id = r.ue()?;
        if is_high_family(profile_idc) {
            if r.ue()? != 1 {
                return Err("not 4:2:0");
            }
            if r.ue()? != 0 || r.ue()? != 0 {
                return Err("not 8-bit");
            }
            if r.bit()? {
                return Err("transform bypass");
            }
            if r.bit()? {
                return Err("scaling matrices");
            }
        }
        let log2_max_frame_num = r.ue()? + 4;
        let poc_type = r.ue()?;
        let mut log2_max_poc_lsb = 0;
        match poc_type {
            0 => log2_max_poc_lsb = r.ue()? + 4,
            2 => {}
            _ => return Err("POC type 1"),
        }
        let max_num_ref_frames = r.ue()?;
        r.bit()?; // gaps_in_frame_num_value_allowed_flag
        let width_mbs = r.ue()? + 1;
        let height_mbs = r.ue()? + 1;
        if !r.bit()? {
            return Err("interlaced");
        }
        let direct_8x8_inference = r.bit()?;
        let mut crop = [0; 4];
        if r.bit()? {
            for offset in &mut crop {
                *offset = r.ue()?;
            }
        }
        // The VUI (if any) is dropped: the caller sets its own.
        Ok(Sps {
            profile_idc,
            constraint_flags,
            level_idc,
            sps_id,
            log2_max_frame_num,
            poc_type,
            log2_max_poc_lsb,
            max_num_ref_frames,
            width_mbs,
            height_mbs,
            direct_8x8_inference,
            crop,
            vui: None,
        })
    }

    /// A picture parameter set: one slice group, one reference in list 0.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Pps {
        pub pps_id: u32,
        pub sps_id: u32,
        /// CABAC (Main and High) or CAVLC.
        pub cabac: bool,
        pub pic_init_qp: i32,
        pub chroma_qp_index_offset: i32,
        pub deblocking_filter_control_present: bool,
        pub constrained_intra_pred: bool,
        /// High profile: the 8×8 transform. `None` leaves the extension out
        /// (Baseline and Main).
        pub transform_8x8_mode: Option<bool>,
    }

    pub fn pps_rbsp(pps: &Pps) -> Vec<u8> {
        let mut w = BitWriter::new();
        w.put_ue(pps.pps_id);
        w.put_ue(pps.sps_id);
        w.put_flag(pps.cabac);
        w.put_flag(false); // bottom_field_pic_order_in_frame_present_flag
        w.put_ue(0); // num_slice_groups_minus1
        w.put_ue(0); // num_ref_idx_l0_default_active_minus1
        w.put_ue(0); // num_ref_idx_l1_default_active_minus1
        w.put_flag(false); // weighted_pred_flag
        w.put_bits(0, 2); // weighted_bipred_idc
        w.put_se(pps.pic_init_qp - 26);
        w.put_se(0); // pic_init_qs_minus26
        w.put_se(pps.chroma_qp_index_offset);
        w.put_flag(pps.deblocking_filter_control_present);
        w.put_flag(pps.constrained_intra_pred);
        w.put_flag(false); // redundant_pic_cnt_present_flag
        if let Some(transform_8x8) = pps.transform_8x8_mode {
            w.put_flag(transform_8x8);
            w.put_flag(false); // pic_scaling_matrix_present_flag
            w.put_se(pps.chroma_qp_index_offset); // second_chroma_qp_index_offset
        }
        w.trailing_bits();
        w.into_bytes()
    }

    pub fn pps_nal(pps: &Pps) -> Vec<u8> {
        let mut out = Vec::new();
        write_nal(&mut out, 3, NAL_PPS, &pps_rbsp(pps));
        out
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum SliceKind {
        P,
        I,
    }

    /// What differs from slice to slice (one slice per picture, a reference
    /// picture, no reordering).
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct SliceHeader {
        pub kind: SliceKind,
        pub idr: bool,
        pub frame_num: u32,
        pub idr_pic_id: u32,
        pub poc_lsb: u32,
        pub qp_delta: i32,
        pub disable_deblocking_filter_idc: u32,
        pub alpha_c0_offset_div2: i32,
        pub beta_offset_div2: i32,
    }

    /// A slice NAL's header as packed header data: the NAL unit with its start
    /// code, the header bits (ending at a byte boundary for CABAC, with
    /// `cabac_alignment_one_bit`s) and no slice data. Returns the bytes and
    /// the length in bits that are the header (what a VA packed header
    /// buffer states).
    pub fn slice_header_nal(sps: &Sps, pps: &Pps, h: &SliceHeader) -> (Vec<u8>, u32) {
        let mut w = BitWriter::new();
        w.put_ue(0); // first_mb_in_slice
        w.put_ue(match h.kind {
            SliceKind::P => 0,
            SliceKind::I => 2,
        });
        w.put_ue(pps.pps_id);
        w.put_bits(h.frame_num, sps.log2_max_frame_num);
        if h.idr {
            w.put_ue(h.idr_pic_id);
        }
        if sps.poc_type == 0 {
            w.put_bits(h.poc_lsb, sps.log2_max_poc_lsb);
        }
        if h.kind == SliceKind::P {
            w.put_flag(false); // num_ref_idx_active_override_flag
            w.put_flag(false); // ref_pic_list_modification_flag_l0
        }
        // dec_ref_pic_marking(): every picture is a reference (nal_ref_idc 3).
        if h.idr {
            w.put_flag(false); // no_output_of_prior_pics_flag
            w.put_flag(false); // long_term_reference_flag
        } else {
            w.put_flag(false); // adaptive_ref_pic_marking_mode_flag
        }
        if pps.cabac && h.kind != SliceKind::I {
            w.put_ue(0); // cabac_init_idc
        }
        w.put_se(h.qp_delta);
        if pps.deblocking_filter_control_present {
            w.put_ue(h.disable_deblocking_filter_idc);
            if h.disable_deblocking_filter_idc != 1 {
                w.put_se(h.alpha_c0_offset_div2);
                w.put_se(h.beta_offset_div2);
            }
        }
        if pps.cabac {
            w.align_with(true); // cabac_alignment_one_bit
        }
        let header_bits = w.bit_len();
        w.align_with(false);
        let mut out = Vec::new();
        let nal_type = if h.idr { NAL_IDR } else { NAL_SLICE };
        write_nal(&mut out, 3, nal_type, w.bytes());
        // Emulation prevention bytes are whole bytes before the last, so the
        // padding of the last byte is what separates bytes from bits.
        let padding = w.bytes().len() * 8 - header_bits;
        let bits = out.len() * 8 - padding;
        (out, bits as u32)
    }

    /// Where the NAL units of an Annex-B stream are.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct NalRange {
        /// The first byte of the start code.
        pub start: usize,
        /// The NAL header byte.
        pub header: usize,
        /// One past the last byte (before the next start code).
        pub end: usize,
    }

    impl NalRange {
        pub fn kind(&self, stream: &[u8]) -> u8 {
            stream[self.header] & 0x1f
        }
    }

    /// Splits an Annex-B stream (three- or four-byte start codes).
    pub fn nal_units(stream: &[u8]) -> Vec<NalRange> {
        let mut found = Vec::new();
        let mut i = 0;
        while i + 3 <= stream.len() {
            if stream[i] == 0 && stream[i + 1] == 0 && stream[i + 2] == 1 {
                // A four-byte start code begins one zero earlier.
                let start = if i > 0 && stream[i - 1] == 0 {
                    i - 1
                } else {
                    i
                };
                found.push((start, i + 3));
                i += 3;
            } else {
                i += 1;
            }
        }
        let mut units = Vec::with_capacity(found.len());
        for (n, &(start, header)) in found.iter().enumerate() {
            if header >= stream.len() {
                continue;
            }
            let end = found.get(n + 1).map_or(stream.len(), |&(next, _)| next);
            units.push(NalRange { start, header, end });
        }
        units
    }

    /// Whether the stream has an IDR slice.
    pub fn has_idr(stream: &[u8]) -> bool {
        nal_units(stream).iter().any(|n| n.kind(stream) == NAL_IDR)
    }

    /// What [`fix_headers`] did.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct Fixed {
        /// SPS NALs replaced by ones with our VUI.
        pub sps_rewritten: u32,
        /// An SPS the parser couldn't take, left as it was.
        pub sps_kept: u32,
        /// Headers added because the encoder's output had none.
        pub added: u32,
    }

    /// Makes an IDR access unit carry the SPS and PPS we mean: the driver's
    /// SPS is parsed and written again with our VUI (kept as it is if it
    /// uses what we don't parse), and a missing SPS or PPS is added in front
    /// of the first slice from `sps` and `pps`. Everything else is copied
    /// as it is. A stream without an IDR slice is copied unchanged.
    pub fn fix_headers(stream: &[u8], sps: &Sps, pps: &Pps, out: &mut Vec<u8>) -> Fixed {
        let units = nal_units(stream);
        let mut fixed = Fixed::default();
        out.clear();
        if !units.iter().any(|n| n.kind(stream) == NAL_IDR) {
            out.extend_from_slice(stream);
            return fixed;
        }
        let (mut have_sps, mut have_pps) = (false, false);
        // Anything before the first start code stays in front.
        if let Some(first) = units.first() {
            out.extend_from_slice(&stream[..first.start]);
        }
        for nal in &units {
            let kind = nal.kind(stream);
            if matches!(kind, NAL_SLICE | NAL_IDR) {
                if !have_sps {
                    out.extend_from_slice(&sps_nal(sps));
                    have_sps = true;
                    fixed.added += 1;
                }
                if !have_pps {
                    out.extend_from_slice(&pps_nal(pps));
                    have_pps = true;
                    fixed.added += 1;
                }
            }
            let bytes = &stream[nal.start..nal.end];
            match kind {
                NAL_SPS => {
                    have_sps = true;
                    let rbsp = unescape(&stream[nal.header + 1..nal.end]);
                    match parse_sps(&rbsp) {
                        Ok(mut parsed) => {
                            // The VUI is ours; a decoder's DPB must still
                            // hold the references the driver's SPS asks for.
                            parsed.vui = sps.vui.clone().map(|mut vui| {
                                vui.max_dec_frame_buffering =
                                    vui.max_dec_frame_buffering.max(parsed.max_num_ref_frames);
                                vui
                            });
                            out.extend_from_slice(&sps_nal(&parsed));
                            fixed.sps_rewritten += 1;
                        }
                        Err(_) => {
                            out.extend_from_slice(bytes);
                            fixed.sps_kept += 1;
                        }
                    }
                }
                NAL_PPS => {
                    have_pps = true;
                    out.extend_from_slice(bytes);
                }
                _ => out.extend_from_slice(bytes),
            }
        }
        fixed
    }

    /// The lowest level (`level_idc`) that takes `width`×`height` at `fps`
    /// (Table A-1: MaxFS and MaxMBPS), or the highest one if none does.
    pub fn level_for(width: u32, height: u32, fps: u32) -> u8 {
        // (level_idc, MaxMBPS, MaxFS)
        const LEVELS: [(u8, u32, u32); 12] = [
            (30, 40_500, 1_620),
            (31, 108_000, 3_600),
            (32, 216_000, 5_120),
            (40, 245_760, 8_192),
            (41, 245_760, 8_192),
            (42, 522_240, 8_704),
            (50, 589_824, 22_080),
            (51, 983_040, 36_864),
            (52, 2_073_600, 36_864),
            (60, 4_177_920, 139_264),
            (61, 8_355_840, 139_264),
            (62, 16_711_680, 139_264),
        ];
        let frame_mbs = width.div_ceil(16) * height.div_ceil(16);
        let mb_rate = u64::from(frame_mbs) * u64::from(fps);
        LEVELS
            .iter()
            .find(|&&(_, max_rate, max_frame)| {
                u64::from(max_rate) >= mb_rate && max_frame >= frame_mbs
            })
            .map_or(62, |l| l.0)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn hex(bytes: &[u8]) -> String {
            bytes.iter().map(|b| format!("{b:02x}")).collect()
        }

        fn sample_sps(vui: Option<Vui>) -> Sps {
            Sps {
                profile_idc: PROFILE_HIGH,
                constraint_flags: 0,
                level_idc: 51,
                sps_id: 0,
                log2_max_frame_num: 8,
                poc_type: 0,
                log2_max_poc_lsb: 12,
                max_num_ref_frames: 1,
                width_mbs: 160,
                height_mbs: 90,
                direct_8x8_inference: true,
                crop: [0; 4],
                vui,
            }
        }

        #[test]
        fn exp_golomb_codes() {
            // Table 9-3: 0 → 1, 1 → 010, 2 → 011, 3 → 00100, 4 → 00101.
            for (value, bits) in [
                (0, "1"),
                (1, "010"),
                (2, "011"),
                (3, "00100"),
                (4, "00101"),
                (7, "0001000"),
                (254, "000000011111111"),
            ] {
                let mut w = BitWriter::new();
                w.put_ue(value);
                let text: String = (0..w.bit_len())
                    .map(|i| {
                        if (w.bytes()[i / 8] >> (7 - i % 8)) & 1 == 1 {
                            '1'
                        } else {
                            '0'
                        }
                    })
                    .collect();
                assert_eq!(text, bits, "ue({value})");
            }
        }

        #[test]
        fn signed_exp_golomb_maps_as_the_spec_says() {
            // Table 9-3: se 0 → 0, 1 → 1, -1 → 2, 2 → 3, -2 → 4.
            for (value, code) in [
                (0, 0),
                (1, 1),
                (-1, 2),
                (2, 3),
                (-2, 4),
                (10, 19),
                (-10, 20),
            ] {
                let mut a = BitWriter::new();
                a.put_se(value);
                let mut b = BitWriter::new();
                b.put_ue(code);
                assert_eq!(a, b, "se({value})");
                let bytes = a.into_bytes();
                assert_eq!(BitReader::new(&bytes).se().unwrap(), value);
            }
        }

        #[test]
        fn bits_pack_most_significant_first() {
            let mut w = BitWriter::new();
            w.put_bits(0b101, 3);
            w.put_bit(true);
            w.put_bits(0xA, 4);
            w.put_bits(0x1FF, 9);
            assert_eq!(w.bit_len(), 17);
            assert_eq!(w.bytes(), [0b1011_1010, 0xFF, 0x80]);
            w.trailing_bits();
            assert_eq!(w.bytes(), [0b1011_1010, 0xFF, 0xC0]);
        }

        #[test]
        fn emulation_prevention() {
            assert_eq!(escape(&[0, 0, 0]), [0, 0, 3, 0, 3]);
            assert_eq!(escape(&[0, 0, 1]), [0, 0, 3, 1]);
            assert_eq!(escape(&[0, 0, 2, 0, 0, 3]), [0, 0, 3, 2, 0, 0, 3, 3]);
            assert_eq!(escape(&[0, 0, 4]), [0, 0, 4]);
            assert_eq!(escape(&[1, 0, 0, 0xff]), [1, 0, 0, 0xff]);
            // A payload never ends in zero.
            assert_eq!(escape(&[7, 0, 0]), [7, 0, 0, 3]);
            for sample in [
                vec![0, 0, 0, 0, 0, 1, 0, 0, 3],
                vec![0, 0, 3],
                vec![5, 0, 0, 0, 7],
            ] {
                assert_eq!(unescape(&escape(&sample)), sample.clone(), "{sample:?}");
            }
        }

        #[test]
        fn a_baseline_sps_without_vui_has_known_bytes() {
            // Constrained Baseline, level 3.1, 320×240, POC type 2, one
            // reference, log2_max_frame_num 4. The expected bytes come from a
            // separate bit-by-bit implementation of 7.3.2.1.1 (a short Python
            // script), not from this module.
            let sps = Sps {
                profile_idc: PROFILE_BASELINE,
                constraint_flags: CONSTRAINT_SET1,
                level_idc: 31,
                sps_id: 0,
                log2_max_frame_num: 4,
                poc_type: 2,
                log2_max_poc_lsb: 0,
                max_num_ref_frames: 1,
                width_mbs: 20,
                height_mbs: 15,
                direct_8x8_inference: true,
                crop: [0; 4],
                vui: None,
            };
            assert_eq!(hex(&sps_rbsp(&sps)), "42401fda0507e4");
        }

        #[test]
        fn a_high_profile_sps_has_known_bytes_with_and_without_the_vui() {
            // 2560×1440 (160×90 MBs), level 5.1, POC type 0 (12-bit LSB),
            // 8-bit frame_num, one reference. Same provenance as above.
            assert_eq!(hex(&sps_rbsp(&sample_sps(None))), "640033ac2c4a00a002d640");
            assert_eq!(
                hex(&sps_rbsp(&sample_sps(Some(Vui::bt709_low_latency(60, 1))))),
                "640033ac2c4a00a002d69a808080a00000002000000f11e1008540"
            );
        }

        #[test]
        fn the_sps_reads_back_as_written() {
            for sps in [
                sample_sps(None),
                sample_sps(Some(Vui::bt709_low_latency(60, 1))),
                Sps {
                    profile_idc: PROFILE_MAIN,
                    poc_type: 2,
                    log2_max_poc_lsb: 0,
                    width_mbs: 120,
                    height_mbs: 68,
                    crop: [0, 0, 0, 4],
                    ..sample_sps(Some(Vui::bt709_low_latency(120, 2)))
                },
            ] {
                let rbsp = sps_rbsp(&sps);
                let mut parsed = parse_sps(&rbsp).unwrap();
                // The VUI isn't read back.
                parsed.vui = sps.vui.clone();
                assert_eq!(parsed, sps);
            }
        }

        #[test]
        fn the_picture_size_follows_the_cropping() {
            // 1080p: 68 MB rows, bottom crop of 4 units of 2 pixels.
            let sps = Sps {
                height_mbs: 68,
                width_mbs: 120,
                crop: [0, 0, 0, 4],
                ..sample_sps(None)
            };
            assert_eq!(sps.picture_size(), (1920, 1080));
            assert_eq!(sample_sps(None).picture_size(), (2560, 1440));
        }

        #[test]
        fn the_vui_says_there_is_no_reordering() {
            let sps = sample_sps(Some(Vui::bt709_low_latency(60, 1)));
            let rbsp = sps_rbsp(&sps);
            // The VUI is the tail: read it back by hand. Skip the fields
            // before it by parsing the SPS the same way.
            let mut r = BitReader::new(&rbsp);
            r.bits(24).unwrap();
            r.ue().unwrap(); // sps id
            for _ in 0..3 {
                r.ue().unwrap(); // chroma format, bit depths
            }
            r.bits(2).unwrap(); // bypass, scaling matrix
            r.ue().unwrap(); // log2_max_frame_num
            assert_eq!(r.ue().unwrap(), 0); // poc type
            r.ue().unwrap(); // log2_max_poc_lsb
            r.ue().unwrap(); // refs
            r.bit().unwrap();
            r.ue().unwrap();
            r.ue().unwrap();
            r.bits(2).unwrap(); // frame_mbs_only, direct_8x8
            assert!(!r.bit().unwrap()); // no cropping
            assert!(r.bit().unwrap()); // vui present
            assert!(!r.bit().unwrap()); // aspect ratio
            assert!(!r.bit().unwrap()); // overscan
            assert!(r.bit().unwrap()); // video signal type
            assert_eq!(r.bits(3).unwrap(), 5);
            assert!(!r.bit().unwrap()); // limited range
            assert!(r.bit().unwrap()); // colour description
            assert_eq!(
                (r.bits(8).unwrap(), r.bits(8).unwrap(), r.bits(8).unwrap()),
                (1, 1, 1)
            );
            assert!(!r.bit().unwrap()); // chroma loc
            assert!(r.bit().unwrap()); // timing
            assert_eq!((r.bits(32).unwrap(), r.bits(32).unwrap()), (1, 120));
            assert!(r.bit().unwrap()); // fixed frame rate
            assert_eq!(r.bits(3).unwrap(), 0); // hrd x2, pic_struct
            assert!(r.bit().unwrap()); // bitstream_restriction_flag
            assert!(r.bit().unwrap()); // mv over pic boundaries
            assert_eq!((r.ue().unwrap(), r.ue().unwrap()), (0, 0));
            assert_eq!((r.ue().unwrap(), r.ue().unwrap()), (15, 15));
            assert_eq!(r.ue().unwrap(), 0, "max_num_reorder_frames");
            assert_eq!(r.ue().unwrap(), 1, "max_dec_frame_buffering");
            // Then the stop bit and zeros.
            assert!(r.bit().unwrap());
        }

        #[test]
        fn pps_bytes_are_known() {
            // CABAC, QP 26, deblocking control present. Main: no extension;
            // High: transform_8x8_mode_flag 1, no scaling matrix, second
            // chroma offset 0. Same provenance as the SPS vectors.
            let main = Pps {
                pps_id: 0,
                sps_id: 0,
                cabac: true,
                pic_init_qp: 26,
                chroma_qp_index_offset: 0,
                deblocking_filter_control_present: true,
                constrained_intra_pred: false,
                transform_8x8_mode: None,
            };
            assert_eq!(hex(&pps_rbsp(&main)), "ee3c80");
            let high = Pps {
                transform_8x8_mode: Some(true),
                ..main.clone()
            };
            assert_eq!(hex(&pps_rbsp(&high)), "ee3cb0");
            // A different QP and chroma offset round into se().
            let other = Pps {
                pic_init_qp: 30,
                chroma_qp_index_offset: -2,
                ..main
            };
            let rbsp = pps_rbsp(&other);
            let mut r = BitReader::new(&rbsp);
            r.ue().unwrap();
            r.ue().unwrap();
            r.bits(2).unwrap();
            r.ue().unwrap();
            r.ue().unwrap();
            r.ue().unwrap();
            r.bits(3).unwrap();
            assert_eq!(r.se().unwrap(), 4);
            assert_eq!(r.se().unwrap(), 0);
            assert_eq!(r.se().unwrap(), -2);
        }

        #[test]
        fn a_nal_can_be_taken_back_to_raw_bytes() {
            let sps = sample_sps(Some(Vui::bt709_low_latency(60, 1)));
            let escaped = sps_nal(&sps);
            // The VUI's timing fields have zero runs that need escaping.
            let raw = unescaped_nal(&escaped);
            assert!(raw.len() < escaped.len());
            assert_eq!(&raw[..5], &escaped[..5]);
            assert_eq!(escape(&raw[5..]), &escaped[5..]);
        }

        #[test]
        fn nals_have_start_codes_and_headers() {
            let pps = Pps {
                pps_id: 0,
                sps_id: 0,
                cabac: false,
                pic_init_qp: 26,
                chroma_qp_index_offset: 0,
                deblocking_filter_control_present: true,
                constrained_intra_pred: false,
                transform_8x8_mode: None,
            };
            let nal = pps_nal(&pps);
            assert_eq!(&nal[..5], [0, 0, 0, 1, 0x68]);
            let sps = sps_nal(&sample_sps(None));
            assert_eq!(&sps[..5], [0, 0, 0, 1, 0x67]);
            assert_eq!(&sps[5..8], [100, 0, 51]);
        }

        fn idr_header() -> SliceHeader {
            SliceHeader {
                kind: SliceKind::I,
                idr: true,
                frame_num: 0,
                idr_pic_id: 0,
                poc_lsb: 0,
                qp_delta: 0,
                disable_deblocking_filter_idc: 0,
                alpha_c0_offset_div2: 0,
                beta_offset_div2: 0,
            }
        }

        #[test]
        fn an_idr_slice_header_for_cavlc_has_known_bytes() {
            let sps = sample_sps(None);
            let pps = Pps {
                pps_id: 0,
                sps_id: 0,
                cabac: false,
                pic_init_qp: 26,
                chroma_qp_index_offset: 0,
                deblocking_filter_control_present: true,
                constrained_intra_pred: false,
                transform_8x8_mode: None,
            };
            let (bytes, bits) = slice_header_nal(&sps, &pps, &idr_header());
            // first_mb 0, I slice, pps 0, frame_num 0 (8 bits), idr_pic_id
            // 0, POC LSB 0 (12 bits), the two marking flags, qp_delta 0,
            // deblocking idc 0 with both offsets 0: 32 bits, from the same
            // separate implementation as the other vectors.
            assert_eq!(hex(&bytes), "00000001 65b804000f".replace(' ', ""));
            // The start code and NAL header count in a packed header's length.
            assert_eq!(bits, 72);
        }

        #[test]
        fn cabac_slice_headers_end_on_a_byte_boundary() {
            let sps = Sps {
                poc_type: 2,
                log2_max_poc_lsb: 0,
                ..sample_sps(None)
            };
            let pps = Pps {
                pps_id: 0,
                sps_id: 0,
                cabac: true,
                pic_init_qp: 26,
                chroma_qp_index_offset: 0,
                deblocking_filter_control_present: true,
                constrained_intra_pred: false,
                transform_8x8_mode: Some(true),
            };
            for header in [
                idr_header(),
                SliceHeader {
                    kind: SliceKind::P,
                    idr: false,
                    frame_num: 5,
                    ..idr_header()
                },
            ] {
                let (bytes, bits) = slice_header_nal(&sps, &pps, &header);
                assert_eq!(bits % 8, 0);
                assert_eq!(bits as usize, bytes.len() * 8, "no escapes expected here");
                // The alignment bits are ones.
                assert_eq!(bytes.last().unwrap() & 1, 1);
            }
            // A P slice has cabac_init_idc; an I slice doesn't.
            let (i, _) = slice_header_nal(&sps, &pps, &idr_header());
            let (p, _) = slice_header_nal(
                &sps,
                &pps,
                &SliceHeader {
                    kind: SliceKind::P,
                    idr: false,
                    ..idr_header()
                },
            );
            assert_eq!(&p[..5], [0, 0, 0, 1, 0x61]);
            assert_eq!(&i[..5], [0, 0, 0, 1, 0x65]);
        }

        #[test]
        fn nal_units_are_found_with_either_start_code() {
            let stream = [
                0, 0, 0, 1, 0x67, 1, 2, 3, //
                0, 0, 1, 0x68, 4, 5, //
                0, 0, 0, 1, 0x65, 9, 9, 9, 9,
            ];
            let units = nal_units(&stream);
            assert_eq!(units.len(), 3);
            assert_eq!(
                units[0],
                NalRange {
                    start: 0,
                    header: 4,
                    end: 8
                }
            );
            assert_eq!(
                units[1],
                NalRange {
                    start: 8,
                    header: 11,
                    end: 14
                }
            );
            assert_eq!(
                units[2],
                NalRange {
                    start: 14,
                    header: 18,
                    end: 23
                }
            );
            assert_eq!(
                units.iter().map(|u| u.kind(&stream)).collect::<Vec<_>>(),
                [NAL_SPS, NAL_PPS, NAL_IDR]
            );
            assert!(has_idr(&stream));
            assert!(!has_idr(&stream[..14]));
        }

        fn main_pps() -> Pps {
            Pps {
                pps_id: 0,
                sps_id: 0,
                cabac: true,
                pic_init_qp: 26,
                chroma_qp_index_offset: 0,
                deblocking_filter_control_present: true,
                constrained_intra_pred: false,
                transform_8x8_mode: Some(true),
            }
        }

        #[test]
        fn a_drivers_sps_gets_our_vui() {
            // A driver's SPS without a VUI, then its PPS and an IDR slice.
            let theirs = sample_sps(None);
            let ours = sample_sps(Some(Vui::bt709_low_latency(60, 1)));
            let mut stream = sps_nal(&theirs);
            stream.extend_from_slice(&pps_nal(&main_pps()));
            stream.extend_from_slice(&[0, 0, 0, 1, 0x65, 0x88, 0x84, 0x21]);
            let mut out = Vec::new();
            let fixed = fix_headers(&stream, &ours, &main_pps(), &mut out);
            assert_eq!(
                fixed,
                Fixed {
                    sps_rewritten: 1,
                    ..Fixed::default()
                }
            );
            let units = nal_units(&out);
            assert_eq!(
                units.iter().map(|u| u.kind(&out)).collect::<Vec<_>>(),
                [NAL_SPS, NAL_PPS, NAL_IDR]
            );
            // The SPS is now the one with the VUI, the slice untouched.
            assert_eq!(&out[units[0].start..units[0].end], sps_nal(&ours));
            assert_eq!(
                &out[units[2].start..units[2].end],
                [0, 0, 0, 1, 0x65, 0x88, 0x84, 0x21]
            );
        }

        #[test]
        fn a_driver_sps_that_asks_for_more_references_keeps_them_in_the_dpb() {
            let theirs = Sps {
                max_num_ref_frames: 4,
                ..sample_sps(None)
            };
            let ours = sample_sps(Some(Vui::bt709_low_latency(60, 1)));
            let mut stream = sps_nal(&theirs);
            stream.extend_from_slice(&pps_nal(&main_pps()));
            stream.extend_from_slice(&[0, 0, 0, 1, 0x65, 1]);
            let mut out = Vec::new();
            fix_headers(&stream, &ours, &main_pps(), &mut out);
            let units = nal_units(&out);
            let rbsp = unescape(&out[units[0].header + 1..units[0].end]);
            let parsed = parse_sps(&rbsp).unwrap();
            assert_eq!(parsed.max_num_ref_frames, 4);
            // (the VUI's max_dec_frame_buffering is the last ue before the
            // stop bit; 4 is ue 00101)
            assert!(out.len() > stream.len() - 3);
        }

        #[test]
        fn missing_headers_are_added_before_the_idr_slice() {
            let ours = sample_sps(Some(Vui::bt709_low_latency(60, 1)));
            let stream = [0, 0, 0, 1, 0x65, 0xaa, 0xbb];
            let mut out = Vec::new();
            let fixed = fix_headers(&stream, &ours, &main_pps(), &mut out);
            assert_eq!(fixed.added, 2);
            let units = nal_units(&out);
            assert_eq!(
                units.iter().map(|u| u.kind(&out)).collect::<Vec<_>>(),
                [NAL_SPS, NAL_PPS, NAL_IDR]
            );
            assert!(out.ends_with(&[0, 0, 0, 1, 0x65, 0xaa, 0xbb]));
        }

        #[test]
        fn an_sps_we_cant_read_is_left_alone() {
            let ours = sample_sps(Some(Vui::bt709_low_latency(60, 1)));
            // Interlaced: frame_mbs_only_flag 0. Hand-built with POC type 2.
            let mut w = BitWriter::new();
            w.put_bits(77, 8);
            w.put_bits(0, 8);
            w.put_bits(40, 8);
            w.put_ue(0);
            w.put_ue(0); // log2_max_frame_num_minus4
            w.put_ue(2); // poc type
            w.put_ue(1);
            w.put_flag(false);
            w.put_ue(119);
            w.put_ue(33);
            w.put_flag(false); // frame_mbs_only_flag = 0
            w.put_flag(false); // mb_adaptive
            w.put_flag(true);
            w.put_flag(false);
            w.put_flag(false);
            w.trailing_bits();
            let mut stream = Vec::new();
            write_nal(&mut stream, 3, NAL_SPS, w.bytes());
            let theirs = stream.clone();
            stream.extend_from_slice(&pps_nal(&main_pps()));
            stream.extend_from_slice(&[0, 0, 0, 1, 0x65, 1]);
            let mut out = Vec::new();
            let fixed = fix_headers(&stream, &ours, &main_pps(), &mut out);
            assert_eq!(fixed.sps_kept, 1);
            assert_eq!(fixed.sps_rewritten, 0);
            assert_eq!(&out[..theirs.len()], theirs);
        }

        #[test]
        fn a_stream_without_an_idr_passes_through() {
            let ours = sample_sps(Some(Vui::bt709_low_latency(60, 1)));
            let stream = [0, 0, 0, 1, 0x41, 0x9a, 1, 2];
            let mut out = vec![9, 9];
            let fixed = fix_headers(&stream, &ours, &main_pps(), &mut out);
            assert_eq!(fixed, Fixed::default());
            assert_eq!(out, stream);
        }

        #[test]
        fn levels_follow_the_picture_size_and_rate() {
            assert_eq!(level_for(1280, 720, 60), 32); // 3600 MBs × 60 = 216000
            assert_eq!(level_for(1920, 1080, 60), 42);
            assert_eq!(level_for(1920, 1080, 120), 51);
            assert_eq!(level_for(2560, 1440, 60), 51);
            assert_eq!(level_for(2560, 1440, 120), 52);
            assert_eq!(level_for(3840, 2160, 60), 52);
            assert_eq!(level_for(3840, 2160, 120), 60);
            assert_eq!(level_for(320, 240, 30), 30);
        }
    }
}

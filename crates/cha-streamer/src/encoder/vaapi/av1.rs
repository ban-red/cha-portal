//! AV1 (Profile 0, 8-bit 4:2:0) on VA-API: what the parameter buffers say, from
//! the stream's settings. Pure code (no libva calls), so what it chooses is
//! tested without a GPU. The counterpart of `h264.rs` and `hevc.rs`; the rate
//! control buffers (`Rate`, `misc_*`) and the packed header parameters are
//! `h264.rs`'s.
//!
//! The stream is built for the lowest latency a decoder can show, like the
//! others:
//! - one tile, one frame OBU per picture, every picture a reference, no
//!   B-frames, one reference (slot 0, the picture before), a key frame when
//!   asked and at the start, an infinite GOP;
//! - CBR with the same HRD buffer;
//! - the sequence header OBU and the frame header OBU are ours
//!   (`bitstream::av1`), which is what VA-API's AV1 wants: `va_enc_av1.h` has
//!   the *application* write both and tell the driver where its rate control
//!   edits go (`bit_offset_*`). A driver may rewrite them: Mesa's radeonsi
//!   reads our headers for the fields it needs and writes its own, from its
//!   own limits (see [`Padding`]). The temporal delimiter is ours too, as a
//!   packed raw OBU.
//!
//! The VA structs are `va_enc_av1.h`'s, from the libva API reference.

use anyhow::{Result, bail, ensure};

use super::ffi;
use super::h264::{Rate, zeroed};
use super::sys;
use crate::encoder::Params;
use crate::encoder::bitstream::av1::{
    self, Cdef, FILTER_SWITCHABLE, Frame, Header, PRIMARY_REF_NONE, Seq,
};

/// The GOP length told to the driver. We choose every picture's type, so it
/// never ends one; a rate controller wants a number.
pub const ADVERTISED_GOP: u32 = 3600;
/// `order_hint` takes this many bits, and wraps.
pub const ORDER_HINT_BITS: u32 = 7;
/// The slot every picture is predicted from and written to (a key frame
/// writes all eight).
const REFERENCE_SLOT: u8 = 0;
/// The `base_qindex` a picture starts from (the driver's rate control moves
/// it) and the one packed in the frame header before it does.
pub const START_QINDEX: u8 = 100;

const FEATURE_REQUIRED: u32 = 2;
const FEATURE_SUPPORTED: u32 = 1;

/// How the driver's coded picture differs from the one asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Padding {
    /// The driver codes the size we give.
    None,
    /// Mesa's radeonsi (VCN 4): the width goes up to a multiple of 64, the
    /// height to one of 16 (and a multiple of 8 that isn't of 16 gains two
    /// rows), and it writes *that* size in the sequence header. The picture
    /// is padded; the true size goes in the frame header's render size.
    Radeon,
}

impl Padding {
    /// From the driver's description (`vaQueryVendorString`).
    pub fn of_driver(vendor: &str) -> Self {
        let lower = vendor.to_ascii_lowercase();
        if lower.contains("radeonsi") || lower.contains("amd") {
            Padding::Radeon
        } else {
            Padding::None
        }
    }

    /// The size the driver codes for a picture of `width`×`height`.
    pub fn coded_size(self, width: u32, height: u32) -> (u32, u32) {
        match self {
            Padding::None => (width, height),
            Padding::Radeon => (
                width.next_multiple_of(64),
                if height.is_multiple_of(8) && !height.is_multiple_of(16) {
                    height + 2
                } else {
                    height.next_multiple_of(16)
                },
            ),
        }
    }
}

/// What the driver's AV1 attributes (`VAConfigAttribEncAV1` and `…Ext1`,
/// `…Ext2`) leave us to choose. Features the driver *requires* go in the
/// sequence header on; the rest stay off (a decoder needs none of them).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tools {
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
    pub restoration: bool,
    /// The driver takes palette mode (screen content).
    pub palette: bool,
    /// `interpolation_filter` to code (bitmask index of the types the driver
    /// takes: eight-tap, smooth, sharp, bilinear, switchable).
    pub interpolation_filter: u8,
    /// `TX_MODE_SELECT` (else `TX_MODE_LARGEST`).
    pub tx_select: bool,
    /// The bytes `obu_size` of the frame header is coded in, at least.
    pub obu_size_bytes: usize,
}

impl Tools {
    /// From the three attributes, as the driver gave them (`None`: it
    /// doesn't have the attribute).
    pub fn from_attributes(av1: Option<u32>, ext1: Option<u32>, ext2: Option<u32>) -> Self {
        // Two bits a feature, in the header's order.
        let level = |index: u32| av1.map_or(0, |v| (v >> (2 * index)) & 3);
        let required = |index| level(index) == FEATURE_REQUIRED;
        let interpolation_filter = match ext1.map(|v| v & 0x1f) {
            // Switchable when taken (or not said), else the first it has.
            None => FILTER_SWITCHABLE,
            Some(mask) if mask & 0x10 != 0 || mask == 0 => FILTER_SWITCHABLE,
            Some(mask) => mask.trailing_zeros() as u8,
        };
        // tx_mode_support: bit 2 is TX_MODE_SELECT (none said: assume it).
        let tx_select = ext2.is_none_or(|v| {
            let modes = (v >> 4) & 7;
            modes == 0 || modes & 4 != 0
        });
        Self {
            use_128x128: required(0),
            filter_intra: required(1),
            intra_edge_filter: required(2),
            interintra_compound: required(3),
            masked_compound: required(4),
            warped_motion: required(5),
            palette: level(6) >= FEATURE_SUPPORTED,
            dual_filter: required(7),
            jnt_comp: required(8),
            ref_frame_mvs: required(9),
            superres: required(10),
            restoration: required(11),
            interpolation_filter,
            tx_select,
            obu_size_bytes: ext2.map_or(4, |v| ((v >> 2) & 3) as usize + 1),
        }
    }
}

/// Settings for trying a driver (`CHA_VAAPI_AV1_TOOLS=a,b`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Knobs {
    /// `nosct`: screen content tools and palette mode off (they are on where
    /// the driver takes palette mode: this stream is mostly a desktop).
    pub no_screen_content: bool,
    /// `nocdf`: no CDF update (each frame starts from the defaults).
    pub no_cdf_update: bool,
    /// `noprimary`: no primary reference frame (CDFs aren't carried over).
    pub no_primary_ref: bool,
    /// `nocdef`: CDEF off in the sequence header.
    pub no_cdef: bool,
    /// `cdef0`: CDEF on but with zero strengths.
    pub zero_cdef: bool,
    /// `lf=n`: the loop filter level (default [`LOOP_FILTER_LEVEL`]).
    pub loop_filter: Option<u8>,
    /// `frameheader`: the frame header as a separate OBU and tile group
    /// OBU instead of one frame OBU.
    pub separate_header: bool,
}

impl Knobs {
    pub fn parse(list: &str) -> Self {
        let mut k = Self::default();
        for name in list.split(',').map(str::trim) {
            match name {
                "nosct" => k.no_screen_content = true,
                "nocdf" => k.no_cdf_update = true,
                "noprimary" => k.no_primary_ref = true,
                "nocdef" => k.no_cdef = true,
                "cdef0" => k.zero_cdef = true,
                "frameheader" => k.separate_header = true,
                other => {
                    if let Some(n) = other.strip_prefix("lf=").and_then(|n| n.parse::<u8>().ok()) {
                        k.loop_filter = Some(n.min(63));
                    }
                }
            }
        }
        k
    }
}

/// The deblocking level written in each frame header (luma, then chroma's
/// a little lower).
pub const LOOP_FILTER_LEVEL: u8 = 8;

/// The sequence and frame header inputs of a stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    pub tools: Tools,
    pub knobs: Knobs,
    pub seq: Seq,
    pub cdef: Cdef,
    /// How the driver pads the picture.
    pub padding: Padding,
    /// The size the driver codes (the size asked for, padded).
    pub coded: (u32, u32),
    /// The display size, where it isn't the coded one.
    pub render: Option<(u32, u32)>,
}

impl Layout {
    pub fn new(tools: Tools, knobs: Knobs, padding: Padding, params: &Params) -> Result<Self> {
        let (width, height) = (params.width, params.height);
        ensure!(
            (16..=65536).contains(&width)
                && (16..=65536).contains(&height)
                && width % 2 == 0
                && height % 2 == 0,
            "{width}×{height} isn't a size AV1 4:2:0 takes"
        );
        if tools.use_128x128 || tools.superres || tools.restoration {
            bail!(
                "the driver requires AV1 tools we don't write (128×128 superblocks {}, super resolution {}, loop restoration {})",
                tools.use_128x128,
                tools.superres,
                tools.restoration
            );
        }
        let coded = padding.coded_size(width, height);
        let (min_cols, min_rows) = av1::min_tiles_log2(coded.0, coded.1, false);
        ensure!(
            (min_cols, min_rows) == (0, 0),
            "a {}×{} AV1 picture needs more than one tile, which this encoder doesn't make",
            coded.0,
            coded.1
        );
        let mut seq = Seq::new(width, height);
        seq.filter_intra = tools.filter_intra;
        seq.intra_edge_filter = tools.intra_edge_filter;
        seq.interintra_compound = tools.interintra_compound;
        seq.masked_compound = tools.masked_compound;
        seq.warped_motion = tools.warped_motion;
        seq.dual_filter = tools.dual_filter;
        seq.jnt_comp = tools.jnt_comp;
        seq.ref_frame_mvs = tools.ref_frame_mvs;
        seq.order_hint_bits = ORDER_HINT_BITS;
        seq.cdef = !knobs.no_cdef;
        let cdef = if knobs.zero_cdef {
            Cdef::single(3, 0, 0)
        } else {
            // Mild: a primary strength of 4 and secondary 1 on luma, 1 on
            // chroma, damping 6.
            Cdef::single(3, 4 << 2 | 1, 1 << 2)
        };
        Ok(Self {
            tools,
            knobs,
            seq,
            cdef,
            padding,
            coded,
            render: (coded != (width, height)).then_some((width, height)),
        })
    }

    /// The size of the surfaces the encoder reads: the picture asked for,
    /// to the next 16 (the video processor fills the rest black).
    pub fn surface_size(&self) -> (u32, u32) {
        (
            self.seq.width.next_multiple_of(16),
            self.seq.height.next_multiple_of(16),
        )
    }

    /// The superblocks across and down in the coded picture.
    pub fn superblocks(&self) -> (u32, u32) {
        (self.coded.0.div_ceil(64), self.coded.1.div_ceil(64))
    }

    /// The same stream at another frame rate: nothing in the headers follows
    /// it (there is no timing info), so nothing changes.
    pub fn with_fps(&mut self, _params: &Params) {}

    fn loop_filter(&self) -> u8 {
        self.knobs.loop_filter.unwrap_or(LOOP_FILTER_LEVEL)
    }
}

/// One picture's place in the stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Picture {
    pub key: bool,
    /// `order_hint`: the pictures since the key frame, modulo 128.
    pub order_hint: u32,
}

/// Numbers the pictures: the order hint counts up from each key frame.
#[derive(Debug, Default)]
pub struct Gop {
    last: Option<Picture>,
}

impl Gop {
    /// The next picture; a non-key one follows the last (a key frame if
    /// there is no last).
    pub fn next(&mut self, key: bool) -> Picture {
        let picture = match self.last {
            Some(last) if !key => Picture {
                key: false,
                order_hint: (last.order_hint + 1) % (1 << ORDER_HINT_BITS),
            },
            _ => Picture {
                key: true,
                order_hint: 0,
            },
        };
        self.last = Some(picture);
        picture
    }

    /// Forget the pictures: the next one is a key frame.
    pub fn reset(&mut self) {
        self.last = None;
    }
}

/// The frame header inputs for `picture`.
pub fn frame(layout: &Layout, picture: &Picture, qindex: u8) -> Frame {
    let knobs = &layout.knobs;
    let level = layout.loop_filter();
    let chroma = level.saturating_sub(2);
    Frame {
        key: picture.key,
        order_hint: picture.order_hint,
        primary_ref_frame: if picture.key || knobs.no_primary_ref {
            PRIMARY_REF_NONE
        } else {
            0
        },
        // A key frame fills every slot; a P picture replaces the one it
        // came from.
        refresh_frame_flags: if picture.key {
            0xff
        } else {
            1 << REFERENCE_SLOT
        },
        ref_frame_idx: [REFERENCE_SLOT; 7],
        allow_screen_content_tools: layout.tools.palette && !knobs.no_screen_content,
        force_integer_mv: false,
        disable_cdf_update: knobs.no_cdf_update,
        disable_frame_end_update_cdf: knobs.no_cdf_update,
        base_qindex: qindex,
        filter_level: [level, level],
        filter_level_uv: [chroma, chroma],
        sharpness: 0,
        cdef: layout.cdef.clone(),
        tx_mode_select: layout.tools.tx_select,
        reduced_tx_set: false,
        allow_high_precision_mv: false,
        interpolation_filter: layout.tools.interpolation_filter,
        render: layout.render,
        frame_obu: !knobs.separate_header,
        size_bytes: layout.tools.obu_size_bytes,
    }
}

/// The packed frame header OBU for `picture`.
pub fn frame_header(layout: &Layout, picture: &Picture) -> Header {
    av1::frame_header_obu(&layout.seq, &frame(layout, picture, START_QINDEX))
}

/// The packed sequence header OBU.
pub fn sequence_header(layout: &Layout) -> Vec<u8> {
    av1::sequence_header_obu(&layout.seq)
}

// ---- VA parameter buffers (the structs are `sys.rs`'s, from libva's headers) ----

pub fn sequence_params(layout: &Layout, rate: &Rate) -> Box<sys::VAEncSequenceParameterBufferAV1> {
    let seq = &layout.seq;
    // SAFETY: integers, arrays and unions of them.
    let mut p = unsafe { zeroed::<sys::VAEncSequenceParameterBufferAV1>() };
    p.seq_profile = 0;
    p.seq_level_idx = seq.level_idx;
    p.seq_tier = seq.tier;
    p.hierarchical_flag = 0;
    p.intra_period = ADVERTISED_GOP;
    p.ip_period = 1; // no B-frames: a P every picture
    p.bits_per_second = rate.bits_per_second;
    p.order_hint_bits_minus_1 = (seq.order_hint_bits - 1) as u8;
    // SAFETY: a union of a u32 and bit-fields over it, zero-initialised.
    unsafe {
        let f = &mut p.seq_fields.bits;
        f.set_use_128x128_superblock(u32::from(seq.use_128x128));
        f.set_enable_filter_intra(u32::from(seq.filter_intra));
        f.set_enable_intra_edge_filter(u32::from(seq.intra_edge_filter));
        f.set_enable_interintra_compound(u32::from(seq.interintra_compound));
        f.set_enable_masked_compound(u32::from(seq.masked_compound));
        f.set_enable_warped_motion(u32::from(seq.warped_motion));
        f.set_enable_dual_filter(u32::from(seq.dual_filter));
        f.set_enable_order_hint(1);
        f.set_enable_jnt_comp(u32::from(seq.jnt_comp));
        f.set_enable_ref_frame_mvs(u32::from(seq.ref_frame_mvs));
        f.set_enable_superres(u32::from(seq.superres));
        f.set_enable_cdef(u32::from(seq.cdef));
        f.set_enable_restoration(u32::from(seq.restoration));
        f.set_bit_depth_minus8(0);
        f.set_subsampling_x(1);
        f.set_subsampling_y(1);
    }
    p
}

/// `current` is the surface the reconstruction goes to, `reference` the
/// previous picture's (for a P picture). `qindex` is `base_qindex`.
pub fn picture_params(
    layout: &Layout,
    picture: &Picture,
    current: u32,
    reference: Option<u32>,
    coded_buf: u32,
    qindex: u8,
) -> Box<sys::VAEncPictureParameterBufferAV1> {
    let header = frame_header(layout, picture);
    let frame = frame(layout, picture, qindex);
    // SAFETY: integers, arrays and unions of them.
    let mut p = unsafe { zeroed::<sys::VAEncPictureParameterBufferAV1>() };
    p.frame_width_minus_1 = (layout.seq.width - 1) as u16;
    p.frame_height_minus_1 = (layout.seq.height - 1) as u16;
    p.reconstructed_frame = current;
    p.coded_buf = coded_buf;
    p.reference_frames = [ffi::INVALID_SURFACE; 8];
    let reference = reference.filter(|_| !picture.key);
    if let Some(surface) = reference {
        p.reference_frames[usize::from(REFERENCE_SLOT)] = surface;
    }
    p.ref_frame_idx = frame.ref_frame_idx;
    p.primary_ref_frame = frame.primary_ref_frame;
    p.order_hint = picture.order_hint as u8;
    p.refresh_frame_flags = frame.refresh_frame_flags;
    // The first list entry (LAST_FRAME) is the one reference, as search
    // index 1 (0 is "none"); the second list has none.
    p.ref_frame_ctrl_l0.value = u32::from(reference.is_some());
    p.ref_frame_ctrl_l1.value = 0;
    // SAFETY: a union of a u32 and bit-fields over it, zero-initialised.
    unsafe {
        let f = &mut p.picture_flags.bits;
        f.set_frame_type(u32::from(if picture.key {
            av1::KEY_FRAME
        } else {
            av1::INTER_FRAME
        }));
        // A shown key frame's error_resilient_mode is 1 by definition.
        f.set_error_resilient_mode(u32::from(picture.key));
        f.set_disable_cdf_update(u32::from(frame.disable_cdf_update));
        f.set_disable_frame_end_update_cdf(u32::from(frame.disable_frame_end_update_cdf));
        f.set_enable_frame_obu(u32::from(frame.frame_obu));
        f.set_allow_screen_content_tools(u32::from(frame.allow_screen_content_tools));
        f.set_palette_mode_enable(u32::from(frame.allow_screen_content_tools));
    }
    p.filter_level = frame.filter_level;
    p.filter_level_u = frame.filter_level_uv[0];
    p.filter_level_v = frame.filter_level_uv[1];
    p.superres_scale_denominator = 8; // no scaling
    p.interpolation_filter = frame.interpolation_filter;
    p.ref_deltas = [1, 0, 0, 0, -1, 0, -1, -1];
    p.base_qindex = qindex;
    // SAFETY: as above.
    unsafe {
        let f = &mut p.mode_control_flags.bits;
        f.set_tx_mode(if frame.tx_mode_select { 2 } else { 1 });
    }
    let (sb_cols, sb_rows) = layout.superblocks();
    p.tile_cols = 1;
    p.tile_rows = 1;
    p.width_in_sbs_minus_1[0] = (sb_cols - 1) as u16;
    p.height_in_sbs_minus_1[0] = (sb_rows - 1) as u16;
    p.cdef_damping_minus_3 = frame.cdef.damping_minus_3;
    p.cdef_bits = frame.cdef.bits;
    p.cdef_y_strengths = frame.cdef.y;
    p.cdef_uv_strengths = frame.cdef.uv;
    let at = header.offsets;
    p.bit_offset_qindex = at.qindex_bit;
    p.bit_offset_segmentation = at.segmentation_bit;
    p.bit_offset_loopfilter_params = at.loop_filter_bit;
    p.bit_offset_cdef_params = at.cdef_bit;
    p.size_in_bits_cdef_params = at.cdef_bits;
    p.byte_offset_frame_hdr_obu_size = at.size_byte;
    p.size_in_bits_frame_hdr_obu = at.total_bits;
    // SAFETY: as above.
    unsafe {
        p.tile_group_obu_hdr_info.bits.set_obu_has_size_field(1);
    }
    p
}

/// The one tile group of a picture: tile 0 to tile 0.
pub fn tile_group_params() -> sys::VAEncTileGroupBufferAV1 {
    sys::VAEncTileGroupBufferAV1 {
        tg_start: 0,
        tg_end: 0,
        va_reserved: [0; 4],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cha_nvenc::Codec;

    fn params(width: u32, height: u32) -> Params {
        Params {
            codec: Codec::Av1,
            width,
            height,
            fps: 60,
            bitrate_bps: 40_000_000,
        }
    }

    fn tools() -> Tools {
        Tools::from_attributes(None, None, None)
    }

    fn layout(width: u32, height: u32, padding: Padding) -> Layout {
        Layout::new(tools(), Knobs::default(), padding, &params(width, height)).unwrap()
    }

    #[test]
    fn radeon_pads_and_says_the_true_size_as_the_render_size() {
        assert_eq!(Padding::Radeon.coded_size(1280, 720), (1280, 720));
        assert_eq!(Padding::Radeon.coded_size(1920, 1080), (1920, 1082));
        assert_eq!(Padding::Radeon.coded_size(2560, 1440), (2560, 1440));
        assert_eq!(Padding::Radeon.coded_size(1366, 768), (1408, 768));
        assert_eq!(Padding::None.coded_size(1920, 1080), (1920, 1080));
        let l = layout(1920, 1080, Padding::Radeon);
        assert_eq!(l.coded, (1920, 1082));
        assert_eq!(l.render, Some((1920, 1080)));
        assert_eq!(l.surface_size(), (1920, 1088));
        assert_eq!(l.superblocks(), (30, 17));
        // Sizes that need no padding have no render size.
        assert_eq!(layout(1280, 720, Padding::Radeon).render, None);
        assert_eq!(layout(1920, 1080, Padding::None).render, None);
    }

    #[test]
    fn the_driver_decides_the_padding() {
        let radeon = "Mesa Gallium driver 26.0.8 for AMD Radeon 780M Graphics (radeonsi, phoenix)";
        assert_eq!(Padding::of_driver(radeon), Padding::Radeon);
        let intel = "Intel iHD driver for Intel(R) Gen Graphics - 25.2.3 ()";
        assert_eq!(Padding::of_driver(intel), Padding::None);
    }

    #[test]
    fn sizes_are_checked() {
        let new = |w, h| Layout::new(tools(), Knobs::default(), Padding::None, &params(w, h));
        assert!(new(1280, 720).is_ok());
        assert!(new(1281, 720).is_err());
        assert!(new(8, 8).is_err());
        // Wider than 4096 needs tiles.
        assert!(new(4098, 1080).is_err());
        assert!(new(4096, 2160).is_ok());
    }

    #[test]
    fn what_the_driver_requires_is_written_on() {
        let none = Tools::from_attributes(None, None, None);
        assert!(!none.filter_intra && !none.palette && !none.warped_motion);
        assert_eq!(none.interpolation_filter, FILTER_SWITCHABLE);
        assert!(none.tx_select);
        assert_eq!(none.obu_size_bytes, 4);
        // Every feature supported (1), none required: all off.
        let supported = 0x0555_5555 & 0x0fff_ffff;
        let t = Tools::from_attributes(Some(supported), None, None);
        assert!(!t.filter_intra && !t.dual_filter && !t.ref_frame_mvs);
        assert!(t.palette);
        // Filter intra (1), dual filter (7) and ref frame mvs (9) required.
        let value = (2 << 2) | (2 << 14) | (2 << 18);
        let t = Tools::from_attributes(Some(value), None, None);
        assert!(t.filter_intra && t.dual_filter && t.ref_frame_mvs);
        assert!(!t.intra_edge_filter && !t.warped_motion);
        // Filters: eight-tap and smooth only: the first.
        assert_eq!(
            Tools::from_attributes(None, Some(3), None).interpolation_filter,
            0
        );
        assert_eq!(
            Tools::from_attributes(None, Some(0x10 | 3), None).interpolation_filter,
            4
        );
        assert_eq!(
            Tools::from_attributes(None, Some(0x08), None).interpolation_filter,
            3
        );
        // tx modes: select is bit 2 of three; the size field is 3 + 1 bytes.
        let ext2 = (3 << 2) | (0x6 << 4);
        let t = Tools::from_attributes(None, None, Some(ext2));
        assert!(t.tx_select);
        assert_eq!(t.obu_size_bytes, 4);
        let ext2 = 0x2 << 4;
        assert!(!Tools::from_attributes(None, None, Some(ext2)).tx_select);
        // A tool we don't write, required: refused.
        let l = Layout::new(
            Tools::from_attributes(Some(2 << 20), None, None),
            Knobs::default(),
            Padding::None,
            &params(1280, 720),
        );
        assert!(l.is_err());
    }

    #[test]
    fn pictures_count_up_from_each_key_frame() {
        let mut gop = Gop::default();
        assert_eq!(
            gop.next(false),
            Picture {
                key: true,
                order_hint: 0
            }
        );
        assert_eq!(gop.next(false).order_hint, 1);
        assert_eq!(gop.next(false).order_hint, 2);
        assert!(gop.next(true).key);
        assert_eq!(gop.next(false).order_hint, 1);
        gop.reset();
        assert!(gop.next(false).key);
        // The order hint wraps at 128.
        let mut gop = Gop::default();
        gop.next(false);
        let hints: Vec<u32> = (0..130).map(|_| gop.next(false).order_hint).collect();
        assert_eq!(hints[126], 127);
        assert_eq!(hints[127], 0);
        assert!(!hints.iter().any(|&h| h > 127));
    }

    #[test]
    fn the_stream_is_low_latency_by_construction() {
        let l = layout(1280, 720, Padding::Radeon);
        let rate = Rate::with_buffer(40_000_000, 60, 4);
        let seq = sequence_params(&l, &rate);
        assert_eq!(
            (seq.seq_profile, seq.seq_level_idx, seq.seq_tier),
            (0, 13, 0)
        );
        assert_eq!(seq.ip_period, 1);
        assert_eq!(seq.order_hint_bits_minus_1, 6);
        // SAFETY: a plain integer union.
        unsafe {
            assert_eq!(seq.seq_fields.bits.enable_order_hint(), 1);
            assert_eq!(seq.seq_fields.bits.subsampling_x(), 1);
            assert_eq!(seq.seq_fields.bits.bit_depth_minus8(), 0);
            assert_eq!(seq.seq_fields.bits.enable_cdef(), 1);
            assert_eq!(seq.seq_fields.bits.use_128x128_superblock(), 0);
            assert_eq!(seq.seq_fields.bits.enable_restoration(), 0);
        }
        // The packed sequence header says the same.
        let packed = sequence_header(&l);
        let obus = av1::parse(&packed).unwrap();
        let info = av1::parse_sequence(obus[0].payload).unwrap();
        assert_eq!((info.width, info.height, info.level_idx), (1280, 720, 13));
    }

    #[test]
    fn a_p_picture_refers_to_slot_0_and_a_key_frame_to_none() {
        let l = layout(1280, 720, Padding::Radeon);
        let key = Picture {
            key: true,
            order_hint: 0,
        };
        let p = Picture {
            key: false,
            order_hint: 1,
        };
        let pic = picture_params(&l, &p, 7, Some(6), 9, 100);
        assert_eq!((pic.reconstructed_frame, pic.coded_buf), (7, 9));
        assert_eq!(pic.reference_frames[0], 6);
        assert_eq!(pic.reference_frames[1], ffi::INVALID_SURFACE);
        assert_eq!(pic.ref_frame_idx, [0; 7]);
        assert_eq!(pic.refresh_frame_flags, 1);
        assert_eq!(pic.primary_ref_frame, 0);
        assert_eq!(pic.order_hint, 1);
        // SAFETY: plain integer unions.
        unsafe {
            assert_eq!(pic.ref_frame_ctrl_l0.value, 1);
            assert_eq!(pic.picture_flags.bits.frame_type(), 1);
            assert_eq!(pic.picture_flags.bits.error_resilient_mode(), 0);
            assert_eq!(pic.picture_flags.bits.enable_frame_obu(), 1);
            assert_eq!(pic.mode_control_flags.bits.tx_mode(), 2);
        }
        assert_eq!((pic.tile_cols, pic.tile_rows), (1, 1));
        assert_eq!(
            (pic.width_in_sbs_minus_1[0], pic.height_in_sbs_minus_1[0]),
            (19, 11)
        );
        let pic = picture_params(&l, &key, 6, Some(7), 9, 100);
        assert_eq!(pic.reference_frames, [ffi::INVALID_SURFACE; 8]);
        assert_eq!(pic.refresh_frame_flags, 0xff);
        assert_eq!(pic.primary_ref_frame, PRIMARY_REF_NONE);
        // SAFETY: plain integer unions.
        unsafe {
            assert_eq!(pic.ref_frame_ctrl_l0.value, 0);
            assert_eq!(pic.picture_flags.bits.frame_type(), 0);
            assert_eq!(pic.picture_flags.bits.error_resilient_mode(), 1);
        }
    }

    #[test]
    fn the_offsets_given_to_the_driver_match_the_packed_header() {
        let l = layout(1280, 720, Padding::None);
        let p = Picture {
            key: false,
            order_hint: 3,
        };
        let header = frame_header(&l, &p);
        let pic = picture_params(&l, &p, 1, Some(2), 3, START_QINDEX);
        let at = header.offsets;
        assert_eq!(pic.bit_offset_qindex, at.qindex_bit);
        assert_eq!(pic.bit_offset_loopfilter_params, at.loop_filter_bit);
        assert_eq!(pic.bit_offset_cdef_params, at.cdef_bit);
        assert_eq!(pic.size_in_bits_cdef_params, 16);
        assert_eq!(pic.byte_offset_frame_hdr_obu_size, 1);
        assert_eq!(pic.size_in_bits_frame_hdr_obu, at.total_bits);
        assert_eq!(u32::from(header.bytes[0]) >> 3 & 15, 6, "an OBU_FRAME");
        // The qindex is where the offset says.
        let at_bit = at.qindex_bit as usize;
        let value = (0..8).fold(0u32, |acc, i| {
            let bit = at_bit + i;
            acc << 1 | u32::from(header.bytes[bit / 8] >> (7 - bit % 8) & 1)
        });
        assert_eq!(value, u32::from(START_QINDEX));
    }

    #[test]
    fn the_knobs_change_the_stream() {
        let k = Knobs::parse("nosct, nocdf,noprimary,nocdef,lf=20,frameheader,bogus");
        assert!(k.no_screen_content && k.no_cdf_update && k.no_primary_ref && k.no_cdef);
        assert!(k.separate_header);
        assert_eq!(k.loop_filter, Some(20));
        assert_eq!(Knobs::parse("lf=99").loop_filter, Some(63));
        // Palette mode taken (index 6 of the features): screen content tools
        // are on, unless `nosct`.
        let palette = Tools::from_attributes(Some(1 << 12), None, None);
        let l = Layout::new(palette, Knobs::default(), Padding::None, &params(1280, 720)).unwrap();
        let p = Picture {
            key: false,
            order_hint: 1,
        };
        assert!(frame(&l, &p, 100).allow_screen_content_tools);
        let pic = picture_params(&l, &p, 1, Some(2), 3, 100);
        // SAFETY: plain integer unions.
        unsafe {
            assert_eq!(pic.picture_flags.bits.palette_mode_enable(), 1);
            assert_eq!(pic.picture_flags.bits.allow_screen_content_tools(), 1);
        }
        let l = Layout::new(palette, k, Padding::None, &params(1280, 720)).unwrap();
        assert!(!l.seq.cdef);
        let f = frame(&l, &p, 100);
        assert_eq!(f.primary_ref_frame, PRIMARY_REF_NONE);
        assert!(!f.allow_screen_content_tools && f.disable_cdf_update && !f.frame_obu);
        assert_eq!(f.filter_level, [20, 20]);
        let pic = picture_params(&l, &p, 1, Some(2), 3, 100);
        // SAFETY: a plain integer union.
        unsafe {
            assert_eq!(pic.picture_flags.bits.palette_mode_enable(), 0);
        }
        assert!(Knobs::parse("cdef0").zero_cdef);
    }
}

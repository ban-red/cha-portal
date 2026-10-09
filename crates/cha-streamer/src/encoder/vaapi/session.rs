//! The VA-API encoder, H.264, HEVC or AV1: one stream on one render node.
//!
//! Per frame, all on the GPU:
//! 1. the compositor's dmabuf is imported as an RGB VA surface (no copy;
//!    cached per output buffer);
//! 2. the video processor converts it to NV12 (BT.709, limited range);
//! 3. the encoder codes that as one slice, an IDR or a P picture referring
//!    to the picture before it, in CBR with a multi-frame HRD buffer (AV1: one
//!    tile, a key frame or an inter frame referring to the frame before);
//! 4. the coded buffer is read back (Annex-B), and an IDR gets our headers
//!    (SPS and PPS, `bitstream::h264::fix_headers`; VPS, SPS and PPS,
//!    `bitstream::h265::fix_headers`); AV1's is shaped into a temporal unit
//!    (`bitstream::av1::shape_temporal_unit`: our temporal delimiter, the
//!    sequence header on a key frame).
//!
//! Settings the node can change without restarting the stream (bitrate, frame
//! rate) go in as rate control buffers on the next frame, with the `reset`
//! flag; a size change makes new contexts and surfaces and starts with an IDR.
//!
//! Environment knobs for trying drivers: `CHA_VAAPI_ENTRYPOINT=slice|lp`
//! picks `EncSlice` or `EncSliceLP`; `CHA_VAAPI_PACKED_HEADERS=off|slice`
//! turns the packed SPS/PPS off (the driver writes its own, which we then
//! rewrite) or adds the packed slice header; `CHA_VAAPI_PACKED_EMULATION=driver`
//! hands the packed headers over without emulation prevention bytes.
//! `CHA_VAAPI_PACKED_HEADERS=headers` packs the parameter sets and leaves the
//! slice header to the driver, where the default packs it too (HEVC, and H.264
//! on Mesa); HEVC also
//! reads `CHA_VAAPI_HEVC_TOOLS=sao,amp,sdh,tskip,nocuqp,nottmvp,nossm`, coding
//! tools to turn on (or, `no…`, off) against the defaults. AV1 reads
//! `CHA_VAAPI_AV1_TOOLS=nosct,nocdf,noprimary,nocdef,cdef0,lf=<n>,frameheader`
//! (see `av1::Knobs`).

use std::collections::HashMap;
use std::ffi::c_int;
use std::path::Path;
use std::time::Instant;

use anyhow::{Context, Result, bail, ensure};
use cha_nvenc::{Codec, Timings};
use smithay::backend::allocator::Buffer;
use smithay::backend::allocator::dmabuf::Dmabuf;
use tracing::{info, warn};

use super::av1::{self, Knobs as Av1Knobs, Padding, Tools as Av1Tools};
use super::ffi::{self, Display, Id, Rectangle};
use super::h264::{
    self, Profile, Rate, misc_frame_rate, misc_hrd, misc_rate_control, packed_header_params,
};
use super::hevc::{self, Blocks, Tools};
use crate::encoder::bitstream::{av1 as bits_av1, h264 as bits, h265 as bits265};
use crate::encoder::{Params, VideoEncoder};
use crate::media::Frame;

/// The HRD buffer and rate window in frames. One frame starved iHD: a
/// 1280x720 IDR got about 41 KB at 20 Mbit/s and came out at 27 dB with
/// whole macroblock rows flat, P frames trailing the motion (23 dB). Measured
/// on a UHD 630 (iHD 26.1.2): 2 frames 39/31 dB, 4 frames 45-56 dB with a
/// 114 KB IDR (under 3 frame times to send), 8 frames 50-65 dB but 260 KB
/// IDRs. IDRs are rare (on request), P frames small.
const DEFAULT_HRD_FRAMES: u32 = 4;
/// HEVC's default: iHD's HEVC rate control gives an IDR a smaller share of
/// the same buffer than its H.264 one (1280x720 at 20 Mbit/s: 52 KB at 34 dB
/// with 4 frames, against 114 KB at 39 dB for H.264). 10 frames gives 200 KB
/// (the IDR 1-2 dB under the P pictures, the average on par with H.264's).
/// Measured on a UHD 630 (iHD 25.2.3).
const DEFAULT_HRD_FRAMES_HEVC: u32 = 10;

/// Settings for trying a driver, from the environment (read when an encoder
/// starts, so they apply to a self-test run and to a streamer alike; they
/// have to reach the process, which a container needs `-e` for).
#[derive(Clone, Debug, Default)]
struct Tuning {
    /// `CHA_VAAPI_HRD_FRAMES=n`: the HRD buffer holds n frames
    /// ([`DEFAULT_HRD_FRAMES`], or [`DEFAULT_HRD_FRAMES_HEVC`], unless set).
    hrd_frames: u32,
    /// `CHA_VAAPI_CQP=qp`: constant quantiser instead of CBR, as a diagnostic:
    /// if the picture is right at a low QP, the pipeline (conversion,
    /// references) is, and the rate control settings are what's off.
    cqp: Option<u32>,
    /// `CHA_VAAPI_DUMP_NV12=path`: write the first frame's NV12 input (after
    /// the video processor) there, packed, with no padding.
    dump_nv12: Option<std::path::PathBuf>,
}

impl Tuning {
    fn from_env(codec: Codec) -> Self {
        let number = |name: &str| std::env::var(name).ok().and_then(|v| v.parse::<u32>().ok());
        Self {
            hrd_frames: number("CHA_VAAPI_HRD_FRAMES")
                .unwrap_or(if codec == Codec::Hevc {
                    DEFAULT_HRD_FRAMES_HEVC
                } else {
                    DEFAULT_HRD_FRAMES
                })
                .clamp(1, 240),
            cqp: number("CHA_VAAPI_CQP").map(|qp| qp.clamp(1, 51)),
            dump_nv12: std::env::var_os("CHA_VAAPI_DUMP_NV12").map(Into::into),
        }
    }

    fn rate_mode(&self) -> u32 {
        if self.cqp.is_some() {
            ffi::RC_CQP
        } else {
            ffi::RC_CBR
        }
    }
}

/// Which headers we pack ourselves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Packed {
    /// The driver writes its own (we rewrite its SPS).
    None,
    /// SPS and PPS are ours.
    Headers,
    /// And the slice header too (HEVC's default, and H.264's on Mesa).
    HeadersAndSlice,
}

impl Packed {
    fn attribute(self) -> Option<u32> {
        match self {
            Packed::None => None,
            Packed::Headers => Some(ffi::PACKED_HEADER_SEQUENCE | ffi::PACKED_HEADER_PICTURE),
            Packed::HeadersAndSlice => Some(
                ffi::PACKED_HEADER_SEQUENCE | ffi::PACKED_HEADER_PICTURE | ffi::PACKED_HEADER_SLICE,
            ),
        }
    }
}

/// What the driver offers that we picked, fixed for the stream.
struct Setup {
    entrypoint: c_int,
    packed: Packed,
    /// AV1: the temporal delimiter goes in as a packed raw OBU.
    raw_data: bool,
    enc_config: Id,
    vpp_config: Id,
}

impl Setup {
    fn low_power(&self) -> bool {
        self.entrypoint == ffi::ENTRYPOINT_ENC_SLICE_LP
    }
}

/// A function that takes a NAL unit's emulation prevention bytes out.
type Unescape = fn(&[u8]) -> Vec<u8>;

/// What was picked for the codec, and what a layout is made from.
#[derive(Clone, Copy, Debug)]
enum Picked {
    H264(Profile),
    /// `VAConfigAttribEncHEVCFeatures` and `…BlockSizes`, if the driver
    /// has them.
    Hevc {
        features: Option<u32>,
        block_sizes: Option<u32>,
    },
    /// `VAConfigAttribEncAV1`, `...Ext1` and `...Ext2`, if the driver has
    /// them.
    Av1 {
        features: Option<u32>,
        ext1: Option<u32>,
        ext2: Option<u32>,
    },
}

/// The profile and entrypoint chosen.
struct Choice {
    picked: Picked,
    va_profile: c_int,
    name: &'static str,
    entrypoint: c_int,
    /// `VAConfigAttribEncPackedHeaders`.
    packed: u32,
}

/// The best profile and entrypoint with CBR: for H.264 High before Main
/// before Constrained Baseline, for HEVC Main; the low-power entrypoint
/// before the other; for AV1 Profile 0.
fn choose(display: &Display, codec: Codec, rate_mode: u32) -> Result<Choice> {
    let forced = std::env::var("CHA_VAAPI_ENTRYPOINT").ok();
    let entrypoints: &[c_int] = match forced.as_deref() {
        Some("slice") => &[ffi::ENTRYPOINT_ENC_SLICE],
        Some("lp") => &[ffi::ENTRYPOINT_ENC_SLICE_LP],
        Some(other) => {
            warn!("CHA_VAAPI_ENTRYPOINT={other} isn't slice or lp; using the default");
            &[ffi::ENTRYPOINT_ENC_SLICE_LP, ffi::ENTRYPOINT_ENC_SLICE]
        }
        None => &[ffi::ENTRYPOINT_ENC_SLICE_LP, ffi::ENTRYPOINT_ENC_SLICE],
    };
    let candidates: Vec<(c_int, &'static str, Option<Profile>)> = match codec {
        Codec::H264 => Profile::PREFERENCE
            .iter()
            .map(|p| (p.va, p.name, Some(*p)))
            .collect(),
        Codec::Hevc => vec![(ffi::PROFILE_HEVC_MAIN, "Main", None)],
        Codec::Av1 => vec![(ffi::PROFILE_AV1_PROFILE0, "Profile0", None)],
    };
    let mut seen = Vec::new();
    for (va_profile, name, profile) in candidates {
        let have = display.entrypoints(va_profile);
        for &entrypoint in entrypoints {
            if !have.contains(&entrypoint) {
                continue;
            }
            let values = display.attributes(
                va_profile,
                entrypoint,
                &[
                    ffi::ATTRIB_RATE_CONTROL,
                    ffi::ATTRIB_ENC_PACKED_HEADERS,
                    ffi::ATTRIB_ENC_HEVC_FEATURES,
                    ffi::ATTRIB_ENC_HEVC_BLOCK_SIZES,
                    ffi::ATTRIB_ENC_AV1,
                    ffi::ATTRIB_ENC_AV1_EXT1,
                    ffi::ATTRIB_ENC_AV1_EXT2,
                ],
            );
            let (rate_control, packed) = (values[0], values[1]);
            if rate_control != ffi::ATTRIB_NOT_SUPPORTED && rate_control & rate_mode != 0 {
                let packed = if packed == ffi::ATTRIB_NOT_SUPPORTED {
                    0
                } else {
                    packed
                };
                let said = |value: u32| (value != ffi::ATTRIB_NOT_SUPPORTED).then_some(value);
                let picked = match (profile, codec) {
                    (Some(profile), _) => Picked::H264(profile),
                    (None, Codec::Av1) => Picked::Av1 {
                        features: said(values[4]),
                        ext1: said(values[5]),
                        ext2: said(values[6]),
                    },
                    (None, _) => Picked::Hevc {
                        features: said(values[2]),
                        block_sizes: said(values[3]),
                    },
                };
                return Ok(Choice {
                    picked,
                    va_profile,
                    name,
                    entrypoint,
                    packed,
                });
            }
            seen.push(format!(
                "{name} entrypoint {entrypoint}: rate control {rate_control:#x}"
            ));
        }
    }
    bail!(
        "the driver has no {} encode entrypoint with CBR ({})",
        codec.name(),
        if seen.is_empty() {
            match codec {
                Codec::Hevc => "Main doesn't encode".to_string(),
                Codec::Av1 => "Profile 0 doesn't encode".to_string(),
                _ => "none of High, Main or Constrained Baseline encodes".to_string(),
            }
        } else {
            seen.join("; ")
        }
    )
}

/// The picture numbering of a stream, in its codec's terms.
#[derive(Clone, Copy, Debug)]
enum Pic {
    H264(h264::Picture),
    Hevc(hevc::Picture),
    Av1(av1::Picture),
}

impl Pic {
    fn idr(&self) -> bool {
        match self {
            Pic::H264(p) => p.idr,
            Pic::Hevc(p) => p.idr,
            Pic::Av1(p) => p.key,
        }
    }
}

/// A stream's layout and picture numbering: what differs between the codecs.
enum Stream {
    H264 {
        layout: h264::Layout,
        gop: h264::Gop,
    },
    Hevc {
        layout: hevc::Layout,
        gop: hevc::Gop,
    },
    Av1 {
        layout: av1::Layout,
        gop: av1::Gop,
    },
}

impl Stream {
    /// The stream's layout; with constant quantiser the PPS carries that QP.
    /// `padding` is how the driver pads an AV1 picture.
    fn new(picked: &Picked, params: &Params, tuning: &Tuning, padding: Padding) -> Result<Self> {
        Ok(match *picked {
            Picked::H264(profile) => {
                let mut layout = h264::Layout::new(profile, params)?;
                if let Some(qp) = tuning.cqp {
                    layout.pps.pic_init_qp = qp as i32;
                }
                Stream::H264 {
                    layout,
                    gop: h264::Gop::default(),
                }
            }
            Picked::Hevc {
                features,
                block_sizes,
            } => {
                let mut tools = Tools::from_attribute(features);
                if let Ok(list) = std::env::var("CHA_VAAPI_HEVC_TOOLS") {
                    for name in list.split(',') {
                        match name {
                            "nocuqp" => tools.cu_qp_delta = false,
                            "sao" => tools.sao = true,
                            "amp" => tools.amp = true,
                            "sdh" => tools.sign_data_hiding = true,
                            "tskip" => tools.transform_skip = true,
                            "nottmvp" => tools.temporal_mvp = false,
                            "nossm" => tools.strong_intra_smoothing = false,
                            _ => {}
                        }
                    }
                }
                let mut layout =
                    hevc::Layout::new(Blocks::from_attribute(block_sizes), tools, params)?;
                if let Some(qp) = tuning.cqp {
                    layout.pps.init_qp = qp as i32;
                }
                Stream::Hevc {
                    layout,
                    gop: hevc::Gop::default(),
                }
            }
            Picked::Av1 {
                features,
                ext1,
                ext2,
            } => {
                let tools = Av1Tools::from_attributes(features, ext1, ext2);
                let knobs = std::env::var("CHA_VAAPI_AV1_TOOLS")
                    .map(|list| Av1Knobs::parse(&list))
                    .unwrap_or_default();
                Stream::Av1 {
                    layout: av1::Layout::new(tools, knobs, padding, params)?,
                    gop: av1::Gop::default(),
                }
            }
        })
    }

    /// The same stream at another size, starting over.
    fn resized(&self, params: &Params, tuning: &Tuning) -> Result<Self> {
        match self {
            Stream::H264 { layout, .. } => {
                Self::new(&Picked::H264(layout.profile), params, tuning, Padding::None)
            }
            Stream::Av1 { layout, .. } => Ok(Stream::Av1 {
                // The tools and knobs were fitted to the driver already.
                layout: av1::Layout::new(layout.tools, layout.knobs, layout.padding, params)?,
                gop: av1::Gop::default(),
            }),
            Stream::Hevc { layout, .. } => {
                // The block sizes and tools were fitted to the driver already.
                let mut new = hevc::Layout::new(layout.blocks, layout.tools, params)?;
                new.pps = layout.pps.clone();
                Ok(Stream::Hevc {
                    layout: new,
                    gop: hevc::Gop::default(),
                })
            }
        }
    }

    fn coded_size(&self) -> (u32, u32) {
        match self {
            Stream::H264 { layout, .. } => layout.coded_size(),
            Stream::Hevc { layout, .. } => layout.coded_size(),
            Stream::Av1 { layout, .. } => layout.surface_size(),
        }
    }

    fn name(&self) -> &'static str {
        match self {
            Stream::H264 { layout, .. } => layout.profile.name,
            Stream::Hevc { .. } => "Main",
            Stream::Av1 { .. } => "Profile0",
        }
    }

    fn level(&self) -> u32 {
        match self {
            Stream::H264 { layout, .. } => u32::from(layout.sps.level_idc),
            Stream::Hevc { layout, .. } => u32::from(layout.sps.level_idc),
            Stream::Av1 { layout, .. } => u32::from(layout.seq.level_idx),
        }
    }

    fn with_fps(&mut self, params: &Params) {
        match self {
            Stream::H264 { layout, .. } => layout.with_fps(params),
            Stream::Hevc { layout, .. } => layout.with_fps(params),
            Stream::Av1 { layout, .. } => layout.with_fps(params),
        }
    }

    fn next(&mut self, idr: bool) -> Pic {
        match self {
            Stream::H264 { gop, .. } => Pic::H264(gop.next(idr)),
            Stream::Hevc { gop, .. } => Pic::Hevc(gop.next(idr)),
            Stream::Av1 { gop, .. } => Pic::Av1(gop.next(idr)),
        }
    }

    fn reset(&mut self) {
        match self {
            Stream::H264 { gop, .. } => gop.reset(),
            Stream::Hevc { gop, .. } => gop.reset(),
            Stream::Av1 { gop, .. } => gop.reset(),
        }
    }

    fn has_idr(&self, coded: &[u8]) -> bool {
        match self {
            Stream::H264 { .. } => bits::has_idr(coded),
            Stream::Hevc { .. } => bits265::has_idr(coded),
            Stream::Av1 { .. } => bits_av1::has_key_frame(coded).unwrap_or(false),
        }
    }

    /// Puts our headers in front of an IDR. Returns whether the driver's SPS
    /// was left as it was (H.264: it uses what we don't parse).
    fn fix_headers(&self, coded: &[u8], out: &mut Vec<u8>) -> bool {
        match self {
            Stream::H264 { layout, .. } => {
                bits::fix_headers(coded, &layout.sps, &layout.pps, out).sps_kept > 0
            }
            Stream::Hevc { layout, .. } => {
                bits265::fix_headers(coded, &layout.sps, &layout.pps, out);
                false
            }
            // Shaped by `Vaapi::encode_with`, which has the Result.
            Stream::Av1 { .. } => {
                out.clear();
                out.extend_from_slice(coded);
                false
            }
        }
    }
}

/// Which headers to pack, from what the driver offers. HEVC packs the slice
/// header too unless told not to: its POC bits depend on our SPS. So does
/// H.264 on Mesa (radeonsi), which otherwise ignores `idr_pic_flag` after the
/// first picture and makes a P where we asked for an IDR (Radeon 780M, Mesa
/// 26.0).
fn packed_mode(offered: u32, setting: Option<&str>, codec: Codec, mesa: bool) -> Packed {
    let both = ffi::PACKED_HEADER_SEQUENCE | ffi::PACKED_HEADER_PICTURE;
    if codec == Codec::Av1 {
        // The sequence and frame header OBUs, always (the driver can't write
        // them without); AV1 has no slice header.
        return if offered & both == both {
            Packed::Headers
        } else {
            Packed::None
        };
    }
    let slice = offered & both == both && offered & ffi::PACKED_HEADER_SLICE != 0;
    match setting {
        Some("off") => Packed::None,
        Some("slice") if slice => Packed::HeadersAndSlice,
        Some("headers") if offered & both == both => Packed::Headers,
        None if (codec == Codec::Hevc || mesa) && slice => Packed::HeadersAndSlice,
        _ if offered & both == both => Packed::Headers,
        _ => Packed::None,
    }
}

/// What depends on the picture size: surfaces, contexts, the coded buffer.
struct Resources {
    /// The NV12 picture the encoder reads, and the video processor writes.
    input: Id,
    /// The reconstructed pictures, taking turns as the reference.
    recon: [Id; 2],
    coded: Id,
    enc_context: Id,
    vpp_context: Id,
}

impl Resources {
    fn create(display: &Display, setup: &Setup, coded: (u32, u32)) -> Result<Self> {
        let mut res = Self {
            input: ffi::INVALID_ID,
            recon: [ffi::INVALID_ID; 2],
            coded: ffi::INVALID_ID,
            enc_context: ffi::INVALID_ID,
            vpp_context: ffi::INVALID_ID,
        };
        match res.fill(display, setup, coded) {
            Ok(()) => Ok(res),
            Err(err) => {
                res.release(display);
                Err(err)
            }
        }
    }

    fn fill(&mut self, display: &Display, setup: &Setup, coded: (u32, u32)) -> Result<()> {
        let (width, height) = coded;
        let surfaces = display.create_nv12_surfaces(width, height, 3)?;
        self.input = surfaces[0];
        self.recon = [surfaces[1], surfaces[2]];
        self.enc_context = display.create_context(setup.enc_config, width, height, &surfaces)?;
        self.vpp_context =
            display.create_context(setup.vpp_config, width, height, &[self.input])?;
        // Room for an IDR at any quality: raw 4:2:0 size.
        let coded_bytes = (width as usize * height as usize * 3 / 2).max(1 << 16);
        // SAFETY: a null pointer asks for uninitialised space.
        self.coded = unsafe {
            display.create_buffer_raw(
                self.enc_context,
                ffi::BUFFER_ENC_CODED,
                coded_bytes,
                std::ptr::null_mut(),
            )
        }?;
        Ok(())
    }

    fn release(&self, display: &Display) {
        if self.coded != ffi::INVALID_ID {
            display.destroy_buffer(self.coded);
        }
        for context in [self.enc_context, self.vpp_context] {
            if context != ffi::INVALID_ID {
                display.destroy_context(context);
            }
        }
        let surfaces: Vec<Id> = [self.input, self.recon[0], self.recon[1]]
            .into_iter()
            .filter(|&s| s != ffi::INVALID_ID)
            .collect();
        display.destroy_surfaces(&surfaces);
    }
}

/// Buffers made for one picture, destroyed when it's done (or failed).
struct Buffers<'a> {
    display: &'a Display,
    ids: Vec<Id>,
}

impl<'a> Buffers<'a> {
    fn new(display: &'a Display) -> Self {
        Self {
            display,
            ids: Vec::new(),
        }
    }

    fn add<T>(&mut self, context: Id, kind: c_int, value: &T) -> Result<()> {
        self.ids
            .push(self.display.create_buffer(context, kind, value)?);
        Ok(())
    }

    fn add_bytes(&mut self, context: Id, kind: c_int, bytes: &[u8]) -> Result<()> {
        self.ids
            .push(self.display.create_buffer_bytes(context, kind, bytes)?);
        Ok(())
    }

    /// A packed header: its parameters, then its bytes. Our NALs have
    /// emulation prevention bytes in, and say so; with `driver_escapes` (the
    /// codec's function for it) the bytes are taken out again and the driver
    /// is asked to put them back
    /// (`CHA_VAAPI_PACKED_EMULATION=driver`, in case a driver ignores the flag).
    fn add_packed(
        &mut self,
        context: Id,
        kind: u32,
        nal: &[u8],
        driver_escapes: Option<Unescape>,
    ) -> Result<()> {
        let raw;
        let (bytes, bit_length): (&[u8], u32) = if let Some(unescaped) = driver_escapes {
            raw = unescaped(nal);
            (&raw, raw.len() as u32 * 8)
        } else {
            (nal, nal.len() as u32 * 8)
        };
        self.add_packed_bits(context, kind, bytes, bit_length, driver_escapes.is_none())
    }

    fn add_packed_bits(
        &mut self,
        context: Id,
        kind: u32,
        bytes: &[u8],
        bit_length: u32,
        has_emulation_bytes: bool,
    ) -> Result<()> {
        self.add(
            context,
            ffi::BUFFER_ENC_PACKED_HEADER_PARAMETER,
            &packed_header_params(kind, bit_length, has_emulation_bytes),
        )?;
        self.add_bytes(context, ffi::BUFFER_ENC_PACKED_HEADER_DATA, bytes)
    }
}

impl Drop for Buffers<'_> {
    fn drop(&mut self) {
        for &id in &self.ids {
            self.display.destroy_buffer(id);
        }
    }
}

pub struct Vaapi {
    display: Display,
    params: Params,
    setup: Setup,
    stream: Stream,
    /// What the driver said it takes (for the self-test's output).
    picked: Picked,
    rate: Rate,
    /// None until the first frame, and after a size change.
    res: Option<Resources>,
    /// The RGB surfaces made from the output buffers, by buffer.
    imports: HashMap<usize, Id>,
    /// The last picture's reconstruction (the next picture's reference).
    last: Option<(Id, Pic)>,
    /// Which of `Resources::recon` the next picture is reconstructed into.
    recon_next: usize,
    index: u64,
    /// The rate control changed: say so with the next picture.
    rc_dirty: bool,
    need_idr: bool,
    tuning: Tuning,
    /// Packed headers go in without emulation prevention bytes.
    driver_escapes: bool,
    imported_logged: bool,
    sequence_logged: bool,
    scratch: Vec<u8>,
    timings: Timings,
}

// SAFETY: a session is used by one thread at a time (its encoder thread).
unsafe impl Send for Vaapi {}

impl Vaapi {
    pub fn new(render_node: &Path, params: Params) -> Result<Self> {
        let display = Display::open(render_node)?;
        let tuning = Tuning::from_env(params.codec);
        let choice = choose(&display, params.codec, tuning.rate_mode())?;
        let (entrypoint, offered) = (choice.entrypoint, choice.packed);
        let packed = packed_mode(
            offered,
            std::env::var("CHA_VAAPI_PACKED_HEADERS").ok().as_deref(),
            params.codec,
            display.vendor().starts_with("Mesa"),
        );
        // AV1's sequence and frame headers are the application's to write:
        // the driver has nowhere else to read the picture's settings from (and
        // Mesa divides by zero without them).
        ensure!(
            params.codec != Codec::Av1 || packed != Packed::None,
            "the driver takes no packed sequence and frame headers (offered {offered:#x}), which AV1 needs"
        );
        let mut attribs = vec![
            (ffi::ATTRIB_RT_FORMAT, ffi::RT_FORMAT_YUV420),
            (ffi::ATTRIB_RATE_CONTROL, tuning.rate_mode()),
        ];
        // AV1's temporal delimiter goes in as a raw OBU, if the driver takes
        // them.
        let raw_data = params.codec == Codec::Av1
            && packed != Packed::None
            && offered & ffi::PACKED_HEADER_RAW_DATA != 0;
        if let Some(value) = packed.attribute() {
            let raw = if raw_data {
                ffi::PACKED_HEADER_RAW_DATA
            } else {
                0
            };
            attribs.push((ffi::ATTRIB_ENC_PACKED_HEADERS, value | raw));
        }
        let enc_config = display
            .create_config(choice.va_profile, entrypoint, &attribs)
            .with_context(|| format!("a {} {} encoder config", params.codec.name(), choice.name))?;
        let vpp_config =
            match display.create_config(ffi::PROFILE_NONE, ffi::ENTRYPOINT_VIDEO_PROC, &[]) {
                Ok(config) => config,
                Err(err) => {
                    display.destroy_config(enc_config);
                    return Err(err.context("the driver has no video processor (RGB to NV12)"));
                }
            };
        let driver = display.vendor();
        let padding = Padding::of_driver(&driver);
        let stream = match Stream::new(&choice.picked, &params, &tuning, padding) {
            Ok(stream) => stream,
            Err(err) => {
                display.destroy_config(vpp_config);
                display.destroy_config(enc_config);
                return Err(err);
            }
        };
        let rate = Rate::with_buffer(params.bitrate_bps, params.fps, tuning.hrd_frames);
        let setup = Setup {
            entrypoint,
            packed,
            raw_data,
            enc_config,
            vpp_config,
        };
        info!(
            codec = params.codec.name(),
            driver = driver.as_str(),
            profile = stream.name(),
            level = stream.level(),
            width = params.width,
            height = params.height,
            entrypoint = if setup.low_power() { "EncSliceLP" } else { "EncSlice" },
            rate_control = if tuning.cqp.is_some() { "CQP" } else { "CBR" },
            cqp = tuning.cqp,
            hrd_bits = rate.buffer_bits,
            packed_headers = ?packed,
            "VA-API encoder ready"
        );
        if let Stream::Hevc { layout, .. } = &stream {
            // What the driver said it takes, and what we picked of it.
            info!(
                driver_says = ?choice.picked,
                ctb = 1 << layout.blocks.log2_ctb,
                min_cb = 1 << layout.blocks.log2_min_cb,
                blocks = ?layout.blocks,
                tools = ?layout.tools,
                "HEVC coding tree and tools"
            );
        }
        if let Stream::Av1 { layout, .. } = &stream {
            // What the driver said it takes, and what we picked of it.
            info!(
                driver_says = ?choice.picked,
                tools = ?layout.tools,
                knobs = ?layout.knobs,
                coded = ?layout.coded,
                render = ?layout.render,
                raw_data_header = raw_data,
                "AV1 tools"
            );
        }
        Ok(Self {
            display,
            params,
            setup,
            stream,
            picked: choice.picked,
            rate,
            res: None,
            imports: HashMap::new(),
            last: None,
            recon_next: 0,
            index: 0,
            rc_dirty: false,
            need_idr: true,
            tuning,
            driver_escapes: std::env::var("CHA_VAAPI_PACKED_EMULATION")
                .is_ok_and(|v| v == "driver"),
            imported_logged: false,
            sequence_logged: false,
            scratch: Vec::with_capacity(1 << 20),
            timings: Timings::default(),
        })
    }

    /// The picture size a key frame's SPS or sequence header should say:
    /// the size asked for (H.264 and HEVC crop to it; AV1's driver may pad).
    fn expected_size(&self) -> (u32, u32) {
        match &self.stream {
            Stream::Av1 { layout, .. } => layout.coded,
            _ => (self.params.width, self.params.height),
        }
    }

    /// What the encoder is set up as, for a self-test's output.
    fn describe(&self) -> String {
        let av1 = match &self.stream {
            Stream::Av1 { layout, .. } => format!(
                ", AV1 attributes {:?}, tools {:?}, knobs {:?}, coded {:?}, render {:?}, raw data header {}",
                self.picked,
                layout.tools,
                layout.knobs,
                layout.coded,
                layout.render,
                self.setup.raw_data
            ),
            _ => String::new(),
        };
        format!(
            "{} entrypoint, {} {} {}, hrd {} bits, packed headers {:?}, {:?}{av1}",
            if self.setup.low_power() {
                "EncSliceLP"
            } else {
                "EncSlice"
            },
            self.params.codec.name(),
            self.stream.name(),
            if self.tuning.cqp.is_some() {
                "CQP"
            } else {
                "CBR"
            },
            self.rate.buffer_bits,
            self.setup.packed,
            self.tuning
        )
    }

    /// The RGB surface for a buffer, made on first use.
    fn import(&mut self, dmabuf: &Dmabuf, key: usize) -> Result<Id> {
        if let Some(&surface) = self.imports.get(&key) {
            return Ok(surface);
        }
        let surface = self.display.import_dmabuf(dmabuf).inspect_err(|err| {
            let format = dmabuf.format();
            let modifier = format!("{:#x}", u64::from(format.modifier));
            warn!(
                format = ?format.code,
                modifier = modifier.as_str(),
                planes = dmabuf.num_planes(),
                "VA-API can't import the output buffer (no copy possible): {err:#}"
            );
        })?;
        if !self.imported_logged {
            self.imported_logged = true;
            let format = dmabuf.format();
            let modifier = format!("{:#x}", u64::from(format.modifier));
            info!(
                format = ?format.code,
                modifier = modifier.as_str(),
                "output buffers imported into VA-API with no copy"
            );
        }
        self.imports.insert(key, surface);
        Ok(surface)
    }

    fn drop_imports(&mut self) {
        let surfaces: Vec<Id> = self.imports.drain().map(|(_, s)| s).collect();
        self.display.destroy_surfaces(&surfaces);
    }

    fn drop_resources(&mut self) {
        if let Some(res) = self.res.take() {
            res.release(&self.display);
        }
    }

    /// RGB surface → NV12 `input`, in BT.709 limited range, on the GPU.
    fn convert(&self, rgb: Id, res: &Resources) -> Result<()> {
        let region = Rectangle {
            x: 0,
            y: 0,
            width: self.params.width as u16,
            height: self.params.height as u16,
        };
        // Opaque black around the picture where the coded size is larger.
        let pipeline = ffi::proc_pipeline(rgb, &region, 0xff00_0000);
        let mut buffers = Buffers::new(&self.display);
        buffers.add(res.vpp_context, ffi::BUFFER_PROC_PIPELINE, &pipeline)?;
        self.display.begin_picture(res.vpp_context, res.input)?;
        self.display.render_picture(res.vpp_context, &buffers.ids)?;
        self.display.end_picture(res.vpp_context)?;
        // The encoder reads what the processor wrote.
        self.display.sync_surface(res.input)
    }

    /// The rate control, HRD and frame rate buffers: with an IDR, and when
    /// the rate changed.
    fn add_rate_control(&self, buffers: &mut Buffers, context: Id, idr: bool) -> Result<()> {
        if !(idr || self.rc_dirty) {
            return Ok(());
        }
        let reset = self.rc_dirty;
        // (With constant quantiser there is no rate to control.)
        if self.tuning.cqp.is_none() {
            buffers.add(
                context,
                ffi::BUFFER_ENC_MISC_PARAMETER,
                &misc_rate_control(&self.rate, reset),
            )?;
            buffers.add(
                context,
                ffi::BUFFER_ENC_MISC_PARAMETER,
                &misc_hrd(&self.rate),
            )?;
        }
        buffers.add(
            context,
            ffi::BUFFER_ENC_MISC_PARAMETER,
            &misc_frame_rate(self.params.fps),
        )
    }

    /// Codes the converted picture; the coded bytes are in `self.scratch`.
    fn submit(&mut self, picture: &Pic, res: &Resources) -> Result<()> {
        let current = res.recon[self.recon_next];
        let reference = self.last.filter(|_| !picture.idr());
        let context = res.enc_context;
        let mut buffers = Buffers::new(&self.display);
        let packed = self.setup.packed;
        let escapes = self.driver_escapes;
        match (&self.stream, picture) {
            (Stream::H264 { layout, .. }, Pic::H264(picture)) => {
                let reference = reference.and_then(|(surface, last)| match last {
                    Pic::H264(last) => Some((surface, last)),
                    _ => None,
                });
                if picture.idr {
                    let seq = h264::sequence_params(layout, &self.rate);
                    buffers.add(context, ffi::BUFFER_ENC_SEQUENCE, &*seq)?;
                    if packed != Packed::None {
                        let nal = bits::sps_nal(&layout.sps);
                        let unescape =
                            escapes.then_some(crate::encoder::bitstream::unescaped_nal as _);
                        buffers.add_packed(context, ffi::PACKED_SEQUENCE, &nal, unescape)?;
                    }
                }
                self.add_rate_control(&mut buffers, context, picture.idr)?;
                let pic = h264::picture_params(layout, picture, current, reference, res.coded);
                buffers.add(context, ffi::BUFFER_ENC_PICTURE, &*pic)?;
                if picture.idr && packed != Packed::None {
                    let nal = bits::pps_nal(&layout.pps);
                    let unescape = escapes.then_some(crate::encoder::bitstream::unescaped_nal as _);
                    buffers.add_packed(context, ffi::PACKED_PICTURE, &nal, unescape)?;
                }
                if packed == Packed::HeadersAndSlice {
                    let (nal, bit_length) = bits::slice_header_nal(
                        &layout.sps,
                        &layout.pps,
                        &h264::slice_header(picture),
                    );
                    buffers.add_packed_bits(context, ffi::PACKED_SLICE, &nal, bit_length, true)?;
                }
                let slice = h264::slice_params(layout, picture, reference);
                buffers.add(context, ffi::BUFFER_ENC_SLICE, &*slice)?;
            }
            (Stream::Hevc { layout, .. }, Pic::Hevc(picture)) => {
                let reference = reference.and_then(|(surface, last)| match last {
                    Pic::Hevc(last) => Some((surface, last)),
                    _ => None,
                });
                let unescape = escapes.then_some(bits265::unescaped_nal as _);
                if picture.idr {
                    let seq = hevc::sequence_params(layout, &self.rate);
                    buffers.add(context, ffi::BUFFER_ENC_SEQUENCE, &*seq)?;
                    if packed != Packed::None {
                        // Both are "sequence" packed headers: VPS, then SPS.
                        let vps = bits265::vps_nal(&layout.sps);
                        buffers.add_packed(context, ffi::PACKED_SEQUENCE, &vps, unescape)?;
                        let sps = bits265::sps_nal(&layout.sps);
                        buffers.add_packed(context, ffi::PACKED_SEQUENCE, &sps, unescape)?;
                    }
                }
                self.add_rate_control(&mut buffers, context, picture.idr)?;
                let pic = hevc::picture_params(layout, picture, current, reference, res.coded);
                buffers.add(context, ffi::BUFFER_ENC_PICTURE, &*pic)?;
                if picture.idr && packed != Packed::None {
                    let nal = bits265::pps_nal(&layout.pps);
                    buffers.add_packed(context, ffi::PACKED_PICTURE, &nal, unescape)?;
                }
                if packed == Packed::HeadersAndSlice {
                    let header = hevc::slice_header(picture);
                    let (nal, bit_length) =
                        bits265::slice_header_nal(&layout.sps, &layout.pps, &header);
                    buffers.add_packed_bits(context, ffi::PACKED_SLICE, &nal, bit_length, true)?;
                }
                let slice = hevc::slice_params(layout, picture, reference);
                buffers.add(context, ffi::BUFFER_ENC_SLICE, &*slice)?;
            }
            (Stream::Av1 { layout, .. }, Pic::Av1(picture)) => {
                let reference = reference.and_then(|(surface, last)| match last {
                    Pic::Av1(_) => Some(surface),
                    _ => None,
                });
                // The temporal delimiter is the first thing in the unit; on a
                // key frame the sequence header follows it.
                if self.setup.raw_data {
                    let td = bits_av1::temporal_delimiter();
                    buffers.add_packed_bits(
                        context,
                        ffi::PACKED_RAW_DATA,
                        &td,
                        td.len() as u32 * 8,
                        false,
                    )?;
                }
                if picture.key {
                    let seq = av1::sequence_params(layout, &self.rate);
                    buffers.add(context, ffi::BUFFER_ENC_SEQUENCE, &*seq)?;
                    if packed != Packed::None {
                        let obu = av1::sequence_header(layout);
                        buffers.add_packed_bits(
                            context,
                            ffi::PACKED_SEQUENCE,
                            &obu,
                            obu.len() as u32 * 8,
                            false,
                        )?;
                    }
                }
                self.add_rate_control(&mut buffers, context, picture.key)?;
                let qindex = self
                    .tuning
                    .cqp
                    .map_or(av1::START_QINDEX, |qp| (qp * 5).min(255) as u8);
                let pic =
                    av1::picture_params(layout, picture, current, reference, res.coded, qindex);
                buffers.add(context, ffi::BUFFER_ENC_PICTURE, &*pic)?;
                if packed != Packed::None {
                    let header = av1::frame_header(layout, picture);
                    let bits = header.bytes.len() as u32 * 8;
                    buffers.add_packed_bits(
                        context,
                        ffi::PACKED_PICTURE,
                        &header.bytes,
                        bits,
                        false,
                    )?;
                }
                buffers.add(context, ffi::BUFFER_ENC_SLICE, &av1::tile_group_params())?;
            }
            _ => bail!("the picture isn't of the stream's codec"),
        }

        let submitted = Instant::now();
        self.display.begin_picture(context, res.input)?;
        self.display.render_picture(context, &buffers.ids)?;
        self.display.end_picture(context)?;
        self.timings.submit = submitted.elapsed();

        let waited = Instant::now();
        self.display.sync_surface(res.input)?;
        self.scratch.clear();
        self.display
            .read_coded_buffer(res.coded, &mut self.scratch)?;
        self.timings.wait = waited.elapsed();
        Ok(())
    }

    /// Encodes the compositor's buffer. `key` identifies the buffer (the
    /// import is cached under it).
    pub fn encode_dmabuf(
        &mut self,
        dmabuf: &Dmabuf,
        key: usize,
        keyframe: bool,
        out: &mut Vec<u8>,
    ) -> Result<bool> {
        let size = dmabuf.size();
        ensure!(
            (size.w as u32, size.h as u32) == (self.params.width, self.params.height),
            "a {}×{} frame for a {}×{} encoder",
            size.w,
            size.h,
            self.params.width,
            self.params.height
        );
        let started = Instant::now();
        let rgb = self.import(dmabuf, key)?;
        let res = match self.res.take() {
            Some(res) => res,
            None => Resources::create(&self.display, &self.setup, self.stream.coded_size())
                .context("creating the encoder's surfaces and contexts")?,
        };
        let result = self.encode_with(&res, rgb, started, keyframe, out);
        self.res = Some(res);
        if result.is_err() {
            // Whatever the driver did with this picture, the next one starts over.
            self.need_idr = true;
            self.stream.reset();
            self.last = None;
        }
        result
    }

    fn encode_with(
        &mut self,
        res: &Resources,
        rgb: Id,
        started: Instant,
        keyframe: bool,
        out: &mut Vec<u8>,
    ) -> Result<bool> {
        self.convert(rgb, res)
            .context("converting the frame to NV12")?;
        self.timings = Timings {
            map: started.elapsed(),
            ..Timings::default()
        };
        if let (Some(path), 0) = (&self.tuning.dump_nv12, self.index) {
            match self
                .display
                .read_nv12(res.input, self.params.width, self.params.height)
                .and_then(|nv12| std::fs::write(path, &nv12).map_err(Into::into))
            {
                Ok(()) => info!(
                    path = %path.display(),
                    width = self.params.width,
                    height = self.params.height,
                    "the encoder's NV12 input for the first frame written (packed NV12, no padding)"
                ),
                Err(err) => warn!("couldn't dump the NV12 input: {err:#}"),
            }
        }
        let idr = keyframe || self.need_idr || self.last.is_none();
        let picture = self.stream.next(idr);
        self.submit(&picture, res).context("encoding the frame")?;
        ensure!(!self.scratch.is_empty(), "the driver made no bitstream");
        let key = if let Stream::Av1 { layout, .. } = &self.stream {
            let sequence = av1::sequence_header(layout);
            let (key, kept) = bits_av1::shape_temporal_unit(&self.scratch, &sequence, out)
                .map_err(|e| anyhow::anyhow!("the driver's AV1 output: {e}"))?;
            ensure!(
                key == picture.idr(),
                "asked for {} frame, the driver made {}",
                if picture.idr() { "a key" } else { "an inter" },
                if key { "a key frame" } else { "an inter frame" }
            );
            if key && !kept && !self.sequence_logged {
                self.sequence_logged = true;
                info!("the driver left the sequence header out of a key frame; ours is added");
            }
            key
        } else {
            let key = self.stream.has_idr(&self.scratch);
            ensure!(
                key == picture.idr(),
                "asked for {} picture, the driver made {}",
                if picture.idr() { "an IDR" } else { "a P" },
                if key { "an IDR" } else { "a P" }
            );
            if key {
                if self.stream.fix_headers(&self.scratch, out) {
                    warn!(
                        "the driver's SPS uses features we don't parse; its VUI is left as it is"
                    );
                }
            } else {
                out.clear();
                out.extend_from_slice(&self.scratch);
            }
            key
        };
        self.last = Some((res.recon[self.recon_next], picture));
        self.recon_next ^= 1;
        self.need_idr = false;
        self.rc_dirty = false;
        self.index += 1;
        Ok(key)
    }
}

impl VideoEncoder for Vaapi {
    fn params(&self) -> &Params {
        &self.params
    }

    fn encode(&mut self, frame: &Frame, keyframe: bool, out: &mut Vec<u8>) -> Result<bool> {
        let dmabuf = frame
            .slot
            .dmabuf()
            .context("VA-API takes frames the compositor shares as dmabufs")?;
        let key = std::sync::Arc::as_ptr(&frame.slot) as usize;
        self.encode_dmabuf(dmabuf, key, keyframe, out)
    }

    fn resize(&mut self, width: u32, height: u32) -> Result<()> {
        let mut params = self.params.clone();
        params.width = width;
        params.height = height;
        let stream = self.stream.resized(&params, &self.tuning)?;
        self.drop_imports();
        self.drop_resources();
        self.params = params;
        self.stream = stream;
        self.need_idr = true;
        self.last = None;
        self.rc_dirty = false;
        Ok(())
    }

    fn set_bitrate(&mut self, bitrate_bps: u32) -> Result<()> {
        self.params.bitrate_bps = bitrate_bps;
        self.rate = Rate::with_buffer(bitrate_bps, self.params.fps, self.tuning.hrd_frames);
        self.rc_dirty = true;
        Ok(())
    }

    fn set_frame_rate(&mut self, fps: u32, bitrate_bps: u32) -> Result<()> {
        self.params.fps = fps;
        self.params.bitrate_bps = bitrate_bps;
        self.stream.with_fps(&self.params);
        self.rate = Rate::with_buffer(bitrate_bps, fps, self.tuning.hrd_frames);
        self.rc_dirty = true;
        Ok(())
    }

    fn next_index(&self) -> u64 {
        self.index
    }

    fn forget_surfaces(&mut self) {
        self.drop_imports();
    }

    fn last_timings(&self) -> Timings {
        self.timings
    }
}

impl Drop for Vaapi {
    fn drop(&mut self) {
        self.drop_imports();
        self.drop_resources();
        self.display.destroy_config(self.setup.vpp_config);
        self.display.destroy_config(self.setup.enc_config);
    }
}

/// Whether the driver on `render_node` takes `dmabuf` as an input surface: the
/// output pool asks while it picks a format (and modifier), so it allocates
/// only buffers the encoder can import without a copy.
pub fn accepts_import(render_node: &Path, dmabuf: &Dmabuf) -> Result<()> {
    let display = Display::open(render_node)?;
    let surface = display.import_dmabuf(dmabuf)?;
    display.destroy_surfaces(&[surface]);
    Ok(())
}

/// One row of the self-test picture, XRGB8888 (B, G, R, unused in memory): a
/// static dark navy background with a fine grid (every 8 pixels, a stronger
/// line every 64) and a purple gradient over the bottom quarter, so drift in
/// still areas shows; and motion: a white bar sweeping across, and a block of
/// 16 squares showing the frame number in binary.
fn paint_row(row: &mut [u8], y: u32, n: u32, width: u32, height: u32) {
    let sweep = (n * 12) % (width - 48);
    for x in 0..width {
        // R, G, B
        let mut rgb = if y >= height * 3 / 4 {
            let t = (y - height * 3 / 4) * 255 / (height / 4);
            [(40 + t / 2) as u8, 20, (80 + t / 2) as u8]
        } else {
            [12, 16, 48]
        };
        if x.is_multiple_of(8) || y.is_multiple_of(8) {
            rgb = [38, 54, 110];
        }
        if x.is_multiple_of(64) || y.is_multiple_of(64) {
            rgb = [90, 120, 200];
        }
        if (sweep..sweep + 48).contains(&x) && (96..160).contains(&y) {
            rgb = [255, 255, 255];
        }
        if (96..96 + 16 * 20).contains(&x) && (200..220).contains(&y) {
            let bit = (x - 96) / 20;
            rgb = if (n >> bit) & 1 == 1 {
                [240, 220, 40]
            } else {
                [30, 30, 30]
            };
        }
        let px = &mut row[x as usize * 4..x as usize * 4 + 4];
        px.copy_from_slice(&[rgb[2], rgb[1], rgb[0], 0]);
    }
}

/// Paints the self-test picture `n` into a linear XRGB dmabuf.
fn paint_dmabuf(dmabuf: &Dmabuf, stride: usize, width: u32, height: u32, n: u32) -> Result<()> {
    use smithay::backend::allocator::dmabuf::{DmabufMappingMode, DmabufSyncFlags};
    let mapping = dmabuf
        .map_plane(0, DmabufMappingMode::WRITE)
        .map_err(|e| anyhow::anyhow!("mapping the buffer: {e}"))?;
    dmabuf
        .sync_plane(0, DmabufSyncFlags::START | DmabufSyncFlags::WRITE)
        .map_err(|e| anyhow::anyhow!("syncing the buffer: {e}"))?;
    let base = mapping.ptr().cast::<u8>();
    for y in 0..height {
        // SAFETY: the mapping holds `stride x height` bytes.
        let row = unsafe {
            std::slice::from_raw_parts_mut(base.add(y as usize * stride), width as usize * 4)
        };
        paint_row(row, y, n, width, height);
    }
    dmabuf
        .sync_plane(0, DmabufSyncFlags::END | DmabufSyncFlags::WRITE)
        .map_err(|e| anyhow::anyhow!("syncing the buffer: {e}"))?;
    Ok(())
}

/// What `check_access_unit` of any codec found.
struct Unit {
    nals: Vec<u8>,
    idr: bool,
    size: Option<(u32, u32)>,
}

fn check_access_unit(codec: Codec, stream: &[u8]) -> Result<Unit> {
    Ok(match codec {
        Codec::Av1 => {
            let unit = bits_av1::check_temporal_unit(stream).map_err(anyhow::Error::msg)?;
            Unit {
                nals: unit.obus,
                idr: unit.key,
                size: unit.size,
            }
        }
        Codec::Hevc => {
            let unit = bits265::check_access_unit(stream).map_err(anyhow::Error::msg)?;
            Unit {
                nals: unit.nals,
                idr: unit.idr,
                size: unit.size,
            }
        }
        _ => {
            let unit = h264::check_access_unit(stream)?;
            Unit {
                nals: unit.nals,
                idr: unit.idr,
                size: unit.size,
            }
        }
    })
}

/// What a self-test run found.
#[derive(Debug)]
pub struct SelfTest {
    pub frames: u32,
    pub keyframes: u32,
    pub bytes: u64,
    pub encode_ms_avg: f64,
    /// The NAL unit types of the first picture (SPS, PPS, IDR slice; HEVC has
    /// a VPS in front); AV1's OBU types (temporal delimiter, sequence header,
    /// frame).
    pub first_nals: Vec<u8>,
    /// The first key frame's size in bytes.
    pub key_bytes: usize,
    /// The average inter picture's size in bytes before the bitrate was
    /// lowered (20 to 8 Mbit/s) and after it had settled.
    pub p_bytes_before: usize,
    pub p_bytes_after: usize,
    /// How the encoder was set up (entrypoint, rate control, headers, knobs).
    pub setup: String,
}

/// Encodes `frames` generated pictures on `render_node` and checks the output
/// is H.264 (or HEVC, with `CHA_ENCODE_TEST_CODEC=hevc`, or AV1 with `av1`) as
/// the players expect: an IDR with its parameter sets first (AV1: a temporal
/// unit with a sequence header and a key frame), P pictures after it, another
/// IDR on request, and the stream still coding after a bitrate change.
/// `CHA_ENCODE_TEST_SIZE=<w>x<h>` picks another picture size (default
/// 1280x720). For `CHA_ENCODE_TEST=<frames> cha-streamer --probe-device
/// vaapi:<node>` and the ignored test below.
pub fn self_test(render_node: &Path, frames: u32) -> Result<SelfTest> {
    use smithay::backend::allocator::dmabuf::{AsDmabuf, DmabufMappingMode, DmabufSyncFlags};
    use smithay::backend::allocator::gbm::{GbmAllocator, GbmBufferFlags, GbmDevice};
    use smithay::backend::allocator::{Allocator, Fourcc, Modifier};
    use smithay::utils::DeviceFd;
    use std::os::fd::OwnedFd;

    ensure!(
        frames >= 8,
        "at least 8 frames, to see an IDR, P pictures and a requested IDR"
    );
    let (width, height) = match std::env::var("CHA_ENCODE_TEST_SIZE") {
        Ok(size) => {
            let (w, h) = size
                .split_once('x')
                .and_then(|(w, h)| Some((w.parse::<u32>().ok()?, h.parse::<u32>().ok()?)))
                .context("CHA_ENCODE_TEST_SIZE is <width>x<height>")?;
            ensure!(
                w >= 128 && h >= 64 && w % 2 == 0 && h % 2 == 0,
                "{w}x{h} is too small or odd"
            );
            (w, h)
        }
        Err(_) => (1280u32, 720u32),
    };
    let file = std::fs::File::options()
        .read(true)
        .write(true)
        .open(render_node)
        .with_context(|| format!("opening {}", render_node.display()))?;
    let gbm =
        GbmDevice::new(DeviceFd::from(OwnedFd::from(file))).context("creating the GBM device")?;
    let mut allocator = GbmAllocator::new(gbm, GbmBufferFlags::RENDERING);
    // Linear, so the CPU can paint it; the imports of tiled ones are the
    // output pool's business (it asks `accepts_import`).
    let buffer = allocator
        .create_buffer(width, height, Fourcc::Xrgb8888, &[Modifier::Linear])
        .context("allocating a linear XRGB buffer")?;
    let dmabuf = buffer.export().context("exporting it as a dmabuf")?;
    let stride = dmabuf.strides().next().context("no stride")? as usize;

    let codec = match std::env::var("CHA_ENCODE_TEST_CODEC").as_deref() {
        Ok("hevc" | "h265") => Codec::Hevc,
        Ok("av1") => Codec::Av1,
        Ok("h264") | Err(_) => Codec::H264,
        Ok(other) => bail!("CHA_ENCODE_TEST_CODEC is h264, hevc or av1, not {other:?}"),
    };
    let mut encoder = Vaapi::new(
        render_node,
        Params {
            codec,
            width,
            height,
            fps: 60,
            bitrate_bps: 20_000_000,
        },
    )?;
    let (mut keyframes, mut bytes) = (0u32, 0u64);
    let mut total = std::time::Duration::ZERO;
    let mut out = Vec::new();
    let mut first_nals = Vec::new();
    // CHA_ENCODE_TEST_DUMP=/path/out.h264 (or .hevc, .obu) writes the stream
    // there (Annex-B; AV1: the temporal units one after another, which `dav1d
    // -i out.obu -o out.y4m` reads) and the generated pictures to
    // /path/out.src: raw, 4 bytes a pixel in the order B, G, R, unused
    // (ffmpeg: `-f rawvideo -pix_fmt bgr0 -video_size 1280x720 -framerate 60
    // -i out.src`).
    let dump = std::env::var_os("CHA_ENCODE_TEST_DUMP").map(std::path::PathBuf::from);
    let mut stream_file = dump
        .as_ref()
        .map(std::fs::File::create)
        .transpose()
        .context("creating the dump file")?;
    let mut source = dump
        .as_ref()
        .map(|p| std::fs::File::create(p.with_extension("src")))
        .transpose()
        .context("creating the source dump file")?;
    if let Some(path) = &dump {
        let (stream, source) = (
            path.display().to_string(),
            path.with_extension("src").display().to_string(),
        );
        info!(
            stream = stream.as_str(),
            source = source.as_str(),
            width,
            height,
            frames,
            pixel_format = "bgr0 (B, G, R, unused), rows of width*4 bytes, no padding",
            "self-test dump"
        );
    }
    let ask_idr_at = frames / 2;
    let change_bitrate_at = frames / 4;
    // Inter pictures' sizes: before the bitrate change (after the first few,
    // which fill the buffer) and after it has settled.
    let (mut before, mut after) = ((0usize, 0usize), (0usize, 0usize));
    let mut key_bytes = 0;
    let expected_size = encoder.expected_size();
    for n in 0..frames {
        {
            let mapping = dmabuf
                .map_plane(0, DmabufMappingMode::WRITE)
                .map_err(|e| anyhow::anyhow!("mapping the buffer: {e}"))?;
            dmabuf
                .sync_plane(0, DmabufSyncFlags::START | DmabufSyncFlags::WRITE)
                .map_err(|e| anyhow::anyhow!("syncing the buffer: {e}"))?;
            let base = mapping.ptr().cast::<u8>();
            for y in 0..height {
                // SAFETY: the mapping holds `stride × height` bytes.
                let row = unsafe {
                    std::slice::from_raw_parts_mut(
                        base.add(y as usize * stride),
                        width as usize * 4,
                    )
                };
                paint_row(row, y, n, width, height);
            }
            if let Some(src) = &mut source {
                use std::io::Write;
                for y in 0..height {
                    // SAFETY: as above.
                    let row = unsafe {
                        std::slice::from_raw_parts(
                            base.add(y as usize * stride),
                            width as usize * 4,
                        )
                    };
                    src.write_all(row)?;
                }
            }
            dmabuf
                .sync_plane(0, DmabufSyncFlags::END | DmabufSyncFlags::WRITE)
                .map_err(|e| anyhow::anyhow!("syncing the buffer: {e}"))?;
        }
        // (A rate change would muddy a comparison: with a dump the rate holds,
        // unless CHA_ENCODE_TEST_RATE_CHANGE is set, to see a decoder take it.)
        if n == change_bitrate_at
            && (dump.is_none() || std::env::var_os("CHA_ENCODE_TEST_RATE_CHANGE").is_some())
        {
            encoder.set_bitrate(8_000_000)?;
        }
        let want_key = n == ask_idr_at;
        let started = Instant::now();
        let key = encoder.encode_dmabuf(&dmabuf, 1, want_key, &mut out)?;
        total += started.elapsed();
        if let Some(file) = &mut stream_file {
            use std::io::Write;
            file.write_all(&out)?;
        }
        let unit = check_access_unit(codec, &out).with_context(|| {
            format!(
                "frame {n} ({} bytes) isn't well-formed {}",
                out.len(),
                codec.name()
            )
        })?;
        ensure!(
            unit.idr == key,
            "frame {n}: the key flag says {key}, the bitstream {}",
            unit.idr
        );
        let expect_key = n == 0 || want_key;
        if !expect_key {
            if (8..change_bitrate_at).contains(&n) {
                before = (before.0 + out.len(), before.1 + 1);
            } else if n >= change_bitrate_at + 10 && n < ask_idr_at {
                after = (after.0 + out.len(), after.1 + 1);
            }
        } else if key_bytes == 0 {
            key_bytes = out.len();
        }
        ensure!(
            key == expect_key,
            "frame {n}: {} where {} was expected",
            if key { "an IDR" } else { "a P picture" },
            if expect_key { "an IDR" } else { "a P picture" }
        );
        if n == 0 {
            first_nals = unit.nals.clone();
        }
        if key {
            keyframes += 1;
            ensure!(
                unit.size == Some(expected_size),
                "frame {n}: the SPS says {:?}, not {}×{}",
                unit.size,
                expected_size.0,
                expected_size.1
            );
        }
        bytes += out.len() as u64;
    }
    ensure!(
        encoder.next_index() == u64::from(frames),
        "the frame counter is off"
    );
    // A size change: new surfaces and contexts, and a key frame first, then
    // inter frames, at the size the headers say.
    let (width2, height2) = ((width * 3 / 4) & !1, (height * 3 / 4) & !1);
    encoder.resize(width2, height2)?;
    let buffer2 = allocator
        .create_buffer(width2, height2, Fourcc::Xrgb8888, &[Modifier::Linear])
        .context("allocating the resized buffer")?;
    let dmabuf2 = buffer2.export().context("exporting it as a dmabuf")?;
    let stride2 = dmabuf2.strides().next().context("no stride")? as usize;
    let expected2 = encoder.expected_size();
    for n in 0..4u32 {
        paint_dmabuf(&dmabuf2, stride2, width2, height2, n)?;
        let key = encoder.encode_dmabuf(&dmabuf2, 1, false, &mut out)?;
        let unit = check_access_unit(codec, &out).with_context(|| {
            format!(
                "frame {n} after the size change to {width2}x{height2} isn't well-formed {}",
                codec.name()
            )
        })?;
        ensure!(
            unit.idr == key && key == (n == 0),
            "after the size change to {width2}x{height2}: frame {n} is {} (a key frame first, then inter ones)",
            if key { "a key frame" } else { "an inter frame" }
        );
        if key {
            ensure!(
                unit.size == Some(expected2),
                "after the size change: the headers say {:?}, not {}x{}",
                unit.size,
                expected2.0,
                expected2.1
            );
        }
    }
    Ok(SelfTest {
        frames,
        keyframes,
        bytes,
        encode_ms_avg: total.as_secs_f64() * 1000.0 / f64::from(frames),
        first_nals,
        key_bytes,
        p_bytes_before: before.0.checked_div(before.1).unwrap_or(0),
        p_bytes_after: after.0.checked_div(after.1).unwrap_or(0),
        setup: encoder.describe(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packed_headers_follow_what_the_driver_offers() {
        let seq_pic = ffi::PACKED_HEADER_SEQUENCE | ffi::PACKED_HEADER_PICTURE;
        let all = seq_pic | ffi::PACKED_HEADER_SLICE | 0x18;
        let h264 = Codec::H264;
        assert_eq!(packed_mode(all, None, h264, false), Packed::Headers);
        assert_eq!(packed_mode(seq_pic, None, h264, false), Packed::Headers);
        // Only the sequence header: the driver writes its own.
        assert_eq!(
            packed_mode(ffi::PACKED_HEADER_SEQUENCE, None, h264, false),
            Packed::None
        );
        assert_eq!(packed_mode(0, None, h264, false), Packed::None);
        // The knobs.
        assert_eq!(packed_mode(all, Some("off"), h264, false), Packed::None);
        assert_eq!(
            packed_mode(all, Some("slice"), h264, false),
            Packed::HeadersAndSlice
        );
        assert_eq!(
            packed_mode(seq_pic, Some("slice"), h264, false),
            Packed::Headers
        );
        // HEVC packs the slice header unless told not to.
        let hevc = Codec::Hevc;
        assert_eq!(packed_mode(all, None, hevc, false), Packed::HeadersAndSlice);
        assert_eq!(
            packed_mode(all, Some("headers"), hevc, false),
            Packed::Headers
        );
        assert_eq!(packed_mode(all, Some("off"), hevc, false), Packed::None);
        assert_eq!(packed_mode(seq_pic, None, hevc, false), Packed::Headers);
        // H.264 on Mesa packs the slice header too, unless told not to.
        assert_eq!(packed_mode(all, None, h264, true), Packed::HeadersAndSlice);
        assert_eq!(packed_mode(seq_pic, None, h264, true), Packed::Headers);
        assert_eq!(
            packed_mode(all, Some("headers"), h264, true),
            Packed::Headers
        );
        // AV1 always packs its two headers, and has no slice header.
        let av1 = Codec::Av1;
        assert_eq!(packed_mode(all, None, av1, true), Packed::Headers);
        assert_eq!(packed_mode(all, Some("off"), av1, true), Packed::Headers);
        assert_eq!(packed_mode(all, Some("slice"), av1, false), Packed::Headers);
        assert_eq!(packed_mode(seq_pic, None, av1, true), Packed::Headers);
        assert_eq!(
            packed_mode(ffi::PACKED_HEADER_SEQUENCE, None, av1, true),
            Packed::None
        );
        assert_eq!(Packed::None.attribute(), None);
        assert_eq!(Packed::Headers.attribute(), Some(3));
        assert_eq!(Packed::HeadersAndSlice.attribute(), Some(7));
    }

    /// On a machine with an Intel or AMD GPU:
    /// `CHA_VAAPI_NODE=/dev/dri/renderD128 cargo test -p cha-streamer
    /// vaapi_self_test -- --ignored --nocapture`.
    #[test]
    #[ignore = "needs a VA-API device (set CHA_VAAPI_NODE)"]
    fn vaapi_self_test() {
        let node = std::env::var("CHA_VAAPI_NODE").unwrap_or_else(|_| "/dev/dri/renderD128".into());
        let result = self_test(Path::new(&node), 120).unwrap_or_else(|e| panic!("{e:#}"));
        println!("{result:?}");
        assert_eq!(result.frames, 120);
        assert_eq!(result.keyframes, 2);
    }
}

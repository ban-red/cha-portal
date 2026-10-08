//! The VA-API H.264 encoder: one stream on one render node.
//!
//! Per frame, all on the GPU:
//! 1. the compositor's dmabuf is imported as an RGB VA surface (no copy;
//!    cached per output buffer);
//! 2. the video processor converts it to NV12 (BT.709, limited range);
//! 3. the encoder codes that as one slice, an IDR or a P picture referring
//!    to the picture before it, in CBR with a one-frame HRD buffer;
//! 4. the coded buffer is read back (Annex-B), and an IDR gets our SPS and
//!    PPS (`bitstream::h264::fix_headers`).
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

use std::collections::HashMap;
use std::ffi::c_int;
use std::path::Path;
use std::time::Instant;

use anyhow::{Context, Result, bail, ensure};
use cha_nvenc::{Codec, Timings};
use smithay::backend::allocator::Buffer;
use smithay::backend::allocator::dmabuf::Dmabuf;
use tracing::{info, warn};

use super::ffi::{self, Display, Id, Rectangle};
use super::h264::{
    self, Gop, Layout, Picture, Profile, Rate, misc_frame_rate, misc_hrd, misc_rate_control,
    packed_header_params, picture_params, sequence_params, slice_params,
};
use crate::encoder::bitstream::h264 as bits;
use crate::encoder::{Params, VideoEncoder};
use crate::media::Frame;

/// The HRD buffer and rate window in frames. One frame starved iHD: a
/// 1280x720 IDR got about 41 KB at 20 Mbit/s and came out at 27 dB with
/// whole macroblock rows flat, P frames trailing the motion (23 dB). Measured
/// on a UHD 630 (iHD 26.1.2): 2 frames 39/31 dB, 4 frames 45-56 dB with a
/// 114 KB IDR (under 3 frame times to send), 8 frames 50-65 dB but 260 KB
/// IDRs. IDRs are rare (on request), P frames small.
const DEFAULT_HRD_FRAMES: u32 = 4;

/// Settings for trying a driver, from the environment (read when an encoder
/// starts, so they apply to a self-test run and to a streamer alike; they
/// have to reach the process, which a container needs `-e` for).
#[derive(Clone, Debug, Default)]
struct Tuning {
    /// `CHA_VAAPI_HRD_FRAMES=n`: the HRD buffer holds n frames
    /// ([`DEFAULT_HRD_FRAMES`] unless set).
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
    fn from_env() -> Self {
        let number = |name: &str| std::env::var(name).ok().and_then(|v| v.parse::<u32>().ok());
        Self {
            hrd_frames: number("CHA_VAAPI_HRD_FRAMES")
                .unwrap_or(DEFAULT_HRD_FRAMES)
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
    /// And the slice header too (an experiment).
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
    enc_config: Id,
    vpp_config: Id,
}

impl Setup {
    fn low_power(&self) -> bool {
        self.entrypoint == ffi::ENTRYPOINT_ENC_SLICE_LP
    }
}

/// The best profile and entrypoint with CBR: High before Main before
/// Constrained Baseline, the low-power entrypoint before the other.
fn choose(display: &Display, rate_mode: u32) -> Result<(Profile, c_int, u32)> {
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
    let mut seen = Vec::new();
    for profile in Profile::PREFERENCE {
        let have = display.entrypoints(profile.va);
        for &entrypoint in entrypoints {
            if !have.contains(&entrypoint) {
                continue;
            }
            let values = display.attributes(
                profile.va,
                entrypoint,
                &[ffi::ATTRIB_RATE_CONTROL, ffi::ATTRIB_ENC_PACKED_HEADERS],
            );
            let (rate_control, packed) = (values[0], values[1]);
            if rate_control != ffi::ATTRIB_NOT_SUPPORTED && rate_control & rate_mode != 0 {
                let packed = if packed == ffi::ATTRIB_NOT_SUPPORTED {
                    0
                } else {
                    packed
                };
                return Ok((profile, entrypoint, packed));
            }
            seen.push(format!(
                "{} entrypoint {entrypoint}: rate control {rate_control:#x}",
                profile.name
            ));
        }
    }
    bail!(
        "the driver has no H.264 encode entrypoint with CBR ({})",
        if seen.is_empty() {
            "none of High, Main or Constrained Baseline encodes".to_string()
        } else {
            seen.join("; ")
        }
    )
}

/// The stream's layout; with constant quantiser the PPS carries that QP.
fn make_layout(profile: Profile, params: &Params, tuning: &Tuning) -> Result<Layout> {
    let mut layout = Layout::new(profile, params)?;
    if let Some(qp) = tuning.cqp {
        layout.pps.pic_init_qp = qp as i32;
    }
    Ok(layout)
}

/// Which headers to pack, from what the driver says it takes.
fn packed_mode(offered: u32, setting: Option<&str>) -> Packed {
    let both = ffi::PACKED_HEADER_SEQUENCE | ffi::PACKED_HEADER_PICTURE;
    match setting {
        Some("off") => Packed::None,
        Some("slice") if offered & both == both && offered & ffi::PACKED_HEADER_SLICE != 0 => {
            Packed::HeadersAndSlice
        }
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
    fn create(display: &Display, setup: &Setup, layout: &Layout) -> Result<Self> {
        let mut res = Self {
            input: ffi::INVALID_ID,
            recon: [ffi::INVALID_ID; 2],
            coded: ffi::INVALID_ID,
            enc_context: ffi::INVALID_ID,
            vpp_context: ffi::INVALID_ID,
        };
        match res.fill(display, setup, layout) {
            Ok(()) => Ok(res),
            Err(err) => {
                res.release(display);
                Err(err)
            }
        }
    }

    fn fill(&mut self, display: &Display, setup: &Setup, layout: &Layout) -> Result<()> {
        let (width, height) = layout.coded_size();
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
    /// emulation prevention bytes in, and say so; with `driver_escapes` the
    /// bytes are taken out again and the driver is asked to put them back
    /// (`CHA_VAAPI_PACKED_EMULATION=driver`, in case a driver ignores the flag).
    fn add_packed(
        &mut self,
        context: Id,
        kind: u32,
        nal: &[u8],
        driver_escapes: bool,
    ) -> Result<()> {
        let raw;
        let (bytes, bit_length): (&[u8], u32) = if driver_escapes {
            raw = crate::encoder::bitstream::unescaped_nal(nal);
            (&raw, raw.len() as u32 * 8)
        } else {
            (nal, nal.len() as u32 * 8)
        };
        self.add_packed_bits(context, kind, bytes, bit_length, !driver_escapes)
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
    layout: Layout,
    rate: Rate,
    /// None until the first frame, and after a size change.
    res: Option<Resources>,
    /// The RGB surfaces made from the output buffers, by buffer.
    imports: HashMap<usize, Id>,
    gop: Gop,
    /// The last picture's reconstruction (the next picture's reference).
    last: Option<(Id, Picture)>,
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
    scratch: Vec<u8>,
    timings: Timings,
}

// SAFETY: a session is used by one thread at a time (its encoder thread).
unsafe impl Send for Vaapi {}

impl Vaapi {
    pub fn new(render_node: &Path, params: Params) -> Result<Self> {
        ensure!(
            params.codec == Codec::H264,
            "VA-API encodes H.264 only so far, not {}",
            params.codec.name()
        );
        let display = Display::open(render_node)?;
        let tuning = Tuning::from_env();
        let (profile, entrypoint, offered) = choose(&display, tuning.rate_mode())?;
        let packed = packed_mode(
            offered,
            std::env::var("CHA_VAAPI_PACKED_HEADERS").ok().as_deref(),
        );
        let mut attribs = vec![
            (ffi::ATTRIB_RT_FORMAT, ffi::RT_FORMAT_YUV420),
            (ffi::ATTRIB_RATE_CONTROL, tuning.rate_mode()),
        ];
        if let Some(value) = packed.attribute() {
            attribs.push((ffi::ATTRIB_ENC_PACKED_HEADERS, value));
        }
        let enc_config = display
            .create_config(profile.va, entrypoint, &attribs)
            .with_context(|| format!("an H.264 {} encoder config", profile.name))?;
        let vpp_config =
            match display.create_config(ffi::PROFILE_NONE, ffi::ENTRYPOINT_VIDEO_PROC, &[]) {
                Ok(config) => config,
                Err(err) => {
                    display.destroy_config(enc_config);
                    return Err(err.context("the driver has no video processor (RGB to NV12)"));
                }
            };
        let layout = make_layout(profile, &params, &tuning)?;
        let rate = Rate::with_buffer(params.bitrate_bps, params.fps, tuning.hrd_frames);
        let setup = Setup {
            entrypoint,
            packed,
            enc_config,
            vpp_config,
        };
        let driver = display.vendor();
        info!(
            codec = "h264",
            driver = driver.as_str(),
            profile = profile.name,
            level = layout.sps.level_idc,
            width = params.width,
            height = params.height,
            entrypoint = if setup.low_power() { "EncSliceLP" } else { "EncSlice" },
            rate_control = if tuning.cqp.is_some() { "CQP" } else { "CBR" },
            cqp = tuning.cqp,
            hrd_bits = rate.buffer_bits,
            packed_headers = ?packed,
            "VA-API encoder ready"
        );
        Ok(Self {
            display,
            params,
            setup,
            layout,
            rate,
            res: None,
            imports: HashMap::new(),
            gop: Gop::default(),
            last: None,
            recon_next: 0,
            index: 0,
            rc_dirty: false,
            need_idr: true,
            tuning,
            driver_escapes: std::env::var("CHA_VAAPI_PACKED_EMULATION")
                .is_ok_and(|v| v == "driver"),
            imported_logged: false,
            scratch: Vec::with_capacity(1 << 20),
            timings: Timings::default(),
        })
    }

    /// What the encoder is set up as, for a self-test's output.
    fn describe(&self) -> String {
        format!(
            "{} entrypoint, {} {}, hrd {} bits, packed headers {:?}, {:?}",
            if self.setup.low_power() {
                "EncSliceLP"
            } else {
                "EncSlice"
            },
            self.layout.profile.name,
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

    /// Codes the converted picture; the coded bytes are in `self.scratch`.
    fn submit(&mut self, picture: &Picture, res: &Resources) -> Result<()> {
        let current = res.recon[self.recon_next];
        let reference = self.last.filter(|_| !picture.idr);
        let context = res.enc_context;
        let mut buffers = Buffers::new(&self.display);
        if picture.idr {
            let seq = sequence_params(&self.layout, &self.rate);
            buffers.add(context, ffi::BUFFER_ENC_SEQUENCE, &*seq)?;
            if self.setup.packed != Packed::None {
                let nal = bits::sps_nal(&self.layout.sps);
                buffers.add_packed(context, ffi::PACKED_SEQUENCE, &nal, self.driver_escapes)?;
            }
        }
        if picture.idr || self.rc_dirty {
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
            )?;
        }
        let pic = picture_params(&self.layout, picture, current, reference, res.coded);
        buffers.add(context, ffi::BUFFER_ENC_PICTURE, &*pic)?;
        if picture.idr && self.setup.packed != Packed::None {
            let nal = bits::pps_nal(&self.layout.pps);
            buffers.add_packed(context, ffi::PACKED_PICTURE, &nal, self.driver_escapes)?;
        }
        if self.setup.packed == Packed::HeadersAndSlice {
            let (nal, bit_length) = bits::slice_header_nal(
                &self.layout.sps,
                &self.layout.pps,
                &h264::slice_header(picture),
            );
            buffers.add_packed_bits(context, ffi::PACKED_SLICE, &nal, bit_length, true)?;
        }
        let slice = slice_params(&self.layout, picture, reference);
        buffers.add(context, ffi::BUFFER_ENC_SLICE, &*slice)?;

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
            None => Resources::create(&self.display, &self.setup, &self.layout)
                .context("creating the encoder's surfaces and contexts")?,
        };
        let result = self.encode_with(&res, rgb, started, keyframe, out);
        self.res = Some(res);
        if result.is_err() {
            // Whatever the driver did with this picture, the next one starts over.
            self.need_idr = true;
            self.gop.reset();
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
        let picture = self.gop.next(idr);
        self.submit(&picture, res).context("encoding the frame")?;
        ensure!(!self.scratch.is_empty(), "the driver made no bitstream");
        let key = bits::has_idr(&self.scratch);
        ensure!(
            key == picture.idr,
            "asked for {} picture, the driver made {}",
            if picture.idr { "an IDR" } else { "a P" },
            if key { "an IDR" } else { "a P" }
        );
        if key {
            let fixed = bits::fix_headers(&self.scratch, &self.layout.sps, &self.layout.pps, out);
            if fixed.sps_kept > 0 {
                warn!("the driver's SPS uses features we don't parse; its VUI is left as it is");
            }
        } else {
            out.clear();
            out.extend_from_slice(&self.scratch);
        }
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
        let layout = make_layout(self.layout.profile, &params, &self.tuning)?;
        self.drop_imports();
        self.drop_resources();
        self.params = params;
        self.layout = layout;
        self.need_idr = true;
        self.gop.reset();
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
        self.layout.with_fps(&self.params);
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

/// What a self-test run found.
#[derive(Debug)]
pub struct SelfTest {
    pub frames: u32,
    pub keyframes: u32,
    pub bytes: u64,
    pub encode_ms_avg: f64,
    /// The NAL unit types of the first picture (SPS, PPS, IDR slice).
    pub first_nals: Vec<u8>,
    /// How the encoder was set up (entrypoint, rate control, headers, knobs).
    pub setup: String,
}

/// Encodes `frames` generated pictures on `render_node` and checks the output
/// is H.264 as the players expect: an IDR with SPS and PPS first, P pictures
/// after it, another IDR on request, and the stream still coding after a
/// bitrate change. For `CHA_ENCODE_TEST=<frames> cha-streamer --probe-device
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
    let (width, height) = (1280u32, 720u32);
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

    let mut encoder = Vaapi::new(
        render_node,
        Params {
            codec: Codec::H264,
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
    // CHA_ENCODE_TEST_DUMP=/path/out.h264 writes the Annex-B stream there and
    // the generated pictures to /path/out.src: raw, 1280×720, 4 bytes a pixel
    // in the order B, G, R, unused (ffmpeg: `-f rawvideo -pix_fmt bgr0
    // -video_size 1280x720 -framerate 60 -i out.src`).
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
        // (A rate change would muddy a comparison: with a dump the rate holds.)
        if n == change_bitrate_at && dump.is_none() {
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
        let unit = h264::check_access_unit(&out)
            .with_context(|| format!("frame {n} ({} bytes) isn't well-formed H.264", out.len()))?;
        ensure!(
            unit.idr == key,
            "frame {n}: the key flag says {key}, the bitstream {}",
            unit.idr
        );
        let expect_key = n == 0 || want_key;
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
                unit.size == Some((width, height)),
                "frame {n}: the SPS says {:?}, not {width}×{height}",
                unit.size
            );
        }
        bytes += out.len() as u64;
    }
    ensure!(
        encoder.next_index() == u64::from(frames),
        "the frame counter is off"
    );
    Ok(SelfTest {
        frames,
        keyframes,
        bytes,
        encode_ms_avg: total.as_secs_f64() * 1000.0 / f64::from(frames),
        first_nals,
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
        assert_eq!(packed_mode(all, None), Packed::Headers);
        assert_eq!(packed_mode(seq_pic, None), Packed::Headers);
        // Only the sequence header: the driver writes its own.
        assert_eq!(packed_mode(ffi::PACKED_HEADER_SEQUENCE, None), Packed::None);
        assert_eq!(packed_mode(0, None), Packed::None);
        // The knobs.
        assert_eq!(packed_mode(all, Some("off")), Packed::None);
        assert_eq!(packed_mode(all, Some("slice")), Packed::HeadersAndSlice);
        assert_eq!(packed_mode(seq_pic, Some("slice")), Packed::Headers);
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

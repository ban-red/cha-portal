//! x264, loaded at run time: the CPU device's H.264 encoder.
//!
//! x264 is GPL-2.0-or-later, compatible with this project's AGPL-3.0. We don't
//! link it: the library is opened with `dlopen` when a CPU device starts, and
//! its settings go in as option strings (`x264_param_parse`), so none of
//! x264's large, build-specific parameter struct is mirrored here. Only
//! the picture and NAL structs' stable prefixes are.
//!
//! Set up for the lowest latency a software encoder gets: `zerolatency` (no
//! B-frames, no lookahead, sliced threads: every core works on one frame
//! rather than on several at once), no scene-cut keyframes (an infinite GOP:
//! keyframes only when asked), and a one-frame VBV, so a frame never takes
//! more than its share of the bitrate.

use std::ffi::{CString, c_char, c_int, c_void};
use std::ptr;
use std::sync::OnceLock;
use std::time::Instant;

use anyhow::{Context, Result, anyhow, bail, ensure};
use cha_nvenc::{Codec, Timings};
use tracing::{info, warn};

use super::dl::Library;
use super::{Params, VideoEncoder};
use crate::media::Frame;

/// Pixels per second of 1080p60: above it, a CPU struggles to keep up.
const COMFORTABLE_PIXEL_RATE: u64 = 1920 * 1080 * 60;
/// Slices cost bits (no prediction across them) and per-slice work: more
/// than this many threads isn't worth it.
const MAX_THREADS: usize = 16;

type ParamDefaultPreset = unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char) -> c_int;
type ParamParse = unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char) -> c_int;
type ParamApplyProfile = unsafe extern "C" fn(*mut c_void, *const c_char) -> c_int;
type PictureInit = unsafe extern "C" fn(*mut Picture);
type EncoderOpen = unsafe extern "C" fn(*mut c_void) -> *mut c_void;
type EncoderEncode = unsafe extern "C" fn(
    *mut c_void,
    *mut *mut Nal,
    *mut c_int,
    *mut Picture,
    *mut Picture,
) -> c_int;
type EncoderClose = unsafe extern "C" fn(*mut c_void);
type EncoderParameters = unsafe extern "C" fn(*mut c_void, *mut c_void);
type EncoderReconfig = unsafe extern "C" fn(*mut c_void, *mut c_void) -> c_int;

struct Lib {
    lib: Library,
    build: u32,
    param_default_preset: ParamDefaultPreset,
    param_parse: ParamParse,
    param_apply_profile: ParamApplyProfile,
    picture_init: PictureInit,
    encoder_open: EncoderOpen,
    encoder_encode: EncoderEncode,
    encoder_close: EncoderClose,
    encoder_parameters: EncoderParameters,
    encoder_reconfig: EncoderReconfig,
}

fn lib() -> Result<&'static Lib> {
    static LIB: OnceLock<Result<Lib, String>> = OnceLock::new();
    LIB.get_or_init(|| load().map_err(|e| format!("{e:#}")))
        .as_ref()
        .map_err(|e| anyhow!("{e}"))
}

/// Whether x264 loads here.
pub fn available() -> Result<()> {
    lib().map(|_| ())
}

fn load() -> Result<Lib> {
    let lib = Library::open(&[
        "libx264.so.164",
        "libx264.so.165",
        "libx264.so.163",
        "libx264.so.162",
        "libx264.so.161",
        "libx264.so.160",
        "libx264.so",
    ])
    .context("x264 isn't installed (libx264)")?;
    // `x264_encoder_open` is the one versioned symbol (`_164`): it ties a
    // program to the build whose structs it was written for. Ours reads only
    // the stable prefixes below, so any build in the range is fine.
    let (build, encoder_open) = (140..=220)
        .rev()
        .find_map(|build| {
            let name = CString::new(format!("x264_encoder_open_{build}")).ok()?;
            // SAFETY: x264.h's signature.
            let open = unsafe { lib.symbol::<EncoderOpen>(&name) }.ok()?;
            Some((build, open))
        })
        .with_context(|| format!("{} has no x264_encoder_open_<build>", lib.name()))?;
    // SAFETY: the signatures are x264.h's.
    unsafe {
        Ok(Lib {
            param_default_preset: lib.symbol(c"x264_param_default_preset")?,
            param_parse: lib.symbol(c"x264_param_parse")?,
            param_apply_profile: lib.symbol(c"x264_param_apply_profile")?,
            picture_init: lib.symbol(c"x264_picture_init")?,
            encoder_encode: lib.symbol(c"x264_encoder_encode")?,
            encoder_close: lib.symbol(c"x264_encoder_close")?,
            encoder_parameters: lib.symbol(c"x264_encoder_parameters")?,
            encoder_reconfig: lib.symbol(c"x264_encoder_reconfig")?,
            encoder_open,
            build,
            lib,
        })
    }
}

/// The build of x264 that loaded (`0.<build>`), for the logs.
pub fn describe() -> Result<String> {
    let lib = lib()?;
    Ok(format!("{} (build {})", lib.lib.name(), lib.build))
}

/// `x264_image_t`: stable since 2010.
#[repr(C)]
struct Image {
    csp: c_int,
    planes: c_int,
    stride: [c_int; 4],
    plane: [*mut u8; 4],
}

/// `x264_picture_t`'s prefix; the rest (properties, HRD timing, SEI, opaque)
/// is x264's, zeroed by `x264_picture_init`, and the tail here is room for it.
#[repr(C)]
struct Picture {
    kind: c_int,
    qpplus1: c_int,
    pic_struct: c_int,
    keyframe: c_int,
    pts: i64,
    dts: i64,
    param: *mut c_void,
    img: Image,
    _rest: [u64; 256],
}

/// `x264_nal_t`.
#[repr(C)]
struct Nal {
    ref_idc: c_int,
    kind: c_int,
    long_startcode: c_int,
    first_mb: c_int,
    last_mb: c_int,
    payload_size: c_int,
    payload: *mut u8,
}

const CSP_I420: c_int = 2;
const TYPE_AUTO: c_int = 0;
const TYPE_IDR: c_int = 1;

impl Picture {
    fn new(lib: &Lib) -> Box<Self> {
        // SAFETY: all-zero bytes are a valid Picture (nulls and zeros); x264
        // then fills in its defaults.
        let mut pic: Box<Self> = Box::new(unsafe { std::mem::zeroed() });
        // SAFETY: a live, large enough picture.
        unsafe { (lib.picture_init)(&mut *pic) };
        pic
    }
}

/// x264's `x264_param_t` is ~1.5 kB and differs by build; this is room for it
/// and the only thing we do with it is hand it back.
#[repr(C, align(16))]
struct ParamBuf([u8; 16 * 1024]);

impl ParamBuf {
    fn new() -> Box<Self> {
        Box::new(Self([0; 16 * 1024]))
    }

    fn as_ptr(&mut self) -> *mut c_void {
        self.0.as_mut_ptr().cast()
    }
}

/// What the encoder is configured with: the preset and tune, the profile
/// cap, and `name=value` options in the order they are applied.
#[derive(Debug, PartialEq, Eq)]
pub struct Options {
    pub preset: &'static str,
    pub tune: &'static str,
    pub profile: &'static str,
    /// The picture size: not an option string (`x264_param_parse` has none for
    /// it), so it is written into the struct's prefix.
    pub size: (u32, u32),
    pub pairs: Vec<(&'static str, String)>,
}

/// The preset for a pixel rate: `superfast` up to 1080p60, `ultrafast` (no
/// CABAC, no 8×8 transform: bigger frames for much less CPU) above it.
/// `CHA_X264_PRESET` overrides it, for measuring.
fn preset_for(width: u32, height: u32, fps: u32) -> &'static str {
    if let Ok(name) = std::env::var("CHA_X264_PRESET") {
        for known in [
            "ultrafast",
            "superfast",
            "veryfast",
            "faster",
            "fast",
            "medium",
        ] {
            if name == known {
                return known;
            }
        }
        warn!("CHA_X264_PRESET={name} isn't a preset; using the default");
    }
    if u64::from(width) * u64::from(height) * u64::from(fps) <= COMFORTABLE_PIXEL_RATE {
        "superfast"
    } else {
        "ultrafast"
    }
}

/// How many threads x264 gets: the cores this container may use.
fn default_threads() -> usize {
    std::thread::available_parallelism()
        .map_or(4, |n| n.get())
        .clamp(1, MAX_THREADS)
}

/// The settings for `params` on `threads` threads. A pure function, so the
/// choices are tested without x264.
pub fn options(params: &Params, threads: usize) -> Options {
    let fps = params.fps.max(1);
    let kbps = (params.bitrate_bps / 1000).max(1);
    // One frame of bits: nothing queues behind a big frame. x264 won't take
    // less than that.
    let vbv_kbit = (params.bitrate_bps / fps / 1000).max(1);
    let pairs = vec![
        ("fps", format!("{fps}/1")),
        ("threads", threads.to_string()),
        // Keyframes (IDR) only when asked: no GOP, no scene-cut ones.
        ("keyint", "infinite".into()),
        ("scenecut", "0".into()),
        ("open-gop", "0".into()),
        ("intra-refresh", "0".into()),
        ("bitrate", kbps.to_string()),
        ("vbv-maxrate", kbps.to_string()),
        ("vbv-bufsize", vbv_kbit.to_string()),
        // Every keyframe carries its SPS/PPS, as NVENC's do: a viewer can
        // join at any one.
        ("repeat-headers", "1".into()),
        ("annexb", "1".into()),
        ("aud", "0".into()),
        // BT.709, limited range, as the NVENC path signals.
        ("colorprim", "bt709".into()),
        ("transfer", "bt709".into()),
        ("colormatrix", "bt709".into()),
        ("fullrange", "off".into()),
    ];
    Options {
        preset: preset_for(params.width, params.height, fps),
        tune: "zerolatency",
        profile: "high",
        size: (params.width, params.height),
        pairs,
    }
}

/// One x264 session. It keeps its frame counter across the reopening a size
/// or frame rate change needs, so frame indices stay unique.
pub struct X264 {
    lib: &'static Lib,
    params: Params,
    threads: usize,
    handle: *mut c_void,
    /// The next frame's index (its pts).
    index: u64,
    /// Just reopened: the first frame is a keyframe, whatever is asked.
    fresh: bool,
    timings: Timings,
}

// SAFETY: a session is used by one thread at a time (its encoder thread).
unsafe impl Send for X264 {}

impl X264 {
    pub fn new(params: Params) -> Result<Self> {
        ensure!(
            params.codec == Codec::H264,
            "x264 makes H.264 only, not {}",
            params.codec.name()
        );
        let lib = lib()?;
        let threads = default_threads();
        let handle = open(lib, &params, threads)?;
        Ok(Self {
            lib,
            params,
            threads,
            handle,
            index: 0,
            fresh: true,
            timings: Timings::default(),
        })
    }

    /// A new session with the current settings (x264 changes size and frame
    /// rate only that way); it starts with a keyframe.
    fn reopen(&mut self) -> Result<()> {
        let handle = open(self.lib, &self.params, self.threads)?;
        // SAFETY: the old session, closed once.
        unsafe { (self.lib.encoder_close)(self.handle) };
        self.handle = handle;
        self.fresh = true;
        Ok(())
    }
}

fn open(lib: &'static Lib, params: &Params, threads: usize) -> Result<*mut c_void> {
    let pixel_rate = u64::from(params.width) * u64::from(params.height) * u64::from(params.fps);
    if pixel_rate > COMFORTABLE_PIXEL_RATE {
        warn!(
            width = params.width,
            height = params.height,
            fps = params.fps,
            "software encoding above 1080p60: expect it to use most of the CPU, and to \
             fall behind on a busy picture (the CPU device is for desktops at modest sizes)"
        );
    }
    let options = options(params, threads);
    let mut buf = ParamBuf::new();
    let param = buf.as_ptr();
    let cstr = |s: &str| CString::new(s).expect("option text has no NUL");
    // SAFETY: a large enough, aligned buffer; valid C strings.
    unsafe {
        let status = (lib.param_default_preset)(
            param,
            cstr(options.preset).as_ptr(),
            cstr(options.tune).as_ptr(),
        );
        ensure!(
            status == 0,
            "x264 has no preset {} / tune {}",
            options.preset,
            options.tune
        );
        // `i_width`, `i_height` and `i_csp` follow six ints and a uint in
        // `x264_param_t`, and have since the first build: at 28, 32 and 36
        // bytes. A preset leaves the size 0 and the colour space I420; if
        // that isn't what's there, this isn't the struct we think it is.
        let fields = param.cast::<c_int>();
        ensure!(
            (*fields.add(7), *fields.add(8), *fields.add(9)) == (0, 0, CSP_I420),
            "this x264's parameter struct isn't the one we know"
        );
        *fields.add(7) = options.size.0 as c_int;
        *fields.add(8) = options.size.1 as c_int;
        for (name, value) in &options.pairs {
            let status = (lib.param_parse)(param, cstr(name).as_ptr(), cstr(value).as_ptr());
            ensure!(status == 0, "x264 refused {name}={value} (status {status})");
        }
        ensure!(
            (lib.param_apply_profile)(param, cstr(options.profile).as_ptr()) == 0,
            "x264 has no profile {}",
            options.profile
        );
        let handle = (lib.encoder_open)(param);
        ensure!(!handle.is_null(), "x264 couldn't open an encoder");
        Ok(handle)
    }
}

impl VideoEncoder for X264 {
    fn params(&self) -> &Params {
        &self.params
    }

    fn encode(&mut self, frame: &Frame, keyframe: bool, out: &mut Vec<u8>) -> Result<bool> {
        let picture = frame
            .slot
            .i420()
            .context("x264 takes frames the compositor read back to memory")?;
        ensure!(
            (picture.width as u32, picture.height as u32)
                == (self.params.width, self.params.height),
            "a {}×{} frame for a {}×{} encoder",
            picture.width,
            picture.height,
            self.params.width,
            self.params.height
        );
        let started = Instant::now();
        let mut pic_in = Picture::new(self.lib);
        let mut pic_out = Picture::new(self.lib);
        pic_in.kind = if keyframe || self.fresh {
            TYPE_IDR
        } else {
            TYPE_AUTO
        };
        pic_in.pts = self.index as i64;
        pic_in.img = Image {
            csp: CSP_I420,
            planes: 3,
            stride: [
                picture.width as c_int,
                (picture.width / 2) as c_int,
                (picture.width / 2) as c_int,
                0,
            ],
            // x264 reads the planes only (the pointers aren't const in C).
            plane: [
                picture.y.as_ptr().cast_mut(),
                picture.u.as_ptr().cast_mut(),
                picture.v.as_ptr().cast_mut(),
                ptr::null_mut(),
            ],
        };
        let mut nals: *mut Nal = ptr::null_mut();
        let mut count: c_int = 0;
        // SAFETY: a live session; the planes outlive the call (the frame),
        // and x264 copies what it keeps before it returns.
        let size = unsafe {
            (self.lib.encoder_encode)(
                self.handle,
                &mut nals,
                &mut count,
                &mut *pic_in,
                &mut *pic_out,
            )
        };
        if size < 0 {
            bail!("x264 failed to encode a frame (status {size})");
        }
        if size == 0 || nals.is_null() || count == 0 {
            bail!("x264 held the frame back (zero latency expected)");
        }
        // The NALs are contiguous: the first one's payload runs for `size`.
        // SAFETY: x264 owns that memory until the next call; read once here.
        let data = unsafe { std::slice::from_raw_parts((*nals).payload, size as usize) };
        out.clear();
        out.extend_from_slice(data);
        self.index += 1;
        self.fresh = false;
        self.timings = Timings {
            submit: started.elapsed(),
            ..Timings::default()
        };
        Ok(pic_out.keyframe != 0)
    }

    fn resize(&mut self, width: u32, height: u32) -> Result<()> {
        self.params.width = width;
        self.params.height = height;
        self.reopen()
    }

    fn set_bitrate(&mut self, bitrate_bps: u32) -> Result<()> {
        let was = self.params.bitrate_bps;
        self.params.bitrate_bps = bitrate_bps;
        let mut buf = ParamBuf::new();
        let param = buf.as_ptr();
        let options = options(&self.params, self.threads);
        // SAFETY: a live session and a large enough buffer; the session's own
        // settings are read back, three options changed and handed in again.
        let status = unsafe {
            (self.lib.encoder_parameters)(self.handle, param);
            for name in ["bitrate", "vbv-maxrate", "vbv-bufsize"] {
                let value = options
                    .pairs
                    .iter()
                    .find(|(n, _)| *n == name)
                    .map(|(_, v)| v.as_str());
                let (name, value) = (CString::new(name)?, CString::new(value.unwrap_or("1"))?);
                let status = (self.lib.param_parse)(param, name.as_ptr(), value.as_ptr());
                ensure!(status == 0, "x264 refused {name:?}={value:?}");
            }
            (self.lib.encoder_reconfig)(self.handle, param)
        };
        if status < 0 {
            self.params.bitrate_bps = was;
            bail!("x264 couldn't change the bitrate (status {status})");
        }
        Ok(())
    }

    fn set_frame_rate(&mut self, fps: u32, bitrate_bps: u32) -> Result<()> {
        // x264 takes a frame rate at open: a new session, and a keyframe.
        info!(fps, "x264 reopens for the new frame rate");
        self.params.fps = fps;
        self.params.bitrate_bps = bitrate_bps;
        self.reopen()
    }

    fn next_index(&self) -> u64 {
        self.index
    }

    fn last_timings(&self) -> Timings {
        self.timings
    }
}

impl Drop for X264 {
    fn drop(&mut self) {
        // SAFETY: the open session, closed once.
        unsafe { (self.lib.encoder_close)(self.handle) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(width: u32, height: u32, fps: u32, bitrate_bps: u32) -> Params {
        Params {
            codec: Codec::H264,
            width,
            height,
            fps,
            bitrate_bps,
        }
    }

    fn get<'a>(options: &'a Options, name: &str) -> &'a str {
        &options.pairs.iter().find(|(n, _)| *n == name).unwrap().1
    }

    #[test]
    fn low_latency_settings() {
        let o = options(&params(1920, 1080, 60, 40_000_000), 8);
        assert_eq!(o.tune, "zerolatency");
        assert_eq!(get(&o, "keyint"), "infinite");
        assert_eq!(get(&o, "scenecut"), "0");
        assert_eq!(get(&o, "intra-refresh"), "0");
        assert_eq!(get(&o, "repeat-headers"), "1");
        assert_eq!(o.size, (1920, 1080));
        assert_eq!(get(&o, "fps"), "60/1");
        assert_eq!(get(&o, "threads"), "8");
        assert_eq!(get(&o, "fullrange"), "off");
        assert_eq!(get(&o, "colormatrix"), "bt709");
    }

    #[test]
    fn the_vbv_is_one_frame_of_bits() {
        let o = options(&params(1920, 1080, 60, 24_000_000), 4);
        assert_eq!(get(&o, "bitrate"), "24000");
        assert_eq!(get(&o, "vbv-maxrate"), "24000");
        // 24 Mbit/s at 60 fps: 400 kbit a frame.
        assert_eq!(get(&o, "vbv-bufsize"), "400");
        let o = options(&params(1920, 1080, 120, 24_000_000), 4);
        assert_eq!(get(&o, "vbv-bufsize"), "200");
        // Never zero.
        let o = options(&params(320, 240, 60, 1000), 1);
        assert_eq!(get(&o, "vbv-bufsize"), "1");
    }

    #[test]
    fn the_preset_follows_the_pixel_rate() {
        // (The override is for measuring and isn't set in tests.)
        assert_eq!(options(&params(1920, 1080, 60, 1), 1).preset, "superfast");
        assert_eq!(options(&params(1280, 720, 120, 1), 1).preset, "superfast");
        assert_eq!(options(&params(2560, 1440, 60, 1), 1).preset, "ultrafast");
        assert_eq!(options(&params(1920, 1080, 120, 1), 1).preset, "ultrafast");
    }

    #[test]
    fn only_h264() {
        let mut p = params(1280, 720, 60, 1_000_000);
        p.codec = Codec::Hevc;
        assert!(
            X264::new(p)
                .err()
                .unwrap()
                .to_string()
                .contains("H.264 only")
        );
    }
}

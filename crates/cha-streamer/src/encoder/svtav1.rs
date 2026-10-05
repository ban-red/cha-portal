//! SVT-AV1, loaded at run time: the CPU device's AV1 encoder.
//!
//! SVT-AV1 is BSD-3-Clause-Clear with the AOM patent licence, compatible with
//! this project's AGPL-3.0. We don't link it: the library is opened with
//! `dlopen` when a CPU device starts, and its settings go in by name
//! (`svt_av1_enc_parse_parameter`), so none of `EbSvtAv1EncConfiguration`'s
//! layout (it changes between releases, with `#if`s on the version) is
//! mirrored: the struct is room that the library fills with its defaults and
//! reads back. What is mirrored are the picture and packet headers, which
//! have been stable since 1.5; the layout is that of v2.3.0 (Ubuntu 26.04's
//! `libsvtav1enc2`), and a library older than 2.x is refused.
//!
//! Set up for real time: the low-delay prediction structure (each picture
//! refers back only, one packet out for each picture in), no lookahead, CBR
//! with a buffer of a few frames at most, an infinite GOP (keyframes only
//! when asked), screen content mode forced on (palette and intra block
//! copy), and the fastest preset (11).
//!
//! The library has no live bitrate change before 3.x: a new target takes
//! effect on a keyframe (a rate-change event in the picture's private data).
//! So a bitrate change waits for the next keyframe unless it is large
//! enough to be worth one (see [`rate_needs_keyframe`]). A new size or frame
//! rate is a new session.

use std::ffi::{CStr, CString, c_char, c_int, c_void};
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
/// Room for `EbSvtAv1EncConfiguration` (624 bytes in 2.3, a little more in
/// later releases).
const CONFIG_ROOM: usize = 4096;

/// The sonames to try, newest first. (`libSvtAv1Enc.so.2` is 2.x, Ubuntu
/// 26.04's.)
const SONAMES: &[&str] = &[
    "libSvtAv1Enc.so.4",
    "libSvtAv1Enc.so.3",
    "libSvtAv1Enc.so.2",
    "libSvtAv1Enc.so",
];

const EB_ERROR_NONE: c_int = 0;
const KEY_PICTURE: u32 = 3;
const INVALID_PICTURE: u32 = 0xFF;
const BUFFERFLAG_EOS: u32 = 1;
/// `PrivDataType::RATE_CHANGE_EVENT`.
const RATE_CHANGE_EVENT: u32 = 4;

type InitHandle = unsafe extern "C" fn(*mut *mut c_void, *mut c_void, *mut c_void) -> c_int;
type SetParameter = unsafe extern "C" fn(*mut c_void, *mut c_void) -> c_int;
type ParseParameter = unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char) -> c_int;
type Init = unsafe extern "C" fn(*mut c_void) -> c_int;
type SendPicture = unsafe extern "C" fn(*mut c_void, *mut BufferHeader) -> c_int;
type GetPacket = unsafe extern "C" fn(*mut c_void, *mut *mut BufferHeader, u8) -> c_int;
type ReleaseOutBuffer = unsafe extern "C" fn(*mut *mut BufferHeader);
type GetVersion = unsafe extern "C" fn() -> *const c_char;

struct Lib {
    lib: Library,
    version: String,
    init_handle: InitHandle,
    set_parameter: SetParameter,
    parse_parameter: ParseParameter,
    init: Init,
    send_picture: SendPicture,
    get_packet: GetPacket,
    release_out_buffer: ReleaseOutBuffer,
    deinit: Init,
    deinit_handle: Init,
}

fn lib() -> Result<&'static Lib> {
    static LIB: OnceLock<Result<Lib, String>> = OnceLock::new();
    LIB.get_or_init(|| load().map_err(|e| format!("{e:#}")))
        .as_ref()
        .map_err(|e| anyhow!("{e}"))
}

/// Whether SVT-AV1 loads here.
pub fn available() -> Result<()> {
    lib().map(|_| ())
}

fn load() -> Result<Lib> {
    let lib = Library::open(SONAMES).context("SVT-AV1 isn't installed (libsvtav1enc)")?;
    // SAFETY: the signatures are EbSvtAv1Enc.h's.
    let get_version: GetVersion = unsafe { lib.symbol(c"svt_av1_get_version")? };
    // SAFETY: a static C string.
    let version = unsafe { CStr::from_ptr(get_version()) }
        .to_string_lossy()
        .into_owned();
    ensure!(
        major_of(&version).is_some_and(|major| major >= 2),
        "{} is SVT-AV1 {version}; 2.x or later is needed",
        lib.name()
    );
    // SAFETY: as above.
    unsafe {
        Ok(Lib {
            init_handle: lib.symbol(c"svt_av1_enc_init_handle")?,
            set_parameter: lib.symbol(c"svt_av1_enc_set_parameter")?,
            parse_parameter: lib.symbol(c"svt_av1_enc_parse_parameter")?,
            init: lib.symbol(c"svt_av1_enc_init")?,
            send_picture: lib.symbol(c"svt_av1_enc_send_picture")?,
            get_packet: lib.symbol(c"svt_av1_enc_get_packet")?,
            release_out_buffer: lib.symbol(c"svt_av1_enc_release_out_buffer")?,
            deinit: lib.symbol(c"svt_av1_enc_deinit")?,
            deinit_handle: lib.symbol(c"svt_av1_enc_deinit_handle")?,
            version,
            lib,
        })
    }
}

/// `v2.3.0` (or `2.3.0-5-gabc`) to 2.
fn major_of(version: &str) -> Option<u32> {
    version
        .trim_start_matches('v')
        .split(['.', '-'])
        .next()?
        .parse()
        .ok()
}

/// The library that loaded and its version, for the logs.
pub fn describe() -> Result<String> {
    let lib = lib()?;
    Ok(format!("{} (SVT-AV1 {})", lib.lib.name(), lib.version))
}

/// `EbBufferHeaderType`: the picture going in and the packet coming out.
#[repr(C)]
struct BufferHeader {
    size: u32,
    p_buffer: *mut u8,
    n_filled_len: u32,
    n_alloc_len: u32,
    p_app_private: *mut c_void,
    wrapper_ptr: *mut c_void,
    n_tick_count: u32,
    dts: i64,
    pts: i64,
    qp: u32,
    pic_type: u32,
    luma_sse: u64,
    cr_sse: u64,
    cb_sse: u64,
    flags: u32,
    luma_ssim: f64,
    cr_ssim: f64,
    cb_ssim: f64,
    metadata: *mut c_void,
}

/// `EbSvtIOFormat`: what an input buffer's `p_buffer` points at. (The
/// strides are in pixels, and the Cr stride comes before Cb's.)
#[repr(C)]
struct IoFormat {
    luma: *const u8,
    cb: *const u8,
    cr: *const u8,
    y_stride: u32,
    cr_stride: u32,
    cb_stride: u32,
    width: u32,
    height: u32,
    org_x: u32,
    org_y: u32,
    color_format: u32,
    bit_depth: u32,
}

/// `EbPrivDataNode`, and `SvtAv1RateInfo` for what it points at.
#[repr(C)]
struct PrivDataNode {
    kind: u32,
    data: *mut c_void,
    size: u32,
    next: *mut PrivDataNode,
}

#[repr(C)]
struct RateInfo {
    /// 0: leave the quantizer alone.
    seq_qp: u32,
    target_bit_rate: u32,
}

const YUV420: u32 = 1;
const EIGHT_BIT: u32 = 8;

/// What the encoder is configured with: the preset and `name=value` options
/// in the order they are applied.
#[derive(Debug, PartialEq, Eq)]
pub struct Options {
    pub preset: u8,
    pub pairs: Vec<(&'static str, String)>,
}

/// The preset: 11, the fastest this library has in low delay (12 and 13 are
/// mapped to it, with a warning, in 2.3; 10 costs about twice as much on a
/// 1080p desktop picture, measured on an i7-13700K). `CHA_SVTAV1_PRESET`
/// overrides it, for measuring (a later release may have faster ones).
fn preset() -> u8 {
    if let Ok(text) = std::env::var("CHA_SVTAV1_PRESET") {
        match text.parse::<u8>() {
            Ok(preset) if (6..=13).contains(&preset) => return preset,
            _ => warn!("CHA_SVTAV1_PRESET={text} isn't a preset (6 to 13); using the default"),
        }
    }
    11
}

/// The settings for `params`. A pure function, so the choices are tested
/// without the library. (Threads aren't among them: left alone, the library
/// picks its level of parallelism from the cores the process may run on, 6
/// of 6 on 16.)
pub fn options(params: &Params) -> Options {
    let fps = params.fps.max(1);
    let kbps = (params.bitrate_bps / 1000).max(1);
    // The rate control's buffer, in time: a frame's worth, and never below
    // the library's minimum (20 ms). The most it holds is twice that.
    let frame_ms = 1000u32.div_ceil(fps).max(20);
    let pairs = vec![
        ("width", params.width.to_string()),
        ("height", params.height.to_string()),
        ("fps-num", fps.to_string()),
        ("fps-denom", "1".into()),
        ("input-depth", "8".into()),
        ("profile", "0".into()),
        // Level 5.1, main tier: seq_level_idx 13, the `av01.0.13M.08` the
        // stream is announced as (NVENC's is the same); 5.1 holds 4K60.
        ("level", "51".into()),
        ("tier", "0".into()),
        // Low delay, no lookahead, CBR; a picture in, a packet out.
        ("pred-struct", "1".into()),
        ("lookahead", "0".into()),
        ("rc", "2".into()),
        ("tbr", kbps.to_string()),
        ("buf-initial-sz", frame_ms.to_string()),
        ("buf-optimal-sz", frame_ms.to_string()),
        ("buf-sz", (frame_ms * 2).to_string()),
        // Keyframes (IDR, self-contained) only when asked: no GOP, no
        // scene-cut ones.
        ("keyint", "-1".into()),
        ("irefresh-type", "2".into()),
        ("scd", "0".into()),
        // Text, UI and flat colour: palette and intra block copy on.
        ("scm", "1".into()),
        // No altref frames to make (low delay has none; it is for the work
        // not done).
        ("enable-tf", "0".into()),
        ("enable-overlays", "0".into()),
        // BT.709, limited range, as the NVENC path signals.
        ("color-primaries", "1".into()),
        ("transfer-characteristics", "1".into()),
        ("matrix-coefficients", "1".into()),
        ("color-range", "0".into()),
    ];
    Options {
        preset: preset(),
        pairs,
    }
}

/// Whether a new target is far enough from the one the library runs at to be
/// worth a keyframe now (it takes effect on one): well below it (shed load) or
/// well above (stop wasting the path). Smaller moves wait for the next one.
pub fn rate_needs_keyframe(active_bps: u32, target_bps: u32) -> bool {
    let (active, target) = (u64::from(active_bps), u64::from(target_bps));
    target * 100 < active * 80 || target * 100 > active * 150
}

/// A session of the library: the handle, closed on drop.
struct Instance {
    lib: &'static Lib,
    handle: *mut c_void,
    started: bool,
}

impl Instance {
    fn open(lib: &'static Lib, params: &Params) -> Result<Self> {
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
        let options = options(params);
        // Room for the library's configuration struct, which it fills in.
        let mut config = Box::new(Config([0; CONFIG_ROOM]));
        let cfg = config.0.as_mut_ptr().cast::<c_void>();
        let mut handle: *mut c_void = ptr::null_mut();
        // SAFETY: a large enough, aligned buffer; the library fills in its
        // defaults.
        let status = unsafe { (lib.init_handle)(&mut handle, ptr::null_mut(), cfg) };
        ensure!(
            status == EB_ERROR_NONE && !handle.is_null(),
            "SVT-AV1 couldn't make an encoder (status {status:#x})"
        );
        // From here a failure drops the instance, which frees the handle.
        let mut instance = Self {
            lib,
            handle,
            started: false,
        };
        let parse = |name: &str, value: &str| -> c_int {
            let (name, value) = (
                CString::new(name).expect("option text has no NUL"),
                CString::new(value).expect("option text has no NUL"),
            );
            // SAFETY: a live configuration and valid C strings.
            unsafe { (lib.parse_parameter)(cfg, name.as_ptr(), value.as_ptr()) }
        };
        let preset = options.preset.to_string();
        for (name, value) in std::iter::once(&("preset", preset)).chain(&options.pairs) {
            let status = parse(name, value);
            ensure!(
                status == EB_ERROR_NONE,
                "SVT-AV1 refused {name}={value} (status {status:#x})"
            );
        }
        // SAFETY: a live handle and its configuration.
        let status = unsafe { (lib.set_parameter)(handle, cfg) };
        ensure!(
            status == EB_ERROR_NONE,
            "SVT-AV1 refused the settings (status {status:#x})"
        );
        // SAFETY: a live handle.
        let status = unsafe { (lib.init)(handle) };
        ensure!(
            status == EB_ERROR_NONE,
            "SVT-AV1 couldn't start (status {status:#x})"
        );
        instance.started = true;
        Ok(instance)
    }
}

impl Instance {
    /// Tells the library no more pictures are coming and takes what it has
    /// left (it complains of a close without that).
    ///
    /// # Safety
    /// A started session.
    unsafe fn finish(&mut self) {
        let mut eos = BufferHeader {
            size: size_of::<BufferHeader>() as u32,
            p_buffer: ptr::null_mut(),
            n_filled_len: 0,
            n_alloc_len: 0,
            p_app_private: ptr::null_mut(),
            wrapper_ptr: ptr::null_mut(),
            n_tick_count: 0,
            dts: 0,
            pts: 0,
            qp: 0,
            pic_type: INVALID_PICTURE,
            luma_sse: 0,
            cr_sse: 0,
            cb_sse: 0,
            flags: BUFFERFLAG_EOS,
            luma_ssim: 0.0,
            cr_ssim: 0.0,
            cb_ssim: 0.0,
            metadata: ptr::null_mut(),
        };
        // SAFETY: the caller's started session.
        unsafe {
            if (self.lib.send_picture)(self.handle, &mut eos) != EB_ERROR_NONE {
                return;
            }
            // The packets that were still inside, then the one that says so.
            for _ in 0..64 {
                let mut packet: *mut BufferHeader = ptr::null_mut();
                if (self.lib.get_packet)(self.handle, &mut packet, 1) != EB_ERROR_NONE
                    || packet.is_null()
                {
                    return;
                }
                let last = (*packet).flags & BUFFERFLAG_EOS != 0;
                (self.lib.release_out_buffer)(&mut packet);
                if last {
                    return;
                }
            }
        }
    }
}

#[repr(C, align(16))]
struct Config([u8; CONFIG_ROOM]);

impl Drop for Instance {
    fn drop(&mut self) {
        // SAFETY: the open handle, closed once (the order the API asks for).
        unsafe {
            if self.started {
                self.finish();
                (self.lib.deinit)(self.handle);
            }
            (self.lib.deinit_handle)(self.handle);
        }
    }
}

/// One SVT-AV1 stream. It keeps its frame counter across the reopening a size
/// or frame rate change needs, so frame indices stay unique.
pub struct SvtAv1 {
    lib: &'static Lib,
    params: Params,
    instance: Instance,
    /// The next frame's index (its pts).
    index: u64,
    /// The target the library runs at (it changes on a keyframe).
    active_bps: u32,
    /// Frames since the last keyframe.
    since_key: u64,
    /// Just reopened: the first frame is a keyframe, whatever is asked.
    fresh: bool,
    timings: Timings,
}

// SAFETY: a session is used by one thread at a time (its encoder thread); the
// library's own threads are its business.
unsafe impl Send for SvtAv1 {}

impl SvtAv1 {
    pub fn new(params: Params) -> Result<Self> {
        ensure!(
            params.codec == Codec::Av1,
            "SVT-AV1 makes AV1 only, not {}",
            params.codec.name()
        );
        ensure!(
            params.width.is_multiple_of(2) && params.height.is_multiple_of(2),
            "4:2:0 needs even sizes, got {}×{}",
            params.width,
            params.height
        );
        let lib = lib()?;
        let instance = Instance::open(lib, &params)?;
        Ok(Self {
            lib,
            active_bps: params.bitrate_bps,
            params,
            instance,
            index: 0,
            since_key: 0,
            fresh: true,
            timings: Timings::default(),
        })
    }

    /// A new session with the current settings; it starts with a keyframe.
    fn reopen(&mut self) -> Result<()> {
        self.instance = Instance::open(self.lib, &self.params)?;
        self.active_bps = self.params.bitrate_bps;
        self.fresh = true;
        Ok(())
    }
}

impl VideoEncoder for SvtAv1 {
    fn params(&self) -> &Params {
        &self.params
    }

    fn encode(&mut self, frame: &Frame, keyframe: bool, out: &mut Vec<u8>) -> Result<bool> {
        let picture = frame
            .slot
            .i420()
            .context("SVT-AV1 takes frames the compositor read back to memory")?;
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
        // A keyframe asked for, or one that a big move of the target is
        // worth (not more than one a second for that).
        let key = keyframe
            || self.fresh
            || (self.since_key >= u64::from(self.params.fps)
                && rate_needs_keyframe(self.active_bps, self.params.bitrate_bps));
        let mut rate = RateInfo {
            seq_qp: 0,
            // Bits a second here (the option strings take kilobits).
            target_bit_rate: self.params.bitrate_bps.max(1000),
        };
        let mut node = PrivDataNode {
            kind: RATE_CHANGE_EVENT,
            data: (&mut rate as *mut RateInfo).cast(),
            size: size_of::<RateInfo>() as u32,
            next: ptr::null_mut(),
        };
        let rate_moved = key && self.active_bps != self.params.bitrate_bps;
        let width = picture.width as u32;
        let io = IoFormat {
            luma: picture.y.as_ptr(),
            cb: picture.u.as_ptr(),
            cr: picture.v.as_ptr(),
            y_stride: width,
            cr_stride: width / 2,
            cb_stride: width / 2,
            width,
            height: picture.height as u32,
            org_x: 0,
            org_y: 0,
            color_format: YUV420,
            bit_depth: EIGHT_BIT,
        };
        let bytes = (picture.y.len() + picture.u.len() + picture.v.len()) as u32;
        let mut header = BufferHeader {
            size: size_of::<BufferHeader>() as u32,
            p_buffer: (&io as *const IoFormat).cast_mut().cast(),
            n_filled_len: bytes,
            n_alloc_len: bytes,
            p_app_private: if rate_moved {
                (&mut node as *mut PrivDataNode).cast()
            } else {
                ptr::null_mut()
            },
            wrapper_ptr: ptr::null_mut(),
            n_tick_count: 0,
            dts: self.index as i64,
            pts: self.index as i64,
            qp: 0,
            pic_type: if key { KEY_PICTURE } else { INVALID_PICTURE },
            luma_sse: 0,
            cr_sse: 0,
            cb_sse: 0,
            flags: 0,
            luma_ssim: 0.0,
            cr_ssim: 0.0,
            cb_ssim: 0.0,
            metadata: ptr::null_mut(),
        };
        // SAFETY: a live session; the planes, the format and the rate event
        // outlive the call, and the library copies what it keeps before it
        // returns (the private data too).
        let status = unsafe { (self.lib.send_picture)(self.instance.handle, &mut header) };
        if status != EB_ERROR_NONE {
            bail!("SVT-AV1 refused a frame (status {status:#x})");
        }
        // Low delay: this waits for the packet of the picture just sent.
        let mut packet: *mut BufferHeader = ptr::null_mut();
        // SAFETY: a live session; `packet` is the library's to fill.
        let status = unsafe { (self.lib.get_packet)(self.instance.handle, &mut packet, 0) };
        if status != EB_ERROR_NONE || packet.is_null() {
            bail!("SVT-AV1 made no packet for a frame (status {status:#x})");
        }
        // SAFETY: the packet is the library's until released, below.
        let (data, kind) = unsafe {
            let p = &*packet;
            (
                std::slice::from_raw_parts(p.p_buffer, p.n_filled_len as usize),
                p.pic_type,
            )
        };
        out.clear();
        out.extend_from_slice(data);
        // SAFETY: released once, after the last read.
        unsafe { (self.lib.release_out_buffer)(&mut packet) };
        let made_key = kind == KEY_PICTURE;
        if rate_moved {
            self.active_bps = self.params.bitrate_bps;
        }
        self.since_key = if made_key { 0 } else { self.since_key + 1 };
        self.index += 1;
        self.fresh = false;
        self.timings = Timings {
            submit: started.elapsed(),
            ..Timings::default()
        };
        Ok(made_key)
    }

    fn resize(&mut self, width: u32, height: u32) -> Result<()> {
        self.params.width = width;
        self.params.height = height;
        self.reopen()
    }

    fn set_bitrate(&mut self, bitrate_bps: u32) -> Result<()> {
        // Applied on a keyframe (see the module's header): the next one, or
        // one `encode` makes for it if the move is big.
        self.params.bitrate_bps = bitrate_bps;
        Ok(())
    }

    fn set_frame_rate(&mut self, fps: u32, bitrate_bps: u32) -> Result<()> {
        // The frame rate is set at open: a new session, and a keyframe.
        info!(fps, "SVT-AV1 reopens for the new frame rate");
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

/// Just enough of an AV1 low-overhead OBU stream (AV1 spec 5.3 to 5.5) to
/// check what the encoder makes, for the tests.
#[cfg(test)]
pub(crate) mod obu {
    pub const SEQUENCE_HEADER: u8 = 1;
    pub const TEMPORAL_DELIMITER: u8 = 2;
    pub const FRAME_HEADER: u8 = 3;
    pub const FRAME: u8 = 6;

    /// The OBUs of `data` in order: (type, payload). Panics on a malformed
    /// stream: the tests want to know.
    pub fn parse(data: &[u8]) -> Vec<(u8, &[u8])> {
        let mut at = 0;
        let mut obus = Vec::new();
        while at < data.len() {
            let head = data[at];
            assert_eq!(head & 0x80, 0, "forbidden bit set at {at}");
            let (kind, extension, has_size) = ((head >> 3) & 15, head & 4 != 0, head & 2 != 0);
            assert!(has_size, "an OBU without a size field at {at}");
            at += 1 + usize::from(extension);
            let (mut size, mut shift) = (0usize, 0);
            loop {
                let byte = data[at];
                at += 1;
                size |= usize::from(byte & 0x7f) << shift;
                shift += 7;
                if byte & 0x80 == 0 {
                    break;
                }
            }
            obus.push((kind, &data[at..at + size]));
            at += size;
        }
        obus
    }

    /// A big-endian bit reader.
    struct Bits<'a>(&'a [u8], usize);

    impl Bits<'_> {
        fn take(&mut self, n: usize) -> u32 {
            (0..n).fold(0, |acc, _| {
                let bit = (self.0[self.1 / 8] >> (7 - self.1 % 8)) & 1;
                self.1 += 1;
                acc << 1 | u32::from(bit)
            })
        }
    }

    /// A sequence header's profile, level index and tier (of operating
    /// point 0).
    pub fn sequence(payload: &[u8]) -> (u32, u32, u32) {
        let mut bits = Bits(payload, 0);
        let profile = bits.take(3);
        let (_still, reduced) = (bits.take(1), bits.take(1));
        assert_eq!(reduced, 0, "a reduced still picture header");
        assert_eq!(
            bits.take(1),
            0,
            "timing info (not expected from this encoder)"
        );
        let _initial_display_delay_present = bits.take(1);
        let _operating_points = bits.take(5) + 1;
        let _idc = bits.take(12);
        let level = bits.take(5);
        let tier = if level > 7 { bits.take(1) } else { 0 };
        (profile, level, tier)
    }

    /// The first frame header's (frame_type, show_frame, show_existing_frame):
    /// frame type 0 is a key frame. (Of an OBU_FRAME or OBU_FRAME_HEADER
    /// payload.)
    pub fn frame(payload: &[u8]) -> (u32, u32, u32) {
        let mut bits = Bits(payload, 0);
        let show_existing = bits.take(1);
        if show_existing == 1 {
            return (u32::MAX, 1, 1);
        }
        let frame_type = bits.take(2);
        (frame_type, bits.take(1), 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(width: u32, height: u32, fps: u32, bitrate_bps: u32) -> Params {
        Params {
            codec: Codec::Av1,
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
    fn real_time_settings() {
        let o = options(&params(1920, 1080, 60, 40_000_000));
        // Low delay (1), no lookahead, CBR (2), screen content on.
        assert_eq!(get(&o, "pred-struct"), "1");
        assert_eq!(get(&o, "lookahead"), "0");
        assert_eq!(get(&o, "rc"), "2");
        assert_eq!(get(&o, "scm"), "1");
        // Keyframes only when asked, and each an IDR.
        assert_eq!(get(&o, "keyint"), "-1");
        assert_eq!(get(&o, "irefresh-type"), "2");
        assert_eq!(get(&o, "scd"), "0");
        // The picture, in kbit/s, BT.709 limited.
        assert_eq!(get(&o, "width"), "1920");
        assert_eq!(get(&o, "height"), "1080");
        assert_eq!(get(&o, "fps-num"), "60");
        assert_eq!(get(&o, "fps-denom"), "1");
        assert_eq!(get(&o, "tbr"), "40000");
        assert_eq!(get(&o, "matrix-coefficients"), "1");
        assert_eq!(get(&o, "color-range"), "0");
    }

    #[test]
    fn the_stream_is_level_5_1_main_tier() {
        // `av01.0.13M.08`: seq_level_idx 13 is level 5.1.
        let o = options(&params(2560, 1440, 60, 1));
        assert_eq!((get(&o, "level"), get(&o, "tier")), ("51", "0"));
        assert_eq!((get(&o, "profile"), get(&o, "input-depth")), ("0", "8"));
    }

    #[test]
    fn the_buffer_is_a_frame_or_two() {
        let o = options(&params(1920, 1080, 30, 1_000_000));
        // A 33.3 ms frame at 30 fps; twice that at most.
        assert_eq!(get(&o, "buf-optimal-sz"), "34");
        assert_eq!(get(&o, "buf-sz"), "68");
        // Faster than 50 fps hits the library's 20 ms floor.
        for fps in [60, 120, 240] {
            let o = options(&params(1920, 1080, fps, 1_000_000));
            assert_eq!(get(&o, "buf-initial-sz"), "20");
            assert_eq!(get(&o, "buf-sz"), "40");
        }
        // Never zero.
        let o = options(&params(320, 240, 60, 1));
        assert_eq!(get(&o, "tbr"), "1");
    }

    #[test]
    fn the_preset_is_the_fastest_for_low_delay() {
        // (The override is for measuring and isn't set in tests.)
        assert_eq!(options(&params(1920, 1080, 60, 1)).preset, 11);
        assert_eq!(options(&params(2560, 1440, 60, 1)).preset, 11);
    }

    #[test]
    fn a_big_move_of_the_target_earns_a_keyframe() {
        let active = 20_000_000;
        // A little either way waits for the next keyframe.
        assert!(!rate_needs_keyframe(active, 19_000_000));
        assert!(!rate_needs_keyframe(active, 17_000_000));
        assert!(!rate_needs_keyframe(active, 25_000_000));
        // A cut of a fifth or more, or a rise of half or more, doesn't.
        assert!(rate_needs_keyframe(active, 15_000_000));
        assert!(rate_needs_keyframe(active, 30_000_001));
        assert!(!rate_needs_keyframe(active, active));
    }

    #[test]
    fn versions_and_sonames() {
        assert_eq!(major_of("v2.3.0"), Some(2));
        assert_eq!(major_of("v3.1.2-14-gabc123-dirty"), Some(3));
        assert_eq!(major_of("1.8.0"), Some(1));
        assert_eq!(major_of("nonsense"), None);
        // Newest first, the unversioned name (a dev package) last.
        assert_eq!(SONAMES.first(), Some(&"libSvtAv1Enc.so.4"));
        assert!(SONAMES.contains(&"libSvtAv1Enc.so.2"));
        assert_eq!(SONAMES.last(), Some(&"libSvtAv1Enc.so"));
    }

    #[test]
    fn the_headers_match_the_c_layout() {
        // From EbSvtAv1.h and EbSvtAv1Enc.h at v2.3.0 (64-bit).
        assert_eq!(size_of::<BufferHeader>(), 136);
        assert_eq!(std::mem::offset_of!(BufferHeader, n_filled_len), 16);
        assert_eq!(std::mem::offset_of!(BufferHeader, p_app_private), 24);
        assert_eq!(std::mem::offset_of!(BufferHeader, pts), 56);
        assert_eq!(std::mem::offset_of!(BufferHeader, pic_type), 68);
        assert_eq!(std::mem::offset_of!(BufferHeader, flags), 96);
        assert_eq!(size_of::<IoFormat>(), 64);
        assert_eq!(std::mem::offset_of!(IoFormat, y_stride), 24);
        assert_eq!(std::mem::offset_of!(IoFormat, width), 36);
        assert_eq!(std::mem::offset_of!(IoFormat, color_format), 52);
        assert_eq!(std::mem::offset_of!(IoFormat, bit_depth), 56);
        assert_eq!(size_of::<PrivDataNode>(), 32);
        assert_eq!(size_of::<RateInfo>(), 8);
    }

    #[test]
    fn only_av1() {
        let mut p = params(1280, 720, 60, 1_000_000);
        p.codec = Codec::H264;
        assert!(
            SvtAv1::new(p)
                .err()
                .unwrap()
                .to_string()
                .contains("AV1 only")
        );
    }
}

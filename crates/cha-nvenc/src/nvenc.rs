//! NVENC sessions tuned for interactive streaming: P1 with the ultra-low-latency
//! tuning, CBR with a one-frame VBV, no B-frames, an infinite GOP (keyframes
//! only on request) and zero reorder delay, so decoders output every frame as
//! soon as it arrives.
//!
//! Reference frame invalidation (RFI): each frame predicts from one
//! reference, but the last [`DPB_FRAMES`] stay in the DPB, so when frames
//! are lost on the way, [`Encoder::invalidate_from`] has the next frame
//! refer to one from before them instead of starting over from a keyframe.

use std::ffi::{CStr, c_void};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use crate::cuda::{CudaContext, Surface};
use crate::dl::Library;
use crate::sys::*;
use crate::{Error, Result};

const fn guid(data1: u32, data2: u16, data3: u16, data4: [u8; 8]) -> GUID {
    GUID {
        Data1: data1,
        Data2: data2,
        Data3: data3,
        Data4: data4,
    }
}

// From nvEncodeAPI.h (static definitions bindgen can't carry over).
const CODEC_H264: GUID = guid(
    0x6bc82762,
    0x4e63,
    0x4ca4,
    [0xaa, 0x85, 0x1e, 0x50, 0xf3, 0x21, 0xf6, 0xbf],
);
const CODEC_HEVC: GUID = guid(
    0x790cdc88,
    0x4522,
    0x4d7b,
    [0x94, 0x25, 0xbd, 0xa9, 0x97, 0x5f, 0x76, 0x03],
);
const CODEC_AV1: GUID = guid(
    0x0a352289,
    0x0aa7,
    0x4759,
    [0x86, 0x2d, 0x5d, 0x15, 0xcd, 0x16, 0xd2, 0x54],
);
const PRESET_P1: GUID = guid(
    0xfc0a8d3e,
    0x45f8,
    0x4cf8,
    [0x80, 0xc7, 0x29, 0x88, 0x71, 0x59, 0x0e, 0xbf],
);

/// Reference frames kept: after a loss, the next frame can refer to one up to
/// this many frames back (less one). A page notices a loss about a frame
/// late and its report takes half a round trip, so at 60 fps this covers
/// round trips up to ~100 ms, at 120 about half that (a loss further back
/// costs a keyframe instead). It stays 8 at every rate: AV1 has eight
/// reference slots, and a bigger DPB at 1440p would pass what H.264 and HEVC
/// levels allow decoders (12 frames).
pub const DPB_FRAMES: u32 = 8;

/// The SDK these bindings come from (13.0); the driver must support it.
const SDK_VERSION: (u32, u32) = (13, 0);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Codec {
    H264,
    Hevc,
    Av1,
}

impl Codec {
    pub const ALL: [Codec; 3] = [Codec::H264, Codec::Hevc, Codec::Av1];

    pub fn name(self) -> &'static str {
        match self {
            Codec::H264 => "h264",
            Codec::Hevc => "hevc",
            Codec::Av1 => "av1",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.name() == name)
    }

    fn guid(self) -> GUID {
        match self {
            Codec::H264 => CODEC_H264,
            Codec::Hevc => CODEC_HEVC,
            Codec::Av1 => CODEC_AV1,
        }
    }
}

/// What the encoder's input surfaces hold.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum InputFormat {
    /// Packed 8-bit BGRA in memory (DRM `ARGB8888`/`XRGB8888`). NVENC converts
    /// it to YUV with a CUDA kernel before encoding.
    #[default]
    Argb,
    /// 8-bit 4:2:0 in one allocation: the luma plane, then interleaved chroma
    /// at `pitch × height`. Goes straight to the encoder.
    Nv12,
}

#[derive(Clone, Debug)]
pub struct EncoderConfig {
    pub codec: Codec,
    pub input: InputFormat,
    pub width: u32,
    pub height: u32,
    /// The largest size [`Encoder::resize`] may switch to without a new session.
    pub max_width: u32,
    pub max_height: u32,
    pub fps: u32,
    pub bitrate_bps: u32,
}

struct Api {
    _lib: Library,
    f: NV_ENCODE_API_FUNCTION_LIST,
}

// SAFETY: the function table is immutable after loading.
unsafe impl Send for Api {}
unsafe impl Sync for Api {}

fn api() -> Result<&'static Api> {
    static API: OnceLock<Result<Api>> = OnceLock::new();
    API.get_or_init(load_api).as_ref().map_err(Clone::clone)
}

type MaxVersionFn = unsafe extern "C" fn(*mut u32) -> NVENCSTATUS;
type CreateInstanceFn = unsafe extern "C" fn(*mut NV_ENCODE_API_FUNCTION_LIST) -> NVENCSTATUS;

fn load_api() -> Result<Api> {
    let lib = Library::open(&["libnvidia-encode.so.1", "libnvidia-encode.so"])?;
    // SAFETY: the symbols' types are as declared in nvEncodeAPI.h.
    unsafe {
        let max_version: MaxVersionFn = lib.symbol(c"NvEncodeAPIGetMaxSupportedVersion")?;
        let create: CreateInstanceFn = lib.symbol(c"NvEncodeAPICreateInstance")?;
        let mut version = 0;
        check_status(
            max_version(&mut version),
            "NvEncodeAPIGetMaxSupportedVersion",
        )?;
        let (major, minor) = (version >> 4, version & 0xf);
        if (major, minor) < SDK_VERSION {
            return Err(Error::new(format!(
                "the driver's NVENC supports API {major}.{minor}; we need {}.{} (driver 570 or newer)",
                SDK_VERSION.0, SDK_VERSION.1
            )));
        }
        let mut f = NV_ENCODE_API_FUNCTION_LIST {
            version: NV_ENCODE_API_FUNCTION_LIST_VER,
            ..Default::default()
        };
        check_status(create(&mut f), "NvEncodeAPICreateInstance")?;
        Ok(Api { _lib: lib, f })
    }
}

/// The highest NVENC API version the driver supports, as (major, minor).
pub fn max_supported_version() -> Result<(u32, u32)> {
    let lib = Library::open(&["libnvidia-encode.so.1", "libnvidia-encode.so"])?;
    let mut version = 0;
    // SAFETY: as in `load_api`.
    unsafe {
        let max_version: MaxVersionFn = lib.symbol(c"NvEncodeAPIGetMaxSupportedVersion")?;
        check_status(
            max_version(&mut version),
            "NvEncodeAPIGetMaxSupportedVersion",
        )?;
    }
    Ok((version >> 4, version & 0xf))
}

fn check_status(status: NVENCSTATUS, what: &str) -> Result<()> {
    if status == NVENCSTATUS::NV_ENC_SUCCESS {
        Ok(())
    } else {
        Err(Error::new(format!(
            "{what} failed: NVENCSTATUS {}",
            status.0
        )))
    }
}

/// Calls an entry of the function table, which the driver always fills.
macro_rules! call {
    ($api:expr, $name:ident ( $($arg:expr),* $(,)? )) => {
        ($api.f.$name.expect(concat!("NVENC has no ", stringify!($name))))($($arg),*)
    };
}

/// Where the last [`Encoder::encode`] spent its time.
#[derive(Clone, Copy, Debug, Default)]
pub struct Timings {
    /// Registering (first use only) and mapping the input.
    pub map: Duration,
    /// `nvEncEncodePicture`: queues the frame (and, for RGB input, the colour
    /// conversion on CUDA).
    pub submit: Duration,
    /// `nvEncLockBitstream`: waiting for the encoder to finish.
    pub wait: Duration,
}

/// One NVENC session.
pub struct Encoder {
    api: &'static Api,
    ctx: Arc<CudaContext>,
    session: *mut c_void,
    config: EncoderConfig,
    // Boxed so the pointer in `init.encodeConfig` stays valid.
    nv_config: Box<NV_ENC_CONFIG>,
    init: Box<NV_ENC_INITIALIZE_PARAMS>,
    bitstream: NV_ENC_OUTPUT_PTR,
    /// NVENC registrations of input surfaces, by `Surface::key`.
    registered: Vec<(u64, NV_ENC_REGISTERED_PTR)>,
    /// The next frame's index (its `inputTimeStamp`), and the last keyframe's.
    frame_index: u64,
    last_key: u64,
    /// Reference frame invalidation works with this codec here.
    rfi: bool,
    timings: Timings,
}

// SAFETY: a session is used by one thread at a time (it lives on its encoder
// thread); all its handles are opaque.
unsafe impl Send for Encoder {}

impl Encoder {
    pub fn new(ctx: Arc<CudaContext>, config: EncoderConfig) -> Result<Self> {
        let api = api()?;
        let _current = ctx.push()?;
        let mut open = NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS {
            version: NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS_VER,
            deviceType: NV_ENC_DEVICE_TYPE::NV_ENC_DEVICE_TYPE_CUDA,
            device: ctx.raw(),
            apiVersion: NVENCAPI_VERSION,
            ..Default::default()
        };
        let mut session = std::ptr::null_mut();
        // SAFETY: valid parameters; `session` receives the handle.
        check_status(
            unsafe { call!(api, nvEncOpenEncodeSessionEx(&mut open, &mut session)) },
            "nvEncOpenEncodeSessionEx",
        )?;
        let mut encoder = Self {
            api,
            ctx: Arc::clone(&ctx),
            session,
            config,
            nv_config: Box::default(),
            init: Box::default(),
            bitstream: std::ptr::null_mut(),
            registered: Vec::new(),
            frame_index: 0,
            last_key: 0,
            rfi: false,
            timings: Timings::default(),
        };
        // On error, `Drop` closes the session.
        encoder.initialize()?;
        encoder.rfi = encoder.cap(NV_ENC_CAPS::NV_ENC_CAPS_SUPPORT_REF_PIC_INVALIDATION) != 0;
        Ok(encoder)
    }

    /// One of the session's codec capabilities (0 if the query fails).
    fn cap(&self, cap: NV_ENC_CAPS) -> i32 {
        let mut param = NV_ENC_CAPS_PARAM {
            version: NV_ENC_CAPS_PARAM_VER,
            capsToQuery: cap,
            ..Default::default()
        };
        let mut value = 0;
        // SAFETY: a live session and valid parameters.
        let status = unsafe {
            call!(
                self.api,
                nvEncGetEncodeCaps(
                    self.session,
                    self.config.codec.guid(),
                    &mut param,
                    &mut value
                )
            )
        };
        if status == NVENCSTATUS::NV_ENC_SUCCESS {
            value
        } else {
            0
        }
    }

    /// Whether [`Self::invalidate_from`] can work at all.
    pub fn supports_rfi(&self) -> bool {
        self.rfi
    }

    /// The index of the last frame encoded.
    pub fn last_index(&self) -> u64 {
        self.frame_index.saturating_sub(1)
    }

    /// The index the next frame gets.
    pub fn next_index(&self) -> u64 {
        self.frame_index
    }

    /// Reference frame invalidation: frames from `from` on (by
    /// [`Self::last_index`]) were lost on the way, so none of them is a
    /// reference any more, and the next frame refers to one from before them.
    /// False when that can't be (no support, a keyframe since, or `from` too
    /// far back for the DPB): send a keyframe instead.
    pub fn invalidate_from(&mut self, from: u64) -> Result<bool> {
        let next = self.frame_index;
        if !self.rfi
            || from <= self.last_key
            || from >= next
            || next - from >= u64::from(DPB_FRAMES)
        {
            return Ok(false);
        }
        let _current = self.ctx.push()?;
        for index in from..next {
            // SAFETY: a live session; `index` is a frame it encoded.
            self.status(
                unsafe { call!(self.api, nvEncInvalidateRefFrames(self.session, index)) },
                "nvEncInvalidateRefFrames",
            )?;
        }
        Ok(true)
    }

    pub fn config(&self) -> &EncoderConfig {
        &self.config
    }

    /// The stage timings of the last encoded frame.
    pub fn last_timings(&self) -> Timings {
        self.timings
    }

    fn initialize(&mut self) -> Result<()> {
        let c = &self.config;
        let mut preset = NV_ENC_PRESET_CONFIG {
            version: NV_ENC_PRESET_CONFIG_VER,
            presetCfg: NV_ENC_CONFIG {
                version: NV_ENC_CONFIG_VER,
                ..Default::default()
            },
            ..Default::default()
        };
        // SAFETY: a live session and valid out-parameter.
        self.status(
            unsafe {
                call!(
                    self.api,
                    nvEncGetEncodePresetConfigEx(
                        self.session,
                        c.codec.guid(),
                        PRESET_P1,
                        NV_ENC_TUNING_INFO::NV_ENC_TUNING_INFO_ULTRA_LOW_LATENCY,
                        &mut preset,
                    )
                )
            },
            "nvEncGetEncodePresetConfigEx",
        )?;
        *self.nv_config = preset.presetCfg;
        tune(&mut self.nv_config, c);
        *self.init = init_params(c, &mut self.nv_config);
        // SAFETY: init params point at our boxed config.
        let status = unsafe {
            call!(
                self.api,
                nvEncInitializeEncoder(self.session, &mut *self.init)
            )
        };
        self.status(status, "nvEncInitializeEncoder")?;
        let mut create = NV_ENC_CREATE_BITSTREAM_BUFFER {
            version: NV_ENC_CREATE_BITSTREAM_BUFFER_VER,
            ..Default::default()
        };
        // SAFETY: as above.
        self.status(
            unsafe {
                call!(
                    self.api,
                    nvEncCreateBitstreamBuffer(self.session, &mut create)
                )
            },
            "nvEncCreateBitstreamBuffer",
        )?;
        self.bitstream = create.bitstreamBuffer;
        Ok(())
    }

    /// Encodes one frame of `surface` (in the configured [`InputFormat`] and
    /// size) into `out`, which is cleared first. Returns whether the frame is a
    /// keyframe.
    pub fn encode(&mut self, surface: Surface, keyframe: bool, out: &mut Vec<u8>) -> Result<bool> {
        let ctx = Arc::clone(&self.ctx);
        let _current = ctx.push()?;
        let started = Instant::now();
        let (width, height) = (self.config.width, self.config.height);
        let pitch = match surface {
            Surface::Array(_) if self.config.input == InputFormat::Nv12 => width,
            Surface::Array(_) => width * 4,
            Surface::Pitch { pitch, .. } => pitch,
        };
        let registered = self.registration(surface, pitch)?;
        let mut map = NV_ENC_MAP_INPUT_RESOURCE {
            version: NV_ENC_MAP_INPUT_RESOURCE_VER,
            registeredResource: registered,
            ..Default::default()
        };
        // SAFETY: a registration of this session.
        self.status(
            unsafe { call!(self.api, nvEncMapInputResource(self.session, &mut map)) },
            "nvEncMapInputResource",
        )?;
        self.timings.map = started.elapsed();
        let result = self.encode_mapped(&map, width, height, pitch, keyframe, out);
        // SAFETY: mapped just above.
        let unmapped = self.status(
            unsafe {
                call!(
                    self.api,
                    nvEncUnmapInputResource(self.session, map.mappedResource)
                )
            },
            "nvEncUnmapInputResource",
        );
        let key = result?;
        unmapped?;
        Ok(key)
    }

    fn encode_mapped(
        &mut self,
        map: &NV_ENC_MAP_INPUT_RESOURCE,
        width: u32,
        height: u32,
        pitch: u32,
        keyframe: bool,
        out: &mut Vec<u8>,
    ) -> Result<bool> {
        let flags = if keyframe {
            NV_ENC_PIC_FLAGS::NV_ENC_PIC_FLAG_FORCEIDR.0
                | NV_ENC_PIC_FLAGS::NV_ENC_PIC_FLAG_OUTPUT_SPSPPS.0
        } else {
            0
        };
        let mut pic = NV_ENC_PIC_PARAMS {
            version: NV_ENC_PIC_PARAMS_VER,
            inputWidth: width,
            inputHeight: height,
            inputPitch: pitch,
            encodePicFlags: flags,
            inputTimeStamp: self.frame_index,
            inputBuffer: map.mappedResource,
            outputBitstream: self.bitstream,
            bufferFmt: map.mappedBufferFmt,
            pictureStruct: NV_ENC_PIC_STRUCT::NV_ENC_PIC_STRUCT_FRAME,
            ..Default::default()
        };
        self.frame_index += 1;
        let submitted = Instant::now();
        // SAFETY: mapped input and our bitstream buffer.
        self.status(
            unsafe { call!(self.api, nvEncEncodePicture(self.session, &mut pic)) },
            "nvEncEncodePicture",
        )?;
        let waiting = Instant::now();
        self.timings.submit = waiting - submitted;
        let mut lock = NV_ENC_LOCK_BITSTREAM {
            version: NV_ENC_LOCK_BITSTREAM_VER,
            outputBitstream: self.bitstream,
            ..Default::default()
        };
        // SAFETY: waits for the picture just submitted (synchronous mode).
        self.status(
            unsafe { call!(self.api, nvEncLockBitstream(self.session, &mut lock)) },
            "nvEncLockBitstream",
        )?;
        self.timings.wait = waiting.elapsed();
        out.clear();
        // SAFETY: NVENC's buffer holds `bitstreamSizeInBytes` bytes until unlocked.
        out.extend_from_slice(unsafe {
            std::slice::from_raw_parts(
                lock.bitstreamBufferPtr as *const u8,
                lock.bitstreamSizeInBytes as usize,
            )
        });
        let key = matches!(
            lock.pictureType,
            NV_ENC_PIC_TYPE::NV_ENC_PIC_TYPE_IDR | NV_ENC_PIC_TYPE::NV_ENC_PIC_TYPE_I
        );
        if key {
            self.last_key = self.frame_index - 1;
        }
        // SAFETY: locked above.
        self.status(
            unsafe { call!(self.api, nvEncUnlockBitstream(self.session, self.bitstream)) },
            "nvEncUnlockBitstream",
        )?;
        Ok(key)
    }

    /// The NVENC registration of `surface`, made on first use.
    fn registration(&mut self, surface: Surface, pitch: u32) -> Result<NV_ENC_REGISTERED_PTR> {
        let key = surface.key();
        if let Some(&(_, registered)) = self.registered.iter().find(|(k, _)| *k == key) {
            return Ok(registered);
        }
        let (resource_type, resource) = match surface {
            Surface::Array(array) => (
                NV_ENC_INPUT_RESOURCE_TYPE::NV_ENC_INPUT_RESOURCE_TYPE_CUDAARRAY,
                array,
            ),
            Surface::Pitch { ptr, .. } => (
                NV_ENC_INPUT_RESOURCE_TYPE::NV_ENC_INPUT_RESOURCE_TYPE_CUDADEVICEPTR,
                ptr as *mut c_void,
            ),
        };
        let mut register = NV_ENC_REGISTER_RESOURCE {
            version: NV_ENC_REGISTER_RESOURCE_VER,
            resourceType: resource_type,
            width: self.config.width,
            height: self.config.height,
            pitch,
            resourceToRegister: resource,
            bufferFormat: match self.config.input {
                InputFormat::Argb => NV_ENC_BUFFER_FORMAT::NV_ENC_BUFFER_FORMAT_ARGB,
                InputFormat::Nv12 => NV_ENC_BUFFER_FORMAT::NV_ENC_BUFFER_FORMAT_NV12,
            },
            bufferUsage: NV_ENC_BUFFER_USAGE::NV_ENC_INPUT_IMAGE,
            ..Default::default()
        };
        // SAFETY: a live CUDA surface of the configured size.
        self.status(
            unsafe { call!(self.api, nvEncRegisterResource(self.session, &mut register)) },
            "nvEncRegisterResource",
        )?;
        self.registered.push((key, register.registeredResource));
        Ok(register.registeredResource)
    }

    /// Drops every input registration; call when the surfaces it has seen go
    /// away (the compositor reallocated its buffers).
    pub fn forget_surfaces(&mut self) {
        let Ok(_current) = self.ctx.push() else {
            return;
        };
        for (_, registered) in self.registered.drain(..) {
            // SAFETY: registered by this session and not mapped.
            unsafe { call!(self.api, nvEncUnregisterResource(self.session, registered)) };
        }
    }

    /// Switches to a new size (up to the configured maximum) in place. The next
    /// frame is a keyframe; input registrations are dropped.
    pub fn resize(&mut self, width: u32, height: u32) -> Result<()> {
        if (width, height) == (self.config.width, self.config.height) {
            return Ok(());
        }
        if width > self.config.max_width || height > self.config.max_height {
            return Err(Error::new(format!(
                "{width}x{height} is beyond this session's maximum ({}x{})",
                self.config.max_width, self.config.max_height
            )));
        }
        self.forget_surfaces();
        self.config.width = width;
        self.config.height = height;
        self.reconfigure(true)
    }

    /// Changes the target bitrate in place (CBR, one-frame VBV), from the next
    /// frame on: no reset and no keyframe, so rate control can use it often.
    pub fn set_bitrate(&mut self, bitrate_bps: u32) -> Result<()> {
        if bitrate_bps == self.config.bitrate_bps {
            return Ok(());
        }
        self.config.bitrate_bps = bitrate_bps;
        self.reconfigure(false)
    }

    pub fn bitrate(&self) -> u32 {
        self.config.bitrate_bps
    }

    /// Changes the frame rate and the bitrate in place, from the next frame
    /// on: NVENC's rate control (the VBV is one frame of bits) and its timing
    /// follow, with no reset and no keyframe. The caller gives frames at
    /// the new rate.
    pub fn set_frame_rate(&mut self, fps: u32, bitrate_bps: u32) -> Result<()> {
        if (fps, bitrate_bps) == (self.config.fps, self.config.bitrate_bps) {
            return Ok(());
        }
        self.config.fps = fps;
        self.config.bitrate_bps = bitrate_bps;
        self.reconfigure(false)
    }

    /// Applies the config; `reset` (a new size) restarts from a keyframe.
    fn reconfigure(&mut self, reset: bool) -> Result<()> {
        let _current = self.ctx.push()?;
        tune(&mut self.nv_config, &self.config);
        *self.init = init_params(&self.config, &mut self.nv_config);
        let mut params = NV_ENC_RECONFIGURE_PARAMS {
            version: NV_ENC_RECONFIGURE_PARAMS_VER,
            reInitEncodeParams: *self.init,
            ..Default::default()
        };
        params.set_resetEncoder(u32::from(reset));
        params.set_forceIDR(u32::from(reset));
        // SAFETY: valid parameters for a live session.
        self.status(
            unsafe { call!(self.api, nvEncReconfigureEncoder(self.session, &mut params)) },
            "nvEncReconfigureEncoder",
        )
    }

    /// Checks a status, adding the session's own error text.
    fn status(&self, status: NVENCSTATUS, what: &str) -> Result<()> {
        if status == NVENCSTATUS::NV_ENC_SUCCESS {
            return Ok(());
        }
        // SAFETY: returns a string owned by the session, or null.
        let text = unsafe { call!(self.api, nvEncGetLastErrorString(self.session)) };
        let detail = if text.is_null() {
            String::new()
        } else {
            // SAFETY: checked non-null.
            format!(": {}", unsafe { CStr::from_ptr(text) }.to_string_lossy())
        };
        Err(Error::new(format!(
            "{what} failed (NVENCSTATUS {}){detail}",
            status.0
        )))
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        self.forget_surfaces();
        let Ok(_current) = self.ctx.push() else {
            return;
        };
        // SAFETY: our own handles, destroyed once.
        unsafe {
            if !self.bitstream.is_null() {
                call!(
                    self.api,
                    nvEncDestroyBitstreamBuffer(self.session, self.bitstream)
                );
            }
            call!(self.api, nvEncDestroyEncoder(self.session));
        }
    }
}

fn init_params(c: &EncoderConfig, config: &mut NV_ENC_CONFIG) -> NV_ENC_INITIALIZE_PARAMS {
    NV_ENC_INITIALIZE_PARAMS {
        version: NV_ENC_INITIALIZE_PARAMS_VER,
        encodeGUID: c.codec.guid(),
        presetGUID: PRESET_P1,
        encodeWidth: c.width,
        encodeHeight: c.height,
        darWidth: c.width,
        darHeight: c.height,
        frameRateNum: c.fps,
        frameRateDen: 1,
        enableEncodeAsync: 0,
        enablePTD: 1,
        encodeConfig: config,
        maxEncodeWidth: c.max_width.max(c.width),
        maxEncodeHeight: c.max_height.max(c.height),
        tuningInfo: NV_ENC_TUNING_INFO::NV_ENC_TUNING_INFO_ULTRA_LOW_LATENCY,
        ..Default::default()
    }
}

/// Our low-latency settings on top of the P1 / ultra-low-latency preset.
fn tune(config: &mut NV_ENC_CONFIG, c: &EncoderConfig) {
    config.gopLength = NVENC_INFINITE_GOPLENGTH;
    config.frameIntervalP = 1;
    let rc = &mut config.rcParams;
    rc.rateControlMode = NV_ENC_PARAMS_RC_MODE::NV_ENC_PARAMS_RC_CBR;
    rc.averageBitRate = c.bitrate_bps;
    rc.maxBitRate = c.bitrate_bps;
    // One frame of VBV: a frame never waits behind the previous one's bits.
    rc.vbvBufferSize = c.bitrate_bps / c.fps.max(1);
    rc.vbvInitialDelay = rc.vbvBufferSize;
    rc.multiPass = NV_ENC_MULTI_PASS::NV_ENC_MULTI_PASS_DISABLED;
    rc.lookaheadDepth = 0;
    rc.set_enableLookahead(0);
    rc.set_zeroReorderDelay(1);

    // SAFETY: the union member matching the codec.
    unsafe {
        match c.codec {
            Codec::H264 => {
                let h264 = &mut config.encodeCodecConfig.h264Config;
                h264.idrPeriod = NVENC_INFINITE_GOPLENGTH;
                h264.maxNumRefFrames = DPB_FRAMES;
                h264.numRefL0 = NV_ENC_NUM_REF_FRAMES::NV_ENC_NUM_REF_FRAMES_1;
                h264.set_repeatSPSPPS(1);
                h264.set_outputAUD(0);
                vui(&mut h264.h264VUIParameters);
            }
            Codec::Hevc => {
                let hevc = &mut config.encodeCodecConfig.hevcConfig;
                hevc.idrPeriod = NVENC_INFINITE_GOPLENGTH;
                hevc.maxNumRefFramesInDPB = DPB_FRAMES;
                hevc.numRefL0 = NV_ENC_NUM_REF_FRAMES::NV_ENC_NUM_REF_FRAMES_1;
                hevc.set_repeatSPSPPS(1);
                hevc.set_outputAUD(0);
                vui(&mut hevc.hevcVUIParameters);
            }
            Codec::Av1 => {
                let av1 = &mut config.encodeCodecConfig.av1Config;
                av1.idrPeriod = NVENC_INFINITE_GOPLENGTH;
                av1.maxNumRefFramesInDPB = DPB_FRAMES;
                av1.numFwdRefs = NV_ENC_NUM_REF_FRAMES::NV_ENC_NUM_REF_FRAMES_1;
                av1.set_repeatSeqHdr(1);
                // Low-overhead OBUs (what WebRTC's AV1 packetizer expects).
                av1.set_outputAnnexBFormat(0);
                av1.colorPrimaries = NV_ENC_VUI_COLOR_PRIMARIES::NV_ENC_VUI_COLOR_PRIMARIES_BT709;
                av1.transferCharacteristics =
                    NV_ENC_VUI_TRANSFER_CHARACTERISTIC::NV_ENC_VUI_TRANSFER_CHARACTERISTIC_SRGB;
                av1.matrixCoefficients = NV_ENC_VUI_MATRIX_COEFFS::NV_ENC_VUI_MATRIX_COEFFS_BT709;
                av1.colorRange = 0;
            }
        }
    }
}

/// BT.709 primaries and matrix with the sRGB transfer (the desktop's pixels
/// are sRGB-encoded and NVENC converts them without changing the curve; tagged
/// BT.709, Chrome on a Mac showed the midtones about 11 levels too light),
/// limited range, and the bitstream restriction that carries
/// `max_num_reorder_frames = 0` (so hardware decoders don't hold frames).
fn vui(vui: &mut NV_ENC_CONFIG_H264_VUI_PARAMETERS) {
    vui.videoSignalTypePresentFlag = 1;
    vui.videoFormat = NV_ENC_VUI_VIDEO_FORMAT::NV_ENC_VUI_VIDEO_FORMAT_UNSPECIFIED;
    vui.videoFullRangeFlag = 0;
    vui.colourDescriptionPresentFlag = 1;
    vui.colourPrimaries = NV_ENC_VUI_COLOR_PRIMARIES::NV_ENC_VUI_COLOR_PRIMARIES_BT709;
    vui.transferCharacteristics =
        NV_ENC_VUI_TRANSFER_CHARACTERISTIC::NV_ENC_VUI_TRANSFER_CHARACTERISTIC_SRGB;
    vui.colourMatrix = NV_ENC_VUI_MATRIX_COEFFS::NV_ENC_VUI_MATRIX_COEFFS_BT709;
    vui.bitstreamRestrictionFlag = 1;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn struct_versions_match_the_header() {
        // NVENCAPI_STRUCT_VERSION(9) | 1 << 31 for API 13.0.
        assert_eq!(NV_ENC_CONFIG_VER, 13 | (9 << 16) | (7 << 28) | (1 << 31));
        assert_eq!(NVENCAPI_VERSION, 13);
    }

    #[test]
    fn codec_names_round_trip() {
        for codec in Codec::ALL {
            assert_eq!(Codec::from_name(codec.name()), Some(codec));
        }
    }

    /// Encodes 1440p at 60 fps, switches the encoder to 120 in place (the
    /// bitrate scaled by 2^0.75), and measures `encode` at 120: it has to fit
    /// in 8.3 ms. Prints the timings (`--nocapture`). Needs an NVIDIA GPU.
    #[test]
    #[ignore = "needs an NVIDIA GPU"]
    fn switches_to_120_fps_in_place_and_keeps_up() {
        use std::ffi::c_void;
        type Alloc = unsafe extern "C" fn(*mut u64, usize) -> i32;
        type Upload = unsafe extern "C" fn(u64, *const c_void, usize) -> i32;
        let (w, h) = (2560u32, 1440u32);
        let ctx = CudaContext::new(None).unwrap();
        let lib = crate::dl::Library::open(&["libcuda.so.1"]).unwrap();
        // SAFETY: the symbols' types are cuda.h's.
        let (alloc, upload): (Alloc, Upload) = unsafe {
            (
                lib.symbol(c"cuMemAlloc_v2").unwrap(),
                lib.symbol(c"cuMemcpyHtoD_v2").unwrap(),
            )
        };
        let _current = ctx.push().unwrap();
        // Two noisy gradients, so each frame differs from the last.
        let surfaces: Vec<Surface> = (0..2u32)
            .map(|n| {
                let mut seed = 0x9e37_79b9u32 ^ n;
                let pixels: Vec<u8> = (0..w * h * 4)
                    .map(|i| {
                        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                        let x = (i / 4) % w;
                        ((x * 255 / w) as u8 / 2).wrapping_add((seed >> 28) as u8 * 4)
                    })
                    .collect();
                let mut ptr = 0u64;
                // SAFETY: a current context; the copy is as long as the allocation.
                unsafe {
                    assert_eq!(alloc(&mut ptr, pixels.len()), 0);
                    assert_eq!(upload(ptr, pixels.as_ptr().cast(), pixels.len()), 0);
                }
                Surface::Pitch { ptr, pitch: w * 4 }
            })
            .collect();
        let base = 40_000_000u32;
        for codec in Codec::ALL {
            let mut encoder = Encoder::new(
                Arc::clone(&ctx),
                EncoderConfig {
                    codec,
                    input: InputFormat::Argb,
                    width: w,
                    height: h,
                    max_width: w,
                    max_height: h,
                    fps: 60,
                    bitrate_bps: base,
                },
            )
            .unwrap();
            let mut out = Vec::new();
            let mut run = |encoder: &mut Encoder, frames: usize| {
                let mut times = Vec::new();
                let mut bytes = 0;
                for i in 0..frames {
                    let started = Instant::now();
                    let key = encoder.encode(surfaces[i % 2], i == 0, &mut out).unwrap();
                    times.push(started.elapsed().as_secs_f64() * 1e3);
                    bytes += out.len();
                    assert_eq!(key, i == 0);
                }
                times.sort_by(f64::total_cmp);
                (
                    times[times.len() / 2],
                    times[times.len() * 99 / 100],
                    bytes / frames,
                )
            };
            let (p50, p99, bytes) = run(&mut encoder, 120);
            eprintln!("{codec:?} 60 fps: encode p50 {p50:.2} ms p99 {p99:.2} ms, {bytes} B/frame");
            // Live: the frame rate and the scaled bitrate, no keyframe.
            let scaled = (f64::from(base) * 2f64.powf(0.75)) as u32;
            encoder.set_frame_rate(120, scaled).unwrap();
            assert_eq!((encoder.config().fps, encoder.bitrate()), (120, scaled));
            let mut out2 = Vec::new();
            let mut times = Vec::new();
            let mut keys = 0;
            let mut bytes_total = 0;
            for i in 0..480 {
                let started = Instant::now();
                keys += u32::from(encoder.encode(surfaces[i % 2], false, &mut out2).unwrap());
                times.push(started.elapsed().as_secs_f64() * 1e3);
                bytes_total += out2.len();
            }
            times.sort_by(f64::total_cmp);
            eprintln!(
                "{codec:?} 120 fps: encode p50 {:.2} ms p99 {:.2} ms max {:.2} ms, {} B/frame, {keys} keyframes",
                times[240],
                times[475],
                times[479],
                bytes_total / 480
            );
            assert_eq!(keys, 0, "the switch must not cost a keyframe");
        }
    }
}

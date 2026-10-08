//! libva, loaded at run time: the functions we call, the constants and the
//! `#[repr(C)]` structs of `va.h`, `va_vpp.h`, `va_drmcommon.h` and
//! `va_enc_h264.h` and `va_enc_hevc.h` that we fill in, and [`Display`], one VA display on a render
//! node.
//!
//! Written from the VA-API headers' declarations as documented (the
//! libva API reference); no other project's source was read. Struct layouts
//! are `#[repr(C)]` with the headers' field order, so the compiler lays them
//! out as C does; bit-field unions are plain `u32`s with the bit positions
//! spelled out where they are set.

use std::ffi::{CStr, c_char, c_int, c_uint, c_void};
use std::fs::File;
use std::os::fd::AsRawFd;
use std::path::Path;
use std::sync::OnceLock;

use anyhow::{Context, Result, anyhow, bail, ensure};
use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::allocator::{Buffer, Fourcc};

use super::sys::{
    self, _VADRMPRIMESurfaceDescriptor__bindgen_ty_1 as DrmObject,
    _VADRMPRIMESurfaceDescriptor__bindgen_ty_2 as DrmLayer, VACodedBufferSegment as CodedSegment,
    VAConfigAttrib as ConfigAttrib, VADRMPRIMESurfaceDescriptor as DrmPrimeDescriptor,
    VASurfaceAttrib as SurfaceAttrib,
};
use crate::encoder::dl::Library;

pub type Id = c_uint;
pub use super::sys::{VAProcPipelineParameterBuffer as ProcPipeline, VARectangle as Rectangle};
pub const INVALID_ID: Id = sys::VA_INVALID_ID;
pub const INVALID_SURFACE: Id = sys::VA_INVALID_SURFACE;

// The numbers below that `sys.rs` doesn't have (the enums of profiles,
// entrypoints and buffer types, which bindgen wasn't asked for) are checked
// against libva 2.22's `va.h`/`va_enc_*.h`.

// VAProfile.
pub const PROFILE_NONE: c_int = -1;
pub const PROFILE_H264_MAIN: c_int = 6;
pub const PROFILE_H264_HIGH: c_int = 7;
pub const PROFILE_H264_CONSTRAINED_BASELINE: c_int = 13;
pub const PROFILE_HEVC_MAIN: c_int = 17;

// VAEntrypoint.
pub const ENTRYPOINT_ENC_SLICE: c_int = 6;
pub const ENTRYPOINT_VIDEO_PROC: c_int = 10;
pub const ENTRYPOINT_ENC_SLICE_LP: c_int = 8;

// VAConfigAttribType.
pub const ATTRIB_RT_FORMAT: c_uint = sys::VAConfigAttribType_VAConfigAttribRTFormat;
pub const ATTRIB_RATE_CONTROL: c_uint = sys::VAConfigAttribType_VAConfigAttribRateControl;
pub const ATTRIB_ENC_PACKED_HEADERS: c_uint =
    sys::VAConfigAttribType_VAConfigAttribEncPackedHeaders;
pub const ATTRIB_ENC_HEVC_FEATURES: c_uint = sys::VAConfigAttribType_VAConfigAttribEncHEVCFeatures;
pub const ATTRIB_ENC_HEVC_BLOCK_SIZES: c_uint =
    sys::VAConfigAttribType_VAConfigAttribEncHEVCBlockSizes;
/// What `vaGetConfigAttributes` puts in an attribute the driver lacks.
pub const ATTRIB_NOT_SUPPORTED: c_uint = sys::VA_ATTRIB_NOT_SUPPORTED;

// Render target formats, and rate control modes.
pub const RT_FORMAT_YUV420: c_uint = sys::VA_RT_FORMAT_YUV420;
pub const RT_FORMAT_RGB32: c_uint = sys::VA_RT_FORMAT_RGB32;
pub const RC_CBR: c_uint = sys::VA_RC_CBR;
pub const RC_CQP: c_uint = sys::VA_RC_CQP;

// VA_ENC_PACKED_HEADER_*.
pub const PACKED_HEADER_SEQUENCE: c_uint = sys::VA_ENC_PACKED_HEADER_SEQUENCE;
pub const PACKED_HEADER_PICTURE: c_uint = sys::VA_ENC_PACKED_HEADER_PICTURE;
pub const PACKED_HEADER_SLICE: c_uint = sys::VA_ENC_PACKED_HEADER_SLICE;

// VABufferType.
pub const BUFFER_ENC_CODED: c_int = 21;
pub const BUFFER_ENC_SEQUENCE: c_int = 22;
pub const BUFFER_ENC_PICTURE: c_int = 23;
pub const BUFFER_ENC_SLICE: c_int = 24;
pub const BUFFER_ENC_PACKED_HEADER_PARAMETER: c_int = 25;
pub const BUFFER_ENC_PACKED_HEADER_DATA: c_int = 26;
pub const BUFFER_ENC_MISC_PARAMETER: c_int = 27;
pub const BUFFER_PROC_PIPELINE: c_int = 41;

// VAEncPackedHeaderType.
pub const PACKED_SEQUENCE: u32 = 1;
pub const PACKED_PICTURE: u32 = 2;
pub const PACKED_SLICE: u32 = 3;

// VAEncMiscParameterType.
pub const MISC_FRAME_RATE: u32 = sys::VAEncMiscParameterType_VAEncMiscParameterTypeFrameRate;
pub const MISC_RATE_CONTROL: u32 = sys::VAEncMiscParameterType_VAEncMiscParameterTypeRateControl;
pub const MISC_HRD: u32 = sys::VAEncMiscParameterType_VAEncMiscParameterTypeHRD;

// VASurfaceAttribType and friends.
const SURFACE_ATTRIB_PIXEL_FORMAT: c_uint = sys::VASurfaceAttribType_VASurfaceAttribPixelFormat;
const SURFACE_ATTRIB_MEMORY_TYPE: c_uint = sys::VASurfaceAttribType_VASurfaceAttribMemoryType;
const SURFACE_ATTRIB_EXTERNAL_BUFFER_DESCRIPTOR: c_uint =
    sys::VASurfaceAttribType_VASurfaceAttribExternalBufferDescriptor;
const SURFACE_ATTRIB_SETTABLE: c_uint = sys::VA_SURFACE_ATTRIB_SETTABLE;
const MEM_TYPE_DRM_PRIME_2: c_uint = sys::VA_SURFACE_ATTRIB_MEM_TYPE_DRM_PRIME_2;
const GENERIC_INTEGER: c_uint = sys::VAGenericValueType_VAGenericValueTypeInteger;
const GENERIC_POINTER: c_uint = sys::VAGenericValueType_VAGenericValueTypePointer;

pub const fn fourcc(a: u8, b: u8, c: u8, d: u8) -> u32 {
    (a as u32) | ((b as u32) << 8) | ((c as u32) << 16) | ((d as u32) << 24)
}

pub const FOURCC_NV12: u32 = sys::VA_FOURCC_NV12;

/// `VA_PROGRESSIVE`, for `vaCreateContext`.
const PROGRESSIVE: c_int = sys::VA_PROGRESSIVE as c_int;

// VAProcColorStandardType.
pub const COLOR_STANDARD_BT709: c_uint = sys::_VAProcColorStandardType_VAProcColorStandardBT709;
pub const COLOR_STANDARD_SRGB: c_uint = sys::_VAProcColorStandardType_VAProcColorStandardSRGB;

// VAPictureH264 flags.
pub const PICTURE_H264_INVALID: u32 = sys::VA_PICTURE_H264_INVALID;
pub const PICTURE_H264_SHORT_TERM_REFERENCE: u32 = sys::VA_PICTURE_H264_SHORT_TERM_REFERENCE;

// VAPictureHEVC flags.
pub const PICTURE_HEVC_INVALID: u32 = sys::VA_PICTURE_HEVC_INVALID;
pub const PICTURE_HEVC_RPS_ST_CURR_BEFORE: u32 = sys::VA_PICTURE_HEVC_RPS_ST_CURR_BEFORE;

type GetDisplayDrm = unsafe extern "C" fn(c_int) -> *mut c_void;
type Initialize = unsafe extern "C" fn(*mut c_void, *mut c_int, *mut c_int) -> c_int;
type Terminate = unsafe extern "C" fn(*mut c_void) -> c_int;
type QueryVendorString = unsafe extern "C" fn(*mut c_void) -> *const c_char;
type ErrorStr = unsafe extern "C" fn(c_int) -> *const c_char;
type MaxNum = unsafe extern "C" fn(*mut c_void) -> c_int;
type QueryConfigProfiles = unsafe extern "C" fn(*mut c_void, *mut c_int, *mut c_int) -> c_int;
type QueryConfigEntrypoints =
    unsafe extern "C" fn(*mut c_void, c_int, *mut c_int, *mut c_int) -> c_int;
type GetConfigAttributes =
    unsafe extern "C" fn(*mut c_void, c_int, c_int, *mut ConfigAttrib, c_int) -> c_int;
type CreateConfig =
    unsafe extern "C" fn(*mut c_void, c_int, c_int, *mut ConfigAttrib, c_int, *mut Id) -> c_int;
type DestroyConfig = unsafe extern "C" fn(*mut c_void, Id) -> c_int;
type CreateSurfaces = unsafe extern "C" fn(
    *mut c_void,
    c_uint,
    c_uint,
    c_uint,
    *mut Id,
    c_uint,
    *mut SurfaceAttrib,
    c_uint,
) -> c_int;
type DestroySurfaces = unsafe extern "C" fn(*mut c_void, *mut Id, c_int) -> c_int;
type CreateContext =
    unsafe extern "C" fn(*mut c_void, Id, c_int, c_int, c_int, *mut Id, c_int, *mut Id) -> c_int;
type DestroyContext = unsafe extern "C" fn(*mut c_void, Id) -> c_int;
type CreateBuffer =
    unsafe extern "C" fn(*mut c_void, Id, c_int, c_uint, c_uint, *mut c_void, *mut Id) -> c_int;
type DestroyBuffer = unsafe extern "C" fn(*mut c_void, Id) -> c_int;
type MapBuffer = unsafe extern "C" fn(*mut c_void, Id, *mut *mut c_void) -> c_int;
type UnmapBuffer = unsafe extern "C" fn(*mut c_void, Id) -> c_int;
type BeginPicture = unsafe extern "C" fn(*mut c_void, Id, Id) -> c_int;
type RenderPicture = unsafe extern "C" fn(*mut c_void, Id, *mut Id, c_int) -> c_int;
type EndPicture = unsafe extern "C" fn(*mut c_void, Id) -> c_int;
type SyncSurface = unsafe extern "C" fn(*mut c_void, Id) -> c_int;
type CreateImage =
    unsafe extern "C" fn(*mut c_void, *mut ImageFormat, c_int, c_int, *mut Image) -> c_int;
type GetImage = unsafe extern "C" fn(*mut c_void, Id, c_int, c_int, c_uint, c_uint, Id) -> c_int;
type DestroyImage = unsafe extern "C" fn(*mut c_void, Id) -> c_int;

/// The libva functions, resolved once for the process.
pub struct Api {
    _va: Library,
    _drm: Library,
    get_display_drm: GetDisplayDrm,
    initialize: Initialize,
    terminate: Terminate,
    query_vendor_string: QueryVendorString,
    error_str: ErrorStr,
    max_profiles: MaxNum,
    query_config_profiles: QueryConfigProfiles,
    max_entrypoints: MaxNum,
    query_config_entrypoints: QueryConfigEntrypoints,
    get_config_attributes: GetConfigAttributes,
    create_config: CreateConfig,
    destroy_config: DestroyConfig,
    create_surfaces: CreateSurfaces,
    destroy_surfaces: DestroySurfaces,
    create_context: CreateContext,
    destroy_context: DestroyContext,
    create_buffer: CreateBuffer,
    destroy_buffer: DestroyBuffer,
    map_buffer: MapBuffer,
    unmap_buffer: UnmapBuffer,
    begin_picture: BeginPicture,
    render_picture: RenderPicture,
    end_picture: EndPicture,
    sync_surface: SyncSurface,
    create_image: CreateImage,
    get_image: GetImage,
    destroy_image: DestroyImage,
}

/// libva, if it loads (`libva2` and `libva-drm2`).
pub fn api() -> Result<&'static Api> {
    static API: OnceLock<std::result::Result<Api, String>> = OnceLock::new();
    API.get_or_init(|| load().map_err(|e| format!("{e:#}")))
        .as_ref()
        .map_err(|e| anyhow!("{e}"))
}

fn load() -> Result<Api> {
    let va = Library::open(&["libva.so.2"]).context("VA-API isn't installed (libva2)")?;
    let drm = Library::open(&["libva-drm.so.2"]).context("VA-API isn't installed (libva-drm2)")?;
    // SAFETY: the signatures are va.h's, va_drm.h's.
    unsafe {
        Ok(Api {
            get_display_drm: drm.symbol(c"vaGetDisplayDRM")?,
            initialize: va.symbol(c"vaInitialize")?,
            terminate: va.symbol(c"vaTerminate")?,
            query_vendor_string: va.symbol(c"vaQueryVendorString")?,
            error_str: va.symbol(c"vaErrorStr")?,
            max_profiles: va.symbol(c"vaMaxNumProfiles")?,
            query_config_profiles: va.symbol(c"vaQueryConfigProfiles")?,
            max_entrypoints: va.symbol(c"vaMaxNumEntrypoints")?,
            query_config_entrypoints: va.symbol(c"vaQueryConfigEntrypoints")?,
            get_config_attributes: va.symbol(c"vaGetConfigAttributes")?,
            create_config: va.symbol(c"vaCreateConfig")?,
            destroy_config: va.symbol(c"vaDestroyConfig")?,
            create_surfaces: va.symbol(c"vaCreateSurfaces")?,
            destroy_surfaces: va.symbol(c"vaDestroySurfaces")?,
            create_context: va.symbol(c"vaCreateContext")?,
            destroy_context: va.symbol(c"vaDestroyContext")?,
            create_buffer: va.symbol(c"vaCreateBuffer")?,
            destroy_buffer: va.symbol(c"vaDestroyBuffer")?,
            map_buffer: va.symbol(c"vaMapBuffer")?,
            unmap_buffer: va.symbol(c"vaUnmapBuffer")?,
            begin_picture: va.symbol(c"vaBeginPicture")?,
            render_picture: va.symbol(c"vaRenderPicture")?,
            end_picture: va.symbol(c"vaEndPicture")?,
            sync_surface: va.symbol(c"vaSyncSurface")?,
            create_image: va.symbol(c"vaCreateImage")?,
            get_image: va.symbol(c"vaGetImage")?,
            destroy_image: va.symbol(c"vaDestroyImage")?,
            _va: va,
            _drm: drm,
        })
    }
}

/// `VAImageFormat` and `VAImage` (`va.h`), for the debug read-back only.
/// Not in `sys.rs`: written from the header as I know it, with the size
/// (120) asserted in a test; check it against the real header before
/// trusting a dump.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ImageFormat {
    fourcc: u32,
    byte_order: u32,
    bits_per_pixel: u32,
    depth: u32,
    red_mask: u32,
    green_mask: u32,
    blue_mask: u32,
    alpha_mask: u32,
    va_reserved: [u32; 4],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Image {
    image_id: u32,
    // `va.h` order: the format comes before the buffer (offsets 4 and 52).
    format: ImageFormat,
    buf: u32,
    width: u16,
    height: u16,
    data_size: u32,
    num_planes: u32,
    pitches: [u32; 3],
    offsets: [u32; 3],
    num_palette_entries: i32,
    entry_bytes: i32,
    component_order: [i8; 4],
    va_reserved: [u32; 4],
}

impl SurfaceAttrib {
    fn integer(kind: c_uint, value: c_uint) -> Self {
        Self {
            type_: kind,
            flags: SURFACE_ATTRIB_SETTABLE,
            value: sys::VAGenericValue {
                type_: GENERIC_INTEGER,
                value: sys::_VAGenericValue__bindgen_ty_1 { i: value as i32 },
            },
        }
    }

    fn pointer(kind: c_uint, value: *mut c_void) -> Self {
        Self {
            type_: kind,
            flags: SURFACE_ATTRIB_SETTABLE,
            value: sys::VAGenericValue {
                type_: GENERIC_POINTER,
                value: sys::_VAGenericValue__bindgen_ty_1 { p: value },
            },
        }
    }
}

/// The VA fourcc of a DRM format we render into.
fn va_fourcc(code: Fourcc) -> Option<u32> {
    match code {
        // DRM names formats by the little-endian word; VA by the byte order.
        Fourcc::Xrgb8888 => Some(fourcc(b'B', b'G', b'R', b'X')),
        Fourcc::Argb8888 => Some(fourcc(b'B', b'G', b'R', b'A')),
        Fourcc::Xbgr8888 => Some(fourcc(b'R', b'G', b'B', b'X')),
        Fourcc::Abgr8888 => Some(fourcc(b'R', b'G', b'B', b'A')),
        _ => None,
    }
}

/// The video processor's job: `surface` into `output_region` of the target
/// as BT.709 limited-range YCbCr. Every other field is zero (no filters, no
/// references, no rotation).
pub fn proc_pipeline(surface: Id, output_region: &Rectangle, background: u32) -> ProcPipeline {
    // SAFETY: integers, null pointers and a zero enum value: all-zero is valid.
    let mut p: ProcPipeline = unsafe { std::mem::zeroed() };
    p.surface = surface;
    p.surface_color_standard = COLOR_STANDARD_SRGB;
    p.output_region = output_region;
    p.output_background_color = background;
    p.output_color_standard = COLOR_STANDARD_BT709;
    // The source is full-range RGB; the output is studio swing BT.709.
    p.input_color_properties.color_range = sys::VA_SOURCE_RANGE_FULL as u8;
    p.output_color_properties.color_range = sys::VA_SOURCE_RANGE_REDUCED as u8;
    p.output_color_properties.colour_primaries = 1;
    p.output_color_properties.transfer_characteristics = 1;
    p.output_color_properties.matrix_coefficients = 1;
    p
}

/// One VA display on a render node.
pub struct Display {
    api: &'static Api,
    raw: *mut c_void,
    // The display belongs to this fd: kept open until `vaTerminate`.
    _file: File,
}

// SAFETY: a VADisplay may be used from any one thread at a time; ours is
// used by its encoder's thread (or the thread that probes).
unsafe impl Send for Display {}

impl Display {
    pub fn open(render_node: &Path) -> Result<Self> {
        let api = api()?;
        let file = File::options()
            .read(true)
            .write(true)
            .open(render_node)
            .with_context(|| format!("opening {}", render_node.display()))?;
        // SAFETY: va_drm.h's signature; the fd outlives the display.
        let raw = unsafe { (api.get_display_drm)(file.as_raw_fd()) };
        ensure!(
            !raw.is_null(),
            "libva couldn't use {}",
            render_node.display()
        );
        let (mut major, mut minor) = (0, 0);
        // SAFETY: a display from vaGetDisplayDRM.
        let status = unsafe { (api.initialize)(raw, &mut major, &mut minor) };
        ensure!(
            status == 0,
            "vaInitialize on {} failed ({}): no VA-API driver for this device",
            render_node.display(),
            api.describe(status)
        );
        Ok(Self {
            api,
            raw,
            _file: file,
        })
    }

    pub fn vendor(&self) -> String {
        // SAFETY: an initialised display; a static string.
        let text = unsafe { (self.api.query_vendor_string)(self.raw) };
        if text.is_null() {
            return String::new();
        }
        // SAFETY: checked non-null; NUL-terminated.
        unsafe { CStr::from_ptr(text) }
            .to_string_lossy()
            .into_owned()
    }

    pub fn profiles(&self) -> Result<Vec<c_int>> {
        // SAFETY: an initialised display; the buffer has the size libva asks.
        unsafe {
            let mut profiles = vec![0; (self.api.max_profiles)(self.raw).max(0) as usize];
            let mut count = 0;
            let status =
                (self.api.query_config_profiles)(self.raw, profiles.as_mut_ptr(), &mut count);
            self.check(status, "vaQueryConfigProfiles")?;
            profiles.truncate(count.max(0) as usize);
            Ok(profiles)
        }
    }

    /// The entrypoints of `profile`; empty if it can't be queried.
    pub fn entrypoints(&self, profile: c_int) -> Vec<c_int> {
        // SAFETY: as above.
        unsafe {
            let mut entrypoints = vec![0; (self.api.max_entrypoints)(self.raw).max(0) as usize];
            let mut count = 0;
            let status = (self.api.query_config_entrypoints)(
                self.raw,
                profile,
                entrypoints.as_mut_ptr(),
                &mut count,
            );
            if status != 0 {
                return Vec::new();
            }
            entrypoints.truncate(count.max(0) as usize);
            entrypoints
        }
    }

    /// The values of `kinds` for a profile and entrypoint
    /// ([`ATTRIB_NOT_SUPPORTED`] for what the driver lacks).
    pub fn attributes(&self, profile: c_int, entrypoint: c_int, kinds: &[c_uint]) -> Vec<c_uint> {
        let mut attribs: Vec<ConfigAttrib> = kinds
            .iter()
            .map(|&kind| ConfigAttrib {
                type_: kind,
                value: ATTRIB_NOT_SUPPORTED,
            })
            .collect();
        // SAFETY: a valid array of the stated length.
        let status = unsafe {
            (self.api.get_config_attributes)(
                self.raw,
                profile,
                entrypoint,
                attribs.as_mut_ptr(),
                attribs.len() as c_int,
            )
        };
        if status != 0 {
            return vec![ATTRIB_NOT_SUPPORTED; kinds.len()];
        }
        attribs.iter().map(|a| a.value).collect()
    }

    pub fn create_config(
        &self,
        profile: c_int,
        entrypoint: c_int,
        attribs: &[(c_uint, c_uint)],
    ) -> Result<Id> {
        let mut list: Vec<ConfigAttrib> = attribs
            .iter()
            .map(|&(kind, value)| ConfigAttrib { type_: kind, value })
            .collect();
        let mut id = INVALID_ID;
        // SAFETY: a valid array of the stated length.
        let status = unsafe {
            (self.api.create_config)(
                self.raw,
                profile,
                entrypoint,
                list.as_mut_ptr(),
                list.len() as c_int,
                &mut id,
            )
        };
        self.check(status, "vaCreateConfig")?;
        Ok(id)
    }

    pub fn destroy_config(&self, config: Id) {
        // SAFETY: a config of this display; failures at teardown don't matter.
        unsafe { (self.api.destroy_config)(self.raw, config) };
    }

    /// `count` NV12 surfaces of `width`×`height`.
    pub fn create_nv12_surfaces(&self, width: u32, height: u32, count: usize) -> Result<Vec<Id>> {
        let mut attribs = [SurfaceAttrib::integer(
            SURFACE_ATTRIB_PIXEL_FORMAT,
            FOURCC_NV12,
        )];
        let mut surfaces = vec![INVALID_ID; count];
        // SAFETY: valid arrays of the stated lengths.
        let status = unsafe {
            (self.api.create_surfaces)(
                self.raw,
                RT_FORMAT_YUV420,
                width,
                height,
                surfaces.as_mut_ptr(),
                count as c_uint,
                attribs.as_mut_ptr(),
                attribs.len() as c_uint,
            )
        };
        self.check(status, "vaCreateSurfaces (NV12)")?;
        Ok(surfaces)
    }

    /// A VA surface over the dmabuf's memory, no copy
    /// (`VA_SURFACE_ATTRIB_MEM_TYPE_DRM_PRIME_2`). The driver takes its own
    /// reference to the buffer; the fds stay ours.
    pub fn import_dmabuf(&self, dmabuf: &Dmabuf) -> Result<Id> {
        let format = dmabuf.format();
        let fourcc = va_fourcc(format.code).with_context(|| {
            format!(
                "{:?} isn't a format VA-API takes as a render target",
                format.code
            )
        })?;
        let size = dmabuf.size();
        let (width, height) = (size.w as u32, size.h as u32);
        let planes = dmabuf.num_planes();
        ensure!(
            (1..=4).contains(&planes),
            "a dmabuf with {planes} planes isn't something VA-API takes"
        );
        // SAFETY: integers only: all-zero is a valid descriptor.
        let mut desc: DrmPrimeDescriptor = unsafe { std::mem::zeroed() };
        desc.fourcc = fourcc;
        desc.width = width;
        desc.height = height;
        desc.num_objects = planes as u32;
        desc.num_layers = 1;
        let modifier: u64 = format.modifier.into();
        let strides: Vec<u32> = dmabuf.strides().collect();
        let offsets: Vec<u32> = dmabuf.offsets().collect();
        let mut layer = DrmLayer {
            drm_format: format.code as u32,
            num_planes: planes as u32,
            object_index: [0; 4],
            offset: [0; 4],
            pitch: [0; 4],
        };
        for (i, fd) in dmabuf.handles().enumerate() {
            let raw = fd.as_raw_fd();
            // The object's size is the buffer's: a dmabuf's file size.
            // SAFETY: lseek on a live fd.
            let end = unsafe { libc::lseek(raw, 0, libc::SEEK_END) };
            let size = if end > 0 {
                end as u32
            } else {
                strides[i] * height
            };
            desc.objects[i] = DrmObject {
                fd: raw,
                size,
                drm_format_modifier: modifier,
            };
            layer.object_index[i] = i as u32;
            layer.offset[i] = offsets[i];
            layer.pitch[i] = strides[i];
        }
        desc.layers[0] = layer;
        let mut attribs = [
            SurfaceAttrib::integer(SURFACE_ATTRIB_MEMORY_TYPE, MEM_TYPE_DRM_PRIME_2),
            SurfaceAttrib::pointer(
                SURFACE_ATTRIB_EXTERNAL_BUFFER_DESCRIPTOR,
                (&mut desc as *mut DrmPrimeDescriptor).cast(),
            ),
        ];
        let mut surface = INVALID_ID;
        // SAFETY: valid arrays; the descriptor outlives the call.
        let status = unsafe {
            (self.api.create_surfaces)(
                self.raw,
                RT_FORMAT_RGB32,
                width,
                height,
                &mut surface,
                1,
                attribs.as_mut_ptr(),
                attribs.len() as c_uint,
            )
        };
        self.check(
            status,
            &format!(
                "vaCreateSurfaces (importing a {:?} dmabuf, modifier {:#x})",
                format.code, modifier
            ),
        )?;
        Ok(surface)
    }

    /// The NV12 `surface`'s top-left `width`×`height`, as a packed NV12
    /// picture (Y rows, then interleaved UV rows, no padding): a debug
    /// read-back through `vaGetImage`.
    pub fn read_nv12(&self, surface: Id, width: u32, height: u32) -> Result<Vec<u8>> {
        let mut format = ImageFormat {
            fourcc: FOURCC_NV12,
            byte_order: sys::VA_LSB_FIRST,
            bits_per_pixel: 12,
            depth: 0,
            red_mask: 0,
            green_mask: 0,
            blue_mask: 0,
            alpha_mask: 0,
            va_reserved: [0; 4],
        };
        // SAFETY: plain integers: all-zero is a valid image.
        let mut image: Image = unsafe { std::mem::zeroed() };
        // SAFETY: valid pointers; sizes as libva asks.
        let status = unsafe {
            (self.api.create_image)(
                self.raw,
                &mut format,
                width as c_int,
                height as c_int,
                &mut image,
            )
        };
        self.check(status, "vaCreateImage (NV12)")?;
        let result = (|| {
            // SAFETY: a surface and an image of this display.
            let status = unsafe {
                (self.api.get_image)(self.raw, surface, 0, 0, width, height, image.image_id)
            };
            self.check(status, "vaGetImage")?;
            let mut mapped: *mut c_void = std::ptr::null_mut();
            // SAFETY: the image's buffer, mapped until the unmap.
            let status = unsafe { (self.api.map_buffer)(self.raw, image.buf, &mut mapped) };
            self.check(status, "vaMapBuffer (image)")?;
            let base = mapped.cast::<u8>();
            let mut out = Vec::with_capacity(width as usize * height as usize * 3 / 2);
            // SAFETY: the mapping holds `data_size` bytes laid out by the
            // image's pitches and offsets.
            unsafe {
                for y in 0..height as usize {
                    let row = base.add(image.offsets[0] as usize + y * image.pitches[0] as usize);
                    out.extend_from_slice(std::slice::from_raw_parts(row, width as usize));
                }
                for y in 0..height as usize / 2 {
                    let row = base.add(image.offsets[1] as usize + y * image.pitches[1] as usize);
                    out.extend_from_slice(std::slice::from_raw_parts(row, width as usize));
                }
                (self.api.unmap_buffer)(self.raw, image.buf);
            }
            Ok(out)
        })();
        // SAFETY: an image of this display.
        unsafe { (self.api.destroy_image)(self.raw, image.image_id) };
        result
    }

    pub fn destroy_surfaces(&self, surfaces: &[Id]) {
        if surfaces.is_empty() {
            return;
        }
        let mut list = surfaces.to_vec();
        // SAFETY: surfaces of this display.
        unsafe { (self.api.destroy_surfaces)(self.raw, list.as_mut_ptr(), list.len() as c_int) };
    }

    pub fn create_context(
        &self,
        config: Id,
        width: u32,
        height: u32,
        targets: &[Id],
    ) -> Result<Id> {
        let mut list = targets.to_vec();
        let mut id = INVALID_ID;
        // SAFETY: a valid array of the stated length.
        let status = unsafe {
            (self.api.create_context)(
                self.raw,
                config,
                width as c_int,
                height as c_int,
                PROGRESSIVE,
                list.as_mut_ptr(),
                list.len() as c_int,
                &mut id,
            )
        };
        self.check(status, "vaCreateContext")?;
        Ok(id)
    }

    pub fn destroy_context(&self, context: Id) {
        // SAFETY: a context of this display.
        unsafe { (self.api.destroy_context)(self.raw, context) };
    }

    /// A buffer holding a copy of `size` bytes at `data` (null: just space
    /// of that size, for the coded buffer).
    ///
    /// # Safety
    /// `data` is null or readable for `size` bytes.
    pub unsafe fn create_buffer_raw(
        &self,
        context: Id,
        kind: c_int,
        size: usize,
        data: *mut c_void,
    ) -> Result<Id> {
        let mut id = INVALID_ID;
        // SAFETY: the caller vouches for `data`.
        let status = unsafe {
            (self.api.create_buffer)(self.raw, context, kind, size as c_uint, 1, data, &mut id)
        };
        self.check(status, &format!("vaCreateBuffer (type {kind})"))?;
        Ok(id)
    }

    /// A buffer with a copy of `value`.
    pub fn create_buffer<T>(&self, context: Id, kind: c_int, value: &T) -> Result<Id> {
        // SAFETY: a reference is readable for its size.
        unsafe {
            self.create_buffer_raw(
                context,
                kind,
                size_of::<T>(),
                (value as *const T).cast_mut().cast(),
            )
        }
    }

    /// A buffer with a copy of `bytes`.
    pub fn create_buffer_bytes(&self, context: Id, kind: c_int, bytes: &[u8]) -> Result<Id> {
        // SAFETY: a slice is readable for its length.
        unsafe {
            self.create_buffer_raw(context, kind, bytes.len(), bytes.as_ptr().cast_mut().cast())
        }
    }

    pub fn destroy_buffer(&self, buffer: Id) {
        // SAFETY: a buffer of this display.
        unsafe { (self.api.destroy_buffer)(self.raw, buffer) };
    }

    pub fn begin_picture(&self, context: Id, target: Id) -> Result<()> {
        // SAFETY: ids of this display.
        let status = unsafe { (self.api.begin_picture)(self.raw, context, target) };
        self.check(status, "vaBeginPicture")
    }

    pub fn render_picture(&self, context: Id, buffers: &[Id]) -> Result<()> {
        let mut list = buffers.to_vec();
        // SAFETY: a valid array of the stated length.
        let status = unsafe {
            (self.api.render_picture)(self.raw, context, list.as_mut_ptr(), list.len() as c_int)
        };
        self.check(status, "vaRenderPicture")
    }

    pub fn end_picture(&self, context: Id) -> Result<()> {
        // SAFETY: a context of this display.
        let status = unsafe { (self.api.end_picture)(self.raw, context) };
        self.check(status, "vaEndPicture")
    }

    pub fn sync_surface(&self, surface: Id) -> Result<()> {
        // SAFETY: a surface of this display.
        let status = unsafe { (self.api.sync_surface)(self.raw, surface) };
        self.check(status, "vaSyncSurface")
    }

    /// Appends the coded buffer's bytes (every segment) to `out`.
    pub fn read_coded_buffer(&self, buffer: Id, out: &mut Vec<u8>) -> Result<()> {
        let mut mapped: *mut c_void = std::ptr::null_mut();
        // SAFETY: a coded buffer of this display.
        let status = unsafe { (self.api.map_buffer)(self.raw, buffer, &mut mapped) };
        self.check(status, "vaMapBuffer (coded buffer)")?;
        let mut segment = mapped.cast::<CodedSegment>();
        // SAFETY: the mapping is a linked list of VACodedBufferSegment that
        // stays valid until the unmap.
        unsafe {
            while !segment.is_null() {
                let s = &*segment;
                if !s.buf.is_null() && s.size > 0 {
                    out.extend_from_slice(std::slice::from_raw_parts(
                        s.buf.cast::<u8>(),
                        s.size as usize,
                    ));
                }
                segment = s.next.cast::<CodedSegment>();
            }
            (self.api.unmap_buffer)(self.raw, buffer);
        }
        Ok(())
    }

    fn check(&self, status: c_int, what: &str) -> Result<()> {
        if status == 0 {
            Ok(())
        } else {
            bail!("{what} failed: {}", self.api.describe(status))
        }
    }
}

impl Api {
    /// `invalid parameter (VA status 2)`: libva's text and the number.
    fn describe(&self, status: c_int) -> String {
        // SAFETY: vaErrorStr returns a static string.
        let text = unsafe { (self.error_str)(status) };
        if text.is_null() {
            return format!("VA status {status}");
        }
        // SAFETY: checked non-null; NUL-terminated.
        let text = unsafe { CStr::from_ptr(text) }.to_string_lossy();
        format!("{text} (VA status {status})")
    }
}

impl Drop for Display {
    fn drop(&mut self) {
        // SAFETY: the initialised display, terminated once; the fd closes
        // after (field order).
        unsafe { (self.api.terminate)(self.raw) };
    }
}

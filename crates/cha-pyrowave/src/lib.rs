//! `cha-pyrowave`: our binding to PyroWave (Hans-Kristian Arntzen's wavelet
//! codec, MIT), for the LAN tier (plan §3.2).
//!
//! `libpyrowave-shared` is loaded at runtime, so the streamer runs without it
//! (the hardware tier needs nothing of it). The library makes its own Vulkan
//! device on the same GPU; our composited GBM buffers come in as dma-bufs
//! with their DRM modifier, imported once each. Encoding takes RGB directly
//! (PyroWave converts to YCbCr on the GPU) and splits the frame into network
//! packets that each decode on their own, so losing one blurs a region
//! instead of breaking the frame.
//!
//! Pinned: upstream `89f7e47` (API 0.6), the bitstream the browser decoder
//! (`@cha/pyrowave-webgpu`) is pinned to as well.

mod ffi;

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::os::fd::{AsRawFd, BorrowedFd};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use ffi::*;

#[derive(Debug, Clone)]
pub struct Error(String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

fn check(what: &str, result: pyrowave_result) -> Result<()> {
    let why = match result {
        PYROWAVE_SUCCESS => return Ok(()),
        -2 => "invalid argument",
        -3 => "out of host memory",
        -4 => "out of GPU memory",
        // Usually the driver's Vulkan ICD failing to load (a missing
        // library); VK_LOADER_DEBUG=error,driver says which.
        -5 => "no Vulkan driver",
        -6 => "not implemented",
        -7 => "unsupported external handle",
        -8 => "the external handle didn't import",
        _ => "error",
    };
    Err(Error(format!("{what} failed: {why} ({result})")))
}

const LIBRARY: &[&str] = &["libpyrowave-shared.so.0", "libpyrowave-shared.so"];

struct Loaded {
    _handle: *mut c_void,
    api: Api,
}

// SAFETY: a dlopen handle and function pointers may be used from any thread.
unsafe impl Send for Loaded {}
unsafe impl Sync for Loaded {}

fn api() -> Result<&'static Api> {
    static LOADED: OnceLock<std::result::Result<Loaded, Error>> = OnceLock::new();
    LOADED
        .get_or_init(load)
        .as_ref()
        .map(|l| &l.api)
        .map_err(Clone::clone)
}

fn load() -> Result<Loaded> {
    let mut errors = Vec::new();
    for name in LIBRARY {
        let c_name = CString::new(*name).expect("no NUL");
        // SAFETY: a valid C string; RTLD_NOW resolves everything up front.
        let handle = unsafe { libc::dlopen(c_name.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
        if handle.is_null() {
            // SAFETY: dlerror returns null or a valid C string.
            let err = unsafe { libc::dlerror() };
            if !err.is_null() {
                // SAFETY: as above.
                errors.push(
                    unsafe { CStr::from_ptr(err) }
                        .to_string_lossy()
                        .into_owned(),
                );
            }
            continue;
        }
        let symbol = |name: &CStr| -> Result<*mut c_void> {
            // SAFETY: a live handle and a valid C string.
            let ptr = unsafe { libc::dlsym(handle, name.as_ptr()) };
            if ptr.is_null() {
                Err(Error(format!(
                    "{} has no {}",
                    c_name.to_string_lossy(),
                    name.to_string_lossy()
                )))
            } else {
                Ok(ptr)
            }
        };
        macro_rules! sym {
            ($name:literal) => {
                // SAFETY: the symbol's type is the one declared in `Api`,
                // from pyrowave.h.
                unsafe { std::mem::transmute_copy::<*mut c_void, _>(&symbol($name)?) }
            };
        }
        let api = Api {
            get_api_version: sym!(c"pyrowave_get_api_version"),
            create_device_by_compat2: sym!(c"pyrowave_create_device_by_compat2"),
            device_destroy: sym!(c"pyrowave_device_destroy"),
            device_report_performance_stats: sym!(c"pyrowave_device_report_performance_stats"),
            image_create: sym!(c"pyrowave_image_create"),
            image_get_image_view: sym!(c"pyrowave_image_get_image_view"),
            image_destroy: sym!(c"pyrowave_image_destroy"),
            encoder_create: sym!(c"pyrowave_encoder_create"),
            encoder_encode_gpu_scaled_synchronous: sym!(
                c"pyrowave_encoder_encode_gpu_scaled_synchronous"
            ),
            encoder_compute_num_packets: sym!(c"pyrowave_encoder_compute_num_packets"),
            encoder_packetize: sym!(c"pyrowave_encoder_packetize"),
            encoder_destroy: sym!(c"pyrowave_encoder_destroy"),
        };
        let (mut major, mut minor, mut patch) = (0, 0, 0);
        // SAFETY: three valid out pointers.
        unsafe { (api.get_api_version)(&mut major, &mut minor, &mut patch) };
        if (major, minor) != (0, 6) {
            return Err(Error(format!(
                "libpyrowave API {major}.{minor}.{patch}; this build needs 0.6"
            )));
        }
        return Ok(Loaded {
            _handle: handle,
            api,
        });
    }
    Err(Error(format!(
        "couldn't load libpyrowave: {}",
        errors.join("; ")
    )))
}

/// Whether libpyrowave loads (and is the API this build speaks).
pub fn available() -> Result<()> {
    api().map(|_| ())
}

/// PyroWave's Vulkan device, on the GPU with this PCI vendor and device id.
/// Encoders on several threads can share one: their calls take turns.
pub struct Device {
    raw: pyrowave_device,
    api: &'static Api,
    /// Held across every call that touches the device (Granite's device
    /// isn't to be driven from two threads at once).
    lock: Mutex<()>,
}

// SAFETY: every call into the device, and into its images and encoders,
// holds `lock`.
unsafe impl Send for Device {}
unsafe impl Sync for Device {}

impl Device {
    pub fn new(vendor: u32, device: u32) -> Result<Arc<Self>> {
        let api = api()?;
        let mut raw = std::ptr::null_mut();
        // SAFETY: plain values and a valid out pointer.
        check("pyrowave_create_device", unsafe {
            (api.create_device_by_compat2)(
                vendor,
                device,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                VK_QUEUE_GLOBAL_PRIORITY_MEDIUM,
                &mut raw,
            )
        })?;
        Ok(Arc::new(Self {
            raw,
            api,
            lock: Mutex::new(()),
        }))
    }

    fn exclusive(&self) -> MutexGuard<'_, ()> {
        self.lock.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// GPU timings of the passes since the last call, one line each.
    pub fn performance_report(&self) -> Vec<String> {
        unsafe extern "C" fn collect(userdata: *mut c_void, msg: *const c_char) {
            // SAFETY: userdata is the Vec below; msg is a C string for the call.
            let lines = unsafe { &mut *userdata.cast::<Vec<String>>() };
            lines.push(
                unsafe { CStr::from_ptr(msg) }
                    .to_string_lossy()
                    .trim()
                    .to_string(),
            );
        }
        let mut lines: Vec<String> = Vec::new();
        let _turn = self.exclusive();
        // SAFETY: a live device; the callback only runs inside this call.
        unsafe {
            (self.api.device_report_performance_stats)(
                self.raw,
                collect,
                (&mut lines as *mut Vec<String>).cast(),
                true,
            )
        };
        lines
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        // SAFETY: created by pyrowave_create_device_by_compat2, destroyed once;
        // images and encoders hold an Arc, so they're gone already.
        unsafe { (self.api.device_destroy)(self.raw) };
    }
}

/// A buffer the compositor rendered: one plane of 8-bit RGB(A).
pub struct Dmabuf<'a> {
    pub fd: BorrowedFd<'a>,
    pub width: u32,
    pub height: u32,
    /// DRM fourcc, e.g. `AR24` (ARGB8888).
    pub fourcc: u32,
    pub modifier: u64,
    pub offset: u32,
    pub stride: u32,
}

/// A dmabuf imported into PyroWave's device.
pub struct Image {
    raw: pyrowave_image,
    width: u32,
    height: u32,
    device: Arc<Device>,
}

// SAFETY: used by one encoder thread at a time.
unsafe impl Send for Image {}

impl Image {
    pub fn import(device: &Arc<Device>, buffer: &Dmabuf<'_>) -> Result<Self> {
        let format = match &buffer.fourcc.to_le_bytes() {
            b"AR24" | b"XR24" => VK_FORMAT_B8G8R8A8_UNORM,
            b"AB24" | b"XB24" => VK_FORMAT_R8G8B8A8_UNORM,
            other => {
                return Err(Error(format!(
                    "can't encode {} buffers",
                    String::from_utf8_lossy(other)
                )));
            }
        };
        let plane = VkSubresourceLayout {
            offset: buffer.offset.into(),
            size: 0,
            row_pitch: buffer.stride.into(),
            array_pitch: 0,
            depth_pitch: 0,
        };
        let modifier = VkImageDrmFormatModifierExplicitCreateInfoEXT {
            s_type: VK_STRUCTURE_TYPE_IMAGE_DRM_FORMAT_MODIFIER_EXPLICIT_CREATE_INFO_EXT,
            p_next: std::ptr::null(),
            drm_format_modifier: buffer.modifier,
            drm_format_modifier_plane_count: 1,
            p_plane_layouts: &plane,
        };
        let image = VkImageCreateInfo {
            s_type: VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO,
            p_next: (&modifier as *const VkImageDrmFormatModifierExplicitCreateInfoEXT).cast(),
            flags: 0,
            image_type: VK_IMAGE_TYPE_2D,
            format,
            extent: VkExtent3D {
                width: buffer.width,
                height: buffer.height,
                depth: 1,
            },
            mip_levels: 1,
            array_layers: 1,
            samples: VK_SAMPLE_COUNT_1_BIT,
            tiling: VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT,
            usage: VK_IMAGE_USAGE_SAMPLED_BIT,
            sharing_mode: VK_SHARING_MODE_EXCLUSIVE,
            queue_family_index_count: 0,
            p_queue_family_indices: std::ptr::null(),
            initial_layout: VK_IMAGE_LAYOUT_UNDEFINED,
        };
        // Vulkan takes the descriptor it imports; ours stays the compositor's.
        // SAFETY: dup of a valid descriptor.
        let fd = unsafe { libc::dup(buffer.fd.as_raw_fd()) };
        if fd < 0 {
            return Err(Error(format!("dup: {}", std::io::Error::last_os_error())));
        }
        let info = pyrowave_image_create_info {
            device: device.raw,
            external_handle: fd as usize,
            handle_type: VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT,
            image_create_info: &image,
        };
        let mut raw = std::ptr::null_mut();
        let turn = device.exclusive();
        // SAFETY: every pointer is valid for the call.
        let result = unsafe { (device.api.image_create)(&info, &mut raw) };
        drop(turn);
        if result != PYROWAVE_SUCCESS {
            // SAFETY: not imported, still ours.
            unsafe { libc::close(fd) };
        }
        check(
            &format!(
                "importing the {}×{} {} dmabuf (modifier {:#x}, stride {}, offset {})",
                buffer.width,
                buffer.height,
                String::from_utf8_lossy(&buffer.fourcc.to_le_bytes()),
                buffer.modifier,
                buffer.stride,
                buffer.offset
            ),
            result,
        )?;
        Ok(Self {
            raw,
            width: buffer.width,
            height: buffer.height,
            device: Arc::clone(device),
        })
    }
}

impl Drop for Image {
    fn drop(&mut self) {
        let _turn = self.device.exclusive();
        // SAFETY: created by pyrowave_image_create, destroyed once.
        unsafe { (self.device.api.image_destroy)(self.raw) };
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Chroma {
    Yuv420,
    Yuv444,
}

/// One packet of an encoded frame: `bytes[offset..offset + len]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Packet {
    pub offset: u32,
    pub len: u32,
}

pub struct Encoder {
    raw: pyrowave_encoder,
    device: Arc<Device>,
    width: u32,
    height: u32,
    packets: Vec<pyrowave_packet>,
}

// SAFETY: the encoder's entry points aren't thread safe; it moves between
// threads but is used by one at a time (`&mut self`).
unsafe impl Send for Encoder {}

impl Encoder {
    pub fn new(device: &Arc<Device>, width: u32, height: u32, chroma: Chroma) -> Result<Self> {
        let info = pyrowave_encoder_create_info {
            device: device.raw,
            width: width as c_int,
            height: height as c_int,
            chroma: match chroma {
                Chroma::Yuv420 => PYROWAVE_CHROMA_SUBSAMPLING_420,
                Chroma::Yuv444 => PYROWAVE_CHROMA_SUBSAMPLING_444,
            },
        };
        let mut raw = std::ptr::null_mut();
        let turn = device.exclusive();
        // SAFETY: valid pointers for the call.
        check("pyrowave_encoder_create", unsafe {
            (device.api.encoder_create)(&info, &mut raw)
        })?;
        drop(turn);
        Ok(Self {
            raw,
            device: Arc::clone(device),
            width,
            height,
            packets: Vec::new(),
        })
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Encodes `image` within `max_bytes`, then splits it into packets of at
    /// most `boundary` bytes: the bitstream goes into `bytes`, its packets
    /// into `packets`. Blocks until the GPU is done.
    pub fn encode(
        &mut self,
        image: &Image,
        max_bytes: usize,
        boundary: usize,
        bytes: &mut Vec<u8>,
        packets: &mut Vec<Packet>,
    ) -> Result<()> {
        if (image.width, image.height) != (self.width, self.height) {
            return Err(Error(format!(
                "a {}×{} image for a {}×{} encoder",
                image.width, image.height, self.width, self.height
            )));
        }
        let api = self.device.api;
        // Through packetizing: the bitstream lives in the device's buffers.
        let _turn = self.device.exclusive();
        let mut view = pyrowave_image_view::default();
        // SAFETY: a live image and a valid out pointer.
        check("pyrowave_image_get_image_view", unsafe {
            (api.image_get_image_view)(
                image.raw,
                VK_IMAGE_ASPECT_COLOR_BIT,
                VK_IMAGE_USAGE_SAMPLED_BIT as c_int,
                &mut view,
            )
        })?;
        // The compositor has finished with the buffer (it waits on its
        // fence); ownership moves from the foreign (GL) side and back.
        let foreign = pyrowave_gpu_external_reference {
            image: image.raw,
            queue_family_index: VK_QUEUE_FAMILY_FOREIGN_EXT,
        };
        let ownership = pyrowave_gpu_sync_operation {
            images: &foreign,
            num_images: 1,
            sync: pyrowave_sync_point {
                semaphore: 0,
                value: 0,
            },
        };
        let info = pyrowave_scaled_encode_info {
            view,
            input_color_space: VK_COLOR_SPACE_SRGB_NONLINEAR_KHR,
            output_color_space: VK_COLOR_SPACE_SRGB_NONLINEAR_KHR,
            intermediate_plane_format: VK_FORMAT_R8_UNORM,
            ycbcr_chroma_midpoint: 0.5,
            force_linear_filtering: false,
            skip_dither: false,
            crop_rect: std::ptr::null(),
        };
        let rate = pyrowave_rate_control {
            maximum_bitstream_size: max_bytes,
        };
        // SAFETY: every pointer is valid for the call.
        check("encoding", unsafe {
            (api.encoder_encode_gpu_scaled_synchronous)(
                self.raw, &ownership, &ownership, &info, &rate,
            )
        })?;
        let mut count = 0;
        // SAFETY: valid out pointer; blocks until the encode is done.
        check("counting packets", unsafe {
            (api.encoder_compute_num_packets)(self.raw, boundary, &mut count)
        })?;
        self.packets.resize(count, pyrowave_packet::default());
        bytes.resize(count * boundary, 0);
        let mut written = 0;
        // SAFETY: `packets` holds `count` entries and `bytes` count × boundary.
        check("packetizing", unsafe {
            (api.encoder_packetize)(
                self.raw,
                self.packets.as_mut_ptr(),
                boundary,
                &mut written,
                bytes.as_mut_ptr().cast(),
                bytes.len(),
            )
        })?;
        packets.clear();
        let mut end = 0;
        for p in &self.packets[..written.min(count)] {
            packets.push(Packet {
                offset: p.offset as u32,
                len: p.size as u32,
            });
            end = end.max(p.offset + p.size);
        }
        bytes.truncate(end);
        Ok(())
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        let _turn = self.device.exclusive();
        // SAFETY: created by pyrowave_encoder_create, destroyed once.
        unsafe { (self.device.api.encoder_destroy)(self.raw) };
    }
}

//! The CUDA driver API calls the encoder needs: pick the GPU, hold its primary
//! context, and register EGL images so NVENC can read them in place.

use std::ffi::{CStr, c_char, c_int, c_uint, c_void};
use std::sync::{Arc, OnceLock};

use crate::dl::Library;
use crate::{Error, Result};

type CUresult = c_int;
type CUdevice = c_int;
pub(crate) type CUcontext = *mut c_void;
type CUgraphicsResource = *mut c_void;
type CUarray = *mut c_void;
type CUdeviceptr = u64;

const CUDA_SUCCESS: CUresult = 0;
const CU_GRAPHICS_REGISTER_FLAGS_READ_ONLY: c_uint = 1;
const CU_EGL_FRAME_TYPE_ARRAY: c_uint = 0;
const CU_EGL_FRAME_MAX_PLANES: usize = 3;

#[repr(C)]
#[derive(Clone, Copy)]
union CUeglFramePlanes {
    array: [CUarray; CU_EGL_FRAME_MAX_PLANES],
    pitch: [*mut c_void; CU_EGL_FRAME_MAX_PLANES],
}

/// `CUeglFrame` from `cuda.h` (CUDA 12).
#[repr(C)]
#[derive(Clone, Copy)]
struct CUeglFrame {
    frame: CUeglFramePlanes,
    width: c_uint,
    height: c_uint,
    depth: c_uint,
    pitch: c_uint,
    plane_count: c_uint,
    num_channels: c_uint,
    frame_type: c_uint,
    egl_color_format: c_uint,
    cu_format: c_uint,
}

struct Cuda {
    _lib: Library,
    device_get_count: unsafe extern "C" fn(*mut c_int) -> CUresult,
    device_get: unsafe extern "C" fn(*mut CUdevice, c_int) -> CUresult,
    device_get_name: unsafe extern "C" fn(*mut c_char, c_int, CUdevice) -> CUresult,
    device_get_pci_bus_id: unsafe extern "C" fn(*mut c_char, c_int, CUdevice) -> CUresult,
    primary_ctx_retain: unsafe extern "C" fn(*mut CUcontext, CUdevice) -> CUresult,
    primary_ctx_release: unsafe extern "C" fn(CUdevice) -> CUresult,
    ctx_push: unsafe extern "C" fn(CUcontext) -> CUresult,
    ctx_pop: unsafe extern "C" fn(*mut CUcontext) -> CUresult,
    egl_register_image:
        unsafe extern "C" fn(*mut CUgraphicsResource, *mut c_void, c_uint) -> CUresult,
    unregister_resource: unsafe extern "C" fn(CUgraphicsResource) -> CUresult,
    mapped_egl_frame:
        unsafe extern "C" fn(*mut CUeglFrame, CUgraphicsResource, c_uint, c_uint) -> CUresult,
    error_string: unsafe extern "C" fn(CUresult, *mut *const c_char) -> CUresult,
}

impl Cuda {
    /// The driver API, loaded and initialized once per process.
    fn get() -> Result<&'static Cuda> {
        static CUDA: OnceLock<Result<Cuda>> = OnceLock::new();
        CUDA.get_or_init(Self::load).as_ref().map_err(Clone::clone)
    }

    fn load() -> Result<Cuda> {
        let lib = Library::open(&["libcuda.so.1", "libcuda.so"])?;
        // SAFETY: each symbol's type matches its declaration in cuda.h.
        let cuda = unsafe {
            let init: unsafe extern "C" fn(c_uint) -> CUresult = lib.symbol(c"cuInit")?;
            let cuda = Cuda {
                device_get_count: lib.symbol(c"cuDeviceGetCount")?,
                device_get: lib.symbol(c"cuDeviceGet")?,
                device_get_name: lib.symbol(c"cuDeviceGetName")?,
                device_get_pci_bus_id: lib.symbol(c"cuDeviceGetPCIBusId")?,
                primary_ctx_retain: lib.symbol(c"cuDevicePrimaryCtxRetain")?,
                primary_ctx_release: lib.symbol(c"cuDevicePrimaryCtxRelease_v2")?,
                ctx_push: lib.symbol(c"cuCtxPushCurrent_v2")?,
                ctx_pop: lib.symbol(c"cuCtxPopCurrent_v2")?,
                egl_register_image: lib.symbol(c"cuGraphicsEGLRegisterImage")?,
                unregister_resource: lib.symbol(c"cuGraphicsUnregisterResource")?,
                mapped_egl_frame: lib.symbol(c"cuGraphicsResourceGetMappedEglFrame")?,
                error_string: lib.symbol(c"cuGetErrorString")?,
                _lib: lib,
            };
            cuda.check(init(0), "cuInit")?;
            cuda
        };
        Ok(cuda)
    }

    fn check(&self, result: CUresult, what: &str) -> Result<()> {
        if result == CUDA_SUCCESS {
            return Ok(());
        }
        let mut message: *const c_char = std::ptr::null();
        // SAFETY: cuGetErrorString writes a static string or leaves null.
        unsafe { (self.error_string)(result, &mut message) };
        let message = if message.is_null() {
            format!("error {result}")
        } else {
            // SAFETY: checked non-null.
            unsafe { CStr::from_ptr(message) }
                .to_string_lossy()
                .into_owned()
        };
        Err(Error::new(format!("{what} failed: {message}")))
    }
}

/// One GPU's primary CUDA context, which NVENC sessions and EGL registrations
/// share.
pub struct CudaContext {
    cuda: &'static Cuda,
    device: CUdevice,
    ctx: CUcontext,
    name: String,
    pci_slot: String,
}

// SAFETY: CUDA contexts may be made current on any thread; we always push and
// pop around use.
unsafe impl Send for CudaContext {}
unsafe impl Sync for CudaContext {}

impl CudaContext {
    /// The GPU at `pci_slot` (as in sysfs, e.g. `0000:01:00.0`; the render
    /// node's device), or the first GPU when `None`.
    pub fn new(pci_slot: Option<&str>) -> Result<Arc<Self>> {
        let cuda = Cuda::get()?;
        let mut count = 0;
        // SAFETY: plain out-parameters throughout.
        unsafe {
            cuda.check((cuda.device_get_count)(&mut count), "cuDeviceGetCount")?;
            for ordinal in 0..count {
                let mut device = 0;
                cuda.check((cuda.device_get)(&mut device, ordinal), "cuDeviceGet")?;
                let mut bus = [0 as c_char; 32];
                cuda.check(
                    (cuda.device_get_pci_bus_id)(bus.as_mut_ptr(), bus.len() as c_int, device),
                    "cuDeviceGetPCIBusId",
                )?;
                let bus = CStr::from_ptr(bus.as_ptr())
                    .to_string_lossy()
                    .to_lowercase();
                if let Some(wanted) = pci_slot
                    && !same_slot(&bus, wanted)
                {
                    continue;
                }
                let mut name = [0 as c_char; 256];
                cuda.check(
                    (cuda.device_get_name)(name.as_mut_ptr(), name.len() as c_int, device),
                    "cuDeviceGetName",
                )?;
                let mut ctx = std::ptr::null_mut();
                cuda.check(
                    (cuda.primary_ctx_retain)(&mut ctx, device),
                    "cuDevicePrimaryCtxRetain",
                )?;
                return Ok(Arc::new(Self {
                    cuda,
                    device,
                    ctx,
                    name: CStr::from_ptr(name.as_ptr()).to_string_lossy().into_owned(),
                    pci_slot: bus,
                }));
            }
        }
        Err(Error::new(match pci_slot {
            Some(slot) => format!("no CUDA device at PCI {slot} (of {count})"),
            None => "no CUDA devices".into(),
        }))
    }

    /// The GPU's name, e.g. "NVIDIA GeForce RTX 4090".
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Its PCI bus id, as CUDA reports it.
    pub fn pci_slot(&self) -> &str {
        &self.pci_slot
    }

    pub(crate) fn raw(&self) -> CUcontext {
        self.ctx
    }

    /// Makes this context current on the calling thread until the guard drops.
    pub(crate) fn push(&self) -> Result<Current<'_>> {
        // SAFETY: a context we retained.
        self.cuda.check(
            unsafe { (self.cuda.ctx_push)(self.ctx) },
            "cuCtxPushCurrent",
        )?;
        Ok(Current(self))
    }
}

impl Drop for CudaContext {
    fn drop(&mut self) {
        // SAFETY: balances the retain in `new`.
        unsafe { (self.cuda.primary_ctx_release)(self.device) };
    }
}

pub(crate) struct Current<'a>(&'a CudaContext);

impl Drop for Current<'_> {
    fn drop(&mut self) {
        let mut popped = std::ptr::null_mut();
        // SAFETY: pops the context `push` made current on this thread.
        unsafe { (self.0.cuda.ctx_pop)(&mut popped) };
    }
}

/// nvidia-smi and CUDA say `00000000:01:00.0`; sysfs says `0000:01:00.0`.
fn same_slot(a: &str, b: &str) -> bool {
    let tail = |s: &str| {
        let s = s.trim().to_lowercase();
        s[s.len().saturating_sub(12)..].to_string()
    };
    tail(a) == tail(b)
}

/// GPU memory NVENC can read: a CUDA array, or pitch-linear device memory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Surface {
    Array(*mut c_void),
    Pitch { ptr: u64, pitch: u32 },
}

impl Surface {
    /// A stable key for caching the encoder's registration of this surface.
    pub(crate) fn key(self) -> u64 {
        match self {
            Surface::Array(array) => array as u64,
            Surface::Pitch { ptr, .. } => ptr,
        }
    }
}

// SAFETY: surfaces are GPU handles, valid on any thread with the context current.
unsafe impl Send for Surface {}

/// An EGL image registered with CUDA, read-only. Register each of the
/// compositor's output buffers once and reuse it every frame.
pub struct RegisteredImage {
    ctx: Arc<CudaContext>,
    resource: CUgraphicsResource,
}

// SAFETY: a CUDA handle; every use pushes the context, and reading the
// mapped frame from several threads at once is allowed.
unsafe impl Send for RegisteredImage {}
unsafe impl Sync for RegisteredImage {}

impl RegisteredImage {
    /// # Safety
    /// `image` must be a live `EGLImage` created on the same GPU, and must
    /// outlive this registration.
    pub unsafe fn new(ctx: &Arc<CudaContext>, image: *mut c_void) -> Result<Self> {
        let _current = ctx.push()?;
        let mut resource = std::ptr::null_mut();
        // SAFETY: the caller vouches for `image`.
        ctx.cuda.check(
            unsafe {
                (ctx.cuda.egl_register_image)(
                    &mut resource,
                    image,
                    CU_GRAPHICS_REGISTER_FLAGS_READ_ONLY,
                )
            },
            "cuGraphicsEGLRegisterImage",
        )?;
        Ok(Self {
            ctx: Arc::clone(ctx),
            resource,
        })
    }

    /// The image as NVENC input, with its width and height.
    pub fn surface(&self) -> Result<(Surface, u32, u32)> {
        let _current = self.ctx.push()?;
        // SAFETY: zeroes are a valid CUeglFrame; the resource is registered.
        let mut frame: CUeglFrame = unsafe { std::mem::zeroed() };
        self.ctx.cuda.check(
            unsafe { (self.ctx.cuda.mapped_egl_frame)(&mut frame, self.resource, 0, 0) },
            "cuGraphicsResourceGetMappedEglFrame",
        )?;
        if frame.plane_count != 1 {
            return Err(Error::new(format!(
                "expected one plane, the image has {}",
                frame.plane_count
            )));
        }
        // SAFETY: the frame type says which union member is set.
        let surface = unsafe {
            if frame.frame_type == CU_EGL_FRAME_TYPE_ARRAY {
                Surface::Array(frame.frame.array[0])
            } else {
                Surface::Pitch {
                    ptr: frame.frame.pitch[0] as CUdeviceptr,
                    pitch: frame.pitch,
                }
            }
        };
        Ok((surface, frame.width, frame.height))
    }
}

impl Drop for RegisteredImage {
    fn drop(&mut self) {
        if let Ok(_current) = self.ctx.push() {
            // SAFETY: registered in `new`.
            unsafe { (self.ctx.cuda.unregister_resource)(self.resource) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_pci_slots_across_spellings() {
        assert!(same_slot("00000000:01:00.0", "0000:01:00.0"));
        assert!(same_slot("0000:01:00.0", "0000:01:00.0"));
        assert!(!same_slot("00000000:02:00.0", "0000:01:00.0"));
    }

    #[test]
    fn egl_frame_matches_cuda_h() {
        // Three plane pointers, then nine 32-bit fields, padded to 8.
        assert_eq!(size_of::<CUeglFrame>(), 64);
    }
}

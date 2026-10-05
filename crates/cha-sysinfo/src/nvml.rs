//! NVIDIA GPUs through NVML, loaded at runtime.

use std::ffi::{CStr, CString, c_char, c_int, c_uint, c_void};
use std::sync::OnceLock;

use tracing::info;

use crate::GpuSample;

type Device = *mut c_void;

#[repr(C)]
#[derive(Default)]
struct Utilization {
    gpu: c_uint,
    memory: c_uint,
}

#[repr(C)]
#[derive(Default)]
struct MemoryInfo {
    total: u64,
    free: u64,
    used: u64,
}

const NVML_SUCCESS: c_int = 0;
const NVML_TEMPERATURE_GPU: c_uint = 0;
const NVML_CLOCK_SM: c_uint = 1;

/// A symbol as the function pointer type `T` it has.
///
/// # Safety
/// `T` must be the symbol's real (pointer-sized) type.
unsafe fn function_pointer<T: Copy>(ptr: *mut c_void) -> T {
    assert_eq!(size_of::<T>(), size_of::<*mut c_void>());
    // SAFETY: the caller vouches for `T`; sizes match (asserted above).
    unsafe { std::mem::transmute_copy(&ptr) }
}

/// The few NVML functions we read, from a library kept for the life of the
/// process.
struct Nvml {
    count: unsafe extern "C" fn(*mut c_uint) -> c_int,
    by_index: unsafe extern "C" fn(c_uint, *mut Device) -> c_int,
    by_slot: unsafe extern "C" fn(*const c_char, *mut Device) -> c_int,
    name: unsafe extern "C" fn(Device, *mut c_char, c_uint) -> c_int,
    utilization: unsafe extern "C" fn(Device, *mut Utilization) -> c_int,
    memory: unsafe extern "C" fn(Device, *mut MemoryInfo) -> c_int,
    encoder: unsafe extern "C" fn(Device, *mut c_uint, *mut c_uint) -> c_int,
    decoder: unsafe extern "C" fn(Device, *mut c_uint, *mut c_uint) -> c_int,
    temperature: unsafe extern "C" fn(Device, c_uint, *mut c_uint) -> c_int,
    power: unsafe extern "C" fn(Device, *mut c_uint) -> c_int,
    power_limit: unsafe extern "C" fn(Device, *mut c_uint) -> c_int,
    clock: unsafe extern "C" fn(Device, c_uint, *mut c_uint) -> c_int,
}

// SAFETY: NVML's calls are thread-safe, and the device handles are plain
// indexes into its tables.
unsafe impl Send for Nvml {}
unsafe impl Sync for Nvml {}

/// NVML, loaded on first use; None if the driver's library isn't there.
fn nvml() -> Option<&'static Nvml> {
    static NVML: OnceLock<Option<Nvml>> = OnceLock::new();
    NVML.get_or_init(|| match Nvml::load() {
        Ok(nvml) => Some(nvml),
        Err(why) => {
            info!("no GPU figures: {why}");
            None
        }
    })
    .as_ref()
}

/// Every NVIDIA GPU's reading now; none without NVML.
pub fn gpus() -> Vec<GpuSample> {
    nvml().map(Nvml::all).unwrap_or_default()
}

/// The reading of the GPU at PCI `slot` (`0000:01:00.0`), else of GPU 0.
pub fn gpu_at(slot: Option<&str>) -> Option<GpuSample> {
    nvml()?.one(slot)
}

impl Nvml {
    fn load() -> Result<Self, String> {
        let mut handle = std::ptr::null_mut();
        for name in [c"libnvidia-ml.so.1", c"libnvidia-ml.so"] {
            // SAFETY: a valid C string; RTLD_NOW resolves everything up front.
            handle = unsafe { libc::dlopen(name.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
            if !handle.is_null() {
                break;
            }
        }
        if handle.is_null() {
            return Err("couldn't load libnvidia-ml.so.1".into());
        }
        // The library stays loaded (unloading NVIDIA's libraries isn't safe
        // while threads may use them), so the pointers stay valid.
        let symbol = |name: &CStr| -> Result<*mut c_void, String> {
            // SAFETY: a live handle and a valid C string.
            let ptr = unsafe { libc::dlsym(handle, name.as_ptr()) };
            if ptr.is_null() {
                Err(format!("libnvidia-ml has no {}", name.to_string_lossy()))
            } else {
                Ok(ptr)
            }
        };
        macro_rules! function {
            ($name:expr) => {
                // SAFETY: the symbol is NVML's function of that name, and the
                // field's type is its signature.
                unsafe { function_pointer(symbol($name)?) }
            };
        }
        let init: unsafe extern "C" fn() -> c_int = function!(c"nvmlInit_v2");
        // SAFETY: no arguments.
        let status = unsafe { init() };
        if status != NVML_SUCCESS {
            return Err(format!("nvmlInit failed ({status})"));
        }
        Ok(Self {
            count: function!(c"nvmlDeviceGetCount_v2"),
            by_index: function!(c"nvmlDeviceGetHandleByIndex_v2"),
            by_slot: function!(c"nvmlDeviceGetHandleByPciBusId_v2"),
            name: function!(c"nvmlDeviceGetName"),
            utilization: function!(c"nvmlDeviceGetUtilizationRates"),
            memory: function!(c"nvmlDeviceGetMemoryInfo"),
            encoder: function!(c"nvmlDeviceGetEncoderUtilization"),
            decoder: function!(c"nvmlDeviceGetDecoderUtilization"),
            temperature: function!(c"nvmlDeviceGetTemperature"),
            power: function!(c"nvmlDeviceGetPowerUsage"),
            power_limit: function!(c"nvmlDeviceGetEnforcedPowerLimit"),
            clock: function!(c"nvmlDeviceGetClockInfo"),
        })
    }

    fn all(&self) -> Vec<GpuSample> {
        let mut count = 0;
        // SAFETY: an out pointer.
        if unsafe { (self.count)(&mut count) } != NVML_SUCCESS {
            return Vec::new();
        }
        (0..count)
            .filter_map(|index| self.at_index(index))
            .collect()
    }

    fn at_index(&self, index: u32) -> Option<GpuSample> {
        let mut device: Device = std::ptr::null_mut();
        // SAFETY: an out pointer.
        (unsafe { (self.by_index)(index, &mut device) } == NVML_SUCCESS)
            .then(|| self.read(device, index))
    }

    fn one(&self, slot: Option<&str>) -> Option<GpuSample> {
        if let Some(slot) = slot.and_then(|s| CString::new(s).ok()) {
            let mut device: Device = std::ptr::null_mut();
            // SAFETY: a valid C string and an out pointer.
            if unsafe { (self.by_slot)(slot.as_ptr(), &mut device) } == NVML_SUCCESS {
                // The slot's own index isn't asked for; it names the one GPU.
                return Some(self.read(device, 0));
            }
        }
        self.at_index(0)
    }

    /// What the GPU answers; a figure it can't give stays out.
    fn read(&self, dev: Device, index: u32) -> GpuSample {
        let mut sample = GpuSample {
            index,
            ..GpuSample::default()
        };
        // SAFETY (all calls below): a valid device handle and out pointers
        // to values of the types NVML writes.
        let mut name = [0 as c_char; 96];
        if unsafe { (self.name)(dev, name.as_mut_ptr(), name.len() as c_uint) } == NVML_SUCCESS {
            // SAFETY: NVML wrote a NUL-terminated string within the buffer.
            sample.name = unsafe { CStr::from_ptr(name.as_ptr()) }
                .to_string_lossy()
                .into_owned();
        }
        let mut util = Utilization::default();
        if unsafe { (self.utilization)(dev, &mut util) } == NVML_SUCCESS {
            sample.util = Some(util.gpu);
        }
        let mut mem = MemoryInfo::default();
        if unsafe { (self.memory)(dev, &mut mem) } == NVML_SUCCESS {
            sample.vram_used = Some(mem.used);
            sample.vram_total = Some(mem.total);
        }
        let read = |call: unsafe extern "C" fn(Device, *mut c_uint, *mut c_uint) -> c_int| {
            let (mut value, mut period) = (0, 0);
            (unsafe { call(dev, &mut value, &mut period) } == NVML_SUCCESS).then_some(value)
        };
        sample.enc = read(self.encoder);
        sample.dec = read(self.decoder);
        let mut value = 0;
        if unsafe { (self.temperature)(dev, NVML_TEMPERATURE_GPU, &mut value) } == NVML_SUCCESS {
            sample.temp = Some(value);
        }
        // Milliwatts.
        if unsafe { (self.power)(dev, &mut value) } == NVML_SUCCESS {
            sample.power = Some(f64::from(value) / 1000.0);
        }
        if unsafe { (self.power_limit)(dev, &mut value) } == NVML_SUCCESS {
            sample.power_limit = Some(f64::from(value) / 1000.0);
        }
        if unsafe { (self.clock)(dev, NVML_CLOCK_SM, &mut value) } == NVML_SUCCESS {
            sample.clock = Some(value);
        }
        sample
    }
}

#[cfg(test)]
mod tests {
    /// Prints the GPUs, to compare with `nvidia-smi` by hand.
    #[test]
    #[ignore = "needs an NVIDIA GPU"]
    fn prints_the_gpus() {
        let gpus = super::gpus();
        println!("{gpus:#?}");
        assert!(!gpus.is_empty());
    }
}

//! A machine's resource use: CPU, RAM and NVIDIA GPUs.
//!
//! Shared by the streamer (its session stats) and the node agent (the portal's
//! Nodes page). Linux only; elsewhere [`Sampler::sample`] gives `None` and
//! [`gpus`] nothing, so the node still builds and tests on a laptop.
//! - CPU: the whole machine's utilisation from `/proc/stat` deltas, its core
//!   count, and the load averages from `/proc/loadavg`. A container sees the
//!   host's `/proc/stat` and `/proc/loadavg`, which is what we want.
//! - RAM: `MemTotal` and `MemAvailable` from `/proc/meminfo`, the host's too
//!   (a container's cgroup limit isn't applied to this view; a LXCFS mount
//!   would show the limit instead, and so would we).
//! - GPUs (NVIDIA): through NVML, loaded at runtime like the other NVIDIA
//!   libraries (the container runtime's CDI spec provides `libnvidia-ml.so.1`).
//!   Without NVML there are no GPUs.
//! - Which processes hold GPU memory ([`gpu_processes`]), to tell environments apart.
//! - This process's CPU from `/proc/self/stat`, in percent of one core (so
//!   it may pass 100).

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
mod nvml;
#[cfg(target_os = "linux")]
pub use linux::Sampler;
#[cfg(target_os = "linux")]
pub use nvml::{gpu_at, gpu_processes, gpus};

#[cfg(not(target_os = "linux"))]
mod other;
#[cfg(not(target_os = "linux"))]
pub use other::{Sampler, gpu_at, gpu_processes, gpus};

/// One reading of the host (percent 0..100, bytes).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HostSample {
    /// Over the time since the last reading; 0 on the first.
    pub cpu: f64,
    pub cores: usize,
    /// The 1, 5 and 15 minute load averages.
    pub load: [f64; 3],
    pub mem_used: u64,
    pub mem_total: u64,
    /// This process's CPU, in percent of one core.
    pub process_cpu: f64,
}

/// One NVIDIA GPU's reading; what NVML can't say for it stays `None`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GpuSample {
    pub index: u32,
    pub name: String,
    /// Utilisation, percent.
    pub util: Option<u32>,
    pub vram_used: Option<u64>,
    pub vram_total: Option<u64>,
    /// NVENC and NVDEC utilisation, percent.
    pub enc: Option<u32>,
    pub dec: Option<u32>,
    /// °C.
    pub temp: Option<u32>,
    /// Watts.
    pub power: Option<f64>,
    pub power_limit: Option<f64>,
    /// The streaming multiprocessors' clock, MHz.
    pub clock: Option<u32>,
}

/// A process holding GPU memory (any NVIDIA GPU), by the pid NVML reports: the
/// host's, which a container sees as is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GpuProcess {
    pub pid: u32,
    /// Bytes of GPU memory it holds.
    pub vram: u64,
}

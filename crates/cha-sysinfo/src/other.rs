//! Where there is no /proc or NVML: nothing is available.

use crate::{GpuSample, HostSample};

pub struct Sampler;

impl Sampler {
    pub fn new() -> Self {
        Self
    }

    pub fn sample(&mut self) -> Option<HostSample> {
        None
    }
}

impl Default for Sampler {
    fn default() -> Self {
        Self::new()
    }
}

pub fn gpus() -> Vec<GpuSample> {
    Vec::new()
}

pub fn gpu_at(_slot: Option<&str>) -> Option<GpuSample> {
    None
}

//! The node's resource use, for the session's stats panel.
//!
//! Each session samples once a second while it is connected and sends the
//! result as `{"t":"system",…}` on the control channel. The readings come from
//! `cha-sysinfo` (CPU and RAM from /proc, the GPU through NVML); this adds
//! which GPU, and the shape the page gets.
//! - GPU (NVIDIA): the one the streamer encodes on, found by the render node's
//!   PCI slot, else the first one. Without NVML the GPU fields are left out.
//! - This process's CPU, in percent of one core (so it may pass 100).

use std::sync::OnceLock;

use serde::Serialize;

/// One second's reading, as the page gets it (percent 0..100, bytes, watts,
/// MHz, °C).
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct SystemSample {
    pub cpu: f64,
    pub cores: usize,
    pub load1: f64,
    pub mem_used: u64,
    pub mem_total: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gpu: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vram_used: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vram_total: Option<u64>,
    /// NVENC and NVDEC utilisation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enc: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dec: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temp: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub power: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub power_limit: Option<f64>,
    /// The streaming multiprocessors' clock.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clock: Option<u32>,
    pub streamer_cpu: f64,
}

/// The GPU's PCI slot (`0000:01:00.0`), set once at start; unset picks GPU 0.
static GPU_SLOT: OnceLock<String> = OnceLock::new();

/// Samples the GPU the streamer encodes on, which is the one at `slot`.
pub fn set_gpu_slot(slot: Option<String>) {
    if let Some(slot) = slot {
        let _ = GPU_SLOT.set(slot);
    }
}

/// A session's sampler: it keeps the last counters to take deltas of.
pub struct Sampler(cha_sysinfo::Sampler);

impl Sampler {
    pub fn new() -> Self {
        Self(cha_sysinfo::Sampler::new())
    }

    /// The reading now (the CPU figures are over the time since the last
    /// one, and 0 on the first), or None if /proc can't be read.
    pub fn sample(&mut self) -> Option<SystemSample> {
        let host = self.0.sample()?;
        let mut sample = SystemSample {
            cpu: host.cpu,
            cores: host.cores,
            load1: host.load[0],
            mem_used: host.mem_used,
            mem_total: host.mem_total,
            streamer_cpu: host.process_cpu,
            ..SystemSample::default()
        };
        if let Some(gpu) = cha_sysinfo::gpu_at(GPU_SLOT.get().map(String::as_str)) {
            sample.gpu = gpu.util;
            sample.vram_used = gpu.vram_used;
            sample.vram_total = gpu.vram_total;
            sample.enc = gpu.enc;
            sample.dec = gpu.dec;
            sample.temp = gpu.temp;
            sample.power = gpu.power;
            sample.power_limit = gpu.power_limit;
            sample.clock = gpu.clock;
        }
        Some(sample)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_is_flat_and_leaves_out_what_is_missing() {
        let sample = SystemSample {
            cpu: 23.5,
            cores: 16,
            load1: 2.1,
            mem_used: 10,
            mem_total: 20,
            streamer_cpu: 4.0,
            ..SystemSample::default()
        };
        let json = serde_json::to_string(&crate::control::ServerMsg::System(sample)).unwrap();
        assert_eq!(
            json,
            r#"{"t":"system","cpu":23.5,"cores":16,"load1":2.1,"mem_used":10,"mem_total":20,"streamer_cpu":4.0}"#
        );
    }

    /// Prints a real sample, to compare with `nvidia-smi` by hand.
    #[test]
    #[ignore = "needs an NVIDIA GPU and /proc"]
    fn prints_a_sample() {
        let mut sampler = Sampler::new();
        sampler.sample().unwrap();
        std::thread::sleep(std::time::Duration::from_secs(1));
        println!("{:#?}", sampler.sample().unwrap());
    }
}

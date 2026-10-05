//! The machine's CPU, RAM and GPU use, for the portal's Nodes page.
//!
//! Read with `cha-sysinfo` (the host's /proc, which the agent's container
//! shares, and NVML from the container runtime's CDI spec) and sent every
//! [`cha_wire::USAGE_INTERVAL_SECS`] to a portal that reads it.

use cha_sysinfo::{GpuSample, HostSample, Sampler};
use cha_wire::{GpuUsage, NodeUsage};

/// The use now, or None where there is no /proc (not Linux). Reads files and
/// asks the driver, so it belongs on a blocking thread.
pub fn sample(sampler: &mut Sampler, environments: u32) -> Option<NodeUsage> {
    Some(usage(sampler.sample()?, cha_sysinfo::gpus(), environments))
}

fn usage(host: HostSample, gpus: Vec<GpuSample>, environments: u32) -> NodeUsage {
    NodeUsage {
        cpu: host.cpu,
        cores: host.cores as u32,
        load: host.load,
        mem_used: host.mem_used,
        mem_total: host.mem_total,
        gpus: gpus
            .into_iter()
            .map(|g| GpuUsage {
                index: g.index,
                name: g.name,
                util: g.util,
                vram_used: g.vram_used,
                vram_total: g.vram_total,
                enc: g.enc,
                dec: g.dec,
                temp: g.temp,
                power: g.power,
                power_limit: g.power_limit,
            })
            .collect(),
        environments,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_host_and_every_gpu_carry_over() {
        let host = HostSample {
            cpu: 50.0,
            cores: 8,
            load: [1.0, 2.0, 3.0],
            mem_used: 1,
            mem_total: 2,
            process_cpu: 9.0,
        };
        let gpu = |index| GpuSample {
            index,
            name: format!("GPU {index}"),
            util: Some(10),
            ..GpuSample::default()
        };
        let u = usage(host, vec![gpu(0), gpu(1)], 3);
        assert_eq!((u.cpu, u.cores, u.load), (50.0, 8, [1.0, 2.0, 3.0]));
        assert_eq!((u.mem_used, u.mem_total, u.environments), (1, 2, 3));
        assert_eq!(u.gpus.len(), 2);
        assert_eq!((u.gpus[1].index, u.gpus[1].util), (1, Some(10)));
        assert_eq!(u.gpus[1].vram_total, None);
    }

    /// Prints a real reading, to compare with `top`, `free` and `nvidia-smi`.
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "needs /proc, and an NVIDIA GPU for the GPU part"]
    fn prints_a_sample() {
        let mut sampler = Sampler::new();
        sample(&mut sampler, 0).unwrap();
        std::thread::sleep(std::time::Duration::from_secs(1));
        println!("{:#?}", sample(&mut sampler, 0).unwrap());
    }
}

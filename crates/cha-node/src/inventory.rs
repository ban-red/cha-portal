//! What this machine has: OS, CPU, memory, GPUs and the addresses browsers
//! might reach it at. Every probe degrades to "unknown" rather than failing.

use std::collections::BTreeSet;
use std::fs;
use std::net::{IpAddr, Ipv6Addr};
use std::process::Command;

use cha_wire::{
    Device, DeviceKind, Disk, DiskUse, Gpu, Inventory, PlacementMode, Platform, PlatformKind,
};

pub fn collect() -> Inventory {
    let mut inventory = Inventory {
        hostname: hostname(),
        os: os_name(),
        arch: std::env::consts::ARCH.into(),
        cpus: std::thread::available_parallelism().map_or(1, |n| n.get() as u32),
        memory_mb: memory_mb().unwrap_or(0),
        gpus: gpus(),
        addresses: addresses(),
        // Only the agent knows it: it adds this when it reports.
        data_root: None,
        shared_dirs: Default::default(),
        devices: None,
        // Likewise: the agent says whether it runs a GameStream host.
        gamestream: None,
        // The agent lists them and its placement mode (ADR 0017).
        images: Vec::new(),
        agent_version: Some(env!("CARGO_PKG_VERSION").to_string()),
        placement: None,
        // The agent adds them: it knows the data root and asks Docker where
        // its own is.
        disks: Vec::new(),
        platform: platform(),
        // The agent adds it: it asks Docker about its own container.
        update: None,
        spec_features: vec![
            cha_wire::SPEC_FEATURE_ENV.into(),
            cha_wire::SPEC_FEATURE_DATA_TEMPLATE.into(),
            cha_wire::SPEC_FEATURE_HOST_OPTIONS.into(),
        ],
        // The agent adds it: the owner's settings are the runtime's.
        host_options: Some(cha_wire::HostPolicy::default()),
    };
    // NVIDIA and the CPU; the agent adds the VA-API devices it probes.
    let mut devices = inventory.devices_or_derived();
    devices.push(cpu_device(inventory.cpus));
    inventory.devices = Some(devices);
    inventory
}

/// What the images disk is called when the engine didn't say where it is.
const DOCKERS_DISK: &str = "Docker's disk";

/// What each of the node's disks has free: the one under Docker's images and
/// the one under the data root, merged when they are the same filesystem.
/// `docker_root` is the engine's `DockerRootDir`, only to show the admin.
pub fn disks(data_root: Option<&str>, docker_root: Option<&str>) -> Vec<Disk> {
    let mut readings = Vec::new();
    // The agent's own root is overlayfs on Docker's data root, so "/" is
    // Docker's disk.
    if let Some(r) = read_disk("/", DiskUse::Images, docker_root.unwrap_or(DOCKERS_DISK)) {
        readings.push(r);
    }
    if let Some(root) = data_root
        && let Some(r) = read_disk(root, DiskUse::AppData, root)
    {
        readings.push(r);
    }
    merge_disks(readings)
}

/// One `statvfs`, before merging.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskReading {
    pub usage: DiskUse,
    pub path: String,
    pub fsid: u64,
    pub total: u64,
    pub free: u64,
}

fn read_disk(at: &str, usage: DiskUse, shown: &str) -> Option<DiskReading> {
    let s = rustix::fs::statvfs(at).ok()?;
    Some(DiskReading {
        usage,
        path: shown.into(),
        fsid: s.f_fsid,
        total: s.f_blocks.saturating_mul(s.f_frsize),
        free: s.f_bavail.saturating_mul(s.f_frsize),
    })
}

/// Readings of the same filesystem become one disk with all their uses (the
/// first reading's path is shown). Overlayfs may report an `f_fsid` of its
/// own, or none, so equal total and free space count as the same filesystem
/// too: two disks that agree to the byte, read a moment apart, are one.
pub fn merge_disks(readings: Vec<DiskReading>) -> Vec<Disk> {
    let mut disks: Vec<(DiskReading, Disk)> = Vec::new();
    for r in readings {
        let same = disks.iter_mut().find(|(seen, _)| {
            (r.fsid != 0 && seen.fsid == r.fsid) || (seen.total == r.total && seen.free == r.free)
        });
        match same {
            Some((_, disk)) => {
                if !disk.uses.contains(&r.usage) {
                    disk.uses.push(r.usage);
                }
                // Where Docker's root is unknown, the data root names it better.
                if disk.path == DOCKERS_DISK && r.usage == DiskUse::AppData {
                    disk.path = r.path;
                }
            }
            None => {
                let disk = Disk {
                    uses: vec![r.usage],
                    path: r.path.clone(),
                    total_bytes: r.total,
                    free_bytes: r.free,
                };
                disks.push((r, disk));
            }
        }
    }
    disks.into_iter().map(|(_, d)| d).collect()
}

/// What the node runs on; `None` off Linux (a development Mac).
#[cfg(target_os = "linux")]
pub fn platform() -> Option<Platform> {
    let read = |path: &str| fs::read_to_string(path).ok();
    Some(detect_platform(&PlatformFacts {
        osrelease: read("/proc/sys/kernel/osrelease"),
        init_environ: fs::read("/proc/1/environ")
            .ok()
            .map(|b| String::from_utf8_lossy(&b).into_owned()),
        container_manager: read("/run/host/container-manager"),
        init_comm: read("/proc/1/comm"),
        init_started: read("/proc/1/stat")
            .and_then(|stat| stat_start_ticks(&stat))
            .map(|ticks| ticks as f64 / USER_HZ),
        sys_vendor: read("/sys/class/dmi/id/sys_vendor"),
        product_name: read("/sys/class/dmi/id/product_name"),
        bios_vendor: read("/sys/class/dmi/id/bios_vendor"),
        hypervisor_type: read("/sys/hypervisor/type"),
        cpuinfo: read("/proc/cpuinfo"),
    }))
}

#[cfg(not(target_os = "linux"))]
pub fn platform() -> Option<Platform> {
    None
}

/// The files [`detect_platform`] reads, as text; `None` where one is missing.
#[derive(Debug, Default)]
pub struct PlatformFacts {
    pub osrelease: Option<String>,
    /// `/proc/1/environ`, NUL-separated. The host's init, since the agent
    /// runs with `pid: host`.
    pub init_environ: Option<String>,
    pub container_manager: Option<String>,
    /// `/proc/1/comm`: which init process 1 is.
    pub init_comm: Option<String>,
    /// When process 1 started, in seconds after the kernel booted
    /// (`/proc/1/stat`, readable without the rights `environ` needs).
    pub init_started: Option<f64>,
    pub sys_vendor: Option<String>,
    pub product_name: Option<String>,
    pub bios_vendor: Option<String>,
    pub hypervisor_type: Option<String>,
    pub cpuinfo: Option<String>,
}

/// The names process 1 has as a machine's or a container's init (not
/// docker-init or tini, which mean the agent can't see the host's processes).
const SYSTEM_INITS: [&str; 5] = ["systemd", "init", "openrc-init", "runit", "s6-svscan"];
/// How long after the kernel booted a system init may start and still be the
/// machine's own.
const LATE_INIT_SECS: f64 = 10.0;

/// The kernel's clock ticks per second in `/proc/<pid>/stat` (USER_HZ),
/// fixed at 100 on x86-64.
#[cfg(target_os = "linux")]
const USER_HZ: f64 = 100.0;

/// The start time field of `/proc/<pid>/stat`, in clock ticks after boot.
/// Fields count from after the command's closing parenthesis, which may
/// contain spaces.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn stat_start_ticks(stat: &str) -> Option<u64> {
    let rest = &stat[stat.rfind(')')? + 1..];
    // `starttime` is field 22; `rest` starts at field 3 (the state).
    rest.split_whitespace().nth(19)?.parse().ok()
}

/// WSL, Docker Desktop, an LXC container, a virtual machine or bare metal,
/// in that order. A machine whose probes all came back empty is `unknown`.
pub fn detect_platform(f: &PlatformFacts) -> Platform {
    let kernel = f
        .osrelease
        .as_deref()
        .map(str::trim)
        .filter(|k| !k.is_empty())
        .map(String::from);
    let platform = |kind, detail: Option<&str>| Platform {
        kind,
        detail: detail.map(String::from),
        kernel: kernel.clone(),
    };
    let release = f.osrelease.as_deref().unwrap_or("").to_lowercase();
    if release.contains("microsoft") {
        return platform(PlatformKind::Wsl, Some("WSL 2"));
    }
    if release.contains("linuxkit") {
        return platform(PlatformKind::DockerDesktop, Some("Docker Desktop"));
    }
    let lxc_env = f
        .init_environ
        .as_deref()
        .is_some_and(|e| e.split('\0').any(|kv| kv.trim() == "container=lxc"));
    let lxc_manager = f.container_manager.as_deref().map(str::trim) == Some("lxc");
    // Without the right to read init's environment (an agent with no
    // CAP_SYS_PTRACE can't), a system init that started well after the
    // kernel booted is a container's: a machine's or a VM's init starts
    // within seconds of its kernel, a container's once the host is up.
    let late_init = f
        .init_comm
        .as_deref()
        .map(str::trim)
        .is_some_and(|c| SYSTEM_INITS.contains(&c))
        && f.init_started.is_some_and(|s| s > LATE_INIT_SECS);
    if lxc_env || lxc_manager || late_init {
        return platform(PlatformKind::Lxc, Some("LXC container"));
    }
    let lower = |v: &Option<String>| v.as_deref().unwrap_or("").trim().to_lowercase();
    let (vendor, product, bios, hv) = (
        lower(&f.sys_vendor),
        lower(&f.product_name),
        lower(&f.bios_vendor),
        lower(&f.hypervisor_type),
    );
    let vm = if [&vendor, &product, &bios]
        .iter()
        .any(|v| v.contains("proxmox"))
    {
        Some("Proxmox VE VM (QEMU)")
    } else if vendor.contains("qemu") || product.contains("kvm") || product.contains("standard pc")
    {
        Some("KVM (QEMU)")
    } else if vendor.contains("vmware") || product.contains("vmware") {
        Some("VMware")
    } else if vendor.contains("microsoft") && product.contains("virtual machine") {
        Some("Hyper-V")
    } else if hv == "xen" || vendor.contains("xen") || product.contains("hvm domu") {
        Some("Xen")
    } else if vendor.contains("innotek") || product.contains("virtualbox") {
        Some("VirtualBox")
    } else if vendor.contains("amazon ec2") || product.contains("amazon ec2") {
        Some("Amazon EC2")
    } else if vendor.contains("google") && product.contains("google compute") {
        Some("Google Compute Engine")
    } else {
        None
    };
    if let Some(detail) = vm {
        return platform(PlatformKind::Vm, Some(detail));
    }
    let hypervisor_flag = f.cpuinfo.as_deref().is_some_and(|c| {
        c.lines()
            .filter(|l| l.starts_with("flags"))
            .any(|l| l.split_whitespace().any(|w| w == "hypervisor"))
    });
    if hypervisor_flag {
        return platform(PlatformKind::Vm, None);
    }
    // Nothing readable at all (no /sys, no /proc): say so rather than guess.
    let nothing = f.osrelease.is_none()
        && f.cpuinfo.is_none()
        && f.sys_vendor.is_none()
        && f.product_name.is_none();
    if nothing {
        return platform(PlatformKind::Unknown, None);
    }
    platform(PlatformKind::BareMetal, None)
}

/// `CHA_PLACEMENT`: `auto` (the default, also when empty) or `manual`.
pub fn parse_placement(value: Option<&str>) -> anyhow::Result<PlacementMode> {
    match value.map(str::trim).unwrap_or_default() {
        "" | "auto" => Ok(PlacementMode::Auto),
        "manual" => Ok(PlacementMode::Manual),
        other => anyhow::bail!("CHA_PLACEMENT must be auto or manual: {other:?}"),
    }
}

/// Puts `vaapi` devices into an inventory's device list, ahead of the CPU.
pub fn add_vaapi(inventory: &mut Inventory, vaapi: Vec<Device>) {
    let Some(devices) = &mut inventory.devices else {
        return;
    };
    let at = devices
        .iter()
        .position(|d| d.kind == DeviceKind::Cpu)
        .unwrap_or(devices.len());
    devices.splice(at..at, vaapi);
}

/// Sets the codecs of the CPU device, from what the streamer image says it
/// makes (`devices::DeviceProbes::cpu_codecs`).
pub fn set_cpu_codecs(inventory: &mut Inventory, codecs: Vec<String>) {
    let cpu = inventory
        .devices
        .iter_mut()
        .flatten()
        .find(|d| d.kind == DeviceKind::Cpu);
    if let Some(cpu) = cpu {
        cpu.codecs = codecs;
    }
}

/// The machine's processor as a device: x264 in software, so H.264 until the
/// streamer image says more (SVT-AV1 adds AV1).
fn cpu_device(cores: u32) -> Device {
    let name = fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|text| cpu_model(&text))
        .unwrap_or_else(|| "CPU".into());
    Device {
        id: "cpu".into(),
        kind: DeviceKind::Cpu,
        name,
        render_node: None,
        vendor: None,
        codecs: vec!["h264".into()],
        cores: Some(cores),
    }
}

fn cpu_model(cpuinfo: &str) -> Option<String> {
    cpuinfo
        .lines()
        .find_map(|l| l.strip_prefix("model name"))
        .and_then(|l| l.split_once(':'))
        .map(|(_, name)| name.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|name| !name.is_empty())
}

/// Render nodes that might do VA-API: every one whose driver isn't NVIDIA's.
pub fn vaapi_candidates() -> Vec<String> {
    render_nodes()
        .into_iter()
        .filter(|n| n.vendor_id != 0x10de && n.driver.as_deref() != Some("nvidia"))
        .map(|n| n.path)
        .collect()
}

/// Whether this machine has an NVIDIA GPU with a render node, as
/// `/sys/class/drm` (which every container sees) says.
pub fn has_nvidia_render_node() -> bool {
    render_nodes()
        .iter()
        .any(|n| n.vendor_id == 0x10de || n.driver.as_deref() == Some("nvidia"))
}

fn command(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8(output.stdout).ok())
        .flatten()
}

fn hostname() -> String {
    fs::read_to_string("/proc/sys/kernel/hostname")
        .ok()
        .or_else(|| command("hostname", &[]))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".into())
}

fn os_name() -> String {
    // In a container, the host's os-release is conventionally mounted at
    // /run/host/os-release; /etc/os-release would describe the image.
    for path in ["/run/host/os-release", "/etc/os-release"] {
        if let Some(name) = fs::read_to_string(path)
            .ok()
            .and_then(|text| os_release_name(&text))
        {
            return name;
        }
    }
    match std::env::consts::OS {
        "macos" => command("sw_vers", &["-productVersion"])
            .map(|v| format!("macOS {}", v.trim()))
            .unwrap_or_else(|| "macOS".into()),
        other => other.into(),
    }
}

fn os_release_name(text: &str) -> Option<String> {
    let field = |key: &str| {
        text.lines()
            .find_map(|line| line.strip_prefix(key)?.strip_prefix('='))
            .map(|v| v.trim().trim_matches('"').to_string())
            .filter(|v| !v.is_empty())
    };
    field("PRETTY_NAME").or_else(|| field("NAME"))
}

fn memory_mb() -> Option<u64> {
    if let Ok(meminfo) = fs::read_to_string("/proc/meminfo") {
        let kb: u64 = meminfo
            .lines()
            .find_map(|l| l.strip_prefix("MemTotal:"))?
            .trim()
            .trim_end_matches("kB")
            .trim()
            .parse()
            .ok()?;
        return Some(kb / 1024);
    }
    let bytes: u64 = command("sysctl", &["-n", "hw.memsize"])?
        .trim()
        .parse()
        .ok()?;
    Some(bytes / (1024 * 1024))
}

/// A DRM render node and the PCI device behind it.
struct RenderNode {
    path: String,
    vendor_id: u16,
    device_id: u16,
    /// e.g. `0000:01:00.0`.
    pci_slot: Option<String>,
    /// The kernel driver bound to it (`i915`, `amdgpu`, `nvidia`).
    driver: Option<String>,
}

fn render_nodes() -> Vec<RenderNode> {
    let Ok(entries) = fs::read_dir("/sys/class/drm") else {
        return Vec::new();
    };
    let mut nodes: Vec<RenderNode> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            if !name.starts_with("renderD") {
                return None;
            }
            let device = entry.path().join("device");
            let hex = |file: &str| {
                let text = fs::read_to_string(device.join(file)).ok()?;
                u16::from_str_radix(text.trim().trim_start_matches("0x"), 16).ok()
            };
            let pci_slot = fs::read_to_string(device.join("uevent"))
                .ok()
                .and_then(|u| {
                    u.lines()
                        .find_map(|l| l.strip_prefix("PCI_SLOT_NAME="))
                        .map(|s| s.trim().to_lowercase())
                });
            Some(RenderNode {
                path: format!("/dev/dri/{name}"),
                vendor_id: hex("vendor")?,
                device_id: hex("device").unwrap_or(0),
                pci_slot,
                driver: fs::read_link(device.join("driver"))
                    .ok()
                    .and_then(|d| d.file_name()?.to_str().map(str::to_string)),
            })
        })
        .collect();
    nodes.sort_by(|a, b| a.path.cmp(&b.path));
    nodes
}

fn gpus() -> Vec<Gpu> {
    let render_nodes = render_nodes();
    let mut gpus = nvidia_gpus(&render_nodes);
    // Render nodes nvidia-smi didn't account for: AMD and Intel (their encoders
    // come in a later phase), or NVIDIA without nvidia-smi.
    for node in &render_nodes {
        if gpus
            .iter()
            .any(|g| g.render_node.as_deref() == Some(&node.path))
        {
            continue;
        }
        let vendor = match node.vendor_id {
            0x10de => "nvidia",
            0x1002 => "amd",
            0x8086 => "intel",
            _ => "other",
        };
        gpus.push(Gpu {
            vendor: vendor.into(),
            name: format!(
                "{vendor} GPU ({:04x}:{:04x})",
                node.vendor_id, node.device_id
            ),
            memory_mb: None,
            driver: None,
            render_node: Some(node.path.clone()),
            encoders: Vec::new(),
        });
    }
    gpus
}

fn nvidia_gpus(render_nodes: &[RenderNode]) -> Vec<Gpu> {
    let Some(csv) = command(
        "nvidia-smi",
        &[
            "--query-gpu=name,memory.total,driver_version,pci.bus_id,compute_cap",
            "--format=csv,noheader,nounits",
        ],
    ) else {
        return Vec::new();
    };
    csv.lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.split(',').map(str::trim).collect();
            let [name, memory, driver, bus_id, compute_cap] = f[..] else {
                return None;
            };
            // nvidia-smi says 00000000:01:00.0; sysfs says 0000:01:00.0.
            let slot = bus_id.to_lowercase();
            let slot = &slot[slot.len().saturating_sub(12)..];
            let render_node = render_nodes
                .iter()
                .find(|n| n.pci_slot.as_deref() == Some(slot))
                .map(|n| n.path.clone());
            Some(Gpu {
                vendor: "nvidia".into(),
                name: name.into(),
                memory_mb: memory.parse().ok(),
                driver: Some(driver.into()),
                render_node,
                encoders: nvenc_codecs(name, compute_cap.parse().unwrap_or(0.0)),
            })
        })
        .collect()
}

/// NVENC by architecture: H.264 and HEVC on every GPU since Pascal, AV1 from
/// Ada (compute capability 8.9) and on Blackwell; compute-only parts have no
/// NVENC. A hint for the UI — the streaming stack checks what actually opens.
fn nvenc_codecs(name: &str, compute_cap: f32) -> Vec<String> {
    const COMPUTE_ONLY: [&str; 7] = ["A100", "A30", "H100", "H200", "H800", "B100", "B200"];
    if compute_cap < 6.0 || COMPUTE_ONLY.iter().any(|m| name.contains(m)) {
        return Vec::new();
    }
    let mut codecs = vec!["h264".to_string(), "hevc".to_string()];
    if compute_cap >= 8.9 {
        codecs.push("av1".into());
    }
    codecs
}

/// Interface addresses a browser could plausibly reach: no loopback, no
/// link-local, no container or VM bridges, no rotating IPv6 privacy addresses.
/// IPv4 first.
fn addresses() -> Vec<String> {
    const BRIDGES: [&str; 8] = [
        "docker", "br-", "veth", "virbr", "cni", "flannel", "vmnet", "bridge",
    ];
    let Ok(interfaces) = if_addrs::get_if_addrs() else {
        return Vec::new();
    };
    let transient = fs::read_to_string("/proc/net/if_inet6")
        .map(|t| transient_ipv6(&t))
        .unwrap_or_default();
    let ips: BTreeSet<(bool, IpAddr)> = interfaces
        .iter()
        .filter(|i| !i.is_loopback() && !BRIDGES.iter().any(|b| i.name.starts_with(b)))
        .map(|i| i.ip())
        .filter(|ip| match ip {
            IpAddr::V4(v4) => !v4.is_link_local(),
            IpAddr::V6(v6) => !v6.is_unicast_link_local() && !transient.contains(v6),
        })
        .map(|ip| (ip.is_ipv6(), ip))
        .collect();
    ips.into_iter().map(|(_, ip)| ip.to_string()).collect()
}

/// Temporary or deprecated IPv6 addresses, from Linux's `/proc/net/if_inet6`
/// (address, ifindex, prefix length, scope, flags, name).
fn transient_ipv6(table: &str) -> BTreeSet<Ipv6Addr> {
    const IFA_F_TEMPORARY: u32 = 0x01;
    const IFA_F_DEPRECATED: u32 = 0x20;
    table
        .lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            let flags = u32::from_str_radix(fields.get(4)?, 16).ok()?;
            if flags & (IFA_F_TEMPORARY | IFA_F_DEPRECATED) == 0 {
                return None;
            }
            Some(Ipv6Addr::from(
                u128::from_str_radix(fields.first()?, 16).ok()?,
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reading(usage: DiskUse, path: &str, fsid: u64, total: u64, free: u64) -> DiskReading {
        DiskReading {
            usage,
            path: path.into(),
            fsid,
            total,
            free,
        }
    }

    #[test]
    fn one_filesystem_is_one_disk_with_both_uses() {
        let disks = merge_disks(vec![
            reading(DiskUse::Images, DOCKERS_DISK, 7, 100, 40),
            reading(DiskUse::AppData, "/srv/cha-portal", 7, 100, 40),
        ]);
        assert_eq!(disks.len(), 1);
        assert_eq!(disks[0].uses, [DiskUse::Images, DiskUse::AppData]);
        assert_eq!(disks[0].path, "/srv/cha-portal");
        assert_eq!((disks[0].total_bytes, disks[0].free_bytes), (100, 40));
    }

    #[test]
    fn overlayfs_with_its_own_fsid_is_still_matched_by_space() {
        let disks = merge_disks(vec![
            reading(DiskUse::Images, "/var/lib/docker", 1, 500, 200),
            reading(DiskUse::AppData, "/data", 2, 500, 200),
        ]);
        assert_eq!(disks.len(), 1);
        assert_eq!(disks[0].path, "/var/lib/docker");
    }

    #[test]
    fn different_filesystems_stay_apart() {
        let disks = merge_disks(vec![
            reading(DiskUse::Images, "/var/lib/docker", 1, 500, 200),
            reading(DiskUse::AppData, "/data", 2, 4000, 3000),
        ]);
        assert_eq!(disks.len(), 2);
        assert_eq!(disks[0].uses, [DiskUse::Images]);
        assert_eq!(disks[1].uses, [DiskUse::AppData]);
        // An fsid of 0 (none reported) doesn't match another's.
        let zero = merge_disks(vec![
            reading(DiskUse::Images, "a", 0, 1, 1),
            reading(DiskUse::AppData, "b", 0, 2, 2),
        ]);
        assert_eq!(zero.len(), 2);
    }

    fn facts() -> PlatformFacts {
        PlatformFacts {
            osrelease: Some("6.14.11-4-pve\n".into()),
            cpuinfo: Some("flags\t\t: fpu vme sse\n".into()),
            sys_vendor: Some("Supermicro\n".into()),
            product_name: Some("X12\n".into()),
            ..PlatformFacts::default()
        }
    }

    #[test]
    fn bare_metal_has_no_hypervisor_hints() {
        let p = detect_platform(&facts());
        assert_eq!(p.kind, PlatformKind::BareMetal);
        assert_eq!(p.detail, None);
        assert_eq!(p.kernel.as_deref(), Some("6.14.11-4-pve"));
    }

    #[test]
    fn wsl_and_docker_desktop_come_from_the_kernel() {
        let wsl = detect_platform(&PlatformFacts {
            osrelease: Some("5.15.153.1-microsoft-standard-WSL2".into()),
            ..facts()
        });
        assert_eq!(wsl.kind, PlatformKind::Wsl);
        assert_eq!(wsl.detail.as_deref(), Some("WSL 2"));
        let dd = detect_platform(&PlatformFacts {
            osrelease: Some("6.10.14-linuxkit".into()),
            ..facts()
        });
        assert_eq!(dd.kind, PlatformKind::DockerDesktop);
        assert_eq!(dd.kernel.as_deref(), Some("6.10.14-linuxkit"));
    }

    #[test]
    fn a_late_system_init_is_a_container() {
        let late = PlatformFacts {
            init_comm: Some("systemd\n".into()),
            init_started: Some(42.0),
            cpuinfo: Some("flags : fpu vme".into()),
            ..PlatformFacts::default()
        };
        assert_eq!(detect_platform(&late).kind, PlatformKind::Lxc);
        let early = PlatformFacts {
            init_started: Some(1.2),
            ..late
        };
        assert_eq!(detect_platform(&early).kind, PlatformKind::BareMetal);
        // The agent's own init (no host process view) says nothing.
        let own = PlatformFacts {
            init_comm: Some("docker-init\n".into()),
            init_started: Some(5000.0),
            cpuinfo: Some("flags : fpu vme".into()),
            ..PlatformFacts::default()
        };
        assert_eq!(detect_platform(&own).kind, PlatformKind::BareMetal);
        let stat = "1 (systemd) S 0 1 1 0 -1 4194560 1 2 3 4 5 6 7 8 20 0 1 0 4271 22609920 3074 18446744073709551615";
        assert_eq!(stat_start_ticks(stat), Some(4271));
        assert_eq!(
            stat_start_ticks("7 (a b) c) S 0 1 1 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 99 0"),
            Some(99)
        );
    }

    #[test]
    fn lxc_from_the_inits_environment_or_the_manager_file() {
        let env = detect_platform(&PlatformFacts {
            init_environ: Some("HOME=/\0container=lxc\0TERM=linux\0".into()),
            // An LXC container shows its host's DMI as a VM would.
            sys_vendor: Some("QEMU".into()),
            ..facts()
        });
        assert_eq!(env.kind, PlatformKind::Lxc);
        assert_eq!(env.detail.as_deref(), Some("LXC container"));
        let file = detect_platform(&PlatformFacts {
            container_manager: Some("lxc\n".into()),
            ..facts()
        });
        assert_eq!(file.kind, PlatformKind::Lxc);
        let other = detect_platform(&PlatformFacts {
            init_environ: Some("container=podman\0".into()),
            ..facts()
        });
        assert_eq!(other.kind, PlatformKind::BareMetal);
    }

    #[test]
    fn virtual_machines_are_named_when_dmi_says() {
        let vm = |vendor: &str, product: &str, bios: &str| {
            detect_platform(&PlatformFacts {
                sys_vendor: Some(vendor.into()),
                product_name: Some(product.into()),
                bios_vendor: Some(bios.into()),
                ..facts()
            })
        };
        let detail = |p: Platform| {
            assert_eq!(p.kind, PlatformKind::Vm);
            p.detail.unwrap()
        };
        assert_eq!(
            detail(vm("QEMU", "Standard PC (Q35 + ICH9, 2009)", "SeaBIOS")),
            "KVM (QEMU)"
        );
        assert_eq!(
            detail(vm(
                "QEMU",
                "Standard PC (Q35 + ICH9, 2009)",
                "Proxmox distribution of EDK II"
            )),
            "Proxmox VE VM (QEMU)"
        );
        assert_eq!(
            detail(vm("VMware, Inc.", "VMware7,1", "VMware, Inc.")),
            "VMware"
        );
        assert_eq!(
            detail(vm(
                "Microsoft Corporation",
                "Virtual Machine",
                "Microsoft Corporation"
            )),
            "Hyper-V"
        );
        assert_eq!(detail(vm("Xen", "HVM domU", "Xen")), "Xen");
        assert_eq!(
            detail(vm("innotek GmbH", "VirtualBox", "innotek GmbH")),
            "VirtualBox"
        );
        assert_eq!(
            detail(vm("Amazon EC2", "c5.large", "Amazon EC2")),
            "Amazon EC2"
        );
        assert_eq!(
            detail(vm("Google", "Google Compute Engine", "Google")),
            "Google Compute Engine"
        );
    }

    #[test]
    fn the_hypervisor_cpu_flag_alone_is_a_vm_without_a_name() {
        let p = detect_platform(&PlatformFacts {
            cpuinfo: Some("flags\t: fpu hypervisor lm\n".into()),
            ..facts()
        });
        assert_eq!(p.kind, PlatformKind::Vm);
        assert_eq!(p.detail, None);
    }

    #[test]
    fn nothing_readable_is_unknown() {
        let p = detect_platform(&PlatformFacts::default());
        assert_eq!(p.kind, PlatformKind::Unknown);
    }

    #[test]
    fn placement_is_auto_or_manual() {
        assert_eq!(parse_placement(None).unwrap(), PlacementMode::Auto);
        assert_eq!(parse_placement(Some("")).unwrap(), PlacementMode::Auto);
        assert_eq!(
            parse_placement(Some(" auto ")).unwrap(),
            PlacementMode::Auto
        );
        assert_eq!(
            parse_placement(Some("manual")).unwrap(),
            PlacementMode::Manual
        );
        let err = parse_placement(Some("Manual")).unwrap_err().to_string();
        assert!(err.contains("CHA_PLACEMENT"), "{err}");
        assert!(parse_placement(Some("never")).is_err());
    }

    #[test]
    fn reads_os_release() {
        let text = "NAME=\"Ubuntu\"\nVERSION_ID=\"26.04\"\nPRETTY_NAME=\"Ubuntu 26.04 LTS\"\n";
        assert_eq!(os_release_name(text).as_deref(), Some("Ubuntu 26.04 LTS"));
        assert_eq!(
            os_release_name("NAME=Arch Linux\n").as_deref(),
            Some("Arch Linux")
        );
        assert_eq!(os_release_name("ID=x\n"), None);
    }

    #[test]
    fn nvenc_hints_follow_the_architecture() {
        assert_eq!(
            nvenc_codecs("NVIDIA GeForce RTX 4090", 8.9),
            ["h264", "hevc", "av1"]
        );
        assert_eq!(
            nvenc_codecs("NVIDIA GeForce RTX 3080", 8.6),
            ["h264", "hevc"]
        );
        assert_eq!(
            nvenc_codecs("NVIDIA GeForce RTX 5090", 12.0),
            ["h264", "hevc", "av1"]
        );
        assert!(nvenc_codecs("NVIDIA H100 80GB HBM3", 9.0).is_empty());
    }

    #[test]
    fn reads_the_cpu_model() {
        let text = "processor\t: 0\nmodel name\t: Intel(R)  Core(TM) i9\nflags\t: x\n";
        assert_eq!(cpu_model(text).as_deref(), Some("Intel(R) Core(TM) i9"));
        assert_eq!(cpu_model("processor : 0\n"), None);
    }

    #[test]
    fn vaapi_devices_go_before_the_cpu() {
        let mut inv = collect();
        let vaapi = Device {
            id: "vaapi:renderD129".into(),
            kind: DeviceKind::Vaapi,
            name: "Arc".into(),
            render_node: Some("/dev/dri/renderD129".into()),
            vendor: Some("intel".into()),
            codecs: vec!["h264".into()],
            cores: None,
        };
        add_vaapi(&mut inv, vec![vaapi]);
        let devices = inv.devices.unwrap();
        let kinds: Vec<_> = devices.iter().map(|d| d.kind).collect();
        assert_eq!(kinds.last(), Some(&DeviceKind::Cpu));
        assert!(kinds.contains(&DeviceKind::Vaapi));
    }

    #[test]
    fn the_cpu_offers_what_the_image_says() {
        let mut inv = collect();
        let codecs = |inv: &Inventory| inv.devices.as_ref().unwrap().last().unwrap().codecs.clone();
        // Until it is asked: H.264.
        assert_eq!(codecs(&inv), ["h264"]);
        set_cpu_codecs(&mut inv, vec!["h264".into(), "av1".into()]);
        assert_eq!(codecs(&inv), ["h264", "av1"]);
        // Only the CPU's.
        let others: Vec<_> = inv
            .devices
            .iter()
            .flatten()
            .filter(|d| d.kind != DeviceKind::Cpu)
            .collect();
        assert!(others.iter().all(|d| d.codecs != ["h264", "av1"]));
        // No devices (an old inventory): nothing to set, nothing broken.
        let mut bare = collect();
        bare.devices = None;
        set_cpu_codecs(&mut bare, vec!["av1".into()]);
        assert!(bare.devices.is_none());
    }

    #[test]
    fn spots_rotating_ipv6_addresses() {
        let table = "\
2001056a7ce6550041545b8b8c56d3f0 02 40 00 01    ens18
2001056a7ce65500e1264ac5dac311e6 02 40 00 21    ens18
2001056a7ce65500be2411fffe3d56b9 02 40 00 00    ens18
";
        let transient = transient_ipv6(table);
        assert_eq!(transient.len(), 2);
        assert!(transient.contains(&"2001:56a:7ce6:5500:4154:5b8b:8c56:d3f0".parse().unwrap()));
        assert!(!transient.contains(&"2001:56a:7ce6:5500:be24:11ff:fe3d:56b9".parse().unwrap()));
    }

    #[test]
    fn collects_something_here() {
        let inv = collect();
        assert!(!inv.hostname.is_empty());
        assert!(inv.cpus >= 1);
        let devices = inv.devices.expect("lists devices");
        let cpu = devices.last().expect("the cpu is always one");
        assert_eq!((cpu.kind, cpu.cores), (DeviceKind::Cpu, Some(inv.cpus)));
    }
}

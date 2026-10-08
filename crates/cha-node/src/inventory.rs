//! What this machine has: OS, CPU, memory, GPUs and the addresses browsers
//! might reach it at. Every probe degrades to "unknown" rather than failing.

use std::collections::BTreeSet;
use std::fs;
use std::net::{IpAddr, Ipv6Addr};
use std::process::Command;

use cha_wire::{Device, DeviceKind, Gpu, Inventory, PlacementMode};

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
    };
    // NVIDIA and the CPU; the agent adds the VA-API devices it probes.
    let mut devices = inventory.devices_or_derived();
    devices.push(cpu_device(inventory.cpus));
    inventory.devices = Some(devices);
    inventory
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

    use super::*;

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

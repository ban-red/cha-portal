//! The device a streamer runs on (`--device`, `docs/devices.md`): what the
//! compositor draws with, what encodes, and which codecs that makes possible.
//!
//! | kind | composites on | encodes with | codecs |
//! |---|---|---|---|
//! | `nvidia` | EGL on the render node | NVENC through CUDA, zero-copy | H.264, HEVC, AV1, PyroWave |
//! | `vaapi` | EGL on the render node (Mesa) | VA-API, zero-copy dmabuf import | H.264 (what the driver encodes, of what is written) |
//! | `cpu` | Mesa's llvmpipe | x264, SVT-AV1 | H.264, AV1 (when SVT-AV1 loads) |
//!
//! `--probe-device <kind>[:<render node>]` prints what a device offers as JSON
//! and exits; the node runs it in a throwaway container to inventory a machine.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, anyhow, bail};
use cha_nvenc::{Codec, CudaContext};
use clap::ValueEnum;
use serde::Serialize;

use crate::codec::VideoCodec;
use crate::encoder::{Backend, Params, VideoEncoder, nvenc::Nvenc, svtav1, vaapi, x264};

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DeviceKind {
    Nvidia,
    Vaapi,
    Cpu,
}

impl DeviceKind {
    pub fn name(self) -> &'static str {
        match self {
            DeviceKind::Nvidia => "nvidia",
            DeviceKind::Vaapi => "vaapi",
            DeviceKind::Cpu => "cpu",
        }
    }
}

/// The encoder for a codec on a kind of device.
pub fn backend(kind: DeviceKind, codec: Codec) -> Result<Backend> {
    match (kind, codec) {
        (DeviceKind::Nvidia, _) => Ok(Backend::Nvenc),
        (DeviceKind::Vaapi, _) => Ok(Backend::Vaapi),
        (DeviceKind::Cpu, Codec::H264) => Ok(Backend::X264),
        (DeviceKind::Cpu, Codec::Av1) => Ok(Backend::SvtAv1),
        (DeviceKind::Cpu, other) => {
            bail!(
                "the CPU device encodes H.264 and AV1 only, not {}",
                other.name()
            )
        }
    }
}

/// What the CPU device makes: H.264 (x264, which it needs to start), and AV1
/// when SVT-AV1 is there.
fn cpu_codecs(svtav1: bool) -> Vec<Codec> {
    let mut codecs = vec![Codec::H264];
    if svtav1 {
        codecs.push(Codec::Av1);
    }
    codecs
}

/// The codecs worth offering: `wanted` (`--codecs`, in its order) that the
/// device can make. PyroWave is NVIDIA's (it makes its own Vulkan device on
/// the GPU; `pyrowave` says the library is there).
pub fn intersect(
    kind: DeviceKind,
    device: &[Codec],
    wanted: &[VideoCodec],
    pyrowave: bool,
) -> Vec<VideoCodec> {
    wanted
        .iter()
        .copied()
        .filter(|codec| match codec {
            VideoCodec::Hw(c) => device.contains(c),
            VideoCodec::PyroWave(_) => kind == DeviceKind::Nvidia && pyrowave,
        })
        .collect()
}

pub struct Device {
    kind: DeviceKind,
    render_node: Option<PathBuf>,
    name: String,
    vendor: Option<&'static str>,
    codecs: Vec<Codec>,
    cuda: Option<Arc<CudaContext>>,
}

impl Device {
    /// Opens the device: CUDA on the render node's GPU (`nvidia`), a look at
    /// what libva's driver encodes (`vaapi`, which needs the node), x264's
    /// and SVT-AV1's presence (`cpu`). Nothing is encoded yet.
    pub fn open(kind: DeviceKind, render_node: Option<&Path>) -> Result<Arc<Self>> {
        let vendor = render_node.and_then(pci_vendor);
        let device = match kind {
            DeviceKind::Nvidia => {
                let slot = render_node.and_then(pci_slot);
                let cuda = CudaContext::new(slot.as_deref()).map_err(|e| anyhow!("{e}"))?;
                Self {
                    kind,
                    render_node: render_node.map(Path::to_path_buf),
                    name: cuda.name().to_string(),
                    vendor: Some("nvidia"),
                    // What NVENC may offer; `probe_codecs` finds what this GPU does.
                    codecs: Codec::ALL.to_vec(),
                    cuda: Some(cuda),
                }
            }
            DeviceKind::Vaapi => {
                let node = render_node.context("--device vaapi needs a render node")?;
                let caps = vaapi::probe(node)?;
                Self {
                    kind,
                    render_node: Some(node.to_path_buf()),
                    name: caps.vendor,
                    vendor,
                    // Of what the driver encodes, the codecs we have an
                    // encoder for.
                    codecs: vaapi::built(caps.codecs),
                    cuda: None,
                }
            }
            DeviceKind::Cpu => {
                x264::available()?;
                tracing::info!(x264 = x264::describe()?, "software encoding");
                let av1 = svtav1::available().and_then(|()| svtav1::describe());
                match &av1 {
                    Ok(version) => tracing::info!(svtav1 = version, "software AV1"),
                    Err(why) => tracing::info!("no software AV1: {why:#}"),
                }
                Self {
                    kind,
                    render_node: None,
                    name: cpu_name(),
                    vendor: None,
                    codecs: cpu_codecs(av1.is_ok()),
                    cuda: None,
                }
            }
        };
        Ok(Arc::new(device))
    }

    pub fn kind(&self) -> DeviceKind {
        self.kind
    }

    pub fn render_node(&self) -> Option<&Path> {
        self.render_node.as_deref()
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// NVIDIA's CUDA context for the GPU.
    pub fn cuda(&self) -> Option<Arc<CudaContext>> {
        self.cuda.clone()
    }

    /// The codecs the device's encoder can make.
    pub fn codecs(&self) -> &[Codec] {
        &self.codecs
    }

    /// Whether an encoder is built for this device.
    pub fn encodes(&self) -> bool {
        self.kind != DeviceKind::Vaapi || vaapi::ENCODER_BUILT
    }

    /// A new encoder for `params.codec`.
    pub fn encoder(&self, params: Params) -> Result<Box<dyn VideoEncoder>> {
        match backend(self.kind, params.codec)? {
            Backend::Nvenc => {
                let cuda = self.cuda.clone().context("no CUDA context")?;
                Ok(Box::new(Nvenc::new(cuda, params)?))
            }
            Backend::X264 => Ok(Box::new(x264::X264::new(params)?)),
            Backend::SvtAv1 => Ok(Box::new(svtav1::SvtAv1::new(params)?)),
            Backend::Vaapi => {
                let node = self.render_node.as_deref().context("no render node")?;
                vaapi::open(node, params)
            }
        }
    }

    /// What `--probe-device` reports.
    fn report(&self, pyrowave: bool) -> Report {
        let mut codecs: Vec<&'static str> = self.codecs.iter().map(|c| c.name()).collect();
        if self.kind == DeviceKind::Nvidia && pyrowave {
            codecs.extend(["pyrowave420", "pyrowave444"]);
        }
        Report {
            kind: self.kind,
            name: self.name.clone(),
            vendor: self.vendor,
            render_node: self.render_node.as_ref().map(|p| p.display().to_string()),
            codecs,
            cores: (self.kind == DeviceKind::Cpu)
                .then(|| std::thread::available_parallelism().map_or(1, |n| n.get())),
        }
    }

    /// NVIDIA: which codecs this GPU's NVENC takes, by opening a session for
    /// each (an old GPU has no AV1).
    fn probe_codecs(&mut self) {
        let Some(cuda) = self.cuda.clone() else {
            return;
        };
        self.codecs = Codec::ALL
            .into_iter()
            .filter(|&codec| {
                Nvenc::new(
                    Arc::clone(&cuda),
                    Params {
                        codec,
                        width: 1280,
                        height: 720,
                        fps: 60,
                        bitrate_bps: 10_000_000,
                    },
                )
                .is_ok()
            })
            .collect();
    }
}

/// `--probe-device`'s output: `{ kind, name, vendor?, renderNode?, codecs,
/// cores? }`.
#[derive(Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub kind: DeviceKind,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vendor: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub render_node: Option<String>,
    pub codecs: Vec<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cores: Option<usize>,
}

/// `kind` or `kind:/dev/dri/renderD129`.
pub fn parse_probe_spec(spec: &str) -> Result<(DeviceKind, Option<PathBuf>)> {
    let (kind, node) = match spec.split_once(':') {
        Some((kind, node)) => (kind, Some(PathBuf::from(node))),
        None => (spec, None),
    };
    let kind = DeviceKind::from_str(kind, true)
        .map_err(|_| anyhow!("unknown device kind {kind:?} (nvidia, vaapi or cpu)"))?;
    Ok((kind, node))
}

/// Opens the device and prints what it offers as JSON.
///
/// With `CHA_ENCODE_TEST=<frames>` set, a VA-API device also encodes that many
/// generated pictures first and checks the stream (the result goes to stderr,
/// a failure ends the probe with an error): the smoke test for a new GPU.
pub fn probe(spec: &str, pyrowave: bool) -> Result<()> {
    let (kind, node) = parse_probe_spec(spec)?;
    let mut device = Device::open(kind, node.as_deref())?;
    if let (DeviceKind::Vaapi, Some(node), Ok(frames)) = (
        kind,
        node.as_deref(),
        std::env::var("CHA_ENCODE_TEST").map(|v| v.parse::<u32>()),
    ) {
        let frames = frames.context("CHA_ENCODE_TEST is a number of frames")?;
        let result = vaapi::self_test(node, frames)?;
        eprintln!("encoder: {}", result.setup);
        eprintln!(
            "encode test passed: {} frames, {} IDR, {} bytes, {:.2} ms a frame (NAL/OBU types of the first: {:?}); first key frame {} bytes, inter frames {} bytes at 20 Mbit/s, {} bytes at 8",
            result.frames,
            result.keyframes,
            result.bytes,
            result.encode_ms_avg,
            result.first_nals,
            result.key_bytes,
            result.p_bytes_before,
            result.p_bytes_after
        );
    }
    if kind == DeviceKind::Vaapi {
        // libva's vendor string names the driver, not the GPU; Mesa's
        // GL_RENDERER names the GPU. Keep the vendor string if there is none.
        match crate::compositor::gl_renderer(&device) {
            Ok(renderer) => match gpu_name(&renderer) {
                Some(name) => Arc::get_mut(&mut device).expect("no other holder yet").name = name,
                None => tracing::debug!(renderer, "GL_RENDERER is not a GPU name"),
            },
            Err(why) => tracing::debug!("no GL_RENDERER for the GPU's name: {why:#}"),
        }
    }
    if kind == DeviceKind::Nvidia {
        Arc::get_mut(&mut device)
            .expect("no other holder yet")
            .probe_codecs();
    }
    println!("{}", serde_json::to_string(&device.report(pyrowave))?);
    Ok(())
}

/// A GPU's name from Mesa's GL_RENDERER: "Mesa Intel(R) UHD Graphics 630
/// (CFL GT2)" becomes "Intel UHD Graphics 630". None for a software renderer
/// (llvmpipe, softpipe) or an empty string, which name no GPU.
fn gpu_name(renderer: &str) -> Option<String> {
    let mut s = renderer.trim();
    s = s.strip_prefix("Mesa ").unwrap_or(s);
    // The trailing detail: "(CFL GT2)", "(radeonsi, navi33, LLVM 19.1.1, ...)".
    if s.ends_with(')')
        && let Some(open) = s.rfind(" (")
    {
        s = &s[..open];
    }
    let s = s.replace("(R)", "").replace("(TM)", "");
    let name = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let lower = name.to_ascii_lowercase();
    let software = ["llvmpipe", "softpipe", "swrast", "software"];
    (!name.is_empty() && !software.iter().any(|w| lower.contains(w))).then_some(name)
}

/// The render node's PCI slot (`0000:01:00.0`), so CUDA picks the same GPU.
pub fn pci_slot(render_node: &Path) -> Option<String> {
    let name = render_node.file_name()?.to_str()?;
    let uevent = std::fs::read_to_string(format!("/sys/class/drm/{name}/device/uevent")).ok()?;
    uevent
        .lines()
        .find_map(|l| l.strip_prefix("PCI_SLOT_NAME="))
        .map(|s| s.trim().to_string())
}

/// Who made the GPU behind a render node, from its PCI vendor id.
fn pci_vendor(render_node: &Path) -> Option<&'static str> {
    let name = render_node.file_name()?.to_str()?;
    let text = std::fs::read_to_string(format!("/sys/class/drm/{name}/device/vendor")).ok()?;
    vendor_name(u32::from_str_radix(text.trim().trim_start_matches("0x"), 16).ok()?)
}

fn vendor_name(pci_vendor: u32) -> Option<&'static str> {
    match pci_vendor {
        0x8086 => Some("intel"),
        0x1002 => Some("amd"),
        0x10de => Some("nvidia"),
        _ => None,
    }
}

/// The processor's model, for the CPU device's name.
fn cpu_name() -> String {
    std::fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|text| {
            text.lines()
                .find_map(|l| l.strip_prefix("model name"))
                .and_then(|l| l.split_once(':'))
                .map(|(_, name)| name.trim().to_string())
        })
        .unwrap_or_else(|| "CPU".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use cha_pyrowave::Chroma;

    #[test]
    fn each_kind_picks_its_encoder() {
        assert_eq!(
            backend(DeviceKind::Nvidia, Codec::Av1).unwrap(),
            Backend::Nvenc
        );
        assert_eq!(
            backend(DeviceKind::Cpu, Codec::H264).unwrap(),
            Backend::X264
        );
        assert_eq!(
            backend(DeviceKind::Vaapi, Codec::Hevc).unwrap(),
            Backend::Vaapi
        );
        assert_eq!(
            backend(DeviceKind::Cpu, Codec::Av1).unwrap(),
            Backend::SvtAv1
        );
        // Nothing on the CPU makes HEVC.
        let err = backend(DeviceKind::Cpu, Codec::Hevc).unwrap_err();
        assert!(err.to_string().contains("H.264 and AV1 only"), "{err}");
    }

    #[test]
    fn codecs_offered_follow_the_device() {
        let wanted: Vec<VideoCodec> = ["hevc", "h264", "av1", "pyrowave420"]
            .iter()
            .map(|n| VideoCodec::from_name(n).unwrap())
            .collect();
        let names = |codecs: Vec<VideoCodec>| codecs.iter().map(|c| c.name()).collect::<Vec<_>>();
        // NVIDIA: everything asked for, in the order asked.
        assert_eq!(
            names(intersect(DeviceKind::Nvidia, &Codec::ALL, &wanted, true)),
            ["hevc", "h264", "av1", "pyrowave420"]
        );
        // Without the PyroWave library, not it.
        assert_eq!(
            names(intersect(DeviceKind::Nvidia, &Codec::ALL, &wanted, false)),
            ["hevc", "h264", "av1"]
        );
        // CPU without SVT-AV1: H.264 only, whatever is asked.
        assert_eq!(
            names(intersect(
                DeviceKind::Cpu,
                &cpu_codecs(false),
                &wanted,
                true
            )),
            ["h264"]
        );
        // With it, H.264 and AV1, in the order asked.
        assert_eq!(
            names(intersect(DeviceKind::Cpu, &cpu_codecs(true), &wanted, true)),
            ["h264", "av1"]
        );
        let av1_first: Vec<VideoCodec> = ["av1", "h264"]
            .iter()
            .map(|n| VideoCodec::from_name(n).unwrap())
            .collect();
        assert_eq!(
            names(intersect(
                DeviceKind::Cpu,
                &cpu_codecs(true),
                &av1_first,
                false
            )),
            ["av1", "h264"]
        );
        // VA-API: what the driver encodes; never PyroWave (yet).
        assert_eq!(
            names(intersect(
                DeviceKind::Vaapi,
                &[Codec::H264, Codec::Hevc],
                &wanted,
                true
            )),
            ["hevc", "h264"]
        );
        // Asking for what the device can't make leaves nothing.
        assert!(
            intersect(
                DeviceKind::Cpu,
                &cpu_codecs(false),
                &[
                    VideoCodec::Hw(Codec::Av1),
                    VideoCodec::PyroWave(Chroma::Yuv444)
                ],
                true
            )
            .is_empty()
        );
    }

    #[test]
    fn the_cpu_offers_av1_when_svtav1_loads() {
        assert_eq!(cpu_codecs(false), [Codec::H264]);
        assert_eq!(cpu_codecs(true), [Codec::H264, Codec::Av1]);
    }

    #[test]
    fn tidies_gl_renderer_into_a_gpu_name() {
        let name = |s| gpu_name(s).unwrap();
        assert_eq!(
            name("Mesa Intel(R) UHD Graphics 630 (CFL GT2)"),
            "Intel UHD Graphics 630"
        );
        assert_eq!(
            name("AMD Radeon RX 7600 (radeonsi, navi33, LLVM 19.1.1, DRM 3.59, 6.8.0)"),
            "AMD Radeon RX 7600"
        );
        assert_eq!(
            name("Intel(R) Arc(TM) A380 Graphics (DG2)"),
            "Intel Arc A380 Graphics"
        );
        assert_eq!(name("  Some  GPU "), "Some GPU");
        assert_eq!(gpu_name(""), None);
        assert_eq!(gpu_name("llvmpipe (LLVM 19.1.1, 256 bits)"), None);
    }

    #[test]
    fn probe_specs() {
        assert_eq!(parse_probe_spec("cpu").unwrap(), (DeviceKind::Cpu, None));
        assert_eq!(
            parse_probe_spec("vaapi:/dev/dri/renderD129").unwrap(),
            (
                DeviceKind::Vaapi,
                Some(PathBuf::from("/dev/dri/renderD129"))
            )
        );
        assert_eq!(parse_probe_spec("nvidia").unwrap().0, DeviceKind::Nvidia);
        assert!(parse_probe_spec("tpu").is_err());
    }

    #[test]
    fn the_probe_report_has_the_contract_shape() {
        let cpu = Report {
            kind: DeviceKind::Cpu,
            name: "Some CPU".into(),
            vendor: None,
            render_node: None,
            codecs: vec!["h264", "av1"],
            cores: Some(16),
        };
        assert_eq!(
            serde_json::to_string(&cpu).unwrap(),
            r#"{"kind":"cpu","name":"Some CPU","codecs":["h264","av1"],"cores":16}"#
        );
        let intel = Report {
            kind: DeviceKind::Vaapi,
            name: "Intel iHD driver".into(),
            vendor: Some("intel"),
            render_node: Some("/dev/dri/renderD129".into()),
            codecs: vec!["h264", "hevc", "av1"],
            cores: None,
        };
        assert_eq!(
            serde_json::to_string(&intel).unwrap(),
            r#"{"kind":"vaapi","name":"Intel iHD driver","vendor":"intel","renderNode":"/dev/dri/renderD129","codecs":["h264","hevc","av1"]}"#
        );
    }

    #[test]
    fn pci_vendors() {
        assert_eq!(vendor_name(0x8086), Some("intel"));
        assert_eq!(vendor_name(0x1002), Some("amd"));
        assert_eq!(vendor_name(0x10de), Some("nvidia"));
        assert_eq!(vendor_name(0x1234), None);
    }
}

//! The PyroWave encoder thread (the LAN tier, plan §3.2): composited frames
//! straight from the output buffers (imported once each as dmabufs) into
//! network packets.
//!
//! Every frame stands alone, so there are no keyframes: a lost packet blurs
//! its region of one frame. When the screen goes still, the last frame is
//! sent once more ("heal"), so a loss doesn't stay on screen; nothing is sent
//! otherwise while nothing changes (the compositor only composites damage).

use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use bytes::Bytes;
use cha_pyrowave::{Chroma, Device, Dmabuf, Encoder, Image, Packet};
use smithay::backend::allocator::Buffer;
use tracing::{info, warn};

use crate::media::{EncodedFrame, Frame, Mailbox, Subscribers, Wake, deliver, pace_of};

/// Bytes of bitstream per packet: one datagram each, with room for our
/// header within QUIC's smallest datagram. A multiple of 4 (the bitstream's
/// word size).
pub const PACKET_BYTES: usize = 1100;
/// Still this long after the last new frame: send it again.
const HEAL_AFTER: Duration = Duration::from_millis(250);

#[derive(Clone)]
pub struct PyroSettings {
    /// The GPU's PCI ids: PyroWave makes its own Vulkan device on it.
    pub vendor_id: u32,
    pub device_id: u32,
    /// The budget for 4:2:0 at 1440p and `fps` (4:4:4 gets twice as much),
    /// scaled with the picture's area.
    pub mbps_420: u32,
    pub fps: u32,
    /// The one Vulkan device the streamer's PyroWave encoders share.
    shared: Arc<OnceLock<Result<Arc<Device>, String>>>,
}

impl PyroSettings {
    pub fn new(vendor_id: u32, device_id: u32, mbps_420: u32, fps: u32) -> Self {
        Self {
            vendor_id,
            device_id,
            mbps_420,
            fps,
            shared: Arc::default(),
        }
    }

    /// The shared device, made on first use (a later caller waits for it).
    pub fn device(&self) -> Result<Arc<Device>, String> {
        self.shared
            .get_or_init(|| Device::new(self.vendor_id, self.device_id).map_err(|e| e.to_string()))
            .clone()
    }

    /// Makes the device now, in the background, while nobody watches:
    /// making one stalls the GPU's other work (NVENC, the compositor) for
    /// ~0.2 s, which a codec switch mid-session would show.
    pub fn warm(&self) {
        let settings = self.clone();
        let spawned = std::thread::Builder::new()
            .name("pyrowave-warm".into())
            .spawn(move || {
                let started = Instant::now();
                match settings.device() {
                    Ok(_) => info!(
                        init_ms = started.elapsed().as_millis() as u64,
                        "pyrowave device ready"
                    ),
                    Err(err) => warn!("pyrowave: no device: {err}"),
                }
            });
        if let Err(err) = spawned {
            warn!("pyrowave: couldn't start warming up: {err}");
        }
    }

    fn frame_budget(&self, chroma: Chroma, width: u32, height: u32) -> usize {
        let area = f64::from(width * height) / f64::from(2560 * 1440);
        let mbps = f64::from(self.mbps_420) * if chroma == Chroma::Yuv444 { 2.0 } else { 1.0 };
        let bytes = mbps * 1e6 / 8.0 / f64::from(self.fps.max(1)) * area;
        (bytes as usize).max(64 * 1024)
    }
}

pub struct PyroWorker {
    pub chroma: Chroma,
    pub settings: PyroSettings,
    pub mailbox: Arc<Mailbox>,
    pub subscribers: Subscribers,
}

struct Imported {
    slot: usize,
    generation: u64,
    image: Image,
}

#[derive(Default)]
struct Stats {
    frames: u64,
    bytes: u64,
    packets: u64,
    heals: u64,
    encode_us: Vec<u64>,
    since: Option<Instant>,
}

impl PyroWorker {
    pub fn run(self) {
        let spawned = Instant::now();
        let device = match self.settings.device() {
            Ok(device) => device,
            Err(err) => {
                // Dropping the subscribers ends their streams.
                warn!("pyrowave: no device: {err}");
                return;
            }
        };
        let mut encoder: Option<Encoder> = None;
        let mut images: Vec<Imported> = Vec::new();
        let mut last: Option<Frame> = None;
        let mut healed = true;
        let mut last_new = Instant::now();
        let mut bytes = Vec::with_capacity(4 << 20);
        let mut packets = Vec::new();
        let mut stats = Stats::default();
        loop {
            let (frame, heal) = match self.mailbox.wait(Duration::from_millis(50)) {
                Wake::Closed => return,
                Wake::Frame(frame, _) => {
                    last = Some(frame.clone());
                    healed = false;
                    last_new = Instant::now();
                    (frame, false)
                }
                // A session asked (it lost something, or just subscribed).
                Wake::Keyframe | Wake::Refresh(_) => match last.clone() {
                    Some(frame) => (frame, true),
                    None => continue,
                },
                Wake::Idle => match last.clone() {
                    Some(frame) if !healed && last_new.elapsed() >= HEAL_AFTER => {
                        healed = true;
                        (frame, true)
                    }
                    _ => continue,
                },
            };
            // A backed-up subscriber's queue: skip this frame (every frame
            // stands alone, so nothing waits on it).
            match pace_of(&self.subscribers) {
                None | Some((true, _)) => continue,
                Some((false, _)) => {}
            }
            let started = Instant::now();
            let result = self.encode(
                &device,
                &mut encoder,
                &mut images,
                &frame,
                &mut bytes,
                &mut packets,
            );
            let encoded = Instant::now();
            if stats.frames == 0 && stats.since.is_none() {
                info!(
                    codec = self.name(),
                    since_start_ms = encoded.duration_since(spawned).as_millis() as u64,
                    encode_ms = encoded.duration_since(started).as_millis() as u64,
                    "first frame encoded"
                );
            }
            if let Err(err) = result {
                warn!(codec = self.name(), "encoding failed: {err:#}");
                encoder = None;
                images.clear();
                continue;
            }
            stats.frames += 1;
            stats.bytes += bytes.len() as u64;
            stats.packets += packets.len() as u64;
            stats.heals += u64::from(heal);
            stats
                .encode_us
                .push(encoded.duration_since(started).as_micros() as u64);
            deliver(
                &self.subscribers,
                EncodedFrame {
                    data: Bytes::copy_from_slice(&bytes),
                    key: true,
                    index: 0,
                    recovery: None,
                    composited: if heal { started } else { frame.rendered },
                    encoded,
                    packets: packets.clone(),
                },
            );
            self.log(&mut stats, &device);
        }
    }

    fn name(&self) -> &'static str {
        match self.chroma {
            Chroma::Yuv420 => "pyrowave420",
            Chroma::Yuv444 => "pyrowave444",
        }
    }

    fn encode(
        &self,
        device: &Arc<Device>,
        encoder: &mut Option<Encoder>,
        images: &mut Vec<Imported>,
        frame: &Frame,
        bytes: &mut Vec<u8>,
        packets: &mut Vec<Packet>,
    ) -> Result<()> {
        if encoder
            .as_ref()
            .is_some_and(|e| e.size() != (frame.width, frame.height))
        {
            *encoder = None;
        }
        let encoder = match encoder {
            Some(encoder) => encoder,
            None => {
                let started = Instant::now();
                let created = Encoder::new(device, frame.width, frame.height, self.chroma)
                    .map_err(|e| anyhow::anyhow!("{e}"))?;
                info!(
                    codec = self.name(),
                    width = frame.width,
                    height = frame.height,
                    init_ms = started.elapsed().as_millis() as u64,
                    "encoder ready"
                );
                encoder.insert(created)
            }
        };
        // Output buffers are reallocated on resize: forget the old ones.
        images.retain(|i| i.generation == frame.generation);
        let slot = Arc::as_ptr(&frame.slot) as usize;
        let index = match images.iter().position(|i| i.slot == slot) {
            Some(index) => index,
            None => {
                let dmabuf = frame.slot.dmabuf();
                let image = Image::import(
                    device,
                    &Dmabuf {
                        fd: dmabuf.handles().next().context("a dmabuf without planes")?,
                        width: frame.width,
                        height: frame.height,
                        fourcc: dmabuf.format().code as u32,
                        modifier: dmabuf.format().modifier.into(),
                        offset: dmabuf.offsets().next().unwrap_or(0),
                        stride: dmabuf
                            .strides()
                            .next()
                            .context("a dmabuf without a stride")?,
                    },
                )
                .map_err(|e| anyhow::anyhow!("{e}"))?;
                images.push(Imported {
                    slot,
                    generation: frame.generation,
                    image,
                });
                images.len() - 1
            }
        };
        let budget = self
            .settings
            .frame_budget(self.chroma, frame.width, frame.height);
        encoder
            .encode(&images[index].image, budget, PACKET_BYTES, bytes, packets)
            .map_err(|e| anyhow::anyhow!("{e}"))
    }

    fn log(&self, stats: &mut Stats, device: &Device) {
        let now = Instant::now();
        let since = *stats.since.get_or_insert(now);
        if now.duration_since(since) < Duration::from_secs(10) {
            return;
        }
        let secs = now.duration_since(since).as_secs_f64();
        stats.encode_us.sort_unstable();
        let p = |q: f64| {
            stats
                .encode_us
                .get(
                    ((stats.encode_us.len() as f64 * q) as usize)
                        .min(stats.encode_us.len().saturating_sub(1)),
                )
                .copied()
                .unwrap_or(0)
        };
        info!(
            codec = self.name(),
            fps = format!("{:.1}", stats.frames as f64 / secs),
            mbps = format!("{:.1}", stats.bytes as f64 * 8.0 / secs / 1e6),
            packets_per_frame = stats.packets / stats.frames.max(1),
            heals = stats.heals,
            gpu = device.performance_report().join("; "),
            "encoder µs p50/p99: {}/{}",
            p(0.5),
            p(0.99),
        );
        *stats = Stats {
            since: Some(now),
            ..Stats::default()
        };
    }
}

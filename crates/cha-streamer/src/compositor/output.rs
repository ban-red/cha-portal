//! The output: a small pool of buffers the scene is composited into. A buffer
//! is free again once every encoder has dropped its
//! [`Frame`](crate::media::Frame).
//!
//! What a buffer is depends on the device:
//! - `nvidia`: a GBM dmabuf, registered with CUDA once so NVENC reads it in place;
//! - `vaapi`: a GBM dmabuf, for the encoder to import as a VA surface (the
//!   format and modifier are ones the VA driver accepts: see `pick_format`);
//! - `cpu`: planar YUV in memory. llvmpipe draws into one renderbuffer; each
//!   composite reads it back and converts it into the free buffer.

use std::ffi::c_void;
use std::fs::File;
use std::os::fd::OwnedFd;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use cha_nvenc::{CudaContext, RegisteredImage};
use smithay::backend::allocator::dmabuf::{AsDmabuf, Dmabuf};
use smithay::backend::allocator::gbm::{GbmAllocator, GbmBufferFlags, GbmDevice};
use smithay::backend::allocator::{Allocator, Fourcc, Modifier};
use smithay::backend::egl::EGLDisplay;
use smithay::backend::egl::ffi::egl as egl_ffi;
use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::gles::GlesRenderbuffer;
use smithay::backend::renderer::{Bind, Color32F};
use smithay::backend::renderer::{ExportMem, Offscreen};
use smithay::desktop::space::render_output;
use smithay::utils::DeviceFd;
use smithay::utils::{Buffer as BufferCoord, Rectangle};
use tracing::{info, warn};

use super::State;
use crate::device::{Device, DeviceKind};
use crate::encoder::i420::{I420, Order};
use crate::encoder::vaapi;
use crate::media::Frame;

/// Buffers in flight: one being composited, one queued for each encoder, one
/// being encoded, and one an encoder keeps to re-encode as a keyframe.
const POOL_SIZE: usize = 4;
const CLEAR: Color32F = Color32F::new(0.0, 0.0, 0.0, 1.0);

/// One output buffer.
pub struct Slot(SlotBuffer);

enum SlotBuffer {
    /// Fields drop in order: the CUDA registration, then the EGL image it was
    /// made from, then the buffer itself.
    Cuda {
        image: RegisteredImage,
        _egl: EglImage,
        dmabuf: Dmabuf,
    },
    Dmabuf(Dmabuf),
    Pixels(I420),
}

impl Slot {
    /// The buffer's CUDA view (NVIDIA).
    pub fn cuda_image(&self) -> Option<&RegisteredImage> {
        match &self.0 {
            SlotBuffer::Cuda { image, .. } => Some(image),
            _ => None,
        }
    }

    /// The buffer itself, for encoders that import it (PyroWave, VA-API).
    pub fn dmabuf(&self) -> Option<&Dmabuf> {
        match &self.0 {
            SlotBuffer::Cuda { dmabuf, .. } | SlotBuffer::Dmabuf(dmabuf) => Some(dmabuf),
            SlotBuffer::Pixels(_) => None,
        }
    }

    /// The picture in memory (the CPU device).
    pub fn i420(&self) -> Option<&I420> {
        match &self.0 {
            SlotBuffer::Pixels(picture) => Some(picture),
            _ => None,
        }
    }
}

struct EglImage {
    display: EGLDisplay,
    image: egl_ffi::types::EGLImage,
}

// SAFETY: an EGLImage is display-wide; destroying it needs no current context.
unsafe impl Send for EglImage {}
unsafe impl Sync for EglImage {}

impl Drop for EglImage {
    fn drop(&mut self) {
        // SAFETY: created on this display, destroyed once.
        unsafe {
            egl_ffi::DestroyImageKHR(**self.display.get_display_handle(), self.image);
        }
    }
}

struct PoolEntry {
    slot: Arc<Slot>,
    /// The composite sequence number this buffer last received.
    last_seq: Option<u64>,
}

pub struct OutputPool {
    kind: DeviceKind,
    name: String,
    /// The GPU devices': GBM to allocate buffers on, and the EGL display
    /// they are imported through.
    allocator: Option<GbmAllocator<DeviceFd>>,
    egl: Option<EGLDisplay>,
    cuda: Option<Arc<CudaContext>>,
    /// The VA-API device's render node: buffers are checked against its
    /// driver while the format is picked.
    vaapi_node: Option<PathBuf>,
    /// The CPU device: what the scene is drawn into, and whether it holds a
    /// frame yet (the next composite then repaints only what changed).
    renderbuffer: Option<GlesRenderbuffer>,
    drawn: bool,
    /// Threads for converting a read-back picture.
    convert_threads: usize,
    entries: Vec<PoolEntry>,
    size: (u32, u32),
    /// Bumped whenever the buffers are reallocated.
    generation: u64,
    damage: Option<OutputDamageTracker>,
    seq: u64,
    format: Option<(Fourcc, Vec<Modifier>)>,
}

impl OutputPool {
    pub fn new(device: &Device, egl: EGLDisplay, size: (u32, u32)) -> Result<Self> {
        let (allocator, egl) = match device.render_node() {
            Some(render_node) => {
                let file = File::options()
                    .read(true)
                    .write(true)
                    .open(render_node)
                    .with_context(|| format!("opening {}", render_node.display()))?;
                let gbm = GbmDevice::new(DeviceFd::from(OwnedFd::from(file)))
                    .context("creating the GBM device")?;
                (
                    Some(GbmAllocator::new(gbm, GbmBufferFlags::RENDERING)),
                    Some(egl),
                )
            }
            None => (None, None),
        };
        Ok(Self {
            kind: device.kind(),
            name: device.name().to_string(),
            allocator,
            egl,
            cuda: device.cuda(),
            vaapi_node: device
                .render_node()
                .filter(|_| device.kind() == DeviceKind::Vaapi)
                .map(PathBuf::from),
            renderbuffer: None,
            drawn: false,
            convert_threads: std::thread::available_parallelism()
                .map_or(2, |n| n.get())
                .clamp(1, CONVERT_THREADS),
            entries: Vec::new(),
            size,
            generation: 0,
            damage: None,
            seq: 0,
            format: None,
        })
    }

    pub fn gpu_name(&self) -> &str {
        &self.name
    }

    /// The output's size, in pixels.
    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    pub fn resize(&mut self, size: (u32, u32)) -> Result<()> {
        self.size = size;
        // Buffers still held by encoders live on until they're done.
        self.entries.clear();
        self.renderbuffer = None;
        self.drawn = false;
        self.generation += 1;
        Ok(())
    }

    /// Allocates the pool on first use (the renderer knows the formats).
    fn ensure(
        &mut self,
        state_formats: impl FnOnce() -> Vec<(Fourcc, Vec<Modifier>)>,
    ) -> Result<()> {
        if !self.entries.is_empty() {
            return Ok(());
        }
        if self.kind == DeviceKind::Cpu {
            let (width, height) = (self.size.0 as usize, self.size.1 as usize);
            for _ in 0..POOL_SIZE {
                self.entries.push(PoolEntry {
                    slot: Arc::new(Slot(SlotBuffer::Pixels(I420::new(width, height)))),
                    last_seq: None,
                });
            }
            info!(
                width,
                height,
                buffers = POOL_SIZE,
                generation = self.generation,
                "output buffers allocated (in memory, 4:2:0)"
            );
            return Ok(());
        }
        if self.format.is_none() {
            self.format = Some(self.pick_format(state_formats())?);
        }
        let (fourcc, modifiers) = self.format.clone().expect("format picked above");
        for _ in 0..POOL_SIZE {
            let slot = self.allocate(fourcc, &modifiers)?;
            self.entries.push(PoolEntry {
                slot: Arc::new(slot),
                last_seq: None,
            });
        }
        info!(
            width = self.size.0,
            height = self.size.1,
            ?fourcc,
            buffers = POOL_SIZE,
            generation = self.generation,
            "output buffers allocated"
        );
        Ok(())
    }

    /// The first renderable format whose buffers the encoder can import (CUDA
    /// on NVIDIA, a VA surface on VA-API: the driver is asked about a real
    /// buffer): the GPU's own tiling first, then linear.
    fn pick_format(
        &mut self,
        formats: Vec<(Fourcc, Vec<Modifier>)>,
    ) -> Result<(Fourcc, Vec<Modifier>)> {
        let mut tried = Vec::new();
        for (fourcc, modifiers) in formats {
            // NVIDIA's GL allocates compressed block-linear buffers, which its
            // Vulkan can't import (PyroWave reads the buffers through Vulkan).
            let uncompressed: Vec<Modifier> = modifiers
                .iter()
                .copied()
                .filter(|m| !nvidia_compressed(*m))
                .collect();
            let modifiers = if uncompressed.is_empty() {
                modifiers
            } else {
                uncompressed
            };
            let mut candidates = vec![modifiers.clone()];
            if modifiers.contains(&Modifier::Linear) && modifiers.len() > 1 {
                candidates.push(vec![Modifier::Linear]);
            }
            for modifiers in candidates {
                match self.allocate(fourcc, &modifiers) {
                    Ok(slot) => {
                        // VA-API: the encoder says whether it takes this one.
                        if let (Some(node), Some(dmabuf)) = (&self.vaapi_node, slot.dmabuf())
                            && let Err(err) = vaapi::accepts_import(node, dmabuf)
                        {
                            tried.push(format!(
                                "{fourcc:?} {modifiers:?}: VA-API won't import it: {err:#}"
                            ));
                            continue;
                        }
                        return Ok((fourcc, modifiers));
                    }
                    Err(err) => tried.push(format!("{fourcc:?} {modifiers:?}: {err:#}")),
                }
            }
        }
        Err(anyhow!(
            "no output buffer format works for both rendering and encoding:\n{}",
            tried.join("\n")
        ))
    }

    fn allocate(&mut self, fourcc: Fourcc, modifiers: &[Modifier]) -> Result<Slot> {
        let allocator = self.allocator.as_mut().context("no GBM device")?;
        let buffer = allocator
            .create_buffer(self.size.0, self.size.1, fourcc, modifiers)
            .context("allocating a GBM buffer")?;
        let dmabuf = buffer
            .export()
            .context("exporting the buffer as a dmabuf")?;
        let Some(cuda) = &self.cuda else {
            return Ok(Slot(SlotBuffer::Dmabuf(dmabuf)));
        };
        let egl = self.egl.as_ref().context("no EGL display")?;
        let image = egl
            .create_image_from_dmabuf(&dmabuf)
            .context("creating an EGL image")?;
        let egl = EglImage {
            display: egl.clone(),
            image,
        };
        // SAFETY: a live EGL image on this GPU, kept alive by the slot.
        let image = unsafe { RegisteredImage::new(cuda, image as *mut c_void) }
            .map_err(|e| anyhow!("{e}"))?;
        Ok(Slot(SlotBuffer::Cuda {
            image,
            _egl: egl,
            dmabuf,
        }))
    }
}

impl State {
    /// Composites the scene into a free output buffer and publishes it.
    pub(super) fn composite(&mut self) -> Result<()> {
        if self.pool.kind == DeviceKind::Cpu {
            return self.composite_in_memory();
        }
        let formats = || {
            let set = Bind::<Dmabuf>::supported_formats(&self.renderer).unwrap_or_default();
            [Fourcc::Xrgb8888, Fourcc::Argb8888]
                .into_iter()
                .map(|code| {
                    let modifiers = set
                        .iter()
                        .filter(|f| f.code == code)
                        .map(|f| f.modifier)
                        .collect::<Vec<_>>();
                    (code, modifiers)
                })
                .filter(|(_, m)| !m.is_empty())
                .collect::<Vec<_>>()
        };
        let formats = formats();
        self.pool.ensure(|| formats)?;

        let Some(index) = self
            .pool
            .entries
            .iter()
            .position(|e| Arc::strong_count(&e.slot) == 1)
        else {
            // Every buffer is still with the encoders: skip this frame.
            self.stats.starved += 1;
            self.dirty = true;
            return Ok(());
        };
        let seq = self.pool.seq + 1;
        let entry = &self.pool.entries[index];
        let age = entry.last_seq.map_or(0, |last| (seq - last) as usize);
        let mut dmabuf = entry
            .slot
            .dmabuf()
            .context("a GPU output buffer without a dmabuf")?
            .clone();
        let slot = Arc::clone(&entry.slot);

        let cursor = self.cursor_elements();
        let started = Instant::now();
        let damage = self
            .pool
            .damage
            .get_or_insert_with(|| OutputDamageTracker::from_output(&self.output));
        let mut target = self
            .renderer
            .bind(&mut dmabuf)
            .context("binding the output buffer")?;
        let result = render_output(
            &self.output,
            &mut self.renderer,
            &mut target,
            1.0,
            age,
            [&self.space],
            &cursor,
            damage,
            CLEAR,
        )
        .map_err(|e| anyhow!("rendering: {e:?}"))?;
        // The encoders read the buffer as soon as they get it.
        if let Err(err) = result.sync.wait() {
            warn!("waiting for the GPU: {err:?}");
        }
        let rendered = Instant::now();
        drop(target);

        self.pool.seq = seq;
        self.pool.entries[index].last_seq = Some(seq);
        self.stats.composited += 1;
        self.stats
            .render_us
            .push(rendered.duration_since(started).as_micros() as u64);

        self.hub.publish(Frame {
            slot,
            width: self.pool.size.0,
            height: self.pool.size.1,
            generation: self.pool.generation,
            rendered,
        });
        Ok(())
    }
}

impl State {
    /// The CPU device's composite: llvmpipe draws into one renderbuffer, which
    /// is read back and converted into a free buffer. Only what changed is
    /// repainted (the renderbuffer is the same one every time, so its age is
    /// 1); the whole picture is read back and converted.
    fn composite_in_memory(&mut self) -> Result<()> {
        self.pool.ensure(Vec::new)?;
        let Some(index) = self
            .pool
            .entries
            .iter()
            .position(|e| Arc::strong_count(&e.slot) == 1)
        else {
            // Every buffer is still with the encoders: skip this frame.
            self.stats.starved += 1;
            self.dirty = true;
            return Ok(());
        };
        let (width, height) = (self.pool.size.0 as i32, self.pool.size.1 as i32);
        if self.pool.renderbuffer.is_none() {
            let buffer = Offscreen::<GlesRenderbuffer>::create_buffer(
                &mut self.renderer,
                Fourcc::Abgr8888,
                (width, height).into(),
            )
            .context("creating the software renderbuffer")?;
            self.pool.renderbuffer = Some(buffer);
        }
        let age = usize::from(std::mem::replace(&mut self.pool.drawn, false));
        let cursor = self.cursor_elements();
        let started = Instant::now();
        let damage = self
            .pool
            .damage
            .get_or_insert_with(|| OutputDamageTracker::from_output(&self.output));
        let renderbuffer = self.pool.renderbuffer.as_mut().expect("made above");
        let mut target = self
            .renderer
            .bind(renderbuffer)
            .context("binding the renderbuffer")?;
        let result = render_output(
            &self.output,
            &mut self.renderer,
            &mut target,
            1.0,
            age,
            [&self.space],
            &cursor,
            damage,
            CLEAR,
        )
        .map_err(|e| anyhow!("rendering: {e:?}"))?;
        if let Err(err) = result.sync.wait() {
            warn!("waiting for the renderer: {err:?}");
        }
        let region = Rectangle::<i32, BufferCoord>::from_size((width, height).into());
        let mapping = self
            .renderer
            .copy_framebuffer(&target, region, Fourcc::Abgr8888)
            .context("reading the picture back")?;
        drop(target);
        let pixels = self
            .renderer
            .map_texture(&mapping)
            .context("mapping the picture")?;
        // The buffer is free (nothing else holds it): the one writer.
        let slot = Arc::get_mut(&mut self.pool.entries[index].slot).expect("a free buffer");
        let SlotBuffer::Pixels(picture) = &mut slot.0 else {
            anyhow::bail!("a memory output buffer isn't in memory");
        };
        picture.convert(
            pixels,
            width as usize * 4,
            Order::Rgba,
            READBACK_UPSIDE_DOWN,
            self.pool.convert_threads,
        );
        self.pool.drawn = true;
        let rendered = Instant::now();

        let seq = self.pool.seq + 1;
        self.pool.seq = seq;
        self.pool.entries[index].last_seq = Some(seq);
        self.stats.composited += 1;
        self.stats
            .render_us
            .push(rendered.duration_since(started).as_micros() as u64);
        self.hub.publish(Frame {
            slot: Arc::clone(&self.pool.entries[index].slot),
            width: self.pool.size.0,
            height: self.pool.size.1,
            generation: self.pool.generation,
            rendered,
        });
        Ok(())
    }
}

/// `glReadPixels` starts at the framebuffer's bottom row; Smithay's offscreen
/// targets hold the picture top row first, in the same order a dmabuf does
/// (checked by `readback_order_matches_the_picture`).
const READBACK_UPSIDE_DOWN: bool = false;
/// Most threads converting a read-back picture to YUV.
const CONVERT_THREADS: usize = 4;

/// An NVIDIA block-linear modifier with compression (its `c` field):
/// `DRM_FORMAT_MOD_NVIDIA_BLOCK_LINEAR_2D(c, s, g, k, h)` in drm_fourcc.h.
fn nvidia_compressed(modifier: Modifier) -> bool {
    let m: u64 = modifier.into();
    (m >> 56) == 0x03 && (m >> 23) & 0x7 != 0
}

/// Compositor counters, logged every few seconds.
#[derive(Default)]
pub struct Stats {
    /// Encode ticks, and those with nothing new to composite.
    pub encode_ticks: u64,
    pub clean: u64,
    pub composited: u64,
    pub starved: u64,
    pub commits: u64,
    pub render_us: Vec<u64>,
    last_log: Option<Instant>,
}

impl Stats {
    const INTERVAL: Duration = Duration::from_secs(10);

    pub fn maybe_log(&mut self, pool: &OutputPool, windows: usize) {
        let now = Instant::now();
        let last = *self.last_log.get_or_insert(now);
        if now.duration_since(last) < Self::INTERVAL {
            return;
        }
        self.last_log = Some(now);
        let secs = now.duration_since(last).as_secs_f64();
        self.render_us.sort_unstable();
        let pct = |q: f64| {
            self.render_us
                .get(
                    ((self.render_us.len() as f64 * q) as usize)
                        .min(self.render_us.len().saturating_sub(1)),
                )
                .copied()
        };
        if self.composited > 0 || self.commits > 0 {
            info!(
                composited_fps = format!("{:.1}", self.composited as f64 / secs),
                commits_per_s = format!("{:.1}", self.commits as f64 / secs),
                starved = self.starved,
                render_us_p50 = pct(0.5),
                render_us_p99 = pct(0.99),
                generation = pool.generation,
                windows,
                encode_ticks = self.encode_ticks,
                clean = self.clean,
                "compositor"
            );
        }
        self.composited = 0;
        self.encode_ticks = 0;
        self.clean = 0;
        self.starved = 0;
        self.commits = 0;
        self.render_us.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smithay::backend::renderer::gles::GlesRenderer;
    use smithay::backend::renderer::{Frame as _, Renderer as _};
    use smithay::utils::{Physical, Transform};

    #[test]
    fn spots_nvidia_compression() {
        // What NVIDIA's GBM picked (compressed) and what its Vulkan imports.
        assert!(nvidia_compressed(Modifier::from(0x0300_0000_00e0_8014)));
        assert!(!nvidia_compressed(Modifier::from(0x0300_0000_0060_6014)));
        assert!(!nvidia_compressed(Modifier::Linear));
    }

    /// A software renderer (Mesa's llvmpipe) on a renderbuffer, as the CPU
    /// device has.
    fn software(width: i32, height: i32) -> (GlesRenderer, GlesRenderbuffer) {
        let device = Device::open(DeviceKind::Cpu, None).expect("x264 for the CPU device");
        let (egl, _) = super::super::egl_display(&device).expect("Mesa's software EGL");
        let context = smithay::backend::egl::EGLContext::new(&egl).unwrap();
        // SAFETY: used on this thread only.
        let mut renderer = unsafe { GlesRenderer::new(context) }.unwrap();
        let buffer = Offscreen::<GlesRenderbuffer>::create_buffer(
            &mut renderer,
            Fourcc::Abgr8888,
            (width, height).into(),
        )
        .unwrap();
        (renderer, buffer)
    }

    /// Draws `rects` (x, y, w, h, rgb) over a black frame and reads it back.
    fn draw_and_read(
        renderer: &mut GlesRenderer,
        buffer: &mut GlesRenderbuffer,
        rects: &[(i32, i32, i32, i32, [f32; 3])],
    ) -> Vec<u8> {
        let size = buffer.size();
        let mut target = renderer.bind(buffer).unwrap();
        {
            let mut frame = renderer
                .render(&mut target, (size.w, size.h).into(), Transform::Normal)
                .unwrap();
            frame
                .clear(
                    Color32F::new(0.0, 0.0, 0.0, 1.0),
                    &[Rectangle::<i32, Physical>::from_size(
                        (size.w, size.h).into(),
                    )],
                )
                .unwrap();
            for &(x, y, w, h, [r, g, b]) in rects {
                frame
                    .clear(
                        Color32F::new(r, g, b, 1.0),
                        &[Rectangle::<i32, Physical>::new(
                            (x, y).into(),
                            (w, h).into(),
                        )],
                    )
                    .unwrap();
            }
            frame.finish().unwrap().wait().unwrap();
        }
        let mapping = renderer
            .copy_framebuffer(
                &target,
                Rectangle::<i32, BufferCoord>::from_size(size),
                Fourcc::Abgr8888,
            )
            .unwrap();
        renderer.map_texture(&mapping).unwrap().to_vec()
    }

    #[test]
    #[ignore = "needs Mesa's software EGL (libegl-mesa0, libgl1-mesa-dri) and libx264"]
    fn readback_order_matches_the_picture() {
        let (width, height) = (64, 32);
        let (mut renderer, mut buffer) = software(width, height);
        // Red in the top left corner (Smithay's origin), 8×4.
        let pixels = draw_and_read(&mut renderer, &mut buffer, &[(0, 0, 8, 4, [1.0, 0.0, 0.0])]);
        let at = |x: usize, y: usize| &pixels[(y * width as usize + x) * 4..][..4];
        let (top, bottom) = (at(2, 1), at(2, height as usize - 2));
        let red = [255, 0, 0, 255];
        let black = [0, 0, 0, 255];
        let upright = top == red && bottom == black;
        let flipped = top == black && bottom == red;
        assert!(upright || flipped, "top {top:?}, bottom {bottom:?}");
        assert_eq!(
            upright,
            !READBACK_UPSIDE_DOWN,
            "READBACK_UPSIDE_DOWN is {READBACK_UPSIDE_DOWN} but the rows run {}",
            if upright { "top first" } else { "bottom first" }
        );
    }

    /// CPU seconds this process has used.
    fn cpu_seconds() -> f64 {
        // SAFETY: getrusage fills a plain struct.
        let usage = unsafe {
            let mut usage = std::mem::zeroed::<libc::rusage>();
            libc::getrusage(libc::RUSAGE_SELF, &mut usage);
            usage
        };
        let secs = |t: libc::timeval| t.tv_sec as f64 + t.tv_usec as f64 / 1e6;
        secs(usage.ru_utime) + secs(usage.ru_stime)
    }

    /// The CPU device end to end at one size and rate: llvmpipe draws a busy
    /// picture (a few thousand blocks, all changing every frame, so it is
    /// not a still desktop), each frame is read back, converted and encoded
    /// with the real settings, by x264 (H.264) and by SVT-AV1 (AV1, screen
    /// content mode) at the same target. Prints ms per frame by stage, the
    /// cores the encoder alone kept busy at the real frame rate, and the
    /// bit rate: `cargo test -p cha-streamer --release -- --ignored --nocapture cpu_device_cost`.
    /// `CHA_SVTAV1_PRESET` tries another preset.
    #[test]
    #[ignore = "needs Mesa's software EGL, libx264 and libsvtav1enc; prints the CPU device's cost"]
    fn cpu_device_cost() {
        use crate::encoder::Params;
        for (width, height, fps, busy) in [
            (1920, 1080, 60, false),
            (1920, 1080, 60, true),
            (2560, 1440, 60, false),
            (2560, 1440, 60, true),
        ] {
            let (mut renderer, mut buffer) = software(width, height);
            let device = Device::open(DeviceKind::Cpu, None).unwrap();
            let bps = 40_000_000 * (width * height) as u64 / (2560 * 1440);
            let threads = std::thread::available_parallelism()
                .unwrap()
                .get()
                .min(CONVERT_THREADS);
            let frames = 120;
            // The same pictures for each codec: drawn once.
            let mut pictures = Vec::new();
            let (mut draw, mut convert) = (Vec::new(), Vec::new());
            for n in 0..frames {
                let t = Instant::now();
                let rects: Vec<_> = (0..2400)
                    .map(|i| {
                        let (x, y) = ((i % 60) * 32, (i / 60) * 27);
                        let v = ((i * 37 + n * 11) % 256) as f32 / 255.0;
                        (
                            x,
                            y,
                            28,
                            24,
                            [v, 1.0 - v, ((i * 7 + n) % 256) as f32 / 255.0],
                        )
                    })
                    .collect();
                let mut pixels = draw_and_read(&mut renderer, &mut buffer, &rects);
                if busy {
                    // Video-like content: a moving gradient with noise on
                    // every pixel (made outside the timed stages).
                    let mut seed = 0x9E37_79B9u32 ^ n as u32;
                    for (i, p) in pixels.chunks_exact_mut(4).enumerate() {
                        seed ^= seed << 13;
                        seed ^= seed >> 17;
                        seed ^= seed << 5;
                        let base = ((i % width as usize) * 2
                            + (i / width as usize) * 3
                            + n as usize * 5) as u8;
                        p[0] = base.wrapping_add((seed & 31) as u8);
                        p[1] = base.wrapping_mul(3).wrapping_add(((seed >> 5) & 31) as u8);
                        p[2] = base.wrapping_add(((seed >> 10) & 63) as u8);
                    }
                }
                draw.push(t.elapsed());
                let t = Instant::now();
                let mut picture = I420::new(width as usize, height as usize);
                picture.convert(
                    &pixels,
                    width as usize * 4,
                    Order::Rgba,
                    READBACK_UPSIDE_DOWN,
                    threads,
                );
                convert.push(t.elapsed());
                pictures.push(Arc::new(Slot(SlotBuffer::Pixels(picture))));
            }
            let ms = |v: &[Duration]| {
                let mut v: Vec<f64> = v.iter().map(|d| d.as_secs_f64() * 1000.0).collect();
                v.sort_by(|a, b| a.partial_cmp(b).unwrap());
                (
                    v.iter().sum::<f64>() / v.len() as f64,
                    v[v.len() / 2],
                    v[v.len() * 99 / 100],
                )
            };
            let (d, c) = (ms(&draw), ms(&convert));
            println!(
                "{width}x{height}@{fps} {}: draw+readback {:.1} ms (p50 {:.1}, p99 {:.1}), convert {:.1} ({:.1}, {:.1}) on {threads} threads",
                if busy { "busy" } else { "desktop" },
                d.0,
                d.1,
                d.2,
                c.0,
                c.1,
                c.2,
            );
            for codec in [cha_nvenc::Codec::H264, cha_nvenc::Codec::Av1] {
                if !device.codecs().contains(&codec) {
                    println!("  {}: not on this device", codec.name());
                    continue;
                }
                let mut encoder = device
                    .encoder(Params {
                        codec,
                        width: width as u32,
                        height: height as u32,
                        fps,
                        bitrate_bps: bps as u32,
                    })
                    .unwrap();
                let (mut encode, mut bytes, mut keys) = (Vec::new(), 0usize, 0);
                let mut out = Vec::new();
                let mut cpu = 0.0;
                for (n, slot) in pictures.iter().enumerate() {
                    let frame = Frame {
                        slot: Arc::clone(slot),
                        width: width as u32,
                        height: height as u32,
                        generation: 0,
                        rendered: Instant::now(),
                    };
                    let (cpu0, t) = (cpu_seconds(), Instant::now());
                    let key = encoder.encode(&frame, n == 60, &mut out).unwrap();
                    encode.push(t.elapsed());
                    cpu += cpu_seconds() - cpu0;
                    keys += usize::from(key);
                    bytes += out.len();
                }
                let e = ms(&encode);
                println!(
                    "  {}: {:.1} ms (p50 {:.1}, p99 {:.1}); {:.1} cores at {fps} fps ({} keyframes), {:.1} Mbit/s for a {:.1} Mbit/s target",
                    codec.name(),
                    e.0,
                    e.1,
                    e.2,
                    cpu / frames as f64 * fps as f64,
                    keys,
                    bytes as f64 * 8.0 / (frames as f64 / fps as f64) / 1e6,
                    bps as f64 / 1e6,
                );
            }
        }
    }

    /// The AV1 stream the CPU device makes, checked the way a decoder would
    /// meet it: every keyframe is a temporal delimiter, a sequence header
    /// (level 5.1, main tier: `av01.0.13M.08`) and a shown key frame, so a
    /// viewer can join at any of them; the pictures between are inter frames
    /// behind a temporal delimiter. `CHA_SVTAV1_DUMP=<dir>` writes the stream
    /// (`all.obu`) and what follows the second keyframe (`from_key.obu`),
    /// for a decoder to try (`dav1d -i from_key.obu --demuxer section5`).
    #[test]
    #[ignore = "needs Mesa's software EGL, libx264 and libsvtav1enc"]
    fn av1_stream_shape() {
        use crate::encoder::Params;
        use crate::encoder::svtav1::obu;
        let (width, height, fps) = (1920, 1080, 60);
        let (mut renderer, mut buffer) = software(width, height);
        let device = Device::open(DeviceKind::Cpu, None).unwrap();
        assert!(
            device.codecs().contains(&cha_nvenc::Codec::Av1),
            "no SVT-AV1"
        );
        let mut encoder = device
            .encoder(Params {
                codec: cha_nvenc::Codec::Av1,
                width: width as u32,
                height: height as u32,
                fps,
                bitrate_bps: 20_000_000,
            })
            .unwrap();
        let (mut all, mut from_key, mut keys, mut sizes) = (Vec::new(), Vec::new(), 0, Vec::new());
        let mut out = Vec::new();
        for n in 0..150 {
            let rects: Vec<_> = (0..600)
                .map(|i| {
                    let (x, y) = ((i % 30) * 64, (i / 30) * 54);
                    let v = ((i * 37 + n * 11) % 256) as f32 / 255.0;
                    (
                        x,
                        y,
                        60,
                        50,
                        [v, 1.0 - v, ((i * 7 + n) % 256) as f32 / 255.0],
                    )
                })
                .collect();
            let pixels = draw_and_read(&mut renderer, &mut buffer, &rects);
            let mut picture = I420::new(width as usize, height as usize);
            picture.convert(
                &pixels,
                width as usize * 4,
                Order::Rgba,
                READBACK_UPSIDE_DOWN,
                4,
            );
            let frame = Frame {
                slot: Arc::new(Slot(SlotBuffer::Pixels(picture))),
                width: width as u32,
                height: height as u32,
                generation: 0,
                rendered: Instant::now(),
            };
            // A big cut of the target is worth a keyframe (the library takes a
            // new rate on one), the second after the last; this is it.
            if n == 120 {
                encoder.set_bitrate(8_000_000).unwrap();
            }
            let asked = n == 0 || n == 40 || n == 41;
            let key = encoder.encode(&frame, asked, &mut out).unwrap();
            assert_eq!(key, asked || n == 120, "frame {n}");
            sizes.push(out.len());
            let obus = obu::parse(&out);
            assert_eq!(
                obus[0].0,
                obu::TEMPORAL_DELIMITER,
                "frame {n} starts with a delimiter"
            );
            let frame_obu = obus
                .iter()
                .find(|(kind, _)| *kind == obu::FRAME || *kind == obu::FRAME_HEADER)
                .unwrap_or_else(|| panic!("frame {n} has no frame OBU"));
            let (frame_type, shown, existing) = obu::frame(frame_obu.1);
            assert_eq!((shown, existing), (1, 0), "frame {n} is shown");
            if key {
                keys += 1;
                assert_eq!(
                    obus[1].0,
                    obu::SEQUENCE_HEADER,
                    "keyframe {n} has the header"
                );
                assert_eq!(
                    obu::sequence(obus[1].1),
                    (0, 13, 0),
                    "main, level 5.1, tier 0"
                );
                assert_eq!(frame_type, 0, "keyframe {n} is a key frame");
            } else {
                assert_ne!(frame_type, 0, "frame {n} isn't a key frame");
            }
            all.extend_from_slice(&out);
            if keys >= 2 {
                from_key.extend_from_slice(&out);
            }
        }
        assert_eq!(keys, 4);
        let mbps = |frames: &[usize]| {
            frames.iter().sum::<usize>() as f64 * 8.0 * 60.0 / frames.len() as f64 / 1e6
        };
        println!(
            "20 Mbit/s target: {:.1} Mbit/s; after the cut to 8: {:.1} Mbit/s",
            mbps(&sizes[60..120]),
            mbps(&sizes[125..150])
        );
        if let Some(dir) = std::env::var_os("CHA_SVTAV1_DUMP") {
            let dir = std::path::PathBuf::from(dir);
            std::fs::write(dir.join("all.obu"), &all).unwrap();
            std::fs::write(dir.join("from_key.obu"), &from_key).unwrap();
        }
    }
}

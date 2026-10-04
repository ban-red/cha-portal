//! The output: a small pool of GBM buffers the scene is composited into, each
//! registered with CUDA once so the encoders read it in place. A buffer is free
//! again once every encoder has dropped its [`Frame`](crate::media::Frame).

use std::ffi::c_void;
use std::fs::File;
use std::os::fd::OwnedFd;
use std::path::Path;
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
use smithay::backend::renderer::{Bind, Color32F};
use smithay::desktop::space::render_output;
use smithay::utils::DeviceFd;
use tracing::{info, warn};

use super::State;
use crate::media::Frame;

/// Buffers in flight: one being composited, one queued for each encoder, one
/// being encoded, and one an encoder keeps to re-encode as a keyframe.
const POOL_SIZE: usize = 4;
const CLEAR: Color32F = Color32F::new(0.0, 0.0, 0.0, 1.0);

/// One output buffer. Fields drop in order: the CUDA registration, then the
/// EGL image it was made from, then the buffer itself.
pub struct Slot {
    pub image: RegisteredImage,
    _egl: EglImage,
    dmabuf: Dmabuf,
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
    allocator: GbmAllocator<DeviceFd>,
    egl: EGLDisplay,
    cuda: Arc<CudaContext>,
    entries: Vec<PoolEntry>,
    size: (u32, u32),
    /// Bumped whenever the buffers are reallocated.
    generation: u64,
    damage: Option<OutputDamageTracker>,
    seq: u64,
    format: Option<(Fourcc, Vec<Modifier>)>,
}

impl OutputPool {
    pub fn new(
        render_node: &Path,
        egl: EGLDisplay,
        cuda: Arc<CudaContext>,
        size: (u32, u32),
    ) -> Result<Self> {
        let file = File::options()
            .read(true)
            .write(true)
            .open(render_node)
            .with_context(|| format!("opening {}", render_node.display()))?;
        let gbm = GbmDevice::new(DeviceFd::from(OwnedFd::from(file)))
            .context("creating the GBM device")?;
        Ok(Self {
            allocator: GbmAllocator::new(gbm, GbmBufferFlags::RENDERING),
            egl,
            cuda,
            entries: Vec::new(),
            size,
            generation: 0,
            damage: None,
            seq: 0,
            format: None,
        })
    }

    pub fn gpu_name(&self) -> &str {
        self.cuda.name()
    }

    pub fn resize(&mut self, size: (u32, u32)) -> Result<()> {
        self.size = size;
        // Buffers still held by encoders live on until they're done.
        self.entries.clear();
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

    /// The first renderable format whose buffers CUDA can import: the GPU's own
    /// tiling first, then linear.
    fn pick_format(
        &mut self,
        formats: Vec<(Fourcc, Vec<Modifier>)>,
    ) -> Result<(Fourcc, Vec<Modifier>)> {
        let mut tried = Vec::new();
        for (fourcc, modifiers) in formats {
            let mut candidates = vec![modifiers.clone()];
            if modifiers.contains(&Modifier::Linear) && modifiers.len() > 1 {
                candidates.push(vec![Modifier::Linear]);
            }
            for modifiers in candidates {
                match self.allocate(fourcc, &modifiers) {
                    Ok(_) => return Ok((fourcc, modifiers)),
                    Err(err) => tried.push(format!("{fourcc:?} {modifiers:?}: {err:#}")),
                }
            }
        }
        Err(anyhow!(
            "no output buffer format works for both rendering and CUDA:\n{}",
            tried.join("\n")
        ))
    }

    fn allocate(&mut self, fourcc: Fourcc, modifiers: &[Modifier]) -> Result<Slot> {
        let buffer = self
            .allocator
            .create_buffer(self.size.0, self.size.1, fourcc, modifiers)
            .context("allocating a GBM buffer")?;
        let dmabuf = buffer
            .export()
            .context("exporting the buffer as a dmabuf")?;
        let image = self
            .egl
            .create_image_from_dmabuf(&dmabuf)
            .context("creating an EGL image")?;
        let egl = EglImage {
            display: self.egl.clone(),
            image,
        };
        // SAFETY: a live EGL image on this GPU, kept alive by the slot.
        let image = unsafe { RegisteredImage::new(&self.cuda, image as *mut c_void) }
            .map_err(|e| anyhow!("{e}"))?;
        Ok(Slot {
            image,
            _egl: egl,
            dmabuf,
        })
    }
}

impl State {
    /// Composites the scene into a free output buffer and publishes it.
    pub(super) fn composite(&mut self) -> Result<()> {
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
        let mut dmabuf = entry.slot.dmabuf.clone();
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

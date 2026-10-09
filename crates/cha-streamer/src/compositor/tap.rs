//! The compositor's half of the frame tap ([`crate::tap`]).
//!
//! On a GPU device the buffer just composited is blitted, scaled with a linear
//! filter, into a small offscreen buffer of its own, and only that small
//! picture is read back (a few hundred KB, not the 15 MB of a 1440p frame). The
//! output pool, the CUDA registrations and the encoders are not touched, so a
//! tap can't hold a pool buffer back. On the CPU device the picture is already
//! in memory and is averaged down there.
//!
//! The GPU path takes two ticks so the compositor thread never waits on the
//! GPU, which on a node running a game is busy: [`Capture::start`] queues the
//! scale and the readback during a composite, and [`Capture::finish`], on a
//! later tick, maps the result once the GPU has got there. Only one capture is
//! in flight at a time.
//!
//! Nothing here runs unless a reader asked recently ([`Tap::wanted`]).

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::gles::{GlesMapping, GlesRenderbuffer, GlesRenderer, GlesTarget};
use smithay::backend::renderer::sync::SyncPoint;
use smithay::backend::renderer::{Bind, Blit, ExportMem, Offscreen, TextureFilter};
use smithay::utils::{Buffer as BufferCoord, Physical, Rectangle};
use tracing::debug;

use crate::tap::Tap;

/// A scale and readback the GPU is still working on.
struct Pending {
    mapping: GlesMapping,
    sync: SyncPoint,
    started: Instant,
    /// What queueing it cost the compositor thread.
    cost: Duration,
}

pub(super) struct Capture {
    tap: Arc<Tap>,
    /// When the last picture was taken.
    last: Option<Instant>,
    /// The small buffer the GPU path scales into, made on first use.
    renderbuffer: Option<GlesRenderbuffer>,
    pending: Option<Pending>,
}

/// The earliest the readback is mapped. The scale finishes first, and the readback queued
/// behind it needs a few more milliseconds on a GPU a game is using; mapping before then
/// blocks the compositor thread on it (3-4 ms measured at 4 ms).
const MIN_AGE: Duration = Duration::from_millis(12);
/// How long to wait for the GPU before mapping the readback regardless.
const GIVE_UP: Duration = Duration::from_millis(50);

impl Capture {
    pub(super) fn new(tap: Arc<Tap>) -> Self {
        Self {
            tap,
            last: None,
            renderbuffer: None,
            pending: None,
        }
    }

    fn due(&self) -> bool {
        self.last
            .is_none_or(|at| at.elapsed() >= self.tap.interval())
    }

    /// What the tap needs of this tick, as `(a reader is asking, composite for it now)`.
    /// With someone encoding (`listeners`) composites happen as the screen changes and the
    /// tap takes pictures from those, so it only forces one for a first request. With nobody
    /// encoding the compositor would idle, so the tap forces a composite whenever one is due.
    pub(super) fn want(&mut self, listeners: bool) -> (bool, bool) {
        if !self.tap.wanted() {
            return (false, false);
        }
        let first = self.tap.take_stale();
        (
            true,
            first || (!listeners && self.due() && self.pending.is_none()),
        )
    }

    /// Whether this composite should be captured; counts it if so.
    pub(super) fn take_turn(&mut self) -> bool {
        if self.pending.is_none() && self.tap.wanted() && self.due() {
            self.last = Some(Instant::now());
            true
        } else {
            false
        }
    }

    /// The GPU path, first half: queues the scale of `from` (the buffer just drawn, `size`
    /// big) into the small buffer and its readback, without waiting for either.
    pub(super) fn start(
        &mut self,
        renderer: &mut GlesRenderer,
        from: &GlesTarget<'_>,
        size: (u32, u32),
    ) -> Result<()> {
        let began = Instant::now();
        let (tw, th) = (self.tap.width as i32, self.tap.height as i32);
        if self.renderbuffer.is_none() {
            let buffer = Offscreen::<GlesRenderbuffer>::create_buffer(
                renderer,
                Fourcc::Abgr8888,
                (tw, th).into(),
            )
            .context("creating the frame tap's buffer")?;
            self.renderbuffer = Some(buffer);
        }
        let buffer = self.renderbuffer.as_mut().expect("made above");
        let mut to = renderer
            .bind(buffer)
            .context("binding the frame tap's buffer")?;
        let src = Rectangle::<i32, Physical>::from_size((size.0 as i32, size.1 as i32).into());
        let dst = Rectangle::<i32, Physical>::from_size((tw, th).into());
        let sync = renderer
            .blit(from, &mut to, src, dst, TextureFilter::Linear)
            .map_err(|e| anyhow!("scaling the picture for the frame tap: {e:?}"))?;
        let region = Rectangle::<i32, BufferCoord>::from_size((tw, th).into());
        let mapping = renderer
            .copy_framebuffer(&to, region, Fourcc::Abgr8888)
            .context("queueing the frame tap's readback")?;
        self.pending = Some(Pending {
            mapping,
            sync,
            started: Instant::now(),
            cost: began.elapsed(),
        });
        Ok(())
    }

    /// The GPU path, second half, run every tick: once the GPU has finished the scale,
    /// maps the readback and publishes it. Returns what the capture cost the compositor
    /// thread in all, in microseconds, when it publishes one.
    pub(super) fn finish(&mut self, renderer: &mut GlesRenderer) -> Result<Option<u64>> {
        let Some(pending) = &self.pending else {
            return Ok(None);
        };
        let age = pending.started.elapsed();
        if age < MIN_AGE || (!pending.sync.is_reached() && age < GIVE_UP) {
            return Ok(None);
        }
        let pending = self.pending.take().expect("checked above");
        let began = Instant::now();
        let pixels = renderer
            .map_texture(&pending.mapping)
            .context("mapping the frame tap's picture")?;
        self.tap.publish_rgba(pixels);
        debug!(
            queue_us = pending.cost.as_micros() as u64,
            gpu_us = (began - pending.started).as_micros() as u64,
            map_us = began.elapsed().as_micros() as u64,
            "frame tap capture"
        );
        Ok(Some((pending.cost + began.elapsed()).as_micros() as u64))
    }

    /// The CPU path: `rgba` is the whole picture, tightly packed, top row first.
    pub(super) fn capture_pixels(&self, rgba: &[u8], width: usize, height: usize) {
        self.tap.publish_downscaled(rgba, width, height);
    }
}

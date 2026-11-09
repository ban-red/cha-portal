//! Draws a decoded picture into a window on `wgpu`, aspect-fit with black bars,
//! stretched by the GPU's bilinear filter or by SGSR 1 (Snapdragon Game Super
//! Resolution 1, `sgsr1.wgsl`), and times it (research 11, step 4).
//!
//! The caller first draws the picture at its own size into [`Upscaler::source`]
//! (with its own YCbCr to RGB pass, whatever the codec), then
//! [`Upscaler::draw`] stretches that into the window in a render pass of its own.
//! SGSR expects display-encoded values (its thresholds are in 8-bit steps), so
//! the source holds the stream's encoded values even when its format is sRGB.
//!
//! SGSR is a spatial upscaler: one picture in, no motion vectors, which is all a
//! video stream has. It sharpens edges a bilinear stretch softens; it can't
//! bring back detail the stream never carried, so text is better served by
//! streaming at the screen's own size than by upscaling.
//!
//! Provenance: `sgsr1.wgsl` ports SnapdragonStudios/snapdragon-gsr's
//! `sgsr1_shader_mobile.frag` (BSD-3-Clause; the notice is in `LICENSE-SGSR`).
//! The rest is ours.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use bytemuck::{Pod, Zeroable};

/// The WGSL, also used by the browser (`@cha/pyrowave-webgpu`).
pub const SHADER: &str = include_str!("sgsr1.wgsl");

/// SGSR's edge threshold as shipped for phones and desktops (4/255 is for VR).
pub const EDGE_THRESHOLD: f32 = 8.0 / 255.0;
/// SGSR's edge sharpness (1 to 2), as shipped.
pub const EDGE_SHARPNESS: f32 = 2.0;

/// What stretches the picture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Filter {
    /// The GPU's bilinear filter: smooth, soft.
    Bilinear,
    /// SGSR 1: bilinear plus a 12-tap edge-directed sharpening.
    Sgsr,
}

impl Filter {
    /// What the stats panel calls it (`upscaler`).
    pub fn name(self) -> &'static str {
        match self {
            Filter::Bilinear => "bilinear",
            Filter::Sgsr => "SGSR 1",
        }
    }
}

/// A rectangle in target pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// The largest rectangle of `picture`'s aspect that fits in `target`, centred.
pub fn aspect_fit(picture: (u32, u32), target: (u32, u32)) -> Rect {
    let (pw, ph) = (picture.0.max(1) as f32, picture.1.max(1) as f32);
    let (tw, th) = (target.0 as f32, target.1 as f32);
    let scale = (tw / pw).min(th / ph);
    let (w, h) = ((pw * scale).round().min(tw), (ph * scale).round().min(th));
    Rect {
        x: ((tw - w) / 2.0).floor(),
        y: ((th - h) / 2.0).floor(),
        width: w,
        height: h,
    }
}

/// Whether `picture` drawn aspect-fit into `target` comes out bigger by more
/// than a pixel of rounding: when an upscaler has anything to do. Shrinking is
/// left to the bilinear filter.
pub fn enlarges(picture: (u32, u32), target: (u32, u32)) -> bool {
    let r = aspect_fit(picture, target);
    r.width > picture.0 as f32 + 1.0 || r.height > picture.1 as f32 + 1.0
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Params {
    viewport_info: [f32; 4],
    dest: [f32; 4],
    mode: [f32; 4],
}

/// Reads the pass's two timestamps back without stalling: one reading in flight
/// at a time, so a frame is timed only once the last reading has landed.
struct Timing {
    query_set: wgpu::QuerySet,
    resolve: wgpu::Buffer,
    readback: wgpu::Buffer,
    busy: Arc<AtomicBool>,
    /// The newest reading in nanoseconds, 0 once taken.
    ns: Arc<AtomicU64>,
    /// This frame's pass was timed: map the readback after the submit.
    armed: bool,
}

struct Source {
    size: (u32, u32),
    texture: wgpu::Texture,
    /// What the caller draws the picture into.
    target: wgpu::TextureView,
    bind_group: wgpu::BindGroup,
}

/// Stretches a picture into a window (see the crate's docs).
pub struct Upscaler {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    params: wgpu::Buffer,
    source_format: wgpu::TextureFormat,
    to_linear: bool,
    source: Option<Source>,
    timing: Option<Timing>,
}

impl Upscaler {
    /// `source_format` is the format the caller's own renderers draw into (they
    /// draw the picture into [`source`](Self::source)); `target_format` is the
    /// window's. Timed when the device has `TIMESTAMP_QUERY`.
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        source_format: wgpu::TextureFormat,
        target_format: wgpu::TextureFormat,
    ) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("upscale"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("upscale"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("upscale"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("upscale"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(target_format.into())],
            }),
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("upscale"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });
        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("upscale params"),
            size: size_of::<Params>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let timing = device
            .features()
            .contains(wgpu::Features::TIMESTAMP_QUERY)
            .then(|| Timing {
                query_set: device.create_query_set(&wgpu::QuerySetDescriptor {
                    label: Some("upscale timestamps"),
                    ty: wgpu::QueryType::Timestamp,
                    count: 2,
                }),
                resolve: device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("upscale timestamps resolve"),
                    size: 16,
                    usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
                    mapped_at_creation: false,
                }),
                readback: device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("upscale timestamps readback"),
                    size: 16,
                    usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                    mapped_at_creation: false,
                }),
                busy: Arc::new(AtomicBool::new(false)),
                ns: Arc::new(AtomicU64::new(0)),
                armed: false,
            });
        Self {
            device: device.clone(),
            queue: queue.clone(),
            pipeline,
            layout,
            sampler,
            params,
            source_format,
            to_linear: target_format.is_srgb(),
            source: None,
            timing,
        }
    }

    /// Where to draw the picture first, at its own `size`, in the format given
    /// to [`new`](Self::new); made again when the size changes.
    pub fn source(&mut self, size: (u32, u32)) -> &wgpu::TextureView {
        let size = (size.0.max(1), size.1.max(1));
        if self.source.as_ref().is_none_or(|s| s.size != size) {
            self.source = Some(self.make_source(size));
        }
        &self.source.as_ref().expect("just made").target
    }

    fn make_source(&self, size: (u32, u32)) -> Source {
        // SGSR reads encoded values: an sRGB source is sampled through a plain view.
        let sampled = self.source_format.remove_srgb_suffix();
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("upscale source"),
            size: wgpu::Extent3d {
                width: size.0,
                height: size.1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.source_format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_DST,
            view_formats: if sampled == self.source_format {
                &[]
            } else {
                std::slice::from_ref(&sampled)
            },
        });
        let target = texture.create_view(&Default::default());
        let view = texture.create_view(&wgpu::TextureViewDescriptor {
            format: Some(sampled),
            ..Default::default()
        });
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("upscale"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.params.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        Source {
            size,
            texture,
            target,
            bind_group,
        }
    }

    /// The source's texture, once [`source`](Self::source) made it: for a picture
    /// that is already RGB, to copy or write in instead of drawing.
    pub fn source_texture(&self) -> Option<&wgpu::Texture> {
        self.source.as_ref().map(|s| &s.texture)
    }

    /// Records a pass that clears `target` (`target_size` pixels) to black and
    /// draws the source into it aspect-fit with `filter`; timed when it can be.
    /// Returns the rectangle drawn, or None before [`source`](Self::source).
    /// Once per submit: the parameters go through `queue.write_buffer`.
    pub fn draw(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        target_size: (u32, u32),
        filter: Filter,
    ) -> Option<Rect> {
        let source = self.source.as_ref()?;
        let rect = aspect_fit(source.size, target_size);
        let (sw, sh) = (source.size.0 as f32, source.size.1 as f32);
        let params = Params {
            viewport_info: [1.0 / sw, 1.0 / sh, sw, sh],
            dest: [rect.x, rect.y, rect.width, rect.height],
            mode: [
                f32::from(u8::from(filter == Filter::Sgsr)),
                f32::from(u8::from(self.to_linear)),
                EDGE_SHARPNESS,
                EDGE_THRESHOLD,
            ],
        };
        self.queue
            .write_buffer(&self.params, 0, bytemuck::bytes_of(&params));
        let timed = self
            .timing
            .as_mut()
            .filter(|t| !t.busy.load(Ordering::Acquire));
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("upscale"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: timed.as_ref().map(|t| wgpu::RenderPassTimestampWrites {
                    query_set: &t.query_set,
                    beginning_of_pass_write_index: Some(0),
                    end_of_pass_write_index: Some(1),
                }),
                occlusion_query_set: None,
                multiview_mask: None,
            });
            if rect.width >= 1.0 && rect.height >= 1.0 {
                pass.set_viewport(rect.x, rect.y, rect.width, rect.height, 0.0, 1.0);
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, &source.bind_group, &[]);
                pass.draw(0..3, 0..1);
            }
        }
        if let Some(t) = timed {
            encoder.resolve_query_set(&t.query_set, 0..2, &t.resolve, 0);
            encoder.copy_buffer_to_buffer(&t.resolve, 0, &t.readback, 0, 16);
            t.busy.store(true, Ordering::Release);
            t.armed = true;
        }
        Some(rect)
    }

    /// Call after submitting the encoder [`draw`](Self::draw) recorded into:
    /// starts reading the timestamps back.
    pub fn after_submit(&mut self) {
        let Some(t) = &mut self.timing else { return };
        if !std::mem::take(&mut t.armed) {
            return;
        }
        let period = f64::from(self.queue.get_timestamp_period());
        let (buffer, busy, ns) = (t.readback.clone(), t.busy.clone(), t.ns.clone());
        t.readback
            .clone()
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                if result.is_ok() {
                    let ts: Vec<u64> = buffer
                        .slice(..)
                        .get_mapped_range()
                        .as_chunks::<8>()
                        .0
                        .iter()
                        .map(|c| u64::from_le_bytes(*c))
                        .collect();
                    buffer.unmap();
                    if let [begin, end] = ts[..] {
                        let took = (end.saturating_sub(begin) as f64 * period) as u64;
                        ns.store(took.max(1), Ordering::Relaxed);
                    }
                }
                busy.store(false, Ordering::Release);
            });
    }

    /// The pass's GPU time in nanoseconds from the newest reading not yet
    /// taken: None while none has landed, or on a device without timestamps.
    pub fn take_gpu_ns(&self) -> Option<u64> {
        let ns = self.timing.as_ref()?.ns.swap(0, Ordering::Relaxed);
        (ns > 0).then_some(ns)
    }
}

/// Steps SGSR down to bilinear when it costs too much of the frame, and tries
/// it again a while later: an upscaler that makes frames late is worse than a
/// soft picture.
#[derive(Clone, Debug)]
pub struct Ladder {
    wanted: Filter,
    now: Filter,
    /// Measured frames in a row over budget, and SGSR frames measured since a step.
    over: u32,
    /// When bilinear was stepped down to, in frames measured since.
    since_step: u32,
}

impl Ladder {
    /// Measured SGSR frames over budget in a row before stepping down.
    pub const STEP_DOWN_AFTER: u32 = 30;
    /// Measured frames on bilinear before SGSR is tried again.
    pub const RETRY_AFTER: u32 = 600;

    /// Starts at `wanted` (what the user picked).
    pub fn new(wanted: Filter) -> Self {
        Self {
            wanted,
            now: wanted,
            over: 0,
            since_step: 0,
        }
    }

    /// The filter to draw the next frame with.
    pub fn filter(&self) -> Filter {
        self.now
    }

    /// What the pass may cost: a quarter of the frame interval, so decode,
    /// the rest of the draw and the compositor keep the other three.
    pub fn budget(frame: Duration) -> Duration {
        frame / 4
    }

    /// One measured frame: the pass took `gpu` with the frames `frame` apart.
    pub fn record(&mut self, gpu: Duration, frame: Duration) -> Filter {
        match self.now {
            Filter::Sgsr => {
                if gpu > Self::budget(frame) {
                    self.over += 1;
                    if self.over >= Self::STEP_DOWN_AFTER {
                        self.now = Filter::Bilinear;
                        self.over = 0;
                        self.since_step = 0;
                    }
                } else {
                    self.over = 0;
                }
            }
            Filter::Bilinear if self.wanted == Filter::Sgsr => {
                self.since_step += 1;
                if self.since_step >= Self::RETRY_AFTER {
                    self.now = Filter::Sgsr;
                    self.since_step = 0;
                }
            }
            Filter::Bilinear => {}
        }
        self.now
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_macbook_panel_is_filled_by_a_1440p_picture() {
        let r = aspect_fit((2216, 1440), (3024, 1964));
        assert_eq!((r.width, r.height), (3022.0, 1964.0));
        assert_eq!((r.x, r.y), (1.0, 0.0));
        assert!(enlarges((2216, 1440), (3024, 1964)));
    }

    #[test]
    fn a_picture_at_its_own_size_or_shrunk_is_not_enlarged() {
        assert!(!enlarges((2560, 1440), (2560, 1440)));
        assert!(!enlarges((3024, 1960), (3024, 1964)));
        assert!(!enlarges((2560, 1440), (1280, 800)));
    }

    #[test]
    fn the_ladder_keeps_sgsr_inside_its_budget() {
        let frame = Duration::from_micros(16_667);
        let mut l = Ladder::new(Filter::Sgsr);
        for _ in 0..1000 {
            assert_eq!(l.record(Duration::from_micros(900), frame), Filter::Sgsr);
        }
    }

    #[test]
    fn the_ladder_steps_down_after_a_run_of_late_frames_and_retries_later() {
        let frame = Duration::from_micros(8_333); // 120 fps: a 2.08 ms budget
        let slow = Duration::from_micros(3_000);
        let mut l = Ladder::new(Filter::Sgsr);
        for _ in 1..Ladder::STEP_DOWN_AFTER {
            assert_eq!(l.record(slow, frame), Filter::Sgsr);
        }
        assert_eq!(l.record(slow, frame), Filter::Bilinear);
        for _ in 1..Ladder::RETRY_AFTER {
            assert_eq!(
                l.record(Duration::from_micros(400), frame),
                Filter::Bilinear
            );
        }
        assert_eq!(l.record(Duration::from_micros(400), frame), Filter::Sgsr);
    }

    #[test]
    fn one_quick_frame_resets_the_run() {
        let frame = Duration::from_micros(16_667);
        let mut l = Ladder::new(Filter::Sgsr);
        for _ in 0..(Ladder::STEP_DOWN_AFTER - 1) {
            l.record(Duration::from_millis(6), frame);
        }
        l.record(Duration::from_millis(1), frame);
        for _ in 0..(Ladder::STEP_DOWN_AFTER - 1) {
            assert_eq!(l.record(Duration::from_millis(6), frame), Filter::Sgsr);
        }
    }

    #[test]
    fn a_bilinear_choice_stays_bilinear() {
        let mut l = Ladder::new(Filter::Bilinear);
        for _ in 0..(Ladder::RETRY_AFTER * 2) {
            assert_eq!(
                l.record(Duration::from_micros(100), Duration::from_millis(16)),
                Filter::Bilinear
            );
        }
    }

    #[test]
    fn the_shader_parses_and_validates() {
        let module = naga::front::wgsl::parse_str(SHADER).expect("WGSL parses");
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::empty(),
        )
        .validate(&module)
        .expect("WGSL validates");
    }
}

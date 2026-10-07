//! Draws the decoder's planes into a caller's render pass: YCbCr to RGB in a fragment
//! shader reading the storage buffer directly. Equivalent of `render.ts`.
//!
//! Assumes what the TypeScript renderer does: the stream's `range` bit picks limited
//! (Y 16..235, CbCr 16..240) or full range, the `ycbcr_transform` bit picks BT.709 or
//! BT.2020 non-constant-luminance coefficients, 4:2:0 chroma is sampled nearest at the
//! pixel centre, and the primaries/transfer bits are ignored (the output is the stream's
//! encoded values, as a display expects them). With no sequence header seen it uses
//! BT.709 limited range.

use bytemuck::{Pod, Zeroable};

use crate::decoder::PlaneLayout;
use crate::layout::Chroma;
use crate::parser::ColorInfo;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Params {
    video_size: [u32; 2],
    chroma420: u32,
    full_range: u32,
    viewport: [f32; 4],
    bt2020: u32,
    to_linear: u32,
    _pad: [u32; 2],
    offsets: [u32; 4],
    strides: [u32; 4],
}

/// A rectangle in target pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Viewport {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// The largest rect of aspect `video` that fits in `target`, centred.
pub fn aspect_fit(video: (u32, u32), target: (u32, u32)) -> Viewport {
    let (vw, vh) = (video.0.max(1) as f32, video.1.max(1) as f32);
    let (tw, th) = (target.0 as f32, target.1 as f32);
    let scale = (tw / vw).min(th / vh);
    let (w, h) = ((vw * scale).round().min(tw), (vh * scale).round().min(th));
    Viewport {
        x: ((tw - w) / 2.0).floor(),
        y: ((th - h) / 2.0).floor(),
        width: w,
        height: h,
    }
}

pub struct YuvRenderer {
    pipeline: wgpu::RenderPipeline,
    params: wgpu::Buffer,
    to_linear: bool,
    source: Option<Source>,
}

struct Source {
    bind_group: wgpu::BindGroup,
    planes_width: u32,
    planes_height: u32,
    chroma: Chroma,
    offsets: [u32; 3],
    strides: [u32; 3],
    color: Option<ColorInfo>,
}

impl YuvRenderer {
    /// `target_format` is the colour format of the pass it will draw into. For an sRGB
    /// format the shader converts to linear first so the hardware's encode gives back
    /// the stream's values.
    pub fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("pyrowave-yuv"),
            source: wgpu::ShaderSource::Wgsl(include_str!("render.wgsl").into()),
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("pyrowave-yuv"),
            layout: None,
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: target_format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("pyrowave-yuv-params"),
            size: size_of::<Params>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        YuvRenderer {
            pipeline,
            params,
            to_linear: target_format.is_srgb(),
            source: None,
        }
    }

    /// Points the renderer at a decoder's planes (call again after a resize). `color`
    /// is [`crate::Decoder::color`].
    pub fn set_source(
        &mut self,
        device: &wgpu::Device,
        planes: &PlaneLayout,
        color: Option<ColorInfo>,
    ) {
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("pyrowave-yuv"),
            layout: &self.pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.params.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: planes.buffer.as_entire_binding(),
                },
            ],
        });
        self.source = Some(Source {
            bind_group,
            planes_width: planes.width,
            planes_height: planes.height,
            chroma: planes.chroma,
            offsets: planes.offsets,
            strides: planes.strides,
            color,
        });
    }

    /// Updates the colour signalling without rebinding (e.g. when the sequence header changes).
    pub fn set_color(&mut self, color: Option<ColorInfo>) {
        if let Some(s) = &mut self.source {
            s.color = color;
        }
    }

    /// The rect [`draw`](Self::draw) fills for a target of this size.
    pub fn viewport(&self, target: (u32, u32)) -> Option<Viewport> {
        self.source
            .as_ref()
            .map(|s| aspect_fit((s.planes_width, s.planes_height), target))
    }

    /// Draws the picture aspect-fit into `pass`, whose attachment is `target` pixels
    /// (the pass viewport is changed; the bars are whatever the pass cleared to). Call at
    /// most once per submission: the parameters go through `queue.write_buffer`.
    /// Does nothing before [`set_source`](Self::set_source).
    pub fn draw(&self, queue: &wgpu::Queue, pass: &mut wgpu::RenderPass<'_>, target: (u32, u32)) {
        let Some(s) = &self.source else { return };
        let vp = aspect_fit((s.planes_width, s.planes_height), target);
        if vp.width < 1.0 || vp.height < 1.0 {
            return;
        }
        let color = s.color.unwrap_or_default();
        let params = Params {
            video_size: [s.planes_width, s.planes_height],
            chroma420: u32::from(s.chroma == Chroma::C420),
            full_range: u32::from(color.full_range),
            viewport: [vp.x, vp.y, vp.width, vp.height],
            bt2020: u32::from(color.ycbcr_transform == 1),
            to_linear: u32::from(self.to_linear),
            _pad: [0; 2],
            offsets: [s.offsets[0], s.offsets[1], s.offsets[2], 0],
            strides: [s.strides[0], s.strides[1], s.strides[2], 0],
        };
        queue.write_buffer(&self.params, 0, bytemuck::bytes_of(&params));
        pass.set_viewport(vp.x, vp.y, vp.width, vp.height, 0.0, 1.0);
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, Some(&s.bind_group), &[]);
        pass.draw(0..3, 0..1);
    }
}

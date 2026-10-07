//! Draws an imported picture, aspect-fit, with black bars.

use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;

use super::color;
use crate::present::ImportedFrame;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Params {
    rows: [[f32; 4]; 3],
    offset: [f32; 4],
}

pub struct VideoPipeline {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
}

/// What to draw this frame: the bind group is built per picture, since the
/// planes are new textures every time.
pub struct Prepared {
    bind_group: wgpu::BindGroup,
    pub viewport: Viewport,
}

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

impl VideoPipeline {
    pub fn new(device: &wgpu::Device, target: wgpu::TextureFormat) -> Self {
        let module = device.create_shader_module(wgpu::include_wgsl!("video.wgsl"));
        let texture = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("video"),
            entries: &[
                texture(0),
                texture(1),
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("video"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("video"),
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
                targets: &[Some(target.into())],
            }),
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("video"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });
        Self {
            pipeline,
            layout,
            sampler,
        }
    }

    pub fn prepare(
        &self,
        device: &wgpu::Device,
        frame: &ImportedFrame,
        target: (u32, u32),
    ) -> Prepared {
        let conversion = color::conversion(frame.color);
        let row = |r: [f32; 3]| [r[0], r[1], r[2], 0.0];
        let params = Params {
            rows: conversion.rows.map(row),
            offset: [
                conversion.offset[0],
                conversion.offset[1],
                conversion.offset[2],
                conversion.sample_scale,
            ],
        };
        let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("video params"),
            contents: bytemuck::bytes_of(&params),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let luma = frame.luma.create_view(&Default::default());
        let chroma = frame.chroma.create_view(&Default::default());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("video"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&luma),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&chroma),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: uniform.as_entire_binding(),
                },
            ],
        });
        Prepared {
            bind_group,
            viewport: aspect_fit((frame.width, frame.height), target),
        }
    }

    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>, prepared: &Prepared) {
        let v = prepared.viewport;
        if v.width < 1.0 || v.height < 1.0 {
            return;
        }
        pass.set_viewport(v.x, v.y, v.width, v.height, 0.0, 1.0);
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &prepared.bind_group, &[]);
        pass.draw(0..3, 0..1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fits_wide_video_in_tall_window() {
        let v = aspect_fit((1600, 900), (800, 800));
        assert_eq!((v.x, v.width, v.height), (0.0, 800.0, 450.0));
        assert_eq!(v.y, 175.0);
    }

    #[test]
    fn fits_tall_target_with_pillars() {
        let v = aspect_fit((1600, 1000), (2000, 1000));
        assert_eq!((v.width, v.height, v.x, v.y), (1600.0, 1000.0, 200.0, 0.0));
    }
}

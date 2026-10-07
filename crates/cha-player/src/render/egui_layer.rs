//! egui on the player's `wgpu` device: input in, paint out.

use std::time::Duration;

use winit::event::WindowEvent;
use winit::window::Window;

use super::Gpu;

pub struct EguiLayer {
    ctx: egui::Context,
    state: egui_winit::State,
    renderer: egui_wgpu::Renderer,
    /// Textures egui is done with; freed after the next submit.
    stale: Vec<egui::TextureId>,
}

/// One egui pass, tessellated and ready to paint.
pub struct Frame {
    primitives: Vec<egui::ClippedPrimitive>,
    textures: egui::TexturesDelta,
    pixels_per_point: f32,
    /// How soon egui wants another pass (zero: now).
    pub repaint_after: Duration,
}

impl EguiLayer {
    pub fn new(gpu: &Gpu) -> Self {
        let ctx = egui::Context::default();
        let state = egui_winit::State::new(
            ctx.clone(),
            egui::ViewportId::ROOT,
            gpu.window.as_ref(),
            Some(gpu.window.scale_factor() as f32),
            None,
            Some(gpu.device.limits().max_texture_dimension_2d as usize),
        );
        let renderer = egui_wgpu::Renderer::new(
            &gpu.device,
            gpu.format,
            egui_wgpu::RendererOptions::default(),
        );
        Self {
            ctx,
            state,
            renderer,
            stale: Vec::new(),
        }
    }

    /// True when egui used the event (a click on a widget, typing in a field).
    pub fn on_event(&mut self, window: &Window, event: &WindowEvent) -> bool {
        let response = self.state.on_window_event(window, event);
        response.consumed
    }

    /// Run the UI once.
    pub fn run(&mut self, window: &Window, mut build: impl FnMut(&mut egui::Ui)) -> Frame {
        let input = self.state.take_egui_input(window);
        let output = self.ctx.run_ui(input, |ui| build(ui));
        self.state
            .handle_platform_output(window, output.platform_output);
        let repaint_after = output
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .map_or(Duration::MAX, |v| v.repaint_delay);
        let pixels_per_point = output.pixels_per_point;
        Frame {
            primitives: self.ctx.tessellate(output.shapes, pixels_per_point),
            textures: output.textures_delta,
            pixels_per_point,
            repaint_after,
        }
    }

    /// Draw `frame` into `target`, over what is there (`clear` is `None`) or
    /// over a cleared background.
    pub fn paint(
        &mut self,
        gpu: &Gpu,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        frame: &Frame,
        clear: Option<wgpu::Color>,
    ) {
        for id in self.stale.drain(..) {
            self.renderer.free_texture(&id);
        }
        for (id, delta) in &frame.textures.set {
            self.renderer
                .update_texture(&gpu.device, &gpu.queue, *id, delta);
        }
        let (width, height) = gpu.size();
        let screen = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [width, height],
            pixels_per_point: frame.pixels_per_point,
        };
        let extra = self.renderer.update_buffers(
            &gpu.device,
            &gpu.queue,
            encoder,
            &frame.primitives,
            &screen,
        );
        // Callback buffers (none here) must be submitted with the frame.
        debug_assert!(extra.is_empty());

        let pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("egui"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: clear.map_or(wgpu::LoadOp::Load, wgpu::LoadOp::Clear),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        self.renderer
            .render(&mut pass.forget_lifetime(), &frame.primitives, &screen);
        self.stale.extend(frame.textures.free.iter().copied());
    }
}

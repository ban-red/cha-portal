//! The GPU side: one `wgpu` device on Metal shared by the video presenter and
//! egui, and the window surface.

pub mod color;
pub mod egui_layer;
pub mod video;

use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use winit::window::Window;

pub struct Gpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub format: wgpu::TextureFormat,
    pub window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
}

/// Why no frame could be drawn this time.
#[derive(Debug)]
pub enum Skip {
    /// Hidden or timed out: try again on the next redraw.
    Later,
    /// The surface was reconfigured: redraw now.
    Retry,
}

impl Gpu {
    pub fn new(window: Arc<Window>) -> Result<Self> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::METAL,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let surface = instance
            .create_surface(window.clone())
            .context("creating the window surface")?;
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
        }))
        .map_err(|e| anyhow!("no Metal adapter: {e}"))?;
        tracing::info!(adapter = %adapter.get_info().name, "GPU");

        // 16-bit normalised planes carry 10-bit video; most Apple GPUs have them.
        // Subgroups run the PyroWave decoder (without them PyroWave is refused,
        // the other codecs are unaffected); timestamps time it.
        let features = adapter.features()
            & (wgpu::Features::TEXTURE_FORMAT_16BIT_NORM
                | wgpu::Features::SUBGROUP
                | wgpu::Features::TIMESTAMP_QUERY);
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("cha-player"),
            required_features: features,
            ..Default::default()
        }))
        .context("creating the GPU device")?;

        let caps = surface.get_capabilities(&adapter);
        let format = egui_wgpu::preferred_framebuffer_format(&caps.formats)
            .map_err(|e| anyhow!("no usable surface format: {e}"))?;
        // Lowest latency first: never wait for a vblank we could skip.
        let present_mode = [
            wgpu::PresentMode::Mailbox,
            wgpu::PresentMode::Immediate,
            wgpu::PresentMode::Fifo,
        ]
        .into_iter()
        .find(|m| caps.present_modes.contains(m))
        .unwrap_or(wgpu::PresentMode::Fifo);
        tracing::info!(?present_mode, ?format, "surface");

        let size = window.inner_size();
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode,
            desired_maximum_frame_latency: 1,
            alpha_mode: caps
                .alpha_modes
                .first()
                .copied()
                .unwrap_or(wgpu::CompositeAlphaMode::Auto),
            view_formats: vec![],
        };
        surface.configure(&device, &config);
        Ok(Self {
            device,
            queue,
            format,
            window,
            surface,
            config,
        })
    }

    pub fn size(&self) -> (u32, u32) {
        (self.config.width, self.config.height)
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
    }

    pub fn acquire(&mut self) -> Result<wgpu::SurfaceTexture, Skip> {
        use wgpu::CurrentSurfaceTexture as T;
        match self.surface.get_current_texture() {
            T::Success(frame) => Ok(frame),
            T::Suboptimal(frame) => {
                // Draw this one; fix the surface for the next.
                self.surface.configure(&self.device, &self.config);
                Ok(frame)
            }
            T::Timeout | T::Occluded => {
                tracing::debug!("surface timeout or occluded");
                Err(Skip::Later)
            }
            T::Outdated | T::Lost => {
                let size = self.window.inner_size();
                self.resize(size.width, size.height);
                Err(Skip::Retry)
            }
            T::Validation => Err(Skip::Later),
        }
    }
}

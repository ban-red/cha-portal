//! Copies decoded planes to the CPU. For tests and debugging; the player never reads back.

use crate::decoder::PlaneLayout;
use crate::layout::{BlockLayout, Chroma};

/// Tightly packed 8-bit planes, `width * height` bytes each (chroma halved for 4:2:0).
#[derive(Clone, Debug)]
pub struct Planes {
    pub chroma: Chroma,
    pub width: u32,
    pub height: u32,
    pub y: Vec<u8>,
    pub cb: Vec<u8>,
    pub cr: Vec<u8>,
}

impl PlaneLayout {
    /// Blocks until the GPU has finished everything submitted, then returns the planes.
    pub fn read_back(&self, device: &wgpu::Device, queue: &wgpu::Queue) -> Planes {
        let size = self.buffer.size();
        let staging = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("pyrowave-readback"),
            size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        encoder.copy_buffer_to_buffer(&self.buffer, 0, &staging, 0, size);
        queue.submit([encoder.finish()]);
        staging
            .slice(..)
            .map_async(wgpu::MapMode::Read, |r| r.expect("map readback buffer"));
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("poll");
        let mapped = staging.slice(..).get_mapped_range();

        // Same plane geometry the decoder uses.
        let geometry =
            BlockLayout::new(self.width, self.height, self.chroma).expect("valid layout");
        let plane = |i: usize| {
            let (w, h) = (
                geometry.plane_width(i) as usize,
                geometry.plane_height(i) as usize,
            );
            let stride = self.strides[i] as usize * 4;
            let base = self.offsets[i] as usize * 4;
            let mut out = Vec::with_capacity(w * h);
            for row in 0..h {
                out.extend_from_slice(&mapped[base + row * stride..base + row * stride + w]);
            }
            out
        };
        let planes = Planes {
            chroma: self.chroma,
            width: self.width,
            height: self.height,
            y: plane(0),
            cb: plane(1),
            cr: plane(2),
        };
        drop(mapped);
        staging.unmap();
        planes
    }
}

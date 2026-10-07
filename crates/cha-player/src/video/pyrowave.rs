//! PyroWave on the render thread's `wgpu` device (ADR 0013, C2.3).
//!
//! Unlike H.264/HEVC there is no separate decoder object: PyroWave is decoded
//! by compute shaders ([`cha_pyrowave_wgpu`]) into a buffer the video pass then
//! samples, so it must run on the device and queue the window draws with. The
//! video thread therefore only keeps the newest raw [`VideoFrame`]
//! ([`crate::session`]), and [`PyroPresenter`] decodes it into the same command
//! encoder, and the same submit, as the frame's render pass.
//!
//! Every `VideoFrame` of a PyroWave session has `data` = the frame's whole
//! wavelet packets concatenated, starting with the 8-byte sequence header;
//! `partial` is true when it was delivered at the deadline with only the
//! packets that arrived (the decoder then fills in the rest, softer where
//! blocks are missing). Frames stand alone, so an error is counted and the
//! next frame is simply decoded: no keyframe is ever requested.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Instant;

use anyhow::{Result, anyhow, bail};
use cha_client::VideoFrame;
use cha_pyrowave_wgpu::{
    DecodeTimestamps, Decoder, Pipelines, Precision, YuvRenderer, parse_sequence_header,
};

/// Reads GPU timestamps back without stalling: one readback buffer, and a frame
/// only carries timestamps while the previous reading has landed.
struct Timing {
    query_set: wgpu::QuerySet,
    resolve: wgpu::Buffer,
    readback: wgpu::Buffer,
    busy: Arc<AtomicBool>,
    /// The latest GPU time for dequant + inverse transform, microseconds.
    gpu_us: Arc<AtomicU32>,
    /// A timestamp copy was recorded this frame; map it after the submit.
    armed: bool,
}

/// What [`PyroPresenter::encode`] did for a frame.
#[derive(Clone, Copy, Debug)]
pub struct Encoded {
    /// Decode time for the stats, microseconds: the latest GPU measurement
    /// when timestamps are available (one frame in a few is measured),
    /// else the CPU time to parse and record.
    pub decode_us: u32,
    /// The frame was decoded from only some of its packets.
    pub partial: bool,
}

pub struct PyroPresenter {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipelines: Pipelines,
    renderer: YuvRenderer,
    decoder: Option<Decoder>,
    /// Size of the picture in the planes, once a frame has been decoded.
    picture: Option<(u32, u32)>,
    timing: Option<Timing>,
}

impl PyroPresenter {
    /// Compiles the decode pipelines (once; they live as long as the
    /// presenter). Fails with a clear error when the GPU has no subgroup
    /// operations.
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        target: wgpu::TextureFormat,
    ) -> Result<Self> {
        Pipelines::supports(device).map_err(|_| {
            anyhow!(
                "PyroWave needs GPU subgroup operations, which this GPU or driver doesn't offer; \
                 pick another codec"
            )
        })?;
        let pipelines = Pipelines::new(device, Precision::default())
            .map_err(|e| anyhow!("PyroWave decode pipelines: {e}"))?;
        let timing = device
            .features()
            .contains(wgpu::Features::TIMESTAMP_QUERY)
            .then(|| Timing {
                query_set: device.create_query_set(&wgpu::QuerySetDescriptor {
                    label: Some("pyrowave timestamps"),
                    ty: wgpu::QueryType::Timestamp,
                    count: 4,
                }),
                resolve: device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("pyrowave timestamps resolve"),
                    size: 32,
                    usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
                    mapped_at_creation: false,
                }),
                readback: device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("pyrowave timestamps readback"),
                    size: 32,
                    usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                    mapped_at_creation: false,
                }),
                busy: Arc::new(AtomicBool::new(false)),
                gpu_us: Arc::new(AtomicU32::new(0)),
                armed: false,
            });
        Ok(Self {
            device: device.clone(),
            queue: queue.clone(),
            pipelines,
            renderer: YuvRenderer::new(device, target),
            decoder: None,
            picture: None,
            timing,
        })
    }

    /// Forget the current picture and decoder (a new stream starts).
    pub fn reset(&mut self) {
        self.decoder = None;
        self.picture = None;
    }

    /// The decoded picture's size, once there is one: the mouse mapping uses it.
    pub fn picture_size(&self) -> Option<(u32, u32)> {
        self.picture
    }

    /// Decodes `frame` into `encoder`. The decoder is made from the frame's
    /// sequence header and remade when the size or chroma changes. Call
    /// [`after_submit`](Self::after_submit) once `encoder` is submitted, and
    /// submit it before encoding another frame.
    pub fn encode(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        frame: &VideoFrame,
    ) -> Result<Encoded> {
        // Lets finished timestamp readbacks call back; never blocks.
        let _ = self.device.poll(wgpu::PollType::Poll);

        let Some(head) = parse_sequence_header(&frame.data) else {
            bail!("PyroWave frame without a sequence header");
        };
        let stale = self.decoder.as_ref().is_none_or(|d| {
            let p = d.planes();
            (p.width, p.height, p.chroma) != (head.width, head.height, head.chroma)
        });
        if stale {
            self.picture = None;
            self.decoder = None;
            let decoder = Decoder::new(
                &self.device,
                &self.queue,
                &self.pipelines,
                head.width,
                head.height,
                head.chroma,
            )
            .map_err(|e| anyhow!("PyroWave decoder for {}x{}: {e}", head.width, head.height))?;
            self.renderer
                .set_source(&self.device, decoder.planes(), None);
            self.decoder = Some(decoder);
        }
        let decoder = self.decoder.as_mut().expect("just made");

        let measure = self
            .timing
            .as_ref()
            .is_some_and(|t| !t.busy.load(Ordering::Acquire));
        let timestamps = self
            .timing
            .as_ref()
            .filter(|_| measure)
            .map(|t| DecodeTimestamps {
                query_set: &t.query_set,
                first: 0,
            });
        let started = Instant::now();
        let recorded = decoder
            .encode_frame(encoder, &frame.data, frame.partial, timestamps)
            .map_err(|e| anyhow!("PyroWave packets: {e}"))?;
        if !recorded {
            bail!(
                "PyroWave frame {} incomplete ({} of {} blocks)",
                frame.number,
                decoder.blocks_received(),
                decoder.blocks_expected()
            );
        }
        let cpu_us = started.elapsed().as_micros() as u32;
        self.renderer.set_color(decoder.color());
        self.picture = Some((head.width, head.height));

        if measure && let Some(t) = &mut self.timing {
            encoder.resolve_query_set(&t.query_set, 0..4, &t.resolve, 0);
            encoder.copy_buffer_to_buffer(&t.resolve, 0, &t.readback, 0, 32);
            t.busy.store(true, Ordering::Release);
            t.armed = true;
        }
        let gpu_us = self
            .timing
            .as_ref()
            .map_or(0, |t| t.gpu_us.load(Ordering::Relaxed));
        Ok(Encoded {
            decode_us: if gpu_us > 0 { gpu_us } else { cpu_us },
            partial: frame.partial,
        })
    }

    /// Draws the latest picture aspect-fit into `pass` (`target` is the
    /// attachment size). Once per submit.
    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>, target: (u32, u32)) {
        if self.picture.is_some() {
            self.renderer.draw(&self.queue, pass, target);
        }
    }

    /// Call after submitting the encoder [`encode`](Self::encode) recorded
    /// into: starts reading back the GPU timestamps.
    pub fn after_submit(&mut self) {
        let Some(t) = &mut self.timing else { return };
        if !std::mem::take(&mut t.armed) {
            return;
        }
        let period = f64::from(self.queue.get_timestamp_period());
        let (buffer, busy, gpu_us) = (t.readback.clone(), t.busy.clone(), t.gpu_us.clone());
        t.readback
            .clone()
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                if result.is_ok() {
                    let ts: Vec<u64> = buffer
                        .slice(..)
                        .get_mapped_range()
                        .chunks_exact(8)
                        .map(|c| u64::from_le_bytes(c.try_into().unwrap()))
                        .collect();
                    buffer.unmap();
                    if let [d0, d1, i0, i1] = ts[..] {
                        let ns = (d1.saturating_sub(d0) + i1.saturating_sub(i0)) as f64 * period;
                        gpu_us.store((ns / 1000.0) as u32, Ordering::Relaxed);
                    }
                }
                busy.store(false, Ordering::Release);
            });
    }
}

#[cfg(test)]
mod tests {
    //! Drives the player's PyroWave path headless: a clip's frames as
    //! `VideoFrame`s through `PyroPresenter::encode` and `draw` into an
    //! offscreen texture, as `app.rs` does per redraw.

    use std::path::PathBuf;

    use bytes::Bytes;
    use cha_client::Codec;
    use cha_pyrowave_wgpu::{Chroma, parse_pyrowave_file, split_records};

    use super::*;

    fn device(subgroups: bool) -> Option<(wgpu::Device, wgpu::Queue)> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = pollster::block_on(instance.request_adapter(&Default::default())).ok()?;
        let mut features = adapter.features() & wgpu::Features::TIMESTAMP_QUERY;
        if subgroups {
            features |= adapter.features() & wgpu::Features::SUBGROUP;
        }
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            required_features: features,
            ..Default::default()
        }))
        .ok()
    }

    fn clip(name: &str) -> Option<Vec<u8>> {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../spikes/s1b-pyrowave-webgpu/clips")
            .join(name);
        std::fs::read(path).ok()
    }

    fn video_frame(codec: Codec, data: &[u8], number: u64, partial: bool) -> VideoFrame {
        VideoFrame {
            codec,
            data: Bytes::copy_from_slice(data),
            key: true,
            partial,
            number,
            received: Instant::now(),
        }
    }

    /// Renders `presenter`'s picture into a target and returns the RGBA bytes.
    fn render(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        presenter: &mut PyroPresenter,
        frame: &VideoFrame,
        size: (u32, u32),
    ) -> Result<(Vec<u8>, Encoded)> {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width: size.0,
                height: size.1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        let bpr = (size.0 * 4).next_multiple_of(256);
        let out = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: u64::from(bpr * size.1),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        let encoded = presenter.encode(&mut encoder, frame);
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            presenter.draw(&mut pass, size);
        }
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &out,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(bpr),
                    rows_per_image: None,
                },
            },
            wgpu::Extent3d {
                width: size.0,
                height: size.1,
                depth_or_array_layers: 1,
            },
        );
        queue.submit([encoder.finish()]);
        presenter.after_submit();
        out.slice(..).map_async(wgpu::MapMode::Read, |r| r.unwrap());
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        let mapped = out.slice(..).get_mapped_range();
        let mut rgba = Vec::new();
        for row in 0..size.1 as usize {
            let o = row * bpr as usize;
            rgba.extend_from_slice(&mapped[o..o + size.0 as usize * 4]);
        }
        drop(mapped);
        out.unmap();
        Ok((rgba, encoded?))
    }

    #[test]
    fn plays_clip_frames_through_the_presenter() {
        let Some((device, queue)) = device(true) else {
            eprintln!("SKIP: no wgpu adapter");
            return;
        };
        if !device.features().contains(wgpu::Features::SUBGROUP) {
            eprintln!("SKIP: no subgroups");
            return;
        }
        let Some(data) = clip("testsrc2-1440p-420-604k.pyrowave") else {
            eprintln!("SKIP: spike clips not present");
            return;
        };
        let file = parse_pyrowave_file(&data).unwrap();
        assert_eq!(file.chroma, Chroma::C420);
        let mut presenter =
            PyroPresenter::new(&device, &queue, wgpu::TextureFormat::Rgba8Unorm).unwrap();
        let size = (640, 480); // letterboxes the 16:9 picture
        let mut last = None;
        for (i, f) in file.frames.iter().take(8).enumerate() {
            let frame = video_frame(Codec::PyroWave420, f, i as u64, false);
            let (rgba, encoded) = render(&device, &queue, &mut presenter, &frame, size).unwrap();
            assert!(!encoded.partial && encoded.decode_us > 0);
            // The bars stay black; the picture is not.
            assert_eq!(&rgba[..4], &[0, 0, 0, 255]);
            let mid = (240 * 640 + 320) * 4;
            assert!(rgba.iter().step_by(4).any(|&r| r > 32), "picture drawn");
            last = Some((rgba, mid));
        }
        assert_eq!(presenter.picture_size(), Some((2560, 1440)));

        // A frame missing some blocks is decoded as a partial frame.
        let f = file.frames[3];
        let mut kept = Vec::new();
        // Drop every 10th record of the finest bands (the highest block
        // indices); the coarse ones must all be there for a partial decode.
        for (n, rec) in split_records(f).into_iter().enumerate() {
            let index = u32::from_le_bytes(rec[4..8].try_into().unwrap()) >> 8;
            if n == 0 || index < 600 || n % 10 != 0 {
                kept.extend_from_slice(rec);
            }
        }
        let partial = video_frame(Codec::PyroWave420, &kept, 99, true);
        let (_, encoded) = render(&device, &queue, &mut presenter, &partial, size).unwrap();
        assert!(encoded.partial);
        // Strictly incomplete when not marked partial: an error, not a crash.
        let strict = video_frame(Codec::PyroWave420, &kept, 100, false);
        assert!(render(&device, &queue, &mut presenter, &strict, size).is_err());
        // Garbage: an error too, and the presenter still decodes afterwards.
        let junk = video_frame(Codec::PyroWave420, &[1, 2, 3], 101, false);
        assert!(render(&device, &queue, &mut presenter, &junk, size).is_err());
        let again = video_frame(Codec::PyroWave420, file.frames[4], 102, false);
        let (rgba, _) = render(&device, &queue, &mut presenter, &again, size).unwrap();
        assert!(rgba.iter().step_by(4).any(|&r| r > 32));
        drop(last);

        // A different size and chroma remakes the decoder.
        if let Some(data) = clip("testsrc2-1440p-444-1229k.pyrowave") {
            let file = parse_pyrowave_file(&data).unwrap();
            let frame = video_frame(Codec::PyroWave444, file.frames[0], 200, false);
            render(&device, &queue, &mut presenter, &frame, size).unwrap();
            assert_eq!(presenter.picture_size(), Some((2560, 1440)));
        }
    }

    #[test]
    fn refuses_without_subgroups() {
        let Some((device, queue)) = device(false) else {
            eprintln!("SKIP: no wgpu adapter");
            return;
        };
        let err = PyroPresenter::new(&device, &queue, wgpu::TextureFormat::Rgba8Unorm)
            .err()
            .expect("no subgroups");
        assert!(err.to_string().contains("subgroup"), "{err}");
    }
}

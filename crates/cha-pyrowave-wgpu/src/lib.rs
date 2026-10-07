//! PyroWave decoding on `wgpu`, for the native player: packets go through dequant and
//! the inverse wavelet transform into packed 8-bit YCbCr planes in a GPU buffer, and
//! [`YuvRenderer`] draws those planes into a render pass with no readback.
//!
//! Provenance (MIT; the notice is in `LICENSE-PYROWAVE`):
//! - `src/shaders/*.wgsl` are vendored byte-for-byte from
//!   [imbcmdth/pyrowave](https://github.com/imbcmdth/pyrowave), branch `webgpu` at
//!   `5e80f92` (2026-09-26), by way of `web/packages/pyrowave-webgpu`.
//! - `layout`, `parser`, `decoder` and `pipelines` are a Rust port of that branch's C++
//!   host (`pyrowave_webgpu_decoder.cpp`, `pyrowave_webgpu_common.cpp`,
//!   `metal/pyrowave_bitstream.cpp`), through our TypeScript port of the same.
//! - `render` and `render.wgsl` port `web/packages/pyrowave-webgpu/src/render.ts` (ours).
//!
//! Needs `wgpu::Features::SUBGROUP` on the device (Metal on Apple silicon has it).
//! The bitstream has no version field; this matches upstream PyroWave `89f7e47`.
//!
//! ```ignore
//! let pipelines = Pipelines::new(&device, Precision::default())?;
//! let head = parse_sequence_header(frame).unwrap();
//! let mut decoder = Decoder::new(&device, &queue, &pipelines, head.width, head.height, head.chroma)?;
//! let mut renderer = YuvRenderer::new(&device, surface_format);
//! renderer.set_source(&device, decoder.planes(), None);
//! // per frame:
//! let mut encoder = device.create_command_encoder(&Default::default());
//! if decoder.encode_frame(&mut encoder, frame, true, None)? {
//!     renderer.set_color(decoder.color());
//!     // ... begin the render pass on `encoder`, then renderer.draw(&queue, &mut pass, size)
//! }
//! queue.submit([encoder.finish()]);
//! ```

mod decoder;
mod error;
mod file;
mod layout;
mod parser;
mod pipelines;
mod readback;
mod render;

pub use decoder::{DecodeTimestamps, Decoder, PlaneLayout};
pub use error::{Error, PacketError};
pub use file::{PyroWaveFile, parse_pyrowave_file};
pub use layout::{BlockLayout, Chroma};
pub use parser::{
    BitstreamParser, ColorInfo, SequenceHeader, parse_sequence_header, split_records,
};
pub use pipelines::{Pipelines, Precision};
pub use readback::Planes;
pub use render::{Viewport, YuvRenderer, aspect_fit};

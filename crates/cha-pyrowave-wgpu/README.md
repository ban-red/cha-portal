# cha-pyrowave-wgpu

PyroWave decoding on [`wgpu`](https://wgpu.rs) for Cha Player (ADR 0013, C2.3). Packets go through dequant and the inverse wavelet transform into packed 8-bit YCbCr planes in a GPU buffer; `YuvRenderer` draws those planes into a render pass with no readback. Platform-neutral: it takes the caller's `wgpu::Device` and `Queue` and never makes its own.

It is the Rust twin of [`web/packages/pyrowave-webgpu`](../../web/packages/pyrowave-webgpu/README.md).

## Where the code comes from

- `src/shaders/*.wgsl` are vendored byte for byte from [imbcmdth/pyrowave](https://github.com/imbcmdth/pyrowave) branch `webgpu` at `5e80f92` (2026-09-26), via `web/packages/pyrowave-webgpu`. MIT, Copyright (c) 2025 Hans-Kristian Arntzen; see `LICENSE-PYROWAVE`.
- `layout.rs`, `parser.rs`, `decoder.rs`, `pipelines.rs` port that branch's C++ host (`pyrowave_webgpu_decoder.cpp`, `pyrowave_webgpu_common.cpp`, `metal/pyrowave_bitstream.cpp`, MIT), by way of our TypeScript port.
- `render.rs` / `render.wgsl` port `render.ts` from the web package (ours), with the picture placed in an aspect-fit viewport.
- Pinned bitstream: upstream PyroWave `89f7e47`. The bitstream has no version field.

## Naga

The WGSL files compile unchanged under naga 29 (wgpu 29.0.4). The host-side prefix differs from the browser's in one line: naga does not parse `enable subgroups;` ([gfx-rs/wgpu#5555](https://github.com/gfx-rs/wgpu/issues/5555)) but accepts the subgroup builtins and operations without it, so `pipelines.rs` omits the directive and keeps `diagnostic(off, subgroup_uniformity);`. Revisit when wgpu supports the directive.

## Requirements

`wgpu::Features::SUBGROUP` on the device (Metal on Apple silicon has it). `Pipelines::new` returns `Error::SubgroupsUnsupported` without it; there is no subgroup-free path. Default limits are enough (the tests use them). Add `Features::TIMESTAMP_QUERY` if you want to pass `DecodeTimestamps`.

## Use

```rust
let pipelines = Pipelines::new(&device, Precision::default())?;      // once per device
let head = parse_sequence_header(frame).unwrap();                     // size + chroma
let mut decoder = Decoder::new(&device, &queue, &pipelines, head.width, head.height, head.chroma)?;
let mut renderer = YuvRenderer::new(&device, surface_format);
renderer.set_source(&device, decoder.planes(), None);

// per frame, in the encoder that also holds the render pass:
if decoder.encode_frame(&mut encoder, frame, /* allow_partial */ true, None)? {
    renderer.set_color(decoder.color());
    // begin the render pass (clear to black), then:
    renderer.draw(&queue, &mut pass, (target_w, target_h));
}
queue.submit([encoder.finish()]);
```

`Decoder::encode` records into the caller's encoder rather than submitting, so the render pass lands in the same submission and wgpu inserts the barrier between compute writes and fragment reads. The uploads use `queue.write_buffer`, which lands before the next `submit`: submit the encoder before encoding another frame. A resize is a new `Decoder` (reuse `Pipelines`); call `set_source` again.

## Tests

`cargo test -p cha-pyrowave-wgpu -- --nocapture` (needs a GPU with subgroups; each test skips with a message otherwise). They read the clips in `spikes/s1b-pyrowave-webgpu/clips` (made by that spike's `tools/make-clips.sh`) and skip if absent. The PSNR test runs `ffmpeg` to regenerate the lavfi source and skips if it is not installed.

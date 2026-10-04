# @cha/pyrowave-webgpu

PyroWave decoding in the browser on WebGPU. Packets go through dequant and the inverse wavelet transform into packed 8-bit planes in a GPU buffer. `YuvRenderer` draws those planes directly, with no readback.

- `src/shaders/*.wgsl` are vendored unchanged from [imbcmdth/pyrowave](https://github.com/imbcmdth/pyrowave) branch `webgpu` at `5e80f92` (2026-09-26). They are MIT licensed; see `LICENSE-PYROWAVE`.
- The TypeScript host (`decoder.ts`, `device.ts`, `layout.ts`, `parser.ts`) is a port of that branch's `pyrowave_webgpu_decoder.cpp` / `pyrowave_webgpu_common.cpp` and `metal/pyrowave_bitstream.cpp` (MIT), adapted to decode into a buffer the renderer samples.
- **Requires the WebGPU `subgroups` feature.** That means Chrome/Edge 134+; Safari and Firefox lack it today. A subgroup-free dequant path is planned (docs/PLAN.md §3.3).
- Pinned bitstream: upstream PyroWave `89f7e47`. The bitstream has no version field yet.

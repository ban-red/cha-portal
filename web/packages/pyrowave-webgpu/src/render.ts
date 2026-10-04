// Draws decoded PyroWave planes straight from the decoder's storage buffer:
// YCbCr → RGB in a fragment shader, no copies or readback.

import type { PlaneLayout } from "./decoder";
import type { ColorInfo } from "./parser";

const SHADER = /* wgsl */ `
struct Params {
  video_size: vec2<u32>,
  canvas_size: vec2<f32>,
  chroma420: u32,
  full_range: u32,
  bt2020: u32,
  _pad: u32,
  offsets: vec4<u32>,
  strides: vec4<u32>,
};

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> planes: array<u32>;

@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
  let xy = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
  return vec4<f32>(xy * 2.0 - 1.0, 0.0, 1.0);
}

fn sample(plane: u32, x: u32, y: u32) -> f32 {
  let word = planes[p.offsets[plane] + y * p.strides[plane] + (x >> 2u)];
  return f32((word >> ((x & 3u) * 8u)) & 0xffu);
}

@fragment
fn fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
  let uv = pos.xy / p.canvas_size;
  let px = min(vec2<u32>(uv * vec2<f32>(p.video_size)), p.video_size - 1u);
  var c = px;
  if (p.chroma420 == 1u) { c = px / 2u; }
  var y = sample(0u, px.x, px.y);
  var cb = sample(1u, c.x, c.y) - 128.0;
  var cr = sample(2u, c.x, c.y) - 128.0;
  if (p.full_range == 1u) {
    y = y / 255.0; cb = cb / 255.0; cr = cr / 255.0;
  } else {
    y = (y - 16.0) / 219.0; cb = cb / 224.0; cr = cr / 224.0;
  }
  var rgb: vec3<f32>;
  if (p.bt2020 == 1u) {
    rgb = vec3<f32>(y + 1.4746 * cr, y - 0.16455 * cb - 0.57135 * cr, y + 1.8814 * cb);
  } else {
    rgb = vec3<f32>(y + 1.5748 * cr, y - 0.1873 * cb - 0.4681 * cr, y + 1.8556 * cb);
  }
  return vec4<f32>(clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}
`;

export class YuvRenderer {
  private readonly pipeline: GPURenderPipeline;
  private readonly params: GPUBuffer;
  private bindGroup: GPUBindGroup | null = null;
  private planes: PlaneLayout | null = null;
  private color: ColorInfo | null = null;

  constructor(
    private readonly device: GPUDevice,
    format: GPUTextureFormat,
  ) {
    const module = device.createShaderModule({ label: "pyrowave-yuv", code: SHADER });
    this.pipeline = device.createRenderPipeline({
      label: "pyrowave-yuv",
      layout: "auto",
      vertex: { module, entryPoint: "vs" },
      fragment: { module, entryPoint: "fs", targets: [{ format }] },
      primitive: { topology: "triangle-list" },
    });
    this.params = device.createBuffer({
      label: "pyrowave-yuv-params",
      size: 64,
      usage: GPUBufferUsage.UNIFORM | GPUBufferUsage.COPY_DST,
    });
  }

  setSource(planes: PlaneLayout, color: ColorInfo | null): void {
    this.planes = planes;
    this.color = color;
    this.bindGroup = this.device.createBindGroup({
      layout: this.pipeline.getBindGroupLayout(0),
      entries: [
        { binding: 0, resource: { buffer: this.params } },
        { binding: 1, resource: { buffer: planes.buffer } },
      ],
    });
  }

  /** Records a full-screen draw of the current planes into `view`. */
  draw(cmd: GPUCommandEncoder, view: GPUTextureView, canvasWidth: number, canvasHeight: number): void {
    if (!this.planes || !this.bindGroup) return;
    const p = this.planes;
    const data = new ArrayBuffer(64);
    const u32 = new Uint32Array(data);
    const f32 = new Float32Array(data);
    u32[0] = p.width;
    u32[1] = p.height;
    f32[2] = canvasWidth;
    f32[3] = canvasHeight;
    u32[4] = p.chroma === "420" ? 1 : 0;
    u32[5] = this.color?.fullRange ? 1 : 0;
    u32[6] = this.color?.ycbcrTransform === 1 ? 1 : 0;
    u32.set(p.offsets, 8);
    u32.set(p.strides, 12);
    this.device.queue.writeBuffer(this.params, 0, data);

    const pass = cmd.beginRenderPass({
      label: "pyrowave-yuv",
      colorAttachments: [{ view, loadOp: "clear", storeOp: "store", clearValue: [0, 0, 0, 1] }],
    });
    pass.setPipeline(this.pipeline);
    pass.setBindGroup(0, this.bindGroup);
    pass.draw(3);
    pass.end();
  }
}

// Draws a WebCodecs VideoFrame with WebGPU through an external texture, the
// zero-copy path the player will use for hardware-decoded H.264/HEVC/AV1.

const SHADER = /* wgsl */ `
struct Out { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> };

@vertex
fn vs(@builtin(vertex_index) i: u32) -> Out {
  let xy = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
  var o: Out;
  o.pos = vec4<f32>(xy * 2.0 - 1.0, 0.0, 1.0);
  o.uv = vec2<f32>(xy.x, 1.0 - xy.y);
  return o;
}

@group(0) @binding(0) var s: sampler;
@group(0) @binding(1) var t: texture_external;

@fragment
fn fs(in: Out) -> @location(0) vec4<f32> {
  return textureSampleBaseClampToEdge(t, s, in.uv);
}
`;

export class VideoFrameRenderer {
  private readonly pipeline: GPURenderPipeline;
  private readonly sampler: GPUSampler;

  constructor(
    private readonly device: GPUDevice,
    format: GPUTextureFormat,
  ) {
    const module = device.createShaderModule({ label: "video-frame", code: SHADER });
    this.pipeline = device.createRenderPipeline({
      label: "video-frame",
      layout: "auto",
      vertex: { module, entryPoint: "vs" },
      fragment: { module, entryPoint: "fs", targets: [{ format }] },
      primitive: { topology: "triangle-list" },
    });
    this.sampler = device.createSampler({ magFilter: "linear", minFilter: "linear" });
  }

  /** Records a full-screen draw of `frame`. The frame must stay open until the work is done. */
  draw(cmd: GPUCommandEncoder, view: GPUTextureView, frame: VideoFrame): void {
    const bindGroup = this.device.createBindGroup({
      layout: this.pipeline.getBindGroupLayout(0),
      entries: [
        { binding: 0, resource: this.sampler },
        { binding: 1, resource: this.device.importExternalTexture({ source: frame }) },
      ],
    });
    const pass = cmd.beginRenderPass({
      label: "video-frame",
      colorAttachments: [{ view, loadOp: "clear", storeOp: "store", clearValue: [0, 0, 0, 1] }],
    });
    pass.setPipeline(this.pipeline);
    pass.setBindGroup(0, bindGroup);
    pass.draw(3);
    pass.end();
  }
}

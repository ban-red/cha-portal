// Draws a picture into a canvas, aspect-fit, stretched by the GPU's bilinear filter or by SGSR 1
// (Snapdragon Game Super Resolution 1, shaders/sgsr1.wgsl), and times it: the browser's half of
// `crates/cha-upscale` (research 11, step 4), with the same shader byte for byte.
//
// The caller draws the picture at its own size into `source()` first (YuvRenderer does, for
// PyroWave), then `draw()` stretches it into the canvas in a pass of its own.

import sgsrWgsl from "./shaders/sgsr1.wgsl?raw";

/** What stretches the picture. */
export type Filter = "bilinear" | "sgsr";

/** What the stats panel calls each (`upscaler`). */
export const FILTER_NAMES: Record<Filter, string> = { bilinear: "bilinear", sgsr: "SGSR 1" };

/** SGSR's edge threshold as shipped for phones and desktops (4/255 is for VR), and its sharpness. */
export const EDGE_THRESHOLD = 8 / 255;
export const EDGE_SHARPNESS = 2;

export interface Rect {
  x: number;
  y: number;
  width: number;
  height: number;
}

/** The largest rectangle of `picture`'s aspect that fits in `target`, centred (cha-upscale's `aspect_fit`). */
export function aspectFit(pw: number, ph: number, tw: number, th: number): Rect {
  const scale = Math.min(tw / Math.max(pw, 1), th / Math.max(ph, 1));
  const width = Math.min(Math.round(Math.max(pw, 1) * scale), tw);
  const height = Math.min(Math.round(Math.max(ph, 1) * scale), th);
  return { x: Math.floor((tw - width) / 2), y: Math.floor((th - height) / 2), width, height };
}

/** Whether the picture drawn aspect-fit into the target comes out bigger by more than a pixel. */
export function enlarges(pw: number, ph: number, tw: number, th: number): boolean {
  const r = aspectFit(pw, ph, tw, th);
  return r.width > pw + 1 || r.height > ph + 1;
}

/**
 * Steps SGSR down to bilinear when it costs more than a quarter of the frame interval for a run of
 * frames, and tries it again later (cha-upscale's `Ladder`, with the same numbers).
 */
export class Ladder {
  static readonly STEP_DOWN_AFTER = 30;
  static readonly RETRY_AFTER = 600;
  private now: Filter;
  private over = 0;
  private sinceStep = 0;

  constructor(private readonly wanted: Filter) {
    this.now = wanted;
  }

  get filter(): Filter {
    return this.now;
  }

  /** One measured frame: the pass took `gpuMs`, frames are `frameMs` apart. Returns the next frame's filter. */
  record(gpuMs: number, frameMs: number): Filter {
    if (this.now === "sgsr") {
      if (gpuMs > frameMs / 4) {
        if (++this.over >= Ladder.STEP_DOWN_AFTER) {
          this.now = "bilinear";
          this.over = 0;
          this.sinceStep = 0;
        }
      } else this.over = 0;
    } else if (this.wanted === "sgsr" && ++this.sinceStep >= Ladder.RETRY_AFTER) {
      this.now = "sgsr";
      this.sinceStep = 0;
    }
    return this.now;
  }
}

/** Format of the source the picture is drawn into first: 8-bit, display-encoded, as SGSR expects. */
export const SOURCE_FORMAT: GPUTextureFormat = "rgba8unorm";

export class Upscaler {
  private readonly pipeline: GPURenderPipeline;
  private readonly sampler: GPUSampler;
  private readonly params: GPUBuffer;
  private readonly toLinear: boolean;
  private src: { width: number; height: number; texture: GPUTexture; bindGroup: GPUBindGroup } | null = null;
  private readonly timing: { query: GPUQuerySet; resolve: GPUBuffer; readback: GPUBuffer } | null;
  private busy = false;
  private armed = false;
  private latestMs: number | null = null;

  /** `targetFormat` is the canvas's; timed when `device` has `timestamp-query`. */
  constructor(
    private readonly device: GPUDevice,
    targetFormat: GPUTextureFormat,
  ) {
    const module = device.createShaderModule({ label: "upscale", code: sgsrWgsl });
    this.pipeline = device.createRenderPipeline({
      label: "upscale",
      layout: "auto",
      vertex: { module, entryPoint: "vs" },
      fragment: { module, entryPoint: "fs", targets: [{ format: targetFormat }] },
      primitive: { topology: "triangle-list" },
    });
    this.sampler = device.createSampler({ magFilter: "linear", minFilter: "linear", addressModeU: "clamp-to-edge", addressModeV: "clamp-to-edge" });
    this.params = device.createBuffer({ label: "upscale-params", size: 48, usage: GPUBufferUsage.UNIFORM | GPUBufferUsage.COPY_DST });
    this.toLinear = targetFormat.endsWith("-srgb");
    this.timing = device.features.has("timestamp-query")
      ? {
          query: device.createQuerySet({ type: "timestamp", count: 2 }),
          resolve: device.createBuffer({ size: 16, usage: GPUBufferUsage.QUERY_RESOLVE | GPUBufferUsage.COPY_SRC }),
          readback: device.createBuffer({ size: 16, usage: GPUBufferUsage.COPY_DST | GPUBufferUsage.MAP_READ }),
        }
      : null;
  }

  /** Where to draw the picture first, at its own size (`SOURCE_FORMAT`); made again when the size changes. */
  source(width: number, height: number): GPUTextureView {
    if (!this.src || this.src.width !== width || this.src.height !== height) {
      this.src?.texture.destroy();
      const texture = this.device.createTexture({
        label: "upscale-source",
        size: [width, height],
        format: SOURCE_FORMAT,
        usage: GPUTextureUsage.RENDER_ATTACHMENT | GPUTextureUsage.TEXTURE_BINDING | GPUTextureUsage.COPY_DST,
      });
      const bindGroup = this.device.createBindGroup({
        layout: this.pipeline.getBindGroupLayout(0),
        entries: [
          { binding: 0, resource: { buffer: this.params } },
          { binding: 1, resource: texture.createView() },
          { binding: 2, resource: this.sampler },
        ],
      });
      this.src = { width, height, texture, bindGroup };
    }
    return this.src.texture.createView();
  }

  /** The source's texture once `source()` made it: for a picture already RGB, to copy in instead of drawing. */
  get sourceTexture(): GPUTexture | null {
    return this.src?.texture ?? null;
  }

  /**
   * Records a pass that clears `view` (`width`×`height`) to black and draws the source into it
   * aspect-fit with `filter`, timed when it can be. Null before `source()`.
   */
  draw(cmd: GPUCommandEncoder, view: GPUTextureView, width: number, height: number, filter: Filter): Rect | null {
    const src = this.src;
    if (!src) return null;
    const rect = aspectFit(src.width, src.height, width, height);
    const p = new Float32Array([
      1 / src.width, 1 / src.height, src.width, src.height,
      rect.x, rect.y, rect.width, rect.height,
      filter === "sgsr" ? 1 : 0, this.toLinear ? 1 : 0, EDGE_SHARPNESS, EDGE_THRESHOLD,
    ]);
    this.device.queue.writeBuffer(this.params, 0, p);
    const timed = this.timing && !this.busy ? this.timing : null;
    const pass = cmd.beginRenderPass({
      label: "upscale",
      colorAttachments: [{ view, loadOp: "clear", storeOp: "store", clearValue: [0, 0, 0, 1] }],
      ...(timed && { timestampWrites: { querySet: timed.query, beginningOfPassWriteIndex: 0, endOfPassWriteIndex: 1 } }),
    });
    if (rect.width >= 1 && rect.height >= 1) {
      pass.setViewport(rect.x, rect.y, rect.width, rect.height, 0, 1);
      pass.setPipeline(this.pipeline);
      pass.setBindGroup(0, src.bindGroup);
      pass.draw(3);
    }
    pass.end();
    if (timed) {
      cmd.resolveQuerySet(timed.query, 0, 2, timed.resolve, 0);
      cmd.copyBufferToBuffer(timed.resolve, 0, timed.readback, 0, 16);
      this.busy = true;
      this.armed = true;
    }
    return rect;
  }

  /** Call after submitting the encoder `draw` recorded into: reads the timestamps back. */
  afterSubmit(): void {
    const t = this.timing;
    if (!t || !this.armed) return;
    this.armed = false;
    t.readback.mapAsync(GPUMapMode.READ).then(
      () => {
        const [begin, end] = new BigUint64Array(t.readback.getMappedRange());
        t.readback.unmap();
        if (begin !== undefined && end !== undefined && end > begin) this.latestMs = Number(end - begin) / 1e6;
        this.busy = false;
      },
      () => {
        this.busy = false;
      },
    );
  }

  /** The pass's GPU time in ms from the newest reading not yet taken; null while none has landed. */
  takeGpuMs(): number | null {
    const ms = this.latestMs;
    this.latestMs = null;
    return ms;
  }

  destroy(): void {
    this.src?.texture.destroy();
    this.src = null;
    this.params.destroy();
    this.timing?.query.destroy();
    this.timing?.resolve.destroy();
    this.timing?.readback.destroy();
  }
}

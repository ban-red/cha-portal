// PyroWave decoder on WebGPU: packets → dequant → iDWT → packed 8-bit planes in a
// GPU buffer, which a renderer samples directly (no readback). Port of
// pyrowave_webgpu_decoder.cpp (imbcmdth/pyrowave@webgpu, MIT).

import type { PyroWaveDevice } from "./device";
import {
  BlockLayout,
  type Chroma,
  DecompositionLevels,
  NumComponents,
  NumFrequencyBandsPerLevel,
  WaveletFP16Levels,
} from "./layout";
import { BitstreamParser } from "./parser";

const DEQUANT_REGISTER_BYTES = 32;
const IDWT_REGISTER_BYTES = 48;
const STORAGE_OFFSET_ALIGNMENT = 256;
const MAX_WORKGROUPS_X = 32768;

interface Dispatch {
  pipeline: GPUComputePipeline;
  bindGroup: GPUBindGroup;
  x: number;
  y: number;
  z: number;
}

/** Where the decoded planes live; offsets and strides are in u32 words (4 samples). */
export interface PlaneLayout {
  buffer: GPUBuffer;
  width: number;
  height: number;
  chroma: Chroma;
  offsets: [number, number, number];
  strides: [number, number, number];
}

/** Optional GPU timestamps: the dequant pass writes `first`/`first+1`, iDWT `first+2`/`first+3`. */
export interface DecodeTimestamps {
  querySet: GPUQuerySet;
  first: number;
}

const alignUp = (v: number, a: number) => Math.ceil(v / a) * a;

export class PyroWaveDecoder {
  readonly layout: BlockLayout;
  readonly parser: BitstreamParser;
  readonly planes: PlaneLayout;

  private readonly device: GPUDevice;
  private readonly pyramid: GPUTexture;
  private readonly levelViews: GPUTextureView[];
  private readonly offsetsBuffer: GPUBuffer;
  private payloadBuffer: GPUBuffer;
  private payloadCapacity: number;
  private readonly dequantTables: { buffer: GPUBuffer; registerBytes: number; rangesOffset: number; rangesBytes: number }[] = [];
  private readonly dequantDispatches: Dispatch[] = [];
  private readonly idwtDispatches: Dispatch[] = [];
  private readonly tableBuffers: GPUBuffer[] = [];

  constructor(
    private readonly pw: PyroWaveDevice,
    width: number,
    height: number,
    chroma: Chroma,
  ) {
    this.device = pw.device;
    this.layout = new BlockLayout(width, height, chroma);
    this.parser = new BitstreamParser(this.layout);
    const layout = this.layout;
    const device = this.device;

    this.pyramid = device.createTexture({
      label: "pyrowave-pyramid",
      size: [layout.alignedWidth / 2, layout.alignedHeight / 2, NumFrequencyBandsPerLevel * NumComponents],
      format: "r32float",
      mipLevelCount: DecompositionLevels,
      usage: GPUTextureUsage.TEXTURE_BINDING | GPUTextureUsage.STORAGE_BINDING | GPUTextureUsage.COPY_SRC,
    });
    this.levelViews = Array.from({ length: DecompositionLevels }, (_, level) =>
      this.pyramid.createView({
        dimension: "2d-array",
        baseMipLevel: level,
        mipLevelCount: 1,
        baseArrayLayer: 0,
        arrayLayerCount: NumFrequencyBandsPerLevel * NumComponents,
      }),
    );

    this.offsetsBuffer = device.createBuffer({
      label: "pyrowave-dequant-offsets",
      size: alignUp(layout.blockCount32x32 * 4, 4),
      usage: GPUBufferUsage.STORAGE | GPUBufferUsage.COPY_DST,
    });
    this.payloadCapacity = 2 * 1024 * 1024;
    this.payloadBuffer = this.createPayloadBuffer(this.payloadCapacity);

    const offsets: [number, number, number] = [0, 0, 0];
    const strides: [number, number, number] = [0, 0, 0];
    let words = 0;
    for (let plane = 0; plane < NumComponents; plane++) {
      const stride = layout.alignedPlaneWidth(plane) / 4;
      offsets[plane] = words;
      strides[plane] = stride;
      words += stride * layout.alignedPlaneHeight(plane);
    }
    this.planes = {
      buffer: device.createBuffer({
        label: "pyrowave-planes",
        size: words * 4,
        usage: GPUBufferUsage.STORAGE | GPUBufferUsage.COPY_SRC,
      }),
      width,
      height,
      chroma,
      offsets,
      strides,
    };

    this.planDequant();
    this.rebuildDequantGroups();
    this.planIdwt();
  }

  /** Feeds one packet; returns false if it was corrupt. */
  pushPacket(data: Uint8Array): boolean {
    return this.parser.pushPacket(data);
  }

  isReady(allowPartialFrame = false): boolean {
    return this.parser.isReady(allowPartialFrame);
  }

  /** Discards any partially received frame. */
  clear(): void {
    this.parser.clear();
  }

  /**
   * Uploads the parsed frame and records dequant + iDWT into `cmd`. The uploads go
   * through the queue, so submit `cmd` before encoding the next frame.
   */
  encode(cmd: GPUCommandEncoder, timestamps?: DecodeTimestamps): void {
    const payload = this.parser.payloadWords();
    // The dequant shader can read slightly past the end of the payload.
    const required = payload.byteLength + 16;
    if (required > this.payloadCapacity) {
      this.payloadBuffer.destroy();
      this.payloadCapacity = alignUp(required * 2, 4);
      this.payloadBuffer = this.createPayloadBuffer(this.payloadCapacity);
      this.rebuildDequantGroups();
    }

    const queue = this.device.queue;
    queue.writeBuffer(this.offsetsBuffer, 0, this.parser.dequantOffsets);
    if (payload.length) queue.writeBuffer(this.payloadBuffer, 0, payload);

    this.recordPass(cmd, "pyrowave-dequant", this.dequantDispatches, timestamps, 0);
    this.recordPass(cmd, "pyrowave-idwt", this.idwtDispatches, timestamps, 2);
    this.parser.markFrameDecoded();
  }

  destroy(): void {
    this.pyramid.destroy();
    this.offsetsBuffer.destroy();
    this.payloadBuffer.destroy();
    this.planes.buffer.destroy();
    for (const t of this.dequantTables) t.buffer.destroy();
    for (const b of this.tableBuffers) b.destroy();
  }

  private recordPass(
    cmd: GPUCommandEncoder,
    label: string,
    dispatches: Dispatch[],
    timestamps: DecodeTimestamps | undefined,
    offset: number,
  ): void {
    const pass = cmd.beginComputePass({
      label,
      timestampWrites: timestamps
        ? {
            querySet: timestamps.querySet,
            beginningOfPassWriteIndex: timestamps.first + offset,
            endOfPassWriteIndex: timestamps.first + offset + 1,
          }
        : undefined,
    });
    let current: GPUComputePipeline | null = null;
    for (const d of dispatches) {
      if (d.pipeline !== current) {
        pass.setPipeline(d.pipeline);
        current = d.pipeline;
      }
      pass.setBindGroup(0, d.bindGroup);
      pass.dispatchWorkgroups(d.x, d.y, d.z);
    }
    pass.end();
  }

  private createPayloadBuffer(size: number): GPUBuffer {
    return this.device.createBuffer({
      label: "pyrowave-payload",
      size,
      usage: GPUBufferUsage.STORAGE | GPUBufferUsage.COPY_DST,
    });
  }

  /** Every band of a level in one dispatch (band_dispatch.wgsl); levels stay separate. */
  private planDequant(): void {
    const layout = this.layout;
    for (let level = 0; level < DecompositionLevels; level++) {
      const registers: ArrayBuffer[] = [];
      const ranges: number[] = [];
      let totalWorkgroups = 0;
      for (let component = 0; component < NumComponents; component++) {
        if (level === 0 && component !== 0 && layout.chroma === "420") continue;
        for (let band = level === DecompositionLevels - 1 ? 0 : 1; band < 4; band++) {
          const meta = layout.blockMeta[component]![level]![band]!;
          const w = layout.levelWidth(level);
          const h = layout.levelHeight(level);
          const regs = new DataView(new ArrayBuffer(DEQUANT_REGISTER_BYTES));
          regs.setInt32(0, w, true);
          regs.setInt32(4, h, true);
          regs.setInt32(8, NumFrequencyBandsPerLevel * component + band, true);
          regs.setInt32(12, meta.blockOffset32x32, true);
          regs.setInt32(16, meta.blockStride32x32, true);
          regs.setUint32(20, this.pw.precision === 1 && level < WaveletFP16Levels ? 1 : 0, true);
          registers.push(regs.buffer);
          const wx = Math.ceil(w / 32);
          const count = wx * Math.ceil(h / 32);
          ranges.push(totalWorkgroups, wx, count, 0);
          totalWorkgroups += count;
        }
      }

      const registerBytes = registers.length * DEQUANT_REGISTER_BYTES;
      const rangesOffset = alignUp(registerBytes, STORAGE_OFFSET_ALIGNMENT);
      const rangesBytes = ranges.length * 4;
      const buffer = this.device.createBuffer({
        label: `pyrowave-dequant-bands-${level}`,
        size: rangesOffset + rangesBytes,
        usage: GPUBufferUsage.STORAGE | GPUBufferUsage.COPY_DST,
      });
      const packed = new Uint8Array(registerBytes);
      registers.forEach((r, i) => packed.set(new Uint8Array(r), i * DEQUANT_REGISTER_BYTES));
      this.device.queue.writeBuffer(buffer, 0, packed);
      this.device.queue.writeBuffer(buffer, rangesOffset, new Uint32Array(ranges));
      this.dequantTables.push({ buffer, registerBytes, rangesOffset, rangesBytes });

      // The shader numbers workgroups x + y * num_workgroups.x, so large tables spill into y.
      const x = Math.min(totalWorkgroups, MAX_WORKGROUPS_X);
      this.dequantDispatches.push({
        pipeline: this.pw.dequant,
        bindGroup: null as unknown as GPUBindGroup, // filled by rebuildDequantGroups()
        x,
        y: Math.ceil(totalWorkgroups / x),
        z: 1,
      });
    }
  }

  /** Dequant bind groups reference the payload buffer, which grows with frame size. */
  private rebuildDequantGroups(): void {
    const layout = this.pw.dequant.getBindGroupLayout(0);
    this.dequantTables.forEach((table, level) => {
      this.dequantDispatches[level]!.bindGroup = this.device.createBindGroup({
        label: `pyrowave-dequant-${level}`,
        layout,
        entries: [
          { binding: 0, resource: { buffer: table.buffer, offset: 0, size: table.registerBytes } },
          { binding: 1, resource: this.levelViews[level]! },
          { binding: 2, resource: { buffer: this.offsetsBuffer } },
          { binding: 3, resource: { buffer: this.payloadBuffer } },
          { binding: 7, resource: { buffer: table.buffer, offset: table.rangesOffset, size: table.rangesBytes } },
        ],
      });
    });
  }

  /**
   * Per level, components writing the next LL band share one dispatch and those
   * writing an output plane share another, one component per workgroup_id.z.
   */
  private planIdwt(): void {
    const layout = this.layout;
    const is420 = layout.chroma === "420";
    interface Spec {
      inputLevel: number;
      finalOutput: boolean;
      regs: ArrayBuffer[];
      resolution: [number, number];
      offset: number;
    }
    const specs: Spec[] = [];

    for (let inputLevel = DecompositionLevels - 1; inputLevel >= 0; inputLevel--) {
      // Transposed.
      const resolution: [number, number] = [layout.levelHeight(inputLevel), layout.levelWidth(inputLevel)];
      const toPyramid: Spec = { inputLevel, finalOutput: false, regs: [], resolution, offset: 0 };
      const toPlanes: Spec = { inputLevel, finalOutput: true, regs: [], resolution, offset: 0 };

      for (let c = 0; c < NumComponents; c++) {
        if (inputLevel === 0 && is420 && c !== 0) continue;
        const regs = new DataView(new ArrayBuffer(IDWT_REGISTER_BYTES));
        regs.setInt32(0, resolution[0], true);
        regs.setInt32(4, resolution[1], true);
        regs.setFloat32(8, 1 / resolution[0], true);
        regs.setFloat32(12, 1 / resolution[1], true);
        regs.setInt32(32, NumFrequencyBandsPerLevel * c, true); // input_layer

        if (inputLevel === 0 || (is420 && c !== 0 && inputLevel === 1)) {
          regs.setUint32(20, this.planes.offsets[c]!, true);
          regs.setUint32(24, this.planes.strides[c]!, true);
          regs.setUint32(28, layout.alignedPlaneHeight(c), true);
          toPlanes.regs.push(regs.buffer);
        } else {
          regs.setUint32(16, this.pw.precision === 1 && inputLevel - 1 < WaveletFP16Levels ? 1 : 0, true);
          regs.setInt32(36, NumFrequencyBandsPerLevel * c, true); // output_layer
          toPyramid.regs.push(regs.buffer);
        }
      }
      if (toPyramid.regs.length) specs.push(toPyramid);
      if (toPlanes.regs.length) specs.push(toPlanes);
    }

    let size = 0;
    for (const spec of specs) {
      spec.offset = alignUp(size, STORAGE_OFFSET_ALIGNMENT);
      size = spec.offset + spec.regs.length * IDWT_REGISTER_BYTES;
    }
    const table = new Uint8Array(alignUp(size, 4));
    for (const spec of specs) {
      spec.regs.forEach((r, i) => table.set(new Uint8Array(r), spec.offset + i * IDWT_REGISTER_BYTES));
    }
    const buffer = this.device.createBuffer({
      label: "pyrowave-idwt-registers",
      size: table.byteLength,
      usage: GPUBufferUsage.STORAGE | GPUBufferUsage.COPY_DST,
    });
    this.device.queue.writeBuffer(buffer, 0, table);
    this.tableBuffers.push(buffer);

    for (const spec of specs) {
      const registers = { buffer, offset: spec.offset, size: spec.regs.length * IDWT_REGISTER_BYTES };
      const pipeline = spec.finalOutput ? this.pw.idwtFinal : this.pw.idwt;
      const bindGroup = this.device.createBindGroup({
        label: `pyrowave-idwt-${spec.inputLevel}${spec.finalOutput ? "-final" : ""}`,
        layout: pipeline.getBindGroupLayout(0),
        entries: spec.finalOutput
          ? [
              { binding: 0, resource: registers },
              { binding: 1, resource: this.levelViews[spec.inputLevel]! },
              { binding: 2, resource: this.pw.mirrorRepeatSampler },
              { binding: 4, resource: { buffer: this.planes.buffer } },
            ]
          : [
              { binding: 0, resource: registers },
              { binding: 1, resource: this.levelViews[spec.inputLevel]! },
              { binding: 2, resource: this.pw.mirrorRepeatSampler },
              { binding: 3, resource: this.levelViews[spec.inputLevel - 1]! },
            ],
      });
      this.idwtDispatches.push({
        pipeline,
        bindGroup,
        x: Math.ceil(spec.resolution[0] / 16),
        y: Math.ceil(spec.resolution[1] / 16),
        z: spec.regs.length,
      });
    }
  }
}

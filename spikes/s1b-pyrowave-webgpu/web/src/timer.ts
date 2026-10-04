// GPU timestamps for the decoder's two passes, read back without stalling the
// pipeline: each frame resolves into its own mappable buffer from a pool.

export interface StageTimes {
  dequantMs: number;
  idwtMs: number;
  /** From the start of dequant to the end of iDWT. */
  totalMs: number;
}

export class GpuTimer {
  readonly querySet: GPUQuerySet;
  private readonly resolveBuffer: GPUBuffer;
  private readonly free: GPUBuffer[] = [];

  constructor(private readonly device: GPUDevice) {
    this.querySet = device.createQuerySet({ type: "timestamp", count: 4 });
    this.resolveBuffer = device.createBuffer({
      size: 32,
      usage: GPUBufferUsage.QUERY_RESOLVE | GPUBufferUsage.COPY_SRC,
    });
  }

  /** Records the resolve; call `read()` with the result after submitting `cmd`. */
  resolve(cmd: GPUCommandEncoder): GPUBuffer {
    const target =
      this.free.pop() ??
      this.device.createBuffer({ size: 32, usage: GPUBufferUsage.MAP_READ | GPUBufferUsage.COPY_DST });
    cmd.resolveQuerySet(this.querySet, 0, 4, this.resolveBuffer, 0);
    cmd.copyBufferToBuffer(this.resolveBuffer, 0, target, 0, 32);
    return target;
  }

  async read(target: GPUBuffer): Promise<StageTimes> {
    await target.mapAsync(GPUMapMode.READ);
    const t = new BigUint64Array(target.getMappedRange().slice(0));
    target.unmap();
    this.free.push(target);
    const ms = (a: number, b: number) => Number(t[b]! - t[a]!) / 1e6;
    return { dequantMs: ms(0, 1), idwtMs: ms(2, 3), totalMs: ms(0, 3) };
  }
}

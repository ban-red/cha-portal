// PyroWave on WebGPU for the player: a frame's packets go through
// `@cha/pyrowave-webgpu` (dequant and inverse wavelet transform on the GPU),
// get drawn into an offscreen canvas, and come out as a VideoFrame for the
// same track generator and <video> as the hardware codecs, so presentation,
// the probe and the stats work alike.

import { PyroWaveDecoder, PyroWaveDevice, YuvRenderer, type Chroma } from "@cha/pyrowave-webgpu";

/** Whether this browser can decode PyroWave (WebGPU with subgroups). */
export async function supportsPyroWave(): Promise<boolean> {
  try {
    const adapter = await navigator.gpu?.requestAdapter({ powerPreference: "high-performance" });
    return !!adapter && PyroWaveDevice.supports(adapter).ok;
  } catch {
    return false;
  }
}

export class PyroPresenter {
  private decoder: PyroWaveDecoder | null = null;
  private readonly canvas = new OffscreenCanvas(16, 16);
  private readonly ctx: GPUCanvasContext;
  private readonly renderer: YuvRenderer;
  private colorSet = false;
  /** Called once if the GPU device is lost (a driver reset, a GPU switch), with why. Not for our own `destroy()`. */
  onLost?: (reason: string) => void;
  private lostReason: string | null = null;

  private constructor(private readonly pw: PyroWaveDevice) {
    void pw.device.lost.then((info) => {
      if (info.reason === "destroyed") return;
      this.lostReason = info.message || "the WebGPU device was lost";
      this.onLost?.(this.lostReason);
    });
    const ctx = this.canvas.getContext("webgpu");
    if (!ctx) throw new Error("no WebGPU canvas");
    this.ctx = ctx;
    const format = navigator.gpu.getPreferredCanvasFormat();
    ctx.configure({ device: pw.device, format, alphaMode: "opaque" });
    this.renderer = new YuvRenderer(pw.device, format);
  }

  static async create(): Promise<PyroPresenter> {
    const adapter = await navigator.gpu?.requestAdapter({ powerPreference: "high-performance" });
    if (!adapter) throw new Error("this browser has no WebGPU adapter");
    const check = PyroWaveDevice.supports(adapter);
    if (!check.ok) throw new Error(`PyroWave can't decode here: ${check.reason}`);
    return new PyroPresenter(await PyroWaveDevice.create(adapter, { timestamps: false }));
  }

  /**
   * Decodes one frame's packets (all of them, or what arrived of them when
   * `partial`), as a VideoFrame stamped `id`; null if nothing decodable.
   */
  decode(data: Uint8Array, id: number, partial: boolean): VideoFrame | null {
    if (this.lostReason) throw new Error(this.lostReason);
    const header = sequenceHeader(data);
    if (!header) return null;
    const { width, height, chroma } = header;
    if (!this.decoder || this.decoder.planes.width !== width || this.decoder.planes.height !== height || this.decoder.planes.chroma !== chroma) {
      this.decoder?.destroy();
      this.decoder = new PyroWaveDecoder(this.pw, width, height, chroma);
      this.canvas.width = width;
      this.canvas.height = height;
      this.colorSet = false;
    }
    const decoder = this.decoder;
    decoder.clear();
    if (!decoder.pushPacket(data) || !decoder.isReady(partial)) return null;
    if (!this.colorSet) {
      this.renderer.setSource(decoder.planes, decoder.parser.color);
      this.colorSet = true;
    }
    const device = this.pw.device;
    const cmd = device.createCommandEncoder();
    decoder.encode(cmd);
    this.renderer.draw(cmd, this.ctx.getCurrentTexture().createView(), width, height);
    device.queue.submit([cmd.finish()]);
    const bitmap = this.canvas.transferToImageBitmap();
    const frame = new VideoFrame(bitmap, { timestamp: id });
    bitmap.close();
    return frame;
  }

  destroy(): void {
    this.decoder?.destroy();
    this.decoder = null;
    this.pw.device.destroy();
  }
}

/** The frame's size and chroma, from the first packet's sequence header. */
function sequenceHeader(data: Uint8Array): { width: number; height: number; chroma: Chroma } | null {
  if (data.byteLength < 8) return null;
  const v = new DataView(data.buffer, data.byteOffset, 8);
  const w0 = v.getUint32(0, true);
  const w1 = v.getUint32(4, true);
  if (!(w0 >>> 31)) return null; // not an extended (start of frame) header
  return {
    width: (w0 & 0x3fff) + 1,
    height: ((w0 >>> 14) & 0x3fff) + 1,
    chroma: (w1 >>> 26) & 1 ? "444" : "420",
  };
}

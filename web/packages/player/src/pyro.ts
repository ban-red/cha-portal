// PyroWave on WebGPU for the player: a frame's packets go through
// `@cha/pyrowave-webgpu` (dequant and inverse wavelet transform on the GPU),
// get drawn into an offscreen canvas, and come out as a VideoFrame for the
// same track generator and <video> as the hardware codecs, so presentation,
// the probe and the stats work alike.
//
// With an upscaler (research 11, step 4) a picture smaller than the element is
// drawn at its own size first, then stretched by SGSR 1 into a canvas of the
// size it is shown at, so the <video> shows the frame pixel for pixel.

import { pyroColour, type PictureColour } from "./colour";
import {
  FILTER_NAMES,
  Ladder,
  PyroWaveDecoder,
  PyroWaveDevice,
  SOURCE_FORMAT,
  Upscaler,
  YuvRenderer,
  aspectFit,
  enlarges,
  type Chroma,
  type Filter,
} from "@cha/pyrowave-webgpu";

/** How the presenter drew its pictures since the last `takeScaling()`, for the stats. */
export interface PyroScaling {
  /** The decoded picture's size. */
  width: number;
  height: number;
  /** The size it was drawn at when the upscaler stretched it (the VideoFrame's), else null. */
  shownWidth: number | null;
  shownHeight: number | null;
  /** The filter of the last frame, when stretched. */
  upscaler: string | null;
  /** The upscale pass's mean GPU time over the frames measured, ms. */
  upscaleMs: number | null;
}

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
  /** The upscaler, when asked for: its own renderer into the source, and the ladder that may step it down. */
  private readonly upscale: { upscaler: Upscaler; renderer: YuvRenderer; ladder: Ladder } | null;
  /** The element's size in device pixels: an upscaled frame is drawn to fit it. Null: never upscale. */
  display: { width: number; height: number } | null = null;
  /** Frames are this far apart (the stream's rate), ms: the upscaler may take a quarter of it. */
  frameMs = 1000 / 60;
  private scaling: PyroScaling | null = null;
  private upscaleTimes: number[] = [];

  private constructor(
    private readonly pw: PyroWaveDevice,
    filter: Filter | null,
  ) {
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
    this.upscale = filter
      ? { upscaler: new Upscaler(pw.device, format), renderer: new YuvRenderer(pw.device, SOURCE_FORMAT), ladder: new Ladder(filter) }
      : null;
  }

  /** `upscale`: stretch pictures smaller than `display` with that filter (timed, when the GPU can). */
  static async create(options: { upscale?: Filter | null } = {}): Promise<PyroPresenter> {
    const adapter = await navigator.gpu?.requestAdapter({ powerPreference: "high-performance" });
    if (!adapter) throw new Error("this browser has no WebGPU adapter");
    const check = PyroWaveDevice.supports(adapter);
    if (!check.ok) throw new Error(`PyroWave can't decode here: ${check.reason}`);
    const upscale = options.upscale ?? null;
    // Timestamps time the upscaler only; the decoder is timed only when asked.
    return new PyroPresenter(await PyroWaveDevice.create(adapter, { timestamps: !!upscale }), upscale);
  }

  /** The colour the stream's sequence header says its pictures are in; null before a frame. */
  get colour(): PictureColour | null {
    return this.colorSet ? pyroColour(this.decoder?.parser.color) : null;
  }

  /** How pictures were drawn since the last call: the size, the stretch and its GPU time. Null before a frame. */
  takeScaling(): PyroScaling | null {
    const s = this.scaling;
    if (!s) return null;
    const times = this.upscaleTimes;
    this.upscaleTimes = [];
    return { ...s, upscaleMs: times.length ? times.reduce((a, b) => a + b, 0) / times.length : null };
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
      this.colorSet = false;
    }
    const decoder = this.decoder;
    decoder.clear();
    if (!decoder.pushPacket(data) || !decoder.isReady(partial)) return null;
    if (!this.colorSet) {
      this.renderer.setSource(decoder.planes, decoder.parser.color);
      this.upscale?.renderer.setSource(decoder.planes, decoder.parser.color);
      this.colorSet = true;
      const c = decoder.parser.color;
      // What the stream says its colours are, to check against what the picture looks like.
      console.info(
        `[pyrowave] ${width}x${height} ${chroma}: ` +
          (c
            ? `${c.primaries ? "BT.2020" : "BT.709"} primaries, ${c.transfer ? "PQ" : "BT.709"} transfer, ` +
              `${c.ycbcrTransform ? "BT.2020" : "BT.709"} matrix, ${c.fullRange ? "full" : "limited"} range`
            : "no colour info in the stream (decoded as BT.709 limited)"),
      );
    }
    const device = this.pw.device;
    const cmd = device.createCommandEncoder();
    decoder.encode(cmd);
    const up = this.upscale;
    const d = this.display;
    const shown = up && d && enlarges(width, height, d.width, d.height) ? aspectFit(width, height, d.width, d.height) : null;
    const [cw, ch] = shown ? [shown.width, shown.height] : [width, height];
    if (this.canvas.width !== cw || this.canvas.height !== ch) [this.canvas.width, this.canvas.height] = [cw, ch];
    const target = this.ctx.getCurrentTexture().createView();
    if (up && shown) {
      up.renderer.draw(cmd, up.upscaler.source(width, height), width, height);
      up.upscaler.draw(cmd, target, cw, ch, up.ladder.filter);
    } else this.renderer.draw(cmd, target, width, height);
    device.queue.submit([cmd.finish()]);
    if (up && shown) {
      up.upscaler.afterSubmit();
      const ms = up.upscaler.takeGpuMs();
      if (ms !== null) {
        this.upscaleTimes.push(ms);
        up.ladder.record(ms, this.frameMs);
      }
    }
    this.scaling = {
      width,
      height,
      shownWidth: shown ? cw : null,
      shownHeight: shown ? ch : null,
      upscaler: up && shown ? FILTER_NAMES[up.ladder.filter] : null,
      upscaleMs: null,
    };
    const bitmap = this.canvas.transferToImageBitmap();
    const frame = new VideoFrame(bitmap, { timestamp: id });
    bitmap.close();
    return frame;
  }

  destroy(): void {
    this.decoder?.destroy();
    this.decoder = null;
    this.upscale?.upscaler.destroy();
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

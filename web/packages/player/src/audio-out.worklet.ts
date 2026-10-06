// The Web Audio output's render side (audio-out.ts loads this as a module in the AudioWorklet
// scope): packets in over the port, 128-frame quanta out, a report back every ~250 ms. The queue
// logic is audio-fifo.ts, unit-tested; this only wires it to the port and the render callback.

import { SoundFifo, type FifoOptions } from "./audio-fifo";

/** Messages to the worklet. */
export type ToSoundWorklet = { type: "push"; left: Float32Array; right: Float32Array };
/** What it reports: totals since it started, and the peak output since the last report. */
export interface SoundReport {
  type: "report";
  played: number;
  pushed: number;
  peak: number;
  underruns: number;
  drops: number;
  droppedFrames: number;
  queued: number;
}

// The worklet scope's globals, which lib.dom doesn't declare.
declare class AudioWorkletProcessor {
  readonly port: MessagePort;
  constructor(options?: unknown);
}
declare function registerProcessor(
  name: string,
  ctor: new (options?: { processorOptions?: FifoOptions }) => AudioWorkletProcessor,
): void;
declare const sampleRate: number;

const REPORT_EVERY_S = 0.25;

class SoundProcessor extends AudioWorkletProcessor {
  private readonly fifo: SoundFifo;
  private sinceReport = 0;

  constructor(options?: { processorOptions?: FifoOptions }) {
    super();
    this.fifo = new SoundFifo(options?.processorOptions);
    this.port.onmessage = (e: MessageEvent<ToSoundWorklet>) => {
      if (e.data.type === "push") this.fifo.push(e.data.left, e.data.right);
    };
  }

  process(_inputs: Float32Array[][], outputs: Float32Array[][]): boolean {
    const out = outputs[0];
    if (!out || !out[0]) return true;
    this.fifo.pull(out[0], out[1]);
    // A mono device would only have channel 0; anything beyond stereo stays silent (zeroed).
    this.sinceReport += out[0].length / sampleRate;
    if (this.sinceReport >= REPORT_EVERY_S) {
      this.sinceReport = 0;
      const c = this.fifo.counters();
      this.port.postMessage({ type: "report", ...c, peak: this.fifo.takePeak() } satisfies SoundReport);
    }
    return true;
  }
}

registerProcessor("cha-sound", SoundProcessor);

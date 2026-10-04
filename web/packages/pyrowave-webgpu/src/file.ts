// The container PyroWave's encode/decode tools use:
//   "PYROWAVE", int32 params[8], then per frame a u32 size and one packet.
// params: width, height, y4m format, chroma (0 = 4:2:0, 1 = 4:4:4), full range,
//         frame rate numerator, frame rate denominator, reserved.

import type { Chroma } from "./layout";

export interface PyroWaveFile {
  width: number;
  height: number;
  chroma: Chroma;
  fullRange: boolean;
  fps: number;
  /** One packet per frame, as views into the original buffer. */
  frames: Uint8Array[];
}

export function parsePyroWaveFile(buffer: ArrayBuffer): PyroWaveFile {
  const magic = new TextDecoder().decode(new Uint8Array(buffer, 0, 8));
  if (magic !== "PYROWAVE") throw new Error("not a PyroWave file");
  const view = new DataView(buffer);
  const param = (i: number) => view.getInt32(8 + i * 4, true);
  const frames: Uint8Array[] = [];
  let offset = 8 + 32;
  while (offset + 4 <= buffer.byteLength) {
    const size = view.getUint32(offset, true);
    offset += 4;
    if (offset + size > buffer.byteLength) break;
    frames.push(new Uint8Array(buffer, offset, size));
    offset += size;
  }
  const den = param(6) || 1;
  return {
    width: param(0),
    height: param(1),
    chroma: param(3) === 1 ? "444" : "420",
    fullRange: param(4) !== 0,
    fps: param(5) / den,
    frames,
  };
}

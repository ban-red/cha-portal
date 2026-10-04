// Port of PyroWave's BitstreamParser (metal/pyrowave_bitstream.cpp, MIT). Turns
// packets into the two flat arrays the dequant shader reads: a per-32x32-block
// offset table and the concatenated payload words.
//
// Wire layout (little-endian bitfields, 8 bytes each):
//   block header:    w0 = ballot:16 | payload_words:12 | sequence:3 | extended:1
//                    w1 = quant_code:8 | block_index:24
//   sequence header: w0 = width-1:14 | height-1:14 | sequence:3 | extended:1
//                    w1 = total_blocks:24 | code:2 | chroma_resolution:1 | primaries:1 |
//                         transfer:1 | ycbcr_transform:1 | range:1 | chroma_siting:1

import { DecompositionLevels, type BlockLayout } from "./layout";

const NOT_RECEIVED = 0xffffffff;
const HEADER_BYTES = 8;
const SEQUENCE_MASK = 0x7;
const CODE_START_OF_FRAME = 0;

/** Colour signalling from the most recent sequence header. */
export interface ColorInfo {
  /** 0 = BT.709, 1 = BT.2020. */
  primaries: number;
  /** 0 = BT.709, 1 = PQ. */
  transfer: number;
  /** 0 = BT.709, 1 = BT.2020 (non-constant luminance). */
  ycbcrTransform: number;
  fullRange: boolean;
  /** 0 = centre, 1 = left. */
  chromaSiting: number;
}

export class BitstreamError extends Error {}

export class BitstreamParser {
  readonly dequantOffsets: Uint32Array;
  private payload: Uint32Array;
  private payloadLength = 0;
  private decodedBlocks = 0;
  private totalBlocksInSequence: number;
  private lastSeq = -1;
  private decodedFrameForCurrentSequence = false;
  color: ColorInfo | null = null;

  constructor(private readonly layout: BlockLayout) {
    this.dequantOffsets = new Uint32Array(layout.blockCount32x32);
    this.payload = new Uint32Array(256 * 1024);
    this.totalBlocksInSequence = layout.blockCount32x32;
    this.clear();
  }

  clear(): void {
    this.dequantOffsets.fill(NOT_RECEIVED);
    this.decodedBlocks = 0;
    this.lastSeq = -1;
    this.decodedFrameForCurrentSequence = false;
    this.totalBlocksInSequence = this.layout.blockCount32x32;
    this.payloadLength = 0;
  }

  /** Payload words received so far, valid until the next push or clear. */
  payloadWords(): Uint32Array {
    return this.payload.subarray(0, this.payloadLength);
  }

  get blocksReceived(): number {
    return this.decodedBlocks;
  }

  get blocksExpected(): number {
    return this.totalBlocksInSequence;
  }

  /**
   * Parses one packet (any number of concatenated block records). Returns false
   * on a corrupt packet; stale packets from an older sequence are ignored.
   */
  pushPacket(data: Uint8Array): boolean {
    // Payload words are copied out as u32, so work on an aligned view.
    const bytes = data.byteOffset % 4 === 0 ? data : data.slice();
    const words = new Uint32Array(bytes.buffer, bytes.byteOffset, bytes.byteLength >>> 2);
    let pos = 0; // in words
    let remaining = bytes.byteLength;

    while (remaining >= HEADER_BYTES) {
      const w0 = words[pos]!;
      const w1 = words[pos + 1]!;
      const extended = w0 >>> 31;
      const sequence = (w0 >>> 28) & SEQUENCE_MASK;

      if (extended) {
        if ((w1 >>> 26) & 1) {
          if (this.layout.chroma !== "444") return this.fail("chroma resolution mismatch");
        } else if (this.layout.chroma !== "420") {
          return this.fail("chroma resolution mismatch");
        }
        const diff = (sequence - this.lastSeq) & SEQUENCE_MASK;
        if (this.lastSeq !== -1 && diff > SEQUENCE_MASK / 2) return true;
        if (this.lastSeq === -1 || diff !== 0) {
          this.clear();
          this.lastSeq = sequence;
        }
        const code = (w1 >>> 24) & 3;
        if (code !== CODE_START_OF_FRAME) return this.fail(`unrecognized sequence header mode ${code}`);
        const width = (w0 & 0x3fff) + 1;
        const height = ((w0 >>> 14) & 0x3fff) + 1;
        if (width !== this.layout.width || height !== this.layout.height) {
          return this.fail(`dimension mismatch: ${width}x${height}`);
        }
        this.totalBlocksInSequence = w1 & 0xffffff;
        this.color = {
          primaries: (w1 >>> 27) & 1,
          transfer: (w1 >>> 28) & 1,
          ycbcrTransform: (w1 >>> 29) & 1,
          fullRange: ((w1 >>> 30) & 1) === 1,
          chromaSiting: (w1 >>> 31) & 1,
        };
        pos += 2;
        remaining -= HEADER_BYTES;
        continue;
      }

      const payloadWords = (w0 >>> 16) & 0xfff;
      const packetBytes = payloadWords * 4;
      if (packetBytes > remaining) return this.fail(`packet claims ${packetBytes} bytes, ${remaining} left`);

      if (this.lastSeq === -1) {
        this.clear();
        this.lastSeq = sequence;
      } else {
        const diff = (sequence - this.lastSeq) & SEQUENCE_MASK;
        if (diff > SEQUENCE_MASK / 2) return true;
        if (diff !== 0) {
          this.clear();
          this.lastSeq = sequence;
        }
      }

      const blockIndex = w1 >>> 8;
      if (blockIndex >= this.layout.blockCount32x32) {
        return this.fail(`block_index ${blockIndex} out of bounds`);
      }
      if (payloadWords < HEADER_BYTES / 4) return this.fail("payload_words smaller than its header");

      if (this.dequantOffsets[blockIndex] === NOT_RECEIVED) {
        this.decodedBlocks++;
        this.dequantOffsets[blockIndex] = this.payloadLength;
        this.append(words, pos, payloadWords);
      }

      pos += payloadWords;
      remaining -= packetBytes;
    }

    if (remaining !== 0) return this.fail("did not consume packet completely");
    return true;
  }

  /**
   * Whether a decode should be submitted. Partial frames need the coarsest
   * `pristineBands` levels complete and more than `minimumPacketRatio` of blocks.
   */
  isReady(allowPartialFrame: boolean, pristineBands = 2, minimumPacketRatio = 0.9): boolean {
    if (this.decodedFrameForCurrentSequence || this.lastSeq === -1) return false;
    if (this.decodedBlocks < this.totalBlocksInSequence) {
      if (!allowPartialFrame) return false;
      if (!this.hasPristineBands(pristineBands)) return false;
      if (this.decodedBlocks <= this.totalBlocksInSequence * minimumPacketRatio) return false;
    }
    return true;
  }

  /** Call once a frame has been submitted, so a sequence is not decoded twice. */
  markFrameDecoded(): void {
    this.decodedFrameForCurrentSequence = true;
  }

  private hasPristineBands(bands: number): boolean {
    const missing = (index: number) => this.dequantOffsets[index] === NOT_RECEIVED;
    for (let band = 0; band < bands; band++) {
      for (const component of this.layout.blockMeta) {
        const ranges =
          band === 0
            ? [component[DecompositionLevels - 1]![0]!]
            : [1, 2, 3].map((b) => component[DecompositionLevels - band]![b]!);
        for (const meta of ranges) {
          for (let i = 0; i < meta.blockCount32x32; i++) {
            if (missing(meta.blockOffset32x32 + i)) return false;
          }
        }
      }
    }
    return true;
  }

  private append(src: Uint32Array, start: number, count: number): void {
    const needed = this.payloadLength + count;
    if (needed > this.payload.length) {
      const grown = new Uint32Array(Math.max(needed, this.payload.length * 2));
      grown.set(this.payload.subarray(0, this.payloadLength));
      this.payload = grown;
    }
    this.payload.set(src.subarray(start, start + count), this.payloadLength);
    this.payloadLength = needed;
  }

  private fail(message: string): false {
    console.warn(`pyrowave: ${message}`);
    return false;
  }
}

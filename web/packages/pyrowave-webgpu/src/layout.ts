// Port of PyroWave's BlockLayout (metal/pyrowave_bitstream.cpp, MIT): where every
// 32x32 block of every band lives in the wavelet pyramid.

export const DecompositionLevels = 5;
export const Alignment = 1 << DecompositionLevels;
/** Below this the coarsest band is too small for mirrored edges. */
export const MinimumImageSize = 4 << DecompositionLevels;
export const NumComponents = 3;
export const NumFrequencyBandsPerLevel = 4;
/** Levels below this are stored through FP16 at precision 1. */
export const WaveletFP16Levels = 2;

export type Chroma = "420" | "444";

export interface BlockInfo {
  blockOffset8x8: number;
  blockStride8x8: number;
  blockOffset32x32: number;
  blockStride32x32: number;
  blockCount32x32: number;
}

const align = (value: number, alignment: number) => (value + alignment - 1) & ~(alignment - 1);

export class BlockLayout {
  readonly alignedWidth: number;
  readonly alignedHeight: number;
  /** Indexed [component][level][band]. */
  readonly blockMeta: BlockInfo[][][];
  readonly blockCount8x8: number;
  readonly blockCount32x32: number;

  constructor(
    readonly width: number,
    readonly height: number,
    readonly chroma: Chroma,
  ) {
    // width_minus_1 / height_minus_1 are 14 bits in the sequence header.
    if (!(width > 0 && height > 0 && width <= 16384 && height <= 16384)) {
      throw new RangeError(`dimensions ${width}x${height} are out of range`);
    }
    this.alignedWidth = Math.max(align(width, Alignment), MinimumImageSize);
    this.alignedHeight = Math.max(align(height, Alignment), MinimumImageSize);

    const empty = (): BlockInfo => ({
      blockOffset8x8: 0,
      blockStride8x8: 0,
      blockOffset32x32: 0,
      blockStride32x32: 0,
      blockCount32x32: 0,
    });
    this.blockMeta = Array.from({ length: NumComponents }, () =>
      Array.from({ length: DecompositionLevels }, () =>
        Array.from({ length: NumFrequencyBandsPerLevel }, empty),
      ),
    );

    let count8x8 = 0;
    let count32x32 = 0;
    for (let level = DecompositionLevels - 1; level >= 0; level--) {
      for (let component = 0; component < NumComponents; component++) {
        // Top-level CbCr is not coded at 4:2:0.
        if (level === 0 && component !== 0 && chroma === "420") continue;
        for (let band = level === DecompositionLevels - 1 ? 0 : 1; band < 4; band++) {
          const w = this.levelWidth(level);
          const h = this.levelHeight(level);
          const blocksX8 = Math.ceil(w / 8);
          const blocksY8 = Math.ceil(h / 8);
          const blocksX32 = Math.ceil(w / 32);
          const blocksY32 = Math.ceil(h / 32);
          this.blockMeta[component]![level]![band] = {
            blockOffset8x8: count8x8,
            blockStride8x8: blocksX8,
            blockOffset32x32: count32x32,
            blockStride32x32: blocksX32,
            blockCount32x32: blocksX32 * blocksY32,
          };
          // Same totals as accumulate_block_mapping(): 32x32 blocks per band are
          // derived from the 8x8 grid rounded up to groups of four.
          count32x32 += Math.ceil(blocksX8 / 4) * Math.ceil(blocksY8 / 4);
          count8x8 += blocksX8 * blocksY8;
        }
      }
    }
    this.blockCount8x8 = count8x8;
    this.blockCount32x32 = count32x32;
  }

  /** Mip `level` of the wavelet image, which is half the aligned frame size. */
  levelWidth(level: number): number {
    return Math.max(1, (this.alignedWidth / 2) >> level);
  }

  levelHeight(level: number): number {
    return Math.max(1, (this.alignedHeight / 2) >> level);
  }

  planeWidth(plane: number): number {
    return plane !== 0 && this.chroma === "420" ? this.width / 2 : this.width;
  }

  planeHeight(plane: number): number {
    return plane !== 0 && this.chroma === "420" ? this.height / 2 : this.height;
  }

  alignedPlaneWidth(plane: number): number {
    return plane !== 0 && this.chroma === "420" ? this.alignedWidth / 2 : this.alignedWidth;
  }

  alignedPlaneHeight(plane: number): number {
    return plane !== 0 && this.chroma === "420" ? this.alignedHeight / 2 : this.alignedHeight;
  }
}

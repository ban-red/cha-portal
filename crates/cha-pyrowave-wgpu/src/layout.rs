//! Where every 32x32 block of every band lives in the wavelet pyramid.
//!
//! Port of `BlockLayout` from PyroWave's `metal/pyrowave_bitstream.cpp`
//! (imbcmdth/pyrowave, MIT; see `LICENSE-PYROWAVE`), through our TypeScript port.

use crate::Error;

pub const DECOMPOSITION_LEVELS: usize = 5;
pub const ALIGNMENT: u32 = 1 << DECOMPOSITION_LEVELS;
/// Below this the coarsest band is too small for mirrored edges.
pub const MINIMUM_IMAGE_SIZE: u32 = 4 << DECOMPOSITION_LEVELS;
pub const NUM_COMPONENTS: usize = 3;
pub const NUM_FREQUENCY_BANDS_PER_LEVEL: usize = 4;
/// Levels below this are stored through FP16 at [`crate::Precision::Fp16`].
pub const WAVELET_FP16_LEVELS: usize = 2;

/// Chroma subsampling of the coded frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Chroma {
    C420,
    C444,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct BlockInfo {
    pub block_offset_8x8: u32,
    pub block_stride_8x8: u32,
    pub block_offset_32x32: u32,
    pub block_stride_32x32: u32,
    pub block_count_32x32: u32,
}

#[derive(Clone, Debug)]
pub struct BlockLayout {
    pub width: u32,
    pub height: u32,
    pub chroma: Chroma,
    pub aligned_width: u32,
    pub aligned_height: u32,
    /// Indexed `[component][level][band]`.
    pub block_meta:
        [[[BlockInfo; NUM_FREQUENCY_BANDS_PER_LEVEL]; DECOMPOSITION_LEVELS]; NUM_COMPONENTS],
    pub block_count_8x8: u32,
    pub block_count_32x32: u32,
}

const fn align(value: u32, alignment: u32) -> u32 {
    (value + alignment - 1) & !(alignment - 1)
}

impl BlockLayout {
    pub fn new(width: u32, height: u32, chroma: Chroma) -> Result<Self, Error> {
        // width_minus_1 / height_minus_1 are 14 bits in the sequence header.
        if !(1..=16384).contains(&width) || !(1..=16384).contains(&height) {
            return Err(Error::BadDimensions { width, height });
        }
        let mut layout = BlockLayout {
            width,
            height,
            chroma,
            aligned_width: align(width, ALIGNMENT).max(MINIMUM_IMAGE_SIZE),
            aligned_height: align(height, ALIGNMENT).max(MINIMUM_IMAGE_SIZE),
            block_meta: Default::default(),
            block_count_8x8: 0,
            block_count_32x32: 0,
        };
        let mut count8 = 0;
        let mut count32 = 0;
        for level in (0..DECOMPOSITION_LEVELS).rev() {
            for component in 0..NUM_COMPONENTS {
                // Top-level CbCr is not coded at 4:2:0.
                if level == 0 && component != 0 && chroma == Chroma::C420 {
                    continue;
                }
                let first_band = if level == DECOMPOSITION_LEVELS - 1 {
                    0
                } else {
                    1
                };
                for band in first_band..4 {
                    let w = layout.level_width(level);
                    let h = layout.level_height(level);
                    let (bx8, by8) = (w.div_ceil(8), h.div_ceil(8));
                    let (bx32, by32) = (w.div_ceil(32), h.div_ceil(32));
                    layout.block_meta[component][level][band] = BlockInfo {
                        block_offset_8x8: count8,
                        block_stride_8x8: bx8,
                        block_offset_32x32: count32,
                        block_stride_32x32: bx32,
                        block_count_32x32: bx32 * by32,
                    };
                    // Same totals as accumulate_block_mapping(): 32x32 blocks per band
                    // come from the 8x8 grid rounded up to groups of four.
                    count32 += bx8.div_ceil(4) * by8.div_ceil(4);
                    count8 += bx8 * by8;
                }
            }
        }
        layout.block_count_8x8 = count8;
        layout.block_count_32x32 = count32;
        Ok(layout)
    }

    /// Mip `level` of the wavelet image, which is half the aligned frame size.
    pub fn level_width(&self, level: usize) -> u32 {
        ((self.aligned_width / 2) >> level).max(1)
    }

    pub fn level_height(&self, level: usize) -> u32 {
        ((self.aligned_height / 2) >> level).max(1)
    }

    pub fn plane_width(&self, plane: usize) -> u32 {
        if plane != 0 && self.chroma == Chroma::C420 {
            self.width / 2
        } else {
            self.width
        }
    }

    pub fn plane_height(&self, plane: usize) -> u32 {
        if plane != 0 && self.chroma == Chroma::C420 {
            self.height / 2
        } else {
            self.height
        }
    }

    pub fn aligned_plane_width(&self, plane: usize) -> u32 {
        if plane != 0 && self.chroma == Chroma::C420 {
            self.aligned_width / 2
        } else {
            self.aligned_width
        }
    }

    pub fn aligned_plane_height(&self, plane: usize) -> u32 {
        if plane != 0 && self.chroma == Chroma::C420 {
            self.aligned_height / 2
        } else {
            self.aligned_height
        }
    }
}

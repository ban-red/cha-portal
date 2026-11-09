//! Colour patches for checking what an encoder does to colour
//! (docs/plans/hdr-and-vrr.md, step 2.2): `CHA_ENCODE_TEST_PATTERN=patches`
//! with the encode self-test, and `scripts/color-check.py` on the dumped
//! stream, which reads them back and says which YCbCr matrix the stream was
//! made with. The layout is that script's too: change both together.
//!
//! An 8 × 3 grid filling the picture, each cell one flat colour (sRGB codes):
//! - row 0: red, green, blue, cyan, magenta, yellow, white, black;
//! - row 1: a grey ramp, 0 to 255 in eight steps;
//! - row 2: a near-black ramp, 0 to 28 in steps of 4.

/// The cells' colours, row by row, as `[r, g, b]` sRGB codes.
pub const PATCHES: [[[u8; 3]; 8]; 3] = [
    [
        [255, 0, 0],
        [0, 255, 0],
        [0, 0, 255],
        [0, 255, 255],
        [255, 0, 255],
        [255, 255, 0],
        [255, 255, 255],
        [0, 0, 0],
    ],
    [
        [0, 0, 0],
        [36, 36, 36],
        [73, 73, 73],
        [109, 109, 109],
        [146, 146, 146],
        [182, 182, 182],
        [219, 219, 219],
        [255, 255, 255],
    ],
    [
        [0, 0, 0],
        [4, 4, 4],
        [8, 8, 8],
        [12, 12, 12],
        [16, 16, 16],
        [20, 20, 20],
        [24, 24, 24],
        [28, 28, 28],
    ],
];

/// The cell a pixel falls in: columns are `width / 8` wide and rows
/// `height / 3` high, the last of each taking what is left.
pub fn cell(x: u32, y: u32, width: u32, height: u32) -> (usize, usize) {
    let col = (x / (width / 8).max(1)).min(7) as usize;
    let row = (y / (height / 3).max(1)).min(2) as usize;
    (row, col)
}

/// Paints row `y` of the pattern into `row` as BGRx (DRM `XRGB8888`).
pub fn paint_row(row: &mut [u8], y: u32, width: u32, height: u32) {
    for (x, px) in row
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .take(width as usize)
        .enumerate()
    {
        let (r, c) = cell(x as u32, y, width, height);
        let [red, green, blue] = PATCHES[r][c];
        px.copy_from_slice(&[blue, green, red, 0xff]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_cell_is_its_colour_in_bgrx() {
        let (w, h) = (1280u32, 720u32);
        let mut row = vec![0u8; w as usize * 4];
        // The middle of row 0, column 0 (red) and column 3 (cyan).
        paint_row(&mut row, 120, w, h);
        assert_eq!(&row[80 * 4..80 * 4 + 4], &[0, 0, 255, 0xff]);
        assert_eq!(
            &row[(3 * 160 + 80) * 4..(3 * 160 + 80) * 4 + 4],
            &[255, 255, 0, 0xff]
        );
        // Row 1's last cell is white, row 2's last the brightest near-black.
        paint_row(&mut row, 360, w, h);
        assert_eq!(&row[(w as usize - 1) * 4..], &[255, 255, 255, 0xff]);
        paint_row(&mut row, 719, w, h);
        assert_eq!(&row[(w as usize - 1) * 4..], &[28, 28, 28, 0xff]);
    }

    #[test]
    fn sizes_that_dont_divide_give_the_last_cells_the_rest() {
        // 1366 / 8 = 170 with 6 left over: column 7 is 176 wide.
        assert_eq!(cell(1365, 0, 1366, 768), (0, 7));
        assert_eq!(cell(170 * 7, 0, 1366, 768), (0, 7));
        assert_eq!(cell(170 * 7 - 1, 0, 1366, 768), (0, 6));
        assert_eq!(cell(0, 767, 1366, 768), (2, 0));
    }
}

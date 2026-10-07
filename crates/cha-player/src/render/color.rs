//! YCbCr → RGB for the video shader, computed once per picture format.

use crate::video::{ColorSpec, Matrix};

/// `rgb = rows · (sample − offset)`, with the range expansion folded in.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Conversion {
    pub rows: [[f32; 3]; 3],
    pub offset: [f32; 3],
    /// Multiplies raw samples first (10-bit data sits in the top of 16 bits).
    pub sample_scale: f32,
}

pub fn conversion(spec: ColorSpec) -> Conversion {
    let (kr, kb) = match spec.matrix {
        Matrix::Bt601 => (0.299_f32, 0.114_f32),
        Matrix::Bt709 => (0.2126, 0.0722),
        Matrix::Bt2020 => (0.2627, 0.0593),
    };
    let kg = 1.0 - kr - kb;
    // Limited range maps Y 16..235 and CbCr 16..240 to the full range.
    let (y_scale, c_scale, y_off) = if spec.full_range {
        (1.0, 1.0, 0.0)
    } else {
        (255.0 / 219.0, 255.0 / 224.0, 16.0 / 255.0)
    };
    let c_off = 128.0 / 255.0;
    let cr_r = 2.0 * (1.0 - kr);
    let cb_b = 2.0 * (1.0 - kb);
    let cb_g = -2.0 * kb * (1.0 - kb) / kg;
    let cr_g = -2.0 * kr * (1.0 - kr) / kg;
    Conversion {
        rows: [
            [y_scale, 0.0, cr_r * c_scale],
            [y_scale, cb_g * c_scale, cr_g * c_scale],
            [y_scale, cb_b * c_scale, 0.0],
        ],
        offset: [y_off, c_off, c_off],
        // A 10-bit code `c` is stored as `c << 6`, read as `c·64 / 65535`.
        sample_scale: if spec.ten_bit { 65535.0 / 65472.0 } else { 1.0 },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(c: &Conversion, yuv: [f32; 3]) -> [f32; 3] {
        let v = yuv.map(|x| x * c.sample_scale);
        let d = [v[0] - c.offset[0], v[1] - c.offset[1], v[2] - c.offset[2]];
        c.rows
            .map(|r| (r[0] * d[0] + r[1] * d[1] + r[2] * d[2]).clamp(0.0, 1.0))
    }

    fn spec(full_range: bool, matrix: Matrix) -> ColorSpec {
        ColorSpec {
            full_range,
            ten_bit: false,
            matrix,
        }
    }

    #[test]
    fn limited_range_black_and_white() {
        let c = conversion(spec(false, Matrix::Bt709));
        let black = apply(&c, [16.0 / 255.0, 128.0 / 255.0, 128.0 / 255.0]);
        let white = apply(&c, [235.0 / 255.0, 128.0 / 255.0, 128.0 / 255.0]);
        for v in black {
            assert!(v.abs() < 0.01, "{black:?}");
        }
        for v in white {
            assert!((v - 1.0).abs() < 0.01, "{white:?}");
        }
    }

    #[test]
    fn bt709_red() {
        // Full-range pure red: Y = 0.2126, Cb = -0.1146, Cr = 0.5.
        let c = conversion(spec(true, Matrix::Bt709));
        let red = apply(&c, [0.2126, 0.5 - 0.1146, 1.0]);
        assert!((red[0] - 1.0).abs() < 0.01);
        assert!(red[1] < 0.01 && red[2] < 0.01, "{red:?}");
    }

    #[test]
    fn ten_bit_codes_land_on_the_same_levels() {
        let c = conversion(ColorSpec {
            full_range: false,
            ten_bit: true,
            matrix: Matrix::Bt709,
        });
        // 10-bit white (940) and neutral chroma (512), as read from 16-bit.
        let white = apply(
            &c,
            [
                940.0 * 64.0 / 65535.0,
                512.0 * 64.0 / 65535.0,
                512.0 * 64.0 / 65535.0,
            ],
        );
        for v in white {
            assert!((v - 1.0).abs() < 0.01, "{white:?}");
        }
    }
}

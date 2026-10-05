//! Packed 8-bit RGB to planar YUV 4:2:0, for the software encoder: BT.709,
//! limited range (what the NVENC path signals, so a picture looks the same on
//! either), in integers.
//!
//! The luma loop is a plain map over 4-byte pixels, which the compiler turns
//! into SIMD; chroma averages each 2×2 block's RGB first, then converts. Big
//! pictures are cut into bands of row pairs, one scoped thread each.

/// The byte order of a packed pixel in memory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Order {
    /// R, G, B, A (what GLES reads back; DRM `ABGR8888`).
    Rgba,
    /// B, G, R, A (DRM `ARGB8888`); the compositor reads back RGBA, so this
    /// is for other sources and the tests.
    #[allow(dead_code)]
    Bgra,
}

/// A planar 4:2:0 picture: luma, then the two chroma planes at half size each
/// way (no row padding).
pub struct I420 {
    pub width: usize,
    pub height: usize,
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
}

impl I420 {
    /// A black picture; both sizes must be even.
    pub fn new(width: usize, height: usize) -> Self {
        assert!(
            width > 0 && height > 0 && width.is_multiple_of(2) && height.is_multiple_of(2),
            "4:2:0 needs even sizes, got {width}×{height}"
        );
        Self {
            width,
            height,
            y: vec![16; width * height],
            u: vec![128; width * height / 4],
            v: vec![128; width * height / 4],
        }
    }

    /// Fills the planes from `src`, `height` rows of `stride` bytes (the pixels
    /// are the first `width × 4` of each). With `flip` the rows run bottom to
    /// top, as GL's `glReadPixels` has them.
    pub fn convert(&mut self, src: &[u8], stride: usize, order: Order, flip: bool, threads: usize) {
        let (width, height) = (self.width, self.height);
        assert!(stride >= width * 4 && src.len() >= stride * (height - 1) + width * 4);
        let pairs = height / 2;
        let threads = threads.clamp(1, pairs);
        let per = pairs.div_ceil(threads);
        let job = Job {
            src,
            stride,
            order,
            flip,
            width,
            height,
        };
        if threads == 1 {
            job.run(0, &mut self.y, &mut self.u, &mut self.v);
            return;
        }
        let chroma = per * (width / 2);
        std::thread::scope(|scope| {
            let bands = self
                .y
                .chunks_mut(per * 2 * width)
                .zip(self.u.chunks_mut(chroma))
                .zip(self.v.chunks_mut(chroma));
            for (band, ((y, u), v)) in bands.enumerate() {
                let job = &job;
                scope.spawn(move || job.run(band * per * 2, y, u, v));
            }
        });
    }
}

struct Job<'a> {
    src: &'a [u8],
    stride: usize,
    order: Order,
    flip: bool,
    width: usize,
    height: usize,
}

impl Job<'_> {
    /// The picture's rows from `row0` (even) for as many as `y` holds.
    fn run(&self, row0: usize, y: &mut [u8], u: &mut [u8], v: &mut [u8]) {
        match self.order {
            Order::Rgba => self.rows::<0, 2>(row0, y, u, v),
            Order::Bgra => self.rows::<2, 0>(row0, y, u, v),
        }
    }

    fn pixels(&self, row: usize) -> &[u8] {
        let row = if self.flip {
            self.height - 1 - row
        } else {
            row
        };
        &self.src[row * self.stride..][..self.width * 4]
    }

    fn rows<const R: usize, const B: usize>(
        &self,
        row0: usize,
        y: &mut [u8],
        u: &mut [u8],
        v: &mut [u8],
    ) {
        let w = self.width;
        let pairs = y.len() / (2 * w);
        for pair in 0..pairs {
            let top = self.pixels(row0 + 2 * pair);
            let bottom = self.pixels(row0 + 2 * pair + 1);
            let (y_top, y_bottom) = y[2 * pair * w..][..2 * w].split_at_mut(w);
            luma::<R, B>(top, y_top);
            luma::<R, B>(bottom, y_bottom);
            chroma::<R, B>(
                top,
                bottom,
                &mut u[pair * (w / 2)..][..w / 2],
                &mut v[pair * (w / 2)..][..w / 2],
            );
        }
    }
}

/// Y = 16 + (47 R + 157 G + 16 B) / 256: 0.2126, 0.7152 and 0.0722 of the
/// 219 steps of limited range; white is 235.
#[inline(always)]
fn luma<const R: usize, const B: usize>(pixels: &[u8], out: &mut [u8]) {
    for (y, p) in out.iter_mut().zip(pixels.chunks_exact(4)) {
        let (r, g, b) = (u32::from(p[R]), u32::from(p[1]), u32::from(p[B]));
        *y = (((47 * r + 157 * g + 16 * b + 128) >> 8) + 16) as u8;
    }
}

/// Cb and Cr from the mean RGB of each 2×2 block (the sum is 4× the mean, so
/// the shift is 10, not 8): the BT.709 rows for 224 steps, each summing to
/// zero so greys stay at 128.
#[inline(always)]
fn chroma<const R: usize, const B: usize>(top: &[u8], bottom: &[u8], u: &mut [u8], v: &mut [u8]) {
    let blocks = top.chunks_exact(8).zip(bottom.chunks_exact(8));
    for ((u, v), (a, b)) in u.iter_mut().zip(v.iter_mut()).zip(blocks) {
        let sum = |c: usize| (a[c] as i32) + (a[4 + c] as i32) + (b[c] as i32) + (b[4 + c] as i32);
        let (r, g, bl) = (sum(R), sum(1), sum(B));
        *u = (((-26 * r - 86 * g + 112 * bl + 512) >> 10) + 128) as u8;
        *v = (((112 * r - 102 * g - 10 * bl + 512) >> 10) + 128) as u8;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The float BT.709 limited-range conversion, rounded.
    fn reference(r: u8, g: u8, b: u8) -> (i32, i32, i32) {
        let (r, g, b) = (f64::from(r), f64::from(g), f64::from(b));
        let y = 16.0 + (0.2126 * r + 0.7152 * g + 0.0722 * b) * 219.0 / 255.0;
        let cb = 128.0 + (b - (0.2126 * r + 0.7152 * g + 0.0722 * b)) / 1.8556 * 224.0 / 255.0;
        let cr = 128.0 + (r - (0.2126 * r + 0.7152 * g + 0.0722 * b)) / 1.5748 * 224.0 / 255.0;
        (y.round() as i32, cb.round() as i32, cr.round() as i32)
    }

    fn solid(width: usize, height: usize, rgb: (u8, u8, u8), order: Order) -> Vec<u8> {
        let (r, g, b) = rgb;
        let pixel = match order {
            Order::Rgba => [r, g, b, 255],
            Order::Bgra => [b, g, r, 255],
        };
        pixel.repeat(width * height)
    }

    #[test]
    fn colours_match_the_float_conversion_within_a_step() {
        let colours = [
            (0, 0, 0),
            (255, 255, 255),
            (128, 128, 128),
            (255, 0, 0),
            (0, 255, 0),
            (0, 0, 255),
            (255, 255, 0),
            (0, 255, 255),
            (255, 0, 255),
            (30, 144, 255),
            (200, 100, 50),
        ];
        for order in [Order::Rgba, Order::Bgra] {
            for rgb in colours {
                let mut out = I420::new(16, 8);
                out.convert(&solid(16, 8, rgb, order), 64, order, false, 1);
                let (y, cb, cr) = reference(rgb.0, rgb.1, rgb.2);
                let got = (
                    i32::from(out.y[0]),
                    i32::from(out.u[0]),
                    i32::from(out.v[0]),
                );
                assert!(
                    (got.0 - y).abs() <= 1 && (got.1 - cb).abs() <= 1 && (got.2 - cr).abs() <= 1,
                    "{rgb:?} {order:?}: got {got:?}, want {:?}",
                    (y, cb, cr)
                );
                // The whole picture is the one colour.
                assert!(out.y.iter().all(|&p| i32::from(p) == got.0));
                assert!(out.u.iter().all(|&p| i32::from(p) == got.1));
                assert!(out.v.iter().all(|&p| i32::from(p) == got.2));
            }
        }
    }

    #[test]
    fn limited_range_ends_and_neutral_chroma() {
        let mut out = I420::new(4, 4);
        out.convert(
            &solid(4, 4, (255, 255, 255), Order::Bgra),
            16,
            Order::Bgra,
            false,
            1,
        );
        assert_eq!((out.y[0], out.u[0], out.v[0]), (235, 128, 128));
        out.convert(
            &solid(4, 4, (0, 0, 0), Order::Bgra),
            16,
            Order::Bgra,
            false,
            1,
        );
        assert_eq!((out.y[0], out.u[0], out.v[0]), (16, 128, 128));
    }

    /// A picture whose every pixel and block differs, to check placement.
    fn gradient(width: usize, height: usize, stride: usize) -> Vec<u8> {
        let mut src = vec![0xAA; stride * height];
        for y in 0..height {
            for x in 0..width {
                let at = y * stride + x * 4;
                src[at..at + 4].copy_from_slice(&[
                    (x * 7 + y) as u8,
                    (y * 5 + x * 3) as u8,
                    (x * 11 + y * 13) as u8,
                    255,
                ]);
            }
        }
        src
    }

    #[test]
    fn rows_and_blocks_land_where_they_belong() {
        let (w, h) = (10, 6);
        let src = gradient(w, h, w * 4 + 8);
        let mut out = I420::new(w, h);
        out.convert(&src, w * 4 + 8, Order::Bgra, false, 1);
        for y in 0..h {
            for x in 0..w {
                let p = &src[y * (w * 4 + 8) + x * 4..];
                let want =
                    (((47 * u32::from(p[2]) + 157 * u32::from(p[1]) + 16 * u32::from(p[0]) + 128)
                        >> 8)
                        + 16) as u8;
                assert_eq!(out.y[y * w + x], want, "luma at {x},{y}");
            }
        }
        // The first chroma sample is the mean of the top left 2x2 block.
        let mean = |c: usize| {
            [(0, 0), (1, 0), (0, 1), (1, 1)]
                .iter()
                .map(|&(x, y)| i32::from(src[y * (w * 4 + 8) + x * 4 + c]))
                .sum::<i32>()
        };
        let (r, g, b) = (mean(2), mean(1), mean(0));
        assert_eq!(
            i32::from(out.u[0]),
            ((-26 * r - 86 * g + 112 * b + 512) >> 10) + 128
        );
        assert_eq!(
            i32::from(out.v[0]),
            ((112 * r - 102 * g - 10 * b + 512) >> 10) + 128
        );
    }

    #[test]
    fn flipping_reads_the_rows_bottom_up() {
        let (w, h) = (8, 4);
        let src = gradient(w, h, w * 4);
        let mut up = I420::new(w, h);
        up.convert(&src, w * 4, Order::Rgba, false, 1);
        let mut flipped = src.clone();
        for y in 0..h {
            flipped[y * w * 4..][..w * 4].copy_from_slice(&src[(h - 1 - y) * w * 4..][..w * 4]);
        }
        let mut down = I420::new(w, h);
        down.convert(&flipped, w * 4, Order::Rgba, true, 1);
        assert_eq!(up.y, down.y);
        assert_eq!(up.u, down.u);
        assert_eq!(up.v, down.v);
    }

    #[test]
    fn threads_make_the_same_picture() {
        let (w, h) = (66, 50);
        let src = gradient(w, h, w * 4);
        let mut one = I420::new(w, h);
        one.convert(&src, w * 4, Order::Rgba, true, 1);
        for threads in [2, 3, 7, 64] {
            let mut many = I420::new(w, h);
            many.convert(&src, w * 4, Order::Rgba, true, threads);
            assert_eq!(one.y, many.y, "{threads} threads");
            assert_eq!(one.u, many.u, "{threads} threads");
            assert_eq!(one.v, many.v, "{threads} threads");
        }
    }
}

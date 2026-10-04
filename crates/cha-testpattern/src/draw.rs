//! The picture: a static backdrop and title, plus what changes every frame
//! (a sweeping bar, the frame counter, the frame number as a 32-cell binary
//! strip) and a flash while input was just received. Only the regions that
//! change are repainted.

/// A rectangle in pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }

    pub fn is_empty(&self) -> bool {
        self.w <= 0 || self.h <= 0
    }

    pub fn intersect(&self, other: &Rect) -> Rect {
        let x0 = self.x.max(other.x);
        let y0 = self.y.max(other.y);
        let x1 = (self.x + self.w).min(other.x + other.w);
        let y1 = (self.y + self.h).min(other.y + other.h);
        Rect::new(x0, y0, x1 - x0, y1 - y0)
    }
}

/// Everything that changes between frames.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Scene {
    pub frame: u64,
    pub bar: Rect,
    pub flash: bool,
}

const MARGIN: i32 = 48;
const TITLE: &str = "CHA TEST PATTERN";
const TITLE_SCALE: i32 = 6;
const COUNTER_SCALE: i32 = 12;
const COUNTER_DIGITS: usize = 8;
const CELL: i32 = 24;
const CELL_GAP: i32 = 6;
const FLASH: i32 = 360;
pub const BAR_WIDTH: i32 = 48;

fn text_rect(x: i32, y: i32, chars: usize, scale: i32) -> Rect {
    Rect::new(x, y, chars as i32 * 6 * scale - scale, 7 * scale)
}

pub fn counter_rect() -> Rect {
    text_rect(
        MARGIN,
        MARGIN + 9 * TITLE_SCALE,
        COUNTER_DIGITS,
        COUNTER_SCALE,
    )
}

pub fn strip_rect(height: i32) -> Rect {
    Rect::new(
        MARGIN,
        height - MARGIN - CELL,
        32 * (CELL + CELL_GAP) - CELL_GAP,
        CELL,
    )
}

pub fn flash_rect(width: i32, height: i32) -> Rect {
    Rect::new((width - FLASH) / 2, (height - FLASH) / 2, FLASH, FLASH)
}

/// A pixel buffer (XRGB8888).
pub struct Canvas<'a> {
    pub pixels: &'a mut [u32],
    pub width: i32,
    pub height: i32,
}

impl Canvas<'_> {
    fn bounds(&self) -> Rect {
        Rect::new(0, 0, self.width, self.height)
    }

    /// Repaints `area` from scratch: backdrop, then every element over it.
    pub fn paint(&mut self, area: Rect, scene: &Scene) {
        let area = area.intersect(&self.bounds());
        if area.is_empty() {
            return;
        }
        for y in area.y..area.y + area.h {
            let row = (y * self.width) as usize;
            for x in area.x..area.x + area.w {
                self.pixels[row + x as usize] = backdrop(x, y, self.height);
            }
        }
        self.text(area, MARGIN, MARGIN, TITLE, TITLE_SCALE, 0x00e8_eef4);
        let counter = format!(
            "{:0width$}",
            scene.frame % 100_000_000,
            width = COUNTER_DIGITS
        );
        let c = counter_rect();
        self.text(area, c.x, c.y, &counter, COUNTER_SCALE, 0x00ff_d400);
        let strip = strip_rect(self.height);
        for bit in 0..32 {
            let on = scene.frame >> (31 - bit) & 1 == 1;
            let cell = Rect::new(strip.x + bit * (CELL + CELL_GAP), strip.y, CELL, CELL);
            self.fill(area, cell, if on { 0x00ff_ffff } else { 0x0020_2020 });
        }
        if scene.flash {
            self.fill(area, flash_rect(self.width, self.height), 0x00ff_ffff);
        }
        self.fill(area, scene.bar, 0x00c8_d0d8);
    }

    fn fill(&mut self, clip: Rect, rect: Rect, color: u32) {
        let r = rect.intersect(&clip);
        if r.is_empty() {
            return;
        }
        for y in r.y..r.y + r.h {
            let row = (y * self.width) as usize;
            self.pixels[row + r.x as usize..row + (r.x + r.w) as usize].fill(color);
        }
    }

    fn text(&mut self, clip: Rect, x: i32, y: i32, text: &str, scale: i32, color: u32) {
        if text_rect(x, y, text.chars().count(), scale)
            .intersect(&clip)
            .is_empty()
        {
            return;
        }
        for (i, ch) in text.chars().enumerate() {
            let gx = x + i as i32 * 6 * scale;
            for (row, bits) in glyph(ch).iter().enumerate() {
                for col in 0..5 {
                    if bits >> (4 - col) & 1 == 1 {
                        let px = Rect::new(gx + col * scale, y + row as i32 * scale, scale, scale);
                        self.fill(clip, px, color);
                    }
                }
            }
        }
    }
}

/// A dark vertical gradient with a grid every 160 px (tearing shows up as a
/// broken line).
fn backdrop(x: i32, y: i32, height: i32) -> u32 {
    if x % 160 == 0 || y % 160 == 0 {
        return 0x0030_3a48;
    }
    let t = (y * 255 / height.max(1)) as u32;
    let r = 0x10 + t / 12;
    let g = 0x14;
    let b = 0x20 + t / 6;
    r << 16 | g << 8 | b
}

/// 5×7 glyphs for the characters we draw.
fn glyph(ch: char) -> [u8; 7] {
    match ch {
        '0' => [0x0e, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0e],
        '1' => [0x04, 0x0c, 0x04, 0x04, 0x04, 0x04, 0x0e],
        '2' => [0x0e, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1f],
        '3' => [0x1f, 0x02, 0x04, 0x02, 0x01, 0x11, 0x0e],
        '4' => [0x02, 0x06, 0x0a, 0x12, 0x1f, 0x02, 0x02],
        '5' => [0x1f, 0x10, 0x1e, 0x01, 0x01, 0x11, 0x0e],
        '6' => [0x06, 0x08, 0x10, 0x1e, 0x11, 0x11, 0x0e],
        '7' => [0x1f, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08],
        '8' => [0x0e, 0x11, 0x11, 0x0e, 0x11, 0x11, 0x0e],
        '9' => [0x0e, 0x11, 0x11, 0x0f, 0x01, 0x02, 0x0c],
        'A' => [0x0e, 0x11, 0x11, 0x1f, 0x11, 0x11, 0x11],
        'C' => [0x0e, 0x11, 0x10, 0x10, 0x10, 0x11, 0x0e],
        'E' => [0x1f, 0x10, 0x10, 0x1e, 0x10, 0x10, 0x1f],
        'H' => [0x11, 0x11, 0x11, 0x1f, 0x11, 0x11, 0x11],
        'N' => [0x11, 0x11, 0x19, 0x15, 0x13, 0x11, 0x11],
        'P' => [0x1e, 0x11, 0x11, 0x1e, 0x10, 0x10, 0x10],
        'R' => [0x1e, 0x11, 0x11, 0x1e, 0x14, 0x12, 0x11],
        'S' => [0x0f, 0x10, 0x10, 0x0e, 0x01, 0x01, 0x1e],
        'T' => [0x1f, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04],
        _ => [0; 7],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intersects_rects() {
        let a = Rect::new(0, 0, 10, 10);
        assert_eq!(a.intersect(&Rect::new(5, 5, 10, 10)), Rect::new(5, 5, 5, 5));
        assert!(a.intersect(&Rect::new(20, 20, 5, 5)).is_empty());
    }

    #[test]
    fn strip_encodes_the_frame_number() {
        let (w, h) = (1280, 720);
        let mut pixels = vec![0u32; (w * h) as usize];
        let mut canvas = Canvas {
            pixels: &mut pixels,
            width: w,
            height: h,
        };
        let scene = Scene {
            frame: 0b101,
            ..Scene::default()
        };
        canvas.paint(Rect::new(0, 0, w, h), &scene);
        let strip = strip_rect(h);
        let cell = |bit: i32| {
            let x = strip.x + bit * (CELL + CELL_GAP) + CELL / 2;
            pixels[((strip.y + CELL / 2) * w + x) as usize]
        };
        assert_eq!(cell(31), 0x00ff_ffff);
        assert_eq!(cell(30), 0x0020_2020);
        assert_eq!(cell(29), 0x00ff_ffff);
    }
}

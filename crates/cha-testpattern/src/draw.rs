//! The picture: a static backdrop and title, plus what changes every frame
//! (a sweeping bar, the frame counter, the frame number as a 32-cell binary
//! strip), a flash while input was just received, and the first gamepad's
//! state. Only the regions that change are repainted.

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
    pub pad: PadView,
    /// Repainted with fresh noise every frame (empty: none).
    pub noise: Rect,
}

/// A centred rectangle covering `percent` of a `width`×`height` screen.
pub fn noise_rect(width: i32, height: i32, percent: u32) -> Rect {
    let side = (f64::from(percent.min(100)) / 100.0).sqrt();
    let (w, h) = (
        (f64::from(width) * side) as i32,
        (f64::from(height) * side) as i32,
    );
    Rect::new((width - w) / 2, (height - h) / 2, w, h)
}

/// A gamepad as the panel shows it (Xbox layout and ranges).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PadView {
    pub connected: bool,
    /// Bits in [`PAD_BUTTONS`] order.
    pub buttons: u16,
    /// Left and right, 0..255.
    pub triggers: [u8; 2],
    /// The d-pad, -1..1 each way (y down).
    pub hat: (i8, i8),
    /// Left and right sticks, -32768..32767 (y down).
    pub sticks: [(i16, i16); 2],
}

/// The panel's buttons, left to right, with their colours when pressed.
pub const PAD_BUTTONS: [(&str, u32); 11] = [
    ("A", 0x003c_cf4e),
    ("B", 0x00e8_4a3c),
    ("X", 0x0036_7ef0),
    ("Y", 0x00f0_c419),
    ("LB", 0x00ff_ffff),
    ("RB", 0x00ff_ffff),
    ("BACK", 0x00ff_ffff),
    ("START", 0x00ff_ffff),
    ("LS", 0x00ff_ffff),
    ("RS", 0x00ff_ffff),
    ("GUIDE", 0x00ff_ffff),
];

const MARGIN: i32 = 48;
const TITLE: &str = "CHA TEST PATTERN";
const TITLE_SCALE: i32 = 6;
const COUNTER_SCALE: i32 = 12;
const COUNTER_DIGITS: usize = 8;
const CELL: i32 = 24;
const CELL_GAP: i32 = 6;
const FLASH: i32 = 360;
pub const BAR_WIDTH: i32 = 48;
const PAD_LABEL_SCALE: i32 = 4;
const STICK: i32 = 96;
const STICK_DOT: i32 = 12;
const TRIGGER: i32 = 96;
const OFF: u32 = 0x0020_2020;

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

/// The gamepad panel, top right: a label, the buttons, the d-pad and
/// triggers, the sticks.
pub fn pad_rect(width: i32) -> Rect {
    let w = PAD_BUTTONS.len() as i32 * (CELL + CELL_GAP) - CELL_GAP;
    let h = 7 * PAD_LABEL_SCALE + 3 * 12 + 2 * CELL + STICK;
    Rect::new(width - MARGIN - w, MARGIN, w, h)
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
        if !pad_rect(self.width).intersect(&area).is_empty() {
            self.pad(area, &scene.pad);
        }
        if scene.flash {
            self.fill(area, flash_rect(self.width, self.height), 0x00ff_ffff);
        }
        self.noise(area, scene.noise, scene.frame);
        self.fill(area, scene.bar, 0x00c8_d0d8);
    }

    /// Pseudo-random pixels, different every frame: incompressible.
    fn noise(&mut self, clip: Rect, rect: Rect, frame: u64) {
        let r = rect.intersect(&clip);
        for y in r.y..r.y + r.h {
            // xorshift64, seeded per row and frame.
            let mut s = (frame << 20 ^ y as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
            let row = (y * self.width) as usize;
            for x in r.x..r.x + r.w {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                self.pixels[row + x as usize] = s as u32 & 0x00ff_ffff;
            }
        }
    }

    fn pad(&mut self, clip: Rect, pad: &PadView) {
        let p = pad_rect(self.width);
        let (label, color) = if pad.connected {
            ("PAD", 0x00e8_eef4)
        } else {
            ("NO PAD", 0x0060_6870)
        };
        self.text(clip, p.x, p.y, label, PAD_LABEL_SCALE, color);
        let mut y = p.y + 7 * PAD_LABEL_SCALE + 12;
        for (i, (_, on)) in PAD_BUTTONS.iter().enumerate() {
            let lit = pad.buttons >> i & 1 == 1;
            let cell = Rect::new(p.x + i as i32 * (CELL + CELL_GAP), y, CELL, CELL);
            self.fill(clip, cell, if lit { *on } else { OFF });
        }
        y += CELL + 12;
        // D-pad: up, down, left, right.
        let dirs = [pad.hat.1 < 0, pad.hat.1 > 0, pad.hat.0 < 0, pad.hat.0 > 0];
        for (i, lit) in dirs.iter().enumerate() {
            let cell = Rect::new(p.x + i as i32 * (CELL + CELL_GAP), y, CELL, CELL);
            self.fill(clip, cell, if *lit { 0x00ff_ffff } else { OFF });
        }
        for (i, value) in pad.triggers.iter().enumerate() {
            let x = p.x + p.w - (2 - i as i32) * (TRIGGER + CELL_GAP) + CELL_GAP;
            self.fill(clip, Rect::new(x, y, TRIGGER, CELL), OFF);
            let filled = i32::from(*value) * TRIGGER / 255;
            self.fill(clip, Rect::new(x, y, filled, CELL), 0x00ff_ffff);
        }
        y += CELL + 12;
        for (i, (sx, sy)) in pad.sticks.iter().enumerate() {
            let x = if i == 0 { p.x } else { p.x + p.w - STICK };
            self.fill(clip, Rect::new(x, y, STICK, STICK), OFF);
            let travel = (STICK - STICK_DOT) / 2;
            let dx = i32::from(*sx) * travel / 32768;
            let dy = i32::from(*sy) * travel / 32768;
            let dot = Rect::new(x + travel + dx, y + travel + dy, STICK_DOT, STICK_DOT);
            self.fill(clip, dot, 0x00ff_ffff);
        }
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
        'D' => [0x1e, 0x11, 0x11, 0x11, 0x11, 0x11, 0x1e],
        'E' => [0x1f, 0x10, 0x10, 0x1e, 0x10, 0x10, 0x1f],
        'H' => [0x11, 0x11, 0x11, 0x1f, 0x11, 0x11, 0x11],
        'N' => [0x11, 0x11, 0x19, 0x15, 0x13, 0x11, 0x11],
        'O' => [0x0e, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0e],
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
            noise: Rect::new(0, 0, 0, 0),
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

    #[test]
    fn pad_panel_lights_pressed_buttons() {
        let (w, h) = (1280, 720);
        let mut pixels = vec![0u32; (w * h) as usize];
        let mut canvas = Canvas {
            pixels: &mut pixels,
            width: w,
            height: h,
        };
        let scene = Scene {
            noise: Rect::new(0, 0, 0, 0),
            pad: PadView {
                connected: true,
                buttons: 0b10, // B
                ..PadView::default()
            },
            ..Scene::default()
        };
        let p = pad_rect(w);
        canvas.paint(p, &scene);
        let y = p.y + 7 * PAD_LABEL_SCALE + 12 + CELL / 2;
        let at = |i: i32| pixels[(y * w + p.x + i * (CELL + CELL_GAP) + CELL / 2) as usize];
        assert_eq!(at(0), OFF);
        assert_eq!(at(1), 0x00e8_4a3c);
        assert!(p.x + p.w <= w);
    }
}

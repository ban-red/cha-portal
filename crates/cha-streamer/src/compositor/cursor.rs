//! The cursor, composited into the video: the app's own cursor surface when it
//! sets one, otherwise our built-in arrow. (Desktop mode will draw the cursor
//! in the browser instead; plan §3.5.)

use std::sync::OnceLock;

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::element::memory::{
    MemoryRenderBuffer, MemoryRenderBufferRenderElement,
};
use smithay::backend::renderer::element::surface::{
    WaylandSurfaceRenderElement, render_elements_from_surface_tree,
};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::input::pointer::{CursorImageStatus, CursorImageSurfaceData};
use smithay::render_elements;
use smithay::utils::{Physical, Point, Scale, Transform};
use smithay::wayland::compositor::with_states;

use super::State;

render_elements! {
    pub CursorElement<=GlesRenderer>;
    Surface=WaylandSurfaceRenderElement<GlesRenderer>,
    Memory=MemoryRenderBufferRenderElement<GlesRenderer>,
}

const ARROW_W: usize = 12;
const ARROW_H: usize = 19;

/// The classic arrow: `#` outline, `.` fill. Hotspot at the tip (0, 0).
const ARROW: [&str; ARROW_H] = [
    "#           ",
    "##          ",
    "#.#         ",
    "#..#        ",
    "#...#       ",
    "#....#      ",
    "#.....#     ",
    "#......#    ",
    "#.......#   ",
    "#........#  ",
    "#.........# ",
    "#......#####",
    "#...#..#    ",
    "#..# #..#   ",
    "#.#  #..#   ",
    "##    #..#  ",
    "#     #..#  ",
    "       #..# ",
    "        ##  ",
];

/// The arrow as a premultiplied ARGB8888 buffer.
pub(super) fn arrow() -> MemoryRenderBuffer {
    static PIXELS: OnceLock<Vec<u8>> = OnceLock::new();
    let pixels = PIXELS.get_or_init(|| {
        let mut out = Vec::with_capacity(ARROW_W * ARROW_H * 4);
        for row in ARROW {
            for ch in row.chars() {
                // Little-endian ARGB8888 is B, G, R, A in memory.
                out.extend_from_slice(match ch {
                    '#' => &[0, 0, 0, 255],
                    '.' => &[255, 255, 255, 255],
                    _ => &[0, 0, 0, 0],
                });
            }
        }
        out
    });
    MemoryRenderBuffer::from_slice(
        pixels,
        Fourcc::Argb8888,
        (ARROW_W as i32, ARROW_H as i32),
        1,
        Transform::Normal,
        None,
    )
}

impl State {
    pub(super) fn cursor_elements(&mut self) -> Vec<CursorElement> {
        let location: Point<i32, Physical> = self.pointer_location.to_physical(1.0).to_i32_round();
        match &self.cursor_status {
            CursorImageStatus::Hidden => Vec::new(),
            CursorImageStatus::Named(_) => MemoryRenderBufferRenderElement::from_buffer(
                &mut self.renderer,
                location.to_f64(),
                &self.arrow,
                None,
                None,
                None,
                Kind::Cursor,
            )
            .map(|e| vec![CursorElement::Memory(e)])
            .unwrap_or_default(),
            CursorImageStatus::Surface(surface) => {
                let hotspot = with_states(surface, |states| {
                    states
                        .data_map
                        .get::<CursorImageSurfaceData>()
                        .map(|d| d.lock().expect("cursor data lock").hotspot)
                        .unwrap_or_default()
                });
                let origin = location - hotspot.to_physical(1);
                render_elements_from_surface_tree(
                    &mut self.renderer,
                    surface,
                    origin,
                    Scale::from(1.0),
                    1.0,
                    Kind::Cursor,
                )
            }
        }
    }
}

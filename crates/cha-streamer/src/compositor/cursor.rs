//! The cursor. Composited into the video (the app's own cursor surface when
//! it sets one, otherwise our built-in arrow), unless the page draws it: then
//! its shape is published instead (plan §3.5), as a CSS keyword when the app
//! names one (the cursor-shape protocol's names are CSS's) or as the app's
//! image, and the pointer moves on the page with no stream delay.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::{Arc, OnceLock};

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::element::memory::{
    MemoryRenderBuffer, MemoryRenderBufferRenderElement,
};
use smithay::backend::renderer::element::surface::{
    WaylandSurfaceRenderElement, render_elements_from_surface_tree,
};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::utils::with_renderer_surface_state;
use smithay::input::pointer::{CursorImageStatus, CursorImageSurfaceData};
use smithay::reexports::wayland_server::protocol::wl_shm;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::render_elements;
use smithay::utils::{Physical, Point, Scale, Transform};
use smithay::wayland::compositor::with_states;
use smithay::wayland::shm::with_buffer_contents;
use tokio::sync::watch;

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

/// The cursor as the page draws it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CursorShape {
    Hidden,
    /// A CSS cursor keyword.
    Named(&'static str),
    /// An app's own cursor: straight-alpha RGBA, the hotspot from its top
    /// left, and `id`, a hash of it all (the page caches images by it).
    Image {
        id: u64,
        width: u32,
        height: u32,
        hotspot: (i32, i32),
        rgba: Arc<[u8]>,
    },
}

/// The current cursor shape, for the sessions.
pub type CursorWatch = watch::Receiver<CursorShape>;

/// Where the pointer is on the picture (0..1), for viewers' pages to draw it
/// when the controller's page draws the cursor (`drawn`: the picture has it).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PointerSpot {
    pub x: f32,
    pub y: f32,
    pub drawn: bool,
}

pub type PointerWatch = watch::Receiver<PointerSpot>;

/// Larger cursor images go as `default` (browsers cap cursors at 128 px).
const MAX_CURSOR: i32 = 256;

impl State {
    /// Publishes the cursor's shape if it may have changed since the last tick.
    pub(super) fn publish_cursor(&mut self) {
        if !std::mem::take(&mut self.cursor_changed) {
            return;
        }
        let shape = match &self.cursor_status {
            CursorImageStatus::Hidden => CursorShape::Hidden,
            CursorImageStatus::Named(icon) => CursorShape::Named(icon.name()),
            CursorImageStatus::Surface(surface) => {
                surface_image(surface).unwrap_or(CursorShape::Named("default"))
            }
        };
        self.cursor.send_if_modified(|current| {
            let changed = *current != shape;
            *current = shape;
            changed
        });
    }

    /// Publishes where the pointer is, if it moved (at encode ticks).
    pub(super) fn publish_pointer(&mut self) {
        let (w, h) = self.pool.size();
        let spot = PointerSpot {
            x: (self.pointer_location.x / f64::from(w.max(1))) as f32,
            y: (self.pointer_location.y / f64::from(h.max(1))) as f32,
            drawn: !self.client_cursor,
        };
        self.pointer.send_if_modified(|current| {
            let changed = *current != spot;
            *current = spot;
            changed
        });
    }

    pub(super) fn cursor_elements(&mut self) -> Vec<CursorElement> {
        if self.client_cursor {
            return Vec::new();
        }
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

/// An app's cursor surface as RGBA, if it's an shm buffer of a sane size.
fn surface_image(surface: &WlSurface) -> Option<CursorShape> {
    let hotspot = with_states(surface, |states| {
        states
            .data_map
            .get::<CursorImageSurfaceData>()
            .map(|d| d.lock().expect("cursor data lock").hotspot)
            .unwrap_or_default()
    });
    let buffer = with_renderer_surface_state(surface, |state| {
        state.buffer().map(|b| std::ops::Deref::deref(b).clone())
    })??;
    let (width, height, rgba) = with_buffer_contents(&buffer, |ptr, len, data| {
        let opaque = match data.format {
            wl_shm::Format::Argb8888 => false,
            wl_shm::Format::Xrgb8888 => true,
            _ => return None,
        };
        if data.width <= 0
            || data.height <= 0
            || data.width > MAX_CURSOR
            || data.height > MAX_CURSOR
        {
            return None;
        }
        let (w, h, stride) = (
            data.width as usize,
            data.height as usize,
            data.stride as usize,
        );
        let start = usize::try_from(data.offset).ok()?;
        if stride < w * 4 || start + stride * h > len {
            return None;
        }
        // SAFETY: the pool maps `len` bytes at `ptr`, and the frame is within them.
        let pixels = unsafe { std::slice::from_raw_parts(ptr.add(start), stride * h) };
        let mut rgba = Vec::with_capacity(w * h * 4);
        for row in pixels.chunks_exact(stride) {
            // Little-endian ARGB8888 is B, G, R, A in memory, premultiplied.
            for px in row[..w * 4].chunks_exact(4) {
                let a = if opaque { 255 } else { px[3] };
                let straight = |c: u8| match a {
                    0 => 0,
                    255 => c,
                    _ => ((u16::from(c) * 255 + u16::from(a) / 2) / u16::from(a)).min(255) as u8,
                };
                rgba.extend_from_slice(&[straight(px[2]), straight(px[1]), straight(px[0]), a]);
            }
        }
        Some((w as u32, h as u32, rgba))
    })
    .ok()??;
    let mut hasher = DefaultHasher::new();
    (width, height, hotspot.x, hotspot.y, &rgba).hash(&mut hasher);
    Some(CursorShape::Image {
        id: hasher.finish(),
        width,
        height,
        hotspot: (hotspot.x, hotspot.y),
        rgba: rgba.into(),
    })
}

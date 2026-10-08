//! Where the panel sits: a corner, or wherever it was dropped, and the drag that moves it.

use egui::{Pos2, Rect, Vec2, pos2};

use crate::overlay_prefs::{Corner, OverlayPrefs, Pos};

/// The gap from the window's edge to the panel.
pub(super) const INSET: f32 = 12.0;

/// The top y of a panel in a top corner: the inset, or below the toolbar (10
/// points clear) while it shows, as in the portal.
pub(super) fn top_y(top_inset: f32) -> f32 {
    if top_inset > 0.0 {
        top_inset + crate::ui::toolbar::TOP + 10.0
    } else {
        INSET
    }
}

/// Where the panel's top left goes, from the size it had last frame (the
/// first frame is invisible while egui measures it).
pub(super) fn position(
    prefs: &OverlayPrefs,
    dragging: Option<&Drag>,
    size: Vec2,
    screen: Rect,
    top_inset: f32,
) -> Pos2 {
    let top_y = top_y(top_inset);
    let place = |left: bool, top: bool| {
        pos2(
            if left {
                INSET
            } else {
                screen.right() - INSET - size.x
            },
            if top {
                top_y
            } else {
                screen.bottom() - INSET - size.y
            },
        )
    };
    match (dragging, prefs.snap, prefs.pos) {
        (Some(d), _, _) => pos2(d.at.left, d.at.top),
        (None, false, Some(p)) => {
            let p = p.clamped(size.x, size.y, screen.width(), screen.height());
            pos2(p.left, p.top)
        }
        (None, _, _) => match prefs.corner {
            Corner::TopLeft => place(true, true),
            Corner::TopRight => place(false, true),
            Corner::BottomLeft => place(true, false),
            Corner::BottomRight => place(false, false),
        },
    }
}

/// A drag of the header in progress.
pub(super) struct Drag {
    /// Pointer minus the panel's top left, when the drag began.
    grab: Vec2,
    at: Pos,
}

impl Drag {
    pub(super) fn begin(pointer: Pos2, rect: Rect) -> Self {
        Self {
            grab: pointer - rect.min,
            at: Pos {
                left: rect.min.x,
                top: rect.min.y,
            },
        }
    }

    pub(super) fn to(&mut self, pointer: Pos2, rect: Rect, screen: Rect) {
        self.at = Pos {
            left: pointer.x - self.grab.x,
            top: pointer.y - self.grab.y,
        }
        .clamped(rect.width(), rect.height(), screen.width(), screen.height());
    }

    /// Dropped: the nearest corner while snapping, else exactly here.
    pub(super) fn drop_on(self, prefs: &mut OverlayPrefs, rect: Rect, screen: Rect) {
        let at = self
            .at
            .clamped(rect.width(), rect.height(), screen.width(), screen.height());
        if prefs.snap {
            prefs.corner = Corner::nearest(
                at.left + rect.width() / 2.0,
                at.top + rect.height() / 2.0,
                screen.width(),
                screen.height(),
            );
        } else {
            prefs.pos = Some(Pos {
                left: at.left.round(),
                top: at.top.round(),
            });
        }
    }
}

//! The spec's icons (`web/packages/ui-spec/icons.json`) drawn with egui's painter.
//!
//! Each icon is SVG path data in a 16-unit box. [`paint`] flattens it at the
//! size it is drawn (finer curves when bigger), then strokes or fills it. egui
//! strokes paths with butt ends and mitre joins, so round caps and joins, which
//! the portal's icons use, are added as discs: at the ends of open subpaths and
//! at corners sharper than a few degrees. Fills are convex polygons where they
//! can be, and ear-clipped triangles where not.

use cha_ui_spec::fill::{area2, is_convex, triangulate};
use cha_ui_spec::{LineCap, LineJoin, Paint};
use egui::epaint::{Mesh, PathShape, PathStroke};
use egui::{Color32, Painter, Pos2, Rect, Shape, Stroke, pos2};

pub use cha_ui_spec::Icon;

/// How far, in physical pixels, a flattened curve may stray from the true one.
const TOLERANCE_PX: f32 = 0.08;
/// A turn below this angle (radians) needs no join disc: the mitre is hidden.
const JOIN_TURN: f32 = 0.35;
/// Strokes never get thinner than this many points; the browser's don't either at 1x.
const MIN_STROKE: f32 = 1.0;

/// Draw `icon` into `rect`, a square standing for the icon's 16-unit box.
pub fn paint(painter: &Painter, rect: Rect, icon: Icon, color: Color32) {
    let spec = icon.spec();
    let k = rect.width() / spec.view_box;
    let tolerance = TOLERANCE_PX / painter.ctx().pixels_per_point();
    let round_cap = spec.linecap == Some(LineCap::Round);
    let round_join = spec.linejoin == Some(LineJoin::Round);
    for part in &spec.parts {
        for sub in part.path.flatten(k, tolerance) {
            let pts: Vec<Pos2> = sub
                .points
                .iter()
                .map(|&(x, y)| pos2(rect.left() + x, rect.top() + y))
                .collect();
            match part.paint {
                Paint::Stroke(width) => {
                    stroke(
                        painter,
                        &pts,
                        sub.closed,
                        (width * k).max(MIN_STROKE),
                        color,
                        round_cap,
                        round_join,
                    );
                }
                Paint::Fill => fill(painter, &pts, color),
            }
        }
    }
}

fn stroke(
    painter: &Painter,
    pts: &[Pos2],
    closed: bool,
    width: f32,
    color: Color32,
    round_cap: bool,
    round_join: bool,
) {
    let r = width / 2.0;
    if pts.len() == 1 {
        // A zero-length subpath draws a dot under round caps, nothing otherwise.
        if round_cap {
            painter.circle_filled(pts[0], r, color);
        }
        return;
    }
    let stroke = PathStroke::new(width, color);
    painter.add(Shape::Path(PathShape {
        points: pts.to_vec(),
        closed,
        fill: Color32::TRANSPARENT,
        stroke,
    }));
    if round_cap && !closed {
        painter.circle_filled(pts[0], r, color);
        painter.circle_filled(pts[pts.len() - 1], r, color);
    }
    if round_join {
        let n = pts.len();
        let range = if closed { 0..n } else { 1..n - 1 };
        for i in range {
            let (a, b, c) = (pts[(i + n - 1) % n], pts[i], pts[(i + 1) % n]);
            let (u, v) = (b - a, c - b);
            if u.length_sq() < 1e-9 || v.length_sq() < 1e-9 {
                continue;
            }
            let turn = u.normalized().dot(v.normalized()).clamp(-1.0, 1.0).acos();
            if turn > JOIN_TURN {
                painter.circle_filled(b, r, color);
            }
        }
    }
}

fn fill(painter: &Painter, pts: &[Pos2], color: Color32) {
    if pts.len() < 3 {
        return;
    }
    let xy: Vec<(f32, f32)> = pts.iter().map(|p| (p.x, p.y)).collect();
    if is_convex(&xy) {
        // egui prefers clockwise, which on screen is a positive area.
        let mut p = pts.to_vec();
        if area2(&xy) < 0.0 {
            p.reverse();
        }
        painter.add(Shape::convex_polygon(p, color, Stroke::NONE));
    } else {
        let mut mesh = Mesh::default();
        for &p in pts {
            mesh.colored_vertex(p, color);
        }
        for [a, b, c] in triangulate(&xy) {
            mesh.add_triangle(a as u32, b as u32, c as u32);
        }
        // Not anti-aliased at its edges; no icon needs it today (the filled ones are discs).
        painter.add(Shape::mesh(mesh));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::epaint::ClippedShape;
    use egui::{Context, RawInput};

    /// What painting one icon at 16 points adds to the frame.
    fn shapes_of(icon: Icon, ppp: f32) -> Vec<ClippedShape> {
        let ctx = Context::default();
        ctx.set_pixels_per_point(ppp);
        let out = ctx.run_ui(RawInput::default(), |ui| {
            paint(
                ui.painter(),
                Rect::from_min_size(pos2(10.0, 10.0), egui::vec2(16.0, 16.0)),
                icon,
                Color32::WHITE,
            );
        });
        out.shapes
    }

    #[test]
    fn every_icon_paints_something_inside_its_box() {
        for icon in Icon::ALL {
            let shapes = shapes_of(icon, 2.0);
            assert!(!shapes.is_empty(), "{} drew nothing", icon.id());
            for s in &shapes {
                let r = s.shape.visual_bounding_rect();
                // The box, plus half a stroke and the mitre's reach.
                assert!(
                    Rect::from_min_max(pos2(7.0, 7.0), pos2(29.0, 29.0)).contains_rect(r),
                    "{}: {r:?}",
                    icon.id()
                );
            }
        }
    }

    #[test]
    fn open_strokes_get_round_caps_and_dots_are_discs() {
        // The check mark is one open polyline: the path, a disc at each end and one at
        // the corner. The settings dots are three filled discs and nothing else.
        assert_eq!(shapes_of(Icon::Check, 2.0).len(), 4);
        assert_eq!(shapes_of(Icon::SettingsDots, 2.0).len(), 3);
    }

    #[test]
    fn a_bigger_icon_is_cut_finer() {
        let points = |ppp: f32| -> usize {
            shapes_of(Icon::Power, ppp)
                .iter()
                .map(|s| match &s.shape {
                    Shape::Path(p) => p.points.len(),
                    _ => 0,
                })
                .sum()
        };
        assert!(points(4.0) > points(1.0));
    }
}

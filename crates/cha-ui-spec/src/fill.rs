//! Filling flattened subpaths: a convexity test, and ear clipping for the
//! ones that are not convex, so a renderer whose fill only handles convex
//! polygons (egui's `PathShape`) can still draw any simple polygon.

type P = (f32, f32);

/// Twice the signed area. Positive is clockwise on a screen (y down).
pub fn area2(pts: &[P]) -> f32 {
    let mut a = 0.0;
    for (i, p) in pts.iter().enumerate() {
        let q = pts[(i + 1) % pts.len()];
        a += p.0 * q.1 - q.0 * p.1;
    }
    a
}

fn cross(o: P, a: P, b: P) -> f32 {
    (a.0 - o.0) * (b.1 - o.1) - (a.1 - o.1) * (b.0 - o.0)
}

/// Whether every turn goes the same way (collinear points are fine).
pub fn is_convex(pts: &[P]) -> bool {
    if pts.len() < 3 {
        return false;
    }
    let mut sign = 0.0f32;
    for i in 0..pts.len() {
        let c = cross(pts[i], pts[(i + 1) % pts.len()], pts[(i + 2) % pts.len()]);
        if c.abs() < 1e-6 {
            continue;
        }
        if sign == 0.0 {
            sign = c.signum();
        } else if c.signum() != sign {
            return false;
        }
    }
    true
}

/// Triangles (indices into `pts`) covering a simple polygon, by ear clipping.
/// Self-intersecting input gives a best effort, not a panic.
pub fn triangulate(pts: &[P]) -> Vec<[usize; 3]> {
    let n = pts.len();
    if n < 3 {
        return Vec::new();
    }
    let mut idx: Vec<usize> = (0..n).collect();
    if area2(pts) < 0.0 {
        idx.reverse();
    }
    let mut out = Vec::with_capacity(n - 2);
    while idx.len() > 3 {
        let m = idx.len();
        let ear = (0..m).find(|&i| {
            let (a, b, c) = (idx[(i + m - 1) % m], idx[i], idx[(i + 1) % m]);
            // Convex corner (clockwise like the polygon), and no other vertex inside.
            if cross(pts[a], pts[b], pts[c]) <= 0.0 {
                return false;
            }
            !idx.iter().any(|&v| {
                v != a
                    && v != b
                    && v != c
                    && cross(pts[a], pts[b], pts[v]) >= 0.0
                    && cross(pts[b], pts[c], pts[v]) >= 0.0
                    && cross(pts[c], pts[a], pts[v]) >= 0.0
            })
        });
        // No ear (degenerate input): clip the first corner anyway.
        let i = ear.unwrap_or(0);
        out.push([idx[(i + m - 1) % m], idx[i], idx[(i + 1) % m]]);
        idx.remove(i);
    }
    out.push([idx[0], idx[1], idx[2]]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn covered(pts: &[P], tris: &[[usize; 3]]) -> f32 {
        tris.iter()
            .map(|t| area2(&[pts[t[0]], pts[t[1]], pts[t[2]]]).abs() / 2.0)
            .sum()
    }

    #[test]
    fn convexity() {
        assert!(is_convex(&[(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0)]));
        // An L is not.
        assert!(!is_convex(&[
            (0.0, 0.0),
            (4.0, 0.0),
            (4.0, 2.0),
            (2.0, 2.0),
            (2.0, 4.0),
            (0.0, 4.0)
        ]));
        assert!(!is_convex(&[(0.0, 0.0), (1.0, 1.0)]));
    }

    #[test]
    fn an_l_shape_is_covered_exactly() {
        for flip in [false, true] {
            let mut l = vec![
                (0.0, 0.0),
                (4.0, 0.0),
                (4.0, 2.0),
                (2.0, 2.0),
                (2.0, 4.0),
                (0.0, 4.0),
            ];
            if flip {
                l.reverse();
            }
            let t = triangulate(&l);
            assert_eq!(t.len(), 4);
            assert!((covered(&l, &t) - 12.0).abs() < 1e-4);
        }
    }

    #[test]
    fn a_chevron_arrow_is_covered_exactly() {
        // Concave with a notch, like a filled arrow head.
        let p = [(0.0, 0.0), (8.0, 4.0), (0.0, 8.0), (3.0, 4.0)];
        let t = triangulate(&p);
        assert_eq!(t.len(), 2);
        assert!((covered(&p, &t) - 20.0).abs() < 1e-4);
    }
}

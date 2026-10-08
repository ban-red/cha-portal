//! SVG path data: a parser for `M m L l H h V v C c S s Q q T t A a Z z` (implicit
//! repeated commands, numbers like `.7.7` and `1e-3`, packed arc flags), and a
//! flattener that turns the result into polylines.
//!
//! Parsing normalises to absolute [`Seg`]s: smooth curves (`S`, `T`) get their
//! reflected control point filled in and `H`/`V` become lines. [`Path::flatten`]
//! then cuts curves and arcs into straight pieces whose distance from the true
//! curve stays under a tolerance, so the caller ties detail to the drawn size.

use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Seg {
    Move(f32, f32),
    Line(f32, f32),
    /// Control, control, end.
    Cubic([f32; 2], [f32; 2], [f32; 2]),
    /// Control, end.
    Quad([f32; 2], [f32; 2]),
    /// An elliptical arc in the spec's endpoint form, from the current point.
    Arc {
        rx: f32,
        ry: f32,
        /// Degrees.
        rotation: f32,
        large: bool,
        sweep: bool,
        to: [f32; 2],
    },
    Close,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError {
    pub at: usize,
    pub message: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} at byte {}", self.message, self.at)
    }
}

impl std::error::Error for ParseError {}

/// A parsed path.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Path {
    pub segs: Vec<Seg>,
}

/// One flattened subpath.
#[derive(Clone, Debug, PartialEq)]
pub struct Subpath {
    pub points: Vec<(f32, f32)>,
    /// Ended with `Z`. A closed subpath's last point is not repeated.
    pub closed: bool,
}

struct Scanner<'a> {
    s: &'a [u8],
    i: usize,
}

impl Scanner<'_> {
    fn err<T>(&self, message: impl Into<String>) -> Result<T, ParseError> {
        Err(ParseError {
            at: self.i,
            message: message.into(),
        })
    }

    fn skip_space(&mut self) {
        while self.i < self.s.len() && matches!(self.s[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }

    /// Whitespace and at most one comma.
    fn skip_sep(&mut self) {
        self.skip_space();
        if self.s.get(self.i) == Some(&b',') {
            self.i += 1;
            self.skip_space();
        }
    }

    fn at_number(&self) -> bool {
        matches!(self.s.get(self.i), Some(b'0'..=b'9' | b'.' | b'-' | b'+'))
    }

    fn digits(&mut self) -> usize {
        let from = self.i;
        while matches!(self.s.get(self.i), Some(b'0'..=b'9')) {
            self.i += 1;
        }
        self.i - from
    }

    fn number(&mut self) -> Result<f32, ParseError> {
        self.skip_sep();
        let start = self.i;
        if matches!(self.s.get(self.i), Some(b'+' | b'-')) {
            self.i += 1;
        }
        let mut n = self.digits();
        if self.s.get(self.i) == Some(&b'.') {
            self.i += 1;
            n += self.digits();
        }
        if n == 0 {
            self.i = start;
            return self.err("expected a number");
        }
        if matches!(self.s.get(self.i), Some(b'e' | b'E')) {
            let save = self.i;
            self.i += 1;
            if matches!(self.s.get(self.i), Some(b'+' | b'-')) {
                self.i += 1;
            }
            if self.digits() == 0 {
                self.i = save;
                return self.err("a number's exponent has no digits");
            }
        }
        let text = std::str::from_utf8(&self.s[start..self.i]).expect("ascii");
        match text.parse::<f32>() {
            Ok(v) if v.is_finite() => Ok(v),
            _ => {
                self.i = start;
                self.err(format!("{text:?} is not a number"))
            }
        }
    }

    fn flag(&mut self) -> Result<bool, ParseError> {
        self.skip_sep();
        match self.s.get(self.i) {
            Some(b'0') => {
                self.i += 1;
                Ok(false)
            }
            Some(b'1') => {
                self.i += 1;
                Ok(true)
            }
            _ => self.err("expected an arc flag, 0 or 1"),
        }
    }

    fn point(&mut self) -> Result<[f32; 2], ParseError> {
        Ok([self.number()?, self.number()?])
    }
}

impl Path {
    pub fn parse(d: &str) -> Result<Path, ParseError> {
        let mut sc = Scanner {
            s: d.as_bytes(),
            i: 0,
        };
        let mut segs = Vec::new();
        let mut cur = [0.0f32; 2];
        let mut start = [0.0f32; 2];
        // The previous curve's last control point, for S and T reflection.
        let mut last_cubic: Option<[f32; 2]> = None;
        let mut last_quad: Option<[f32; 2]> = None;
        let mut cmd: Option<u8> = None;

        loop {
            sc.skip_space();
            if sc.i >= sc.s.len() {
                break;
            }
            let c = sc.s[sc.i];
            if c.is_ascii_alphabetic() {
                if !b"MmLlHhVvCcSsQqTtAaZz".contains(&c) {
                    return sc.err(format!("unknown command {:?}", c as char));
                }
                sc.i += 1;
                cmd = Some(c);
                if matches!(c, b'Z' | b'z') {
                    segs.push(Seg::Close);
                    cur = start;
                    last_cubic = None;
                    last_quad = None;
                    // Numbers right after Z need a new command.
                    cmd = None;
                    continue;
                }
            } else if cmd.is_none() || !(sc.at_number() || c == b',') {
                return sc.err("expected a command");
            }
            let Some(c) = cmd else {
                return sc.err("expected a command");
            };
            if segs.is_empty() && !matches!(c, b'M' | b'm') {
                return sc.err("a path starts with M");
            }
            let rel = c.is_ascii_lowercase();
            let (ox, oy) = if rel { (cur[0], cur[1]) } else { (0.0, 0.0) };
            let mut new_cubic = None;
            let mut new_quad = None;
            match c.to_ascii_uppercase() {
                b'M' => {
                    let p = sc.point()?;
                    cur = [p[0] + ox, p[1] + oy];
                    start = cur;
                    segs.push(Seg::Move(cur[0], cur[1]));
                    // More pairs are implicit lines.
                    cmd = Some(if rel { b'l' } else { b'L' });
                }
                b'L' => {
                    let p = sc.point()?;
                    cur = [p[0] + ox, p[1] + oy];
                    segs.push(Seg::Line(cur[0], cur[1]));
                }
                b'H' => {
                    cur[0] = sc.number()? + ox;
                    segs.push(Seg::Line(cur[0], cur[1]));
                }
                b'V' => {
                    cur[1] = sc.number()? + oy;
                    segs.push(Seg::Line(cur[0], cur[1]));
                }
                b'C' => {
                    let a = sc.point()?;
                    let b = sc.point()?;
                    let e = sc.point()?;
                    let (a, b, e) = (
                        [a[0] + ox, a[1] + oy],
                        [b[0] + ox, b[1] + oy],
                        [e[0] + ox, e[1] + oy],
                    );
                    segs.push(Seg::Cubic(a, b, e));
                    new_cubic = Some(b);
                    cur = e;
                }
                b'S' => {
                    let b = sc.point()?;
                    let e = sc.point()?;
                    let (b, e) = ([b[0] + ox, b[1] + oy], [e[0] + ox, e[1] + oy]);
                    let a = match last_cubic {
                        Some(p) => [2.0 * cur[0] - p[0], 2.0 * cur[1] - p[1]],
                        None => cur,
                    };
                    segs.push(Seg::Cubic(a, b, e));
                    new_cubic = Some(b);
                    cur = e;
                }
                b'Q' => {
                    let a = sc.point()?;
                    let e = sc.point()?;
                    let (a, e) = ([a[0] + ox, a[1] + oy], [e[0] + ox, e[1] + oy]);
                    segs.push(Seg::Quad(a, e));
                    new_quad = Some(a);
                    cur = e;
                }
                b'T' => {
                    let e = sc.point()?;
                    let e = [e[0] + ox, e[1] + oy];
                    let a = match last_quad {
                        Some(p) => [2.0 * cur[0] - p[0], 2.0 * cur[1] - p[1]],
                        None => cur,
                    };
                    segs.push(Seg::Quad(a, e));
                    new_quad = Some(a);
                    cur = e;
                }
                b'A' => {
                    let rx = sc.number()?;
                    let ry = sc.number()?;
                    let rotation = sc.number()?;
                    let large = sc.flag()?;
                    let sweep = sc.flag()?;
                    let e = sc.point()?;
                    let to = [e[0] + ox, e[1] + oy];
                    segs.push(Seg::Arc {
                        rx,
                        ry,
                        rotation,
                        large,
                        sweep,
                        to,
                    });
                    cur = to;
                }
                _ => unreachable!("filtered above"),
            }
            last_cubic = new_cubic;
            last_quad = new_quad;
        }
        if segs.is_empty() {
            return Err(ParseError {
                at: 0,
                message: "empty path".into(),
            });
        }
        Ok(Path { segs })
    }

    /// Cut the path into polylines. `scale` is output units per path unit and
    /// `tolerance` the largest distance, in output units, a flattened curve may
    /// stray from the true one. Points come out in output units.
    pub fn flatten(&self, scale: f32, tolerance: f32) -> Vec<Subpath> {
        let tol = (tolerance / scale).max(1e-6);
        let mut out: Vec<Subpath> = Vec::new();
        let mut pts: Vec<[f32; 2]> = Vec::new();
        let mut cur = [0.0f32; 2];
        let mut start = [0.0f32; 2];

        for seg in &self.segs {
            match *seg {
                Seg::Move(x, y) => {
                    finish(&mut out, &mut pts, false, scale);
                    cur = [x, y];
                    start = cur;
                    pts.push(cur);
                }
                Seg::Line(x, y) => {
                    ensure_started(&mut pts, cur);
                    cur = [x, y];
                    pts.push(cur);
                }
                Seg::Cubic(a, b, e) => {
                    ensure_started(&mut pts, cur);
                    // Uniform steps err by at most |B''|max / 8n^2, and |B''| <= 6 dd.
                    let dd = len2(sub2(add2(cur, b), mul2(a, 2.0)))
                        .max(len2(sub2(add2(a, e), mul2(b, 2.0))));
                    let n = ((0.75 * dd / tol).sqrt().ceil() as usize).clamp(1, 256);
                    for i in 1..=n {
                        let t = i as f32 / n as f32;
                        let u = 1.0 - t;
                        let w = [u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t];
                        pts.push([
                            w[0] * cur[0] + w[1] * a[0] + w[2] * b[0] + w[3] * e[0],
                            w[0] * cur[1] + w[1] * a[1] + w[2] * b[1] + w[3] * e[1],
                        ]);
                    }
                    cur = e;
                }
                Seg::Quad(a, e) => {
                    ensure_started(&mut pts, cur);
                    let dd = len2(sub2(add2(cur, e), mul2(a, 2.0)));
                    let n = ((0.25 * dd / tol).sqrt().ceil() as usize).clamp(1, 256);
                    for i in 1..=n {
                        let t = i as f32 / n as f32;
                        let u = 1.0 - t;
                        pts.push([
                            u * u * cur[0] + 2.0 * u * t * a[0] + t * t * e[0],
                            u * u * cur[1] + 2.0 * u * t * a[1] + t * t * e[1],
                        ]);
                    }
                    cur = e;
                }
                Seg::Arc {
                    rx,
                    ry,
                    rotation,
                    large,
                    sweep,
                    to,
                } => {
                    ensure_started(&mut pts, cur);
                    let arc = Arc {
                        from: cur,
                        rx,
                        ry,
                        rotation,
                        large,
                        sweep,
                        to,
                    };
                    arc.points(tol, &mut pts);
                    cur = to;
                }
                Seg::Close => {
                    finish(&mut out, &mut pts, true, scale);
                    cur = start;
                }
            }
        }
        finish(&mut out, &mut pts, false, scale);
        out
    }
}

fn finish(out: &mut Vec<Subpath>, pts: &mut Vec<[f32; 2]>, closed: bool, scale: f32) {
    let mut v = std::mem::take(pts);
    if closed && v.len() > 1 && v.first() == v.last() {
        v.pop();
    }
    if v.len() > 1 || (v.len() == 1 && closed) {
        out.push(Subpath {
            points: v.iter().map(|p| (p[0] * scale, p[1] * scale)).collect(),
            closed,
        });
    }
}

/// After `Z`, a drawing command continues from the subpath's start.
fn ensure_started(pts: &mut Vec<[f32; 2]>, cur: [f32; 2]) {
    if pts.is_empty() {
        pts.push(cur);
    }
}

fn add2(a: [f32; 2], b: [f32; 2]) -> [f32; 2] {
    [a[0] + b[0], a[1] + b[1]]
}
fn sub2(a: [f32; 2], b: [f32; 2]) -> [f32; 2] {
    [a[0] - b[0], a[1] - b[1]]
}
fn mul2(a: [f32; 2], k: f32) -> [f32; 2] {
    [a[0] * k, a[1] * k]
}
fn len2(a: [f32; 2]) -> f32 {
    a[0].hypot(a[1])
}

struct Arc {
    from: [f32; 2],
    rx: f32,
    ry: f32,
    rotation: f32,
    large: bool,
    sweep: bool,
    to: [f32; 2],
}

impl Arc {
    /// SVG 1.1 implementation notes F.6.5 and F.6.6: endpoint form to centre
    /// form, then points along the arc, as many as the tolerance asks for.
    fn points(&self, tol: f32, pts: &mut Vec<[f32; 2]>) {
        if self.from == self.to {
            return;
        }
        let (mut rx, mut ry) = (self.rx.abs() as f64, self.ry.abs() as f64);
        if rx == 0.0 || ry == 0.0 {
            pts.push(self.to);
            return;
        }
        let phi = (self.rotation as f64).to_radians();
        let (sin_p, cos_p) = phi.sin_cos();
        let (x1, y1) = (self.from[0] as f64, self.from[1] as f64);
        let (x2, y2) = (self.to[0] as f64, self.to[1] as f64);
        let (dx, dy) = ((x1 - x2) / 2.0, (y1 - y2) / 2.0);
        let x1p = cos_p * dx + sin_p * dy;
        let y1p = -sin_p * dx + cos_p * dy;
        // Radii too small to span the endpoints are scaled up.
        let lambda = (x1p * x1p) / (rx * rx) + (y1p * y1p) / (ry * ry);
        if lambda > 1.0 {
            let s = lambda.sqrt();
            rx *= s;
            ry *= s;
        }
        let num = rx * rx * ry * ry - rx * rx * y1p * y1p - ry * ry * x1p * x1p;
        let den = rx * rx * y1p * y1p + ry * ry * x1p * x1p;
        let coef = if den == 0.0 {
            0.0
        } else {
            (num / den).max(0.0).sqrt() * if self.large == self.sweep { -1.0 } else { 1.0 }
        };
        let cxp = coef * (rx * y1p / ry);
        let cyp = coef * -(ry * x1p / rx);
        let cx = cos_p * cxp - sin_p * cyp + (x1 + x2) / 2.0;
        let cy = sin_p * cxp + cos_p * cyp + (y1 + y2) / 2.0;
        let angle =
            |ux: f64, uy: f64, vx: f64, vy: f64| (ux * vy - uy * vx).atan2(ux * vx + uy * vy);
        let ux = (x1p - cxp) / rx;
        let uy = (y1p - cyp) / ry;
        let vx = (-x1p - cxp) / rx;
        let vy = (-y1p - cyp) / ry;
        let theta1 = angle(1.0, 0.0, ux, uy);
        let mut dtheta = angle(ux, uy, vx, vy);
        let tau = std::f64::consts::TAU;
        if !self.sweep && dtheta > 0.0 {
            dtheta -= tau;
        } else if self.sweep && dtheta < 0.0 {
            dtheta += tau;
        }
        // A chord spanning angle s on radius r sags r (1 - cos(s / 2)).
        let r = rx.max(ry);
        let tol = tol as f64;
        let step = if tol >= r {
            std::f64::consts::PI
        } else {
            2.0 * (1.0 - tol / r).acos()
        };
        let n = ((dtheta.abs() / step).ceil() as usize).clamp(1, 512);
        for i in 1..=n {
            if i == n {
                pts.push(self.to);
                break;
            }
            let t = theta1 + dtheta * i as f64 / n as f64;
            let (st, ct) = t.sin_cos();
            let x = cos_p * rx * ct - sin_p * ry * st + cx;
            let y = sin_p * rx * ct + cos_p * ry * st + cy;
            pts.push([x as f32, y as f32]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(d: &str) -> Vec<Seg> {
        Path::parse(d).unwrap().segs
    }

    #[test]
    fn absolute_and_relative_lines() {
        assert_eq!(
            parse("M1 2L3 4 l1 1 H9 h-1 V0 v2z"),
            vec![
                Seg::Move(1.0, 2.0),
                Seg::Line(3.0, 4.0),
                Seg::Line(4.0, 5.0),
                Seg::Line(9.0, 5.0),
                Seg::Line(8.0, 5.0),
                Seg::Line(8.0, 0.0),
                Seg::Line(8.0, 2.0),
                Seg::Close,
            ]
        );
    }

    #[test]
    fn implicit_repeats() {
        // After M the extra pairs are lines (relative after m); after L, more L.
        assert_eq!(
            parse("m1 1 2 2 3 3"),
            vec![
                Seg::Move(1.0, 1.0),
                Seg::Line(3.0, 3.0),
                Seg::Line(6.0, 6.0)
            ]
        );
        assert_eq!(
            parse("M0 0L1 1 2 2"),
            vec![
                Seg::Move(0.0, 0.0),
                Seg::Line(1.0, 1.0),
                Seg::Line(2.0, 2.0)
            ]
        );
        assert_eq!(parse("M8 4.5v2.5").len(), 2);
    }

    #[test]
    fn numbers_without_separators() {
        // ".7.7" is 0.7 and 0.7; "-" starts a number; exponents.
        assert_eq!(parse("M.7.7"), vec![Seg::Move(0.7, 0.7)]);
        assert_eq!(parse("M1-2"), vec![Seg::Move(1.0, -2.0)]);
        assert_eq!(parse("M1e-3 2E2"), vec![Seg::Move(0.001, 200.0)]);
        assert_eq!(parse("M1,2L3,4")[1], Seg::Line(3.0, 4.0));
        assert_eq!(parse("M10.5.5"), vec![Seg::Move(10.5, 0.5)]);
        assert_eq!(parse("M+1 +2"), vec![Seg::Move(1.0, 2.0)]);
    }

    #[test]
    fn arc_flags_may_be_packed() {
        let s = parse("M0 0a.7.7 0 0 0-.7.7");
        assert!(
            matches!(s[1], Seg::Arc { large: false, sweep: false, to, .. } if to == [-0.7, 0.7])
        );
        let s = parse("M0 0a1 1 0 011 1");
        assert!(matches!(s[1], Seg::Arc { large: false, sweep: true, to, .. } if to == [1.0, 1.0]));
        let s = parse("M4.6 3.9a5.5 5.5 0 1 0 6.8 0");
        assert!(
            matches!(s[1], Seg::Arc { large: true, sweep: false, to, .. } if (to[0] - 11.4).abs() < 1e-5)
        );
    }

    #[test]
    fn smooth_curves_reflect_the_last_control_point() {
        let s = parse("M0 0C1 1 2 1 3 0S5 -1 6 0");
        assert_eq!(s[2], Seg::Cubic([4.0, -1.0], [5.0, -1.0], [6.0, 0.0]));
        // Without a previous curve the first control point is the current point.
        let s = parse("M0 0L1 0S2 1 3 0");
        assert_eq!(s[2], Seg::Cubic([1.0, 0.0], [2.0, 1.0], [3.0, 0.0]));
        let s = parse("M0 0Q1 1 2 0T4 0");
        assert_eq!(s[2], Seg::Quad([3.0, -1.0], [4.0, 0.0]));
        let s = parse("M0 0s1 1 2 0");
        assert_eq!(s[1], Seg::Cubic([0.0, 0.0], [1.0, 1.0], [2.0, 0.0]));
    }

    #[test]
    fn relative_commands_are_from_the_current_point() {
        let s = parse("M1 1c1 0 2 1 3 3");
        assert_eq!(s[1], Seg::Cubic([2.0, 1.0], [3.0, 2.0], [4.0, 4.0]));
        // After z the current point is the subpath's start.
        let s = parse("M2 2l3 0 0 3zl1 1");
        assert_eq!(s[4], Seg::Line(3.0, 3.0));
    }

    #[test]
    fn bad_paths_say_where() {
        for d in [
            "",
            "L1 1",
            "M1",
            "M1 2 X",
            "M1 2 L",
            "M1 2 3z4",
            "M0 0a1 1 0 2 0 1 1",
            "M1e 2",
        ] {
            assert!(Path::parse(d).is_err(), "{d:?} should not parse");
        }
        let e = Path::parse("M1 2 X").unwrap_err();
        assert_eq!(e.at, 5);
    }

    fn on_circle(d: &str, c: (f32, f32), r: f32, scale: f32, tol: f32) {
        let subs = Path::parse(d).unwrap().flatten(scale, tol);
        assert!(!subs.is_empty());
        for s in &subs {
            // Vertices sit on the circle; a chord between two sags no more than `tol`.
            for w in s.points.windows(2) {
                let mid = ((w[0].0 + w[1].0) / 2.0, (w[0].1 + w[1].1) / 2.0);
                let dist = (mid.0 - c.0 * scale).hypot(mid.1 - c.1 * scale);
                assert!(
                    (dist - r * scale).abs() <= tol * 1.05,
                    "{dist} vs {}",
                    r * scale
                );
            }
            for p in &s.points {
                let dist = (p.0 - c.0 * scale).hypot(p.1 - c.1 * scale);
                assert!(
                    (dist - r * scale).abs() < 5e-3,
                    "vertex {p:?} off the circle: {dist} vs {}",
                    r * scale
                );
            }
        }
    }

    #[test]
    fn arcs_flatten_to_the_circle_within_the_tolerance() {
        for (scale, tol) in [(0.75, 0.05), (1.0, 0.05), (1.5, 0.05), (4.0, 0.02)] {
            on_circle(
                "M6.4 8a1.6 1.6 0 1 0 3.2 0a1.6 1.6 0 1 0-3.2 0z",
                (8.0, 8.0),
                1.6,
                scale,
                tol,
            );
            on_circle(
                "M4.6 3.9a5.5 5.5 0 1 0 6.8 0",
                (8.0, 3.9 + (5.5f32 * 5.5 - 3.4 * 3.4).sqrt()),
                5.5,
                scale,
                tol,
            );
        }
    }

    #[test]
    fn a_finer_tolerance_means_more_points() {
        let p = Path::parse("M2 8a6 6 0 0 1 12 0").unwrap();
        let coarse = p.flatten(1.0, 0.5)[0].points.len();
        let fine = p.flatten(1.0, 0.01)[0].points.len();
        assert!(fine > coarse * 2, "{coarse} vs {fine}");
        // Bigger on screen, more points too.
        assert!(p.flatten(4.0, 0.05)[0].points.len() > p.flatten(1.0, 0.05)[0].points.len());
    }

    #[test]
    fn arcs_pick_the_side_the_flags_say() {
        // Half circle from (0,0) to (2,0), radius 1: sweep 1 goes over the top (y < 0).
        let top = Path::parse("M0 0a1 1 0 0 1 2 0")
            .unwrap()
            .flatten(1.0, 0.01);
        assert!(top[0].points.iter().any(|p| p.1 < -0.9));
        let bottom = Path::parse("M0 0a1 1 0 0 0 2 0")
            .unwrap()
            .flatten(1.0, 0.01);
        assert!(bottom[0].points.iter().any(|p| p.1 > 0.9));
        // Radius 1 through (0,0) and (1,1): the large flag takes the long way.
        let small = Path::parse("M0 0a1 1 0 0 1 1 1")
            .unwrap()
            .flatten(1.0, 0.01);
        let large = Path::parse("M0 0a1 1 0 1 1 1 1")
            .unwrap()
            .flatten(1.0, 0.01);
        assert!(large[0].points.len() > small[0].points.len());
    }

    #[test]
    fn too_small_radii_are_scaled_up_and_zero_radii_are_lines() {
        let s = Path::parse("M0 0A1 1 0 0 1 10 0")
            .unwrap()
            .flatten(1.0, 0.01);
        assert_eq!(*s[0].points.last().unwrap(), (10.0, 0.0));
        assert!(s[0].points.iter().any(|p| p.1 < -4.9));
        let s = Path::parse("M0 0A0 5 0 0 1 10 0")
            .unwrap()
            .flatten(1.0, 0.01);
        assert_eq!(s[0].points, vec![(0.0, 0.0), (10.0, 0.0)]);
    }

    #[test]
    fn curves_end_where_they_say_and_subpaths_split() {
        let s = Path::parse("M0 0C0 4 4 4 4 0M10 10l2 2z")
            .unwrap()
            .flatten(2.0, 0.05);
        assert_eq!(s.len(), 2);
        assert_eq!(*s[0].points.last().unwrap(), (8.0, 0.0));
        assert!(!s[0].closed && s[1].closed);
        // The cubic peaks at y = 3 (6 in output units).
        let peak = s[0].points.iter().map(|p| p.1).fold(0.0f32, f32::max);
        assert!((peak - 6.0).abs() < 0.06, "{peak}");
    }

    #[test]
    fn a_closed_subpath_does_not_repeat_its_first_point() {
        let s = Path::parse("M0 0h4v4H0z").unwrap().flatten(1.0, 0.05);
        assert_eq!(s[0].points.len(), 4);
        let s = Path::parse("M0 0h4v4H0V0z").unwrap().flatten(1.0, 0.05);
        assert_eq!(s[0].points.len(), 4);
    }
}

//! How much of each pixel a layer's solid covers, 0 to 1: the anti-aliasing of the layer image.
//!
//! Two ways to the same number:
//! - [`half_plane`]: from a distance. Near the surface a solid is a half-plane; the share of a
//!   pixel on its inside follows in closed form from the edge's direction and its distance from
//!   the pixel's centre. A field part's pixels take this.
//! - [`Polygons`]: from outlines. Each edge adds its signed area to the cells it crosses and
//!   to the cell right after them; a running sum along a row then holds, at every cell, the
//!   share of it inside the outline (holes wind the other way and subtract). Exact for straight
//!   edges at any angle. A mesh part's slice contours take this.

/// The share of a unit pixel on the inside of a straight edge whose inside starts `s` pixels
/// from the pixel's centre along the edge's normal `(u, w)` (unit length; only the sizes of
/// its components matter): `s > 0` puts the centre inside.
pub fn half_plane(s: f32, u: f32, w: f32) -> f32 {
    let (a, b) = if u.abs() >= w.abs() { (u.abs(), w.abs()) } else { (w.abs(), u.abs()) };
    let h = 0.5 * (a + b);
    if s <= -h {
        return 0.0;
    }
    if s >= h {
        return 1.0;
    }
    // how far the edge has swept into the square from the corner it meets first
    let t = s + h;
    if b < 1.0e-6 {
        return t / a;
    }
    if t < b {
        t * t / (2.0 * a * b)
    } else if t <= a {
        (t - 0.5 * b) / a
    } else {
        let r = a + b - t;
        1.0 - r * r / (2.0 * a * b)
    }
}

/// Outlines drawn into a window of pixels by signed area: [`Polygons::edge`] each edge of
/// every loop (any winding: outer loops one way, holes the other), then [`Polygons::cover`].
pub struct Polygons {
    w: usize,
    h: usize,
    /// Per row, `w + 1` cells: what each cell adds to the running sum along the row.
    acc: Vec<f32>,
}

impl Polygons {
    /// An empty window `w` by `h` pixels.
    pub fn new(w: usize, h: usize) -> Polygons {
        Polygons { w, h, acc: vec![0.0; (w + 1) * h] }
    }

    /// The window empty again.
    pub fn clear(&mut self) {
        self.acc.fill(0.0);
    }

    /// The edge from `p0` to `p1`, in the window's pixel coordinates (pixel `(0, 0)` covers
    /// `[0, 1] × [0, 1]`).
    pub fn edge(&mut self, p0: [f64; 2], p1: [f64; 2]) {
        if p0[1] == p1[1] {
            return;
        }
        let dir = if p1[1] > p0[1] { 1.0 } else { -1.0 };
        let (a, b) = if p0[1] < p1[1] { (p0, p1) } else { (p1, p0) };
        let y_end = b[1].min(self.h as f64);
        let mut y = a[1].max(0.0);
        if y >= y_end {
            return;
        }
        let dxdy = (b[0] - a[0]) / (b[1] - a[1]);
        let mut row = y.floor() as usize;
        while y < y_end && row < self.h {
            let next = ((row + 1) as f64).min(y_end);
            let xs = a[0] + (y - a[1]) * dxdy;
            let xe = a[0] + (next - a[1]) * dxdy;
            self.row_piece(row, xs, xe, (next - y) * dir);
            y = next;
            row += 1;
        }
    }

    /// A piece of edge inside one row: from `xs` to `xe`, `dy` signed rows tall.
    fn row_piece(&mut self, row: usize, xs: f64, xe: f64, dy: f64) {
        let w = self.w;
        let cells = &mut self.acc[row * (w + 1)..(row + 1) * (w + 1)];
        let (lo, hi) = if xs <= xe { (xs, xe) } else { (xe, xs) };
        if hi <= 0.0 {
            // left of the window: all of the row to its right is on its far side
            cells[0] += dy as f32;
            return;
        }
        if lo >= w as f64 {
            return;
        }
        let mut cell = |cx: f64, dy: f64, fx: f64| {
            if cx < 0.0 {
                cells[0] += dy as f32;
            } else if cx < w as f64 {
                let c = cx as usize;
                cells[c] += (dy * (1.0 - fx)) as f32;
                cells[c + 1] += (dy * fx) as f32;
            }
        };
        let c0 = lo.floor();
        if hi - c0 <= 1.0 {
            cell(c0, dy, 0.5 * (lo + hi) - c0);
            return;
        }
        // split where it crosses a column
        let span = hi - lo;
        let mut x = lo;
        while x < hi {
            let cx = x.floor();
            let xn = (cx + 1.0).min(hi);
            cell(cx, dy * (xn - x) / span, 0.5 * (x + xn) - cx);
            x = xn;
        }
    }

    /// Each pixel's covered share, taken into `cov` where it is more (row-major, the window's
    /// size).
    pub fn cover(&self, cov: &mut [f32]) {
        let w = self.w;
        for row in 0..self.h {
            let cells = &self.acc[row * (w + 1)..row * (w + 1) + w];
            let out = &mut cov[row * w..(row + 1) * w];
            let mut sum = 0.0f32;
            for (o, c) in out.iter_mut().zip(cells) {
                sum += c;
                let v = sum.abs().min(1.0);
                if v > *o {
                    *o = v;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A half-plane's share: a half at the edge, the right ends, and the pixel's area in the
    /// mean over every offset, at any angle.
    #[test]
    fn a_half_planes_share_is_the_area_inside() {
        for deg in [0.0f32, 10.0, 30.0, 45.0, 60.0, 90.0, 135.0] {
            let (u, w) = (deg.to_radians().cos(), deg.to_radians().sin());
            assert!((half_plane(0.0, u, w) - 0.5).abs() < 1e-6, "{deg}°: the centre on the edge");
            assert_eq!(half_plane(0.75, u, w), 1.0, "{deg}°");
            assert_eq!(half_plane(-0.75, u, w), 0.0, "{deg}°");
            // integrated over s, the share is the pixel's whole area: no bias
            let n = 4000;
            let sum: f32 = (0..n).map(|k| half_plane(-1.0 + 2.0 * (k as f32 + 0.5) / n as f32, u, w)).sum::<f32>() * 2.0 / n as f32;
            assert!((sum - 1.0).abs() < 1e-3, "{deg}°: {sum}");
            // monotone
            let mut last = 0.0;
            for k in 0..=100 {
                let v = half_plane(-1.0 + k as f32 * 0.02, u, w);
                assert!(v >= last - 1e-6, "{deg}°");
                last = v;
            }
        }
        // a 45° edge cutting off a corner with legs of a half: an eighth of the pixel
        let d = std::f32::consts::FRAC_1_SQRT_2;
        assert!((half_plane(0.5 * d - d, d, d) - 0.125).abs() < 1e-5);
    }

    /// A square and a square with a square hole: every pixel's share is its exact area inside,
    /// edges off the grid and at an angle alike.
    #[test]
    fn outlines_cover_their_exact_area() {
        let mut p = Polygons::new(20, 20);
        let ring = |p: &mut Polygons, pts: &[[f64; 2]]| {
            for k in 0..pts.len() {
                p.edge(pts[k], pts[(k + 1) % pts.len()]);
            }
        };
        ring(&mut p, &[[2.25, 3.5], [12.75, 3.5], [12.75, 9.5], [2.25, 9.5]]);
        let mut cov = vec![0.0; 400];
        p.cover(&mut cov);
        let total: f32 = cov.iter().sum();
        assert!((total - 10.5 * 6.0).abs() < 1e-3, "{total}");
        assert!((cov[3 * 20 + 2] - 0.75 * 0.5).abs() < 1e-5, "a corner pixel: {}", cov[3 * 20 + 2]);
        assert_eq!(cov[5 * 20 + 5], 1.0);
        assert_eq!(cov[5 * 20 + 13], 0.0);
        // a hole, wound the other way, and a diamond, off the window's left edge
        p.clear();
        ring(&mut p, &[[1.0, 1.0], [19.0, 1.0], [19.0, 19.0], [1.0, 19.0]]);
        ring(&mut p, &[[5.5, 5.5], [5.5, 14.5], [14.5, 14.5], [14.5, 5.5]]);
        let mut cov = vec![0.0; 400];
        p.cover(&mut cov);
        let total: f32 = cov.iter().sum();
        assert!((total - (18.0 * 18.0 - 9.0 * 9.0)).abs() < 1e-3, "{total}");
        let mut p = Polygons::new(20, 20);
        ring(&mut p, &[[0.0, 10.0], [7.0, 3.0], [14.0, 10.0], [7.0, 17.0]].map(|q| [q[0] - 3.0, q[1]]));
        let mut cov = vec![0.0; 400];
        p.cover(&mut cov);
        let total: f32 = cov.iter().sum();
        // the diamond's area 98 less the part left of the window: a triangle of 3 x 6
        assert!((total - (98.0 - 9.0)).abs() < 1e-3, "{total}");
    }
}

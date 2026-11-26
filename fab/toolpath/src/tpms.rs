//! Plane sections of periodic infill surfaces. Field coordinates are mm from the object pivot.

use crate::clipper::{Clipper, Pieces, FREE};
use crate::warp;
use makepad_fab_slicer::geom::*;
use std::f64::consts::TAU;

/// Full period of Schwarz D in units of spacing / density. Its raw section length in a
/// unit-period square averaged 3.04666 over 192 midpoint Z samples, on a 192² cell grid.
pub const DIAMOND_PERIOD: f64 = 3.0467;
/// Full period of Fischer-Koch S in the same units. The same unit-square contour
/// measurement, before simplification, wall joins or loop filtering, gave 4.27176.
pub const FISCHER_KOCH_PERIOD: f64 = 4.2718;

#[derive(Clone, Copy, Debug)]
pub enum Tpms {
    Gyroid,
    Diamond,
    FischerKoch,
}

#[derive(Clone, Copy)]
struct Wave {
    sin: f64,
    cos: f64,
    cos2: f64,
}

impl Wave {
    fn new(angle: f64) -> Wave {
        let (sin, cos) = angle.sin_cos();
        Wave { sin, cos, cos2: cos * cos - sin * sin }
    }
}

/// The coefficients of sin X, cos X, cos 2X and a constant for a row at fixed Y and Z.
struct Row {
    sin_x: f64,
    cos_x: f64,
    cos_2x: f64,
    constant: f64,
}

impl Row {
    fn at(&self, x: Wave) -> f64 {
        self.sin_x * x.sin + self.cos_x * x.cos + self.cos_2x * x.cos2 + self.constant
    }
}

impl Tpms {
    fn coefficients(self, y: Wave, z: Wave) -> Row {
        match self {
            Tpms::Gyroid => Row { sin_x: y.cos, cos_x: z.sin, cos_2x: 0.0, constant: y.sin * z.cos },
            Tpms::Diamond => Row { sin_x: y.sin * z.sin + y.cos * z.cos, cos_x: y.sin * z.cos + y.cos * z.sin, cos_2x: 0.0, constant: 0.0 },
            Tpms::FischerKoch => Row { sin_x: y.cos * z.cos2, cos_x: y.cos2 * z.sin, cos_2x: y.sin * z.cos, constant: 0.0 },
        }
    }

    fn value(self, x: Wave, y: Wave, z: Wave) -> f64 {
        self.coefficients(y, z).at(x)
    }

    fn cells(self) -> f64 {
        match self {
            Tpms::Gyroid => 32.0,
            Tpms::Diamond => 24.0,
            Tpms::FischerKoch => 40.0,
        }
    }

    /// Contour the uniform surface at print height `z` and full period `period`, both in mm.
    pub fn contours(self, clip: &mut Clipper, pivot: Pt, z: f64, period: f64) -> (Pieces, Vec<Vec<Pt>>) {
        let k = TAU / period;
        contour(clip, pivot, period / self.cells(), &Uniform { surface: self, k, z: Wave::new(k * z) })
    }

    /// Resolve the shortest local wavelength using the guarded warp's largest stretch.
    pub fn warped_contours(self, clip: &mut Clipper, pivot: Pt, z: f64, period: f64, layer: &warp::Layer, stretch: f64) -> (Pieces, Vec<Vec<Pt>>) {
        let k = TAU / period;
        let field = Warped { surface: self, k, z: Wave::new(k * z), layer, pivot_mm: (pivot.x_mm(), pivot.y_mm()) };
        contour(clip, pivot, period / (self.cells() * stretch.max(1.0)), &field)
    }
}

/// A warped layer can keep just x for each column and evaluate (x, y) jointly in each row.
pub(crate) trait LayerField {
    type Column;
    fn column(&self, x: f64) -> Self::Column;
    fn row(&self, columns: &[Self::Column], y: f64, values: &mut [f64]);
    fn point(&self, x: f64, y: f64) -> f64;
}

struct Uniform {
    surface: Tpms,
    k: f64,
    z: Wave,
}

impl LayerField for Uniform {
    type Column = Wave;

    fn column(&self, x: f64) -> Wave {
        Wave::new(self.k * x)
    }

    fn row(&self, columns: &[Wave], y: f64, values: &mut [f64]) {
        let row = self.surface.coefficients(Wave::new(self.k * y), self.z);
        for (v, &x) in values.iter_mut().zip(columns) {
            *v = row.at(x);
        }
    }

    fn point(&self, x: f64, y: f64) -> f64 {
        self.surface.value(self.column(x), Wave::new(self.k * y), self.z)
    }
}

struct Warped<'a> {
    surface: Tpms,
    k: f64,
    z: Wave,
    layer: &'a warp::Layer,
    pivot_mm: (f64, f64),
}

impl LayerField for Warped<'_> {
    type Column = f64;

    fn column(&self, x: f64) -> f64 { x }

    fn row(&self, columns: &[f64], y: f64, values: &mut [f64]) {
        for (v, &x) in values.iter_mut().zip(columns) {
            *v = self.point(x, y);
        }
    }

    fn point(&self, x: f64, y: f64) -> f64 {
        let [vx, vy] = self.layer.at(x + self.pivot_mm.0, y + self.pivot_mm.1);
        self.surface.value(Wave::new(self.k * (x - vx)), Wave::new(self.k * (y - vy)), self.z)
    }
}

// Only sign-changing edges get ids. Adjacent cells share the same id even if two
// distinct edges cross at the same grid vertex after rounding.
const NONE: usize = usize::MAX;

struct Crossing {
    pt: Pt,
    links: [usize; 2],
}

/// An interpolated zero on a bracketed segment, refined once against the true field.
fn zero(field: &impl LayerField, a: (f64, f64), b: (f64, f64), va: f64, vb: f64) -> Pt {
    if va == 0.0 { return Pt::from_mm(a.0, a.1); }
    if vb == 0.0 { return Pt::from_mm(b.0, b.1); }
    let at = |t: f64| (a.0 + t * (b.0 - a.0), a.1 + t * (b.1 - a.1));
    let t = va / (va - vb);
    let (x, y) = at(t);
    let v = field.point(x, y);
    // One more secant through the two samples still bracketing zero.
    let t = if v == 0.0 { t } else if (v >= 0.0) == (va >= 0.0) {
        t + (1.0 - t) * v / (v - vb)
    } else {
        t * va / (va - v)
    };
    let (x, y) = at(t);
    Pt::from_mm(x, y)
}

fn crossing(field: &impl LayerField, pivot: Pt, nodes: &mut Vec<Crossing>, a: (f64, f64), b: (f64, f64), va: f64, vb: f64) -> usize {
    if (va >= 0.0) == (vb >= 0.0) {
        return NONE;
    }
    let id = nodes.len();
    nodes.push(Crossing { pt: pivot.add(zero(field, a, b, va, vb)), links: [NONE; 2] });
    id
}

fn connect(nodes: &mut [Crossing], a: usize, b: usize) {
    for (id, other) in [(a, b), (b, a)] {
        let links = &mut nodes[id].links;
        let slot = if links[0] == NONE { 0 } else { 1 };
        debug_assert_eq!(links[slot], NONE);
        links[slot] = other;
    }
}

/// Clipping creates vertices on chords. At a wall, the 0.01 mm simplification alone
/// can put such a vertex more than 0.005 mm off the surface. Resample only chords
/// whose diameter disk meets a wall, bracketing the surface across the chord normal.
/// Keep straight wall approaches compact too.
fn wall_chords(clip: &Clipper, field: &impl LayerField, pivot: Pt, h: f64, path: &[Pt]) -> Vec<Pt> {
    let mut out = Vec::with_capacity(path.len());
    let mut samples = Vec::new();
    for w in path.windows(2) {
        let (a, b) = (w[0], w[1]);
        out.push(a);
        let len = a.dist(b);
        let steps = (len / (h * SCALE / 4.0)).ceil() as usize;
        let radius = (len * 0.5).ceil() as i64 + 1;
        if steps < 2 || clip.boundary_within(lerp(a, b, 0.5), radius) >= radius as f64 {
            continue;
        }
        let d = b.sub(a);
        let (nx, ny) = (-d.y as f64 / len * h / 4.0, d.x as f64 / len * h / 4.0);
        samples.clear();
        samples.push(a);
        for i in 1..steps {
            let m = lerp(a, b, i as f64 / steps as f64).sub(pivot);
            let (x, y) = (m.x_mm(), m.y_mm());
            let (lo, hi) = ((x - nx, y - ny), (x + nx, y + ny));
            let (vl, vh) = (field.point(lo.0, lo.1), field.point(hi.0, hi.1));
            if (vl >= 0.0) != (vh >= 0.0) {
                samples.push(pivot.add(zero(field, lo, hi, vl, vh)));
            }
        }
        samples.push(b);
        let refined = simplify_polyline(&samples, scaled(0.001) as f64);
        out.extend_from_slice(&refined[1..refined.len() - 1]);
    }
    out.extend(path.last());
    out
}

/// Simplified closed paths include their first vertex again. Clip from outside whenever
/// possible so the clipper assigns a wall edge to both ends of each piece.
fn clip_loop(clip: &mut Clipper, mut line: Vec<Pt>, pieces: &mut Pieces, loops: &mut Vec<Vec<Pt>>) {
    line.pop();
    if line.len() < 3 {
        return;
    }
    if let Some(i) = line.iter().position(|&p| !clip.contains(p)) {
        line.rotate_left(i);
        line.push(line[0]);
        clip.clip(&line, pieces);
        return;
    }
    // Even with every vertex inside, a chord can cut across a hole or a concave wall.
    line.push(line[0]);
    let mut cuts = Pieces::default();
    clip.clip(&line, &mut cuts);
    if cuts.is_empty() {
        return;
    }
    let last = cuts.len() - 1;
    if cuts.ea[0] == FREE && cuts.eb[last] == FREE {
        if last == 0 {
            line.pop();
            loops.push(line);
        } else {
            let mut seam = cuts.piece(last).to_vec();
            seam.extend_from_slice(&cuts.piece(0)[1..]);
            pieces.push(&seam, cuts.ea[last], cuts.eb[0]);
            for i in 1..last {
                pieces.push(cuts.piece(i), cuts.ea[i], cuts.eb[i]);
            }
        }
    } else {
        pieces.append(&cuts);
    }
}

/// March the grid in the pivot frame. Row buffers hold two rows of samples and the
/// shared edge ids between them; the graph retains only edges that have crossings.
pub(crate) fn contour(clip: &mut Clipper, pivot: Pt, h: f64, field: &impl LayerField) -> (Pieces, Vec<Vec<Pt>>) {
    let (mut pieces, mut loops) = (Pieces::default(), Vec::new());
    let bb = clip.bbox;
    if bb.is_empty() {
        return (pieces, loops);
    }
    let bounds = |lo, hi, p| ((unscaled(lo - p) / h).floor() as i64 - 1)..=((unscaled(hi - p) / h).ceil() as i64 + 1);
    let xs: Vec<_> = bounds(bb.min.x, bb.max.x, pivot.x).map(|i| i as f64 * h).collect();
    let columns: Vec<_> = xs.iter().map(|&x| field.column(x)).collect();
    let mut ys = bounds(bb.min.y, bb.max.y, pivot.y).map(|j| j as f64 * h);
    let mut y0 = ys.next().unwrap();
    let mut nodes = Vec::new();
    let mut lower = vec![0.0; xs.len()];
    let mut upper = lower.clone();
    let horizontal = |nodes: &mut Vec<Crossing>, y, values: &[f64], ids: &mut [usize]| {
        for (i, id) in ids.iter_mut().enumerate() {
            *id = crossing(field, pivot, nodes, (xs[i], y), (xs[i + 1], y), values[i], values[i + 1]);
        }
    };
    let mut bottom = vec![NONE; xs.len() - 1];
    let mut top = bottom.clone();
    let mut sides = vec![NONE; xs.len()];
    field.row(&columns, y0, &mut lower);
    horizontal(&mut nodes, y0, &lower, &mut bottom);
    for y in ys {
        field.row(&columns, y, &mut upper);
        for (i, &x) in xs.iter().enumerate() {
            sides[i] = crossing(field, pivot, &mut nodes, (x, y0), (x, y), lower[i], upper[i]);
        }
        horizontal(&mut nodes, y, &upper, &mut top);
        for i in 0..bottom.len() {
            let edges = [bottom[i], sides[i + 1], top[i], sides[i]];
            match edges {
                [NONE, NONE, NONE, NONE] => {}
                [a, b, NONE, NONE] | [a, NONE, b, NONE] | [a, NONE, NONE, b] |
                [NONE, a, b, NONE] | [NONE, a, NONE, b] | [NONE, NONE, a, b] => connect(&mut nodes, a, b),
                [b, r, t, l] => {
                    // Equal centre and lower-left signs: cut off lower-right and upper-left.
                    let centre = lower[i] + lower[i + 1] + upper[i] + upper[i + 1];
                    if (centre >= 0.0) == (lower[i] >= 0.0) {
                        connect(&mut nodes, b, r);
                        connect(&mut nodes, t, l);
                    } else {
                        connect(&mut nodes, b, l);
                        connect(&mut nodes, t, r);
                    }
                }
            }
        }
        y0 = y;
        std::mem::swap(&mut lower, &mut upper);
        std::mem::swap(&mut bottom, &mut top);
    }

    let mut line = Vec::new();
    // Walk the degree-one border ends first. Every remaining component is a cycle.
    for closed in [false, true] {
        for start in 0..nodes.len() {
            if nodes[start].links[0] == NONE || (!closed && nodes[start].links[1] != NONE) {
                continue;
            }
            line.clear();
            let (mut prev, mut id) = (NONE, start);
            loop {
                line.push(nodes[id].pt);
                let [a, b] = nodes[id].links;
                nodes[id].links = [NONE; 2];
                let next = if a == prev { b } else { a };
                if next == NONE || next == start {
                    if next == start {
                        line.push(nodes[start].pt);
                    }
                    break;
                }
                prev = id;
                id = next;
            }
            line.dedup();
            let simplified = simplify_polyline(&line, scaled(0.01) as f64);
            let path = wall_chords(clip, field, pivot, h, &simplified);
            if closed {
                clip_loop(clip, path, &mut pieces, &mut loops);
            } else {
                clip.clip(&path, &mut pieces);
            }
        }
    }
    (pieces, loops)
}

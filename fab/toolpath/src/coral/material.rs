//! A support layer is its paths (C1): what it prints, M_l, is their capsules, each centreline
//! swept by a disc of half its width. Demand, footprints and contact outlines are kept apart and
//! are never material; whatever is said about a support layer is said from its paths.

use crate::ExtrusionPath;
use makepad_fab_slicer::clip::union_ex;
use makepad_fab_slicer::geom::*;

/// What a path is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Role {
    /// The first layer, on the bed.
    Pad,
    /// The support between its footing and its interface.
    Body,
    /// The last layers under a contact, the one at the top gap under the part included.
    Interface,
}

/// One printed line of a support layer.
#[derive(Clone, Debug, PartialEq)]
pub struct SupportPath {
    /// Its centreline; a closed one ends where it starts.
    pub pts: Vec<Pt>,
    /// Its width (mm).
    pub width: f64,
    pub role: Role,
    /// The support it is part of.
    pub support: usize,
    /// Which of that support's fronds it is.
    pub frond: usize,
}

/// How far a capsule's chords lie inside its arcs at most (units): material only ever comes out
/// a little smaller than its paths print.
const ARC: f64 = 1000.0;

impl SupportPath {
    /// Its segments, `(a, b)`; a lone point is one segment from it to itself.
    pub fn segments(&self) -> impl Iterator<Item = (Pt, Pt)> + '_ {
        let lone = (self.pts.len() == 1).then(|| (self.pts[0], self.pts[0]));
        self.pts.windows(2).map(|w| (w[0], w[1])).chain(lone)
    }
}

/// The band of radius `r` (units) round segment `a`-`b`: two half discs joined by its sides, their
/// arcs cut by chords inside them. A lone point gives its disc.
pub fn capsule(a: Pt, b: Pt, r: f64) -> Polygon {
    let steps = if r <= ARC { 2 } else { (std::f64::consts::PI / (2.0 * (1.0 - ARC / r).acos())).ceil().max(2.0) as usize };
    let (dx, dy) = ((b.x - a.x) as f64, (b.y - a.y) as f64);
    let len = dx.hypot(dy);
    let start = if len > 0.0 { dy.atan2(dx) - std::f64::consts::FRAC_PI_2 } else { 0.0 };
    let mut pts = Vec::with_capacity(2 * steps + 2);
    // Counter-clockwise: round `b` from its right side to its left, then round `a` back.
    for (c, from) in [(b, start), (a, start + std::f64::consts::PI)] {
        for k in 0..=steps {
            let t = from + std::f64::consts::PI * k as f64 / steps as f64;
            pts.push(Pt::new(c.x + (r * t.cos()).round() as i64, c.y + (r * t.sin()).round() as i64));
        }
    }
    pts.dedup();
    if pts.len() > 1 && pts.first() == pts.last() {
        pts.pop();
    }
    Polygon::new(pts)
}

/// What `paths` print, M_l: the union of their capsules.
pub fn material(paths: &[SupportPath]) -> Vec<ExPolygon> {
    grown(paths, 0.0)
}

/// What of `paths` prints in any of `boxes`, and more: the union of the capsules that come into
/// one, which is all of their material in the boxes.
pub fn material_in(paths: &[SupportPath], boxes: &[BBox]) -> Vec<ExPolygon> {
    let mut all = BBox::empty();
    boxes.iter().for_each(|b| {
        all.add(b.min);
        all.add(b.max);
    });
    let bands: Vec<ExPolygon> = paths.iter().flat_map(|p| {
        let r = p.width / 2.0 * SCALE;
        let reach = r.ceil() as i64;
        p.segments()
            .filter(move |&(a, b)| {
                let bb = BBox { min: Pt::new(a.x.min(b.x) - reach, a.y.min(b.y) - reach), max: Pt::new(a.x.max(b.x) + reach, a.y.max(b.y) + reach) };
                bb.overlaps(&all) && boxes.iter().any(|x| x.overlaps(&bb))
            })
            .map(move |(a, b)| ExPolygon::new(capsule(a, b, r)))
    }).collect();
    if bands.is_empty() { Vec::new() } else { union_ex(&bands) }
}

/// `paths`' material grown by `by` (mm): each centreline swept by a disc of half its width and
/// `by`.
pub fn grown(paths: &[SupportPath], by: f64) -> Vec<ExPolygon> {
    let bands: Vec<ExPolygon> = paths.iter().flat_map(|p| {
        let r = (p.width / 2.0 + by) * SCALE;
        p.segments().map(move |(a, b)| ExPolygon::new(capsule(a, b, r)))
    }).collect();
    if bands.is_empty() { Vec::new() } else { union_ex(&bands) }
}

/// What a support layer `paths`, made as extrusion, prints as paths of `role`: each run of a path
/// that extrudes, a closed loop closed. The tree's own output, to be told by the same contracts.
pub fn of_extrusion(paths: &[ExtrusionPath], role: impl Fn(&ExtrusionPath) -> Role, support: usize) -> Vec<SupportPath> {
    let mut out = Vec::new();
    for (frond, e) in paths.iter().enumerate() {
        let mut pts = e.pts.clone();
        let mut dry = e.dry.clone();
        if e.closed && pts.len() > 2 && pts.first() != pts.last() {
            pts.push(pts[0]);
            if !dry.is_empty() {
                dry.push(false);
            }
        }
        let mut run: Vec<Pt> = Vec::new();
        for i in 0..pts.len() {
            if i > 0 && dry.get(i - 1).copied().unwrap_or(false) {
                if run.len() > 1 {
                    out.push(SupportPath { pts: std::mem::take(&mut run), width: e.width, role: role(e), support, frond });
                }
                run.clear();
            }
            if run.last() != Some(&pts[i]) {
                run.push(pts[i]);
            }
        }
        if run.len() > 1 || e.pts.len() == 1 {
            out.push(SupportPath { pts: run, width: e.width, role: role(e), support, frond });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(pts: &[(f64, f64)]) -> SupportPath {
        SupportPath { pts: pts.iter().map(|&(x, y)| Pt::from_mm(x, y)).collect(), width: 0.4, role: Role::Body, support: 0, frond: 0 }
    }

    /// A line prints its length by its width and a bead's disc; a point its disc; two lines over
    /// each other print once; and nothing prints nothing.
    #[test]
    fn material_is_what_the_paths_sweep() {
        let disc = std::f64::consts::PI * 0.2 * 0.2;
        let line = total_area_mm2(&material(&[path(&[(0.0, 0.0), (10.0, 0.0)])]));
        assert!((line - (4.0 + disc)).abs() < 2e-3, "a line: {line} mm2");
        let dot = total_area_mm2(&material(&[path(&[(5.0, 5.0)])]));
        assert!((dot - disc).abs() < 2e-3, "a point: {dot} mm2");
        let twice = total_area_mm2(&material(&[path(&[(0.0, 0.0), (10.0, 0.0)]), path(&[(0.0, 0.0), (5.0, 0.0), (10.0, 0.0)])]));
        assert!((twice - line).abs() < 2e-3, "a line printed twice: {twice} mm2");
        let wide = total_area_mm2(&grown(&[path(&[(0.0, 0.0), (10.0, 0.0)])], 0.3));
        assert!((wide - (10.0 + std::f64::consts::PI * 0.25)).abs() < 5e-3, "grown by 0.3 mm: {wide} mm2");
        assert!(material(&[]).is_empty());
        assert!(capsule(Pt::new(0, 0), Pt::new(1000, 0), 1e5).is_ccw());
    }
}

//! Beads against non-printed inserts, in printer millimetres.
//!
//! O(insert triangles + layers spanned x section edges + extrusion moves within the insert's
//! bbox per layer); runs once per slice on the slice or report worker, never per frame.
//! The triangle z-range index is built once; sections and their segment grids are cached.

use crate::{Footprint, Issue, Severity};
use fab_plate::{Bounds, Insert, Part, PartMesh};
use makepad_fab_gcode::record::{flags, MoveKind, MoveRecord};
use makepad_fab_slicer::{geom::{BBox, Pt, SCALE}, grid::SegGrid};
use std::collections::{BTreeMap, HashMap};

/// One error per insert per run of consecutive hit layers. An empty insert list does not
/// consume records. Travels and extruder-only moves do not deposit a bead.
pub fn bead_issues(inserts: &[(&Part, &Insert)], records: &mut dyn Iterator<Item = MoveRecord>) -> Vec<Issue> {
    if inserts.is_empty() { return Vec::new(); }
    let mut check = BeadCheck::new(inserts);
    for record in records { check.record(&record); }
    check.finish()
}

/// Items printed into their part (`embed: true`) that no layer of the part prints over: the
/// print never pauses for them and nothing closes them in. `layers`: the whole slice. Cost: per
/// such item, a pass over the layers' objects; once per slice, on the report worker.
pub fn uncovered(plate: &fab_plate::Plate, layers: &[makepad_fab_toolpath::Layer]) -> Vec<Issue> {
    plate.inserts().into_iter().filter(|(_, insert, _)| insert.embed).filter_map(|(part, _, host)| {
        let b = part.bounds();
        let closed = layers.iter().any(|l| l.print_z - l.height >= b.max[2] - 1e-6 && l.objects.iter().any(|o| o.object == host && !o.support_only && !o.slices.is_empty()));
        (!closed).then(|| Issue {
            kind: "embed_uncovered", severity: Severity::Warning, object: Some(part.object.clone()),
            at_mm: [0.5 * (b.min[0] + b.max[0]), 0.5 * (b.min[1] + b.max[1]), b.max[2]], footprint: Footprint::of_bounds(b), layer: None, layer_to: None,
            measure: b.max[2],
            message: format!("Nothing prints over {}: the print never pauses to place it, and nothing closes it in", part.name),
            hint: "Lower it into its part, or let the part reach over it".to_string(), fix: None, area_mm2: 0.0, lowest_z: b.min[2], highest_z: b.max[2], flags: 0,
        })
    }).collect()
}

/// Allows the ordinary move checks and the insert checks to share one record stream.
pub(crate) struct BeadCheck<'a> {
    inserts: Vec<Checked<'a>>,
}

struct Checked<'a> {
    part: &'a Part,
    insert: &'a Insert,
    bounds: Bounds,
    triangles: Option<Box<ZIndex>>,
    sections: HashMap<u64, Section>,
    hits: BTreeMap<u32, Hit>,
}

#[derive(Clone, Copy)]
struct TriangleRange { triangle: usize, lo: f64, hi: f64 }

/// An interval tree: at a plane, only the crossing prefix and one child are visited.
struct ZIndex {
    z: f64,
    starts: Vec<TriangleRange>,
    ends: Vec<TriangleRange>,
    below: Option<Box<ZIndex>>,
    above: Option<Box<ZIndex>>,
}

impl ZIndex {
    fn new(mut ranges: Vec<TriangleRange>) -> Option<Box<Self>> {
        if ranges.is_empty() { return None; }
        let mid = ranges.len() / 2;
        ranges.select_nth_unstable_by(mid, |a, b| (a.lo + a.hi).total_cmp(&(b.lo + b.hi)));
        let z = (ranges[mid].lo + ranges[mid].hi) * 0.5;
        let (mut below, mut above, mut starts) = (Vec::new(), Vec::new(), Vec::new());
        for r in ranges {
            if r.hi < z { below.push(r); }
            else if r.lo > z { above.push(r); }
            else { starts.push(r); }
        }
        let mut ends = starts.clone();
        starts.sort_unstable_by(|a, b| a.lo.total_cmp(&b.lo));
        ends.sort_unstable_by(|a, b| b.hi.total_cmp(&a.hi));
        Some(Box::new(Self { z, starts, ends, below: Self::new(below), above: Self::new(above) }))
    }

    fn crossing(&self, z: f64, visit: &mut impl FnMut(usize)) {
        if z <= self.z {
            for r in self.starts.iter().take_while(|r| r.lo <= z) { visit(r.triangle); }
            if let Some(below) = &self.below { below.crossing(z, visit); }
        } else {
            for r in self.ends.iter().take_while(|r| r.hi >= z) { visit(r.triangle); }
            if let Some(above) = &self.above { above.crossing(z, visit); }
        }
    }
}

struct Section {
    bounds: BBox,
    grid: SegGrid<usize>,
    seen: Vec<u64>,
    query: u64,
}

impl Section {
    fn new(mesh: &PartMesh, index: &Option<Box<ZIndex>>, z: f64) -> Self {
        let mut segments = Vec::new();
        let mut bounds = BBox::empty();
        if let Some(index) = index {
            index.crossing(z, &mut |i| {
                let p = mesh.triangles[i].map(|i| mesh.positions[i as usize]);
                let mut cut = Vec::with_capacity(2);
                for k in 0..3 {
                    let (a, b) = (p[k], p[(k + 1) % 3]);
                    // Half-open edges count a vertex once and omit coplanar faces.
                    if (a[2] <= z && b[2] > z) || (b[2] <= z && a[2] > z) {
                        let t = (z - a[2]) / (b[2] - a[2]);
                        cut.push(Pt::from_mm(a[0] + t * (b[0] - a[0]), a[1] + t * (b[1] - a[1])));
                    }
                }
                if cut.len() == 2 && cut[0] != cut[1] {
                    bounds.add(cut[0]);
                    bounds.add(cut[1]);
                    segments.push((cut[0], cut[1], segments.len()));
                }
            });
        }
        Self { bounds, seen: vec![0; segments.len()], query: 0, grid: SegGrid::new(segments) }
    }

    fn contains(&mut self, p: Pt) -> bool {
        if p.x < self.bounds.min.x || p.x > self.bounds.max.x || p.y < self.bounds.min.y || p.y > self.bounds.max.y { return false; }
        self.query = self.query.wrapping_add(1);
        if self.query == 0 { self.seen.fill(0); self.query = 1; }
        let (seen, query) = (&mut self.seen, self.query);
        let mut inside = false;
        self.grid.visit_box(BBox { min: p, max: Pt::new(self.bounds.max.x, p.y) }, |a, b, i| {
            if seen[i] == query { return; }
            seen[i] = query;
            if (a.y > p.y) != (b.y > p.y) {
                let x = a.x as f64 + (p.y - a.y) as f64 * (b.x - a.x) as f64 / (b.y - a.y) as f64;
                if x > p.x as f64 { inside = !inside; }
            }
        });
        inside
    }

    fn hit(&mut self, a: Pt, b: Pt, radius: f64, z: f64, hit: &mut Option<Hit>) {
        for p in [a, b] {
            if self.contains(p) { Hit::add(hit, [p.x_mm(), p.y_mm(), z]); }
        }
        self.grid.visit_box(grown(BBox::of(&[a, b]), radius), |c, d, _| {
            if let Some(p) = near(a, b, c, d, radius) {
                Hit::add(hit, [p.x_mm(), p.y_mm(), z]);
            }
        });
    }
}

fn grown(mut b: BBox, mm: f64) -> BBox {
    let pad = (mm * SCALE).ceil() as i64;
    b.min = Pt::new(b.min.x - pad, b.min.y - pad);
    b.max = Pt::new(b.max.x + pad, b.max.y + pad);
    b
}

/// A point on a-b within `radius` of c-d, including a crossing between samples.
fn near(a: Pt, b: Pt, c: Pt, d: Pt, radius: f64) -> Option<Pt> {
    let cross = |u: [f64; 2], v: [f64; 2]| u[0] * v[1] - u[1] * v[0];
    let ab = [(b.x - a.x) as f64, (b.y - a.y) as f64];
    let cd = [(d.x - c.x) as f64, (d.y - c.y) as f64];
    let ac = [(c.x - a.x) as f64, (c.y - a.y) as f64];
    let den = cross(ab, cd);
    if den != 0.0 {
        let (t, u) = (cross(ac, cd) / den, cross(ac, ab) / den);
        if (0.0..=1.0).contains(&t) && (0.0..=1.0).contains(&u) {
            return Some(Pt::new((a.x as f64 + t * ab[0]).round() as i64, (a.y as f64 + t * ab[1]).round() as i64));
        }
    }
    let project = |p: Pt, x: Pt, y: Pt| {
        let v = [(y.x - x.x) as f64, (y.y - x.y) as f64];
        let norm = v[0] * v[0] + v[1] * v[1];
        let t = if norm == 0.0 { 0.0 } else { (((p.x - x.x) as f64 * v[0] + (p.y - x.y) as f64 * v[1]) / norm).clamp(0.0, 1.0) };
        Pt::new((x.x as f64 + t * v[0]).round() as i64, (x.y as f64 + t * v[1]).round() as i64)
    };
    [(a, project(a, c, d)), (b, project(b, c, d)), (project(c, a, b), c), (project(d, a, b), d)]
        .into_iter().filter(|(p, q)| p.dist(*q) <= radius * SCALE).min_by(|(p, q), (r, s)| p.dist(*q).total_cmp(&r.dist(*s))).map(|(p, _)| p)
}

#[derive(Clone, Copy)]
struct Hit { at: [f64; 3], bounds: Bounds }

impl Hit {
    fn add(hit: &mut Option<Self>, p: [f64; 3]) {
        let h = hit.get_or_insert(Self { at: p, bounds: Bounds::EMPTY });
        h.bounds.add_point(p);
    }

    fn merge(&mut self, other: Self) { self.bounds = self.bounds.union(&other.bounds); }
}

impl<'a> BeadCheck<'a> {
    pub(crate) fn new(inserts: &[(&'a Part, &'a Insert)]) -> Self {
        Self { inserts: inserts.iter().map(|&(part, insert)| {
            let ranges = part.mesh.triangles.iter().enumerate().filter_map(|(triangle, t)| {
                let z = t.map(|i| part.mesh.positions[i as usize][2]);
                let (lo, hi) = (z.into_iter().fold(f64::INFINITY, f64::min), z.into_iter().fold(f64::NEG_INFINITY, f64::max));
                (lo.is_finite() && hi.is_finite() && lo < hi).then_some(TriangleRange { triangle, lo, hi })
            }).collect();
            Checked { part, insert, bounds: part.mesh.bounds(), triangles: ZIndex::new(ranges), sections: HashMap::new(), hits: BTreeMap::new() }
        }).collect() }
    }

    pub(crate) fn record(&mut self, m: &MoveRecord) {
        if m.kind != MoveKind::Extrude || m.e <= 0.0 || m.width <= 0.0 || m.height <= 0.0 { return; }
        let (z, height, width) = (m.z_layer as f64, m.height as f64, m.width as f64);
        let arc = m.flags & flags::ARC != 0 && m.sweep.abs() > 1e-6;
        if m.p0 == m.p1 && !arc { return; }
        let a = Pt::from_mm(m.p0[0] as f64, m.p0[1] as f64);
        let b = Pt::from_mm(m.p1[0] as f64, m.p1[1] as f64);
        let radius = (m.p0[0] as f64 - m.center[0] as f64).hypot(m.p0[1] as f64 - m.center[1] as f64);
        let move_box = if arc { BBox::of(&[
            Pt::from_mm(m.center[0] as f64 - radius, m.center[1] as f64 - radius),
            Pt::from_mm(m.center[0] as f64 + radius, m.center[1] as f64 + radius),
        ]) } else { BBox::of(&[a, b]) };
        let length = if arc { radius * (m.sweep as f64).abs() } else { a.dist(b) / SCALE };
        let mut steps = (length / 0.5).ceil().max(1.0) as usize;
        if arc && radius > 0.001 {
            // Chords deviate by at most a micron, even on a tight curve.
            let angle = 2.0 * (1.0 - 0.001 / radius).acos();
            steps = steps.max(((m.sweep as f64).abs() / angle).ceil() as usize);
        }
        let start_angle = (m.p0[1] as f64 - m.center[1] as f64).atan2(m.p0[0] as f64 - m.center[0] as f64);
        for insert in &mut self.inserts {
            // Touching the bottom or top is allowed; only a real vertical overlap counts.
            if z.min(insert.bounds.max[2]) - (z - height).max(insert.bounds.min[2]) <= 0.05 { continue; }
            let pad = insert.insert.clearance + width;
            let mesh_box = BBox::of(&[Pt::from_mm(insert.bounds.min[0], insert.bounds.min[1]), Pt::from_mm(insert.bounds.max[0], insert.bounds.max[1])]);
            if !grown(mesh_box, pad).overlaps(&move_box) { continue; }
            let mid = z - height * 0.5;
            let section = insert.sections.entry(mid.to_bits()).or_insert_with(|| Section::new(&insert.part.mesh, &insert.triangles, mid));
            if section.grid.is_empty() || !grown(section.bounds, pad).overlaps(&move_box) { continue; }
            let mut hit = None;
            let mut from = a;
            for step in 1..=steps {
                let t = step as f64 / steps as f64;
                let to = if step == steps { b } else if arc {
                    let angle = start_angle + t * m.sweep as f64;
                    Pt::from_mm(m.center[0] as f64 + radius * angle.cos(), m.center[1] as f64 + radius * angle.sin())
                } else {
                    Pt::new((a.x as f64 + t * (b.x - a.x) as f64).round() as i64, (a.y as f64 + t * (b.y - a.y) as f64).round() as i64)
                };
                if grown(section.bounds, pad).overlaps(&BBox::of(&[from, to])) {
                    section.hit(from, to, insert.insert.clearance + width * 0.5, z, &mut hit);
                }
                from = to;
            }
            if let Some(hit) = hit {
                insert.hits.entry(m.layer).and_modify(|h| h.merge(hit)).or_insert(hit);
            }
        }
    }

    pub(crate) fn finish(self) -> Vec<Issue> {
        let mut issues = Vec::new();
        for insert in self.inserts {
            let mut hits = insert.hits.into_iter().peekable();
            while let Some((first, mut hit)) = hits.next() {
                let mut last = first;
                while hits.peek().is_some_and(|(layer, _)| last.checked_add(1) == Some(*layer)) {
                    let (layer, next) = hits.next().unwrap();
                    last = layer;
                    hit.merge(next);
                }
                let layers = if first == last { format!("layer {first}") } else { format!("layers {first} to {last}") };
                issues.push(Issue {
                    kind: "insert_hit", severity: Severity::Error, object: Some(insert.part.object.clone()),
                    at_mm: hit.at, footprint: Footprint::of_bounds(hit.bounds), layer: Some(first), layer_to: Some(last),
                    measure: insert.insert.clearance,
                    message: format!("A bead enters {} at {layers} (it keeps {} mm clear)", insert.part.name, fab_plate::fmt_mm(insert.insert.clearance)),
                    hint: String::new(), fix: None, area_mm2: 0.0, lowest_z: hit.bounds.min[2], highest_z: hit.bounds.max[2], flags: 0,
                });
            }
        }
        issues
    }
}

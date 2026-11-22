use crate::{Footprint, Issue, Materials, ObjectStats, Report, Severity};
use fab_plate::{machine::{self, Machine}, Bounds, Part, PartMesh, Plate};
use std::collections::HashMap;
use std::sync::OnceLock;
use makepad_fab_slicer::{
    geom::{ExPolygon, Pt, orient},
    grid::SegGrid,
    slice::{segments_to_regions, SliceParams},
};

pub(crate) fn placement(machine: &Machine, part: &Part, layer: Option<u32>) -> Vec<Issue> {
    let b = part.bounds();
    if b.is_empty() { return Vec::new(); }
    machine::check_part(machine, part).into_iter().filter_map(|i| {
        use machine::Issue as P;
        let (kind, severity, measure, flags, hint) = match &i {
            // Extrusion paths get an exact keep-out intersection test after
            // their group boxes have rejected distant zones.
            P::KeepOut { .. } if layer.is_some() => return None,
            P::OffBed | P::BelowPlate { .. } | P::KeepOut { .. } => ("outside_bed", Severity::Error, 0.0, 4, "Move the part inside the bed and clear of the exclusion zones; drop its base to Z=0."),
            P::TooTall { height, .. } => ("height", Severity::Error, *height, if part.nozzle.is_some() { 10 } else { 8 }, "Rotate, scale or split the part to fit the build height."),
            P::AboveNozzle { height, .. } => ("height", Severity::Error, *height, 2, "Use a nozzle with enough height, or shorten the part."),
            P::OutOfReach { .. } | P::NoNozzle => ("reach", Severity::Error, 0.0, 1, "Move the part into the assigned nozzle's reach or choose a compatible nozzle."),
            P::Floating { gap } if layer.is_none() => ("mid_air", Severity::Warning, *gap, 0, "Drop the part onto the bed or add supports beneath it."),
            _ => return None,
        };
        let mut issue = Issue::new(kind, severity, Some(&part.object), b.center(), Footprint::of_bounds(b), measure, i.describe(machine), hint);
        issue.layer = layer;
        issue.lowest_z = b.min[2]; issue.highest_z = b.max[2];
        issue.flags = flags;
        if layer.is_some() { issue.at_mm[2] = b.max[2]; }
        Some(issue)
    }).collect()
}

fn class(kind: &str) -> u8 {
    let k = kind.to_uppercase();
    if ["PLA", "TPU", "PVA", "BVOH"].iter().any(|s| k.starts_with(s)) { 1 }
    else if ["ABS", "ASA", "PC", "PA", "PPA", "PPS", "PEI", "PEEK"].iter().any(|s| k.starts_with(s)) { 2 }
    else { 0 }
}

/// Per part: whether it is a body of a group that prints as one ([`Plate::groups`]) other than
/// the group's lowest, so whether it floats is the lowest's to tell: the group stands as one.
pub fn above_its_group(plate: &Plate) -> Vec<bool> {
    let mut out = vec![false; plate.parts.len()];
    let floor = |i: usize| plate.parts[i].known_floor().unwrap_or_else(|| plate.parts[i].bounds().min[2]);
    for g in plate.groups() {
        let lowest = g.iter().copied().min_by(|&a, &b| floor(a).total_cmp(&floor(b)).then(a.cmp(&b)));
        for i in g {
            out[i] = Some(i) != lowest;
        }
    }
    out
}

pub(crate) fn material_issues(machine: &Machine, plate: &Plate, materials: &Materials, report: &mut Report) {
    let above = above_its_group(plate);
    // A resin printer has no nozzles and no filaments: where each part stands is all to check.
    if machine.nozzles.is_empty() {
        for (p, &above) in plate.parts.iter().zip(&above) {
            report.issues.extend(placement(machine, p, None).into_iter().filter(|i| i.kind != "mid_air" || !above));
        }
        return;
    }
    let mut classes: [Option<(u32, String, [f64; 3])>; 3] = [None, None, None];
    for (p, &above) in plate.parts.iter().zip(&above) {
        let f = materials.filament(p.filament);
        let b = p.bounds();
        if b.is_empty() { continue; }
        let at = b.center();
        classes[class(&f.kind) as usize].get_or_insert((p.filament, f.kind.clone(), at));
        let assigned = p.nozzle.or(f.nozzle);
        let allowed: Vec<usize> = (0..machine.nozzles.len()).filter(|&n| n < 64 && f.printable & (1u64 << n) != 0).collect();
        let incompatible = assigned.is_some_and(|n| !allowed.contains(&n)) || allowed.is_empty()
            || p.nozzle.zip(f.nozzle).is_some_and(|(a, b)| a != b)
            || p.filament == 0;
        if incompatible {
            report.issues.push(Issue::new("filament_nozzle", Severity::Error, Some(&p.object), at, Footprint::of_bounds(b), p.filament as f64,
                format!("Filament {} ({}) cannot print on the assigned nozzle", p.filament, f.kind),
                "Select a nozzle allowed by the filament profile and match the part's nozzle assignment."));
        }
        // Check the actual assignment, or a geometrically feasible allowed nozzle.
        // The placement checker remains the sole owner of reach/height rules.
        let reaching = machine.nozzles_reaching(&b);
        let nozzle = assigned.or_else(|| allowed.iter().find(|n| reaching.contains(n)).copied()).or_else(|| allowed.first().copied());
        // Its paint's colours print with their own nozzles, each checked as well.
        let proxy = Part { body_of: None, print_mesh: None, paint: p.paint.clone(), print: Vec::new(), object: p.object.clone(), name: p.name.clone(), filament: p.filament, color: p.color, mesh: PartMesh { positions: vec![b.min, b.max], triangles: Vec::new() }, nozzle, field_bounds: p.field_bounds, field_floor: p.field_floor.clone(), ..Default::default() };
        // Enabled supports are only a request. Their plan owns the floating
        // verdict; until it exists, neither warning nor reassurance is known.
        let supports = materials.of_part(p).supports;
        report.issues.extend(placement(machine, &proxy, None).into_iter().filter(|i| i.kind != "mid_air" || !(supports || above)));
        if !crate::filament_suits_plate(f.bed_unheated, f.bed_temperature) || !crate::filament_suits_plate(f.bed_unheated, f.first_bed_temperature) {
            report.issues.push(Issue::new("bed_temperature", Severity::Error, Some(&p.object), at, Footprint::of_bounds(b), 0.0,
                format!("Filament {} has no compatible bed temperature for this plate (first {:.0} °C, later {:.0} °C)", p.filament, f.first_bed_temperature, f.bed_temperature),
                "Choose a plate type with non-zero first-layer and printing bed temperatures for this filament."));
        }
        let kind = f.kind.to_uppercase();
        if (kind.contains("CF") || kind.contains("GF")) && nozzle.is_none_or(|n| !materials.hardened_nozzles.get(n).copied().unwrap_or(false)) {
            report.issues.push(Issue::new("abrasive", Severity::Error, Some(&p.object), at, Footprint::of_bounds(b), p.filament as f64,
                format!("{} needs an abrasion-resistant nozzle", f.kind), "Use a hardened nozzle for fibre-filled filament."));
        }
    }
    // One bed temperature for the plate: a filament far below it fails.
    let slots: Vec<u32> = plate.parts.iter().map(|p| p.filament).collect();
    if let Some(why) = materials.bed_conflict(&slots) {
        let at = plate.parts.first().map(|p| p.bounds().center()).unwrap_or_default();
        report.issues.push(Issue::new("bed_temperature_mix", Severity::Error, None, at, Footprint::of_bounds(plate.parts.iter().fold(Bounds::EMPTY, |b, p| b.union(&p.bounds()))), 0.0, why,
            "Print the materials as separate plates, or pick filaments whose bed temperatures are close."));
    }
    for ca in 0..3 {
        for cb in ca+1..3 {
            let (Some((a, fa, _)), Some((b, fb, at))) = (&classes[ca], &classes[cb]) else { continue };
            let severity = if ca != 0 && cb != 0 { Severity::Error } else { Severity::Warning };
            report.issues.push(Issue::new("temperature_mix", severity, None, *at, Footprint::of_bounds(plate.parts.iter().filter(|p| [*a, *b].contains(&p.filament)).fold(Bounds::EMPTY, |b, p| b.union(&p.bounds()))), 0.0,
                format!("Filaments {a} ({fa}) and {b} ({fb}) mix temperature classes"),
                "Print these materials on separate plates or select materials in the same temperature class."));
        }
    }
}

/// Face normals and edge adjacency are visited once. Exact-coordinate welding
/// joins imported meshes that duplicate vertices at normal or material seams.
pub fn analyse_plate(machine: &Machine, plate: &Plate, materials: &Materials) -> Report {
    analyse_plate_with(machine, plate, materials, &|_| None, &|| false).expect("nothing stops it")
}

/// How many facets or vertices a part's checks run through between asks of their `stop`.
const STOP_EVERY: usize = 1 << 14;

/// Whether the checks stop at their `i`th facet or vertex: `stop`, asked every [`STOP_EVERY`].
pub(crate) fn stopped(i: usize, stop: &dyn Fn() -> bool) -> bool {
    i % STOP_EVERY == 0 && stop()
}

/// A part's exact surface: its distance field in plate coordinates
/// (negative inside), whose gradient gives the surface normal at a point.
pub type PartField<'a> = &'a (dyn Fn(f64, f64, f64) -> f64 + Sync);

/// [`analyse_plate`] where `field(part)` gives a part's exact surface: its
/// facets then take the surface normal at their centre (the field's
/// gradient) instead of the facet's own, so a part sliced from its field is
/// judged by its true slopes, not by its display mesh's tessellation.
/// The parts of a glued group ([`Plate::groups`]) are judged as the group prints: a facet
/// inside another of its group's parts is no surface of the group's and hints nothing.
/// `stop` is asked between the steps of a part's checks and every [`STOP_EVERY`] facets or
/// vertices within one: once it is true the checks end with None, telling nothing they found.
pub fn analyse_plate_with<'f>(machine: &Machine, plate: &Plate, materials: &Materials, field: &dyn Fn(usize) -> Option<PartField<'f>>, stop: &dyn Fn() -> bool) -> Option<Report> {
    let covers = Covers::of(plate, field);
    let parts: Vec<PartAnalysis> = plate.parts.iter().enumerate().map(|(i, p)| {
        let covered = |q: [f64; 3]| covers.covered(i, q);
        analyse_part_with(machine, p, materials, field(i), covers.grouped(i).then_some(&covered as &dyn Fn([f64; 3]) -> bool), stop)
    }).collect::<Option<_>>()?;
    let mut report = combine_plate(machine, plate, materials, parts.iter());
    report.issues.extend(crate::overlap::overlap_issues(plate, field, stop)?);
    report.sort();
    Some(report)
}

/// How far (mm) inside a part's exact surface a point lies to count as inside it.
pub(crate) const COVER: f64 = 0.05;

/// The parts of a plate's glued groups as solids: whether a point lies inside one.
struct Covers<'a> {
    /// Per part: its group's index, when it is in one.
    group: Vec<Option<usize>>,
    groups: Vec<Vec<usize>>,
    /// Per part of a group: its solid.
    solids: Vec<Option<Solid<'a>>>,
}

/// A part as a solid: its exact surface (negative inside), else its closed mesh.
pub(crate) enum Solid<'a> {
    Field(PartField<'a>),
    Mesh(MeshSolid<'a>),
}


impl<'a> Covers<'a> {
    fn of<'f: 'a>(plate: &'a Plate, field: &dyn Fn(usize) -> Option<PartField<'f>>) -> Covers<'a> {
        let groups = plate.groups();
        let mut group = vec![None; plate.parts.len()];
        let mut solids: Vec<Option<Solid<'a>>> = (0..plate.parts.len()).map(|_| None).collect();
        for (k, g) in groups.iter().enumerate() {
            for &i in g {
                group[i] = Some(k);
                solids[i] = Some(match field(i) {
                    Some(f) => Solid::Field(f),
                    None => Solid::Mesh(MeshSolid::new(&plate.parts[i].mesh)),
                });
            }
        }
        Covers { group, groups, solids }
    }

    fn grouped(&self, i: usize) -> bool {
        self.group[i].is_some()
    }

    /// Whether `q` lies inside a part of part `i`'s group other than `i`.
    fn covered(&self, i: usize, q: [f64; 3]) -> bool {
        let Some(k) = self.group[i] else { return false };
        self.groups[k].iter().filter(|&&j| j != i).any(|&j| match &self.solids[j] {
            Some(Solid::Field(f)) => f(q[0], q[1], q[2]) < -COVER,
            Some(Solid::Mesh(m)) => m.holds(q),
            None => false,
        })
    }
}

/// A closed mesh as a solid: a point is inside when a ray up from it crosses the surface an odd
/// number of times, over the facets an XY grid lists under the point.
pub(crate) struct MeshSolid<'a> {
    pub(crate) mesh: &'a PartMesh,
    bounds: Bounds,
    cell: f64,
    nx: usize,
    ny: usize,
    /// Per cell, the facets whose XY box meets it.
    cells: Vec<Vec<u32>>,
    tree: OnceLock<FacetTree>,
    shells: OnceLock<Vec<usize>>,
}

impl<'a> MeshSolid<'a> {
    pub(crate) fn new(mesh: &'a PartMesh) -> MeshSolid<'a> {
        let bounds = mesh.bounds();
        let n = mesh.triangles.len().max(1) as f64;
        let (w, h) = ((bounds.max[0] - bounds.min[0]).max(1e-6), (bounds.max[1] - bounds.min[1]).max(1e-6));
        // about one facet's footprint a cell, no more cells than facets
        let cell = (w * h / n).sqrt().max(1e-3);
        let (nx, ny) = ((w / cell).ceil() as usize + 1, (h / cell).ceil() as usize + 1);
        let mut cells = vec![Vec::new(); nx * ny];
        if !bounds.is_empty() {
            let at = |v: f64, lo: f64, n: usize| (((v - lo) / cell).floor().max(0.0) as usize).min(n - 1);
            for (t, tri) in mesh.triangles.iter().enumerate() {
                let p = tri.map(|v| mesh.positions[v as usize]);
                let (x0, x1) = (p.iter().map(|q| q[0]).fold(f64::INFINITY, f64::min), p.iter().map(|q| q[0]).fold(f64::NEG_INFINITY, f64::max));
                let (y0, y1) = (p.iter().map(|q| q[1]).fold(f64::INFINITY, f64::min), p.iter().map(|q| q[1]).fold(f64::NEG_INFINITY, f64::max));
                for y in at(y0, bounds.min[1], ny)..=at(y1, bounds.min[1], ny) {
                    for x in at(x0, bounds.min[0], nx)..=at(x1, bounds.min[0], nx) {
                        cells[y * nx + x].push(t as u32);
                    }
                }
            }
        }
        MeshSolid { mesh, bounds, cell, nx, ny, cells, tree: OnceLock::new(), shells: OnceLock::new() }
    }

    /// Whether any facet lies within `r` of `c`, including facets outside a pair's meeting box.
    pub(crate) fn within(&self, c: [f64; 3], r: f64) -> bool {
        self.tree.get_or_init(|| FacetTree::new(self.mesh)).within(self.mesh, c, r)
    }

    /// Per facet, its shell's root. A stopped build leaves the cache empty for the next ask.
    pub(crate) fn shells(&self, stop: &dyn Fn() -> bool) -> Option<&[usize]> {
        if let Some(shells) = self.shells.get() {
            return Some(shells);
        }
        let roots = shells(self.mesh, stop)?;
        let _ = self.shells.set(roots);
        self.shells.get().map(Vec::as_slice)
    }

    pub(crate) fn holds(&self, q: [f64; 3]) -> bool {
        let b = &self.bounds;
        if b.is_empty() || (0..3).any(|k| q[k] < b.min[k] || q[k] > b.max[k]) {
            return false;
        }
        // off any edge or vertex of a grid of round numbers
        let (x, y) = (q[0] + 1.234_567e-7, q[1] + 2.345_678e-7);
        let cx = (((x - b.min[0]) / self.cell).floor().max(0.0) as usize).min(self.nx - 1);
        let cy = (((y - b.min[1]) / self.cell).floor().max(0.0) as usize).min(self.ny - 1);
        let mut crossings = 0;
        for &t in &self.cells[cy * self.nx + cx] {
            let [a, bb, c] = self.mesh.triangles[t as usize].map(|v| self.mesh.positions[v as usize]);
            let d = (bb[1] - c[1]) * (a[0] - c[0]) + (c[0] - bb[0]) * (a[1] - c[1]);
            if d == 0.0 {
                continue;
            }
            let l1 = ((bb[1] - c[1]) * (x - c[0]) + (c[0] - bb[0]) * (y - c[1])) / d;
            let l2 = ((c[1] - a[1]) * (x - c[0]) + (a[0] - c[0]) * (y - c[1])) / d;
            let l3 = 1.0 - l1 - l2;
            if l1 < 0.0 || l2 < 0.0 || l3 < 0.0 {
                continue;
            }
            if l1 * a[2] + l2 * bb[2] + l3 * c[2] > q[2] {
                crossings += 1;
            }
        }
        crossings % 2 == 1
    }
}

/// All facets of a mesh in a tree of boxes, split at the median along the widest spread of
/// centroids. It keeps no reference to the mesh, so a [`MeshSolid`] stays covariant in its
/// lifetime; its queries take the mesh it was built from.
struct FacetTree {
    nodes: Vec<FacetNode>,
    facets: Vec<usize>,
}

struct FacetNode {
    bounds: Bounds,
    facets: std::ops::Range<usize>,
    children: Option<[usize; 2]>,
}

impl FacetTree {
    fn new(mesh: &PartMesh) -> Self {
        let mut tree = Self { nodes: Vec::new(), facets: (0..mesh.triangles.len()).collect() };
        if tree.facets.is_empty() {
            return tree;
        }
        let boxes: Vec<(Bounds, [f64; 3])> = mesh.triangles.iter().map(|t| {
            let p = t.map(|v| mesh.positions[v as usize]);
            let mut bounds = Bounds::EMPTY;
            for q in p { bounds.add_point(q); }
            (bounds, std::array::from_fn(|k| (p[0][k] + p[1][k] + p[2][k]) / 3.0))
        }).collect();
        tree.nodes.push(FacetNode { bounds: Bounds::EMPTY, facets: 0..tree.facets.len(), children: None });
        let mut todo = vec![0];
        while let Some(i) = todo.pop() {
            let range = tree.nodes[i].facets.clone();
            let (mut bounds, mut centroids) = (Bounds::EMPTY, Bounds::EMPTY);
            for &t in &tree.facets[range.clone()] {
                bounds = bounds.union(&boxes[t].0);
                centroids.add_point(boxes[t].1);
            }
            tree.nodes[i].bounds = bounds;
            if range.len() <= 4 {
                continue;
            }
            let span = centroids.size();
            let axis = (0..3).max_by(|&a, &b| span[a].total_cmp(&span[b])).unwrap_or(0);
            let half = range.len() / 2;
            tree.facets[range.clone()].select_nth_unstable_by(half, |&a, &b| boxes[a].1[axis].total_cmp(&boxes[b].1[axis]));
            let mid = range.start + half;
            let child = tree.nodes.len();
            tree.nodes.extend([
                FacetNode { bounds: Bounds::EMPTY, facets: range.start..mid, children: None },
                FacetNode { bounds: Bounds::EMPTY, facets: mid..range.end, children: None },
            ]);
            tree.nodes[i].children = Some([child, child + 1]);
            todo.extend([child, child + 1]);
        }
        tree
    }

    fn within(&self, mesh: &PartMesh, c: [f64; 3], r: f64) -> bool {
        if self.nodes.is_empty() {
            return false;
        }
        let r2 = r * r;
        let mut todo = Vec::with_capacity(32);
        todo.push(0);
        while let Some(i) = todo.pop() {
            let node = &self.nodes[i];
            let distance2: f64 = (0..3).map(|k| {
                let d = (node.bounds.min[k] - c[k]).max(c[k] - node.bounds.max[k]).max(0.0);
                d * d
            }).sum();
            if distance2 > r2 {
                continue;
            }
            if let Some(children) = node.children {
                todo.extend(children);
            } else {
                for &t in &self.facets[node.facets.clone()] {
                    let p = mesh.triangles[t].map(|v| mesh.positions[v as usize]);
                    if triangle_distance2(c, p) <= r2 {
                        return true;
                    }
                }
            }
        }
        false
    }
}

/// Squared distance to the closest point of a triangle, by Ericson's vertex, edge and face
/// regions. A triangle of no area uses its three segments, including their endpoints.
fn triangle_distance2(p: [f64; 3], [a, b, c]: [[f64; 3]; 3]) -> f64 {
    let sub = |a: [f64; 3], b: [f64; 3]| [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
    let dot = |a: [f64; 3], b: [f64; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
    let along = |a: [f64; 3], d: [f64; 3], t: f64| [a[0] + d[0] * t, a[1] + d[1] * t, a[2] + d[2] * t];
    let distance2 = |q| { let d = sub(p, q); dot(d, d) };
    let edges = || [(a, b), (b, c), (c, a)].into_iter().map(|(u, v)| {
        let edge = sub(v, u);
        let len2 = dot(edge, edge);
        let t = if len2 == 0.0 { 0.0 } else { (dot(sub(p, u), edge) / len2).clamp(0.0, 1.0) };
        distance2(along(u, edge, t))
    }).fold(f64::INFINITY, f64::min);
    let (ab, ac, ap) = (sub(b, a), sub(c, a), sub(p, a));
    let normal = [ab[1] * ac[2] - ab[2] * ac[1], ab[2] * ac[0] - ab[0] * ac[2], ab[0] * ac[1] - ab[1] * ac[0]];
    if dot(normal, normal) == 0.0 {
        return edges();
    }
    let (d1, d2) = (dot(ab, ap), dot(ac, ap));
    if d1 <= 0.0 && d2 <= 0.0 {
        return distance2(a);
    }
    let bp = sub(p, b);
    let (d3, d4) = (dot(ab, bp), dot(ac, bp));
    if d3 >= 0.0 && d4 <= d3 {
        return distance2(b);
    }
    let vc = d1 * d4 - d3 * d2;
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        return distance2(along(a, ab, d1 / (d1 - d3)));
    }
    let cp = sub(p, c);
    let (d5, d6) = (dot(ab, cp), dot(ac, cp));
    if d6 >= 0.0 && d5 <= d6 {
        return distance2(c);
    }
    let vb = d5 * d2 - d1 * d6;
    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
        return distance2(along(a, ac, d2 / (d2 - d6)));
    }
    let va = d3 * d6 - d5 * d4;
    if va <= 0.0 && d4 - d3 >= 0.0 && d5 - d6 >= 0.0 {
        return distance2(along(b, sub(c, b), (d4 - d3) / ((d4 - d3) + (d5 - d6))));
    }
    let sum = va + vb + vc;
    if sum <= 0.0 {
        return edges();
    }
    distance2(along(along(a, ab, vb / sum), ac, vc / sum))
}

/// A connected stretch of a part's overhanging facets: what a support has to reach.
#[derive(Clone, Debug)]
pub struct OverhangPatch {
    /// Its facets, as indices into the mesh's triangles.
    pub triangles: Vec<u32>,
    pub area_mm2: f64,
    pub centroid: [f64; 3],
    /// Its lowest corner.
    pub lowest: [f64; 3],
    pub bounds: Bounds,
    /// Its steepest facet's degree (1..5).
    pub worst: u8,
}

/// How far past the limit a facet leans, 0 (it holds itself, or lies on the bed) to 5.
/// `nz`: the z of its unit surface normal. `limit`: the face tilt from vertical it may have.
pub fn overhang_degree(nz: f64, corners: [[f64; 3]; 3], limit: f64) -> u8 {
    let excess = (-nz).clamp(-1.0, 1.0).asin().to_degrees() - limit;
    if excess <= 0.0 || corners.iter().all(|p| p[2].abs() <= machine::EDGE_SLACK) {
        return 0;
    }
    (5.0 * excess / (90.0 - limit).max(1.0)).ceil().clamp(1.0, 5.0) as u8
}

/// One overhang degree per triangle of a display mesh (printer frame, mm) at `limit`: what its
/// facets' own normals say. Degrees describe tilt, including bridged ceilings;
/// bridge safety depends on anchors and material settings, and lives in the issues.
pub fn render_overhang(rm: &fab_plate::RenderMesh, limit: f64) -> Vec<u8> {
    rm.indices.chunks_exact(3).map(|t| {
        let [a, b, c] = [t[0], t[1], t[2]].map(|i| rm.positions[i as usize].map(f64::from));
        let n = [(b[1] - a[1]) * (c[2] - a[2]) - (b[2] - a[2]) * (c[1] - a[1]), (b[2] - a[2]) * (c[0] - a[0]) - (b[0] - a[0]) * (c[2] - a[2]), (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])];
        let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
        if len > 0.0 { overhang_degree(n[2] / len, [a, b, c], limit) } else { 0 }
    }).collect()
}

/// The connected overhang patches of `mesh`, `degrees` holding one degree per triangle.
pub fn overhang_patches(mesh: &PartMesh, degrees: &[u8]) -> Vec<OverhangPatch> {
    let never = || false;
    welded(mesh, &never).and_then(|ids| flood_patches(mesh, &ids, degrees, &never)).expect("nothing stops it")
}

/// Exact-coordinate welding joins imported meshes that duplicate vertices at normal or material seams.
/// None once `stop` is true, asked every [`STOP_EVERY`] vertices.
fn welded(mesh: &PartMesh, stop: &dyn Fn() -> bool) -> Option<Vec<usize>> {
    let mut vertices = HashMap::with_capacity(mesh.positions.len());
    mesh.positions.iter().enumerate().map(|(i, p)| {
        if stopped(i, stop) {
            return None;
        }
        let key = p.map(|v| if v == 0.0 { 0 } else { v.to_bits() });
        let next = vertices.len();
        Some(*vertices.entry(key).or_insert(next))
    }).collect()
}

/// Triangle indices are u32; a referenced vertex's welded ID cannot exceed
/// its index. Keep an undirected edge in one word for both adjacency tables.
fn edge_key(a: usize, b: usize) -> u64 {
    ((a.min(b) as u64) << 32) | a.max(b) as u64
}

/// Facets sharing an edge are one patch. Linear in the overhanging facets. None once `stop` is
/// true, asked every [`STOP_EVERY`] facets each pass goes through.
fn flood_patches(mesh: &PartMesh, ids: &[usize], degrees: &[u8], stop: &dyn Fn() -> bool) -> Option<Vec<OverhangPatch>> {
    let mut edges: HashMap<u64, u32> = HashMap::new();
    let mut adjacent: HashMap<u32, Vec<u32>> = HashMap::new();
    for (i, t) in mesh.triangles.iter().enumerate() {
        if stopped(i, stop) {
            return None;
        }
        if degrees[i] == 0 { continue; }
        for k in 0..3 {
            let (a, b) = (ids[t[k] as usize], ids[t[(k + 1) % 3] as usize]);
            if let Some(j) = edges.insert(edge_key(a, b), i as u32) {
                adjacent.entry(i as u32).or_default().push(j);
                adjacent.entry(j).or_default().push(i as u32);
            }
        }
    }
    let mut out = Vec::new();
    let mut seen = vec![false; degrees.len()];
    let mut flooded = 0;
    for start in 0..degrees.len() {
        if stopped(start, stop) {
            return None;
        }
        if degrees[start] == 0 || seen[start] { continue; }
        seen[start] = true;
        let mut pending = vec![start as u32];
        let mut patch = OverhangPatch { triangles: Vec::new(), area_mm2: 0.0, centroid: [0.0; 3], lowest: [0.0, 0.0, f64::INFINITY], bounds: Bounds::EMPTY, worst: 0 };
        while let Some(i) = pending.pop() {
            flooded += 1;
            if stopped(flooded, stop) {
                return None;
            }
            patch.triangles.push(i);
            patch.worst = patch.worst.max(degrees[i as usize]);
            let [a, b, c] = mesh.triangles[i as usize].map(|v| mesh.positions[v as usize]);
            let (u, v) = ([b[0]-a[0], b[1]-a[1], b[2]-a[2]], [c[0]-a[0], c[1]-a[1], c[2]-a[2]]);
            let n = [u[1]*v[2]-u[2]*v[1], u[2]*v[0]-u[0]*v[2], u[0]*v[1]-u[1]*v[0]];
            let area = 0.5 * n.iter().map(|x| x*x).sum::<f64>().sqrt();
            patch.area_mm2 += area;
            for d in 0..3 { patch.centroid[d] += (a[d] + b[d] + c[d]) / 3.0 * area; }
            for p in [a, b, c] {
                patch.bounds.add_point(p);
                if p[2] < patch.lowest[2] { patch.lowest = p; }
            }
            for &j in adjacent.get(&i).map_or(&[][..], |v| &v[..]) {
                if !seen[j as usize] { seen[j as usize] = true; pending.push(j); }
            }
        }
        if patch.area_mm2 > 0.0 {
            for d in &mut patch.centroid { *d /= patch.area_mm2; }
        }
        out.push(patch);
    }
    Some(out)
}

/// A ceiling's local section consists of its boundary edges whose other facet
/// extends below the plane. Locate anchors within the sliced check's 0.01 mm
/// tolerance of those segments, without slicing unrelated geometry.
/// Only candidate edges are indexed; the mesh and its vertices stay borrowed.
fn ceiling_sections(mesh: &PartMesh, ids: &[usize], patches: &[OverhangPatch], candidates: &[usize]) -> HashMap<usize, (Vec<ExPolygon>, SegGrid<u32>)> {
    let mut edges = HashMap::new();
    for &k in candidates {
        for &i in &patches[k].triangles {
            let t = mesh.triangles[i as usize];
            for j in 0..3 {
                let (a, b) = (t[j] as usize, t[(j + 1) % 3] as usize);
                let key = edge_key(ids[a], ids[b]);
                // Reverse downward facets to keep ceiling material on the left.
                if edges.remove(&key).is_none() { edges.insert(key, (k, b, a)); }
            }
        }
    }
    let mut supported: HashMap<usize, Vec<(Pt, Pt)>> = HashMap::new();
    for t in &mesh.triangles {
        for j in 0..3 {
            let (a, b, c) = (t[j] as usize, t[(j + 1) % 3] as usize, t[(j + 2) % 3] as usize);
            let (pa, pb, pc) = (mesh.positions[a], mesh.positions[b], mesh.positions[c]);
            if (pa[2] - pb[2]).abs() > 1e-7 || pc[2] >= pa[2].min(pb[2]) - 1e-7 { continue; }
            let key = edge_key(ids[a], ids[b]);
            let Some(&(k, _, _)) = edges.get(&key) else { continue; };
            let (a, b) = (Pt::from_mm(pa[0], pa[1]), Pt::from_mm(pb[0], pb[1]));
            if a != b { supported.entry(k).or_default().push((a, b)); }
        }
    }
    let mut boundaries: HashMap<usize, Vec<(Pt, Pt)>> = HashMap::new();
    for (_, (k, a, b)) in edges {
        let (a, b) = (mesh.positions[a], mesh.positions[b]);
        boundaries.entry(k).or_default().push((Pt::from_mm(a[0], a[1]), Pt::from_mm(b[0], b[1])));
    }
    let params = SliceParams { closing_radius: 0.0, resolution: 0.0, max_gap: 0.0 };
    boundaries.into_iter().filter_map(|(k, boundary)| {
        let anchors = supported.remove(&k)?;
        let (a, b) = anchors[0];
        // Supports on one straight edge cannot anchor an interior chord.
        if anchors.iter().all(|&(c, d)| orient(a, b, c) == 0 && orient(a, b, d) == 0) { return None; }
        Some((k, (segments_to_regions(boundary, &params), SegGrid::new(anchors.into_iter().map(|(a, b)| (a, b, 0)).collect()))))
    }).collect()
}

/// Canonical translated boundaries and supports. Merge exact collinear axis
/// segments so different triangulations of the same straight wall share a key.
/// Non-axis segments remain exact: a missed reuse is preferable to rounding a
/// different anchor arrangement into the same ceiling. The cache lives only
/// for this part analysis, so edits and material changes always recompute it.
#[derive(Hash, PartialEq, Eq)]
struct CeilingKey {
    boundary: Vec<[i64; 4]>,
    supports: Vec<[i64; 4]>,
}

fn ceiling_key(regions: &[ExPolygon], supports: &SegGrid<u32>) -> (CeilingKey, Pt) {
    let origin = regions.iter().map(|e| e.bbox().min).reduce(|a, b| Pt::new(a.x.min(b.x), a.y.min(b.y))).unwrap_or_default();
    let segment = |a: Pt, b: Pt| {
        let (a, b) = (a.sub(origin), b.sub(origin));
        if (a.x, a.y) < (b.x, b.y) { [a.x, a.y, b.x, b.y] } else { [b.x, b.y, a.x, a.y] }
    };
    let mut boundary = Vec::new();
    for e in regions {
        for r in e.rings() {
            for i in 0..r.pts.len() { boundary.push(segment(r.pts[i], r.pts[(i+1)%r.pts.len()])); }
        }
    }
    boundary.sort_unstable();
    let mut horizontal = Vec::new();
    let mut vertical = Vec::new();
    let mut other = Vec::new();
    for &(a, b, _) in supports.segments() {
        let s = segment(a, b);
        if s[1] == s[3] { horizontal.push([s[1], s[0], s[2]]); }
        else if s[0] == s[2] { vertical.push([s[0], s[1], s[3]]); }
        else { other.push(s); }
    }
    for (rows, vertical) in [(&mut horizontal, false), (&mut vertical, true)] {
        rows.sort_unstable();
        let mut merged: Vec<[i64; 3]> = Vec::new();
        for &row in rows.iter() {
            if let Some(last) = merged.last_mut().filter(|last| last[0] == row[0] && last[2] >= row[1]) {
                last[2] = last[2].max(row[2]);
            } else { merged.push(row); }
        }
        other.extend(merged.into_iter().map(|[fixed, lo, hi]| if vertical { [fixed, lo, fixed, hi] } else { [lo, fixed, hi, fixed] }));
    }
    other.sort_unstable();
    (CeilingKey { boundary, supports: other }, origin)
}

/// As the sliced check does, choose one direction per region by the anchored
/// share of its lines. Holes may interrupt some lines; they do not invalidate the
/// anchored threads across the same region. An entirely one-ended shelf still has
/// no bridge. The lines run through `pivot`, the key's origin, so the translated
/// copies of a ceiling that share a key measure alike.
fn ceiling_span(regions: &[ExPolygon], under: &SegGrid<u32>, pivot: Pt, width: f64, limit: f64) -> Option<(f64, [f64; 2])> {
    let mut best: Option<(f64, [f64; 2])> = None;
    for region in regions {
        let b = region.bbox();
        // If no chord can exceed the limit, only bridge existence matters.
        let measure = b.min.dist(b.max) / makepad_fab_slicer::geom::SCALE > limit
            && region.area_mm2() / width > limit;
        let (span, at, _) = crate::sliced::bridge_span_edges(region, under, width, measure, pivot)?;
        if best.is_none_or(|(length, _)| span > length) { best = Some((span, at)); }
    }
    best
}

/// One part's share of [`analyse_plate`]: a pure function of the part (as
/// placed), the machine and the materials, so a caller may keep it for a
/// part that has not changed.
#[derive(Clone, Debug)]
pub struct PartAnalysis {
    stats: ObjectStats,
    issues: Vec<Issue>,
    overhang: Vec<u8>,
}

/// [`analyse_plate`] from its parts' analyses (one per plate part, in order).
pub fn combine_plate<'a>(machine: &Machine, plate: &Plate, materials: &Materials, parts: impl Iterator<Item = &'a PartAnalysis>) -> Report {
    let mut report = Report { overhang_limit: materials.overhang_angle_deg, ..Report::default() };
    material_issues(machine, plate, materials, &mut report);
    for a in parts {
        report.issues.extend(a.issues.iter().cloned());
        report.objects.push(a.stats.clone());
        report.overhang.push(a.overhang.clone());
    }
    merge_objects(&mut report);
    report.sort();
    report
}

pub fn analyse_part(machine: &Machine, p: &Part, materials: &Materials) -> PartAnalysis {
    analyse_part_with(machine, p, materials, None, None, &|| false).expect("nothing stops it")
}

/// [`analyse_part`] judged by the part's exact surface when given (see [`analyse_plate_with`]),
/// a facet whose centre is `covered` (inside another part it prints as one with) no overhang;
/// None once `stop` is true (asked as [`analyse_plate_with`] asks it).
pub fn analyse_part_with(machine: &Machine, p: &Part, materials: &Materials, exact: Option<PartField<'_>>, covered: Option<&dyn Fn([f64; 3]) -> bool>, stop: &dyn Fn() -> bool) -> Option<PartAnalysis> {
    let materials = materials.of_part(p);
    let materials = materials.as_ref();
    let mut issues = Vec::new();
    {
        let mesh = &p.mesh;
        let bounds = p.bounds();
        let mut stats = ObjectStats { object: p.object.clone(), reaching_nozzles: if bounds.is_empty() { Vec::new() } else { machine.nozzles_reaching(&bounds) }, max_height_per_nozzle: vec![0.0; machine.nozzles.len()], ..Default::default() };
        if let Some(n) = p.nozzle.or(materials.filament(p.filament).nozzle) {
            if let Some(z) = stats.max_height_per_nozzle.get_mut(n) { *z = if bounds.is_empty() { 0.0 } else { bounds.max[2] }; }
        }
        let ids = welded(mesh, stop)?;
        let mut degrees = vec![0; mesh.triangles.len()];
        // A closed triangular mesh has three half-edges per face, paired.
        // Reserve that existing adjacency table once instead of rehashing it
        // repeatedly while the same geometry is analysed after every edit.
        let mut edge_counts: HashMap<u64, usize> = HashMap::with_capacity(mesh.triangles.len().saturating_mul(3) / 2);
        let (mut surface_area, mut volume) = (0.0, 0.0);
        for (i, t) in mesh.triangles.iter().enumerate() {
            if stopped(i, stop) {
                return None;
            }
            let [a, b, c] = t.map(|v| mesh.positions[v as usize]);
            let u = [b[0]-a[0], b[1]-a[1], b[2]-a[2]];
            let v = [c[0]-a[0], c[1]-a[1], c[2]-a[2]];
            let n = [u[1]*v[2]-u[2]*v[1], u[2]*v[0]-u[0]*v[2], u[0]*v[1]-u[1]*v[0]];
            let len = n.iter().map(|x| x*x).sum::<f64>().sqrt();
            if len <= 1e-12 { continue; }
            surface_area += len * 0.5;
            volume += (a[0] * (b[1] * c[2] - b[2] * c[1]) - a[1] * (b[0] * c[2] - b[2] * c[0]) + a[2] * (b[0] * c[1] - b[1] * c[0])) / 6.0;
            for k in 0..3 {
                let (a, b) = (ids[t[k] as usize], ids[t[(k + 1) % 3] as usize]);
                *edge_counts.entry(edge_key(a, b)).or_default() += 1;
            }
            // The surface normal at the facet's centre: the field's gradient.
            let nz = match exact {
                Some(f) => {
                    let c = [(a[0]+b[0]+c[0])/3.0, (a[1]+b[1]+c[1])/3.0, (a[2]+b[2]+c[2])/3.0];
                    const H: f64 = 1e-4;
                    let g = [
                        f(c[0]+H, c[1], c[2]) - f(c[0]-H, c[1], c[2]),
                        f(c[0], c[1]+H, c[2]) - f(c[0], c[1]-H, c[2]),
                        f(c[0], c[1], c[2]+H) - f(c[0], c[1], c[2]-H),
                    ];
                    let gl = g.iter().map(|x| x*x).sum::<f64>().sqrt();
                    if gl > 1e-12 { g[2] / gl } else { n[2] / len }
                }
                None => n[2] / len,
            };
            let mut degree = overhang_degree(nz, [a, b, c], materials.overhang_angle_deg);
            if degree > 0 && covered.is_some_and(|inside| inside([(a[0] + b[0] + c[0]) / 3.0, (a[1] + b[1] + c[1]) / 3.0, (a[2] + b[2] + c[2]) / 3.0])) {
                degree = 0;
            }
            degrees[i] = degree;
            stats.overhang_area_mm2[degree as usize] += len * 0.5;
        }
        let open_edges = edge_counts.values().filter(|&&count| count == 1).count();
        let nonmanifold_edges = edge_counts.values().filter(|&&count| count > 2).count();
        for (kind, count, label) in [("open_edges", open_edges, "open edges"), ("nonmanifold_edges", nonmanifold_edges, "nonmanifold edges")] {
            if count > 0 {
                let mut issue = Issue::new(kind, Severity::Warning, Some(&p.object), bounds.center(), Footprint::of_bounds(bounds), count as f64,
                    format!("Mesh has {count} {label}"), "Make printable closes holes and repairs the mesh before slicing.");
                issue.fix = Some("make_printable");
                issues.push(issue);
            }
        }
        // A cheap sheet estimate alongside the existing triangle pass, not a ray cast:
        // twice volume/area approaches thickness for a broad thin closed shell.
        if open_edges == 0 && nonmanifold_edges == 0 && surface_area > 0.0 {
            let thickness = 2.0 * volume.abs() / surface_area;
            if thickness > 0.0 && thickness < 0.8 {
                let mut issue = Issue::new("thin_feature", Severity::Warning, Some(&p.object), bounds.center(), Footprint::of_bounds(bounds), thickness,
                    format!("Mesh is about {thickness:.2} mm thick"), "Make printable strengthens thin mesh features.");
                issue.fix = Some("make_printable");
                issues.push(issue);
            }
        }
        let patches = flood_patches(mesh, &ids, &degrees, stop)?;
        // Test flatness only on patches that could produce an issue. No ceiling
        // bookkeeping or section work for parts without a candidate.
        let candidates: Vec<_> = if open_edges == 0 && nonmanifold_edges == 0 {
            patches.iter().enumerate().filter_map(|(k, patch)| {
                if patch.bounds.max[2] - patch.bounds.min[2] > 1e-7 { return None; }
                // A chord cannot exceed the bounding rectangle's diagonal.
                // Small ceilings that cannot warn need no anchor work either.
                let could_warn = patch.area_mm2 > materials.patch_area_mm2
                    || (patch.area_mm2 / materials.line_width > materials.max_bridge_length
                        && (patch.bounds.max[0] - patch.bounds.min[0]).hypot(patch.bounds.max[1] - patch.bounds.min[1]) > materials.max_bridge_length);
                (could_warn
                    && patch.triangles.iter().all(|&i| {
                        let [a, b, c] = mesh.triangles[i as usize].map(|v| mesh.positions[v as usize]);
                        if (b[0]-a[0])*(c[1]-a[1]) - (b[1]-a[1])*(c[0]-a[0]) >= 0.0 { return false; }
                        // Preserve field-normal eligibility without a flatness
                        // buffer or additional work on non-candidate facets.
                        if let Some(f) = exact {
                            let c = [(a[0]+b[0]+c[0])/3.0, (a[1]+b[1]+c[1])/3.0, (a[2]+b[2]+c[2])/3.0];
                            const H: f64 = 1e-4;
                            let g = [f(c[0]+H,c[1],c[2])-f(c[0]-H,c[1],c[2]), f(c[0],c[1]+H,c[2])-f(c[0],c[1]-H,c[2]), f(c[0],c[1],c[2]+H)-f(c[0],c[1],c[2]-H)];
                            let len = g.iter().map(|x| x*x).sum::<f64>().sqrt();
                            if len > 1e-12 && g[2] / len >= -1.0 + 1e-12 { return false; }
                        }
                        true
                    })).then_some(k)
            }).collect()
        } else { Vec::new() };
        if stop() {
            return None;
        }
        let sections = if candidates.is_empty() { HashMap::new() } else { ceiling_sections(mesh, &ids, &patches, &candidates) };
        if stop() {
            return None;
        }
        let mut spans = HashMap::new();
        for (k, found) in patches.into_iter().enumerate() {
            if let Some((regions, under)) = sections.get(&k) {
                let (key, origin) = ceiling_key(regions, under);
                let bridge = spans.entry(key).or_insert_with(|| {
                    ceiling_span(regions, under, origin, materials.line_width, materials.max_bridge_length)
                        .map(|(span, at)| (span, [at[0] - origin.x_mm(), at[1] - origin.y_mm()]))
                });
                if let Some((span, relative)) = *bridge {
                    let xy = [relative[0] + origin.x_mm(), relative[1] + origin.y_mm()];
                    // Match the sliced check: sub-bead strips do not warn.
                    if span > materials.max_bridge_length && found.area_mm2 / span >= materials.line_width {
                        let mut issue = crate::sliced::bridge_issue(Some(&p.object),
                            [xy[0], xy[1], found.bounds.min[2]], Footprint::of_bounds(found.bounds), span);
                        issue.area_mm2 = found.area_mm2;
                        issue.lowest_z = found.bounds.min[2];
                        issue.highest_z = found.bounds.max[2];
                        issues.push(issue);
                    }
                    continue;
                }
            }
            let OverhangPatch { area_mm2: area, centroid, lowest, bounds: patch, .. } = found;
            if area > materials.patch_area_mm2 {
                let mut contact = Bounds::EMPTY;
                for &t in &found.triangles {
                    for v in mesh.triangles[t as usize] {
                        let p = mesh.positions[v as usize];
                        if p[2] <= materials.first_layer_height + machine::EDGE_SLACK {
                            contact.add_point(p);
                        }
                    }
                }
                let low = lowest[2];
                // A normal alone overstates the risk at a small rounded bed
                // edge. Use only this connected patch's first-layer footprint,
                // not the whole object's box (which could hide a cantilever).
                // This is a conservative mesh-stage note; it does not exempt
                // elevated caps or patches that extend far beyond bed contact.
                let bottom_edge = bounds.min[2].abs() <= machine::EDGE_SLACK
                    && !contact.is_empty() && patch.max[2] <= 3.0
                    && (0..2).all(|d| contact.min[d] - patch.min[d] <= 3.0 && patch.max[d] - contact.max[d] <= 3.0);
                let (severity, message, hint) = if bottom_edge {
                    (Severity::Info, format!("{area:.1} mm² bottom edge rounding/chamfer"),
                        "Small rounding at the bed; check first-layer adhesion and the sliced edge.")
                } else {
                    (if materials.supports { Severity::Info } else { Severity::Warning },
                        format!("{area:.1} mm² steep overhang; supports {}", if materials.supports { "enabled; check sliced coverage" } else { "disabled" }),
                        "Rotate the part, add a chamfer beneath the patch, or enable supports and check the slice.")
                };
                let mut issue = Issue::new("overhang", severity, Some(&p.object), centroid, Footprint::of_bounds(patch), area, message, hint);
                issue.area_mm2 = area; issue.lowest_z = low; issue.highest_z = patch.max[2];
                issues.push(issue);
            }
        }
        issues.extend(islands(p, materials, stop)?);
        Some(PartAnalysis { stats, issues, overhang: degrees })
    }
}

/// Per facet, the root of its shell: triangles joined through shared welded vertices.
/// None once `stop` is true, asked as [`analyse_plate_with`] asks it.
pub(crate) fn shells(mesh: &PartMesh, stop: &dyn Fn() -> bool) -> Option<Vec<usize>> {
    let ids = welded(mesh, stop)?;
    let mut set: Vec<usize> = (0..ids.iter().max().map_or(0, |&n| n + 1)).collect();
    fn find(set: &mut [usize], i: usize) -> usize {
        let mut r = i;
        while set[r] != r { r = set[r]; }
        let mut j = i;
        while set[j] != r { let n = set[j]; set[j] = r; j = n; }
        r
    }
    for (i, t) in mesh.triangles.iter().enumerate() {
        if stopped(i, stop) {
            return None;
        }
        let (a, b, c) = (find(&mut set, ids[t[0] as usize]), find(&mut set, ids[t[1] as usize]), find(&mut set, ids[t[2] as usize]));
        set[a] = c;
        let b = find(&mut set, b);
        set[b] = c;
    }
    Some(mesh.triangles.iter().map(|t| find(&mut set, ids[t[0] as usize])).collect())
}

/// Solid shells of a part that float: a separate closed piece (a positive volume, not a
/// cavity's inner surface) whose lowest point has nothing within a layer below it: no other
/// shell's surface under that point and not the plate. It prints as loose strands in the air
/// (a disk left inside a vase's neck, a speck inside a hollow). None once `stop` is true, asked
/// as [`analyse_plate_with`] asks it.
pub fn islands(p: &Part, materials: &Materials, stop: &dyn Fn() -> bool) -> Option<Vec<Issue>> {
    let mesh = &p.mesh;
    if mesh.triangles.is_empty() {
        return Some(Vec::new());
    }
    let shell = shells(mesh, stop)?;
    let mut shells: HashMap<usize, (f64, [f64; 3], Bounds)> = HashMap::new(); // root -> (volume, lowest point)
    for (i, (t, root)) in mesh.triangles.iter().zip(&shell).enumerate() {
        if stopped(i, stop) {
            return None;
        }
        let [a, b, c] = t.map(|v| mesh.positions[v as usize]);
        let v = (a[0] * (b[1] * c[2] - b[2] * c[1]) - a[1] * (b[0] * c[2] - b[2] * c[0]) + a[2] * (b[0] * c[1] - b[1] * c[0])) / 6.0;
        let e = shells.entry(*root).or_insert((0.0, [0.0, 0.0, f64::INFINITY], Bounds::EMPTY));
        e.0 += v;
        for q in [a, b, c] {
            e.2.add_point(q);
            if q[2] < e.1[2] { e.1 = q; }
        }
    }
    if shells.len() < 2 {
        return Some(Vec::new());
    }
    let base = mesh.bounds().min[2];
    let layer = materials.first_layer_height.max(0.05);
    let mut out = Vec::new();
    let mut roots: Vec<_> = shells.iter().collect();
    roots.sort_by(|a, b| a.1 .1[2].total_cmp(&b.1 .1[2]));
    for (root, (volume, low, bounds)) in roots {
        // A cavity's inner surface (negative volume), a speck, or a shell on the plate.
        if *volume <= 1e-3 || low[2] - base <= layer {
            continue;
        }
        // The highest surface of another shell under the lowest point.
        let mut below = base;
        for (i, (t, s)) in mesh.triangles.iter().zip(&shell).enumerate() {
            if stopped(i, stop) {
                return None;
            }
            if s == root {
                continue;
            }
            let [a, b, c] = t.map(|v| mesh.positions[v as usize]);
            let d = (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0]);
            if d.abs() < 1e-12 {
                continue;
            }
            let u = ((low[0] - a[0]) * (c[1] - a[1]) - (low[1] - a[1]) * (c[0] - a[0])) / d;
            let w = ((b[0] - a[0]) * (low[1] - a[1]) - (b[1] - a[1]) * (low[0] - a[0])) / d;
            if u < -1e-9 || w < -1e-9 || u + w > 1.0 + 1e-9 {
                continue;
            }
            let z = a[2] + u * (b[2] - a[2]) + w * (c[2] - a[2]);
            if z <= low[2] + 1e-6 && z > below {
                below = z;
            }
        }
        let gap = low[2] - below;
        if gap <= layer {
            continue;
        }
        let mut issue = Issue::new(
            "floating_island",
            Severity::Warning,
            Some(&p.object),
            *low,
            Footprint::of_bounds(*bounds),
            gap,
            format!("floating island at z = {:.1} mm (nothing below it for {gap:.1} mm)", low[2]),
            "A separate piece of the part floats in the air or inside a hollow: cut it away (open the cavity fully), join it to the rest, or support it.",
        );
        issue.lowest_z = low[2];
        issue.highest_z = low[2];
        out.push(issue);
    }
    Some(out)
}

fn merge_objects(report: &mut Report) {
    // Multiple parts may belong to one object; retain part-indexed maps but
    // combine statistics for the report's object list.
    let mut indices: HashMap<String, usize> = HashMap::new();
    let mut objects: Vec<ObjectStats> = Vec::new();
    for s in report.objects.drain(..) {
        if let Some(&index) = indices.get(&s.object) {
            let o = &mut objects[index];
            for d in 0..6 { o.overhang_area_mm2[d] += s.overhang_area_mm2[d]; }
            for (z, v) in o.max_height_per_nozzle.iter_mut().zip(s.max_height_per_nozzle) { *z = z.max(v); }
            o.reaching_nozzles.retain(|n| s.reaching_nozzles.contains(n));
        } else {
            indices.insert(s.object.clone(), objects.len());
            objects.push(s);
        }
    }
    report.objects = objects;
}

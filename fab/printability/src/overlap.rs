//! Two objects on one plate whose bodies take the same space: neither prints as drawn. Their
//! boxes first; only where two boxes meet, the exact test: a surface of one crossing into the
//! other, or one inside the other.

use crate::mesh::{stopped, MeshSolid, PartField, Solid, COVER};
use crate::{Footprint, Issue, Severity};
use fab_plate::{Bounds, Part, Plate};
use std::collections::HashSet;

/// The side (mm) under which the test of an exact surface splits a box no further.
const CELL: f64 = 0.5;

/// The surface used to judge a part for overlap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Judged {
    Field,
    Mesh,
    Neither,
}

impl Judged {
    fn of(part: &Part, field: Option<PartField<'_>>) -> Self {
        if field.is_some() {
            Self::Field
        } else if part.mesh.triangles.is_empty() {
            Self::Neither
        } else {
            Self::Mesh
        }
    }
}

/// A plate's overlap issues ([`overlaps`]) and how the check judged each part whose box met
/// another's: what a later check of the same parts, unmoved, reuses when it judges them alike.
#[derive(Clone, Debug, Default)]
pub struct Overlaps {
    pub issues: Vec<Issue>,
    /// None for a part whose box never met another box checked against it.
    judged: Vec<Option<Judged>>,
}

impl Overlaps {
    /// Whether the same kinds of surfaces would be used again. The caller vouches
    /// that `plate` holds the same parts, unmoved.
    pub fn judged_alike<'f>(&self, plate: &Plate, field: &dyn Fn(usize) -> Option<PartField<'f>>) -> bool {
        self.judged.len() == plate.parts.len() && self.judged.iter().enumerate().all(|(k, judged)| {
            judged.is_none_or(|j| j == Judged::of(&plate.parts[k], field(k)))
        })
    }
}

/// An error for each two objects of `plate` whose bodies meet deeper than [`COVER`] (parts that
/// only touch don't), naming both. Bodies of one object ([`Plate::groups`]) print as one, and
/// parts fused by design ([`fab_plate::Part::fused`]) may share space: neither is checked. A
/// part is judged by `field` (its exact surface) when it gives one, else by its closed mesh; a
/// part with neither is not judged. None once `stop` is true (asked as
/// [`crate::analyse_plate_with`] asks it).
pub fn overlaps<'f>(plate: &Plate, field: &dyn Fn(usize) -> Option<PartField<'f>>, stop: &dyn Fn() -> bool) -> Option<Overlaps> {
    let boxes: Vec<Bounds> = plate.parts.iter().map(Part::bounds).collect();
    // built the first time a part's box meets another's
    let mut solids: Vec<Option<Option<Solid>>> = (0..plate.parts.len()).map(|_| None).collect();
    let mut out: Vec<Issue> = Vec::new();
    // the objects named once already (a part is one body of its object)
    let mut named: Vec<(&str, &str)> = Vec::new();
    let object = |k: usize| plate.parts[k].body_of.as_deref().unwrap_or(&plate.parts[k].object);
    for i in 0..plate.parts.len() {
        for j in i + 1..plate.parts.len() {
            let (a, b) = (&plate.parts[i], &plate.parts[j]);
            if a.body_of.is_some() && a.body_of == b.body_of {
                continue;
            }
            // bodies a script fuses on purpose (a bond, a brake's shoe) may share their space
            if a.fused.contains(&b.object) || b.fused.contains(&a.object) {
                continue;
            }
            let Some(meet) = meeting(&boxes[i], &boxes[j]) else { continue };
            if stop() {
                return None;
            }
            for k in [i, j] {
                if solids[k].is_none() {
                    let p = &plate.parts[k];
                    let exact = field(k);
                    solids[k] = Some(match Judged::of(p, exact) {
                        Judged::Field => exact.map(Solid::Field),
                        Judged::Mesh => Some(Solid::Mesh(MeshSolid::new(&p.mesh))),
                        Judged::Neither => None,
                    });
                }
            }
            let (Some(Some(sa)), Some(Some(sb))) = (&solids[i], &solids[j]) else { continue };
            let pair = (object(i), object(j));
            if named.contains(&pair) || !overlap(sa, sb, &meet, stop)? {
                continue;
            }
            let mut issue = Issue::new("overlap", Severity::Error, Some(&a.object), meet.center(), Footprint::of_bounds(meet), (0..3).map(|k| meet.max[k] - meet.min[k]).fold(f64::INFINITY, f64::min),
                format!("{} and {} overlap: their bodies take the same space", a.name, b.name),
                "Move one of them off the other: two parts never print in one place.");
            (issue.lowest_z, issue.highest_z) = (meet.min[2], meet.max[2]);
            out.push(issue);
            named.push(pair);
        }
    }
    let judged = solids.iter().map(|entry| entry.as_ref().map(|solid| match solid {
        Some(Solid::Field(_)) => Judged::Field,
        Some(Solid::Mesh(_)) => Judged::Mesh,
        None => Judged::Neither,
    })).collect();
    Some(Overlaps { issues: out, judged })
}

/// Where two boxes meet, when they meet more than [`COVER`] along every axis: footprints first.
fn meeting(a: &Bounds, b: &Bounds) -> Option<Bounds> {
    let meet = Bounds { min: std::array::from_fn(|k| a.min[k].max(b.min[k])), max: std::array::from_fn(|k| a.max[k].min(b.max[k])) };
    ([0, 1, 2].iter().all(|&k| meet.max[k] - meet.min[k] > COVER) && !a.is_empty() && !b.is_empty()).then_some(meet)
}

/// Whether two solids share a point of `meet` more than [`COVER`] inside both. If they do, a
/// surface of one reaches inside the other (they cross, or one lies inside the other), or for
/// two exact surfaces a point lies inside both. Two meshes split pieces near the other's
/// surface and test one distant piece per shell, so a wholly contained shell is found too.
fn overlap(a: &Solid, b: &Solid, meet: &Bounds, stop: &dyn Fn() -> bool) -> Option<bool> {
    Some(match (a, b) {
        (Solid::Mesh(a), Solid::Mesh(b)) => {
            reaches(a, &|q| b.holds(q), Near::Surface { other: b, shells: a.shells(stop)? }, meet, stop)?
                || reaches(b, &|q| a.holds(q), Near::Surface { other: a, shells: b.shells(stop)? }, meet, stop)?
        }
        (Solid::Mesh(m), Solid::Field(f)) | (Solid::Field(f), Solid::Mesh(m)) => {
            let inside = |q: [f64; 3]| f(q[0], q[1], q[2]) < -COVER;
            // a field changes by at most the distance moved
            let clear = |c: [f64; 3], r: f64| f(c[0], c[1], c[2]) - r >= -COVER;
            reaches(m, &inside, Near::Outside(&clear), meet, stop)? || cells(&[*f], meet, stop)?.is_some_and(|q| m.holds(q))
        }
        (Solid::Field(a), Solid::Field(b)) => cells(&[*a, *b], meet, stop)?.is_some(),
    })
}

/// What the other solid has within a piece's reach: its body, or its mesh's surface.
enum Near<'a, 'mesh> {
    Outside(&'a dyn Fn([f64; 3], f64) -> bool),
    Surface { other: &'a MeshSolid<'mesh>, shells: &'a [usize] },
}

/// Whether the surface of `m` within `meet` reaches inside the solid `inside` tells of: each
/// facet split in two across its longest edge down to [`CELL`] across, each piece's centre
/// taken [`COVER`] and a half into `m` along the facet's outward normal, so a face that only
/// lies on the other's face moves off it. A field sets aside a piece where it has no point
/// within the radius plus the push. A mesh sets aside pieces that far from every facet after
/// testing the first one's pushed centre per shell. A shell with distant pieces on both sides
/// of the other body crosses its surface, where pieces are still tested and split.
fn reaches(m: &MeshSolid, inside: &dyn Fn([f64; 3]) -> bool, near: Near<'_, '_>, meet: &Bounds, stop: &dyn Fn() -> bool) -> Option<bool> {
    let mesh = m.mesh;
    let at = |t: &[u32; 3]| t.map(|v| mesh.positions[v as usize]);
    let sub = |a: [f64; 3], b: [f64; 3]| [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
    let cross = |a: [f64; 3], b: [f64; 3]| [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
    let mid = |a: [f64; 3], b: [f64; 3]| [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5, (a[2] + b[2]) * 0.5];
    // a mesh wound inside out has its normals inward: its volume says so
    let volume: f64 = mesh.triangles.iter().map(|t| { let p = at(t); let c = cross(p[1], p[2]); p[0][0] * c[0] + p[0][1] * c[1] + p[0][2] * c[2] }).sum();
    let outward = if volume < 0.0 { -1.0 } else { 1.0 };
    let off = |p: &[[f64; 3]; 3]| (0..3).any(|k| p.iter().all(|q| q[k] < meet.min[k]) || p.iter().all(|q| q[k] > meet.max[k]));
    let mut todo: Vec<[[f64; 3]; 3]> = Vec::new();
    let mut seen = HashSet::new();
    let mut n = 0;
    for (facet, t) in mesh.triangles.iter().enumerate() {
        let p = at(t);
        let normal = cross(sub(p[1], p[0]), sub(p[2], p[0]));
        let len = (normal[0] * normal[0] + normal[1] * normal[1] + normal[2] * normal[2]).sqrt();
        if len <= 1e-12 || off(&p) {
            continue;
        }
        let push = normal.map(|v| -v / len * outward * COVER * 1.5);
        todo.push(p);
        while let Some(p) = todo.pop() {
            n += 1;
            if stopped(n, stop) {
                return None;
            }
            let c = [(p[0][0] + p[1][0] + p[2][0]) / 3.0, (p[0][1] + p[1][1] + p[2][1]) / 3.0, (p[0][2] + p[1][2] + p[2][2]) / 3.0];
            let r = p.iter().map(|q| ((q[0] - c[0]).powi(2) + (q[1] - c[1]).powi(2) + (q[2] - c[2]).powi(2)).sqrt()).fold(0.0, f64::max);
            let pushed = [c[0] + push[0], c[1] + push[1], c[2] + push[2]];
            match &near {
                Near::Outside(clear) if clear(c, r + COVER * 1.5) => continue,
                Near::Surface { other, shells } if !other.within(c, r + COVER * 1.5) => {
                    if seen.insert(shells[facet]) && inside(pushed) {
                        return Some(true);
                    }
                    continue;
                }
                _ => {}
            }
            if inside(pushed) {
                return Some(true);
            }
            if r * 2.0 <= CELL {
                continue;
            }
            // in two across its longest edge: a long thin facet goes in pieces along its length,
            // as many as its area and length need, not in quarters that stay as thin
            let d = |a: [f64; 3], b: [f64; 3]| (a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2);
            let k = (0..3).max_by(|&i, &j| d(p[i], p[(i + 1) % 3]).total_cmp(&d(p[j], p[(j + 1) % 3]))).unwrap_or(0);
            let (u, v, w) = (p[k], p[(k + 1) % 3], p[(k + 2) % 3]);
            let m = mid(u, v);
            todo.extend([[u, m, w], [m, v, w]].into_iter().filter(|q| !off(q)));
        }
    }
    Some(false)
}

/// A point of `meet` more than [`COVER`] inside every one of `fields` (Some(None): none): the
/// box split in eight down to [`CELL`], a cell set aside where a field shows it clear.
fn cells(fields: &[PartField<'_>], meet: &Bounds, stop: &dyn Fn() -> bool) -> Option<Option<[f64; 3]>> {
    let mut todo = vec![(meet.center(), std::array::from_fn::<f64, 3, _>(|k| (meet.max[k] - meet.min[k]) * 0.5))];
    let mut n = 0;
    while let Some((c, h)) = todo.pop() {
        n += 1;
        if stopped(n, stop) {
            return None;
        }
        let r = (h[0] * h[0] + h[1] * h[1] + h[2] * h[2]).sqrt();
        let (mut inside, mut clear) = (true, false);
        for f in fields {
            let d = f(c[0], c[1], c[2]);
            inside &= d < -COVER;
            clear |= d - r >= -COVER;
        }
        if inside {
            return Some(Some(c));
        }
        if clear {
            continue;
        }
        let split: [bool; 3] = std::array::from_fn(|k| h[k] * 2.0 > CELL);
        if !split.contains(&true) {
            continue;
        }
        let half: [f64; 3] = std::array::from_fn(|k| if split[k] { h[k] * 0.5 } else { h[k] });
        for corner in 0..8usize {
            if (0..3).any(|k| !split[k] && corner >> k & 1 == 1) {
                continue;
            }
            let at = std::array::from_fn(|k| if split[k] { c[k] + if corner >> k & 1 == 1 { half[k] } else { -half[k] } } else { c[k] });
            todo.push((at, half));
        }
    }
    Some(None)
}

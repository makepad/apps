//! Case checks in the board frame: fit reads body boxes (a mesh body: the convex hull of its posed
//! points, the hull once per mesh file and per body) and the case surfaces, reach reads
//! placed port profiles and directions, depth reads their reach, and hold reads mechanical holes
//! and both faces.
//!
//! One 2 mm grid indexes the case's T triangles in O(T) (plus their bbox cell memberships).
//! Body, depth and hole queries visit only overlapping cells; a stamp tests each triangle once
//! per query. Containment uses ray parity in that same index. Each port sweeps its whole profile:
//! triangles in the prism's indexed bounds and overlapping body boxes get clipped and projected
//! to small polygons, then one union and intersection per blocker. A blocked case needs about
//! 12 offset/difference steps to measure widening. Ports are few; this runs once per case build
//! or board change on the evaluator's worker or the pool, never per frame.

use crate::board_mech::{dot, normalised, port_axes};
use crate::{BoardMech, BodyBox, PlacedPort};
use fab_elec_circuit::{CheckKind, Violation};
use fab_elec_board::drc::content_id;
use fab_elec_board::Board;
use fab_elec_core::poly::ring_area2;
use fab_elec_core::{mm, to_mm, CompRef, FeatureRef, PolySet, Sel, Severity, Side, P};
use crate::hull::BodyHull;
use fab_elec_parts::{BodyPortKind, BodySource};
use std::collections::HashMap;

/// A case part's surface in the board's frame (mm): what the checks test the board against.
#[derive(Clone, Debug, PartialEq)]
pub struct CaseSolid {
    pub name: String,
    pub positions: Vec<[f64; 3]>,
    pub triangles: Vec<[u32; 3]>,
}

impl CaseSolid {
    /// `to_board` maps the mesh's positions (mm) into the board's frame (mm), row-major 3x4.
    pub fn mapped(
        name: &str,
        positions: &[[f64; 3]],
        triangles: &[[u32; 3]],
        to_board: &[[f64; 4]; 3],
    ) -> Self {
        Self {
            name: name.into(),
            positions: positions
                .iter()
                .map(|p| {
                    std::array::from_fn(|r| {
                        to_board[r][0] * p[0]
                            + to_board[r][1] * p[1]
                            + to_board[r][2] * p[2]
                            + to_board[r][3]
                    })
                })
                .collect(),
            triangles: triangles.to_vec(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaseCheck {
    Fit,
    Reach,
    Depth,
    Hold,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Error,
    Warning,
    Info,
}

/// A Checks row: what, where (board frame, mm), the feature it is about, and how to fix it.
#[derive(Clone, Debug, PartialEq)]
pub struct CaseFinding {
    pub check: CaseCheck,
    pub level: Level,
    pub feature: FeatureRef,
    pub at: [f64; 3],
    pub message: String,
    pub hint: String,
}

impl CaseFinding {
    /// The finding as a Checks list row of `board`: a mechanical check (rule `case.fit`,
    /// `case.reach`, `case.depth`, `case.hold` or `case.unknown`) about its feature, shown at its plan point,
    /// with its fix hint. Its id is the DRC's content id: kind, rule and the feature by the part's
    /// path (a body or a port) or the hole's geometry, so a waiver survives edits that do not
    /// touch those, and the finding's place in the list never matters.
    pub fn violation(&self, board: &Board) -> Violation {
        let rule = match self.check {
            CaseCheck::Fit => "case.fit",
            CaseCheck::Reach => "case.reach",
            CaseCheck::Depth => "case.depth",
            CaseCheck::Hold => "case.hold",
            CaseCheck::Unknown => "case.unknown",
        };
        let severity = match self.level {
            Level::Error => Severity::Error,
            Level::Warning => Severity::Warning,
            Level::Info => Severity::Info,
        };
        let mut v = Violation::new(CheckKind::Mech, severity, Some(board.unit), vec![Sel::Feature(self.feature.clone())], rule, self.message.clone())
            .at(P::mm(self.at[0], self.at[1]))
            .hint(self.hint.clone());
        v.id = content_id(board, &v.kind, &v.items, &v.rule);
        v
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct CheckOpts {
    /// mm, default 0.2.
    pub clearance: f64,
    /// Hole names that need no standoff.
    pub free: Vec<String>,
    /// The case's clear light pipes in the board's frame ([`crate::bridge::case_pipes`]): a
    /// light with one in front of its mouth, within its reach, shines through it however deep
    /// the case is round it.
    pub pipes: Vec<CaseSolid>,
}

impl Default for CheckOpts {
    fn default() -> Self {
        Self { clearance: 0.2, free: Vec::new(), pipes: Vec::new() }
    }
}

/// Findings in body, port, then hole order. Case surfaces must be closed solids.
pub fn case_checks(mech: &BoardMech, case: &[CaseSolid], opts: &CheckOpts) -> Vec<CaseFinding> {
    let mut index = Index::new(case);
    let mut pipes = (!opts.pipes.is_empty()).then(|| Index::new(&opts.pipes));
    let mut findings = Vec::new();
    let thickness = to_mm(mech.thickness);
    // A body with a mesh file is checked as the convex hull of its posed points, else as its box.
    // What stands in the opening of a port standing out of the housing (a shaft, a key's stem)
    // is the port's: the case opens round it and the port checks test it, so the hull leaves it
    // out (else a shaft's hull is a cone from the housing to its tip, wider than its hole).
    let mut corners: HashMap<&str, Option<Vec<[f64; 3]>>> = HashMap::new();
    let mut points: HashMap<&str, Option<Vec<[f64; 3]>>> = HashMap::new();
    for body in &mech.bodies {
        let standing: Vec<&PlacedPort> = mech.ports.iter().filter(|p| p.comp == body.comp && p.stand > 0).collect();
        let hull = match &body.source {
            Some(BodySource::Mesh(path)) if !standing.is_empty() => points
                .entry(path.as_str())
                .or_insert_with(|| crate::mesh::file_body_points(path).ok())
                .as_ref()
                .and_then(|points| BodyHull::new(&points.iter().map(|p| posed(&body.pose, *p))
                    .filter(|q| !standing.iter().any(|port| stands_in(port, *q))).collect::<Vec<_>>())),
            Some(BodySource::Mesh(path)) => corners
                .entry(path.as_str())
                .or_insert_with(|| crate::mesh::file_body_points(path).ok().and_then(|p| BodyHull::new(&p)).map(|h| h.points))
                .as_ref()
                .and_then(|points| BodyHull::new(&points.iter().map(|p| posed(&body.pose, *p)).collect::<Vec<_>>())),
            _ => None,
        };
        if let Some(finding) = fit(mech, body, hull.as_ref(), case, opts, &mut index) {
            findings.push(finding);
        }
    }
    for port in &mech.ports {
        let feature = FeatureRef::Port(CompRef { board: mech.unit, comp: port.comp }, port.index);
        let (what, lower) = match port.kind {
            BodyPortKind::Plug => ("The plug", "the plug"),
            BodyPortKind::Light => ("The light", "the light"),
            BodyPortKind::Press => ("The button's travel", "the button's travel"),
            BodyPortKind::Air | BodyPortKind::Cable => ("The opening", "the opening"),
            BodyPortKind::Shaft => ("The shaft", "the shaft"),
        };
        let dir = normalised(port.dir);
        let (u, v) = port_axes(dir);
        let at = port.at.map(to_mm);
        let reach = to_mm(port.reach);
        let mouth = |p: fab_elec_core::P| {
            std::array::from_fn(|i| at[i] + to_mm(p.x) * u[i] + to_mm(p.y) * v[i])
        };
        if let Some(finding) = port_blocked(mech, port, case, &mut index, what) {
            findings.push(finding);
        }
        let surround = port.profile.offset(port.margin + mm(0.5), mm(0.02));
        let mut deepest: Option<PortHit> = None;
        for p in surround.rings().flatten() {
            if let Some(hit) = port_hit(&mut index, mouth(*p), dir, reach, true) {
                if deepest.as_ref().is_none_or(|old| hit.distance > old.distance) {
                    deepest = Some(hit);
                }
            }
        }
        let piped = port.kind == BodyPortKind::Light
            && pipes.as_mut().is_some_and(|pipes| port_hit(pipes, at, dir, reach, false).is_some_and(|hit| hit.distance <= reach));
        if let Some(hit) = deepest.filter(|h| h.distance > reach + EPS && !piped) {
            findings.push(CaseFinding {
                check: CaseCheck::Depth,
                level: Level::Error,
                feature,
                at: hit.at,
                message: format!("The case is {} mm deep in front of {}'s mouth; {lower} reaches {} mm", number(hit.distance), port.path, number(reach)),
                hint: format!("Move {} to the wall, or thin the wall round its cutout.", port.path),
            });
        }
    }
    for hole in &mech.holes {
        if opts.free.contains(&hole.name) {
            continue;
        }
        let radius = to_mm(hole.d) / 2.0 + 0.6;
        for (face, sign, level) in [(0.0, -1.0, Level::Warning), (thickness, 1.0, Level::Info)] {
            let mut held = 0;
            let mut nearest: Option<Hit> = None;
            for i in 0..9 {
                let p = if i < 8 {
                    let angle = i as f64 * std::f64::consts::TAU / 8.0;
                    [to_mm(hole.at.x) + radius * angle.cos(), to_mm(hole.at.y) + radius * angle.sin(), face]
                } else if held < 6 {
                    [to_mm(hole.at.x), to_mm(hole.at.y), face]
                } else {
                    break;
                };
                let start = [p[0], p[1], face + sign * 0.01];
                let end = [p[0], p[1], face + sign * 20.0];
                let hits = index.segment(start, end);
                // Flush supports already contain the origin 0.01 mm past the board face;
                // their next surface is the far end of the post, not its contact face.
                if i < 8 && (hits.iter().any(|h| 0.01 + h.fraction * 19.99 <= 0.3 + EPS)
                    || index.inside(start).iter().any(Option::is_some)) {
                    held += 1;
                    continue;
                }
                // How far the support is: only rays that found none measure it.
                for hit in hits {
                    if nearest.as_ref().is_none_or(|old| {
                        hit.fraction.total_cmp(&old.fraction).then(hit.triangle.cmp(&old.triangle)).is_lt()
                    }) {
                        nearest = Some(hit);
                    }
                }
            }
            if held < 6 {
                let (message, hint) = if let Some(hit) = nearest {
                    let part = &case[index.triangles[hit.triangle].solid].name;
                    let distance = number(0.01 + hit.fraction * 19.99);
                    if sign < 0.0 {
                        (format!("Nothing holds {} from below: the {part} is {distance} mm under it", hole.name), format!("Put a standoff under {} that reaches the board ({distance} mm), or waive this if it needs none.", hole.name))
                    } else {
                        (format!("Nothing presses {} from above: the {part} is {distance} mm over it", hole.name), format!("A post in the lid over {} ({distance} mm) holds the board down, or waive this.", hole.name))
                    }
                } else if sign < 0.0 {
                    (format!("Nothing holds {} from below: nothing is under it within 20 mm", hole.name), format!("Put a standoff under {}, or waive this if it needs none.", hole.name))
                } else {
                    (format!("Nothing presses {} from above", hole.name), format!("A post in the lid over {} holds the board down, or waive this.", hole.name))
                };
                findings.push(CaseFinding {
                    check: CaseCheck::Hold,
                    level,
                    feature: hole.feature.clone(),
                    at: [to_mm(hole.at.x), to_mm(hole.at.y), face],
                    message,
                    hint,
                });
            }
        }
    }
    findings
}

/// Whether `q` (board frame, mm) stands in `port`'s opening over its housing: past the housing's
/// face (`stand` behind the tip), no further than the tip, inside the hole the case leaves.
fn stands_in(port: &PlacedPort, q: [f64; 3]) -> bool {
    let dir = normalised(port.dir);
    let offset = sub(q, port.at.map(to_mm));
    let along = dot(offset, dir);
    if along <= 0.05 - to_mm(port.stand) || along > 0.05 {
        return false;
    }
    let (u, v) = port_axes(dir);
    port.opening.contains(P::from_f(dot(offset, u) * 1e6, dot(offset, v) * 1e6))
}

/// A body-frame point (mm) in the board frame (mm), through the body's pose (nm).
pub(crate) fn posed(pose: &fab_elec_core::Xform3, p: [f64; 3]) -> [f64; 3] {
    pose.apply_f(p.map(|v| v * 1e6)).map(|v| v * 1e-6)
}

fn fit(mech: &BoardMech, body: &BodyBox, hull: Option<&BodyHull>, case: &[CaseSolid], opts: &CheckOpts, index: &mut Index) -> Option<CaseFinding> {
    let bounds = hull.map_or_else(|| body_bounds(body, to_mm(mech.thickness)), |h| Bounds { min: h.min, max: h.max });
    // The body's own surface: the hull when it has one, else the box itself.
    let touches = |t: &[[f64; 3]; 3]| hull.map_or_else(|| triangle_box(t, bounds), |h| h.overlaps(t));
    let closest = |t: &[[f64; 3]; 3]| hull.map_or_else(|| box_triangle_closest(bounds, t), |h| h.closest(t));
    let feature = FeatureRef::Body(CompRef { board: mech.unit, comp: body.comp });
    if !body.known && hull.is_none() {
        return Some(CaseFinding {
            check: CaseCheck::Unknown,
            level: Level::Info,
            feature,
            at: bounds.centre(),
            message: format!("{} has no 3D body: its fit is not checked", body.path),
            hint: "Give the part a body (at least its height).".into(),
        });
    }
    let grown = bounds.grown(opts.clearance.max(0.0));
    let mut contact: Vec<Vec<usize>> = vec![Vec::new(); case.len()];
    let mut near: Vec<Vec<usize>> = vec![Vec::new(); case.len()];
    index.visit(grown, |id, triangle| {
        if touches(&triangle.points) {
            contact[triangle.solid].push(id);
        } else if triangle_box(&triangle.points, grown) {
            near[triangle.solid].push(id);
        }
    });
    let inside = index.inside(hull.map_or_else(|| bounds.centre(), |h| h.centre()));
    for (solid, part) in case.iter().enumerate() {
        // The case inside the body: per contact triangle, its part inside the hull (with the
        // triangle's normal); for a box body, its part inside the box.
        let inner: Vec<(Vec<[f64; 3]>, [f64; 3])> = hull.map_or_else(Vec::new, |h| contact[solid].iter().filter_map(|&id| {
            let t = &index.triangles[id].points;
            let polygon = h.clip(t);
            (!polygon.is_empty()).then(|| (polygon, cross(sub(t[1], t[0]), sub(t[2], t[0]))))
        }).collect());
        let clipped = match hull {
            Some(_) => inner.iter().fold(Bounds::EMPTY, |all, (polygon, _)| all.union(Bounds::points(polygon))),
            None => contact[solid].iter().fold(Bounds::EMPTY, |all, &id| all.union(clip_box(&index.triangles[id].points, bounds))),
        };
        let (level, triangle, message, hint) = if let Some(&triangle) = contact[solid].iter().min() {
            // A touch the clip keeps nothing of (an edge or a corner on the box's surface) is
            // still a fault: 0 mm, on the side the touch is.
            let (distance, side) = if clipped.is_empty() {
                (0.0, touch_side(bounds, closest(&index.triangles[triangle].points).0))
            } else if let Some(h) = hull {
                hull_contact(h, &inner, body.side)
            } else {
                fit_contact(bounds, clipped, body.side)
            };
            // A depth that reads as 0 mm at the message's precision is a touch.
            let depth = number(distance);
            (Level::Error, triangle,
                if depth == "0" {
                    format!("{} touches the {} {}", body.path, part.name, side.location())
                } else {
                    format!("{} runs {depth} mm into the {} {}", body.path, part.name, side.location())
                },
                side.hint(&body.path, distance + opts.clearance))
        } else if let Some(triangle) = inside[solid] {
            (Level::Error, triangle,
                format!("{} is inside the {}", body.path, part.name),
                format!("Move {} out of the {}.", body.path, part.name))
        } else if let Some((gap, triangle, on_box, on_triangle)) = near[solid].iter().map(|&id| {
            let (on_box, on_triangle) = closest(&index.triangles[id].points);
            let d = sub(on_triangle, on_box);
            (dot(d, d).sqrt(), id, on_box, on_triangle)
        }).min_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1))).filter(|closest| closest.0 < opts.clearance) {
            // The grown box reaches √3 x the clearance at its corners: only a true gap under it warns.
            let (side, amount) = fit_gap(bounds, gap, on_box, on_triangle, opts.clearance);
            (Level::Warning, triangle,
                format!("{} is {} mm from the {} {}; it needs {} mm", body.path, number(gap), part.name, side.location(), number(opts.clearance)),
                side.hint(&body.path, amount))
        } else {
            continue;
        };
        let points = index.triangles[triangle].points;
        let at = std::array::from_fn(|i| ((points[0][i] + points[1][i] + points[2][i]) / 3.0).clamp(bounds.min[i], bounds.max[i]));
        return Some(CaseFinding { check: CaseCheck::Fit, level, feature, at, message, hint });
    }
    None
}

#[derive(Clone, Copy)]
enum FitSide {
    Above,
    Below,
    PosX,
    NegX,
    PosY,
    NegY,
}

impl FitSide {
    fn location(self) -> &'static str {
        match self {
            Self::Above => "above it",
            Self::Below => "below it",
            Self::PosX => "on its +x side",
            Self::NegX => "on its -x side",
            Self::PosY => "on its +y side",
            Self::NegY => "on its -y side",
        }
    }

    fn hint(self, path: &str, amount: f64) -> String {
        let amount = number((amount * 100.0 - 1e-6).ceil() / 100.0);
        let direction = match self {
            Self::Above => return format!("Raise the roof by {amount} mm (top_gap), or move {path}."),
            Self::Below => return format!("Raise the standoffs by {amount} mm (standoffs: {{height}}), or move {path}."),
            Self::PosX => "-x",
            Self::NegX => "+x",
            Self::PosY => "-y",
            Self::NegY => "+y",
        };
        format!("Move {path} {amount} mm in {direction}.")
    }
}

fn clip_box(points: &[[f64; 3]; 3], bounds: Bounds) -> Bounds {
    let mut polygon = points.to_vec();
    for axis in 0..3 {
        let mut normal = [0.0; 3];
        normal[axis] = 1.0;
        polygon = clip_plane(&polygon, normal, bounds.max[axis]);
        normal[axis] = -1.0;
        polygon = clip_plane(&polygon, normal, -bounds.min[axis]);
    }
    Bounds::points(&polygon)
}

/// A hull body's contact: per move (vertical, then +x, -x, +y, -y, as `fit_contact`), the exact
/// distance the hull must move to leave every polygon of case inside it, measured against the hull's
/// own faces (a slanted or round body would read deeper against its box); the smallest move wins.
fn hull_contact(hull: &BodyHull, inner: &[(Vec<[f64; 3]>, [f64; 3])], side: Side) -> (f64, FitSide) {
    let vertical = match side {
        Side::Top => ([0.0, 0.0, -1.0], FitSide::Above),
        Side::Bottom => ([0.0, 0.0, 1.0], FitSide::Below),
    };
    let moves = [vertical, ([1.0, 0.0, 0.0], FitSide::NegX), ([-1.0, 0.0, 0.0], FitSide::PosX),
        ([0.0, 1.0, 0.0], FitSide::NegY), ([0.0, -1.0, 0.0], FitSide::PosY)];
    let dirs = moves.map(|(u, _)| u);
    let mut need = [0.0f64; 5];
    for (polygon, normal) in inner {
        for (need, t) in need.iter_mut().zip(hull.exits(polygon, *normal, &dirs)) {
            *need = need.max(t);
        }
    }
    let mut cause = (need[0], moves[0].1);
    for (t, (_, side)) in need.into_iter().zip(moves).skip(1) {
        if t < cause.0 {
            cause = (t, side);
        }
    }
    cause
}

fn fit_contact(bounds: Bounds, clipped: Bounds, side: Side) -> (f64, FitSide) {
    let mut cause = match side {
        Side::Top => (bounds.max[2] - clipped.min[2], FitSide::Above),
        Side::Bottom => (clipped.max[2] - bounds.min[2], FitSide::Below),
    };
    // Keep the first equal move: vertical, then +x, -x, +y, -y.
    for candidate in [
        (clipped.max[0] - bounds.min[0], FitSide::NegX),
        (bounds.max[0] - clipped.min[0], FitSide::PosX),
        (clipped.max[1] - bounds.min[1], FitSide::NegY),
        (bounds.max[1] - clipped.min[1], FitSide::PosY),
    ] {
        if candidate.0 < cause.0 {
            cause = candidate;
        }
    }
    cause
}

/// Where a gap of `gap` mm between the body's box and the case points (the axis the closest
/// points differ most along, from the box to the case), and how far the fix along that axis
/// must go for the gap to reach `clearance`: moving t along an axis that makes up a fraction f
/// of the gap's direction opens the gap by t * f.
fn fit_gap(bounds: Bounds, gap: f64, on_box: [f64; 3], on_case: [f64; 3], clearance: f64) -> (FitSide, f64) {
    let d = sub(on_case, on_box);
    if gap <= EPS {
        return (touch_side(bounds, on_box), clearance);
    }
    let axis = [2, 0, 1].into_iter().fold(2, |best, axis| if d[axis].abs() > d[best].abs() + EPS { axis } else { best });
    let side = match (axis, d[axis] > 0.0) {
        (2, true) => FitSide::Above,
        (2, false) => FitSide::Below,
        (0, true) => FitSide::PosX,
        (0, false) => FitSide::NegX,
        (_, true) => FitSide::PosY,
        (_, false) => FitSide::NegY,
    };
    (side, (clearance - gap) * gap / d[axis].abs())
}

/// The side of the box a point on its surface lies on: the axis it is farthest out along,
/// relative to the box's half size (z, then x, then y on a tie: an edge or a corner).
fn touch_side(bounds: Bounds, p: [f64; 3]) -> FitSide {
    let centre = bounds.centre();
    let out = |axis: usize| {
        let half = (bounds.max[axis] - bounds.min[axis]) / 2.0;
        (p[axis] - centre[axis]) / half.max(EPS)
    };
    let axis = [2, 0, 1].into_iter().fold(2, |best, axis| if out(axis).abs() > out(best).abs() + 1e-6 { axis } else { best });
    match (axis, out(axis) > 0.0) {
        (2, true) => FitSide::Above,
        (2, false) => FitSide::Below,
        (0, true) => FitSide::PosX,
        (0, false) => FitSide::NegX,
        (_, true) => FitSide::PosY,
        (_, false) => FitSide::NegY,
    }
}

/// The closest points of a box and a triangle that do not overlap, exact: the closest pair of two
/// convex polytopes is a vertex of one against the other, or an edge of each (3 triangle vertices
/// against the box, 8 corners against the triangle, 3 x 12 edge pairs).
fn box_triangle_closest(bounds: Bounds, t: &[[f64; 3]; 3]) -> ([f64; 3], [f64; 3]) {
    let mut best = (f64::INFINITY, [0.0; 3], [0.0; 3]);
    let mut offer = |p: [f64; 3], q: [f64; 3]| {
        let d = sub(q, p);
        if dot(d, d) < best.0 {
            best = (dot(d, d), p, q);
        }
    };
    for &v in t {
        offer(std::array::from_fn(|i| v[i].clamp(bounds.min[i], bounds.max[i])), v);
    }
    let corners: [[f64; 3]; 8] = std::array::from_fn(|i| std::array::from_fn(|axis| if i & (1 << axis) == 0 { bounds.min[axis] } else { bounds.max[axis] }));
    for corner in corners {
        offer(corner, closest_on_triangle(corner, t));
    }
    for i in 0..8 {
        for axis in 0..3 {
            if i & (1 << axis) == 0 {
                for k in 0..3 {
                    let (p, q) = closest_on_segments(corners[i], corners[i | (1 << axis)], t[k], t[(k + 1) % 3]);
                    offer(p, q);
                }
            }
        }
    }
    (best.1, best.2)
}

fn closest_on_segment(p: [f64; 3], a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    let ab = sub(b, a);
    let length = dot(ab, ab);
    let t = if length <= 1e-24 { 0.0 } else { (dot(sub(p, a), ab) / length).clamp(0.0, 1.0) };
    along(a, ab, t)
}

/// The point of a triangle closest to p, by its Voronoi regions; a degenerate triangle by its edges.
pub(crate) fn closest_on_triangle(p: [f64; 3], t: &[[f64; 3]; 3]) -> [f64; 3] {
    let [a, b, c] = *t;
    let (ab, ac, ap) = (sub(b, a), sub(c, a), sub(p, a));
    let (d1, d2) = (dot(ab, ap), dot(ac, ap));
    if d1 <= 0.0 && d2 <= 0.0 {
        return a;
    }
    let bp = sub(p, b);
    let (d3, d4) = (dot(ab, bp), dot(ac, bp));
    if d3 >= 0.0 && d4 <= d3 {
        return b;
    }
    let vc = d1 * d4 - d3 * d2;
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        return along(a, ab, d1 / (d1 - d3));
    }
    let cp = sub(p, c);
    let (d5, d6) = (dot(ab, cp), dot(ac, cp));
    if d6 >= 0.0 && d5 <= d6 {
        return c;
    }
    let vb = d5 * d2 - d1 * d6;
    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
        return along(a, ac, d2 / (d2 - d6));
    }
    let va = d3 * d6 - d5 * d4;
    if va <= 0.0 && d4 - d3 >= 0.0 && d5 - d6 >= 0.0 {
        return along(b, sub(c, b), (d4 - d3) / ((d4 - d3) + (d5 - d6)));
    }
    let sum = va + vb + vc;
    if sum.abs() <= 1e-24 {
        return [closest_on_segment(p, a, b), closest_on_segment(p, b, c), closest_on_segment(p, c, a)]
            .into_iter().min_by(|x, y| dot(sub(*x, p), sub(*x, p)).total_cmp(&dot(sub(*y, p), sub(*y, p)))).unwrap_or(a);
    }
    along(along(a, ab, vb / sum), ac, vc / sum)
}

/// The closest points of two segments (clamped to both).
pub(crate) fn closest_on_segments(p1: [f64; 3], q1: [f64; 3], p2: [f64; 3], q2: [f64; 3]) -> ([f64; 3], [f64; 3]) {
    let (d1, d2, r) = (sub(q1, p1), sub(q2, p2), sub(p1, p2));
    let (a, e, f) = (dot(d1, d1), dot(d2, d2), dot(d2, r));
    if a <= 1e-24 {
        return (p1, closest_on_segment(p1, p2, q2));
    }
    if e <= 1e-24 {
        return (closest_on_segment(p2, p1, q1), p2);
    }
    let (b, c) = (dot(d1, d2), dot(d1, r));
    let denom = a * e - b * b;
    let mut s = if denom > 1e-12 * a * e { ((b * f - c * e) / denom).clamp(0.0, 1.0) } else { 0.0 };
    let mut t = (b * s + f) / e;
    if t < 0.0 {
        t = 0.0;
        s = (-c / a).clamp(0.0, 1.0);
    } else if t > 1.0 {
        t = 1.0;
        s = ((b - c) / a).clamp(0.0, 1.0);
    }
    (along(p1, d1, s), along(p2, d2, t))
}

/// Sweep the plug's whole profile, retaining each blocker's shadow and nearest crossing.
fn port_blocked(mech: &BoardMech, port: &PlacedPort, case: &[CaseSolid], index: &mut Index, what: &str) -> Option<CaseFinding> {
    let profile_bounds = port.profile.bbox();
    if profile_bounds.is_empty() {
        return None;
    }
    let dir = normalised(port.dir);
    let (u, v) = port_axes(dir);
    let at = port.at.map(to_mm);
    let far = to_mm(port.reach) + 50.0;
    let point = |p: P, w: f64| -> [f64; 3] {
        std::array::from_fn(|i| at[i] + to_mm(p.x) * u[i] + to_mm(p.y) * v[i] + w * dir[i])
    };
    let mut bounds = Bounds::EMPTY;
    for x in [profile_bounds.min.x, profile_bounds.max.x] {
        for y in [profile_bounds.min.y, profile_bounds.max.y] {
            bounds = bounds.union(Bounds::points(&[point(P { x, y }, 0.05), point(P { x, y }, far)]));
        }
    }

    // Case solids precede body boxes, so the combined index also gives the tie order.
    let mut projections = vec![Vec::new(); case.len() + mech.bodies.len()];
    let mut distances = vec![f64::INFINITY; projections.len()];
    let mut project = |solid: usize, points: [[f64; 3]; 3]| {
        let points = points.map(|p| {
            let p = sub(p, at);
            [dot(u, p), dot(v, p), dot(dir, p)]
        });
        let clipped = clip_plane(&points, [0.0, 0.0, 1.0], far);
        let clipped = clip_plane(&clipped, [0.0, 0.0, -1.0], -0.05);
        if clipped.len() < 3 {
            return;
        }
        let ring: Vec<P> = clipped.iter().map(|p| P { x: mm(p[0]), y: mm(p[1]) }).collect();
        // Tunnel side walls seen edge-on must not enter even the normalising boolean.
        if ring_area2(&ring).abs() < 200_000_000 {
            return;
        }
        let shadow = PolySet::from_ring_nonzero(ring);
        // Any overlap counts for the distance: a blocker whose union passes PORT_AREA may be
        // made of triangles that each cover less.
        if shadow.intersection(&port.profile).area() > 0.0 {
            for p in &clipped {
                distances[solid] = distances[solid].min(p[2]);
            }
        }
        projections[solid].push(shadow);
    };
    index.visit(bounds, |_, triangle| project(triangle.solid, triangle.points));
    for (i, body) in mech.bodies.iter().enumerate() {
        if body.comp == port.comp || !body.known {
            continue;
        }
        let boxed = body_bounds(body, to_mm(mech.thickness));
        if boxed.intersection(bounds).is_empty() {
            continue;
        }
        for triangle in box_triangles(boxed) {
            project(case.len() + i, triangle);
        }
    }
    let shadows: Vec<_> = projections.iter().map(PolySet::union_all).collect();
    let hit = shadows.iter().enumerate().filter_map(|(solid, shadow)| {
        let overlap = shadow.intersection(&port.profile);
        if overlap.area() <= PORT_AREA || !distances[solid].is_finite() {
            return None;
        }
        Some(PortHit { solid, distance: distances[solid], at: point(overlap.bbox().center(), distances[solid]) })
    }).min_by(|a, b| a.distance.total_cmp(&b.distance).then(a.solid.cmp(&b.solid)))?;
    let (message, hint) = if hit.solid < case.len() {
        let region = port.opening.clone();
        // A wall can be split across the base/lid joint: its opening clears every case part.
        let case_shadow = PolySet::union_all(&shadows[..case.len()]);
        let open = region.difference(&case_shadow);
        if open.area() > PORT_AREA {
            let widening = cutout_widening(&region, &open);
            (
                format!("{what} for {} is blocked by the {} {} mm out of its mouth", port.path, case[hit.solid].name, number(hit.distance)),
                format!("Widen its cutout by {} mm, or move {} to its opening.", number(widening), port.path),
            )
        } else {
            (
                format!("Nothing opens in front of {}: the {} is {} mm out of its mouth", port.path, case[hit.solid].name, number(hit.distance)),
                format!("Give {} a cutout (the case's ports), or move it to a wall that has one.", port.path),
            )
        }
    } else {
        let body = &mech.bodies[hit.solid - case.len()];
        (
            format!("{what} for {} is blocked by {} {} mm out of its mouth", port.path, body.path, number(hit.distance)),
            format!("Move {} out of its path, or move {}.", body.path, port.path),
        )
    };
    Some(CaseFinding {
        check: CaseCheck::Reach,
        level: Level::Error,
        feature: FeatureRef::Port(CompRef { board: mech.unit, comp: port.comp }, port.index),
        at: hit.at,
        message,
        hint,
    })
}

/// A body's box in the board frame (mm): its plan box, off the face it sits on.
fn body_bounds(body: &BodyBox, thickness: f64) -> Bounds {
    let height = to_mm(body.height);
    let (lo, hi) = match body.side {
        Side::Top => (thickness, thickness + height),
        Side::Bottom => (-height, 0.0),
    };
    Bounds { min: [to_mm(body.min.x), to_mm(body.min.y), lo], max: [to_mm(body.max.x), to_mm(body.max.y), hi] }
}

fn box_triangles(bounds: Bounds) -> [[[f64; 3]; 3]; 12] {
    let corners: [[f64; 3]; 8] = std::array::from_fn(|i| {
        std::array::from_fn(|axis| if i & (1 << axis) == 0 { bounds.min[axis] } else { bounds.max[axis] })
    });
    [
        [0, 2, 3], [0, 3, 1], [4, 5, 7], [4, 7, 6],
        [0, 1, 5], [0, 5, 4], [2, 6, 7], [2, 7, 3],
        [0, 4, 6], [0, 6, 2], [1, 3, 7], [1, 7, 5],
    ].map(|triangle| triangle.map(|i| corners[i]))
}

fn cutout_widening(region: &PolySet, open: &PolySet) -> f64 {
    let bounds = region.bbox();
    let mut lo = 0.0;
    let mut hi = (bounds.width() as f64).hypot(bounds.height() as f64) / 1e6;
    while hi - lo > 0.01 {
        let mid = (lo + hi) / 2.0;
        if region.difference(&open.offset(mm(mid), mm(0.02))).area() <= PORT_AREA {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    (hi * 100.0).ceil() / 100.0
}

/// Significant overlap or opening area in nm² (0.01 mm²).
const PORT_AREA: f64 = 1e10;

struct PortHit {
    solid: usize,
    distance: f64,
    at: [f64; 3],
}

fn port_hit(index: &mut Index, mouth: [f64; 3], dir: [f64; 3], reach: f64, farthest: bool) -> Option<PortHit> {
    let start = along(mouth, dir, 0.05);
    let end = along(mouth, dir, reach + 50.0);
    let hits = index.segment(start, end);
    let hit = hits.iter().min_by(|a, b| {
        let order = a.fraction.total_cmp(&b.fraction);
        (if farthest { order.reverse() } else { order }).then(a.triangle.cmp(&b.triangle))
    })?;
    let distance = 0.05 + hit.fraction * (reach + 50.0 - 0.05);
    Some(PortHit { solid: index.triangles[hit.triangle].solid, distance, at: along(mouth, dir, distance) })
}

fn number(value: f64) -> String {
    let value = format!("{value:.3}");
    value.trim_end_matches('0').trim_end_matches('.').to_string()
}

const CELL: f64 = 2.0;
const EPS: f64 = 1e-9;

#[derive(Clone, Copy)]
struct Bounds {
    min: [f64; 3],
    max: [f64; 3],
}

impl Bounds {
    const EMPTY: Self = Self { min: [f64::INFINITY; 3], max: [f64::NEG_INFINITY; 3] };

    fn points(points: &[[f64; 3]]) -> Self {
        points.iter().fold(Self::EMPTY, |b, p| b.union(Self { min: *p, max: *p }))
    }

    fn union(self, other: Self) -> Self {
        Self { min: std::array::from_fn(|i| self.min[i].min(other.min[i])), max: std::array::from_fn(|i| self.max[i].max(other.max[i])) }
    }

    fn intersection(self, other: Self) -> Self {
        Self { min: std::array::from_fn(|i| self.min[i].max(other.min[i])), max: std::array::from_fn(|i| self.max[i].min(other.max[i])) }
    }

    fn is_empty(self) -> bool {
        (0..3).any(|i| self.min[i] > self.max[i])
    }

    fn contains(self, p: [f64; 3]) -> bool {
        (0..3).all(|i| p[i] >= self.min[i] && p[i] <= self.max[i])
    }

    fn centre(self) -> [f64; 3] {
        std::array::from_fn(|i| (self.min[i] + self.max[i]) / 2.0)
    }

    fn grown(self, delta: f64) -> Self {
        Self { min: self.min.map(|v| v - delta), max: self.max.map(|v| v + delta) }
    }

    fn cells(self, mut visit: impl FnMut([i64; 3])) {
        if self.is_empty() {
            return;
        }
        let lo = self.min.map(|v| (v / CELL).floor() as i64);
        let hi = self.max.map(|v| (v / CELL).floor() as i64);
        for x in lo[0]..=hi[0] {
            for y in lo[1]..=hi[1] {
                for z in lo[2]..=hi[2] {
                    visit([x, y, z]);
                }
            }
        }
    }
}

struct Triangle {
    solid: usize,
    points: [[f64; 3]; 3],
}

struct Hit {
    triangle: usize,
    fraction: f64,
}

struct Index {
    triangles: Vec<Triangle>,
    cells: HashMap<[i64; 3], Vec<usize>>,
    stamps: Vec<u64>,
    stamp: u64,
    bounds: Bounds,
    solid_bounds: Vec<Bounds>,
}

impl Index {
    fn new(case: &[CaseSolid]) -> Self {
        let mut index = Self {
            triangles: Vec::new(),
            cells: HashMap::new(),
            stamps: Vec::new(),
            stamp: 0,
            bounds: Bounds::EMPTY,
            solid_bounds: vec![Bounds::EMPTY; case.len()],
        };
        for (solid, part) in case.iter().enumerate() {
            for triangle in &part.triangles {
                let points = triangle.map(|i| part.positions[i as usize]);
                let bounds = Bounds::points(&points);
                let id = index.triangles.len();
                bounds.grown(EPS).cells(|cell| index.cells.entry(cell).or_default().push(id));
                index.triangles.push(Triangle { solid, points });
                index.solid_bounds[solid] = index.solid_bounds[solid].union(bounds);
                index.bounds = index.bounds.union(bounds);
            }
        }
        index.stamps.resize(index.triangles.len(), 0);
        index
    }

    fn visit(&mut self, bounds: Bounds, mut visit: impl FnMut(usize, &Triangle)) {
        self.stamp = self.stamp.wrapping_add(1);
        if self.stamp == 0 {
            self.stamps.fill(0);
            self.stamp = 1;
        }
        bounds.grown(EPS).intersection(self.bounds.grown(EPS)).cells(|cell| {
            if let Some(ids) = self.cells.get(&cell) {
                for &id in ids {
                    if self.stamps[id] != self.stamp {
                        self.stamps[id] = self.stamp;
                        visit(id, &self.triangles[id]);
                    }
                }
            }
        });
    }

    fn segment(&mut self, start: [f64; 3], end: [f64; 3]) -> Vec<Hit> {
        let mut hits = Vec::new();
        self.visit(Bounds::points(&[start, end]), |triangle, t| {
            if let Some(fraction) = segment_triangle(start, end, &t.points) {
                hits.push(Hit { triangle, fraction });
            }
        });
        hits
    }

    /// The first triangle (input order) of an inside point's parity ray, per containing solid.
    fn inside(&mut self, p: [f64; 3]) -> Vec<Option<usize>> {
        let mut inside = vec![None; self.solid_bounds.len()];
        if !self.solid_bounds.iter().any(|b| b.contains(p)) {
            return inside;
        }
        let dir = normalised([0.5773, 0.5774, 0.5775]);
        let far: [f64; 3] = std::array::from_fn(|i| (p[i] - self.bounds.min[i]).abs().max((p[i] - self.bounds.max[i]).abs()));
        let length = dot(far, far).sqrt() + 1.0;
        let mut hits = self.segment(p, along(p, dir, length));
        hits.retain(|h| h.fraction * length > EPS && self.solid_bounds[self.triangles[h.triangle].solid].contains(p));
        hits.sort_unstable_by(|a, b| {
            self.triangles[a.triangle].solid.cmp(&self.triangles[b.triangle].solid)
                .then(a.fraction.total_cmp(&b.fraction)).then(a.triangle.cmp(&b.triangle))
        });
        let mut begin = 0;
        while begin < hits.len() {
            let solid = self.triangles[hits[begin].triangle].solid;
            let mut end = begin;
            let mut crossings = 0;
            let mut last = f64::NEG_INFINITY;
            let mut first = hits[begin].triangle;
            while end < hits.len() && self.triangles[hits[end].triangle].solid == solid {
                let hit = &hits[end];
                // Adjacent triangles sharing the crossed edge or vertex are one surface hit.
                if (hit.fraction - last) * length > EPS {
                    crossings += 1;
                    last = hit.fraction;
                }
                first = first.min(hit.triangle);
                end += 1;
            }
            if crossings % 2 == 1 {
                inside[solid] = Some(first);
            }
            begin = end;
        }
        inside
    }
}

pub(crate) fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|i| a[i] - b[i])
}

pub(crate) fn along(p: [f64; 3], dir: [f64; 3], distance: f64) -> [f64; 3] {
    std::array::from_fn(|i| p[i] + dir[i] * distance)
}

pub(crate) fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

/// The part of a convex polygon where `dot(normal, p) <= offset` (one Sutherland-Hodgman pass);
/// empty when it lies wholly past the plane.
pub(crate) fn clip_plane(polygon: &[[f64; 3]], normal: [f64; 3], offset: f64) -> Vec<[f64; 3]> {
    let mut out = Vec::with_capacity(polygon.len() + 1);
    clip_plane_into(polygon, normal, offset, &mut out);
    out
}

/// `clip_plane` into a reused buffer (cleared first).
pub(crate) fn clip_plane_into(polygon: &[[f64; 3]], normal: [f64; 3], offset: f64, out: &mut Vec<[f64; 3]>) {
    out.clear();
    for (i, &a) in polygon.iter().enumerate() {
        let b = polygon[(i + 1) % polygon.len()];
        let (da, db) = (dot(normal, a) - offset, dot(normal, b) - offset);
        if da <= 0.0 {
            out.push(a);
        }
        if (da < 0.0 && db > 0.0) || (da > 0.0 && db < 0.0) {
            let t = da / (da - db);
            out.push(std::array::from_fn(|i| a[i] + (b[i] - a[i]) * t));
        }
    }
}

/// Separating axes: the box's three normals, the triangle's normal, and nine edge cross axes.
fn triangle_box(points: &[[f64; 3]; 3], bounds: Bounds) -> bool {
    let centre = bounds.centre();
    let half = sub(bounds.max, centre);
    let p = points.map(|p| sub(p, centre));
    let edges = [sub(p[1], p[0]), sub(p[2], p[1]), sub(p[0], p[2])];
    let separated = |axis: [f64; 3]| {
        let radius = half[0] * axis[0].abs() + half[1] * axis[1].abs() + half[2] * axis[2].abs();
        let projections = p.map(|v| dot(v, axis));
        let tolerance = EPS * dot(axis, axis).sqrt();
        projections.into_iter().fold(f64::INFINITY, f64::min) > radius + tolerance
            || projections.into_iter().fold(f64::NEG_INFINITY, f64::max) < -radius - tolerance
    };
    let axes = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    if axes.into_iter().any(separated) || separated(cross(edges[0], edges[1])) {
        return false;
    }
    !edges.into_iter().any(|edge| axes.into_iter().any(|axis| separated(cross(edge, axis))))
}

/// Two-sided Moller-Trumbore, returning a fraction of the finite segment (including its ends).
fn segment_triangle(start: [f64; 3], end: [f64; 3], points: &[[f64; 3]; 3]) -> Option<f64> {
    let dir = sub(end, start);
    let e1 = sub(points[1], points[0]);
    let e2 = sub(points[2], points[0]);
    let h = cross(dir, e2);
    let det = dot(e1, h);
    if det.abs() <= 1e-12 * (dot(e1, e1) * dot(h, h)).sqrt() {
        return None;
    }
    let s = sub(start, points[0]);
    let u = dot(s, h) / det;
    let q = cross(s, e1);
    let v = dot(dir, q) / det;
    let t = dot(e2, q) / det;
    let tolerance = EPS / dot(dir, dir).sqrt();
    if u >= -1e-12 && v >= -1e-12 && u + v <= 1.0 + 1e-12 && t >= -tolerance && t <= 1.0 + tolerance {
        Some(t.clamp(0.0, 1.0))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fab_elec_core::{CompId, ItemId};
    use fab_elec_fixtures::ledctl;

    /// A closed box mesh between two corners.
    fn block(name: &str, min: [f64; 3], max: [f64; 3]) -> CaseSolid {
        let positions: Vec<[f64; 3]> = box_triangles(Bounds { min, max }).into_iter().flatten().collect();
        let triangles = (0..12).map(|i| [3 * i, 3 * i + 1, 3 * i + 2]).collect();
        CaseSolid { name: name.into(), positions, triangles }
    }

    #[test]
    fn a_touch_at_an_edge_or_a_corner_is_a_fault_and_a_slanted_wall_reads_its_true_gap() {
        let design = ledctl();
        let unit = &design.units[0];
        let board = unit.board.as_deref().expect("the fixture has a board");
        let mech = crate::board_mech(board, &unit.circuit);
        let usb = unit.circuit.comp("usb.j").expect("the fixture has its receptacle").id;
        let body = mech.bodies.iter().find(|b| b.comp == usb).expect("the receptacle has a body box");
        let Bounds { min: lo, max: hi } = body_bounds(body, to_mm(mech.thickness));
        let opts = CheckOpts::default();
        let check = |case: &[CaseSolid]| fit(&mech, body, None, case, &opts, &mut Index::new(case));

        // Only the box's vertical +x +y edge, then only its top +x +y corner, touch the case.
        let edge = check(&[block("Edge", [hi[0], hi[1], lo[2]], [hi[0] + 2.0, hi[1] + 2.0, hi[2]])]).expect("a case touching the box along an edge is a fit fault");
        assert!(edge.level == Level::Error && edge.message.contains("touches the Edge"), "an edge touch is an error that says it touches: {edge:#?}");
        assert!(edge.hint.contains("0.2 mm"), "its fix moves the part the clearance away: {edge:#?}");
        let corner = check(&[block("Corner", hi, [hi[0] + 2.0, hi[1] + 2.0, hi[2] + 2.0])]).expect("a case touching the box at a corner is a fit fault");
        assert!(corner.level == Level::Error && corner.message.contains("touches the Corner"), "a corner touch is an error that says it touches: {corner:#?}");

        // A 45 degree wall 0.1 mm from the box's +x +y edge: no axis separates them, the true gap is 0.1.
        let gap = 0.1;
        let (x0, y0) = (hi[0] + gap / std::f64::consts::SQRT_2, hi[1] + gap / std::f64::consts::SQRT_2);
        let plan = [[x0 + 3.0, y0 - 3.0], [x0 - 3.0, y0 + 3.0], [x0 + 3.0, y0 + 3.0]];
        let positions: Vec<[f64; 3]> = [lo[2] - 1.0, hi[2] + 1.0].into_iter().flat_map(|z| plan.map(|p| [p[0], p[1], z])).collect();
        let triangles = vec![[0, 1, 2], [3, 5, 4], [0, 3, 4], [0, 4, 1], [1, 4, 5], [1, 5, 2], [2, 5, 3], [2, 3, 0]];
        let slant = check(&[CaseSolid { name: "Slant".into(), positions, triangles }]).expect("a wall 0.1 mm off the box warns");
        assert!(slant.level == Level::Warning && slant.message.contains("is 0.1 mm from the Slant"), "the warning reads the true gap: {slant:#?}");
        assert!(slant.hint.contains("0.15 mm in -x"), "moving along x opens the 45 degree gap by 1/sqrt 2 of the move, so 0.1 mm more needs 0.15: {slant:#?}");

        // The same wall 0.25 mm off: inside the grown box at its corner, but clear of it.
        let far = 0.25 / std::f64::consts::SQRT_2 - gap / std::f64::consts::SQRT_2;
        let moved: Vec<[f64; 3]> = [lo[2] - 1.0, hi[2] + 1.0].into_iter().flat_map(|z| plan.map(|p| [p[0] + far, p[1] + far, z])).collect();
        let triangles = vec![[0, 1, 2], [3, 5, 4], [0, 3, 4], [0, 4, 1], [1, 4, 5], [1, 5, 2], [2, 5, 3], [2, 3, 0]];
        assert_eq!(check(&[CaseSolid { name: "Slant".into(), positions: moved, triangles }]), None, "a true gap over the clearance is no finding");
    }

    #[test]
    fn a_case_findings_id_survives_an_unrelated_edit_and_renumbering() {
        let design = ledctl();
        let unit = &design.units[0];
        let board = unit.board.as_deref().expect("the fixture has a board");
        let usb = unit.circuit.comp("usb.j").expect("the fixture has its receptacle").id;
        let mech = crate::board_mech(board, &unit.circuit);
        let plug = mech.ports.iter().find(|p| p.comp == usb).expect("the receptacle has a port").index;
        let hole = mech.holes.iter().find(|h| h.name == "mount1").expect("the fixture has mount1");
        let FeatureRef::Hole(_, hole_id) = hole.feature else { panic!("mount1 is a board hole: {:?}", hole.feature) };
        let findings = |comp: CompId, hole: ItemId, at: f64| {
            let finding = |check, feature| CaseFinding { check, level: Level::Error, feature, at: [at, at, 1.0], message: format!("at {at}"), hint: String::new() };
            let part = CompRef { board: board.unit, comp };
            [finding(CaseCheck::Fit, FeatureRef::Body(part)), finding(CaseCheck::Reach, FeatureRef::Port(part, plug)), finding(CaseCheck::Hold, FeatureRef::Hole(board.unit, hole)), finding(CaseCheck::Depth, FeatureRef::Port(part, plug))]
        };
        let ids = |board: &Board, found: &[CaseFinding]| found.iter().map(|f| f.violation(board).id).collect::<Vec<u64>>();
        let first = ids(board, &findings(usb, hole_id, 5.0));
        assert!(first[0] != first[1] && first[1] != first[2] && first[0] != first[2], "fit, reach and hold on one part are three findings: {first:?}");
        assert_ne!(first[1], first[3], "reach and depth on the same port have distinct ids: {first:?}");

        // Unrelated edits: a track rerouted, another part nudged; the findings move and reword.
        let mut edited = board.clone();
        match edited.tracks[0].path.segs.last_mut().expect("the fixture track has an end") {
            fab_elec_core::Seg::Line(end) | fab_elec_core::Seg::Arc { end, .. } => end.x += mm(1.0),
        }
        let cap = unit.circuit.comp("reg.c_in").expect("the fixture has reg.c_in").id;
        edited.placed.iter_mut().find(|p| p.comp == cap).expect("the fixture places reg.c_in").at.x += mm(1.0);
        assert_eq!(ids(&edited, &findings(usb, hole_id, 7.0)), first, "an edit that touches none of its items keeps each finding's id");

        // Renumbered: every component and the hole under new ids, the same paths and geometry.
        let mut renumbered = edited.clone();
        renumbered.placed.iter_mut().for_each(|p| p.comp = CompId(p.comp.0 + 100));
        renumbered.holes.iter_mut().find(|h| h.id == hole_id).expect("mount1 is a board hole").id = ItemId(9100);
        assert_eq!(ids(&renumbered, &findings(CompId(usb.0 + 100), ItemId(9100), 7.0)), first, "ids key on the part's path and the hole's geometry, not on their numbers");
    }
}

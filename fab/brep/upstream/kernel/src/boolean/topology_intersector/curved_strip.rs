//! Not upstream (tangency-labs/solidcraft#18166, generalising #15718's
//! [`super::coincident_strip`] to a curved face): a surface/surface curve of a face pair
//! that runs, inside a face A, from an end of an edge E of A to A's boundary next to E's
//! other end, over a strip of A that lies within the intersection tolerance of the other
//! face's surface B, is no event: the pair's intersection there is E, which already lies
//! on B, and the curve is dropped from the pair before any edge or vertex definition is
//! made of it ([`drop_strip_crossings`]).
//!
//! A face A that leaves B tangentially along E (a rolling-ball blend leaving the cone it
//! rolls on, whose neighbour across E is B's coincident piece) lies within the tolerance
//! of B over a strip beside E. The surface/surface intersection of A and B is then fixed
//! only to that strip: the marching finds a curve C anywhere in it, from E's end X to the
//! point where it crosses A's boundary edge N next to E's other end V. C cuts a strip off
//! A that is within the tolerance of B (so of A's coincident neighbour) everywhere, and
//! leaves C's end on N and V as two vertices farther apart than any tolerance, which the
//! later merge cannot relate (`make_faceuse`: "the related vertex is not an end of the
//! edge use"). Within the tolerance, E and C are one intersection: E is kept (it is
//! imprinted on B by the coincident pair of B and A's neighbour), C is dropped, and C's
//! end is never made a vertex, so it and V need not be related.
//!
//! C may also run within the tolerance of E from X before it leaves E: its classification
//! on A then reads that stretch as on E and starts C's interval inside A at the point P
//! on E where C leaves it (an end classified on E, not at X). The strip is then the one
//! beside E's piece from P to V, and the same certificate decides it with that piece of
//! E's trim in place of the whole trim. Only C's interval inside A, from P, is dropped;
//! C's stretch along E stays, so P stays an end on E, a kept edge it lies on within the
//! tolerance (no event left far from anything it must be related to, unlike C's end on
//! N, which the event check on N guards).
//!
//! The decision is a certificate over A's strip, not a sample. A's surface `S` is its net
//! (a B-spline face's own surface, or a blend's or sweep's certified net) plus the net's
//! certified distance from its definition, `ε` (0 for a plain B-spline face). The strip is
//! the region of A's parameter plane bounded by E's trim on A, C's UV curve on A over the
//! curve's interval, and the piece of N's trim on A between V and C's end. E's trim and
//! N's are A's own boundary (a face is the region its trims bound), so that region is A's
//! strip beside E whatever the edges' 3D curves are. Two of the pieces are also tied to
//! their 3D curves at the same parameters by [`curve_uv_gap_bound`] (`sup |C(t) −
//! S(W(t))|` enclosed over the whole piece in rounded interval arithmetic, plus `ε`):
//! C's UV curve within the tolerance of C (PROVEN: the curve dropped is the one whose UV
//! image bounds the strip), and N's trim over the piece within the tolerance plus N's
//! tolerance of N's curve (PROVEN: the piece the event check below reads in N's 3D
//! parameters is the strip's side). E needs no such tie and has none: its trim is checked
//! only at its ends against E's vertices (MEASURED on the #18166 join: E's trim lies a
//! certified 3.77e-5 from E's curve, over E's 1e-5 tolerance, a defect of that B-rep the
//! certificate does not rest on). E's trim is assumed to share E's parameter interval:
//! it is cut over that interval, and its two ends are the ones checked against E's
//! vertices, so a trim parametrised otherwise is refused in practice by those checks (the
//! same assumption as for N's trim over N's piece). Each piece is cut into equal
//! parameter pieces, and each of those lies in the box of its certified hull points (`Curve::piece_hull_points`, their coordinates' errors added); where two
//! pieces meet, the segment closing the gap between their ends lies in the box of their
//! end boxes. So the closed boundary lies in the union of those boxes. The span of the
//! boxes in u is cut into equal slabs: a vertical line through the strip meets it between
//! points of its boundary on that line, each in a box meeting the slab, so the strip over
//! a slab lies in the slab times the span in v of the boxes meeting it. Every box must lie
//! in the net's domain (within the extraction's snapping), so no piece of the boundary
//! runs past a seam. A's surface over that box is the restriction of its net: the net's
//! sub-patch over the box (extracted by knot insertion and checked to cover it) is
//! enclosed against B's implicit equation by `implicit::off_surface_bound` (the Bernstein
//! coefficients of the equation, their rounding and the extraction's counted, turned into
//! a distance by the cone's least radius over the patch). When the largest of those bounds
//! plus `ε` is within the tolerance, every point of the strip is within the tolerance of
//! B's surface (PROVEN for the surface, given that the strip is what the three pieces
//! bound).
//!
//! The region is the strip's, not its complement (PROVEN by the checks that refuse
//! otherwise): V's end of N is found by vertex identity, and an N that starts and ends at
//! V is refused ([`end_at`]); the two UV points where two pieces meet (one 3D point) must
//! be less than half the net's domain apart in each direction, so a junction across a
//! periodic seam (where they are a period apart) is refused ([`junction`]); and every box
//! lies in the net's domain. In the parameter plane the three pieces and the junction
//! segments are then one closed curve in the domain, which bounds one region.
//!
//! The drop removes C's end P on N as an event. It is refused while any other interval of
//! any face pair in the intersection ends on N inside the certified piece of N, or at N's
//! end away from V when the piece reaches it ([`event_in_piece`]): such an event would be
//! left on N with nothing from A to relate it to (the class of failure this rule fixes).
//!
//! What is ARGUED, not proven: that the strip lies over B's face, not only its surface: E
//! and C both lie on B's face (E imprinted by the coincident pair, C classified inside
//! it).
//!
//! A crossing past the tolerance is not dropped: where A's points between E and C reach
//! farther from B than the tolerance (A passes to B's other side there, or leaves B before
//! C), the bound exceeds it and the curve stays (the unit test
//! `a_strip_off_b_keeps_its_curve`). A that changes sides of B along C but stays within
//! the tolerance of B between E and C is dropped: at that tolerance A meets B along E and
//! the strip, the case of a face that leaves B tangentially at C, and A past C is off B
//! either way.
//!
//! Nothing is made on the B-rep while deciding: the trims used must already exist (the
//! curve's classification against A makes them), so a refused candidate leaves the
//! B-rep as upstream's path finds it.
//!
//! | Rust | Upstream |
//! |------|----------|
//! | [`drop_strip_crossings`] (super) | none (own design) |
//!
//! Deviations: not upstream at all; always on. B is a plane or a cone (a cylinder
//! included), A a B-spline face (with or without a kept definition) or a procedural
//! blend; every other pair is intersected as upstream does. No recursion.

use crate::curve::curve::p2::Curve;
use crate::error::Result;
use crate::geom::extent1d::Extent1d;
use crate::geom::extent2d::Extent2d;
use crate::geom::solution_array::SolutionObject;
use crate::intersect::curve_class::p1::CurveInterval;
use crate::intersect::point_class::{
    ClassGeometry, ClassObject, PointClassType, PointClassification,
};
use crate::kernel::Kernel;
use crate::math::{Point2, Point3};
use crate::nurbs::b_spline_surface::BSplineSurface;
use crate::nurbs::geom::Rectangle;
use crate::nurbs::nurbs_crv::{is_clamped, nonempty_spans};
use crate::nurbs::srf_basic::Surface as NurbSurface;
use crate::nurbs::srf_tool::p1::{extract_patch, snap as patch_snap};
use crate::query::curve_surface_gap::curve_uv_gap_bound;
use crate::surface::implicit::{off_surface_bound, AnalyticSupport, Implicit};
use crate::surface::surface::p2::SurfaceKind;
use crate::surface::surface::Surface;
use crate::topology::edgeuse::p1 as eu1;
use crate::topology::{edge, vertex, vertexuse, Brep, EdgeId, EdgeuseId, FaceId, VertexId};
use crate::EntityRef;

use super::coincident_strip::FacePairIntersection;
use super::p2::IntersectorBrepP2;

const OP: &str = "boolean::topology_intersector::curved_strip::drop_strip_crossings";

/// Drops from each face pair that is not coincident every curve interval that runs over a
/// strip of one face within `tol` of the other face's surface, beside an edge of the first
/// face that the coincident pair of its neighbour and the other face imprints (module
/// docs). The interval is removed from both faces' classifications.
///
/// Returns the number of intervals dropped.
///
/// # Errors
///
/// Those of the curve and surface evaluations, the topology walks and the enclosure;
/// cancellation.
pub(super) fn drop_strip_crossings<O: ClassObject + SolutionObject + Ord>(
    k: &Kernel,
    brep: &impl IntersectorBrepP2<O>,
    intersections: &mut [FacePairIntersection<O>],
    tol: f64,
) -> Result<usize> {
    let coincident: Vec<(O, O)> = intersections
        .iter()
        .filter(|x| x.2)
        .map(|x| (x.0, x.1))
        .collect();
    if coincident.is_empty() {
        return Ok(0);
    }
    // (decided over the intersection as it stands, then removed: the event check reads
    // every pair)
    let mut dropped: Vec<[usize; 3]> = Vec::new();
    for (pi, (f1, f2, are_coincident, fi, _)) in intersections.iter().enumerate() {
        if *are_coincident {
            continue;
        }
        k.check_cancelled(OP)?;
        for (ci, (c1, c2)) in fi.cc1.iter().zip(&fi.cc2).enumerate() {
            let Some(curve_h) = c1.curve() else {
                continue;
            };
            // the curve's UV curves on the two faces: the surface/surface curve lists
            // hold them beside the 3D curve
            let [cc_3d, cc_uv1, cc_uv2] = &fi.cc_curves;
            let at = cc_3d.iter().position(|&h| Some(h) == c1.curve());
            let uv_on = |list: &[O]| at.and_then(|j| list.get(j).copied());
            for ll in 0..c1.size().min(c2.size()) {
                let (i1, i2) = (c1.at(ll)?, c2.at(ll)?);
                // A is the face the curve runs inside; B the other
                let sides = [(*f2, *f1, i2, uv_on(cc_uv2)), (*f1, *f2, i1, uv_on(cc_uv1))];
                for (fa, fb, ia, uv) in sides {
                    if !shape_fits(ia) {
                        continue;
                    }
                    let (Some(curve), Some(uv)) = (
                        ClassGeometry::curve(brep, curve_h),
                        uv.and_then(|c| ClassGeometry::curve(brep, c)),
                    ) else {
                        continue;
                    };
                    let strip = Strip {
                        fa,
                        fb,
                        interval: ia,
                        curve,
                        uv,
                        coincident: &coincident,
                        all: intersections,
                        own: [pi, ci, ll],
                        tol,
                    };
                    if strip.certified(k, brep)? {
                        dropped.push([pi, ci, ll]);
                        break;
                    }
                }
            }
        }
    }
    for &[pi, ci, ll] in dropped.iter().rev() {
        let Some((_, _, _, fi, _)) = intersections.get_mut(pi) else {
            continue;
        };
        if let Some(c) = fi.cc1.get_mut(ci) {
            c.remove_at(ll)?;
        }
        if let Some(c) = fi.cc2.get_mut(ci) {
            c.remove_at(ll)?;
        }
    }
    Ok(dropped.len())
}

/// Test-only seam ([`crate::kernel::StripProbe`]): makes each run the kernel's probe asks
/// for on a copy of the operation's real intersection list `intersections` at the drop's
/// call, and records the number of intervals each drops.
///
/// # Errors
///
/// Those of [`drop_strip_crossings`].
#[cfg(test)]
pub(super) fn probe<O: ClassObject + SolutionObject + Ord>(
    k: &Kernel,
    brep: &impl IntersectorBrepP2<O>,
    intersections: &[FacePairIntersection<O>],
) -> Result<()> {
    let runs = match k.strip_probe.lock() {
        Ok(mut p) => core::mem::take(&mut p.runs),
        Err(_) => return Ok(()),
    };
    for run in runs {
        if let Ok(mut p) = k.strip_probe.lock() {
            p.active = Some(run);
        }
        let mut copy = intersections.to_vec();
        let got = drop_strip_crossings(k, brep, &mut copy, run.tol);
        if let Ok(mut p) = k.strip_probe.lock() {
            p.active = None;
            if let Ok(n) = got {
                p.dropped.push(n);
            }
        }
        got?;
    }
    Ok(())
}

/// The active probe run's perturbations ([`crate::kernel::StripProbeRun`]): what is added
/// to the strip's bound, the curve's gap and N's piece's gap, and whether a synthetic
/// event lies on N's piece. None outside the crate's unit tests.
#[cfg(test)]
fn perturbed(k: &Kernel) -> ([f64; 3], bool) {
    k.strip_probe
        .lock()
        .ok()
        .and_then(|p| p.active)
        .map_or(([0.0; 3], false), |r| (r.extra, r.event_on_n))
}

/// See the test build's version: no perturbation. Outside the crate's unit tests this is
/// identically `([0.0; 3], false)`, so every use of it is a no-op there: `x + 0.0` and
/// `x − 0.0` are `x` for the finite values it meets, and the synthetic event is
/// `false && …`.
#[cfg(not(test))]
fn perturbed(_k: &Kernel) -> ([f64; 3], bool) {
    ([0.0; 3], false)
}

/// One candidate: the curve interval `interval` of `curve` as face `fa`'s classification
/// reads it, with the curve's UV curve `uv` on `fa`, against face `fb`; `all` the whole
/// intersection and `own` the interval's place in it (pair, curve, interval).
struct Strip<'a, O> {
    fa: O,
    fb: O,
    interval: &'a CurveInterval<O>,
    curve: &'a Curve,
    uv: &'a Curve,
    coincident: &'a [(O, O)],
    all: &'a [FacePairIntersection<O>],
    own: [usize; 3],
    tol: f64,
}

/// Face A: its B-rep and id, its surface, the surface's net and the net's certified
/// distance from the surface `ε`.
struct FaceA<'b> {
    b: &'b Brep,
    fa: FaceId,
    surface: &'b Surface,
    net: BSplineSurface,
    net_bound: f64,
}

impl<O: ClassObject + SolutionObject + Ord> Strip<'_, O> {
    /// Whether the strip the interval cuts off A beside an edge E is certified within
    /// the tolerance of B (module docs): the edges E beside it are found on the B-rep as
    /// it is and each strip enclosed. Nothing is made on the B-rep.
    fn certified(&self, k: &Kernel, brep: &impl IntersectorBrepP2<O>) -> Result<bool> {
        let Some(implicit) = brep
            .face_surface(self.fb)
            .flatten()
            .and_then(|s| ClassGeometry::surface(brep, s))
            .and_then(AnalyticSupport::of)
            .map(|s| s.implicit())
        else {
            return Ok(false);
        };
        // B is a plane or a cone (a cylinder included)
        if !matches!(implicit, Implicit::Plane { .. } | Implicit::Cone { .. }) {
            return Ok(false);
        }
        let Some((b, EntityRef::Face(fa))) = ClassGeometry::topology_entity(brep, self.fa) else {
            return Ok(false);
        };
        let Some(surface) = b.face(fa)?.surface.as_ref() else {
            return Ok(false);
        };
        let Some((net, net_bound)) = net_of(surface) else {
            return Ok(false);
        };
        let a = FaceA {
            b,
            fa,
            surface,
            net,
            net_bound,
        };
        for cand in self.candidates(k, brep, &a)? {
            k.check_cancelled(OP)?;
            if self.strip_beside(k, brep, &a, &implicit, cand)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// The edges E of A the strip may lie beside: the curve's end X is an end of E (a
    /// vertex) or lies on E (the classification's point on E where the curve leaves it,
    /// [`XOn::Edge`]), E's other face is coincident with B in this intersection, and the
    /// curve's other end is E's end V away from X or lies on an edge N of A at V.
    fn candidates(
        &self,
        k: &Kernel,
        brep: &impl IntersectorBrepP2<O>,
        a: &FaceA<'_>,
    ) -> Result<Vec<Candidate>> {
        let mut out = Vec::new();
        let ivl = self.interval;
        let t = ivl.interval;
        let ends = [
            (&ivl.start, t.min(), &ivl.end, t.max()),
            (&ivl.end, t.max(), &ivl.start, t.min()),
        ];
        for (x_class, tx, n_class, tn) in ends {
            let x_on = match x_class.point_class() {
                PointClassType::Vertex => {
                    match x_class.object().and_then(|o| vertex_of(brep, a.b, o)) {
                        Some(x) => XOn::Vertex(x),
                        None => continue,
                    }
                }
                PointClassType::Edge => {
                    match x_class.object().and_then(|o| edge_of(brep, a.b, o)) {
                        Some(e) => XOn::Edge(e, x_class.t_param()),
                        None => continue,
                    }
                }
                _ => continue,
            };
            let far = match n_class.point_class() {
                PointClassType::Edge => {
                    match n_class.object().and_then(|o| edge_of(brep, a.b, o)) {
                        Some(n) => Far::Edge(n, n_class.t_param()),
                        None => continue,
                    }
                }
                PointClassType::Vertex => {
                    match n_class.object().and_then(|o| vertex_of(brep, a.b, o)) {
                        Some(v) => Far::Vertex(v),
                        None => continue,
                    }
                }
                _ => continue,
            };
            match x_on {
                XOn::Vertex(x) => {
                    for e in vertex::edges(a.b, x, Some(a.fa))? {
                        k.check_cancelled(OP)?;
                        if let Some(v) = self.beside(brep, a, e, x, far)? {
                            out.push(Candidate {
                                e,
                                v,
                                far,
                                t: [tx, tn],
                                e_at_x: None,
                            });
                        }
                    }
                }
                XOn::Edge(e, te) => {
                    // (V is either end of E, read as the end away from the other one)
                    for x in [edge::start_vertex(a.b, e)?, edge::end_vertex(a.b, e)?]
                        .into_iter()
                        .flatten()
                    {
                        k.check_cancelled(OP)?;
                        if let Some(v) = self.beside(brep, a, e, x, far)? {
                            out.push(Candidate {
                                e,
                                v,
                                far,
                                t: [tx, tn],
                                e_at_x: Some(te),
                            });
                        }
                    }
                }
            }
        }
        Ok(out)
    }

    /// E's other end V when `e` may bound the strip ([`Strip::candidates`]).
    fn beside(
        &self,
        brep: &impl IntersectorBrepP2<O>,
        a: &FaceA<'_>,
        e: EdgeId,
        x: VertexId,
        far: Far,
    ) -> Result<Option<VertexId>> {
        let b = a.b;
        // E's neighbour across it is a face coincident with B in this intersection
        let faces = edge::faces(b, e, None)?;
        let Some(&neighbour) = faces.iter().find(|&&f| f != a.fa) else {
            return Ok(None);
        };
        if faces.iter().filter(|&&f| f == a.fa).count() != 1 {
            return Ok(None);
        }
        let neighbour_is_coincident = self.coincident.iter().any(|&(g1, g2)| {
            let other = if g1 == self.fb {
                g2
            } else if g2 == self.fb {
                g1
            } else {
                return false;
            };
            matches!(
                ClassGeometry::topology_entity(brep, other),
                Some((ob, EntityRef::Face(f))) if core::ptr::eq(ob, b) && f == neighbour
            )
        });
        if !neighbour_is_coincident {
            return Ok(None);
        }
        let Some(v) = edge::other_vertex(b, e, Some(x))? else {
            return Ok(None);
        };
        if v == x {
            return Ok(None);
        }
        let reaches_v = match far {
            Far::Vertex(w) => w == v,
            Far::Edge(n, _) => {
                n != e
                    && (edge::start_vertex(b, n)? == Some(v) || edge::end_vertex(b, n)? == Some(v))
            }
        };
        Ok(reaches_v.then_some(v))
    }

    /// Whether the strip beside the candidate's edge E is certified (module docs): the
    /// boxes of the pieces of the curve's UV curve, E's trim and N's trim between V and
    /// the curve's end, and of the junctions where they meet; no other event on N's
    /// piece; the net's bound over each slab of the boxes; and each piece's certified gap
    /// to its 3D curve.
    fn strip_beside(
        &self,
        k: &Kernel,
        brep: &impl IntersectorBrepP2<O>,
        a: &FaceA<'_>,
        implicit: &Implicit,
        cand: Candidate,
    ) -> Result<bool> {
        let Candidate {
            e,
            v,
            far,
            t: [tx, tn],
            e_at_x: x_inside_e,
        } = cand;
        let b = a.b;
        let domain = a.net.natural_uv_domain();
        let c_range = [tx.min(tn), tx.max(tn)];

        // the curve's UV curve over its interval (its ends checked first: a cheap filter,
        // the gap below is the certificate)
        for t in [tx, tn] {
            let p = self.curve.evaluate_point(k, t)?;
            if !uv_meets(k, a.surface, self.uv, t, p, self.tol)? {
                return Ok(false);
            }
        }
        let Some(c_boxes) = piece_boxes(self.uv, Extent1d::from_min_max(c_range[0], c_range[1]))
        else {
            return Ok(false);
        };
        // (the box at the curve's end at X, and at its far end)
        let (c_at_x, c_far) = ends(&c_boxes, tx <= tn);
        let c_x_uv = uv_at(k, self.uv, tx)?;
        let c_far_uv = uv_at(k, self.uv, tn)?;

        // E's trim on A between V and the curve's end at E (A's own boundary there): the
        // whole edge when that end is E's end X, else the piece from V to the curve's
        // point on E; checked at the piece's ends (against E's vertex at an end of E,
        // against E's curve inside it)
        let e_ivl = b.edge(e)?.interval;
        let Some(e_trim) = trim_on(b, e, a.fa)? else {
            return Ok(false);
        };
        // (V's end of E by identity: E is not closed, `beside` refuses V = X)
        let Some(e_v_at_start) = end_at(edge::start_vertex(b, e)?, edge::end_vertex(b, e)?, v)
        else {
            return Ok(false);
        };
        let (e_v_param, e_end_param) = if e_v_at_start {
            (e_ivl.min(), e_ivl.max())
        } else {
            (e_ivl.max(), e_ivl.min())
        };
        let e_x_param = x_inside_e.unwrap_or(e_end_param);
        if !(e_ivl.min() <= e_x_param && e_x_param <= e_ivl.max()) {
            return Ok(false);
        }
        for t in [e_v_param, e_x_param] {
            if !trim_meets(k, b, e, a.surface, e_trim, t, self.tol)? {
                return Ok(false);
            }
        }
        let e_piece = Extent1d::from_min_max(e_v_param.min(e_x_param), e_v_param.max(e_x_param));
        let Some(e_boxes) = piece_boxes(e_trim, e_piece) else {
            return Ok(false);
        };
        let (e_at_v, e_at_x) = ends(&e_boxes, e_v_param <= e_x_param);
        let e_v_uv = uv_at(k, e_trim, e_v_param)?;
        let e_x_uv = uv_at(k, e_trim, e_x_param)?;

        let mut boxes: Vec<Extent2d> = Vec::new();
        // the junction where the curve meets E at X
        let Some(jx) = junction(c_at_x, e_at_x, c_x_uv, e_x_uv, &domain) else {
            return Ok(false);
        };
        boxes.push(jx);
        // the piece of N's trim between V and the curve's far end
        let mut n_piece: Option<(&Curve, &Curve, [f64; 2], f64)> = None;
        match far {
            Far::Vertex(_) => {
                let Some(jv) = junction(c_far, e_at_v, c_far_uv, e_v_uv, &domain) else {
                    return Ok(false);
                };
                boxes.push(jv);
            }
            Far::Edge(n, tc) => {
                let rec = b.edge(n)?;
                let (Some(n_curve), n_ivl, n_tol) =
                    (rec.curve.as_ref(), rec.interval, rec.tolerance)
                else {
                    return Ok(false);
                };
                let Some(n_trim) = trim_on(b, n, a.fa)? else {
                    return Ok(false);
                };
                // V's parameter on N, by identity: an N that starts and ends at V (closed)
                // has two, and either piece may be the strip's: refused
                let Some(v_at_start) =
                    end_at(edge::start_vertex(b, n)?, edge::end_vertex(b, n)?, v)
                else {
                    return Ok(false);
                };
                let (tv, tw) = if v_at_start {
                    (n_ivl.min(), n_ivl.max())
                } else {
                    (n_ivl.max(), n_ivl.min())
                };
                if !(n_ivl.min() <= tc && tc <= n_ivl.max()) {
                    return Ok(false);
                }
                for t in [tv, tc] {
                    if !trim_meets(k, b, n, a.surface, n_trim, t, self.tol)? {
                        return Ok(false);
                    }
                }
                let piece = Extent1d::from_min_max(tv.min(tc), tv.max(tc));
                // no other event on N's piece (module docs)
                let w = if v_at_start {
                    edge::end_vertex(b, n)?
                } else {
                    edge::start_vertex(b, n)?
                };
                if self.other_event_on(k, brep, b, n, v, w, tw, piece)? {
                    return Ok(false);
                }
                let Some(n_boxes) = piece_boxes(n_trim, piece) else {
                    return Ok(false);
                };
                let (n_at_v, n_far) = ends(&n_boxes, tv <= tc);
                // the junctions where N's piece meets E at V and the curve at its far end
                let Some(jv) = junction(n_at_v, e_at_v, uv_at(k, n_trim, tv)?, e_v_uv, &domain)
                else {
                    return Ok(false);
                };
                let Some(jc) = junction(n_far, c_far, uv_at(k, n_trim, tc)?, c_far_uv, &domain)
                else {
                    return Ok(false);
                };
                boxes.push(jv);
                boxes.push(jc);
                boxes.extend(n_boxes);
                n_piece = Some((n_curve, n_trim, [piece.min(), piece.max()], n_tol));
            }
        }
        boxes.extend(c_boxes);
        boxes.extend(e_boxes);

        // the strip over each slab of the boxes' span in u, enclosed
        // (`extra` is the test seam's perturbation, 0 outside the unit tests)
        let (extra, _) = perturbed(k);
        if !strip_within(
            k,
            &a.net,
            a.net_bound + extra[0],
            &boxes,
            implicit,
            self.tol,
        )? {
            return Ok(false);
        }

        // the dropped curve and N's piece are their 3D curves' images on S at the same
        // parameters (module docs): the curve within the tolerance, N's piece within it
        // plus N's tolerance
        let floor = 0.01 * self.tol;
        if !gap_within(
            k,
            a,
            self.curve,
            self.uv,
            c_range,
            self.tol - extra[1],
            floor,
        )? {
            return Ok(false);
        }
        if let Some((n_curve, n_trim, range, n_tol)) = n_piece {
            if !gap_within(
                k,
                a,
                n_curve,
                n_trim,
                range,
                self.tol + n_tol - extra[2],
                floor,
            )? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Whether any interval of the intersection other than this one ends on edge `n` of
    /// A's B-rep `b` inside `piece`, or at `n`'s end `w` away from `v` (at parameter `tw`)
    /// when `piece` reaches it ([`event_in_piece`]).
    #[allow(clippy::too_many_arguments)]
    fn other_event_on(
        &self,
        k: &Kernel,
        brep: &impl IntersectorBrepP2<O>,
        b: &Brep,
        n: EdgeId,
        v: VertexId,
        w: Option<VertexId>,
        tw: f64,
        piece: Extent1d,
    ) -> Result<bool> {
        for (pi, (_, _, _, fi, _)) in self.all.iter().enumerate() {
            for (ci, (c1, c2)) in fi.cc1.iter().zip(&fi.cc2).enumerate() {
                for cc in [c1, c2] {
                    for ll in 0..cc.size() {
                        if [pi, ci, ll] == self.own {
                            continue;
                        }
                        let ivl = cc.at(ll)?;
                        for end in [&ivl.start, &ivl.end] {
                            let at = end_on(brep, b, end);
                            if event_in_piece(at, n, v, w, tw, piece) {
                                return Ok(true);
                            }
                        }
                    }
                }
            }
        }
        // (the test seam's synthetic end on N, at its piece's middle: none otherwise, where
        // `perturbed` is identically zero and this is `Ok(false)`)
        let synthetic = EndOn::Edge(n, 0.5 * (piece.min() + piece.max()));
        Ok(perturbed(k).1 && event_in_piece(synthetic, n, v, w, tw, piece))
    }
}

/// Where an interval's end lies on A's B-rep: on an edge at its parameter, at a vertex,
/// or elsewhere (another B-rep, a face, nothing).
#[derive(Clone, Copy, Debug)]
enum EndOn {
    Edge(EdgeId, f64),
    Vertex(VertexId),
    Elsewhere,
}

/// [`EndOn`] of a point classification read on A's B-rep `b`.
fn end_on<O: ClassObject>(
    brep: &impl ClassGeometry<O>,
    b: &Brep,
    end: &PointClassification<O>,
) -> EndOn {
    match end.point_class() {
        PointClassType::Edge => match end.object().and_then(|o| edge_of(brep, b, o)) {
            Some(e) => EndOn::Edge(e, end.t_param()),
            None => EndOn::Elsewhere,
        },
        PointClassType::Vertex => match end.object().and_then(|o| vertex_of(brep, b, o)) {
            Some(x) => EndOn::Vertex(x),
            None => EndOn::Elsewhere,
        },
        _ => EndOn::Elsewhere,
    }
}

/// Whether an interval end `at` is an event on N's certified `piece` other than at V: on
/// edge `n` at a parameter inside `piece` (its ends included; a parameter that is not a
/// number counts as inside), or at N's end `w` away from V (parameter `tw`) when `piece`
/// reaches `tw`. An end at V itself is E's end, kept as an event by the drop.
fn event_in_piece(
    at: EndOn,
    n: EdgeId,
    v: VertexId,
    w: Option<VertexId>,
    tw: f64,
    piece: Extent1d,
) -> bool {
    let inside = |t: f64| !(t < piece.min() || t > piece.max());
    match at {
        EndOn::Edge(e, t) => e == n && inside(t),
        EndOn::Vertex(x) => x != v && Some(x) == w && inside(tw),
        EndOn::Elsewhere => false,
    }
}

/// Which end of an edge running from `start` to `end` is the vertex `v`: `Some(true)` at
/// its start, `Some(false)` at its end; `None` when neither is, or both are (a closed
/// edge, whose two pieces either may be the strip's).
fn end_at(start: Option<VertexId>, end: Option<VertexId>, v: VertexId) -> Option<bool> {
    match (start == Some(v), end == Some(v)) {
        (true, false) => Some(true),
        (false, true) => Some(false),
        _ => None,
    }
}

/// The box closing the junction of two pieces whose ends' boxes are `a` and `b` and whose
/// UV points there are `pa` and `pb` (one 3D point): the box of both boxes, which holds
/// the segment between the points. `None` when the points are half the net's `domain` or
/// more apart in u or in v, or are not numbers: across a periodic seam they are a period
/// apart, and the segment is not the strip's boundary.
fn junction(
    a: Extent2d,
    b: Extent2d,
    pa: Point2,
    pb: Point2,
    domain: &Extent2d,
) -> Option<Extent2d> {
    let (lo, hi) = (domain.min(), domain.max());
    let near =
        (pa.x - pb.x).abs() < 0.5 * (hi.x - lo.x) && (pa.y - pb.y).abs() < 0.5 * (hi.y - lo.y);
    near.then(|| union(a, b))
}

/// The UV point of a UV curve or trim at `t`.
fn uv_at(k: &Kernel, uv: &Curve, t: f64) -> Result<Point2> {
    let q = uv.evaluate_point(k, t)?;
    Ok(Point2::new(q.x, q.y))
}

/// Whether `bound` plus the net's `net_bound`, grown by its roundings, is within `reach`
/// (not a number is not within).
fn within(bound: f64, net_bound: f64, reach: f64) -> bool {
    (bound + net_bound) * (1.0 + 4.0 * f64::EPSILON) <= reach
}

/// Whether the 3D curve `curve` lies within `reach` of A's surface at its UV curve `uv`'s
/// points over `range`: [`curve_uv_gap_bound`] against A's net, plus the net's bound.
/// Either curve not a B-spline (or a kind derived from one), or a gap not enclosed, is
/// not within.
fn gap_within(
    k: &Kernel,
    a: &FaceA<'_>,
    curve: &Curve,
    uv: &Curve,
    range: [f64; 2],
    reach: f64,
    floor: f64,
) -> Result<bool> {
    let (Some(c), Some(w)) = (curve.b_spline_base(), uv.b_spline_base()) else {
        return Ok(false);
    };
    let Some(gap) = curve_uv_gap_bound(k, c, w, a.surface, range, floor)? else {
        return Ok(false);
    };
    Ok(within(gap, a.net_bound, reach))
}

/// The number of equal parameter pieces each boundary piece's hull is taken over, and of
/// equal slabs in u the strip is enclosed over.
const PIECES: u32 = 32;

/// The boxes of the hulls of `uv` over [`PIECES`] equal pieces of `ivl` in parameter
/// order, each widened by its points' errors: every point of `uv` over `ivl` lies in one.
/// `None` when a hull cannot be taken or is not finite.
fn piece_boxes(uv: &Curve, ivl: Extent1d) -> Option<Vec<Extent2d>> {
    let (lo, hi) = (ivl.min(), ivl.max());
    if !(lo <= hi && lo.is_finite() && hi.is_finite()) {
        return None;
    }
    let mut boxes = Vec::new();
    let mut a = lo;
    for i in 1..=PIECES {
        // (the last piece ends at `hi` exactly: the pieces cover the interval)
        let b = if i == PIECES {
            hi
        } else {
            lo + (hi - lo) * (f64::from(i) / f64::from(PIECES))
        };
        let b = b.max(a);
        let mut piece = Extent2d::new();
        for (q, err) in uv.piece_hull_points(&Extent1d::from_min_max(a, b))? {
            if !(q.x.is_finite() && q.y.is_finite() && err.is_finite()) {
                return None;
            }
            piece.add_point(Point2::new(q.x - err, q.y - err));
            piece.add_point(Point2::new(q.x + err, q.y + err));
        }
        let held = piece.min().x <= piece.max().x;
        if !held {
            return None;
        }
        boxes.push(piece);
        a = b;
    }
    Some(boxes)
}

/// The boxes at a piece's two ends: (at its parameter's start, at its end) when
/// `start_first`, else swapped.
fn ends(boxes: &[Extent2d], start_first: bool) -> (Extent2d, Extent2d) {
    let first = boxes.first().copied().unwrap_or_default();
    let last = boxes.last().copied().unwrap_or_default();
    if start_first {
        (first, last)
    } else {
        (last, first)
    }
}

/// The box of two boxes: it holds every segment between their points (where two pieces'
/// ends meet across a gap).
fn union(a: Extent2d, b: Extent2d) -> Extent2d {
    let mut u = a;
    u.add_point(b.min());
    u.add_point(b.max());
    u
}

/// The strip cut into [`PIECES`] equal slabs of the boxes' span in u, each the slab times
/// the span in v of the boxes meeting it (`None` for a slab no box meets). A vertical
/// line through the strip meets it between points of its boundary on that line, and each
/// such point lies in a box meeting the slab, so the strip over a slab lies in its box.
fn slabs(boxes: &[Extent2d]) -> Vec<Option<Extent2d>> {
    let mut span = Extent2d::new();
    for bx in boxes {
        span.add_point(bx.min());
        span.add_point(bx.max());
    }
    let (u0, u1) = (span.min().x, span.max().x);
    let mut out = Vec::new();
    let mut a = u0;
    for i in 1..=PIECES {
        let b = if i == PIECES {
            u1
        } else {
            u0 + (u1 - u0) * (f64::from(i) / f64::from(PIECES))
        };
        let b = b.max(a);
        let mut v: Option<(f64, f64)> = None;
        for bx in boxes {
            if bx.min().x <= b && a <= bx.max().x {
                let (lo, hi) = v.unwrap_or_else(|| (bx.min().y, bx.max().y));
                v = Some((lo.min(bx.min().y), hi.max(bx.max().y)));
            }
        }
        out.push(v.map(|(lo, hi)| Extent2d::from_bounds(a, lo, b, hi)));
        a = b;
    }
    out
}

/// Whether A's net plus its bound `net_bound` lies within `tol` of B's surface
/// (`implicit`) over the strip whose boundary lies in `boxes` (module docs): every slab
/// of [`slabs`] meets a box, lies in the net's domain, and its sub-patch's
/// [`off_surface_bound`] plus `net_bound`, grown by its roundings, is within `tol`.
///
/// # Errors
///
/// Cancellation (polled at every slab).
fn strip_within(
    k: &Kernel,
    net: &BSplineSurface,
    net_bound: f64,
    boxes: &[Extent2d],
    implicit: &Implicit,
    tol: f64,
) -> Result<bool> {
    let mut worst = 0.0_f64;
    for slab in slabs(boxes) {
        k.check_cancelled(OP)?;
        let Some(slab) = slab else {
            return Ok(false);
        };
        let Some(bound) = net_bound_over(net, &slab, implicit) else {
            return Ok(false);
        };
        worst = worst.max(bound);
        if !within(worst, net_bound, tol) {
            return Ok(false);
        }
    }
    Ok(true)
}

/// An edge E the strip may lie beside: E, its end V away from the curve's end at E,
/// the curve's far end, the curve's parameters at its two ends (at E first), and E's
/// parameter at the curve's end on E when that end lies on E rather than at E's end X.
#[derive(Clone, Copy)]
struct Candidate {
    e: EdgeId,
    v: VertexId,
    far: Far,
    t: [f64; 2],
    e_at_x: Option<f64>,
}

/// Where the curve's end at E lies: at a vertex X (an end of the candidate E), or on the
/// edge E at E's parameter. The second is the classification of a curve that runs
/// within the tolerance of E from E's end and leaves it: the curve's interval in A then
/// starts where it leaves E, on E.
#[derive(Clone, Copy)]
enum XOn {
    Vertex(VertexId),
    Edge(EdgeId, f64),
}

/// Where the curve ends away from E: on A's boundary edge N at N's parameter, or at a
/// vertex.
#[derive(Clone, Copy)]
enum Far {
    Edge(EdgeId, f64),
    Vertex(VertexId),
}

/// A's net and its certified distance `ε` from A's surface: a B-spline surface is its own
/// net, `ε` the bound of the definition it is kept as the net of (a round's, a corner's, a
/// draft's, a section blend's or a sweep's) or 0 for a plain one; a procedural blend its
/// net with the net's bound. `None` for every other kind, and for a bound that is not a
/// finite number at least 0.
fn net_of(surface: &Surface) -> Option<(BSplineSurface, f64)> {
    let net = match surface.kind() {
        SurfaceKind::BSpline(b) => b,
        SurfaceKind::ProceduralBlend(pb) => pb.net(),
        _ => return None,
    };
    let bound = match surface.procedural_definition() {
        Some(definition) => definition.bound(),
        None if matches!(surface.kind(), SurfaceKind::BSpline(_)) => 0.0,
        // (a procedural blend always has its definition)
        None => return None,
    };
    // (`clone` fixes the knots up, which can move near-coincident ones: the net certified
    // is the face's own)
    (bound >= 0.0 && bound.is_finite()).then(|| (net.clone_exact(), bound))
}

/// The bound of [`off_surface_bound`] over the net's sub-patch covering `box_uv`: the
/// net's knots in u and in v must be finite and non-decreasing (each knot insertion's
/// ratio then lies in `[0, 1]`, as the rounding count assumes) and clamped with end
/// multiplicity exactly `d + 1` (the extraction takes the net as clamped: on an unclamped
/// net its patch at an end on the valid domain's end knot is not A, and above `d + 1` A's
/// point at the domain end is a control point the cut drops), the box
/// must lie in the net's domain within the extraction's snapping (a box past it would
/// hold points of no face, or of the surface across a seam), and the sub-patch is
/// extracted over the box widened by that snapping and must cover the box; else `None`.
///
/// The sub-patch is A's own surface over its domain, by construction: it is cut by
/// [`extract_patch`] from A's NURB directly, its ends first snapped here as the extraction
/// would, and refused unless each snapped end is a fixed point of that snap (so the
/// extraction's snap moves nothing and no reparametrisation rescales or shifts its knots;
/// where A has knots closer than the snapping, a second snap could move an end to an
/// earlier knot and shift the patch off A), and it is never passed through a knot fixup (`copy_sub_patch` and
/// `from_gw_nurb` spread knots closer than 1.2e-8 without moving the control points, a
/// different surface the bound would not count). Its knots must then be bit-equal to A's
/// inside its domain and its ends the snapped box's, else `None`.
///
/// A bound that cannot be made is `None` too, a refusal: the drop is then not decided and
/// the pair goes down today's path (an interior knot of multiplicity `d + 1` makes the
/// Bézier decomposition fail, and that must not fail the boolean).
fn net_bound_over(net: &BSplineSurface, box_uv: &Extent2d, implicit: &Implicit) -> Option<f64> {
    let cut = net_cut(net, box_uv)?;
    // (borrowed: no knot fixup)
    off_surface_bound(&BSplineSurface::from_nurb(cut, true), implicit, net)
        .ok()
        .flatten()
}

/// [`net_bound_over`]'s sub-patch of `net` covering `box_uv`, its knots `net`'s own;
/// `None` where there is none, and for a net whose knots do not meet
/// [`net_bound_over`]'s conditions (unclamped, non-monotone, non-finite, or an end
/// multiplicity other than `d + 1`).
fn net_cut(net: &BSplineSurface, box_uv: &Extent2d) -> Option<NurbSurface> {
    let nurb = net.gw_nurb()?;
    let natural = net.natural_uv_domain();
    let (n_lo, n_hi) = (natural.min(), natural.max());
    // (the extraction snaps a bound to a knot within 1e-8: widen past it, and check)
    let widen = |a: f64| 2.0e-8 * (1.0 + a.abs());
    let (lo, hi) = (box_uv.min(), box_uv.max());
    let inside = n_lo.x - widen(n_lo.x) <= lo.x
        && n_lo.y - widen(n_lo.y) <= lo.y
        && hi.x <= n_hi.x + widen(n_hi.x)
        && hi.y <= n_hi.y + widen(n_hi.y);
    if !inside {
        return None;
    }
    // (A's points are its surface's over its domain: the box is cut to the domain, the
    // hulls' errors may reach past it)
    let (lo, hi) = (
        Point2::new(lo.x.max(n_lo.x), lo.y.max(n_lo.y)),
        Point2::new(hi.x.min(n_hi.x), hi.y.min(n_hi.y)),
    );
    if !(lo.x <= hi.x && lo.y <= hi.y) {
        return None;
    }
    let (_, _, r, s) = nurb.array_sizes();
    let uk = nurb.u_knots().get(..=r)?;
    let vk = nurb.v_knots().get(..=s)?;
    // the extraction takes the net as clamped: at an end on `k_p` or `k_{r-p}` of an
    // unclamped net its patch is not A (its domain `[k_0, k_r]` is wider than the valid
    // `[k_p, k_{r-p}]` too), so only clamped nets are cut, as `srf_piece_box` requires
    let (p, q) = nurb.degrees();
    if !is_clamped(uk, p) || !is_clamped(vk, q) {
        return None;
    }
    // and as a net of finite, non-decreasing knots (each insertion's ratio in [0, 1], as
    // the rounding count assumes) whose ends have multiplicity exactly `d + 1` (a first
    // or last point with no support is still A's point at the domain end, and the cut
    // would drop it), else no cut
    if !knots_fit_a_cut(uk, p, nurb.n()) || !knots_fit_a_cut(vk, q, nurb.m()) {
        return None;
    }
    // the widened box in the domain (as `copy_sub_patch` cut it), each end snapped as
    // `extract_patch` snaps it: to the first of the knots `0..r` within its tolerance
    let snap = |x: f64, knots: &[f64]| {
        let mut x = x;
        let _ = patch_snap(
            &mut x,
            knots,
            isize::try_from(knots.len().saturating_sub(1)).unwrap_or(0),
        );
        x
    };
    let rect = Rectangle {
        ul: snap((lo.x - widen(lo.x)).max(n_lo.x), uk),
        ur: snap((hi.x + widen(hi.x)).min(n_hi.x), uk),
        vb: snap((lo.y - widen(lo.y)).max(n_lo.y), vk),
        vt: snap((hi.y + widen(hi.y)).min(n_hi.y), vk),
    };
    // the extraction snaps each end again: only where that snap is the identity is its
    // patch A over `rect` (a knot within the tolerance of a snapped end would move it,
    // and the reparametrisation back to `rect` would shift the patch off A)
    let fixed = snap(rect.ul, uk) == rect.ul
        && snap(rect.ur, uk) == rect.ur
        && snap(rect.vb, vk) == rect.vb
        && snap(rect.vt, vk) == rect.vt;
    if !fixed {
        return None;
    }
    let cut = extract_patch(nurb, rect).ok()?;
    let (_, _, cr, cs) = cut.array_sizes();
    let (p, q) = cut.degrees();
    let own = knots_are_nets(cut.u_knots().get(..=cr)?, uk, p, rect.ul, rect.ur)
        && knots_are_nets(cut.v_knots().get(..=cs)?, vk, q, rect.vb, rect.vt);
    let covers = rect.ul <= lo.x && rect.vb <= lo.y && hi.x <= rect.ur && hi.y <= rect.vt;
    (own && covers).then_some(cut)
}

/// Whether a clamped knot vector of degree `d` with last control index `n` can be cut:
/// `n + d + 2` finite, non-decreasing knots ([`nonempty_spans`], as `srf_piece_box`
/// requires) whose first and last values occur exactly `d + 1` times, and whose range
/// `last − first` is finite (each insertion's ratio divides by a span: an overflowing one
/// is infinite and the ratio 0, a cut that is not the net's).
fn knots_fit_a_cut(knots: &[f64], d: usize, n: usize) -> bool {
    let len = knots.len();
    let simple_end = |i: Option<usize>| {
        i.and_then(|i| Some((knots.get(i)?, knots.get(i.checked_add(1)?)?)))
            .is_some_and(|(a, b)| a < b)
    };
    let finite_range = match (knots.first(), knots.last()) {
        (Some(&a), Some(&b)) => (b - a).is_finite(),
        _ => false,
    };
    nonempty_spans(knots, d, n).is_some()
        && finite_range
        && simple_end(Some(d))
        && simple_end(len.checked_sub(d.saturating_add(2)))
}

/// Whether the knots `cut` of a patch of degree `d` cut from a net with knots `net` over
/// `[a, b]` are the net's own: `d + 1` of `a`, the net's knots strictly between `a` and
/// `b` bit for bit, and `d + 1` of `b`.
fn knots_are_nets(cut: &[f64], net: &[f64], d: usize, a: f64, b: f64) -> bool {
    let ends = d.saturating_add(1);
    let Some(inner) = cut.len().checked_sub(ends) else {
        return false;
    };
    let (Some(head), Some(tail), Some(mid)) = (
        cut.get(..ends),
        cut.get(inner..),
        cut.get(ends.min(inner)..inner),
    ) else {
        return false;
    };
    inner >= ends
        && head.iter().all(|&k| k == a)
        && tail.iter().all(|&k| k == b)
        && mid
            .iter()
            .copied()
            .eq(net.iter().copied().filter(|&k| a < k && k < b))
}

/// Whether edge `e`'s UV trim `trim` on `surface` meets the edge at parameter `t`: at an
/// end of the edge's interval it meets the end's vertex within `tol` plus the vertex's
/// tolerance (the edge's curve and its trims each end within that of the vertex), inside
/// the interval the edge's curve within `tol` plus the edge's tolerance. A cheap filter
/// before the certified gap ([`gap_within`]).
///
/// # Errors
///
/// Those of the topology walk and of the evaluations.
fn trim_meets(
    k: &Kernel,
    b: &Brep,
    e: EdgeId,
    surface: &Surface,
    trim: &Curve,
    t: f64,
    tol: f64,
) -> Result<bool> {
    let rec = b.edge(e)?;
    let (Some(curve), ivl, edge_tol) = (rec.curve.as_ref(), rec.interval, rec.tolerance) else {
        return Ok(false);
    };
    let end = if t == ivl.min() {
        edge::start_vertex(b, e)?
    } else if t == ivl.max() {
        edge::end_vertex(b, e)?
    } else {
        None
    };
    match end {
        Some(v) => uv_meets(
            k,
            surface,
            trim,
            t,
            vertex::point(b, v)?,
            tol + vertex::tolerance(b, v)?,
        ),
        None => uv_meets(
            k,
            surface,
            trim,
            t,
            curve.evaluate_point(k, t)?,
            tol + edge_tol,
        ),
    }
}

/// Whether the surface's point at `uv_curve(t)` lies within `reach` of `p`.
fn uv_meets(
    k: &Kernel,
    surface: &Surface,
    uv_curve: &Curve,
    t: f64,
    p: Point3,
    reach: f64,
) -> Result<bool> {
    let q = uv_curve.evaluate_point(k, t)?;
    let s = surface.evaluate_point(k, Point2::new(q.x, q.y))?;
    Ok(s.distance(p) <= reach)
}

/// Whether a curve interval can be a strip's curve on its face: it runs inside the face
/// and one of its ends is a vertex or on an edge (the end at E).
fn shape_fits<O: ClassObject>(ivl: &CurveInterval<O>) -> bool {
    let on_boundary = |c: &PointClassification<O>| {
        matches!(
            c.point_class(),
            PointClassType::Vertex | PointClassType::Edge
        )
    };
    ivl.mid.point_class() == PointClassType::Face
        && (on_boundary(&ivl.start) || on_boundary(&ivl.end))
}

/// An edge use of edge `e` on face `fa`, where the edge meets the face once: its uses
/// on the face are one use and its mate (the face's two sides). `None` for none, or for
/// a use besides those (a seam, the face on both sides of the edge).
fn single_edgeuse_on(b: &Brep, e: EdgeId, fa: FaceId) -> Result<Option<EdgeuseId>> {
    let mut found: Option<(EdgeuseId, Option<EdgeuseId>)> = None;
    for eu in edge::edgeuses(b, e, None, None)? {
        if eu1::face(b, eu)? != Some(fa) {
            continue;
        }
        match found {
            None => found = Some((eu, eu1::mate(b, eu)?)),
            Some((first, mate)) => {
                if eu != first && Some(eu) != mate {
                    return Ok(None);
                }
            }
        }
    }
    Ok(found.map(|(eu, _)| eu))
}

/// The UV trim curve on face `fa` of edge `e`'s single edge use there, as the B-rep holds
/// it (none is made: a missing trim refuses the strip).
fn trim_on(b: &Brep, e: EdgeId, fa: FaceId) -> Result<Option<&Curve>> {
    match single_edgeuse_on(b, e, fa)? {
        // (its own or its mate's: both on the face)
        Some(eu) => Ok(eu1::uv_trim_curve(b, eu)?.0),
        None => Ok(None),
    }
}

/// The vertex a classification object of A's B-rep names (a vertex or its use).
fn vertex_of<O: ClassObject>(brep: &impl ClassGeometry<O>, b: &Brep, o: O) -> Option<VertexId> {
    match ClassGeometry::topology_entity(brep, o)? {
        (ob, EntityRef::Vertex(v)) if core::ptr::eq(ob, b) => Some(v),
        (ob, EntityRef::Vertexuse(vu)) if core::ptr::eq(ob, b) => vertexuse::vertex(b, vu).ok()?,
        _ => None,
    }
}

/// The edge a classification object of A's B-rep names (an edge or its use).
fn edge_of<O: ClassObject>(brep: &impl ClassGeometry<O>, b: &Brep, o: O) -> Option<EdgeId> {
    match ClassGeometry::topology_entity(brep, o)? {
        (ob, EntityRef::Edge(e)) if core::ptr::eq(ob, b) => Some(e),
        (ob, EntityRef::Edgeuse(eu)) if core::ptr::eq(ob, b) => eu1::edge(b, eu).ok()?,
        _ => None,
    }
}


// Modified for Makepad by fab/brep/sync/tool (fab/brep/SYNC.md): S1.

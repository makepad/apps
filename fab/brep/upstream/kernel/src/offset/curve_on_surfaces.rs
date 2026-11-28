//! Whether a curve is proved to lie within a distance of two surfaces: the check
//! `MateEdges` makes before it keeps an offset edge whose ends match the mated intersection
//! (not upstream).
//!
//! Upstream: none (not upstream; solidcore-native).
//!
//! | Rust                     | Upstream |
//! |--------------------------|----------|
//! | [`curve_on_surfaces`]    | none     |
//!
//! **The bound.** For a curve `C` and a surface `S`, split the curve's parameter range into
//! segments `[a, b]`, each inside one polynomial piece of `C`. At both ends take any surface
//! point `S(w_a)`, `S(w_b)` (the surface's closest point; any point gives an upper bound) and
//! join them in the surface's parameters by the straight line `w(t)`. The distance of `C(t)`
//! from `S` is at most `|g(t)|`, `g = C - S(w)`, and linear interpolation of `g` misses it
//! by at most `h^2 / 8 sup |g''|` (`h = b - a`): so the distance is at most
//! `max(|g(a)|, |g(b)|) + h^2 / 8 sup |g''|` on the segment. With `w' = (du, dv) / h`
//! constant, `g'' = C'' - (S_uu du^2 + 2 S_uv du dv + S_vv dv^2) / h^2`. `C(a)`, `C(b)`,
//! `C''` and the surface's point and second partials are enclosed in rounded interval
//! arithmetic ([`curve_derivs_over`], [`partials_over`]) over the segment and the parameter
//! box of `w`. A segment over the distance is cut in two, to [`MAX_DEPTH`] halvings.
//!
//! **Offsets.** For an offset `O = S + d n` of a B-spline net (`n = N / |N|`,
//! `N = S_u x S_v`), `S` is replaced by `O`: its point by the net's point plus `d N / |N|`
//! (enclosed by interval division, `|N|` proved at least twice `EFF_ZERO`, where the offset
//! takes that normal), and its second derivative along
//! `w` by the net's plus `d n''`, bounded by `|n''| <= 2 |N''| / |N| + 5 |N'|^2 / |N|^2`
//! (from `n'' = N''/r - 2 N' r'/r^2 + N (2 r'^2 / r^3 - r''/r^2)`, `r = |N|`, `|r'| <= |N'|`,
//! `|r''| <= (|N'|^2 + r |N''|) / r`), `N'` and `N''` along `w` from the net's partials to
//! the third order. An offset is checked only over its net's own domain, where it is that
//! formula (outside it the offset may evaluate an extension).
//!
//! The interpolation bound needs `g` once continuously differentiable: the segments are cut
//! at the curve's knots, and where the line `w` crosses a crease line of the surface (an
//! interior knot of multiplicity at least its degree, for an offset one less: there the
//! parameterisation is not continuously differentiable, as at the quarter arcs of a
//! rational cylinder), at a point on that line exactly, so each piece lies in one closed
//! patch; a piece that still crosses one is not proved there.
//!
//! **Seams.** Of a foot's parameters (a point on a seam or pole has several) the one nearest
//! the segment's other foot is taken. Where the two feet still lie more than half the
//! domain apart in a coordinate (the curve runs across a closed net's seam), the segment is
//! cut where the line between them, the short way round, leaves the domain: the first piece
//! ends on that domain edge, the second starts on the opposite one, each bounded on its own
//! (its end points need only be surface points; a net not closed there leaves a gap that is
//! not proved).
//!
//! **Planes and cones.** A plane or a cone (a cylinder is one) is bounded in closed form by
//! its distance function instead (module `analytic`): a line by its segment, an ellipse by
//! the rational NURB it evaluates through, a B-spline as above.
//!
//! The curve is taken as the kernel evaluates it: a B-spline, an ellipse's NURB, a line's
//! analytic segment (`Line::segment_over`, rounding included); a NURBS curve only over its
//! own domain `[u_p, u_n]` (past it a polynomial piece's continuation is not the curve).
//!
//! Deviations: none (no upstream counterpart). Checked: a B-spline or ellipse on B-spline
//! nets or offsets of them, and a line, ellipse or B-spline on planes and cones; anything
//! else (spheres, tori, surfaces of revolution, blends, offsets of analytic surfaces) is
//! not proved (`false`). Recursion: none (explicit stacks, at most [`MAX_DEPTH`] halvings of
//! each knot span and [`MAX_SEGMENTS`] segments in all for one check, past which nothing is
//! proved).

use crate::blend::fillet_section::certificate::{curve_derivs_over, NurbsCurve};
use crate::blend::fillet_section::interval::{round_down, round_up, Ivl};
use crate::blend::fillet_section::series::V3;
use crate::blend::fillet_section::station::Nurbs;
use crate::curve::curve::{Curve, CurveKind};
use crate::error::{ignored, Result};
use crate::geom::extent1d::Extent1d;
use crate::geom::extent2d::Extent2d;
use crate::geom::vector2d::Point2;
use crate::kernel::Kernel;
use crate::mass::enclosure::partials_over;
use crate::math::Point3;
use crate::nurbs::b_spline_curve::BSplineCurve;
use crate::nurbs::math::CPoint;
use crate::query::api_queries::surface_closest_point;
use crate::surface::surface::p2::SurfaceKind;
use crate::surface::surface::Surface;

mod analytic;

use analytic::{on_analytic, Analytic, Path};

const OP: &str = "offset::curve_on_surfaces";

/// The most halvings of one knot span of the curve.
pub const MAX_DEPTH: u32 = 24;

/// The most segments one check examines, over all the curve's knot spans and both
/// surfaces; past it the curve is not proved.
pub const MAX_SEGMENTS: usize = 1 << 16;

/// Whether a net normal `N` of length at least `r` is the one an offset takes there:
/// `OffsetSurface::point_with` uses `N / |N|` only where `|N|^2` is at least `EFF_ZERO^2`
/// and another normal below, so the bound holds only above it (twice `EFF_ZERO`, a margin
/// for the rounding of its `|N|^2`).
fn normal_as_offset(r: f64) -> bool {
    r >= 2.0 * crate::math::EFF_ZERO
}

/// Charges one segment to `budget`: `false` when it is spent.
fn charge(budget: &mut usize) -> bool {
    match budget.checked_sub(1) {
        Some(left) => {
            *budget = left;
            true
        }
        None => false,
    }
}

/// The first segments a knot span is cut into.
const FIRST_SEGMENTS: usize = 16;

/// A surface the check handles: a B-spline net, or an offset of one by `d` over the net's
/// domain.
struct Checked {
    net: Nurbs,
    offset: Option<(f64, Extent2d)>,
}

impl Checked {
    /// `None` for any other kind, and for a surface (or an offset's base) that carries a
    /// procedural definition: the kernel evaluates that exactly, not its net, so a bound on
    /// the net would prove nothing about it (as `offset_executive::face` guards).
    fn of(k: &Kernel, s: &Surface) -> Option<Self> {
        if s.procedural_definition().is_some() {
            return None;
        }
        match s.kind() {
            SurfaceKind::BSpline(b) => Some(Self {
                net: Nurbs::of(b).ok()?,
                offset: None,
            }),
            SurfaceKind::OffsetSurface(o) => {
                if o.base_surface().procedural_definition().is_some() {
                    return None;
                }
                let SurfaceKind::BSpline(b) = o.base_surface().kind() else {
                    return None;
                };
                let d = o.offset_distance();
                let domain = o.base_surface().natural_uv_domain_with_nurb(k).ok()?;
                d.is_finite().then_some(())?;
                Some(Self {
                    net: Nurbs::of(b).ok()?,
                    offset: Some((d, domain)),
                })
            }
            _ => None,
        }
    }

    /// Whether the box lies where the bound holds.
    fn covers(&self, u: Ivl, v: Ivl) -> bool {
        self.offset.as_ref().is_none_or(|(_, dom)| {
            u.lo >= dom.u_min() && u.hi <= dom.u_max() && v.lo >= dom.v_min() && v.hi <= dom.v_max()
        })
    }
}

/// Whether `curve` over `interval` is proved within `tol` of each of `surfaces` (module
/// docs). `false` where it is not proved: the kinds are not checked, a closest point is not
/// found, an enclosure fails or a segment stays over `tol` after [`MAX_DEPTH`] halvings.
///
/// # Errors
///
/// [`SolidError::Cancelled`](crate::error::SolidError::Cancelled) when the kernel's cancel
/// token fires (polled once per segment); none from the checks themselves.
pub fn curve_on_surfaces(
    k: &Kernel,
    curve: &Curve,
    interval: Extent1d,
    surfaces: [&Surface; 2],
    tol: f64,
) -> Result<bool> {
    if !(tol > 0.0 && interval.min().is_finite() && interval.max().is_finite()) {
        return Ok(false);
    }
    // the curve as the kernel evaluates it: a B-spline, an ellipse through its rational
    // NURB, a line by its analytic segment
    let nurbs = match curve.kind() {
        CurveKind::BSpline(bs) => NurbsCurve::of(bs).ok(),
        CurveKind::Ellipse(e) => NurbsCurve::of(&e.base).ok(),
        _ => None,
    };
    // the interval must lie in the curve's own domain `[u_p, u_n]`: past it a polynomial
    // piece's continuation is not the curve
    if let Some(cn) = &nurbs {
        let (Some(&u0), Some(&u1)) = (cn.u.get(cn.p), cn.u.get(cn.n)) else {
            return Ok(false);
        };
        if interval.min() < u0 || interval.max() > u1 {
            return Ok(false);
        }
    }
    // every segment either check examines, for both surfaces, counts against one budget
    let mut budget = MAX_SEGMENTS;
    let segment = match curve.kind() {
        CurveKind::Line(l) => l.segment_over(&interval).map(|[(p, e), (q, f)]| {
            let boxed = |p: Point3, e: f64| V3 {
                x: Ivl::checked(p.x - e, p.x + e),
                y: Ivl::checked(p.y - e, p.y + e),
                z: Ivl::checked(p.z - e, p.z + e),
            };
            [boxed(p, e), boxed(q, f)]
        }),
        _ => None,
    };
    // a line on a net: its rounded segment as a degree 1 B-spline over [0, 1], and the
    // slack its ends' rounding takes (each coordinate within e, so a point within 2 e)
    let segment_curve = match curve.kind() {
        CurveKind::Line(l) => l.segment_over(&interval).and_then(|[(p, e), (q, f)]| {
            let pw = [
                CPoint::new(p.x, p.y, p.z, 1.0),
                CPoint::new(q.x, q.y, q.z, 1.0),
            ];
            let bs = BSplineCurve::from_data(3, 1, 3, 1, &pw, &[0.0, 0.0, 1.0, 1.0]).ok()?;
            let sn = NurbsCurve::of(&bs).ok()?;
            let slack = round_up(2.0 * e.max(f));
            Some((Curve::from(bs), sn, slack))
        }),
        _ => None,
    };
    for s in surfaces {
        let proved = if let Some(a) = Analytic::of(s) {
            match (&nurbs, segment) {
                (Some(cn), _) => on_analytic(k, &Path::Nurbs(cn), &a, interval, tol, &mut budget)?,
                (None, Some(seg)) => on_analytic(
                    k,
                    &Path::Segment(seg),
                    &a,
                    Extent1d::from_min_max(0.0, 1.0),
                    tol,
                    &mut budget,
                )?,
                (None, None) => false,
            }
        } else {
            match (&nurbs, &segment_curve, Checked::of(k, s)) {
                (Some(cn), _, Some(c)) => {
                    on_surface(k, cn, curve, s, &c, interval, tol, &mut budget)?
                }
                // a line: its rounded segment, within the line's rounding less
                (None, Some((seg, sn, slack)), Some(c)) => {
                    let unit = Extent1d::from_min_max(0.0, 1.0);
                    let inner = round_down(tol - slack);
                    inner > 0.0 && on_surface(k, sn, seg, s, &c, unit, inner, &mut budget)?
                }
                _ => false,
            }
        };
        if !proved {
            return Ok(false);
        }
    }
    Ok(true)
}

/// The net's crease lines: its distinct interior knots of multiplicity at least its degree
/// (one less for an offset, whose normal takes one more derivative), in u and in v. Across
/// one the surface's parameterisation is not continuously differentiable.
fn crease_lines(c: &Checked) -> (Vec<f64>, Vec<f64>) {
    let less = usize::from(c.offset.is_some());
    let lines = |knots: &[f64], p: usize| {
        let limit = p.saturating_sub(less).max(1);
        let interior = knots
            .get(p + 1..knots.len().saturating_sub(p + 1))
            .unwrap_or(&[]);
        let mut out: Vec<f64> = Vec::new();
        let mut run = 0;
        for (i, &x) in interior.iter().enumerate() {
            run = if i > 0 && interior.get(i - 1) == Some(&x) {
                run + 1
            } else {
                1
            };
            if run >= limit && out.last() != Some(&x) {
                out.push(x);
            }
        }
        out
    };
    (lines(&c.net.u, c.net.p), lines(&c.net.v, c.net.q))
}

/// The segment `[a, b]` with surface points `wa`, `wb` cut where the straight line between
/// them crosses a crease line: the nodes `(t, w)`, ends included, each cut point on its line
/// exactly (any surface point at a node gives the bound).
fn cut_at_creases(
    lines: &(Vec<f64>, Vec<f64>),
    a: f64,
    b: f64,
    wa: Point2,
    wb: Point2,
) -> Vec<(f64, Point2)> {
    let (mut sa, mut sb) = (wa, wb);
    let mut cuts: Vec<(f64, Point2)> = Vec::new();
    for (axis, ls) in [(0, &lines.0), (1, &lines.1)] {
        let (ca, cb) = if axis == 0 {
            (wa.x, wb.x)
        } else {
            (wa.y, wb.y)
        };
        for &l in ls {
            if !(ca.min(cb) < l && l < ca.max(cb)) {
                continue;
            }
            let lam = (l - ca) / (cb - ca);
            let on_line = |w: &mut Point2| {
                if axis == 0 {
                    w.x = l;
                } else {
                    w.y = l;
                }
            };
            // a crossing that rounds onto an end moves that end's point onto the line, in
            // `lam` or in its parameter `t` (a cut whose `t` rounds to `b` would leave a piece
            // of no length past it, which the bound cannot take; with the end's point on the
            // line, the piece before it runs up to the line and covers `t` to `b` itself)
            if lam.is_nan() || lam <= 0.0 || a + lam * (b - a) <= a {
                on_line(&mut sa);
                continue;
            }
            if lam >= 1.0 || a + lam * (b - a) >= b {
                on_line(&mut sb);
                continue;
            }
            let w = if axis == 0 {
                Point2::new(l, wa.y + lam * (wb.y - wa.y))
            } else {
                Point2::new(wa.x + lam * (wb.x - wa.x), l)
            };
            cuts.push((lam, w));
        }
    }
    cuts.sort_by(|x, y| x.0.total_cmp(&y.0));
    let mut nodes = vec![(a, sa)];
    for (lam, w) in cuts {
        nodes.push((a + lam * (b - a), w));
    }
    nodes.push((b, sb));
    nodes
}

/// The segment `[a, b]` with surface points `wa`, `wb` split where it runs across the net's
/// parameter domain the short way round, through a seam: where `wa` and `wb` lie more than
/// half the domain apart in a coordinate, the line from `wa` to `wb` shifted by the
/// domain's length is cut where it crosses the domain's edge, the first piece ending on
/// that edge and the second starting on the opposite one, at the same parameter `t` (each
/// piece is bounded on its own, so its ends need only be surface points; a surface not
/// closed there leaves a gap the bound does not prove). Pieces `(ta, pa, tb, pb)`.
fn split_at_seams(
    net: &Nurbs,
    a: f64,
    b: f64,
    wa: Point2,
    wb: Point2,
) -> Vec<(f64, Point2, f64, Point2)> {
    let mut pieces = vec![(a, wa, b, wb)];
    for axis in [0, 1] {
        let (knots, deg, n) = if axis == 0 {
            (&net.u, net.p, net.nu)
        } else {
            (&net.v, net.q, net.nv)
        };
        let (Some(&d0), Some(&d1)) = (knots.get(deg), knots.get(n)) else {
            continue;
        };
        let len = d1 - d0;
        let coord = |w: Point2| if axis == 0 { w.x } else { w.y };
        let mut out = Vec::new();
        for (ta, pa, tb, pb) in pieces {
            let (ca, cb) = (coord(pa), coord(pb));
            if !(len > 0.0 && (cb - ca).abs() > 0.5 * len) {
                out.push((ta, pa, tb, pb));
                continue;
            }
            // up through d1 into d0, or down through d0 into d1
            let (shifted, leave, enter) = if cb < ca {
                (cb + len, d1, d0)
            } else {
                (cb - len, d0, d1)
            };
            let lam = (leave - ca) / (shifted - ca);
            let (oa, ob) = if axis == 0 {
                (pa.y, pb.y)
            } else {
                (pa.x, pb.x)
            };
            let at = |c: f64, other: f64| {
                if axis == 0 {
                    Point2::new(c, other)
                } else {
                    Point2::new(other, c)
                }
            };
            // a crossing at an end: that end is taken on the seam's other edge instead
            if lam.is_nan() || lam <= 0.0 {
                out.push((ta, at(enter, oa), tb, pb));
                continue;
            }
            if lam >= 1.0 {
                out.push((ta, pa, tb, at(leave, ob)));
                continue;
            }
            let t = ta + lam * (tb - ta);
            let other = oa + lam * (ob - oa);
            out.push((ta, pa, t, at(leave, other)));
            out.push((t, at(enter, other), tb, pb));
        }
        pieces = out;
    }
    pieces
}

/// Whether the parameter box of `p`, `q` holds no crease line inside it (it lies in one
/// closed patch).
fn in_one_patch(lines: &(Vec<f64>, Vec<f64>), p: Point2, q: Point2) -> bool {
    let open = |ls: &[f64], x: f64, y: f64| ls.iter().all(|&l| !(x.min(y) < l && l < x.max(y)));
    open(&lines.0, p.x, q.x) && open(&lines.1, p.y, q.y)
}

/// The surface parameters of the surface point nearest the curve's at `t` (a seam or a pole
/// has several), `None` where none is found.
///
/// # Errors
///
/// A cancellation or an unported dependency of the evaluation or the closest-point
/// solve (see [`ignored`]); any other failure is `None` (the segment is not proved).
fn feet_at(k: &Kernel, s: &Surface, t: f64, curve: &Curve) -> Result<Option<Vec<Point2>>> {
    let Some(p) = ignored(curve.evaluate_point(k, t))? else {
        return Ok(None);
    };
    let Some(cp) = ignored(surface_closest_point(k, s, p))? else {
        return Ok(None);
    };
    Ok(Some(cp.uvs))
}

/// Of the parameters `uvs` of one surface point, the one nearest `near` when given (so a
/// segment's two feet are taken on the same side of a seam), else the first; finite ones
/// only.
fn pick_foot(uvs: &[Point2], near: Option<Point2>) -> Option<Point2> {
    let finite = |uv: &&Point2| uv.x.is_finite() && uv.y.is_finite();
    match near {
        None => uvs.iter().find(finite).copied(),
        Some(w) => uvs
            .iter()
            .filter(finite)
            .min_by(|x, y| x.distance(w).total_cmp(&y.distance(w)))
            .copied(),
    }
}

/// The closest points one [`on_surface`] check has found, by the curve parameter's bits
/// (not upstream). Neighbouring segments share their ends, and a halved segment its
/// parent's: each point is solved for once. The memo holds what [`feet_at`] returned, so a
/// foot is picked from the same parameters as without it.
#[derive(Default)]
struct Feet {
    found: std::collections::HashMap<u64, Option<Vec<Point2>>>,
}

impl Feet {
    /// A surface parameter of the surface point nearest the curve's at `t` ([`feet_at`],
    /// [`pick_foot`]), `None` where none is found.
    ///
    /// # Errors
    ///
    /// Those of [`feet_at`].
    fn foot(
        &mut self,
        k: &Kernel,
        s: &Surface,
        t: f64,
        curve: &Curve,
        near: Option<Point2>,
    ) -> Result<Option<Point2>> {
        let uvs = match self.found.entry(t.to_bits()) {
            std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
            std::collections::hash_map::Entry::Vacant(e) => e.insert(feet_at(k, s, t, curve)?),
        };
        Ok(uvs.as_deref().and_then(|uvs| pick_foot(uvs, near)))
    }
}

type IV3 = V3<Ivl>;

/// An upper bound of `|v|`, rounding included.
fn norm_up(v: IV3) -> f64 {
    let (x, y, z) = (v.x.mag(), v.y.mag(), v.z.mag());
    round_up(round_up(round_up(x * x + y * y) + z * z).sqrt())
}

/// A lower bound of `|v|`, rounding included (0 where it is not proved positive).
fn norm_down(v: IV3) -> f64 {
    let sq = |c: Ivl| {
        if c.lo > 0.0 {
            round_down(c.lo * c.lo)
        } else if c.hi < 0.0 {
            round_down(c.hi * c.hi)
        } else {
            0.0
        }
    };
    let s = round_down(round_down(sq(v.x) + sq(v.y)) + sq(v.z));
    if s > 0.0 {
        round_down(s.sqrt())
    } else {
        0.0
    }
}

fn add(a: IV3, b: IV3) -> IV3 {
    V3 {
        x: a.x + b.x,
        y: a.y + b.y,
        z: a.z + b.z,
    }
}

fn sub(a: IV3, b: IV3) -> IV3 {
    V3 {
        x: a.x - b.x,
        y: a.y - b.y,
        z: a.z - b.z,
    }
}

fn scale(a: IV3, c: Ivl) -> IV3 {
    V3 {
        x: a.x * c,
        y: a.y * c,
        z: a.z * c,
    }
}

fn cross(a: IV3, b: IV3) -> IV3 {
    V3 {
        x: a.y * b.z - a.z * b.y,
        y: a.z * b.x - a.x * b.z,
        z: a.x * b.y - a.y * b.x,
    }
}

/// An upper bound of `|C(t) - O(w)|`, `None` where an enclosure fails.
fn gap_at(cn: &NurbsCurve, c: &Checked, t: f64, w: Point2) -> Option<f64> {
    let (u, v) = (Ivl::from(w.x), Ivl::from(w.y));
    if !c.covers(u, v) {
        return None;
    }
    let cv = *curve_derivs_over(cn, Ivl::from(t), 0, None).ok()?.first()?;
    let order = usize::from(c.offset.is_some());
    let p = partials_over(&c.net, u, v, order).ok().flatten()?;
    let mut sp = p.at(0, 0);
    if let Some((d, _)) = c.offset {
        let n = cross(p.at(1, 0), p.at(0, 1));
        if !normal_as_offset(norm_down(n)) {
            return None;
        }
        let len = (n.x * n.x + n.y * n.y + n.z * n.z).sqrt();
        let unit = V3 {
            x: n.x / len,
            y: n.y / len,
            z: n.z / len,
        };
        sp = add(sp, scale(unit, Ivl::from(d)));
    }
    let g = norm_up(sub(cv, sp));
    g.is_finite().then_some(g)
}

/// An upper bound of `h^2 / 8 sup |g''|` over the segment (module docs).
fn bend(cn: &NurbsCurve, c: &Checked, a: f64, b: f64, wa: Point2, wb: Point2) -> Option<f64> {
    let (u, v) = (
        Ivl::new(wa.x.min(wb.x), wa.x.max(wb.x)),
        Ivl::new(wa.y.min(wb.y), wa.y.max(wb.y)),
    );
    if !c.covers(u, v) {
        return None;
    }
    let c2 = *curve_derivs_over(cn, Ivl::new(a, b), 2, None)
        .ok()?
        .get(2)?;
    let order = if c.offset.is_some() { 3 } else { 2 };
    let p = partials_over(&c.net, u, v, order).ok().flatten()?;
    let (du, dv) = (
        Ivl::from(wb.x) - Ivl::from(wa.x),
        Ivl::from(wb.y) - Ivl::from(wa.y),
    );
    let h = Ivl::from(b) - Ivl::from(a);
    let two = Ivl::from(2.0);
    // second derivative along w of a field F with partials F_u, F_v and second ones, times
    // h^2: F_uu du^2 + 2 F_uv du dv + F_vv dv^2
    let quad = |uu: IV3, uv: IV3, vv: IV3| {
        add(
            add(scale(uu, du * du), scale(uv, two * du * dv)),
            scale(vv, dv * dv),
        )
    };
    let s2 = quad(p.at(2, 0), p.at(1, 1), p.at(0, 2));
    let g = sub(scale(c2, h * h), s2);
    let mut extra = 0.0;
    if let Some((dist, _)) = c.offset {
        let (su, sv) = (p.at(1, 0), p.at(0, 1));
        let r = norm_down(cross(su, sv));
        if !normal_as_offset(r) {
            return None;
        }
        // first and second derivatives of N = S_u x S_v along w, times h and h^2
        let su1 = add(scale(p.at(2, 0), du), scale(p.at(1, 1), dv));
        let sv1 = add(scale(p.at(1, 1), du), scale(p.at(0, 2), dv));
        let n1 = add(cross(su1, sv), cross(su, sv1));
        let su2 = quad(p.at(3, 0), p.at(2, 1), p.at(1, 2));
        let sv2 = quad(p.at(2, 1), p.at(1, 2), p.at(0, 3));
        let n2 = add(
            add(cross(su2, sv), scale(cross(su1, sv1), two)),
            cross(su, sv2),
        );
        let (n1m, n2m) = (norm_up(n1), norm_up(n2));
        // |n''| h^2 <= 2 |N''| h^2 / r + 5 (|N'| h)^2 / r^2
        let nn = round_up(
            round_up(2.0 * n2m / r) + round_up(5.0 * round_up(n1m * n1m) / round_up(r * r)),
        );
        extra = round_up(dist.abs() * nn);
    }
    let bound = round_up(round_up(norm_up(g) + extra) / 8.0);
    bound.is_finite().then_some(bound)
}

/// Whether the curve is proved within `tol` of one surface over `interval`, within the
/// segment `budget`.
#[allow(clippy::too_many_arguments)] // the curve both ways, the surface both ways, the budget
fn on_surface(
    k: &Kernel,
    cn: &NurbsCurve,
    curve: &Curve,
    s: &Surface,
    c: &Checked,
    interval: Extent1d,
    tol: f64,
    budget: &mut usize,
) -> Result<bool> {
    let lines = crease_lines(c);
    let mut feet = Feet::default();
    // (`gap_at` by its arguments' bits: a segment's ends are its pieces' first and last
    // nodes, and its neighbour's ends; the bound is a function of them alone)
    let mut gaps: std::collections::HashMap<(u64, u64, u64), Option<f64>> =
        std::collections::HashMap::new();
    let mut gap = |t: f64, w: Point2| {
        *gaps
            .entry((t.to_bits(), w.x.to_bits(), w.y.to_bits()))
            .or_insert_with(|| gap_at(cn, c, t, w))
    };
    // the knot spans inside the interval
    let (lo, hi) = (interval.min(), interval.max());
    let mut cuts = vec![lo];
    for &u in &cn.u {
        if u > lo && u < hi && cuts.last().is_some_and(|&l| u > l) {
            cuts.push(u);
        }
    }
    cuts.push(hi);
    for span in cuts.windows(2) {
        let [a0, b0] = span else { continue };
        let mut stack: Vec<(f64, f64, u32)> = Vec::new();
        for i in (0..FIRST_SEGMENTS).rev() {
            let f = |j: usize| {
                if j == FIRST_SEGMENTS {
                    *b0
                } else {
                    a0 + (b0 - a0) * (j as f64) / (FIRST_SEGMENTS as f64)
                }
            };
            stack.push((f(i), f(i + 1), 0));
        }
        while let Some((a, b, depth)) = stack.pop() {
            k.check_cancelled(OP)?;
            if !charge(budget) {
                return Ok(false);
            }
            let Some(wa) = feet.foot(k, s, a, curve, None)? else {
                return Ok(false);
            };
            let Some(wb) = feet.foot(k, s, b, curve, Some(wa))? else {
                return Ok(false);
            };
            match (gap(a, wa), gap(b, wb)) {
                (Some(ga), Some(gb)) if ga.max(gb) > tol => return Ok(false),
                (Some(_), Some(_)) => {}
                _ => return Ok(false),
            }
            // each piece across the seams and between crease lines on its own (its `g` is
            // smooth there)
            let mut ok = true;
            for (ta, pa, tb, pb) in split_at_seams(&c.net, a, b, wa, wb) {
                let nodes = cut_at_creases(&lines, ta, tb, pa, pb);
                ok = ok
                    && nodes.windows(2).all(|w| {
                        let [(ta, pa), (tb, pb)] = w else {
                            return false;
                        };
                        ta < tb
                            && in_one_patch(&lines, *pa, *pb)
                            && match (gap(*ta, *pa), gap(*tb, *pb)) {
                                (Some(ga), Some(gb)) => bend(cn, c, *ta, *tb, *pa, *pb)
                                    .is_some_and(|e| round_up(ga.max(gb) + e) <= tol),
                                _ => false,
                            }
                    });
            }
            if ok {
                continue;
            }
            let m = 0.5 * (a + b);
            if depth >= MAX_DEPTH || !(a < m && m < b) {
                return Ok(false);
            }
            stack.push((m, b, depth + 1));
            stack.push((a, m, depth + 1));
        }
    }
    Ok(true)
}


// Modified for Makepad by fab/brep/sync/tool (fab/brep/SYNC.md): S1.

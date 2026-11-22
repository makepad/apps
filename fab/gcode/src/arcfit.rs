//! Arc fitting (R1 §8.7): a greedy growing window over a polyline keeps
//! extending a run of points while they all lie on one circle within the
//! tolerance (the G-code resolution); a run of at least three points
//! becomes one arc, anything shorter stays lines.
//!
//! A run becomes an arc only when the arc the printer will draw is the
//! polyline: every point within the tolerance of the circle, every chord
//! advancing around the centre in the written direction (so the angle grows
//! monotonically and the sweep is the sum of the steps, under a full turn),
//! every piece of arc within the tolerance of the chord it replaces, and the
//! sweep the printer computes from the written (3-decimal) start, end and
//! centre equal to the fitted one. A run that doubles back, or whose rounded
//! words would turn the other way or the long way round, stays lines.

/// One output segment, ending at `end` (mm).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Seg {
    Line { end: [f64; 2] },
    /// `center` absolute (mm); `ccw` for G3; `len` the arc length; `sweep`
    /// the angle swept (radians, positive, under a full turn).
    Arc { end: [f64; 2], center: [f64; 2], ccw: bool, len: f64, sweep: f64 },
}

/// Largest radius treated as an arc (a flatter run is a line).
const MAX_RADIUS: f64 = 2000.0;
/// Smallest radius treated as an arc.
const MIN_RADIUS: f64 = 0.05;
/// The G-code's coordinate resolution (`fmt::xyz`: 3 decimals).
pub const QUANTUM: f64 = 0.001;
/// The written start and end are at least this far apart (mm): with the
/// coordinates rounded to [`QUANTUM`], a shorter gap could flip the
/// printer's sweep between a sliver and a full turn.
const MIN_END_GAP: f64 = 0.05;
/// How far the printer's sweep (from the written words) may differ from the
/// fitted one (radians).
const SWEEP_SLACK: f64 = 1e-3;

/// A coordinate as written (rounded to [`QUANTUM`]).
pub fn quantize(v: f64) -> f64 {
    (v / QUANTUM).round() * QUANTUM
}

fn circle(a: [f64; 2], b: [f64; 2], c: [f64; 2]) -> Option<([f64; 2], f64)> {
    let d = 2.0 * (a[0] * (b[1] - c[1]) + b[0] * (c[1] - a[1]) + c[0] * (a[1] - b[1]));
    if d.abs() < 1e-12 {
        return None;
    }
    let a2 = a[0] * a[0] + a[1] * a[1];
    let b2 = b[0] * b[0] + b[1] * b[1];
    let c2 = c[0] * c[0] + c[1] * c[1];
    let ux = (a2 * (b[1] - c[1]) + b2 * (c[1] - a[1]) + c2 * (a[1] - b[1])) / d;
    let uy = (a2 * (c[0] - b[0]) + b2 * (a[0] - c[0]) + c2 * (b[0] - a[0])) / d;
    let r = ((a[0] - ux).powi(2) + (a[1] - uy).powi(2)).sqrt();
    if !(ux.is_finite() && uy.is_finite() && r.is_finite()) {
        return None;
    }
    Some(([ux, uy], r))
}

fn cross(o: [f64; 2], a: [f64; 2], b: [f64; 2]) -> f64 {
    (a[0] - o[0]) * (b[1] - o[1]) - (a[1] - o[1]) * (b[0] - o[0])
}

fn dist(a: [f64; 2], b: [f64; 2]) -> f64 {
    (a[0] - b[0]).hypot(a[1] - b[1])
}

/// The signed angle from `a` to `b` around `c` (radians, in (-π, π]).
fn step(c: [f64; 2], a: [f64; 2], b: [f64; 2]) -> f64 {
    let dot = (a[0] - c[0]) * (b[0] - c[0]) + (a[1] - c[1]) * (b[1] - c[1]);
    cross(c, a, b).atan2(dot)
}

/// The sweep a printer draws for an arc from `start` to `end` around
/// `center` (radians, in (0, 2π]; equal start and end angles are a full
/// circle), as the firmware computes it from the written words.
pub fn printed_sweep(start: [f64; 2], end: [f64; 2], center: [f64; 2], ccw: bool) -> f64 {
    let a0 = (start[1] - center[1]).atan2(start[0] - center[0]);
    let a1 = (end[1] - center[1]).atan2(end[0] - center[0]);
    let s = if ccw { a1 - a0 } else { a0 - a1 }.rem_euclid(std::f64::consts::TAU);
    if s <= 0.0 {
        std::f64::consts::TAU
    } else {
        s
    }
}

/// The words of an arc from the written position `start` (already rounded):
/// the rounded end and the rounded centre offset I J.
pub fn arc_words(start: [f64; 2], end: [f64; 2], center: [f64; 2]) -> ([f64; 2], [f64; 2]) {
    ([quantize(end[0]), quantize(end[1])], [quantize(center[0] - start[0]), quantize(center[1] - start[1])])
}

#[derive(Clone, Copy, Debug)]
struct Fit {
    center: [f64; 2],
    ccw: bool,
    len: f64,
    sweep: f64,
}

/// The arc through `pts[i..=j]`, if they fit.
fn fit(pts: &[[f64; 2]], i: usize, j: usize, tol: f64, max_sweep: f64) -> Option<Fit> {
    if j < i + 2 {
        return None;
    }
    let m = (i + j) / 2;
    let (c, r) = circle(pts[i], pts[m], pts[j])?;
    if !(MIN_RADIUS..=MAX_RADIUS).contains(&r) {
        return None;
    }
    // Every point on the circle within the tolerance.
    let off = |p: [f64; 2]| (dist(p, c) - r).abs();
    if (i..=j).any(|k| off(pts[k]) > tol) {
        return None;
    }
    // Every chord advances around the centre the way the first one does
    // (monotonic angle: a doubling-back run fails here), every turn of the
    // polyline the same way, and every piece of arc within the tolerance of
    // the chord it replaces: the arc between the chord's angles is within the
    // sagitta of the chord between the points projected onto the circle, and
    // that chord within the larger radial offset of the real one.
    // The direction is the centre's: the sign every step must share, never a single
    // triple's turn (a collinear triple has none: such a run stays lines).
    let ccw = step(c, pts[i], pts[i + 1]) > 0.0;
    let turns = |cr: f64| if ccw { cr > 0.0 } else { cr < 0.0 };
    let mut sweep = 0.0;
    for k in i..j {
        let (p, q) = (pts[k], pts[k + 1]);
        let s = step(c, p, q);
        if s == 0.0 || (s > 0.0) != ccw {
            return None;
        }
        if k + 2 <= j && !turns(cross(p, q, pts[k + 2])) {
            return None;
        }
        let sagitta = r * (1.0 - (s.abs() * 0.5).cos());
        if sagitta + off(p).max(off(q)) > tol {
            return None;
        }
        sweep += s.abs();
    }
    if sweep >= std::f64::consts::TAU.min(max_sweep) {
        return None;
    }
    Some(Fit { center: c, ccw, len: r * sweep, sweep })
}

/// The arc as written from the rounded `pts[i]` draws the fitted sweep, the
/// same way round, on (nearly) one radius, and its ends are far enough apart
/// for the rounding not to matter.
fn writes_true(pts: &[[f64; 2]], i: usize, j: usize, f: &Fit, tol: f64) -> bool {
    if dist(pts[i], pts[j]) < MIN_END_GAP {
        return false;
    }
    let start = [quantize(pts[i][0]), quantize(pts[i][1])];
    let (end, ij) = arc_words(start, pts[j], f.center);
    let center = [start[0] + ij[0], start[1] + ij[1]];
    let sweep = printed_sweep(start, end, center, f.ccw);
    (sweep - f.sweep).abs() <= SWEEP_SLACK && (dist(start, center) - dist(end, center)).abs() <= tol
}

/// Fit `pts` (the first is where the head is) into lines and arcs.
pub fn fit_arcs(pts: &[[f64; 2]], tol: f64) -> Vec<Seg> {
    fit_arcs_within(pts, tol, std::f64::consts::TAU)
}

/// The largest sweep (radians) one arc of a feature may draw, under the print check's caps
/// (359 degrees for loops, 270 for the rest): loops (walls, support, skirt and brim) nearly
/// a full turn, everything else less (an infill or surface boundary around a round region
/// would otherwise close into one arc).
pub fn max_sweep_for(role_name: &str) -> f64 {
    let f = role_name.to_ascii_lowercase();
    let loops = ["wall", "perimeter", "vertical shell", "support", "skirt", "brim"].iter().any(|k| f.contains(k)) && !f.contains("tower");
    if loops { 358f64.to_radians() } else { 268f64.to_radians() }
}

/// [`fit_arcs`] with no arc sweeping `max_sweep` (radians) or more.
pub fn fit_arcs_within(pts: &[[f64; 2]], tol: f64, max_sweep: f64) -> Vec<Seg> {
    let n = pts.len();
    let mut out = Vec::new();
    let mut i = 0;
    while i + 1 < n {
        let mut best: Option<(usize, Fit)> = None;
        let mut j = i + 2;
        while j < n {
            match fit(pts, i, j, tol, max_sweep) {
                // A run that fits keeps growing; the longest one whose words
                // draw it truly becomes the arc.
                Some(a) => {
                    if writes_true(pts, i, j, &a, tol) {
                        best = Some((j, a));
                    }
                    j += 1;
                }
                None => break,
            }
        }
        match best {
            Some((j, f)) => {
                out.push(Seg::Arc { end: pts[j], center: f.center, ccw: f.ccw, len: f.len, sweep: f.sweep });
                i = j;
            }
            None => {
                out.push(Seg::Line { end: pts[i + 1] });
                i += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::{PI, TAU};

    const TOL: f64 = 0.012;

    /// A small deterministic generator (xorshift64*): the tests need no crate.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
        }
        fn f(&mut self) -> f64 {
            (self.next() >> 11) as f64 / (1u64 << 53) as f64
        }
        fn range(&mut self, a: f64, b: f64) -> f64 {
            a + (b - a) * self.f()
        }
        fn n(&mut self, a: usize, b: usize) -> usize {
            a + (self.next() % (b - a + 1) as u64) as usize
        }
    }

    fn seg_point_distance(p: [f64; 2], a: [f64; 2], b: [f64; 2]) -> f64 {
        let (vx, vy) = (b[0] - a[0], b[1] - a[1]);
        let l = vx * vx + vy * vy;
        let t = if l == 0.0 { 0.0 } else { (((p[0] - a[0]) * vx + (p[1] - a[1]) * vy) / l).clamp(0.0, 1.0) };
        (p[0] - a[0] - t * vx).hypot(p[1] - a[1] - t * vy)
    }

    /// The point at angle offset `t` (radians along the sweep) of the arc the printer draws.
    fn arc_point(start: [f64; 2], center: [f64; 2], ccw: bool, t: f64) -> [f64; 2] {
        let r = dist(start, center);
        let a0 = (start[1] - center[1]).atan2(start[0] - center[0]);
        let a = if ccw { a0 + t } else { a0 - t };
        [center[0] + r * a.cos(), center[1] + r * a.sin()]
    }

    /// The distance from `p` to the arc the printer draws.
    fn arc_distance(p: [f64; 2], start: [f64; 2], center: [f64; 2], ccw: bool, sweep: f64) -> f64 {
        let r = dist(start, center);
        let a0 = (start[1] - center[1]).atan2(start[0] - center[0]);
        let a = (p[1] - center[1]).atan2(p[0] - center[0]);
        let off = if ccw { a - a0 } else { a0 - a }.rem_euclid(TAU);
        if off <= sweep {
            (dist(p, center) - r).abs()
        } else {
            dist(p, start).min(dist(p, arc_point(start, center, ccw, sweep)))
        }
    }

    /// Every property of one fitted output against the polyline it came from.
    fn check(pts: &[[f64; 2]], tol: f64, what: &str) {
        let segs = fit_arcs(pts, tol);
        let mut i = 0;
        for seg in &segs {
            match *seg {
                Seg::Line { end } => {
                    assert_eq!(end, pts[i + 1], "{what}: a line ends at the next point");
                    i += 1;
                }
                Seg::Arc { end, center, ccw, len, sweep } => {
                    // The run the arc replaced: the next points up to `end`.
                    let j = (i + 2..pts.len()).find(|&k| pts[k] == end).unwrap_or_else(|| panic!("{what}: arc end is not a later point"));
                    let run = &pts[i..=j];
                    let r = dist(run[0], center);
                    // Span: positive and under a full turn, and its length is r * sweep.
                    assert!(sweep > 0.0 && sweep < TAU, "{what}: sweep {sweep}");
                    assert!((len - r * sweep).abs() < 1e-9 * (1.0 + len), "{what}: len {len} vs r*sweep {}", r * sweep);
                    // Direction: every point of the run advances around the centre the
                    // written way, and the angles add up to the sweep (monotonic).
                    let mut acc = 0.0;
                    for k in 0..run.len() - 1 {
                        let s = step(center, run[k], run[k + 1]);
                        assert!(if ccw { s > 0.0 } else { s < 0.0 }, "{what}: point {k} steps {s} against ccw={ccw}");
                        acc += s.abs();
                    }
                    assert!((acc - sweep).abs() < 1e-9, "{what}: steps add to {acc}, sweep {sweep}");
                    // What the printer draws from the written words is that sweep, the same way round.
                    let start = [quantize(run[0][0]), quantize(run[0][1])];
                    let (wend, ij) = arc_words(start, end, center);
                    let wc = [start[0] + ij[0], start[1] + ij[1]];
                    let printed = printed_sweep(start, wend, wc, ccw);
                    assert!((printed - sweep).abs() <= SWEEP_SLACK, "{what}: printer sweeps {printed}, fit {sweep}");
                    assert!(dist(run[0], run[run.len() - 1]) >= MIN_END_GAP, "{what}: ends too close");
                    // Deviation both ways, per chord: the piece of arc between a chord's
                    // angles from that chord, and every point of the chord from the arc.
                    let mut at = 0.0;
                    for k in 0..run.len() - 1 {
                        let s = step(center, run[k], run[k + 1]).abs();
                        for u in 0..=32 {
                            let p = arc_point(run[0], center, ccw, at + s * u as f64 / 32.0);
                            let d = seg_point_distance(p, run[k], run[k + 1]);
                            assert!(d <= tol + 1e-7, "{what}: the arc stands {d} mm off chord {k} (tol {tol})");
                        }
                        for u in 0..=8 {
                            let t = u as f64 / 8.0;
                            let p = [run[k][0] + (run[k + 1][0] - run[k][0]) * t, run[k][1] + (run[k + 1][1] - run[k][1]) * t];
                            let d = arc_distance(p, run[0], center, ccw, sweep);
                            assert!(d <= tol + 1e-7, "{what}: chord {k} stands {d} mm off the arc (tol {tol})");
                        }
                        at += s;
                    }
                    // The length agrees with the polyline it replaced.
                    let poly: f64 = run.windows(2).map(|w| dist(w[0], w[1])).sum();
                    assert!(len >= poly * (1.0 - 1e-9) - 2.0 * tol && len <= poly * 1.01 + 2.0 * tol, "{what}: arc {len} mm for a {poly} mm polyline");
                    i = j;
                }
            }
        }
        assert_eq!(i, pts.len().saturating_sub(1), "{what}: covers every point");
    }

    #[test]
    fn circle_points_become_one_arc_and_lines_stay_lines() {
        let pts: Vec<[f64; 2]> = (0..=32).map(|k| {
            let a = k as f64 / 64.0 * TAU;
            [10.0 + 2.0 * a.cos(), 20.0 + 2.0 * a.sin()]
        }).collect();
        let segs = fit_arcs(&pts, TOL);
        assert_eq!(segs.len(), 1);
        match segs[0] {
            Seg::Arc { center, ccw, len, sweep, .. } => {
                assert!((center[0] - 10.0).abs() < 1e-6 && (center[1] - 20.0).abs() < 1e-6);
                assert!(ccw);
                assert!((len - PI * 2.0).abs() < 1e-6);
                assert!((sweep - PI).abs() < 1e-9);
            }
            _ => panic!(),
        }
        check(&pts, TOL, "half circle");
        let line = [[0.0, 0.0], [1.0, 0.0], [2.0, 0.0], [3.0, 1.0]];
        assert!(fit_arcs(&line, TOL).iter().all(|s| matches!(s, Seg::Line { .. })));
    }

    /// The bug class from the tree-support slice: a short clockwise run that
    /// the fitter wrote counter-clockwise (the printer swept ~356° round a
    /// 12.7 mm circle). Three points that double back pass the turn test but
    /// not the advance test.
    #[test]
    fn a_run_that_doubles_back_stays_lines() {
        // Forward along a 12.7 mm circle clockwise, then a point back between the first two.
        let c = [180.0, 150.0];
        let r = 12.7;
        let p = |deg: f64| [c[0] + r * deg.to_radians().cos(), c[1] + r * deg.to_radians().sin()];
        let back = [p(90.0), p(86.0), p(88.0)];
        assert!(fit_arcs(&back, TOL).iter().all(|s| matches!(s, Seg::Line { .. })), "{:?}", fit_arcs(&back, TOL));
        // Collinear-ish doubling back: out and back along a line, with a nudge.
        let zig = [[0.0, 0.0], [1.0, 0.0], [0.5, 0.001]];
        assert!(fit_arcs(&zig, TOL).iter().all(|s| matches!(s, Seg::Line { .. })));
        // A true clockwise 4° run (several points) becomes a clockwise arc of 4°.
        let cw: Vec<[f64; 2]> = (0..=8).map(|k| p(90.0 - k as f64 * 0.5)).collect();
        let segs = fit_arcs(&cw, TOL);
        assert_eq!(segs.len(), 1, "{segs:?}");
        match segs[0] {
            Seg::Arc { ccw, sweep, .. } => {
                assert!(!ccw);
                assert!((sweep - 4f64.to_radians()).abs() < 1e-9);
            }
            _ => panic!(),
        }
        check(&cw, TOL, "cw 4 degrees");
    }

    /// FABBENCH's finding: nearly straight runs (radii up to ~1000 mm) were written the
    /// wrong way round, the direction taken from the first triple's turn and a collinear
    /// triple counted as clockwise. Straight runs stay lines; a gentle curve either way
    /// keeps its direction.
    #[test]
    fn straight_and_nearly_straight_runs() {
        let straight: Vec<[f64; 2]> = (0..12).map(|k| [100.0 + 0.06 * k as f64, 50.0 + 0.03 * k as f64]).collect();
        assert!(fit_arcs(&straight, TOL).iter().all(|s| matches!(s, Seg::Line { .. })));
        // A collinear first triple, then a bend: no arc may start from the straight part's turn.
        let mut bent = vec![[0.0, 0.0], [1.0, 0.0], [2.0, 0.0]];
        bent.extend((1..10).map(|k| [2.0 + k as f64, -0.01 * (k * k) as f64]));
        check(&bent, TOL, "collinear then bending");
        for &r in &[300.0, 1023.0, 1900.0] {
            for &dir in &[1.0, -1.0] {
                for &steps in &[0.06, 0.5, 3.0] {
                    let pts: Vec<[f64; 2]> = (0..20).map(|k| {
                        let a = -std::f64::consts::FRAC_PI_2 + dir * steps * k as f64 / r;
                        [150.0 + r * a.cos(), 150.0 + r + r * a.sin()]
                    }).collect();
                    check(&pts, TOL, &format!("r {r} dir {dir} step {steps}"));
                    for s in fit_arcs(&pts, TOL) {
                        if let Seg::Arc { ccw, sweep, .. } = s {
                            assert_eq!(ccw, dir > 0.0, "r {r}: direction");
                            assert!(sweep < 0.2, "r {r}: a gentle curve sweeps {sweep}");
                        }
                    }
                }
            }
        }
    }

    /// Randomised polylines of every kind: arcs either way with noise, runs that
    /// double back, near-collinear runs, zigzags, arcs with a reversal, tiny
    /// and huge radii. Every arc out must hold every property.
    #[test]
    fn random_polylines_hold_every_arc_property() {
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
        let mut arcs = 0;
        for case in 0..6000 {
            let kind = case % 7;
            let n = rng.n(3, 40);
            let tol = [TOL, 0.005, 0.02][rng.n(0, 2)];
            let mut pts: Vec<[f64; 2]> = Vec::new();
            let c = [rng.range(0.0, 350.0), rng.range(0.0, 320.0)];
            let r = match rng.n(0, 3) { 0 => rng.range(0.06, 0.5), 1 => rng.range(0.5, 5.0), 2 => rng.range(5.0, 50.0), _ => rng.range(50.0, 2500.0) };
            let dir = if rng.f() < 0.5 { 1.0 } else { -1.0 };
            let mut a = rng.range(0.0, TAU);
            let noise = rng.range(0.0, tol);
            let da = rng.range(0.0005, 0.6) / r.sqrt().max(0.2);
            let reverse_at = rng.n(1, n.max(2) - 1);
            for k in 0..n {
                let p = match kind {
                    // Arc, one way, with radial noise.
                    0 | 1 => {
                        let rr = r + rng.range(-noise, noise);
                        let p = [c[0] + rr * a.cos(), c[1] + rr * a.sin()];
                        a += dir * da * rng.range(0.3, 1.7);
                        p
                    }
                    // Arc that reverses (doubles back) part way.
                    2 => {
                        let p = [c[0] + r * a.cos(), c[1] + r * a.sin()];
                        a += if k < reverse_at { dir * da } else { -dir * da * rng.range(0.2, 1.5) };
                        p
                    }
                    // Near-collinear with tiny noise.
                    3 => [c[0] + k as f64 * rng.range(0.05, 2.0), c[1] + rng.range(-noise, noise) * 0.1],
                    // Zigzag / random walk.
                    4 => {
                        let last = pts.last().copied().unwrap_or(c);
                        [last[0] + rng.range(-1.0, 1.0), last[1] + rng.range(-1.0, 1.0)]
                    }
                    // Out and straight back along nearly the same line.
                    5 => {
                        let t = if k < reverse_at { k as f64 } else { 2.0 * reverse_at as f64 - k as f64 - 0.5 };
                        [c[0] + t * 0.4, c[1] + rng.range(-noise, noise)]
                    }
                    // Tiny steps on a circle (sub-quantum spacing), either way.
                    _ => {
                        let p = [c[0] + r * a.cos(), c[1] + r * a.sin()];
                        a += dir * rng.range(1e-6, 0.002) / r.max(0.05);
                        p
                    }
                };
                pts.push(p);
            }
            check(&pts, tol, &format!("case {case} kind {kind}"));
            arcs += fit_arcs(&pts, tol).iter().filter(|s| matches!(s, Seg::Arc { .. })).count();
        }
        assert!(arcs > 1000, "the generator should make many arcs, made {arcs}");
    }

    /// Near-full circles: an arc may sweep almost a full turn, but never one the
    /// rounding could turn into a sliver or a full circle.
    #[test]
    fn near_full_circles_keep_their_ends_apart() {
        for &r in &[0.08, 0.3, 2.301, 12.7] {
            for &dir in &[1.0, -1.0] {
                let n = 200;
                let pts: Vec<[f64; 2]> = (0..=n).map(|k| {
                    let a = dir * k as f64 / n as f64 * TAU;
                    [100.0 + r * a.cos(), 100.0 + r * a.sin()]
                }).collect();
                check(&pts, TOL, &format!("full circle r {r} dir {dir}"));
                for s in fit_arcs(&pts, TOL) {
                    if let Seg::Arc { sweep, .. } = s {
                        assert!(sweep < TAU);
                    }
                }
            }
        }
    }
}

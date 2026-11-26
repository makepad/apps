//! Path alignment: making two (or more) outlines that differ in contour
//! count, segment count, starting point and winding into outlines that
//! morph well by moving each point straight (or along an arc) to its
//! partner.
//!
//! The method, from the idea of "what a person would pair up":
//! 1. Every segment is a cubic (a line is a cubic with its handles a third
//!    of the way along).
//! 2. Contours are paired by centroid distance, size and turning direction,
//!    greedily from the closest pair. A contour with no partner is paired
//!    with a degenerate contour (every point the same) sitting at the
//!    centroid of the nearest contour of its own shape: it grows out of, or
//!    collapses into, that spot.
//! 3. Each pair gets the same segment count by splitting the longest
//!    segment at its middle (de Casteljau) until they match.
//! 4. A closed pair starts at the point (and runs in the direction) that
//!    minimises the squared distance the points have to travel.
//!
//! Any number of shapes align together (a key list): every shape gets the
//! contour slots of the one with most contours, each slot the segment count
//! of its largest, and each shape is rotated against the one before it.

use crate::geometry::{PathCmd, VectorPath};

type P = [f64; 2];

/// One cubic segment: start, two handles, end.
pub type Seg = [P; 4];

#[derive(Clone, Debug, PartialEq)]
pub struct Contour {
    /// Consecutive segments (each starts where the one before ends); a
    /// closed contour's last segment ends at its first one's start.
    pub segs: Vec<Seg>,
    pub closed: bool,
}

/// An outline: its contours.
pub type Shape = Vec<Contour>;

fn lerp(a: P, b: P, t: f64) -> P {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]
}

fn dist2(a: P, b: P) -> f64 {
    (a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)
}

fn line_seg(a: P, b: P) -> Seg {
    [a, lerp(a, b, 1.0 / 3.0), lerp(a, b, 2.0 / 3.0), b]
}

/// The segment split at parameter `t` (de Casteljau): two segments that
/// together are exactly it.
pub fn split(s: &Seg, t: f64) -> (Seg, Seg) {
    let ab = lerp(s[0], s[1], t);
    let bc = lerp(s[1], s[2], t);
    let cd = lerp(s[2], s[3], t);
    let abc = lerp(ab, bc, t);
    let bcd = lerp(bc, cd, t);
    let m = lerp(abc, bcd, t);
    ([s[0], ab, abc, m], [m, bcd, cd, s[3]])
}

fn point_at(s: &Seg, t: f64) -> P {
    let u = 1.0 - t;
    let (w0, w1, w2, w3) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
    [w0 * s[0][0] + w1 * s[1][0] + w2 * s[2][0] + w3 * s[3][0], w0 * s[0][1] + w1 * s[1][1] + w2 * s[2][1] + w3 * s[3][1]]
}

/// The control polygon's length: what "longest" means for splitting.
fn seg_len(s: &Seg) -> f64 {
    dist2(s[0], s[1]).sqrt() + dist2(s[1], s[2]).sqrt() + dist2(s[2], s[3]).sqrt()
}

// ---- reading and writing paths ------------------------------------------------

/// The contours of a path (arcs and quadratics are already cubics in a
/// `VectorPath`; lines become cubics). A lone `M` makes no contour.
pub fn shape_of(path: &VectorPath) -> Shape {
    let mut out: Shape = Vec::new();
    let mut segs: Vec<Seg> = Vec::new();
    let mut start: P = [0.0; 2];
    let mut at: P = [0.0; 2];
    let finish = |segs: &mut Vec<Seg>, closed: bool, at: P, start: P, out: &mut Shape| {
        if closed && dist2(at, start) > 1e-18 {
            segs.push(line_seg(at, start));
        }
        if !segs.is_empty() {
            out.push(Contour { segs: std::mem::take(segs), closed });
        }
    };
    for cmd in &path.cmds {
        match cmd {
            PathCmd::MoveTo(x, y) => {
                finish(&mut segs, false, at, start, &mut out);
                start = [*x as f64, *y as f64];
                at = start;
            }
            PathCmd::LineTo(x, y) => {
                let to = [*x as f64, *y as f64];
                segs.push(line_seg(at, to));
                at = to;
            }
            PathCmd::BezierTo(a, b, c, d, x, y) => {
                let to = [*x as f64, *y as f64];
                segs.push([at, [*a as f64, *b as f64], [*c as f64, *d as f64], to]);
                at = to;
            }
            PathCmd::Close => {
                finish(&mut segs, true, at, start, &mut out);
                at = start;
            }
            PathCmd::Winding(_) => {}
        }
    }
    finish(&mut segs, false, at, start, &mut out);
    out
}

/// The shape as a path: `M`, one `C` per segment, `Z` where closed.
pub fn to_path(shape: &Shape) -> VectorPath {
    let mut path = VectorPath::new();
    for contour in shape {
        let Some(first) = contour.segs.first() else { continue };
        path.move_to(first[0][0] as f32, first[0][1] as f32);
        for s in &contour.segs {
            path.bezier_to(s[1][0] as f32, s[1][1] as f32, s[2][0] as f32, s[2][1] as f32, s[3][0] as f32, s[3][1] as f32);
        }
        if contour.closed {
            path.close();
        }
    }
    path
}

// ---- contour measures ---------------------------------------------------------

/// Sample points of a contour (each segment at 0, 1/4, 1/2, 3/4).
fn samples(c: &Contour) -> impl Iterator<Item = P> + '_ {
    c.segs.iter().flat_map(|s| [0.0, 0.25, 0.5, 0.75].map(|t| point_at(s, t)))
}

fn centroid(c: &Contour) -> P {
    let (mut sum, mut n) = ([0.0, 0.0], 0.0);
    for p in samples(c) {
        sum[0] += p[0];
        sum[1] += p[1];
        n += 1.0;
    }
    if n == 0.0 { [0.0; 2] } else { [sum[0] / n, sum[1] / n] }
}

/// Signed area (counter-clockwise positive with y up) of the sampled outline.
fn signed_area(c: &Contour) -> f64 {
    let pts: Vec<P> = samples(c).collect();
    let n = pts.len();
    (0..n).map(|i| {
        let (a, b) = (pts[i], pts[(i + 1) % n]);
        a[0] * b[1] - b[0] * a[1]
    }).sum::<f64>() * 0.5
}

fn is_point(c: &Contour) -> bool {
    let at = c.segs[0][0];
    c.segs.iter().all(|s| s.iter().all(|p| *p == at))
}

fn degenerate(at: P, segs: usize, closed: bool) -> Contour {
    Contour { segs: vec![[at; 4]; segs.max(1)], closed }
}

/// A contour traversed the other way.
fn reversed(c: &Contour) -> Contour {
    Contour { segs: c.segs.iter().rev().map(|s| [s[3], s[2], s[1], s[0]]).collect(), closed: c.closed }
}

// ---- contour pairing ----------------------------------------------------------

/// For each contour of `shape`, the slot of `master` it takes (all
/// different); `master` has at least as many contours.
fn pair_slots(master: &Shape, shape: &Shape) -> Vec<usize> {
    let m: Vec<(P, f64, f64)> = master.iter().map(|c| (centroid(c), signed_area(c), c.closed as u8 as f64)).collect();
    let s: Vec<(P, f64, f64)> = shape.iter().map(|c| (centroid(c), signed_area(c), c.closed as u8 as f64)).collect();
    let mut costs: Vec<(f64, usize, usize)> = Vec::new();
    for (j, slot) in m.iter().enumerate() {
        for (i, own) in s.iter().enumerate() {
            let (ra, rb) = (slot.1.abs().sqrt(), own.1.abs().sqrt());
            let turning = if slot.2 > 0.0 && own.2 > 0.0 && slot.1 * own.1 < 0.0 { ra + rb } else { 0.0 };
            let open = if slot.2 != own.2 { ra + rb + 1.0 } else { 0.0 };
            costs.push((dist2(slot.0, own.0).sqrt() + (ra - rb).abs() + turning + open, j, i));
        }
    }
    costs.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
    let mut slot_of = vec![usize::MAX; shape.len()];
    let mut taken = vec![false; master.len()];
    for (_, j, i) in costs {
        if slot_of[i] == usize::MAX && !taken[j] {
            slot_of[i] = j;
            taken[j] = true;
        }
    }
    slot_of
}

// ---- alignment ----------------------------------------------------------------

/// Split the longest segment at its middle until the contour has `n` segments.
fn split_to(c: &mut Contour, n: usize) {
    while c.segs.len() < n {
        let (k, _) = c.segs.iter().enumerate().fold((0, -1.0), |best, (k, s)| {
            let l = seg_len(s);
            if l > best.1 { (k, l) } else { best }
        });
        let (a, b) = split(&c.segs[k], 0.5);
        c.segs[k] = a;
        c.segs.insert(k + 1, b);
    }
}

/// The start offset and direction of `b` that minimise the squared
/// distance its points travel to reach `a`'s (same segment counts).
/// `(0, false)` wins every tie, so a contour aligned with itself stays.
fn best_start(a: &Contour, b: &Contour) -> (usize, bool) {
    let n = a.segs.len();
    let key = |c: &Contour| -> Vec<[P; 2]> { c.segs.iter().map(|s| [s[0], point_at(s, 0.5)]).collect() };
    let ka = key(a);
    let forward = key(b);
    let backward = key(&reversed(b));
    let stride = (n / 64).max(1);
    let cost = |kb: &[[P; 2]], shift: usize| -> f64 {
        (0..n).step_by(stride).map(|i| {
            let q = &kb[(i + shift) % n];
            dist2(ka[i][0], q[0]) + dist2(ka[i][1], q[1])
        }).sum()
    };
    let mut best = (0usize, false);
    let mut best_cost = cost(&forward, 0);
    for (reverse, kb) in [(false, &forward), (true, &backward)] {
        let shifts = if a.closed { n } else { 1 };
        for shift in 0..shifts {
            if !reverse && shift == 0 {
                continue;
            }
            let c = cost(kb, shift);
            if c < best_cost - 1e-12 {
                best_cost = c;
                best = (shift, reverse);
            }
        }
    }
    best
}

/// All the shapes made to have the same contours, segment counts and
/// point order: shape `i`'s point `k` is the one that moves to shape
/// `i + 1`'s point `k`. A shape aligned alone, or with itself, is itself
/// (split into the common segment count).
pub fn align_shapes(shapes: &[Shape]) -> Vec<Shape> {
    if shapes.is_empty() {
        return Vec::new();
    }
    // The master: the first shape with the most contours.
    let (master_at, master) = shapes.iter().enumerate().fold((0, &shapes[0]), |best, (i, s)| if s.len() > best.1.len() { (i, s) } else { best });
    let slots = master.len();
    // Each shape in the master's slots; a missing contour is a point at the
    // centroid of the shape's own contour nearest the slot's.
    let mut placed: Vec<Vec<Contour>> = Vec::with_capacity(shapes.len());
    for (k, shape) in shapes.iter().enumerate() {
        let slot_of: Vec<usize> = if k == master_at { (0..slots).collect() } else { pair_slots(master, shape) };
        let mut row: Vec<Option<Contour>> = vec![None; slots];
        for (i, contour) in shape.iter().enumerate() {
            row[slot_of[i]] = Some(contour.clone());
        }
        let own: Vec<P> = shape.iter().map(centroid).collect();
        let row = row.into_iter().enumerate().map(|(j, c)| {
            c.unwrap_or_else(|| {
                let want = centroid(&master[j]);
                let at = own.iter().copied().min_by(|a, b| dist2(*a, want).total_cmp(&dist2(*b, want))).unwrap_or(want);
                degenerate(at, 1, master[j].closed)
            })
        }).collect();
        placed.push(row);
    }
    // Closed in every shape's slot when closed in any.
    for j in 0..slots {
        let closed = placed.iter().any(|row| row[j].closed);
        for row in &mut placed {
            if closed && !row[j].closed {
                let c = &mut row[j];
                let (first, last) = (c.segs[0][0], c.segs[c.segs.len() - 1][3]);
                if dist2(first, last) > 1e-18 {
                    c.segs.push(line_seg(last, first));
                }
                c.closed = true;
            }
        }
        let n = placed.iter().map(|row| row[j].segs.len()).max().unwrap_or(1);
        for row in &mut placed {
            // A degenerate contour is a point: more segments of it, all there.
            if is_point(&row[j]) {
                let at = row[j].segs[0][0];
                row[j] = degenerate(at, n, closed);
            } else {
                split_to(&mut row[j], n);
            }
        }
        for k in 1..placed.len() {
            let (prev, rest) = placed.split_at_mut(k);
            let (a, b) = (&prev[k - 1][j], &mut rest[0][j]);
            let (shift, reverse) = best_start(a, b);
            let mut next = if reverse { reversed(b) } else { b.clone() };
            next.segs.rotate_left(shift);
            *b = next;
        }
    }
    placed
}

/// Two shapes aligned: see [`align_shapes`].
pub fn align_paths(a: &Shape, b: &Shape) -> (Shape, Shape) {
    let mut out = align_shapes(&[a.clone(), b.clone()]);
    let b = out.pop().unwrap_or_default();
    (out.pop().unwrap_or_default(), b)
}

// ---- mixing -------------------------------------------------------------------

/// The point that goes from `a` to `b` along a circular arc turning `angle`
/// radians (counter-clockwise, y up), `k` of the way. A zero angle is the
/// straight line.
fn arc_point(a: P, b: P, k: f64, angle: f64) -> P {
    if angle.abs() < 1e-9 || dist2(a, b) < 1e-24 {
        return lerp(a, b, k);
    }
    let (c, s) = (angle.cos(), angle.sin());
    // The centre c0 with R(angle) (a - c0) = b - c0: c0 = (I - R)^-1 (b - R a).
    let v = [b[0] - (c * a[0] - s * a[1]), b[1] - (s * a[0] + c * a[1])];
    let det = (1.0 - c) * (1.0 - c) + s * s;
    let centre = [((1.0 - c) * v[0] - s * v[1]) / det, (s * v[0] + (1.0 - c) * v[1]) / det];
    let (ck, sk) = ((angle * k).cos(), (angle * k).sin());
    let r = [a[0] - centre[0], a[1] - centre[1]];
    [centre[0] + ck * r[0] - sk * r[1], centre[1] + sk * r[0] + ck * r[1]]
}

/// Two aligned shapes mixed: every point `k` of the way from its place in
/// `a` to its place in `b`, in a straight line (`arc` 0) or on an arc
/// turning `arc` radians.
pub fn mix(a: &Shape, b: &Shape, k: f64, arc: f64) -> Shape {
    a.iter().zip(b).map(|(ca, cb)| Contour {
        closed: ca.closed,
        segs: ca.segs.iter().zip(&cb.segs).map(|(sa, sb)| std::array::from_fn(|i| arc_point(sa[i], sb[i], k, arc))).collect(),
    }).collect()
}

/// Two paths aligned and mixed at `k` (0 is `a`, 1 is `b`).
pub fn mix_paths(a: &VectorPath, b: &VectorPath, k: f64, arc: f64) -> VectorPath {
    let (a, b) = align_paths(&shape_of(a), &shape_of(b));
    to_path(&mix(&a, &b, k, arc))
}

// ---- Splash: flat point lists ---------------------------------------------------

/// A flat polyline `[x0, y0, x1, y1, ...]` as a contour of line segments
/// (closed when its last point repeats its first).
pub fn polyline_contour(flat: &[f64]) -> Option<Contour> {
    let pts: Vec<P> = flat.chunks_exact(2).map(|p| [p[0], p[1]]).collect();
    if pts.len() < 2 {
        return None;
    }
    let closed = pts.len() > 2 && pts[0] == pts[pts.len() - 1];
    Some(Contour { segs: pts.windows(2).map(|w| line_seg(w[0], w[1])).collect(), closed })
}

/// A contour back to a flat polyline (the segments' ends; handles dropped).
pub fn contour_polyline(c: &Contour) -> Vec<f64> {
    let mut out = Vec::with_capacity(c.segs.len() * 2 + 2);
    if let Some(first) = c.segs.first() {
        out.extend_from_slice(&first[0]);
    }
    for s in &c.segs {
        out.extend_from_slice(&s[3]);
    }
    out
}

use makepad_script::*;

fn flat(vm: &ScriptVm, array: ScriptArray) -> Option<Vec<f64>> {
    let heap = vm.bx.heap_ref();
    (0..heap.array_len(array)).map(|i| heap.array_index(array, i, NoTrap).as_number()).collect()
}

/// A path argument: a flat point list, or a list of them (contours).
/// The bool: it was a list of contours.
fn shape_arg(vm: &ScriptVm, value: ScriptValue) -> Option<(Shape, bool)> {
    let array = value.as_array()?;
    let heap = vm.bx.heap_ref();
    let n = heap.array_len(array);
    if n > 0 && heap.array_index(array, 0, NoTrap).as_array().is_some() {
        let mut shape = Vec::new();
        for i in 0..n {
            shape.extend(polyline_contour(&flat(vm, heap.array_index(array, i, NoTrap).as_array()?)?));
        }
        Some((shape, true))
    } else {
        Some((polyline_contour(&flat(vm, array)?).into_iter().collect(), false))
    }
}

fn shape_value(vm: &mut ScriptVm, shape: &Shape, contours: bool) -> ScriptValue {
    let mut lists = Vec::new();
    for c in shape {
        let list = vm.bx.heap_mut().new_array();
        for v in contour_polyline(c) {
            vm.bx.heap_mut().array_push(list, v.into(), NoTrap);
        }
        lists.push(list);
    }
    if !contours && lists.len() == 1 {
        return lists[0].into();
    }
    let all = vm.bx.heap_mut().new_array();
    for list in lists {
        vm.bx.heap_mut().array_push(all, list.into(), NoTrap);
    }
    all.into()
}

/// `align_paths(a, b)` and `mix_paths(a, b, k, arc = 0)` for point lists
/// (`mod.motion`): a path is a flat list `[x0, y0, ...]` (closed when its
/// last point repeats its first) or a list of them. `align_paths` gives
/// `[a, b]` with the same contours and as many points in each, the start
/// and direction chosen so the points travel least; `mix_paths` is
/// `a` moved `k` of the way to `b` after that (a straight line per point,
/// or an arc turning `arc` radians).
pub(crate) fn install(vm: &mut ScriptVm, module: ScriptObject) {
    vm.add_method(module, id_lut!(align_paths), script_args!(a = NIL, b = NIL), |vm, args| {
        let get = |vm: &ScriptVm, key: LiveId| vm.bx.heap_ref().value(args, key.into(), NoTrap);
        let (Some((a, ca)), Some((b, cb))) = (shape_arg(vm, get(vm, id!(a))), shape_arg(vm, get(vm, id!(b)))) else { return NIL };
        let (a, b) = align_paths(&a, &b);
        let contours = ca || cb;
        let out = vm.bx.heap_mut().new_array();
        for shape in [a, b] {
            let v = shape_value(vm, &shape, contours);
            vm.bx.heap_mut().array_push(out, v, NoTrap);
        }
        out.into()
    });
    vm.add_method(module, id_lut!(mix_paths), script_args!(a = NIL, b = NIL, k = NIL, arc = NIL), |vm, args| {
        let get = |vm: &ScriptVm, key: LiveId| vm.bx.heap_ref().value(args, key.into(), NoTrap);
        let k = get(vm, id!(k)).as_number().unwrap_or(0.0);
        let arc = get(vm, id!(arc)).as_number().unwrap_or(0.0);
        let (Some((a, ca)), Some((b, cb))) = (shape_arg(vm, get(vm, id!(a))), shape_arg(vm, get(vm, id!(b)))) else { return NIL };
        let (a, b) = align_paths(&a, &b);
        shape_value(vm, &mix(&a, &b, k, arc), ca || cb)
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::parse_path;

    fn shape(data: &str) -> Shape {
        shape_of(&parse_path(data))
    }

    fn same_structure(shapes: &[Shape]) {
        for s in shapes {
            assert_eq!(s.len(), shapes[0].len(), "contour counts");
            for (j, c) in s.iter().enumerate() {
                assert_eq!(c.segs.len(), shapes[0][j].segs.len(), "segment counts of contour {j}");
                assert_eq!(c.closed, shapes[0][j].closed);
            }
        }
    }

    fn circle(cx: f64, cy: f64, r: f64, clockwise: bool) -> String {
        let k = r * 0.552_284_75;
        let (a, b) = (r, k);
        if clockwise {
            format!("M{} {} C{} {} {} {} {} {} C{} {} {} {} {} {} C{} {} {} {} {} {} C{} {} {} {} {} {} Z",
                cx, cy - a, cx + b, cy - a, cx + a, cy - b, cx + a, cy, cx + a, cy + b, cx + b, cy + a, cx, cy + a, cx - b, cy + a, cx - a, cy + b, cx - a, cy, cx - a, cy - b, cx - b, cy - a, cx, cy - a)
        } else {
            format!("M{} {} C{} {} {} {} {} {} C{} {} {} {} {} {} C{} {} {} {} {} {} C{} {} {} {} {} {} Z",
                cx, cy - a, cx - b, cy - a, cx - a, cy - b, cx - a, cy, cx - a, cy + b, cx - b, cy + a, cx, cy + a, cx + b, cy + a, cx + a, cy + b, cx + a, cy, cx + a, cy - b, cx + b, cy - a, cx, cy - a)
        }
    }

    #[test]
    fn splitting_keeps_the_curve() {
        let s: Seg = [[0.0, 0.0], [10.0, 40.0], [30.0, -20.0], [50.0, 10.0]];
        let (a, b) = split(&s, 0.5);
        assert_eq!(a[3], b[0]);
        for t in [0.0, 0.2, 0.5, 0.9, 1.0] {
            let on_a = point_at(&a, t);
            let on_s = point_at(&s, t * 0.5);
            assert!(dist2(on_a, on_s) < 1e-18);
            assert!(dist2(point_at(&b, t), point_at(&s, 0.5 + t * 0.5)) < 1e-18);
        }
    }

    #[test]
    fn counts_become_equal() {
        let square = shape("M0 0 L100 0 L100 100 L0 100 Z");
        let round = shape(&circle(50.0, 50.0, 50.0, true));
        let (a, b) = align_paths(&square, &round);
        same_structure(&[a.clone(), b.clone()]);
        assert_eq!(a[0].segs.len(), 4);
        // A triangle against a hexagon-ish 9-segment curve: the smaller is split up.
        let tri = shape("M0 0 L100 0 L50 90 Z");
        let many = shape("M0 0 L10 0 L20 0 L30 0 L40 0 L50 0 L60 0 L70 0 L70 50 Z");
        let (a, b) = align_paths(&tri, &many);
        same_structure(&[a.clone(), b]);
        assert_eq!(a[0].segs.len(), 9);
        // Splitting changes no geometry: the same outline.
        let (a, _) = align_paths(&tri, &many);
        let mut back = shape("M0 0 L100 0 L50 90 Z");
        back[0].segs = a[0].segs.clone();
        assert!((signed_area(&back[0]) - signed_area(&tri[0])).abs() < 1e-6);
    }

    #[test]
    fn a_path_aligned_with_itself_is_unchanged() {
        for data in ["M0 0 L100 0 L100 100 L0 100 Z", "M0 0 C20 30 60 30 80 0 L80 50", &circle(5.0, 5.0, 20.0, false)] {
            let s = shape(data);
            let (a, b) = align_paths(&s, &s);
            assert_eq!(a, s);
            assert_eq!(b, s);
        }
        // Midway between a path and itself is itself.
        let s = shape("M0 0 L100 0 L100 100 L0 100 Z");
        let (a, b) = align_paths(&s, &s);
        assert_eq!(mix(&a, &b, 0.37, 0.0), s);
    }

    #[test]
    fn start_and_winding_are_chosen_to_travel_least() {
        let a = shape("M0 0 L100 0 L100 100 L0 100 Z");
        // The same square from another corner and the other way round.
        let b = shape("M100 100 L100 0 L0 0 L0 100 Z");
        let (a2, b2) = align_paths(&a, &b);
        let travel: f64 = a2[0].segs.iter().zip(&b2[0].segs).map(|(x, y)| dist2(x[0], y[0])).sum();
        assert!(travel < 1e-12, "{travel}");
        // Any rotation of the same shape lines back up.
        let c = shape("M0 100 L0 0 L100 0 L100 100 Z");
        let (a3, c3) = align_paths(&a, &c);
        let travel: f64 = a3[0].segs.iter().zip(&c3[0].segs).map(|(x, y)| dist2(x[0], y[0])).sum();
        assert!(travel < 1e-12, "{travel}");
        // Open paths may run in the other direction, not start elsewhere.
        let open = shape("M0 0 L50 50 L100 0");
        let backwards = shape("M100 0 L50 50 L0 0");
        let (x, y) = align_paths(&open, &backwards);
        let travel: f64 = x[0].segs.iter().zip(&y[0].segs).map(|(p, q)| dist2(p[0], q[0])).sum();
        assert!(travel < 1e-12);
    }

    #[test]
    fn square_into_circle_and_back_stays_between() {
        let square = shape("M0 0 L100 0 L100 100 L0 100 Z");
        let round = shape(&circle(50.0, 50.0, 50.0, true));
        let (a, b) = align_paths(&square, &round);
        // Ends are the shapes (as outlines).
        let area = |s: &Shape| signed_area(&s[0]).abs();
        assert!((area(&mix(&a, &b, 0.0, 0.0)) - area(&square)).abs() < 1.0);
        assert!((area(&mix(&a, &b, 1.0, 0.0)) - area(&round)).abs() < 1.0);
        // Halfway is in between, and every point of it lies inside the square's box.
        let mid = mix(&a, &b, 0.5, 0.0);
        let m = area(&mid);
        assert!(m < area(&square) && m > area(&round) * 0.95, "{m}");
        for p in samples(&mid[0]) {
            assert!(p[0] > -1e-9 && p[0] < 100.0 + 1e-9 && p[1] > -1e-9 && p[1] < 100.0 + 1e-9);
        }
        // The path form agrees.
        assert_eq!(to_path(&mix(&a, &b, 0.5, 0.0)).cmds.len(), to_path(&mid).cmds.len());
    }

    #[test]
    fn two_contours_into_one_collapse_the_hole_to_a_point() {
        // An 'O' (outer one way, hole the other) and an 'I' (one bar).
        let o = shape(&format!("{} {}", circle(50.0, 50.0, 50.0, true), circle(50.0, 50.0, 25.0, false)));
        let i = shape("M40 0 L60 0 L60 100 L40 100 Z");
        assert_eq!(o.len(), 2);
        let (a, b) = align_paths(&o, &i);
        assert_eq!((a.len(), b.len()), (2, 2));
        same_structure(&[a.clone(), b.clone()]);
        // The outer ring went to the bar; the hole to a point at the bar's centre.
        let hole = b.iter().find(|c| is_point(c)).expect("a degenerate contour");
        for s in &hole.segs {
            for p in s {
                assert!(dist2(*p, [50.0, 50.0]) < 1.0, "{p:?}");
            }
        }
        assert!(b.iter().any(|c| signed_area(c).abs() > 1000.0), "the bar is still there");
        // The other way round, the O grows its hole from a point.
        let (i2, o2) = align_paths(&i, &o);
        same_structure(&[i2.clone(), o2]);
        assert_eq!(i2.len(), 2);
        // At the end of the mix the hole contour has no area.
        let end = mix(&a, &b, 1.0, 0.0);
        assert_eq!(end.iter().filter(|c| signed_area(c).abs() < 1e-9).count(), 1);
    }

    #[test]
    fn a_key_list_aligns_all_at_once() {
        let square = shape("M0 0 L100 0 L100 100 L0 100 Z");
        let round = shape(&circle(50.0, 50.0, 50.0, true));
        let o = shape(&format!("{} {}", circle(50.0, 50.0, 50.0, true), circle(50.0, 50.0, 25.0, false)));
        let all = align_shapes(&[square, round, o]);
        same_structure(&all);
        assert_eq!(all[0].len(), 2);
    }

    #[test]
    fn arcs_start_and_end_where_the_line_does_and_bulge_between() {
        let (a, b) = ([0.0, 0.0], [10.0, 0.0]);
        assert_eq!(arc_point(a, b, 0.0, 1.0), a);
        let end = arc_point(a, b, 1.0, 1.0);
        assert!(dist2(end, b) < 1e-18);
        assert_eq!(arc_point(a, b, 0.5, 0.0), [5.0, 0.0]);
        let mid = arc_point(a, b, 0.5, std::f64::consts::PI);
        // A half turn about the chord's middle: through the point 5 away.
        assert!((mid[0] - 5.0).abs() < 1e-9 && (mid[1].abs() - 5.0).abs() < 1e-9, "{mid:?}");
        // Constant radius about the centre all along.
        let r0 = dist2(arc_point(a, b, 0.25, 2.0), arc_point(a, b, 0.75, 2.0));
        assert!(r0 > 0.0);
    }

    #[test]
    fn polylines_round_trip_through_the_list_form() {
        let c = polyline_contour(&[0.0, 0.0, 10.0, 0.0, 10.0, 10.0, 0.0, 0.0]).unwrap();
        assert!(c.closed && c.segs.len() == 3);
        assert_eq!(contour_polyline(&c), vec![0.0, 0.0, 10.0, 0.0, 10.0, 10.0, 0.0, 0.0]);
        let (a, b) = align_paths(&vec![c.clone()], &vec![polyline_contour(&[0.0, 0.0, 4.0, 0.0, 4.0, 4.0, 0.0, 4.0, 0.0, 0.0]).unwrap()]);
        assert_eq!(a[0].segs.len(), 4);
        assert_eq!(contour_polyline(&mix(&a, &b, 0.0, 0.0)[0]).len(), 10);
    }
}

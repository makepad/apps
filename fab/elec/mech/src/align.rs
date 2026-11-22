//! Align: a board onto the bosses of a printed part. The part's mesh gives its boss axes (a
//! column's round flat top, or a round hole in a flat face, the axis along the seat's normal), the
//! board gives its mounting holes, and a rigid fit in the seat's plane pairs the two patterns and
//! reports, per hole, how far its boss's axis lies from it. Pure geometry in millimetres; it runs
//! once per Align on the caller's worker, never per frame: O(triangles) to find the bosses, and
//! O(holes^2 x bosses^2) hypotheses of O(holes x bosses) each to fit them.

use std::collections::HashMap;

/// The distance (mm) within which a hole counts as on its boss's axis. The fit pairs the patterns
/// by the holes within it; a hole further off is a miss, reported with its distance.
pub const TOL: f64 = 0.1;
/// Vertices closer than this (mm) are one.
const WELD: f64 = 1e-4;
/// A seat face's normal within 2 degrees of the seat's, its corners within this of one height.
const SEAT_COS: f64 = 0.99939;
const FLAT: f64 = 1e-3;
/// The round loops a boss can show: radius (mm), corners at the least, and how far a corner may
/// stray from the circle (mm, plus a share of the radius for a coarse tessellation).
const R_MIN: f64 = 0.3;
const R_MAX: f64 = 15.0;
const MIN_CORNERS: usize = 8;
const ROUND: f64 = 0.02;
/// Loops whose axes lie closer than this (mm) are one boss's.
const SAME_AXIS: f64 = 0.05;

/// A boss on a part: a column, a hole, or a column with a hole down it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Boss {
    /// Its axis in the seat's plane, along [`seat_basis`]'s u and v (mm).
    pub at: [f64; 2],
    /// Its top along the seat's normal: where a board's underside rests on it.
    pub top: f64,
    /// The axis at its top, in the part's frame.
    pub axis: [f64; 3],
    /// The column's outside across at its top, when it stands as a column.
    pub outer_d: Option<f64>,
    /// The hole down its axis across at its top (an insert's or a pilot's mouth).
    pub hole_d: Option<f64>,
}

/// The seat's frame `[u, v, up]`: u and v span the plane a board rests in, `up` its normal out of
/// it. u is the part's x laid into the plane (its y when x runs along `up`) and v = up x u, so a
/// board's x runs along u and its y along v: on a plate (z up) along +x and +y, in a case's model
/// frame (y up) along +x and -z.
pub fn seat_basis(up: [f64; 3]) -> [[f64; 3]; 3] {
    let n = unit(up);
    let x = if n[0].abs() < 0.9 { [1.0, 0.0, 0.0] } else { [0.0, 1.0, 0.0] };
    let u = unit(sub(x, scale(n, dot(x, n))));
    [u, cross(n, u), n]
}

/// The bosses of a mesh whose seat faces point along `up`: every flat face facing up whose
/// boundary has a round loop (a column's top, its outer loop; a hole, its inner one). Loops on
/// one axis are one boss, at the highest of them, so a column's foot and a hole's floor fold into
/// the column's top. In a stable order (by u, then v).
pub fn find_bosses(positions: &[[f64; 3]], triangles: &[[u32; 3]], up: [f64; 3]) -> Vec<Boss> {
    let basis = seat_basis(up);
    let mut ids = HashMap::new();
    let mut pts: Vec<[f64; 3]> = Vec::new();
    let weld: Vec<usize> = positions.iter().map(|p| {
        let key = p.map(|c| (c / WELD).round() as i64);
        *ids.entry(key).or_insert_with(|| {
            pts.push(basis.map(|b| dot(*p, b)));
            pts.len() - 1
        })
    }).collect();

    // The faces a board could rest on, each with its height.
    let mut seats: Vec<([usize; 3], f64)> = Vec::new();
    for t in triangles {
        let c = t.map(|i| weld[i as usize]);
        if c[0] == c[1] || c[1] == c[2] || c[0] == c[2] {
            continue;
        }
        let [a, b, d] = c.map(|i| pts[i]);
        let e = cross(sub(b, a), sub(d, a));
        let h = (a[2] + b[2] + d[2]) / 3.0;
        if e[2] > SEAT_COS * len(e) && [a, b, d].iter().all(|p| (p[2] - h).abs() < FLAT) {
            seats.push((c, h));
        }
    }

    // One flat face: seat triangles joined by an edge at one height.
    let mut root: Vec<usize> = (0..seats.len()).collect();
    fn find(root: &mut [usize], mut i: usize) -> usize {
        while root[i] != i {
            root[i] = root[root[i]];
            i = root[i];
        }
        i
    }
    let mut by_edge: HashMap<(usize, usize), usize> = HashMap::new();
    for (k, (c, h)) in seats.iter().enumerate() {
        for e in 0..3 {
            let (a, b) = (c[e], c[(e + 1) % 3]);
            match by_edge.get(&(a.min(b), a.max(b))) {
                Some(&j) if (seats[j].1 - h).abs() < 2.0 * FLAT => {
                    let (x, y) = (find(&mut root, j), find(&mut root, k));
                    root[x] = y;
                }
                _ => {
                    by_edge.insert((a.min(b), a.max(b)), k);
                }
            }
        }
    }
    let mut faces: HashMap<usize, Vec<usize>> = HashMap::new();
    for k in 0..seats.len() {
        let r = find(&mut root, k);
        faces.entry(r).or_default().push(k);
    }

    // Round loops of each face's boundary: (axis, radius, height, an outer loop).
    let mut rounds: Vec<([f64; 2], f64, f64, bool)> = Vec::new();
    for tris in faces.values() {
        let h = seats[tris[0]].1;
        let mut edges = std::collections::HashSet::new();
        for &k in tris {
            let c = seats[k].0;
            for e in 0..3 {
                edges.insert((c[e], c[(e + 1) % 3]));
            }
        }
        // The boundary runs anticlockwise round the face (seen from up) and clockwise round its holes.
        let mut next: HashMap<usize, Vec<usize>> = HashMap::new();
        let mut open = 0;
        for &(a, b) in &edges {
            if !edges.contains(&(b, a)) {
                next.entry(a).or_default().push(b);
                open += 1;
            }
        }
        let mut starts: Vec<usize> = next.keys().copied().collect();
        starts.sort_unstable();
        for start in starts {
            if next[&start].is_empty() {
                continue;
            }
            let mut lp = vec![start];
            let mut at = next.get_mut(&start).and_then(|to| to.pop());
            let mut closed = false;
            while let Some(v) = at {
                if v == start {
                    closed = true;
                    break;
                }
                if lp.len() > open {
                    break;
                }
                lp.push(v);
                at = next.get_mut(&v).and_then(|to| to.pop());
            }
            if !closed || lp.len() < MIN_CORNERS {
                continue;
            }
            let ring: Vec<[f64; 2]> = lp.iter().map(|&i| [pts[i][0], pts[i][1]]).collect();
            if let Some((c, r)) = circle(&ring).filter(|&(_, r)| (R_MIN..=R_MAX).contains(&r)) {
                if ring.iter().all(|p| (len2(sub2(*p, c)) - r).abs() <= ROUND + ROUND * r) {
                    rounds.push((c, r, h, area(&ring) > 0.0));
                }
            }
        }
    }

    // Loops on one axis: one boss, at the highest of them.
    let mut groups: Vec<Vec<usize>> = Vec::new();
    for i in 0..rounds.len() {
        match groups.iter_mut().find(|g| len2(sub2(rounds[g[0]].0, rounds[i].0)) < SAME_AXIS) {
            Some(g) => g.push(i),
            None => groups.push(vec![i]),
        }
    }
    let [u, v, n] = basis;
    let mut bosses: Vec<Boss> = groups.iter().map(|g| {
        let top = g.iter().map(|&i| rounds[i].2).fold(f64::MIN, f64::max);
        let at_top = || g.iter().map(|&i| rounds[i]).filter(|r| (r.2 - top).abs() < 2.0 * FLAT);
        let at = rounds[g[0]].0;
        Boss {
            at,
            top,
            axis: add(add(scale(u, at[0]), scale(v, at[1])), scale(n, top)),
            outer_d: at_top().filter(|r| r.3).map(|r| 2.0 * r.1).reduce(f64::max),
            hole_d: at_top().filter(|r| !r.3).map(|r| 2.0 * r.1).reduce(f64::min),
        }
    }).collect();
    bosses.sort_by(|a, b| a.at[0].total_cmp(&b.at[0]).then(a.at[1].total_cmp(&b.at[1])));
    bosses
}

/// One board hole's place after a fit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HoleFit {
    /// The boss it pairs with (an index into the bosses), none when every boss is taken.
    pub boss: Option<usize>,
    /// Where its boss's axis lies from it, in the board's frame (mm): the move that puts the hole
    /// on the axis.
    pub off: [f64; 2],
    /// How far that is (mm).
    pub residual: f64,
    /// How far its boss's top lies below the seat (mm): the gap under the board there.
    pub gap: f64,
}

/// A board fitted onto a part's bosses.
#[derive(Clone, Debug, PartialEq)]
pub struct Fit {
    /// The board's frame (mm, its underside at z = 0) in the part's: a board point `p` lands at
    /// `pose * [p, 1]`. Its underside rests on the highest of its bosses.
    pub pose: [[f64; 4]; 3],
    /// Per board hole, in the order given.
    pub holes: Vec<HoleFit>,
    /// How many holes are within [`TOL`] of their boss's axis.
    pub on: usize,
}

/// A rigid move in the seat's plane: a turn, then a shift.
#[derive(Clone, Copy, Debug)]
struct Rigid {
    turn: f64,
    shift: [f64; 2],
}

impl Rigid {
    fn map(&self, p: [f64; 2]) -> [f64; 2] {
        let (s, c) = self.turn.sin_cos();
        [c * p[0] - s * p[1] + self.shift[0], s * p[0] + c * p[1] + self.shift[1]]
    }
}

/// Fit `holes` (the board's mounting holes in its own frame, mm) onto `bosses` (from
/// [`find_bosses`] with the same `up`). Pairs the patterns by every pair of holes against every
/// pair of bosses as far apart, keeps the pairing that puts the most holes within [`TOL`] of their
/// boss, then least squares over those; of equal fits, the one nearest `prior` (the board's pose
/// now, in the part's frame), so a symmetric pattern keeps the board's way round. A hole further
/// off than [`TOL`] keeps out of the least squares and is reported by its full distance. None
/// without holes or bosses.
pub fn fit(holes: &[[f64; 2]], bosses: &[Boss], up: [f64; 3], prior: Option<&[[f64; 4]; 3]>) -> Option<Fit> {
    if holes.is_empty() || bosses.is_empty() {
        return None;
    }
    let basis = seat_basis(up);
    let [u, v, n] = basis;
    let start = prior.map_or(Rigid { turn: 0.0, shift: [0.0; 2] }, |p| {
        let x = [p[0][0], p[1][0], p[2][0]];
        let t = [p[0][3], p[1][3], p[2][3]];
        Rigid { turn: dot(x, v).atan2(dot(x, u)), shift: [dot(t, u), dot(t, v)] }
    });
    let axes: Vec<[f64; 2]> = bosses.iter().map(|b| b.at).collect();

    let mut tries: Vec<Rigid> = Vec::new();
    for h in holes {
        for b in &axes {
            let turned = Rigid { turn: start.turn, shift: [0.0; 2] }.map(*h);
            tries.push(Rigid { turn: start.turn, shift: sub2(*b, turned) });
        }
    }
    for i in 0..holes.len() {
        for j in i + 1..holes.len() {
            let dh = sub2(holes[j], holes[i]);
            for k in 0..axes.len() {
                for l in 0..axes.len() {
                    let db = sub2(axes[l], axes[k]);
                    if k == l || (len2(dh) - len2(db)).abs() > 2.0 * TOL {
                        continue;
                    }
                    let turn = db[1].atan2(db[0]) - dh[1].atan2(dh[0]);
                    let mid_h = scale2(add2(holes[i], holes[j]), 0.5);
                    let turned = Rigid { turn, shift: [0.0; 2] }.map(mid_h);
                    tries.push(Rigid { turn, shift: sub2(scale2(add2(axes[k], axes[l]), 0.5), turned) });
                }
            }
        }
    }

    // Each try settled by least squares over the holes it puts on their bosses, then ranked: the
    // most holes on, the least spread over them, the nearest the board's pose now.
    let settle = |mut r: Rigid| {
        for _ in 0..3 {
            let pairs = pair(&r, holes, &axes);
            let on: Vec<_> = pairs.iter().enumerate().filter(|(_, p)| p.1 <= TOL).map(|(i, p)| (holes[i], axes[p.0.unwrap()])).collect();
            if on.len() >= 2 {
                r = least_squares(&on);
            } else if let [(h, b)] = on[..] {
                r.shift = sub2(b, Rigid { turn: r.turn, shift: [0.0; 2] }.map(h));
            }
        }
        let pairs = pair(&r, holes, &axes);
        let on = pairs.iter().filter(|p| p.1 <= TOL).count();
        let spread: f64 = pairs.iter().filter(|p| p.1 <= TOL).map(|p| p.1 * p.1).sum();
        let turn = (r.turn - start.turn + std::f64::consts::PI).rem_euclid(std::f64::consts::TAU) - std::f64::consts::PI;
        Settled { r, on, spread, turn: turn.abs(), moved: len2(sub2(r.shift, start.shift)), pairs }
    };
    let close = |a: f64, b: f64| (a - b).abs() < 1e-6;
    let better = |b: &Settled, a: &Settled| {
        if b.on != a.on {
            return b.on > a.on;
        }
        if !close(b.spread, a.spread) {
            return b.spread < a.spread;
        }
        if !close(b.turn, a.turn) {
            return b.turn < a.turn;
        }
        b.moved < a.moved
    };
    let Settled { r, on, pairs, .. } = tries.into_iter().map(settle).reduce(|a, b| if better(&b, &a) { b } else { a })?;

    let seat = pairs.iter().filter(|p| p.1 <= TOL || on == 0).filter_map(|p| p.0).map(|k| bosses[k].top).fold(f64::MIN, f64::max);
    let back = Rigid { turn: -r.turn, shift: [0.0; 2] };
    let holes = holes.iter().zip(&pairs).map(|(h, &(k, residual))| {
        let off = k.map_or([0.0; 2], |k| back.map(sub2(axes[k], r.map(*h))));
        HoleFit { boss: k, off, residual: if k.is_some() { residual } else { f64::INFINITY }, gap: k.map_or(0.0, |k| seat - bosses[k].top) }
    }).collect();
    let (s, c) = r.turn.sin_cos();
    let x = add(scale(u, c), scale(v, s));
    let y = add(scale(u, -s), scale(v, c));
    let t = add(add(scale(u, r.shift[0]), scale(v, r.shift[1])), scale(n, seat));
    let pose = std::array::from_fn(|i| [x[i], y[i], n[i], t[i]]);
    Some(Fit { pose, holes, on })
}

/// `a` after `b`: a point `p` goes to `a * (b * p)` (affine maps as `[[f64; 4]; 3]` rows).
pub fn compose(a: &[[f64; 4]; 3], b: &[[f64; 4]; 3]) -> [[f64; 4]; 3] {
    std::array::from_fn(|r| std::array::from_fn(|c| {
        let mut v = a[r][0] * b[0][c] + a[r][1] * b[1][c] + a[r][2] * b[2][c];
        if c == 3 {
            v += a[r][3];
        }
        v
    }))
}

/// The inverse of an affine map, none when it flattens space.
pub fn invert(m: &[[f64; 4]; 3]) -> Option<[[f64; 4]; 3]> {
    let a = |r: usize, c: usize| m[r][c];
    let co = |r0: usize, r1: usize, c0: usize, c1: usize| a(r0, c0) * a(r1, c1) - a(r0, c1) * a(r1, c0);
    let det = a(0, 0) * co(1, 2, 1, 2) - a(0, 1) * co(1, 2, 0, 2) + a(0, 2) * co(1, 2, 0, 1);
    if det.abs() < 1e-12 {
        return None;
    }
    let k = 1.0 / det;
    let inv = [
        [co(1, 2, 1, 2) * k, -co(0, 2, 1, 2) * k, co(0, 1, 1, 2) * k],
        [-co(1, 2, 0, 2) * k, co(0, 2, 0, 2) * k, -co(0, 1, 0, 2) * k],
        [co(1, 2, 0, 1) * k, -co(0, 2, 0, 1) * k, co(0, 1, 0, 1) * k],
    ];
    Some(std::array::from_fn(|r| {
        let t = -(inv[r][0] * m[0][3] + inv[r][1] * m[1][3] + inv[r][2] * m[2][3]);
        [inv[r][0], inv[r][1], inv[r][2], t]
    }))
}

/// A board's pose in a part's model frame (board mm to model mm, y up) as the CAD language's
/// `board_mount(pcb, {at turn})` writes it: the model point of the board's `middle` (its
/// outline's bounds' middle, board mm) and the turn about +y (degrees). None when the board does
/// not lie flat facing up in that frame, which a mount cannot say.
pub fn mount_of(model_from_board: &[[f64; 4]; 3], middle: [f64; 2]) -> Option<([f64; 3], f64)> {
    let m = model_from_board;
    let z = [m[0][2], m[1][2], m[2][2]];
    if dot(z, [0.0, 1.0, 0.0]) < 1.0 - 1e-6 {
        return None;
    }
    let at = std::array::from_fn(|r| m[r][0] * middle[0] + m[r][1] * middle[1] + m[r][3]);
    Some((at, (-m[2][0]).atan2(m[0][0]).to_degrees()))
}

/// A try at the fit after least squares: how many holes it puts on, their squared distances
/// summed, its turn and shift away from the board's pose now, and each hole's pairing.
struct Settled {
    r: Rigid,
    on: usize,
    spread: f64,
    turn: f64,
    moved: f64,
    pairs: Vec<(Option<usize>, f64)>,
}

/// Each hole's boss under `r` and its distance: the nearest free boss, the closest pairs first.
fn pair(r: &Rigid, holes: &[[f64; 2]], axes: &[[f64; 2]]) -> Vec<(Option<usize>, f64)> {
    let placed: Vec<_> = holes.iter().map(|h| r.map(*h)).collect();
    let mut all: Vec<(f64, usize, usize)> = Vec::with_capacity(holes.len() * axes.len());
    for (i, p) in placed.iter().enumerate() {
        for (k, a) in axes.iter().enumerate() {
            all.push((len2(sub2(*a, *p)), i, k));
        }
    }
    all.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut out = vec![(None, f64::INFINITY); holes.len()];
    let mut taken = vec![false; axes.len()];
    for (d, i, k) in all {
        if out[i].0.is_none() && !taken[k] {
            out[i] = (Some(k), d);
            taken[k] = true;
        }
    }
    out
}

/// The rigid move that best lays the first of each pair on the second (two or more pairs).
fn least_squares(pairs: &[([f64; 2], [f64; 2])]) -> Rigid {
    let k = 1.0 / pairs.len() as f64;
    let ca = scale2(pairs.iter().fold([0.0; 2], |s, p| add2(s, p.0)), k);
    let cb = scale2(pairs.iter().fold([0.0; 2], |s, p| add2(s, p.1)), k);
    let (mut along, mut across) = (0.0, 0.0);
    for (a, b) in pairs {
        let (a, b) = (sub2(*a, ca), sub2(*b, cb));
        along += a[0] * b[0] + a[1] * b[1];
        across += a[0] * b[1] - a[1] * b[0];
    }
    let turn = across.atan2(along);
    let turned = Rigid { turn, shift: [0.0; 2] }.map(ca);
    Rigid { turn, shift: sub2(cb, turned) }
}

/// The circle through a loop's corners by least squares (centre, radius).
fn circle(ring: &[[f64; 2]]) -> Option<([f64; 2], f64)> {
    let k = 1.0 / ring.len() as f64;
    let m = scale2(ring.iter().fold([0.0; 2], |s, p| add2(s, *p)), k);
    let (mut xx, mut xy, mut yy, mut xz, mut yz, mut zz) = (0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
    for p in ring {
        let [x, y] = sub2(*p, m);
        let z = x * x + y * y;
        xx += x * x;
        xy += x * y;
        yy += y * y;
        xz += x * z;
        yz += y * z;
        zz += z;
    }
    let det = xx * yy - xy * xy;
    if det.abs() < 1e-12 {
        return None;
    }
    let d = -(xz * yy - yz * xy) / det;
    let e = -(xx * yz - xy * xz) / det;
    let r2 = (d * d + e * e) / 4.0 + zz * k;
    (r2 > 0.0).then(|| ([m[0] - d / 2.0, m[1] - e / 2.0], r2.sqrt()))
}

/// Twice the signed area of a loop: positive anticlockwise.
fn area(ring: &[[f64; 2]]) -> f64 {
    (0..ring.len()).map(|i| {
        let (a, b) = (ring[i], ring[(i + 1) % ring.len()]);
        a[0] * b[1] - a[1] * b[0]
    }).sum()
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}
fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn add(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
fn scale(a: [f64; 3], k: f64) -> [f64; 3] {
    [a[0] * k, a[1] * k, a[2] * k]
}
fn len(a: [f64; 3]) -> f64 {
    dot(a, a).sqrt()
}
fn unit(a: [f64; 3]) -> [f64; 3] {
    scale(a, 1.0 / len(a))
}
fn sub2(a: [f64; 2], b: [f64; 2]) -> [f64; 2] {
    [a[0] - b[0], a[1] - b[1]]
}
fn add2(a: [f64; 2], b: [f64; 2]) -> [f64; 2] {
    [a[0] + b[0], a[1] + b[1]]
}
fn scale2(a: [f64; 2], k: f64) -> [f64; 2] {
    [a[0] * k, a[1] * k]
}
fn len2(a: [f64; 2]) -> f64 {
    (a[0] * a[0] + a[1] * a[1]).sqrt()
}

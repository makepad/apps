//! `m.voronoi` over its seeds: what its kernels read against brute force over all of them, and what
//! the cells make of a curved skin, unrolled.

use super::*;
use makepad_csg_math::Vec3d;

type V = [f64; 3];

const ROOTS: &[RootFn] = &[RootFn { name: "place", args: &["solid"], model: false }];
const HEAD: &str = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\n";

/// What `read` makes of the shape of the part `v` of `b` and `opts`.
fn with_shape<T>(source: &str, opts: &str, read: impl FnOnce(&csg_sdf::SdfShape) -> T) -> T {
    let src = format!("{HEAD}{source}\nlet v = m.voronoi(b, {{{opts}}})\nplace(v)\n");
    let options = Options { generated: &Default::default(), assets: &Assets::new(), images: &Default::default(), extension: "fab", libraries: &[], inputs: &NO_INPUTS, unit: 1000.0, roots: ROOTS, overrides: HashMap::new(), weave: None, solved: HashMap::new() };
    let p = evaluate(&src, &options).unwrap_or_else(|e| panic!("{src}: {e:?}"));
    let Op::Field { shape, .. } = &p.graph.node(p.solid("v").unwrap().node).op else { panic!("not a field") };
    read(shape)
}

/// The field of the part `v` of `b` and `opts` at `ps`.
fn field(source: &str, opts: &str, ps: &[V]) -> Vec<f64> {
    with_shape(source, opts, |shape| {
        let ps: Vec<Vec3d> = ps.iter().map(|p| Vec3d::new(p[0], p[1], p[2])).collect();
        let mut out = vec![0.0; ps.len()];
        shape.field().distance_many(&ps, &mut out);
        out
    })
}

fn sub(a: V, b: V) -> V {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn dot(a: V, b: V) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: V, b: V) -> V {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

fn far(a: V, b: V) -> f64 {
    dot(sub(a, b), sub(a, b)).sqrt()
}

/// The same 40 mm cube the scripts below make, and its cells in the op's own terms: q = m p.
const CUBE: &str = "let b = m.box({size: [40mm 40mm 40mm]})";
const M: V = [1.0 / 6.0, 1.0 / 9.6, 1.0 / 6.0];

fn cube(strut: f64) -> (voronoi::Spec, voronoi::Cells) {
    let source = csg_sdf::SdfNode::Box { size: [40.0; 3], radius: 0.0 };
    let spec = voronoi::Spec { size: 6.0, stretch: 1.6, axis: 1, skin: 2.0 * strut, random: 0.6, salt: 0, base: 0.0, rim: 0.0, strut, thinnest: strut, round: 0.1, tan: None, unit: 1000.0 };
    let cells = voronoi::cells(&source, ([-20.0; 3], [20.0; 3]), &spec, true);
    (spec, cells)
}

fn q_of(p: V) -> V {
    [p[0] * M[0], p[1] * M[1], p[2] * M[2]]
}

/// Deterministic points in a box (a 32-bit LCG).
fn points(n: usize, half: f64) -> Vec<V> {
    let mut s = 12345u32;
    let mut r = || {
        s = s.wrapping_mul(1664525).wrapping_add(1013904223);
        (s >> 8) as f64 / 16777216.0
    };
    (0..n).map(|_| [0; 3].map(|_| (r() * 2.0 - 1.0) * half)).collect()
}

/// The distance from p to the faces of its cell, over all seeds.
fn brute_face(seeds: &[V], q: V) -> f64 {
    let c = *seeds.iter().min_by(|a, b| far(**a, q).total_cmp(&far(**b, q))).unwrap();
    let a = sub(c, q);
    seeds
        .iter()
        .filter(|s| far(**s, c) > 1e-9)
        .map(|s| {
            let n = sub(*s, c);
            let w = [n[0] * M[0], n[1] * M[1], n[2] * M[2]];
            let b = sub(*s, q);
            (dot(b, b) - dot(a, a)) * 0.5 / dot(w, w).sqrt()
        })
        .fold(f64::INFINITY, f64::min)
}

/// The distance from p (q = m p) to the nearest edge of the cells (where three meet), when it is at
/// most `reach`: an edge that near has its three seeds, and every seed nearer to its points, within
/// f1 + 2 reach of q, so the lines of those triples clipped by those seeds are exact there.
fn brute_edge(seeds: &[V], q: V, reach: f64) -> Option<f64> {
    let mut seeds = seeds.to_vec();
    seeds.sort_by(|a, b| far(*a, q).total_cmp(&far(*b, q)));
    let f1 = far(seeds[0], q);
    let mmax = M[0].max(M[1]).max(M[2]);
    let near: Vec<V> = seeds.into_iter().take_while(|s| far(*s, q) <= f1 + 2.0 * reach * mmax).collect();
    let to_p = |x: V| [x[0] / M[0], x[1] / M[1], x[2] / M[2]];
    let mut best = f64::INFINITY;
    for i in 0..near.len() {
        for j in i + 1..near.len() {
            for k in j + 1..near.len() {
                let (a, b, c) = (near[i], near[j], near[k]);
                let (n1, n2) = (sub(b, a), sub(c, a));
                let dir = cross(n1, n2);
                let det = dot(n1, cross(n2, dir));
                if det.abs() < 1e-12 {
                    continue;
                }
                let (r1, r2, r3) = ((dot(b, b) - dot(a, a)) * 0.5, (dot(c, c) - dot(a, a)) * 0.5, dot(dir, q));
                let (u, v, w) = (cross(n2, dir), cross(dir, n1), cross(n1, n2));
                let x0 = [0, 1, 2].map(|e| (r1 * u[e] + r2 * v[e] + r3 * w[e]) / det);
                // where a (and so b and c) is nearer than every other seed: t in lo..hi
                let (mut lo, mut hi) = (f64::NEG_INFINITY, f64::INFINITY);
                for s in &near {
                    if far(*s, a) < 1e-6 || far(*s, b) < 1e-6 || far(*s, c) < 1e-6 {
                        continue;
                    }
                    let n = sub(*s, a);
                    let (slope, room) = (2.0 * dot(n, dir), dot(*s, *s) - dot(a, a) - 2.0 * dot(n, x0));
                    if slope.abs() < 1e-15 {
                        if room < 0.0 {
                            hi = f64::NEG_INFINITY;
                        }
                    } else if slope > 0.0 {
                        hi = hi.min(room / slope);
                    } else {
                        lo = lo.max(room / slope);
                    }
                }
                if lo > hi {
                    continue;
                }
                // the nearest point to p in p's metric
                let (x, d, qp) = (to_p(x0), to_p(dir), to_p(q));
                let t = (dot(sub(qp, x), d) / dot(d, d)).clamp(lo, hi);
                best = best.min(far([x[0] + t * d[0], x[1] + t * d[1], x[2] + t * d[2]], qp));
            }
        }
    }
    (best <= reach).then_some(best)
}

/// The kernel's faces read the true distance to a point's cell wherever its lists and its bound say,
/// and never more: each is what its seeds make, so a list holds every seed that matters.
#[test]
fn voronoi_faces_match_brute_force() {
    let strut = 1.5;
    let (_, cells) = cube(strut);
    // the middle of the skin of an upright face and of the top, off the cells' pitch
    let wall = (0..40 * 40).map(|i| [18.5, (i % 40) as f64 * 0.7 - 13.7, (i / 40) as f64 * 0.7 - 13.7]);
    let top = (0..40 * 40).map(|i| [(i % 40) as f64 * 0.7 - 13.7, 18.5, (i / 40) as f64 * 0.7 - 13.7]);
    let ps: Vec<V> = wall.chain(top).collect();
    let d = field(CUBE, "size: 6mm strut: 1.5mm round: 0.1mm tops: @round core: @none", &ps);
    let (mut exact, mut checked) = (0, 0);
    for (i, p) in ps.iter().enumerate() {
        let web = brute_face(&cells.seeds, q_of(*p)) - 0.5 * strut;
        assert!(d[i] >= -1.5 - 1e-3, "inside the skin at {p:?}: {}", d[i]);
        // past the shell's reach and its blend: the faces, a little under where two round together
        if web > -1.3 {
            checked += 1;
            assert!(d[i] <= web + 1e-3, "a bound: {} over the true {web} at {p:?}", d[i]);
            exact += (d[i] >= web - 0.1) as usize;
        }
    }
    assert!(checked > 2000 && exact * 100 >= checked * 95, "{exact} of {checked} exact");
}

/// A foam's kernel reads the true distance to the cells' edges up to its reach, and that reach past
/// it: a strut only where an edge runs, and a bound everywhere.
#[test]
fn voronoi_edges_match_brute_force_and_bound_their_field() {
    let strut = 1.5;
    let (s, cells) = cube(strut);
    let all: Vec<V> = cells.seeds.iter().chain(&cells.core).copied().collect();
    let r = cells.edges_reach(&s);
    let ps = points(150, 11.0);
    let step = [0.033, -0.021, 0.039];
    let shifted: Vec<V> = ps.iter().map(|p| [p[0] + step[0], p[1] + step[1], p[2] + step[2]]).collect();
    let d = field(CUBE, "size: 6mm strut: 1.5mm round: 0.1mm tops: @round", &[ps.clone(), shifted].concat());
    let (here, there) = d.split_at(ps.len());
    let cap = 3.6;
    let (mut checked, mut struts) = (0, 0);
    for (i, p) in ps.iter().enumerate() {
        assert!((there[i] - here[i]).abs() <= dot(step, step).sqrt() * 1.01, "the field steps {} to {} at {p:?}", here[i], there[i]);
        let q = q_of(*p);
        let mut f: Vec<f64> = all.iter().map(|s| far(*s, q)).collect();
        f.sort_by(f64::total_cmp);
        let reach = (f[6].min(r) - f[0]).max(0.0) * 0.5 * 6.0;
        let truth = brute_edge(&all, q, cap);
        let expected = truth.unwrap_or(cap).min(reach);
        let read = here[i] + 0.5 * strut;
        assert!((read.min(cap) - expected).abs() <= 1e-2, "the edge field {read} at {p:?}: the true {truth:?}, reach {reach}");
        checked += truth.is_some() as usize;
        struts += (expected < 0.5 * strut) as usize;
    }
    assert!(checked * 2 >= ps.len() && struts > 3, "{checked} edge distances checked, {struts} in a strut");
}

/// The nodes of the cells round the middle of `seeds` (q), each as its 4 seeds and the point as far
/// from them, with no other seed nearer.
fn nodes(seeds: &[V]) -> Vec<([V; 4], V)> {
    let mut found = Vec::new();
    for (i, s0) in seeds.iter().enumerate().filter(|(_, s)| s.iter().all(|v| v.abs() <= 0.9)) {
        let near: Vec<&V> = seeds[i + 1..].iter().filter(|s| far(**s, *s0) < 2.4).collect();
        for a in 0..near.len() {
            for b in a + 1..near.len() {
                for c in b + 1..near.len() {
                    let s = [*s0, *near[a], *near[b], *near[c]];
                    let (u, v, w) = (sub(s[1], s[0]), sub(s[2], s[0]), sub(s[3], s[0]));
                    let (vw, wu, uv) = (cross(v, w), cross(w, u), cross(u, v));
                    let det = dot(u, vw);
                    if det.abs() < 1e-9 {
                        continue;
                    }
                    let (ru, rv, rw) = (dot(u, u) * 0.5, dot(v, v) * 0.5, dot(w, w) * 0.5);
                    let x = [0, 1, 2].map(|k| s[0][k] + (ru * vw[k] + rv * wu[k] + rw * uv[k]) / det);
                    let r = dot(sub(x, s[0]), sub(x, s[0])) - 1e-9;
                    if r < 4.0 && !seeds.iter().any(|e| dot(sub(*e, x), sub(*e, x)) < r) {
                        found.push((s, x));
                    }
                }
            }
        }
    }
    found
}

/// A strut ends at its node: each triple's line followed past the node, toward the fourth seed,
/// where no edge runs, is out of the struts.
#[test]
fn a_voronoi_strut_ends_at_its_node() {
    let strut = 0.6;
    let (_, cells) = cube(strut);
    let all: Vec<V> = cells.seeds.iter().chain(&cells.core).copied().collect();
    let mut ps = Vec::new();
    for (s, x) in nodes(&all) {
        for skip in 0..4 {
            let t: Vec<V> = (0..4).filter(|i| *i != skip).map(|i| s[i]).collect();
            let mut n = cross(sub(t[1], t[0]), sub(t[2], t[0]));
            if dot(n, sub(s[skip], t[0])) < 0.0 {
                n = n.map(|v| -v);
            }
            let d = [n[0] / M[0], n[1] / M[1], n[2] / M[2]];
            let (len, past) = (dot(d, d).sqrt(), strut * 1.75);
            ps.push([0, 1, 2].map(|i| x[i] / M[i] + d[i] / len * past));
        }
    }
    let d = field(CUBE, "size: 6mm strut: 0.6mm round: 0.1mm tops: @round", &ps);
    let spikes = ps.iter().zip(&d).filter(|(p, e)| {
        let truth = brute_edge(&all, q_of(**p), 3.6).unwrap_or(f64::INFINITY);
        **e < 0.0 && truth > strut * 0.5 * 1.01
    });
    let (spikes, points) = (spikes.count(), ps.len());
    assert!(points >= 40 && spikes == 0, "{spikes} of {points} points past a node lie inside a strut");
}

/// A skin unrolled: `n[0]` round the axis by `n[1]` along it, solid or not.
struct Unrolled {
    n: [usize; 2],
    solid: Vec<bool>,
    /// mm per step, round and along.
    step: [f64; 2],
}

impl Unrolled {
    /// The middle of the skin of a cylinder of radius `r`, from `low` to `high` on the axis.
    fn read(source: &str, opts: &str, r: f64, low: f64, high: f64) -> Unrolled {
        let step = 0.25;
        let round = ((std::f64::consts::TAU * r) / step).round() as usize;
        let along = ((high - low) / step).round() as usize;
        let mut ps = Vec::with_capacity(round * along);
        for j in 0..along {
            let y = low + (j as f64 + 0.5) * step;
            for i in 0..round {
                let a = std::f64::consts::TAU * i as f64 / round as f64;
                ps.push([r * a.cos(), y, r * a.sin()]);
            }
        }
        let d = field(source, opts, &ps);
        Unrolled { n: [round, along], solid: d.iter().map(|d| *d < 0.0).collect(), step: [std::f64::consts::TAU * r / round as f64, step] }
    }

    fn next(&self, i: usize) -> impl Iterator<Item = usize> {
        let [w, h] = self.n;
        let (x, y) = (i % w, i / w);
        let mut out = [None; 4];
        out[0] = Some(y * w + (x + 1) % w);
        out[1] = Some(y * w + (x + w - 1) % w);
        out[2] = (y + 1 < h).then(|| (y + 1) * w + x);
        out[3] = (y > 0).then(|| (y - 1) * w + x);
        out.into_iter().flatten()
    }

    /// The connected pieces of what `of` says, by their sizes in points.
    fn pieces(&self, of: impl Fn(bool) -> bool) -> Vec<usize> {
        let mut left: Vec<bool> = self.solid.iter().map(|s| of(*s)).collect();
        let mut out = Vec::new();
        for start in 0..left.len() {
            if left[start] {
                left[start] = false;
                let (mut stack, mut size) = (vec![start], 0);
                while let Some(i) = stack.pop() {
                    size += 1;
                    for j in self.next(i) {
                        if left[j] {
                            left[j] = false;
                            stack.push(j);
                        }
                    }
                }
                out.push(size);
            }
        }
        out
    }

    /// The largest distance (mm) over the skin from solid to a hole, in steps along the grid.
    fn deepest(&self) -> f64 {
        let mut dist: Vec<u32> = self.solid.iter().map(|s| if *s { u32::MAX } else { 0 }).collect();
        let mut front: Vec<usize> = (0..dist.len()).filter(|i| dist[*i] == 0).collect();
        let mut level = 0;
        while !front.is_empty() {
            level += 1;
            let mut next = Vec::new();
            for i in front {
                for j in self.next(i) {
                    if dist[j] == u32::MAX {
                        dist[j] = level;
                        next.push(j);
                    }
                }
            }
            front = next;
        }
        level.saturating_sub(1) as f64 * self.step[0].min(self.step[1])
    }

    /// The share of the skin in a ring of heights, solid.
    fn solid_share(&self, rows: std::ops::Range<usize>) -> f64 {
        let [w, _] = self.n;
        let n = rows.len() * w;
        rows.flat_map(|j| (0..w).map(move |i| j * w + i)).filter(|i| self.solid[*i]).count() as f64 / n as f64
    }
}

/// Over a curved skin the holes are one size, about `size` less a strut, with no slivers among them
/// and no closed patch wider than where three struts meet.
#[test]
fn voronoi_web_is_even_over_a_curved_skin() {
    let (r, h, size, strut) = (24.0, 60.0, 6.0, 1.5);
    let source = format!("let b = m.cylinder({{radius: {r}mm height: {h}mm}})");
    let opts = format!("size: {size}mm strut: {strut}mm core: @none");
    let skin = Unrolled::read(&source, &opts, r - strut, -0.4 * h, 0.4 * h);
    let area = skin.step[0] * skin.step[1];
    let holes: Vec<f64> = skin.pieces(|s| !s).iter().map(|n| (4.0 / std::f64::consts::PI * *n as f64 * area).sqrt()).collect();
    let mean = holes.iter().sum::<f64>() / holes.len() as f64;
    let spread = (holes.iter().map(|d| (d - mean) * (d - mean)).sum::<f64>() / holes.len() as f64).sqrt() / mean;
    // those cut by the window's ends are smaller: only the whole ones say how even they are
    let whole = holes.iter().filter(|d| **d > 0.4 * size).count();
    assert!(whole * 100 >= holes.len() * 75, "slivers: {whole} of {} holes over 0.4 cells", holes.len());
    assert!((0.8 * size..1.6 * size).contains(&mean), "holes about a cell across: {mean}");
    assert!(spread < 0.35, "even holes: {spread} of {holes:?}");
    assert!(skin.deepest() < 4.5, "a closed patch: {} mm from any hole", skin.deepest());
    let pieces = skin.pieces(|s| s);
    assert!(pieces.len() == 1, "one piece in the window's middle: {pieces:?}");
}

/// A web's foot and collar are solid all round, its struts merge into both, its underside is flat:
/// the rings hold, the first layer has no hole, and all of it is one piece.
#[test]
fn voronoi_stands_on_its_base_under_its_rim() {
    let (r, h) = (24.0, 60.0);
    let source = format!("let b = m.cylinder({{radius: {r}mm height: {h}mm}})");
    let opts = "size: 6mm strut: 1.5mm core: @none base: 4mm rim: 6mm";
    let skin = Unrolled::read(&source, opts, r - 1.5, -0.5 * h, 0.5 * h);
    let row = |y: f64| (y / skin.step[1]) as usize;
    assert_eq!(skin.solid_share(0..row(3.8)), 1.0, "the foot");
    assert_eq!(skin.solid_share(row(h - 5.8)..row(h)), 1.0, "the collar");
    let middle = skin.solid_share(row(10.0)..row(h - 12.0));
    assert!((0.15..0.7).contains(&middle), "holes between them: {middle}");
    let pieces = skin.pieces(|s| s);
    assert!(pieces.len() == 1, "struts merge into both: {pieces:?}");
    // flat on the bed: the whole underside under a thin first layer
    let bed: Vec<V> = (0..60).map(|i| {
        let a = i as f64 * 0.7;
        let (u, v) = (a.cos(), a.sin());
        let at = (r - 0.6) * (i as f64 + 1.0) / 60.0;
        [at * u, -0.5 * h + 0.1, at * v]
    }).collect();
    let d = field(&source, opts, &bed);
    assert!(d.iter().all(|d| *d < 0.0), "a clean first layer: {d:?}");
}

/// A rod thinner than a cell stays whole: a hole across it would cut it in two.
#[test]
fn voronoi_keeps_a_rod_whole() {
    let source = "let b = m.cylinder({radius: 2.5mm height: 40mm})";
    let along: Vec<V> = (0..80).map(|j| [0.0, j as f64 * 0.45 - 17.8, 0.0]).collect();
    // solid round the axis at every height: the rod's core reads solid under its skin
    let rings: Vec<V> = along.iter().flat_map(|p| (0..8).map(move |i| [1.0 * (i as f64 * 0.785).cos(), p[1], 1.0 * (i as f64 * 0.785).sin()])).collect();
    let d = field(source, "size: 6mm strut: 1.5mm core: @none", &rings);
    let solid = d.iter().filter(|d| **d < 0.0).count();
    assert_eq!(solid, rings.len(), "the rod's middle at every height");
}

/// A small edit of the part keeps its cells, so a drag does not make the web jump.
#[test]
fn voronoi_cells_stay_put_under_a_small_edit() {
    let spec = voronoi::Spec { size: 6.0, stretch: 1.0, axis: 1, skin: 3.0, random: 0.6, salt: 0, base: 0.0, rim: 0.0, strut: 1.5, thinnest: 1.5, round: 0.1, tan: Some(1.0), unit: 1000.0 };
    let cells = |r: f64| voronoi::cells(&csg_sdf::SdfNode::Sphere { radius: r }, ([-r; 3], [r; 3]), &spec, false);
    let (was, now) = (cells(20.0), cells(20.2));
    let moved = now.seeds.iter().filter(|q| was.seeds.iter().all(|p| spec.size * far(**q, *p) > spec.strut)).count();
    assert!(moved * 3 < now.seeds.len(), "{moved} of {} seeds moved over a strut", now.seeds.len());
}

/// The first program under a node.
fn first_kernel(n: &csg_sdf::SdfNode) -> Option<&csg_sdf::SdfKernel> {
    use csg_sdf::SdfNode::*;
    match n {
        Kernel(k) => Some(k),
        Combine { of, .. } => of.iter().find_map(first_kernel),
        Transform { of, .. } | Round { of, .. } | Shell { of, .. } | Id { of, .. } => first_kernel(of),
        _ => None,
    }
}

/// However a table lays its lists out, on a multiple or in wider buckets, the kernel reads the field
/// it reads from the plain table: only the walking differs.
#[test]
fn voronoi_field_does_not_depend_on_its_tables_layout() {
    let (spec, cells) = cube(1.5);
    let expr = with_shape(CUBE, "size: 6mm strut: 1.5mm round: 0.1mm tops: @round core: @none", |shape| {
        first_kernel(&shape.root).and_then(|k| k.splash.clone()).expect("the web's kernel")
    });
    // through the skin, in the kernel's own units: the seeds reach all of it
    let skin = points(4000, 20.0).into_iter().filter(|p| p.iter().any(|x| x.abs() > 17.0));
    let xyz: Vec<f32> = skin.flat_map(|p| p.map(|x| (x / 1000.0) as f32)).collect();
    let read = |table: Vec<f32>| -> (Vec<f32>, f32, f32) {
        let (width, every) = (1.0 / table[3], table[8]);
        let grid = csg_sdf::SdfGrid::new([0.0; 3], 1.0, [2, 2, 2], 0.0, vec![0.0; 8].into()).with_table(table.into());
        let mut out = vec![0.0; xyz.len() / 3];
        expr.with_grid(grid).distance_batch(&xyz, &mut out);
        (out, width, every)
    };
    let (plain, width, every) = read(cells.faces_table(&spec));
    assert_eq!((width, every), (0.75, 1.0));
    assert!(plain.iter().any(|d| *d < 0.0) && plain.iter().any(|d| *d > 0.0), "a web to read");
    let long = cells.faces_table(&spec).len();
    for (name, exact, widen, wider, multiple) in [("on a multiple", long / 4, f64::INFINITY, false, true), ("in wider buckets", long, 1.0, true, false), ("both", 1 << 10, 1.0, true, true)] {
        let (d, w, e) = read(cells.faces_table_within(&spec, exact, widen));
        assert_eq!((w > 0.75, e > 1.0), (wider, multiple), "{name}: buckets {w}, every {e}");
        let worst = plain.iter().zip(&d).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
        // a list of more seeds rounds a little differently, in the f32's last place
        assert!(worst <= if wider { 1e-7 } else { 0.0 }, "{name}: {worst} m off");
    }
}

/// A part whose table is longer than an f32 counts words is a part like any other: no `size` is
/// asked of it, and its lists, some past a loop's 1024, read to their ends.
#[test]
fn voronoi_takes_a_table_of_any_length() {
    let spec = voronoi::Spec { size: 3.0, stretch: 1.6, axis: 1, skin: 1.0, random: 0.6, salt: 0, base: 0.0, rim: 0.0, strut: 8.0, thinnest: 8.0, round: 4.0, tan: Some(1.0), unit: 1000.0 };
    let cells = voronoi::cells(&csg_sdf::SdfNode::Sphere { radius: 24.0 }, ([-24.0; 3], [24.0; 3]), &spec, true);
    let table = cells.edges_table(&spec);
    assert!(table.len() > 1 << 24 && table[8] > 1.0, "{} words, lists every {}", table.len(), table[8]);
    let buckets = (table[4] * table[5] * table[6]) as usize;
    let longest = table[9..9 + buckets].iter().map(|w| table[*w as usize * table[8] as usize]).fold(0.0, f32::max);
    assert!(longest > 1024.0, "the longest list has {longest} seeds");
    let d = field("let b = m.sphere({radius: 24mm})", "size: 3mm strut: 8mm skin: 1mm", &[[23.5, 0.0, 0.0], [0.0, 0.0, 30.0]]);
    assert!(d[0] < 0.0 && d[1] > 0.0, "the skin and the air around it: {d:?}");
}

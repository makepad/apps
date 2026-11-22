//! A slime mould's veins as tubes ([`centrelines`]): the line down the middle of each vein over the
//! skin, and at each of its points how far it is to the vein's edge (the vein's half-width there),
//! as round-cone pieces a shape raises from the part's surface (`m.veins`).
//!
//! The veins are the skin cells whose trail is over the threshold their colour is cut at
//! ([`super::physarum::places`]). Each vein cell's way over the skin to the vein's edge is the
//! least over its neighbours of the way to the edge between them (where the trail's logarithm
//! crosses the threshold's), then on through the vein. The vein is then thinned to its middle: in
//! order of that way, nearest the edge first and over again till none goes, a cell goes when it
//! is on the edge, has more than one vein neighbour left (an end stays) and its vein neighbours
//! and its others each stay one piece round it (so no vein breaks and no mesh fills). What is left
//! is a line a cell wide down the middle of each vein, its points that way from the edge. A spur
//! shorter than [`SPUR`] times the half-width where it leaves a vein is the edge's ragged bit, not
//! a branch, and goes. The lines are traced from end to junction. Where veins meet, the trail's
//! blob is wider than any of them: each vein is as wide there as it is once out of the blob, so
//! the dome is the widest vein's. The lines are smoothed ([`SMOOTH`] times each point toward its
//! neighbours', its half-width too, the ends and junctions staying), and cut into as few straight
//! pieces as keep within [`TOLERANCE`] of them, position and half-width.
//!
//! On each piece [`Piece::tube`] raises a round cone as wide as the vein and half as high (from
//! [`LOWEST`] to [`TALLEST`]), its cross-section the circle through its edges on the surface and
//! its top, cut off [`BELOW`] under the surface. [`printable`] keeps the tubes whose bare skin
//! (out of the part and the other tubes) faces down nowhere steeper than the printer builds
//! without support, less the scraps and stubs that leaves, and [`raised`] grows the mould and
//! does it all.

use super::band::{Level, Many, Surface};
use super::physarum::veined;
use super::rd::State;
use super::Spec;
use crate::bake::Fan;
use std::cmp::Reverse;
use std::collections::BinaryHeap;

/// A spur shorter than this times the half-width of the vein it leaves goes.
const SPUR: f64 = 2.0;
/// How many times each point of a line moves halfway to its neighbours' middle, and its
/// half-width.
const SMOOTH: usize = 4;
const SMOOTH_WIDTH: usize = 16;
/// A line between two junctions shorter than this (cells) is a link within one junction.
const LINK: f64 = 1.5;
/// How far out of a junction's circle (times its radius) a vein may still flare into it.
const FLARE: f64 = 2.0;
/// How far (mm) a straight piece may stray from the line it stands for, and its half-width from
/// the line's.
const TOLERANCE: f64 = 0.04;
/// The thinnest tube's half-width (mm): two lines of a 0.4 mm nozzle across at its foot.
pub const THINNEST: f64 = 0.5;
/// A tube's height over its half-width.
const RISE: f64 = 0.5;
/// The lowest tube's height and the tallest's (mm): two layers of 0.2 mm, and eight.
pub const LOWEST: f64 = 0.4;
pub const TALLEST: f64 = 1.6;
/// How far under the surface a tube reaches at most (mm): the round of a low wide one goes deep,
/// and would show through a thin wall.
pub const BELOW: f64 = 0.8;
/// The points round a tube its skin is looked at by, and the angle between the rings over its
/// round ends: every 15°, as fine as the margin the host keeps from the printer's overhang angle,
/// so a bare way down steeper than that margin allows is not missed between them.
const AROUND: usize = 24;
const RING: f64 = std::f64::consts::PI / 12.0;
/// How far out of the part (mm) a point of a tube's skin is bare, and how far into another tube
/// it is held.
const EXPOSED: f64 = 0.05;
/// The buckets tubes are found near by (mm).
const BUCKET: f64 = 2.0;
/// A vein the tubes end in (from a free end to a junction) shorter than this many times its
/// width goes: the stub a cut leaves, or a ragged end.
const STUB: f64 = 2.0;
/// The shortest network of tubes kept, as a share of the longest along their middles: shorter
/// is a scrap of a vein the rest of which does not print, noise off the network.
const SCRAP: f64 = 0.1;

/// A piece of a vein's middle: from `a` to `b` (mm, the surface's frame), the vein's half-width
/// at each (mm), and the way out of the surface at each.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Piece {
    pub a: [f64; 3],
    pub ra: f64,
    pub na: [f64; 3],
    pub b: [f64; 3],
    pub rb: f64,
    pub nb: [f64; 3],
}

impl Piece {
    /// The tube raised on it: as wide as the vein (at least [`THINNEST`] either side of its
    /// middle), [`RISE`] times that half-width high (from [`LOWEST`] to [`TALLEST`]), its
    /// cross-section the circle through its two edges on the surface and its top (so a wide one
    /// is a low dome), cut off [`BELOW`] under the surface.
    pub fn tube(&self) -> Tube {
        let end = |p: [f64; 3], r: f64, n: [f64; 3]| {
            let w = r.max(THINNEST);
            let h = (RISE * w).clamp(LOWEST, TALLEST);
            let radius = (w * w + h * h) / (2.0 * h);
            (std::array::from_fn(|c| p[c] - (radius - h) * n[c]), radius)
        };
        let (a, ra) = end(self.a, self.ra, self.na);
        let (b, rb) = end(self.b, self.rb, self.nb);
        let cut = unit(std::array::from_fn(|c| self.na[c] + self.nb[c]));
        let under = 0.5 * (dot(self.a, cut) + dot(self.b, cut)) - BELOW;
        Tube { a, ra, b, rb, cut, under }
    }
}

/// A round cone from `a` (radius `ra`) to `b` (radius `rb`), mm, less what is past the plane
/// `dot(p, cut) = under` on the side away from `cut`: a tube raised on a [`Piece`] of a vein.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tube {
    pub a: [f64; 3],
    pub ra: f64,
    pub b: [f64; 3],
    pub rb: f64,
    pub cut: [f64; 3],
    pub under: f64,
}

impl Tube {
    /// A bound on the distance from `p` to it (the round cone's, exact, and the plane's).
    pub fn distance(&self, p: [f64; 3]) -> f64 {
        round_cone(p, self.a, self.ra, self.b, self.rb).max(self.under - dot(p, self.cut))
    }

    /// The words a kernel reads it by: a, ra, b, rb, cut and under.
    pub fn words(&self) -> [f32; 12] {
        let [a, b, c] = [self.a, self.b, self.cut].map(|v| v.map(|x| x as f32));
        [a[0], a[1], a[2], self.ra as f32, b[0], b[1], b[2], self.rb as f32, c[0], c[1], c[2], self.under as f32]
    }

    fn reach(&self) -> ([f64; 3], [f64; 3]) {
        (std::array::from_fn(|c| (self.a[c] - self.ra).min(self.b[c] - self.rb)), std::array::from_fn(|c| (self.a[c] + self.ra).max(self.b[c] + self.rb)))
    }

    /// Points over its skin and the way out of it at each: round its side at either end and
    /// halfway, over its round ends, and over the face its cut leaves.
    fn skin(&self) -> Vec<([f64; 3], [f64; 3])> {
        let (a, b, ra, rb) = (self.a, self.b, self.ra, self.rb);
        let ab = sub(b, a);
        let l = dot(ab, ab).sqrt();
        let mut out = Vec::new();
        let ring = |u: [f64; 3]| -> Vec<[f64; 3]> {
            let e1 = unit(cross(u, if u[0].abs() < 0.9 { [1.0, 0.0, 0.0] } else { [0.0, 1.0, 0.0] }));
            let e2 = cross(u, e1);
            (0..AROUND).map(|k| {
                let t = std::f64::consts::TAU * k as f64 / AROUND as f64;
                std::array::from_fn(|c| t.cos() * e1[c] + t.sin() * e2[c])
            }).collect()
        };
        let ball = |out: &mut Vec<([f64; 3], [f64; 3])>, c: [f64; 3], r: f64, u: [f64; 3], reach: f64| {
            // the round end about `u`, out to `reach` from it
            out.push((std::array::from_fn(|i| c[i] + r * u[i]), u));
            let rings = (reach / RING).ceil().max(1.0) as usize;
            for e in ring(u) {
                for f in 1..=rings {
                    let t = f as f64 / rings as f64 * reach;
                    let m: [f64; 3] = std::array::from_fn(|i| t.cos() * u[i] + t.sin() * e[i]);
                    out.push((std::array::from_fn(|i| c[i] + r * m[i]), m));
                }
            }
        };
        if l <= (ra - rb).abs() {
            // one ball holds the other
            let (c, r) = if ra >= rb { (a, ra) } else { (b, rb) };
            ball(&mut out, c, r, [0.0, 1.0, 0.0], std::f64::consts::PI);
            ball(&mut out, c, r, [0.0, -1.0, 0.0], std::f64::consts::FRAC_PI_2);
        } else {
            let u = ab.map(|x| x / l);
            let s = (ra - rb) / l;
            let c = (1.0 - s * s).sqrt();
            for e in ring(u) {
                let m: [f64; 3] = std::array::from_fn(|i| c * e[i] + s * u[i]);
                for t in [0.0, 0.5, 1.0] {
                    out.push((std::array::from_fn(|i| a[i] + ra * m[i] + t * (b[i] + rb * m[i] - a[i] - ra * m[i])), m));
                }
            }
            ball(&mut out, a, ra, u.map(|x| -x), (-s).acos());
            ball(&mut out, b, rb, u, s.acos());
        }
        // the cut's face, where it crosses the round cone, facing in
        let side = unit(cross(self.cut, ab));
        for t in [0.0, 0.5, 1.0] {
            let centre: [f64; 3] = std::array::from_fn(|i| a[i] + t * ab[i]);
            let r = ra + t * (rb - ra);
            let h = self.under - dot(centre, self.cut);
            if h.abs() < r {
                let w = (r * r - h * h).sqrt();
                for k in [-0.8, 0.0, 0.8] {
                    out.push((std::array::from_fn(|i| centre[i] + h * self.cut[i] + k * w * side[i]), self.cut.map(|x| -x)));
                }
            }
        }
        out
    }
}

/// The tubes over `pieces` that print without support: what of each stands out of the part (the
/// distance `distance` reads) and the tubes left faces down nowhere steeper than `lowest` (the
/// least up-component of a way out of a surface the printer builds on nothing, along unit `up`),
/// and none of it is under `floor` along `up` (the part's lowest point). A tube that does not is
/// left out, and so is a network of those left (tubes joined where the middle's end of one is in
/// another) shorter than [`SCRAP`] of the longest along its middle (a scrap of a vein the rest of
/// which does not print) and a vein they end in shorter than [`STUB`] widths (the stub a cut
/// leaves). The rest are looked at again, till none goes.
pub fn printable(pieces: &[Piece], distance: &Many, up: [f64; 3], lowest: f64, floor: f64) -> Vec<Tube> {
    let tubes: Vec<Tube> = pieces.iter().map(Piece::tube).collect();
    let skins: Vec<Vec<([f64; 3], [f64; 3])>> = tubes.iter().map(Tube::skin).collect();
    let first: Vec<usize> = skins.iter().scan(0, |n, s| {
        let at = *n;
        *n += s.len();
        Some(at)
    }).collect();
    let points: Vec<[f64; 3]> = skins.iter().flatten().map(|s| s.0).collect();
    let mut outside = vec![0.0; points.len()];
    distance(&points, &mut outside);
    let near = Near::new(&tubes, BUCKET);
    let cull = Cull { pieces, tubes, skins, first, outside, near, up, lowest, floor };
    let mut kept = vec![true; cull.tubes.len()];
    loop {
        let hung: Vec<usize> = (0..kept.len()).filter(|&i| kept[i] && cull.hangs(&kept, i)).collect();
        for &i in &hung {
            kept[i] = false;
        }
        if hung.is_empty() && !cull.scraps(&mut kept) && !cull.stubs(&mut kept) {
            break;
        }
    }
    cull.tubes.into_iter().zip(kept).filter(|(_, k)| *k).map(|(t, _)| t).collect()
}

/// What [`printable`] looks at: the tubes, the points over their skins (each tube's from its
/// `first`) and how far out of the part each is.
struct Cull<'a> {
    pieces: &'a [Piece],
    tubes: Vec<Tube>,
    skins: Vec<Vec<([f64; 3], [f64; 3])>>,
    first: Vec<usize>,
    outside: Vec<f64>,
    near: Near,
    up: [f64; 3],
    lowest: f64,
    floor: f64,
}

impl Cull<'_> {
    /// The points of tube `i`'s skin that need holding up if bare: out of the part, on its cut's
    /// side, and facing down steeper than the printer builds on nothing or under the floor.
    fn needy(&self, i: usize) -> impl Iterator<Item = [f64; 3]> + '_ {
        let t = &self.tubes[i];
        self.skins[i].iter().enumerate().filter(move |(k, (p, m))| {
            self.outside[self.first[i] + k] > EXPOSED && dot(*p, t.cut) >= t.under - EXPOSED && (dot(*m, self.up) < self.lowest || dot(*p, self.up) < self.floor)
        }).map(|(_, (p, _))| *p)
    }

    /// Whether a point of tube `i`'s skin that needs holding up is in no other `kept` tube.
    fn hangs(&self, kept: &[bool], i: usize) -> bool {
        self.needy(i).any(|p| self.near.clear(&self.tubes, kept, i, p))
    }

    /// Whether tube `j`'s skin out of the part is all in tube `i` (to within [`EXPOSED`]): it adds
    /// nothing to it.
    fn within(&self, j: usize, i: usize) -> bool {
        let t = &self.tubes[j];
        self.skins[j].iter().enumerate().all(|(k, (p, _))| self.outside[self.first[j] + k] <= EXPOSED || dot(*p, t.cut) < t.under - EXPOSED || self.tubes[i].distance(*p) <= EXPOSED)
    }

    /// Leaves out each vein the `kept` tubes end in shorter than [`STUB`] times its width (its
    /// half-width's mean along it, twice): its pieces from a free end of one (an end no other kept
    /// piece has, in no other kept tube but one within its own) to where it joins others (an end
    /// three or more have, or in another tube). Whether any went.
    fn stubs(&self, kept: &mut [bool]) -> bool {
        let pieces = self.pieces;
        let key = |p: [f64; 3]| p.map(f64::to_bits);
        let mut at: std::collections::HashMap<[u64; 3], Vec<usize>> = std::collections::HashMap::new();
        for i in (0..pieces.len()).filter(|&i| kept[i]) {
            at.entry(key(pieces[i].a)).or_default().push(i);
            at.entry(key(pieces[i].b)).or_default().push(i);
        }
        // whether a kept tube not of `vein` holds its end `q` on piece `i` (it joins there as at
        // a junction), one within `i` aside
        let held = |q: [u64; 3], vein: &[usize], i: usize| {
            let p = q.map(f64::from_bits);
            self.near.at(p).iter().map(|&j| j as usize).any(|j| !vein.contains(&j) && kept[j] && self.tubes[j].distance(p) < 0.0 && !self.within(j, i))
        };
        let mut stubs: Vec<Vec<usize>> = Vec::new();
        for (end, list) in &at {
            if list.len() != 1 {
                continue;
            }
            // the vein from this end, along ends two pieces have
            let (mut i, mut here, mut vein, mut length, mut area) = (list[0], *end, Vec::new(), 0.0, 0.0);
            let (far_end, last) = loop {
                vein.push(i);
                let p = pieces[i];
                let l = far(p.a, p.b);
                length += l;
                area += l * 0.5 * (p.ra.max(THINNEST) + p.rb.max(THINNEST));
                let there = if key(p.a) == here { key(p.b) } else { key(p.a) };
                let next = match at[&there].as_slice() {
                    &[j, k] => Some(if j == i { k } else { j }),
                    _ => None,
                };
                match next {
                    Some(n) if !vein.contains(&n) => (i, here) = (n, there),
                    _ => break (there, i),
                }
            };
            // free here, and at the far end three or more pieces meet or another tube holds it
            let joined = at[&far_end].len() >= 3 || held(far_end, &vein, last);
            if !held(*end, &vein, list[0]) && joined && (length == 0.0 || length * length < STUB * 2.0 * area) {
                stubs.push(vein);
            }
        }
        for &i in stubs.iter().flatten() {
            kept[i] = false;
        }
        !stubs.is_empty()
    }

    /// Leaves out the networks of the `kept` tubes shorter than [`SCRAP`] of the longest along
    /// their middles (a tube joined to another where an end of its piece's middle is in it).
    /// Whether any went.
    fn scraps(&self, kept: &mut [bool]) -> bool {
        fn root(parent: &mut [usize], mut i: usize) -> usize {
            while parent[i] != i {
                parent[i] = parent[parent[i]];
                i = parent[i];
            }
            i
        }
        let (pieces, tubes) = (self.pieces, &self.tubes);
        let mut parent: Vec<usize> = (0..tubes.len()).collect();
        let on: Vec<usize> = (0..tubes.len()).filter(|&i| kept[i]).collect();
        for &i in &on {
            for q in [pieces[i].a, pieces[i].b] {
                for &j in self.near.at(q) {
                    let j = j as usize;
                    if j != i && kept[j] && tubes[j].distance(q) < 0.0 {
                        let (a, b) = (root(&mut parent, i), root(&mut parent, j));
                        parent[a] = b;
                    }
                }
            }
        }
        let mut length = vec![0.0; tubes.len()];
        for &i in &on {
            let r = root(&mut parent, i);
            length[r] += far(pieces[i].a, pieces[i].b);
        }
        let least = SCRAP * length.iter().copied().fold(0.0, f64::max);
        let mut any = false;
        for &i in &on {
            if length[root(&mut parent, i)] < least {
                kept[i] = false;
                any = true;
            }
        }
        any
    }
}

/// The tubes of the veins the slime mould `spec` grows on `surface` to its steps (its running
/// simulation's state when one is, else a new one's), that print without support ([`printable`])
/// with the printer's up along unit `up` (the surface's frame) and `lowest` its steepest way down
/// out of a surface that needs none. None: `stop` ended it first, or the surface has no skin.
pub fn raised(spec: &Spec, surface: &Surface, up: [f64; 3], lowest: f64, fan: Fan, stop: &dyn Fn() -> bool) -> Option<Vec<Tube>> {
    let made = || Some(surface.clone());
    let sim = super::live::grown_sim(spec, surface.key, &made, fan, stop)?;
    let (level, state) = sim.now();
    let skin = &*state.crowd.skin;
    let floor = skin.cells.iter().map(|&c| dot(level.surface[c as usize].map(|v| v as f64 * level.pitch), up)).fold(f64::INFINITY, f64::min);
    Some(printable(&centrelines(level, &state, fan), &surface.distance, up, lowest, floor))
}

/// The tubes near each bucket of a grid over them: those whose box reaches it.
struct Near {
    origin: [f64; 3],
    size: f64,
    dims: [usize; 3],
    lists: Vec<Vec<u32>>,
}

impl Near {
    fn new(tubes: &[Tube], size: f64) -> Near {
        let (mut lo, mut hi) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
        for t in tubes {
            let (a, b) = t.reach();
            for c in 0..3 {
                lo[c] = lo[c].min(a[c]);
                hi[c] = hi[c].max(b[c]);
            }
        }
        if tubes.is_empty() {
            (lo, hi) = ([0.0; 3], [0.0; 3]);
        }
        let dims: [usize; 3] = std::array::from_fn(|c| ((hi[c] - lo[c]) / size).ceil() as usize + 1);
        let mut lists = vec![Vec::new(); dims[0] * dims[1] * dims[2]];
        for (i, t) in tubes.iter().enumerate() {
            let (a, b) = t.reach();
            let first: [usize; 3] = std::array::from_fn(|c| ((a[c] - lo[c]) / size) as usize);
            let last: [usize; 3] = std::array::from_fn(|c| (((b[c] - lo[c]) / size) as usize).min(dims[c] - 1));
            for z in first[2]..=last[2] {
                for y in first[1]..=last[1] {
                    for x in first[0]..=last[0] {
                        lists[x + dims[0] * (y + dims[1] * z)].push(i as u32);
                    }
                }
            }
        }
        Near { origin: lo, size, dims, lists }
    }

    /// The tubes whose box reaches `p`'s bucket (none off the grid).
    fn at(&self, p: [f64; 3]) -> &[u32] {
        let mut cell = [0usize; 3];
        for c in 0..3 {
            let u = ((p[c] - self.origin[c]) / self.size).floor();
            if u < 0.0 || u >= self.dims[c] as f64 {
                return &[];
            }
            cell[c] = u as usize;
        }
        &self.lists[cell[0] + self.dims[0] * (cell[1] + self.dims[1] * cell[2])]
    }

    /// Whether `p` is in no kept tube but `own` (deeper than [`EXPOSED`]).
    fn clear(&self, tubes: &[Tube], kept: &[bool], own: usize, p: [f64; 3]) -> bool {
        self.at(p).iter().all(|&j| {
            let j = j as usize;
            j == own || !kept[j] || tubes[j].distance(p) > -EXPOSED
        })
    }
}

/// The distance from `p` to the round cone from `a` (radius `ra`) to `b` (radius `rb`): the
/// hull of the two balls, or the larger when it holds the other.
fn round_cone(p: [f64; 3], a: [f64; 3], ra: f64, b: [f64; 3], rb: f64) -> f64 {
    let ab = sub(b, a);
    let l = dot(ab, ab).sqrt();
    if l <= (ra - rb).abs() {
        return (far(p, a) - ra).min(far(p, b) - rb);
    }
    let u = ab.map(|x| x / l);
    let pa = sub(p, a);
    let y = dot(pa, u);
    let x = (dot(pa, pa) - y * y).max(0.0).sqrt();
    let s = (ra - rb) / l;
    let c = (1.0 - s * s).sqrt();
    let k = c * y - s * x;
    if k < 0.0 {
        (x * x + y * y).sqrt() - ra
    } else if k > c * l {
        (x * x + (y - l) * (y - l)).sqrt() - rb
    } else {
        x * c + y * s - ra
    }
}

/// The middle of each vein of a slime mould's state on `level` and its half-width along it, as
/// straight pieces (see the module).
pub fn centrelines(level: &Level, state: &State, fan: Fan) -> Vec<Piece> {
    let Some((trail, lo)) = veined(state, fan) else { return Vec::new() };
    let skin = &*state.crowd.skin;
    let n = trail.len();
    let next = |k: usize| &skin.next[skin.start[k] as usize..skin.start[k + 1] as usize];
    let at = |k: usize| level.surface[skin.cells[k] as usize].map(|v| v as f64 * level.pitch);
    let out = |k: usize| skin.up(skin.cells[k] as usize).map(|v| v as f64);
    let vein: Vec<bool> = trail.iter().map(|&t| t > lo).collect();
    // each vein cell's way to the vein's edge
    let mut edge = vec![f64::INFINITY; n];
    let mut heap = BinaryHeap::new();
    for k in (0..n).filter(|&k| vein[k]) {
        for &j in next(k) {
            let j = j as usize;
            if !vein[j] {
                // where between the two the trail crosses the threshold, by its logarithm
                let (tk, tj) = (trail[k] as f64, trail[j] as f64);
                let f = if tj > 0.0 { (tk / lo as f64).ln() / (tk / tj).ln() } else { (tk - lo as f64) / tk };
                edge[k] = edge[k].min(f.clamp(0.0, 1.0) * far(at(k), at(j)));
            }
        }
        if edge[k].is_finite() {
            heap.push(Reverse((edge[k].to_bits(), k)));
        }
    }
    while let Some(Reverse((bits, k))) = heap.pop() {
        let d = f64::from_bits(bits);
        if d > edge[k] {
            continue;
        }
        for &j in next(k) {
            let j = j as usize;
            let e = d + far(at(k), at(j));
            if vein[j] && e < edge[j] {
                edge[j] = e;
                heap.push(Reverse((e.to_bits(), j)));
            }
        }
    }
    // thinned to the middle, nearest the edge first
    let mut on = vein.clone();
    let mut order: Vec<usize> = (0..n).filter(|&k| vein[k]).collect();
    order.sort_by(|&a, &b| edge[a].total_cmp(&edge[b]).then(a.cmp(&b)));
    let pieces = |on: &[bool], k: usize, side: bool| -> usize {
        let around = next(k);
        let mut seen = vec![false; around.len()];
        let mut count = 0;
        for s in 0..around.len() {
            if seen[s] || on[around[s] as usize] != side {
                continue;
            }
            count += 1;
            seen[s] = true;
            let mut stack = vec![s];
            while let Some(i) = stack.pop() {
                let near = next(around[i] as usize);
                for t in 0..around.len() {
                    if !seen[t] && on[around[t] as usize] == side && near.contains(&around[t]) {
                        seen[t] = true;
                        stack.push(t);
                    }
                }
            }
        }
        count
    };
    loop {
        let mut thinned = false;
        for &k in &order {
            if !on[k] {
                continue;
            }
            let left = next(k).iter().filter(|&&j| on[j as usize]).count();
            if left <= 1 || left == next(k).len() {
                continue;
            }
            if pieces(&on, k, true) == 1 && pieces(&on, k, false) == 1 {
                on[k] = false;
                thinned = true;
            }
        }
        if !thinned {
            break;
        }
    }
    // the middle as a graph: each cell left joined to the ones left round it, less the long side
    // of each three joined round (a step across the lattice's diagonal), so a cell along a line
    // has two
    let mut adj: Vec<Vec<usize>> = (0..n).map(|k| if on[k] { next(k).iter().map(|&j| j as usize).filter(|&j| on[j]).collect() } else { Vec::new() }).collect();
    for k in 0..n {
        let mut i = 0;
        while i < adj[k].len() {
            let a = adj[k][i];
            let mut cut = false;
            for &b in &adj[k] {
                if b != a && adj[a].contains(&b) {
                    // three joined round: the longest side goes (by index on a tie)
                    let sides = [(far(at(k), at(a)), k.min(a), k.max(a)), (far(at(k), at(b)), k.min(b), k.max(b)), (far(at(a), at(b)), a.min(b), a.max(b))];
                    let longest = sides.iter().copied().fold(sides[0], |m, s| if (s.0, s.1, s.2) > (m.0, m.1, m.2) { s } else { m });
                    if (longest.1, longest.2) == (k.min(a), k.max(a)) {
                        cut = true;
                        break;
                    }
                }
            }
            if cut {
                adj[k].remove(i);
                adj[a].retain(|&j| j != k);
            } else {
                i += 1;
            }
        }
    }
    // the lines from each end and junction, along cells of two neighbours
    let trace = |adj: &[Vec<usize>]| -> Vec<Vec<usize>> {
        let mut used = std::collections::HashSet::new();
        let mut lines = Vec::new();
        let walk = |from: usize, first: usize, used: &mut std::collections::HashSet<(usize, usize)>| -> Option<Vec<usize>> {
            if !used.insert((from.min(first), from.max(first))) {
                return None;
            }
            let (mut line, mut prev, mut here) = (vec![from, first], from, first);
            while adj[here].len() == 2 && here != from {
                let to = if adj[here][0] == prev { adj[here][1] } else { adj[here][0] };
                if !used.insert((here.min(to), here.max(to))) {
                    break;
                }
                line.push(to);
                (prev, here) = (here, to);
            }
            Some(line)
        };
        for e in (0..n).filter(|&k| !adj[k].is_empty() && adj[k].len() != 2) {
            for &j in &adj[e] {
                lines.extend(walk(e, j, &mut used));
            }
        }
        // loops with no end or junction on them
        for k in (0..n).filter(|&k| adj[k].len() == 2) {
            lines.extend(walk(k, adj[k][0], &mut used));
        }
        lines
    };
    // spurs: an end's line to a junction shorter than SPUR times the junction's half-width
    for line in trace(&adj) {
        let (first, last) = (line[0], line[line.len() - 1]);
        let (end, junction) = match (adj[first].len(), adj[last].len()) {
            (1, d) if d >= 3 => (true, last),
            (d, 1) if d >= 3 => (false, first),
            _ => continue,
        };
        let length: f64 = line.windows(2).map(|w| far(at(w[0]), at(w[1]))).sum();
        if length < SPUR * edge[junction] {
            let spur: Vec<usize> = if end { line[..line.len() - 1].to_vec() } else { line[1..].to_vec() };
            for &k in &spur {
                for j in std::mem::take(&mut adj[k]) {
                    adj[j].retain(|&i| i != k);
                }
            }
        }
    }
    let junction = |k: usize| adj[k].len() >= 3;
    // a link within a junction's cells goes: the veins leaving it cover it
    let lines: Vec<Vec<usize>> = trace(&adj).into_iter().filter(|line| {
        let (first, last) = (line[0], line[line.len() - 1]);
        let length: f64 = line.windows(2).map(|w| far(at(w[0]), at(w[1]))).sum();
        !(junction(first) && junction(last) && length < LINK * level.pitch)
    }).collect();
    // At a junction the way to the edge is the widest circle the veins meeting there hold, not a
    // vein's half-width, and out of that circle the vein still flares into it. Each vein takes,
    // from the junction out, the narrowest half-width it has from the circle out to FLARE times
    // it (short of its middle): where it is its own width. A junction's dome is the widest of
    // those that meet there; a line with none of it out of its junctions' circles is as wide as
    // the narrower dome of the two (else as its own narrowest).
    let flare = |line: &[usize], from_last: bool| -> Option<(usize, f64)> {
        let order: Vec<usize> = if from_last { (0..line.len()).rev().collect() } else { (0..line.len()).collect() };
        let length: f64 = line.windows(2).map(|w| far(at(w[0]), at(w[1]))).sum();
        let circle = edge[line[order[0]]];
        let (mut gone, mut narrowest) = (0.0, None::<(usize, f64)>);
        for w in order.windows(2) {
            gone += far(at(line[w[0]]), at(line[w[1]]));
            if gone > (FLARE * circle).min(0.5 * length) {
                break;
            }
            if gone > circle && narrowest.is_none_or(|(_, r)| edge[line[w[1]]] < r) {
                narrowest = Some((w[1], edge[line[w[1]]]));
            }
        }
        narrowest
    };
    let ends: Vec<[Option<(usize, f64)>; 2]> = lines.iter().map(|line| [junction(line[0]).then(|| flare(line, false)).flatten(), junction(line[line.len() - 1]).then(|| flare(line, true)).flatten()]).collect();
    let mut domes: std::collections::HashMap<usize, f64> = std::collections::HashMap::new();
    for (line, end) in lines.iter().zip(&ends) {
        for (k, e) in [(line[0], end[0]), (line[line.len() - 1], end[1])] {
            if let Some((_, r)) = e {
                let d = domes.entry(k).or_insert(r);
                *d = d.max(r);
            }
        }
    }
    let mut out_pieces = Vec::new();
    for (line, end) in lines.iter().zip(&ends) {
        let (first, last) = (line[0], line[line.len() - 1]);
        let mut ps: Vec<[f64; 3]> = line.iter().map(|&k| at(k)).collect();
        let mut rs: Vec<f64> = line.iter().map(|&k| edge[k]).collect();
        let ns: Vec<[f64; 3]> = line.iter().map(|&k| out(k)).collect();
        // a junction's end with none of it out of the circle short of the middle: as wide as the
        // junction's dome or the vein's other end, the narrower (else its own narrowest)
        let own = rs.iter().copied().fold(f64::INFINITY, f64::min);
        let within = |k: usize, other: Option<(usize, f64)>| {
            let r = [domes.get(&k).copied(), other.map(|o| o.1)].into_iter().flatten().fold(f64::INFINITY, f64::min);
            if r.is_finite() { r } else { own }
        };
        let (w0, w1) = ((junction(first) && end[0].is_none()).then(|| within(first, end[1])), (junction(last) && end[1].is_none()).then(|| within(last, end[0])));
        if let Some((i, r)) = end[0] {
            rs[..i].fill(r);
        }
        if let Some((i, r)) = end[1] {
            rs[i + 1..].fill(r);
        }
        let n = rs.len();
        match (w0, w1) {
            (Some(a), Some(b)) => rs.fill(a.min(b)),
            (Some(a), None) => rs[..n.div_ceil(2)].fill(a),
            (None, Some(b)) => rs[n / 2..].fill(b),
            (None, None) => {}
        }
        for pass in 0..SMOOTH_WIDTH {
            let (p0, r0) = (ps.clone(), rs.clone());
            for i in 1..ps.len().saturating_sub(1) {
                if pass < SMOOTH {
                    let mid: [f64; 3] = std::array::from_fn(|c| 0.5 * (p0[i - 1][c] + p0[i + 1][c]));
                    let moved: [f64; 3] = std::array::from_fn(|c| 0.5 * (p0[i][c] + mid[c]));
                    // kept on the surface: what it moved along the way out goes
                    let off = dot(sub(moved, at(line[i])), ns[i]);
                    ps[i] = std::array::from_fn(|c| moved[c] - off * ns[i][c]);
                }
                rs[i] = 0.5 * r0[i] + 0.25 * (r0[i - 1] + r0[i + 1]);
            }
        }
        let keep = simplify(&ps, &rs);
        for w in keep.windows(2) {
            let (i, j) = (w[0], w[1]);
            out_pieces.push(Piece { a: ps[i], ra: rs[i], na: ns[i], b: ps[j], rb: rs[j], nb: ns[j] });
        }
    }
    out_pieces
}

/// The points of a line (positions and half-widths) that straight pieces between keep within
/// [`TOLERANCE`] of the rest (by halving: the farthest point splits a piece that strays).
fn simplify(ps: &[[f64; 3]], rs: &[f64]) -> Vec<usize> {
    let mut keep = vec![false; ps.len()];
    keep[0] = true;
    keep[ps.len() - 1] = true;
    let mut stack = vec![(0, ps.len() - 1)];
    while let Some((i, j)) = stack.pop() {
        let (mut worst, mut at) = (0.0, i);
        for k in i + 1..j {
            let (d, t) = to_segment(ps[k], ps[i], ps[j]);
            let r = rs[i] + t * (rs[j] - rs[i]);
            let e = d.max((rs[k] - r).abs());
            if e > worst {
                (worst, at) = (e, k);
            }
        }
        if worst > TOLERANCE {
            keep[at] = true;
            stack.push((i, at));
            stack.push((at, j));
        }
    }
    (0..ps.len()).filter(|&k| keep[k]).collect()
}

/// How far `p` is from the segment `a..b`, and where along it (0..1) its nearest point is.
fn to_segment(p: [f64; 3], a: [f64; 3], b: [f64; 3]) -> (f64, f64) {
    let ab = sub(b, a);
    let l = dot(ab, ab);
    let t = if l > 0.0 { (dot(sub(p, a), ab) / l).clamp(0.0, 1.0) } else { 0.0 };
    (far(p, std::array::from_fn(|c| a[c] + t * ab[c])), t)
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

fn unit(a: [f64; 3]) -> [f64; 3] {
    let l = dot(a, a).sqrt();
    if l > 0.0 { a.map(|v| v / l) } else { a }
}

fn far(a: [f64; 3], b: [f64; 3]) -> f64 {
    let d = sub(a, b);
    dot(d, d).sqrt()
}

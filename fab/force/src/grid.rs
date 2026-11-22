//! The analysis grid: cubic cells of side `h` over the part's box (printer frame, mm), each
//! with its eight Gauss points' (octants') share of material and which printed material it
//! is. Everything comes from one lattice of distance samples at the octant centres (spacing
//! `h/2`): the filled share of an octant from the distance at its centre and the field's
//! gradient, the materials from the depth under the surface measured across the layers (the
//! walls) and along them (the skins).

use crate::element::V3;
use crate::material::{angle_bin, Materials};

/// One Gauss point (its cell's octant): its filled share and the shares of walls and skin
/// in its material (the rest is infill), 0..255, the walls' bead direction bin, and which
/// infill it holds (its place in [`Materials::fills`]: 0 the part's own, else a zone's).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Gp {
    pub fill: u8,
    pub wall: u8,
    pub skin: u8,
    pub angle: u8,
    pub infill: u16,
}

/// The weight an empty octant of a cell that has material keeps (relative): the finite cell
/// method's fictitious stiffness, so a cell's stiffness never loses rank.
pub const FICTITIOUS: f64 = 1e-3;

impl Gp {
    pub fn weight(&self) -> f64 {
        (self.fill as f64 / 255.0).max(FICTITIOUS)
    }
    pub fn shares(&self) -> (f64, f64) {
        (self.wall as f64 / 255.0, self.skin as f64 / 255.0)
    }
}

/// Distance samples at the octant centres: `n` per axis (twice the cells), the first at
/// `origin`, `spacing` apart. Negative inside.
#[derive(Clone, Debug, Default)]
pub struct Lattice {
    pub n: [usize; 3],
    pub origin: V3,
    pub spacing: f64,
    pub d: Vec<f32>,
    /// How deep inside the part its samples are known to read the part's depth (mm): a
    /// field's [`Lattice::regularise`]'s `deep`, none for a field only sampled, every depth for
    /// samples filled in otherwise (a mesh's, exact to its triangles).
    pub band: f64,
}

impl Lattice {
    pub fn index(&self, i: usize, j: usize, k: usize) -> usize {
        i + self.n[0] * (j + self.n[1] * k)
    }
    pub fn at(&self, i: usize, j: usize, k: usize) -> f64 {
        self.d[self.index(i, j, k)] as f64
    }
    pub fn pos(&self, i: usize, j: usize, k: usize) -> V3 {
        [self.origin[0] + i as f64 * self.spacing, self.origin[1] + j as f64 * self.spacing, self.origin[2] + k as f64 * self.spacing]
    }
    /// The field's gradient at a sample (central differences, one-sided at the border), unnormalised.
    pub fn gradient(&self, i: usize, j: usize, k: usize) -> V3 {
        let p = [i, j, k];
        std::array::from_fn(|a| {
            let (lo, hi) = (p[a].saturating_sub(1), (p[a] + 1).min(self.n[a] - 1));
            if lo == hi {
                return 0.0;
            }
            let mut q = p;
            q[a] = hi;
            let f1 = self.at(q[0], q[1], q[2]);
            q[a] = lo;
            let f0 = self.at(q[0], q[1], q[2]);
            (f1 - f0) / ((hi - lo) as f64 * self.spacing)
        })
    }

    /// The field's steps to a sample's neighbours along each axis (mm): from the lower one
    /// (`d - d(-)`) and to the higher (`d(+) - d`). At the border the missing step is the other
    /// one, the field taken as linear across it.
    fn steps(&self, i: usize, j: usize, k: usize) -> [(f64, f64); 3] {
        let p = [i, j, k];
        let d = self.at(i, j, k);
        std::array::from_fn(|a| {
            let at = |q: [usize; 3]| self.at(q[0], q[1], q[2]);
            let lo = (p[a] > 0).then(|| {
                let mut q = p;
                q[a] -= 1;
                d - at(q)
            });
            let hi = (p[a] + 1 < self.n[a]).then(|| {
                let mut q = p;
                q[a] += 1;
                at(q) - d
            });
            match (lo, hi) {
                (Some(l), Some(h)) => (l, h),
                (Some(s), None) | (None, Some(s)) => (s, s),
                (None, None) => (0.0, 0.0),
            }
        })
    }

    /// The filled share of the octant a sample stands for (side `spacing`), from its distance
    /// `d`, the field's unit gradient `n` there and the sampled gradient's length `slope`, and its
    /// six neighbours (FORCE A5 2):
    /// - A curved surface lies inside its tangent plane where it is convex (outside where it is
    ///   concave), by its curvature over the octant: the plane is moved by `a^2 / 24` times the
    ///   field's Laplacian (the six neighbours' second differences; a distance field's normal
    ///   curvature is nil, so this is the surface's mean curvature) and cut exactly ([`cut`]).
    ///   By the field's unit slope the move is at most `a / 4`.
    /// - Where the sampled gradient is short the samples straddle a crease (inside a convex edge,
    ///   outside a concave one) and its direction is a blend of two faces': there the nearest
    ///   face decides, square to an axis.
    /// - Beside an edge the centred normal is off by up to a fifth (one neighbour feels the other
    ///   face), so a sliver can read under [`SNAP`] full where the face alone leaves nothing (or
    ///   that much empty where it fills it). Such a share stands only if a plane with the upwind
    ///   normal (per axis the one-sided step toward the surface) reaches the octant too.
    pub fn share(&self, i: usize, j: usize, k: usize, n: V3, slope: f64) -> f64 {
        let a = self.spacing;
        let d = self.at(i, j, k);
        let steps = self.steps(i, j, k);
        let share = if slope < CREASE {
            (0.5 - d / a).clamp(0.0, 1.0)
        } else {
            let lap: f64 = steps.iter().map(|(lo, hi)| hi - lo).sum();
            cut(d + lap / 24.0, n, a).0
        };
        // the upwind normal's 1-norm, `d` read as `sign d` (toward smaller `sign d`); none where
        // no neighbour falls that way
        let reach = |sign: f64| {
            let g = steps.map(|(lo, hi)| {
                let (lo, hi) = (sign * lo, sign * hi);
                let (down, up) = (lo.max(0.0), hi.min(0.0));
                if down >= -up { down } else { up }
            });
            let len = (g[0] * g[0] + g[1] * g[1] + g[2] * g[2]).sqrt();
            (len > 0.0).then(|| 0.5 * a * (g[0].abs() + g[1].abs() + g[2].abs()) / len)
        };
        if share > 0.0 && share < SNAP && reach(1.0).is_some_and(|r| d >= r) {
            return 0.0;
        }
        if share < 1.0 && share > 1.0 - SNAP && reach(-1.0).is_some_and(|r| -d >= r) {
            return 1.0;
        }
        share
    }

    /// A sample's 26 neighbours on the lattice.
    fn neighbours(&self, i: usize, j: usize, k: usize) -> impl Iterator<Item = [usize; 3]> + '_ {
        let p = [i, j, k];
        (0..27).filter(|&o| o != 13).filter_map(move |o| {
            let q: [i64; 3] = std::array::from_fn(|a| p[a] as i64 + [o % 3, o / 3 % 3, o / 9][a] as i64 - 1);
            (0..3).all(|a| q[a] >= 0 && (q[a] as usize) < self.n[a]).then(|| q.map(|v| v as usize))
        })
    }

    fn ijk(&self, s: usize) -> [usize; 3] {
        [s % self.n[0], s / self.n[0] % self.n[1], s / (self.n[0] * self.n[1])]
    }

    /// Writes the samples from `distance` (printer-frame points in, distances out, batched),
    /// one z slab at a time.
    pub fn sample(&mut self, distance: &mut dyn FnMut(&[V3], &mut [f64])) {
        self.band = 0.0;
        let slab = self.n[0] * self.n[1];
        let mut pts = Vec::with_capacity(slab);
        let mut out = vec![0.0; slab];
        for k in 0..self.n[2] {
            pts.clear();
            for j in 0..self.n[1] {
                for i in 0..self.n[0] {
                    pts.push(self.pos(i, j, k));
                }
            }
            distance(&pts, &mut out);
            for (s, d) in out.iter().enumerate() {
                self.d[k * slab + s] = *d as f32;
            }
        }
    }

    /// Makes every inside sample within `deep` of the surface read the part's depth where the
    /// field's value is not it (FORCE A15), from the field it was sampled from. Where two
    /// bodies share or bury a face (a radius-0 union, a small overlap) the field reads the
    /// nearer body's face, which inside the part is no surface: the fill, the walls' and skins'
    /// depth, the gradient and the contour would all see one there. Outside the part a `min`
    /// of exact distances is exact, so the true surface is known from outside:
    ///
    /// 1. candidates: inside samples within `deep` that are a ridge along some axis (shallower
    ///    than both neighbours on it, both inside; a tie counts on the upper side only), and
    ///    samples reading zero (to [`ZERO`] of a spacing). A convex body has none; holes give
    ///    a few, which fail the test;
    /// 2. the test ([`Lattice::governs`]): the field's normal at the sample, its foot that far
    ///    along it and one probe [`PROBE`] of a spacing past it. On the part's surface the
    ///    probe reads outside; on a face inside the part it does not, and the sample is
    ///    governed by that face. A sample whose stencil straddles a crease of the field cannot
    ///    be read: unsure;
    /// 3. the flood: from the governed candidates through their 26 neighbours inside within
    ///    `deep` (or zero), each tested the same way. An unsure sample it reaches is governed
    ///    but floods no further, and an unsure candidate seeds nothing, so a crease of the part
    ///    itself never carries it;
    /// 4. each governed sample's distance becomes its depth under the surface the outside
    ///    samples near it show ([`Lattice::depths`]).
    ///
    /// A part where no sample is governed keeps its samples bit for bit. [`Lattice::band`]
    /// becomes `deep`.
    pub fn regularise(&mut self, deep: f64, distance: &mut dyn FnMut(&[V3], &mut [f64])) -> Regularised {
        let zero = ZERO * self.spacing;
        self.band = deep;
        let candidates = self.candidates(deep, zero);
        let mut out = Regularised { candidates: candidates.len(), ..Default::default() };
        if candidates.is_empty() {
            return out;
        }
        // 0 untested, 1 tested, 2 governed, 3 a candidate the test could not read
        let mut state = vec![0u8; self.d.len()];
        for &s in &candidates {
            state[s] = 1;
        }
        let mut wave = candidates;
        let mut governed = Vec::new();
        let mut seeding = true;
        while !wave.is_empty() {
            out.tested += wave.len();
            let held = self.governs(&wave, zero, distance);
            let mut next = Vec::new();
            // a sample the test cannot read (its stencil straddles a crease of the field) seeds
            // nothing; reached from a governed neighbour it is governed, but floods on no
            // further, so a crease of the part itself never carries the flood along
            if seeding {
                for (&s, held) in wave.iter().zip(&held) {
                    if *held == Test::Unsure {
                        state[s] = 3;
                    }
                }
            }
            for (&s, held) in wave.iter().zip(held) {
                match held {
                    Test::Surface => continue,
                    Test::Unsure if seeding => continue,
                    Test::Unsure => {
                        state[s] = 2;
                        governed.push(s);
                        out.unsure += 1;
                        continue;
                    }
                    Test::Governed => {}
                }
                state[s] = 2;
                governed.push(s);
                let [i, j, k] = self.ijk(s);
                for [x, y, z] in self.neighbours(i, j, k) {
                    let t = self.index(x, y, z);
                    if state[t] == 0 && self.within(t, deep, zero) {
                        state[t] = 1;
                        next.push(t);
                    } else if state[t] == 3 {
                        // a candidate the test could not read, now reached: governed, a leaf
                        state[t] = 2;
                        governed.push(t);
                        out.unsure += 1;
                    }
                }
            }
            wave = next;
            seeding = false;
        }
        if governed.is_empty() {
            return out;
        }
        let (depths, points) = self.depths(&governed, deep, distance);
        out.points = points;
        for (&s, depth) in governed.iter().zip(depths) {
            if depth < -(self.d[s] as f64) - 1e-6 {
                out.shallower += 1;
            }
            self.d[s] = -depth as f32;
        }
        out.governed = governed;
        out
    }

    /// Inside within `deep` of zero, or reading zero (to `zero`).
    fn within(&self, s: usize, deep: f64, zero: f64) -> bool {
        let d = self.d[s] as f64;
        (d < 0.0 && -d < deep) || (0.0..=zero).contains(&d)
    }

    /// [`Lattice::regularise`]'s first step: one pass over the samples in order, each read
    /// against its two neighbours along each axis only when it lies inside within `deep`.
    fn candidates(&self, deep: f64, zero: f64) -> Vec<usize> {
        let [nx, ny, nz] = self.n;
        let d = &self.d;
        let stride = [1, nx, nx * ny];
        let mut out = Vec::new();
        for k in 0..nz {
            for j in 0..ny {
                let row = (k * ny + j) * nx;
                for i in 0..nx {
                    let s = row + i;
                    let v = f64::from(d[s]);
                    if v > zero || v <= -deep {
                        continue;
                    }
                    if v >= 0.0 {
                        out.push(s);
                        continue;
                    }
                    // a ridge along some axis: shallower than both neighbours, both inside
                    let ridge = [i, j, k].iter().zip(self.n).zip(stride).any(|((&p, n), step)| {
                        p > 0 && p + 1 < n && {
                            let (lo, hi) = (f64::from(d[s - step]), f64::from(d[s + step]));
                            lo < 0.0 && hi < 0.0 && v > lo && v >= hi
                        }
                    });
                    if ridge {
                        out.push(s);
                    }
                }
            }
        }
        out
    }

    /// [`Lattice::regularise`]'s test at each of `samples`: whether a face inside the part
    /// governs it. A sample within [`PROBE`] / sqrt 3 of zero (or reading zero) probes the
    /// field that far along each axis both ways, which crosses any face that near: governed
    /// when none reads outside. Any other takes the field's gradient on a stencil a quarter of
    /// that across (so no seam that near straddles it); unsure when its length is not one
    /// (to [`UNIT`]: the stencil straddles a crease, where its direction points at no face),
    /// else governed when the probe past its foot along it does not read outside.
    fn governs(&self, samples: &[usize], zero: f64, distance: &mut dyn FnMut(&[V3], &mut [f64])) -> Vec<Test> {
        const K: [V3; 4] = [[1.0, -1.0, -1.0], [-1.0, -1.0, 1.0], [-1.0, 1.0, -1.0], [1.0, 1.0, 1.0]];
        let eps = PROBE * self.spacing;
        let near = eps / 3f64.sqrt();
        let step = 0.25 * eps;
        let mut out = Vec::with_capacity(samples.len());
        for chunk in samples.chunks(1 << 15) {
            let at: Vec<V3> = chunk.iter().map(|&s| {
                let [i, j, k] = self.ijk(s);
                self.pos(i, j, k)
            }).collect();
            let stencil: Vec<V3> = at.iter().flat_map(|p| K.map(|k| std::array::from_fn(|a| p[a] + step * k[a]))).collect();
            let mut read = vec![0.0; stencil.len()];
            field_at(&mut *distance, &stencil, &mut read);
            // per sample its probes' first place in `probe`, or unsure
            let mut probe = Vec::with_capacity(at.len() * 2);
            let mut first = Vec::with_capacity(at.len() + 1);
            let mut unsure = vec![false; at.len()];
            let mut foot: Vec<Option<V3>> = vec![None; at.len()];
            for (x, (&s, p)) in chunk.iter().zip(&at).enumerate() {
                first.push(probe.len());
                let d = self.d[s] as f64;
                if d >= -near && d <= zero {
                    for a in 0..6 {
                        let mut q = *p;
                        q[a / 2] += if a % 2 == 0 { eps } else { -eps };
                        probe.push(q);
                    }
                    continue;
                }
                let g: V3 = std::array::from_fn(|a| (0..4).map(|t| K[t][a] * read[4 * x + t]).sum::<f64>() / (4.0 * step));
                let len = (g[0] * g[0] + g[1] * g[1] + g[2] * g[2]).sqrt();
                if (len - 1.0).abs() > UNIT {
                    unsure[x] = true;
                    continue;
                }
                foot[x] = Some(std::array::from_fn(|a| p[a] - d * g[a] / len));
                probe.push(std::array::from_fn(|a| p[a] + (-d + eps) * g[a] / len));
            }
            first.push(probe.len());
            let mut read = vec![0.0; probe.len()];
            field_at(&mut *distance, &probe, &mut read);
            let tests: Vec<Test> = first.windows(2).enumerate().map(|(x, w)| {
                if unsure[x] {
                    Test::Unsure
                } else if read[w[0]..w[1]].iter().all(|r| *r <= 0.0) {
                    Test::Governed
                } else {
                    Test::Surface
                }
            }).collect();
            // a governed foot is a zero inside the part (a shared face) or lies inside another
            // body, whose field is then exact there (a buried face); a foot where the field
            // reads neither (a smooth blend, whose slope is not one) is not this fault
            let feet: Vec<(usize, V3)> = tests.iter().enumerate().filter(|(_, t)| **t == Test::Governed).filter_map(|(x, _)| foot[x].map(|q| (x, q))).collect();
            let mut at_feet = Vec::with_capacity(feet.len() * 5);
            for (_, q) in &feet {
                at_feet.push(*q);
                at_feet.extend(K.map(|k| std::array::from_fn(|a| q[a] + step * k[a])));
            }
            let mut read = vec![0.0; at_feet.len()];
            field_at(&mut *distance, &at_feet, &mut read);
            let mut tests = tests;
            for (y, (x, _)) in feet.iter().enumerate() {
                let v = read[5 * y];
                if v.abs() <= eps {
                    continue;
                }
                let g: V3 = std::array::from_fn(|a| (0..4).map(|t| K[t][a] * read[5 * y + 1 + t]).sum::<f64>() / (4.0 * step));
                let len = (g[0] * g[0] + g[1] * g[1] + g[2] * g[2]).sqrt();
                if (len - 1.0).abs() > UNIT {
                    tests[*x] = Test::Unsure;
                }
            }
            out.extend(tests);
        }
        out
    }

    /// The depth (mm, at most `deep`) of each of `samples` under the part's surface, and how
    /// many surface points that took: each outside sample within 1.3 spacings of zero near
    /// them, taken onto the field's zero set with its normal (two steps along it, as
    /// [`crate::thin::Thickness::of_field`] does). Each depth is a distance to the surface
    /// through a point known to lie on or outside it, so none is shallower than the part's:
    /// the nearest point's; less through one of the nearest points' planes where the sample's
    /// foot on it lies within a spacing of the point and the field shows the foot outside (its
    /// value added: outside a `min` of exact distances it is exact) or on the surface with the
    /// probe past it outside (a seam's zero is no surface); and, unless the nearest point's
    /// own flat face gave the depth, [`MIRRORS`] closest-point rounds from outside (the sample
    /// mirrored through its nearest point along that point's normal, taken onto the surface),
    /// which find a curved face's nearest point. `deep` where no point is that near.
    fn depths(&self, samples: &[usize], deep: f64, distance: &mut dyn FnMut(&[V3], &mut [f64])) -> (Vec<f64>, usize) {
        let (at, normal) = self.surface_points(samples, deep, distance);
        let a = self.spacing;
        let side = 4.0 * a;
        let dims: [usize; 3] = std::array::from_fn(|x| ((self.n[x] as f64 * a) / side).ceil() as usize + 1);
        let bin_of = |p: V3| -> [usize; 3] { std::array::from_fn(|x| (((p[x] - self.origin[x]) / side).floor().max(0.0) as usize).min(dims[x] - 1)) };
        let flat = |b: [usize; 3]| b[0] + dims[0] * (b[1] + dims[1] * b[2]);
        // the points binned (counting sort)
        let mut start = vec![0u32; dims[0] * dims[1] * dims[2] + 1];
        for p in &at {
            start[flat(bin_of(*p)) + 1] += 1;
        }
        for b in 1..start.len() {
            start[b] += start[b - 1];
        }
        let mut fill = start.clone();
        let mut items = vec![0u32; at.len()];
        for (i, p) in at.iter().enumerate() {
            let b = flat(bin_of(*p));
            items[fill[b] as usize] = i as u32;
            fill[b] += 1;
        }
        // per sample: the nearest point's distance, the point and its normal, and its best
        // planes' (depth, foot, normal)
        const PLANES: usize = 3;
        let mut planes: Vec<[(f64, V3, V3); PLANES]> = Vec::with_capacity(samples.len());
        let mut closest: Vec<Option<(V3, V3)>> = Vec::with_capacity(samples.len());
        let mut depths: Vec<f64> = samples.iter().map(|&s| {
            let [i, j, k] = self.ijk(s);
            let p = self.pos(i, j, k);
            let b = bin_of(p);
            let mut nearest = deep;
            let mut point = None;
            let mut best = [(f64::INFINITY, [0.0; 3], [0.0; 3]); PLANES];
            // rings of bins outward, while a bin can hold a point nearer than the nearest
            for ring in 0..=dims.iter().copied().max().unwrap_or(0) {
                if (ring as f64 - 1.0) * side > nearest + a {
                    break;
                }
                let r = ring as i64;
                for z in -r..=r {
                    for y in -r..=r {
                        for x in -r..=r {
                            if x.abs().max(y.abs()).max(z.abs()) != r {
                                continue;
                            }
                            let c = [b[0] as i64 + x, b[1] as i64 + y, b[2] as i64 + z];
                            if (0..3).any(|t| c[t] < 0 || c[t] >= dims[t] as i64) {
                                continue;
                            }
                            // the distance from `p` to the bin's box
                            let gap = (0..3).map(|t| {
                                let lo = self.origin[t] + c[t] as f64 * side;
                                (lo - p[t]).max(p[t] - lo - side).max(0.0).powi(2)
                            }).sum::<f64>().sqrt();
                            if gap > nearest + a {
                                continue;
                            }
                            let bin = c[0] as usize + dims[0] * (c[1] as usize + dims[1] * c[2] as usize);
                            for &q in &items[start[bin] as usize..start[bin + 1] as usize] {
                                let (q, m) = (at[q as usize], normal[q as usize]);
                                let r: V3 = std::array::from_fn(|t| p[t] - q[t]);
                                let t = r[0] * m[0] + r[1] * m[1] + r[2] * m[2];
                                let along = r[0] * r[0] + r[1] * r[1] + r[2] * r[2];
                                if along.sqrt() < nearest {
                                    nearest = along.sqrt();
                                    point = Some((q, m));
                                }
                                let off = (along - t * t).max(0.0).sqrt();
                                if off <= a && t.abs() < best[PLANES - 1].0 {
                                    let foot: V3 = std::array::from_fn(|x| p[x] - t * m[x]);
                                    best[PLANES - 1] = (t.abs(), foot, m);
                                    best.sort_by(|x, y| x.0.total_cmp(&y.0));
                                }
                            }
                        }
                    }
                }
            }
            planes.push(best);
            closest.push(point);
            nearest
        }).collect();
        // the planes' feet and the probes past them, read
        let eps = PROBE * a;
        let zero = ZERO * a;
        let mut read: Vec<V3> = Vec::with_capacity(samples.len() * PLANES * 2);
        for best in &planes {
            for (_, foot, m) in best {
                read.push(*foot);
                read.push(std::array::from_fn(|x| foot[x] + eps * m[x]));
            }
        }
        let mut value = vec![0.0; read.len()];
        field_at(&mut *distance, &read, &mut value);
        // the depths a plane gave with its foot on the surface, through the face the nearest
        // point lies on (its normal the plane's): no point of a flat face is nearer
        let mut exact = vec![false; samples.len()];
        for (x, (depth, best)) in depths.iter_mut().zip(&planes).enumerate() {
            let mut on = (f64::INFINITY, [0.0; 3]);
            for (y, (t, _, m)) in best.iter().enumerate() {
                let (v, w) = (value[2 * (PLANES * x + y)], value[2 * (PLANES * x + y) + 1]);
                if t.is_finite() && (v > zero || (v >= -zero && w > 0.0)) {
                    *depth = depth.min(t + v.max(0.0));
                    if v.abs() <= zero && *t < on.0 {
                        on = (*t, *m);
                    }
                }
            }
            exact[x] = on.0 <= *depth && closest[x].is_some_and(|(_, n)| n[0] * on.1[0] + n[1] * on.1[1] + n[2] * on.1[2] > 1.0 - 1e-6);
        }
        // the rest (a curved or creased surface near them): the point nearest the sample on the
        // surface, found from outside, where the field is exact: the sample mirrored through its
        // nearest point along that point's normal, taken onto the surface, each round's point a
        // candidate
        let (todo, mut on): (Vec<(usize, V3)>, Vec<(V3, V3)>) = (0..samples.len()).filter(|&x| !exact[x]).filter_map(|x| {
            let [i, j, k] = self.ijk(samples[x]);
            closest[x].map(|qm| ((x, self.pos(i, j, k)), qm))
        }).unzip();
        let mut alive = vec![true; todo.len()];
        let step = 0.25 * eps;
        for _ in 0..MIRRORS {
            let ys: Vec<V3> = todo.iter().zip(&on).map(|((_, p), (q, m))| {
                let r = ((p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2) + (p[2] - q[2]).powi(2)).sqrt();
                std::array::from_fn(|x| p[x] + r * m[x])
            }).collect();
            let mut fy = vec![0.0; ys.len()];
            field_at(&mut *distance, &ys, &mut fy);
            let mut ny = vec![[0.0; 3]; ys.len()];
            normals(&mut *distance, step, &ys, &mut ny);
            let xs: Vec<V3> = ys.iter().zip(&fy).zip(&ny).map(|((y, f), n)| std::array::from_fn(|x| y[x] - f * n[x])).collect();
            let mut nx = vec![[0.0; 3]; xs.len()];
            normals(&mut *distance, step, &xs, &mut nx);
            let mut fx = vec![0.0; xs.len()];
            field_at(&mut *distance, &xs, &mut fx);
            for (z, ((o, x), n)) in on.iter_mut().zip(xs).zip(nx).enumerate() {
                alive[z] &= fy[z] > 0.0;
                // each round's point a distance to the surface (it lies on it, or this near)
                if alive[z] && fx[z] >= -zero {
                    let (s, p) = todo[z];
                    let r = ((p[0] - x[0]).powi(2) + (p[1] - x[1]).powi(2) + (p[2] - x[2]).powi(2)).sqrt();
                    depths[s] = depths[s].min(r + fx[z].max(0.0));
                }
                *o = (x, n);
            }
        }
        (depths, at.len())
    }

    /// [`Lattice::depths`]'s surface points near `samples` and their normals.
    fn surface_points(&self, samples: &[usize], deep: f64, distance: &mut dyn FnMut(&[V3], &mut [f64])) -> (Vec<V3>, Vec<V3>) {
        let a = self.spacing;
        let eps = PROBE * a;
        // bins of four spacings over the lattice, those within `deep` of a sample marked
        let side = 4.0 * a;
        let dims: [usize; 3] = std::array::from_fn(|x| ((self.n[x] as f64 * a) / side).ceil() as usize + 1);
        let bin_of = |p: V3| -> [usize; 3] { std::array::from_fn(|x| (((p[x] - self.origin[x]) / side).floor().max(0.0) as usize).min(dims[x] - 1)) };
        let flat = |b: [usize; 3]| b[0] + dims[0] * (b[1] + dims[1] * b[2]);
        let reach = (deep / side).ceil() as usize + 1;
        let mut near = vec![false; dims[0] * dims[1] * dims[2]];
        let mut own = vec![false; self.d.len()];
        for &s in samples {
            own[s] = true;
            let [i, j, k] = self.ijk(s);
            near[flat(bin_of(self.pos(i, j, k)))] = true;
        }
        // grown by `reach` bins, one axis at a time
        for x in 0..3 {
            let was = near.clone();
            for (f, on) in near.iter_mut().enumerate() {
                let mut b = [f % dims[0], f / dims[0] % dims[1], f / (dims[0] * dims[1])];
                let c = b[x];
                *on = (c.saturating_sub(reach)..(c + reach + 1).min(dims[x])).any(|v| {
                    b[x] = v;
                    was[flat(b)]
                });
            }
        }
        let mut at: Vec<V3> = (0..self.d.len()).filter(|&s| {
            let d = self.d[s] as f64;
            !own[s] && d >= 0.0 && d < 1.3 * a && {
                let [i, j, k] = self.ijk(s);
                near[flat(bin_of(self.pos(i, j, k)))]
            }
        }).map(|s| {
            let [i, j, k] = self.ijk(s);
            self.pos(i, j, k)
        }).collect();
        let mut normal = vec![[0.0; 3]; at.len()];
        let mut d = vec![0.0; at.len()];
        for _ in 0..2 {
            normals(&mut *distance, eps, &at, &mut normal);
            field_at(&mut *distance, &at, &mut d);
            for ((p, n), d) in at.iter_mut().zip(&normal).zip(&d) {
                *p = std::array::from_fn(|x| p[x] - n[x] * d);
            }
        }
        normals(&mut *distance, eps, &at, &mut normal);
        (at, normal)
    }
}

/// `distance` at `at` into `out`, not called for no points (an evaluator may refuse an
/// empty batch).
fn field_at(distance: &mut dyn FnMut(&[V3], &mut [f64]), at: &[V3], out: &mut [f64]) {
    if !at.is_empty() {
        distance(at, out);
    }
}

/// [`crate::surface::field_normals`] at `at`, `eps` across, into `out`; nothing for no points.
fn normals(distance: &mut dyn FnMut(&[V3], &mut [f64]), eps: f64, at: &[V3], out: &mut [V3]) {
    if !at.is_empty() {
        crate::surface::field_normals(distance, eps)(at, out);
    }
}

/// What [`Lattice::regularise`] did: its candidates, the samples it tested, those it found
/// governed (their distances rewritten) and how many of those were unsure, the surface points
/// it took their depths from, and how many it made shallower than they read (where the field
/// is a `min` of exact distances none should be: the part's depth is at least either body's).
#[derive(Clone, Debug, Default)]
pub struct Regularised {
    pub candidates: usize,
    pub tested: usize,
    pub governed: Vec<usize>,
    pub unsure: usize,
    pub points: usize,
    pub shallower: usize,
}

/// [`Lattice::regularise`]'s test's answers: the sample's foot is on the part's surface, or on a
/// face inside the part, or its stencil straddles a crease of the field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Test {
    Surface,
    Governed,
    Unsure,
}

/// [`Lattice::depths`]'s rounds toward the nearest surface point.
const MIRRORS: usize = 4;

/// How far from one the field's gradient may read before [`Lattice::regularise`]'s stencil is
/// taken to straddle a crease: a sharper crease would move the foot by more than its probe.
const UNIT: f64 = 1e-3;

/// The deepest any reader of the lattice looks under the surface (mm), for samples `a` apart:
/// the walls' band (each zone's too), the skins', a cut's reach, and two spacings more for the
/// gradient's stencil (FORCE A15).
pub fn deepest(m: &Materials, a: f64) -> f64 {
    let zones = m.zones.iter().filter_map(|z| z.wall_loops).map(|n| m.print.wall_thickness_of(n)).fold(0.0, f64::max);
    m.wall_thickness.max(m.top_thickness).max(m.bottom_thickness).max(zones).max(1.3 * a) + 2.0 * a
}

/// [`Lattice::regularise`]'s probe past a sample's foot, and its normals' stencil, in spacings.
const PROBE: f64 = 0.05;

/// A sample reading this close to zero (in spacings) may lie on a face two bodies share
/// ([`Lattice::regularise`]).
const ZERO: f64 = 1e-3;

#[derive(Clone, Debug, Default)]
pub struct Grid {
    /// Cells per axis.
    pub n: [usize; 3],
    /// Cell size per axis: cubic on the analysed grid, flat on coarse levels of a thin part
    /// (see [`Grid::coarser`]).
    pub h: V3,
    /// The corner of cell (0, 0, 0).
    pub origin: V3,
    pub gp: Vec<[Gp; 8]>,
    /// Each cell's eight Gauss points' offset (mm) from where the 2x2x2 rule puts them, the
    /// same for all eight: so their centroid, weighted as the stiffness takes them, is the
    /// cell's material's. Zero where every octant is full or empty; at most `0.212 h` per axis
    /// (`1/2 - GP/2`), so every point stays in its cell. Empty on a coarse level.
    pub offset: Vec<[f32; 3]>,
}

impl Grid {
    pub fn cells(&self) -> usize {
        self.n[0] * self.n[1] * self.n[2]
    }
    pub fn node_dims(&self) -> [usize; 3] {
        [self.n[0] + 1, self.n[1] + 1, self.n[2] + 1]
    }
    pub fn nodes(&self) -> usize {
        let d = self.node_dims();
        d[0] * d[1] * d[2]
    }
    pub fn cell(&self, i: usize, j: usize, k: usize) -> usize {
        i + self.n[0] * (j + self.n[1] * k)
    }
    pub fn cell_ijk(&self, c: usize) -> [usize; 3] {
        [c % self.n[0], (c / self.n[0]) % self.n[1], c / (self.n[0] * self.n[1])]
    }
    pub fn node(&self, i: usize, j: usize, k: usize) -> usize {
        let d = self.node_dims();
        i + d[0] * (j + d[1] * k)
    }
    pub fn node_ijk(&self, n: usize) -> [usize; 3] {
        let d = self.node_dims();
        [n % d[0], (n / d[0]) % d[1], n / (d[0] * d[1])]
    }
    pub fn node_pos(&self, n: usize) -> V3 {
        let p = self.node_ijk(n);
        std::array::from_fn(|a| self.origin[a] + p[a] as f64 * self.h[a])
    }
    pub fn cell_center(&self, c: usize) -> V3 {
        let p = self.cell_ijk(c);
        std::array::from_fn(|a| self.origin[a] + (p[a] as f64 + 0.5) * self.h[a])
    }
    /// The eight nodes of a cell, in corner order.
    pub fn cell_nodes(&self, c: usize) -> [usize; 8] {
        let [i, j, k] = self.cell_ijk(c);
        std::array::from_fn(|q| {
            let o = crate::element::corner(q);
            self.node(i + o[0], j + o[1], k + o[2])
        })
    }
    pub fn active(&self, c: usize) -> bool {
        self.gp[c].iter().any(|g| g.fill > 0)
    }
    /// Cell `c`'s point offset (mm): [`Grid::offset`], zero on a coarse level.
    pub fn offset_of(&self, c: usize) -> V3 {
        self.offset.get(c).map_or([0.0; 3], |o| o.map(f64::from))
    }
    /// Gauss point `q` of cell `c` in the cell's natural coordinates: the 2x2x2 rule's place
    /// moved by the cell's offset.
    pub fn point_xi(&self, c: usize, q: usize) -> V3 {
        let k = crate::element::corner(q);
        let o = self.offset_of(c);
        std::array::from_fn(|a| (2.0 * k[a] as f64 - 1.0) * crate::element::GP + 2.0 * o[a] / self.h[a])
    }
    /// The cell holding a point and the point's natural coordinates in it (clamped to the grid).
    pub fn locate(&self, p: V3) -> (usize, V3) {
        let mut ijk = [0; 3];
        let mut xi = [0.0; 3];
        for a in 0..3 {
            let t = (p[a] - self.origin[a]) / self.h[a];
            let i = (t.floor().max(0.0) as usize).min(self.n[a] - 1);
            ijk[a] = i;
            xi[a] = ((t - i as f64) * 2.0 - 1.0).clamp(-1.0, 1.0);
        }
        (self.cell(ijk[0], ijk[1], ijk[2]), xi)
    }

    /// The grid of a part: cells of side `h` over `lo..hi` (printer mm) and one more around
    /// it, sampled from `distance` (printer-frame points in, distances out, batched), its
    /// Gauss points' materials by `materials`. Returns the lattice it sampled too.
    pub fn from_field(lo: V3, hi: V3, h: f64, distance: &mut dyn FnMut(&[V3], &mut [f64]), materials: &Materials) -> (Grid, Lattice) {
        let (n, origin, mut lattice) = Grid::layout(lo, hi, h);
        lattice.sample(distance);
        lattice.regularise(deepest(materials, lattice.spacing), distance);
        let grid = Grid::from_lattice(&lattice, n, origin, h, materials, None);
        (grid, lattice)
    }

    /// The cells over `lo..hi` (printer mm) at `h` and one more around it: their counts, their
    /// corner, and their lattice laid out, every sample zero.
    pub fn layout(lo: V3, hi: V3, h: f64) -> ([usize; 3], V3, Lattice) {
        let n: [usize; 3] = std::array::from_fn(|a| (((hi[a] - lo[a]) / h).ceil().max(1.0) as usize) + 2);
        let origin: V3 = std::array::from_fn(|a| 0.5 * (lo[a] + hi[a]) - 0.5 * n[a] as f64 * h);
        let ln = [n[0] * 2, n[1] * 2, n[2] * 2];
        let lorigin: V3 = std::array::from_fn(|a| origin[a] + 0.25 * h);
        (n, origin, Lattice { n: ln, origin: lorigin, spacing: h * 0.5, d: vec![0.0; ln[0] * ln[1] * ln[2]], band: f64::INFINITY })
    }

    /// [`Grid::from_field`] with the lattice's samples written by `fill` (its distances, laid
    /// out and zero when it is called): a field's, or a mesh's ([`crate::mesh::Mesh::sample`]).
    pub fn from_samples(lo: V3, hi: V3, h: f64, fill: &mut dyn FnMut(&mut Lattice), materials: &Materials) -> (Grid, Lattice) {
        let (n, origin, mut lattice) = Grid::layout(lo, hi, h);
        fill(&mut lattice);
        let grid = Grid::from_lattice(&lattice, n, origin, h, materials, None);
        (grid, lattice)
    }

    /// The cells over a lattice sampled at their octant centres; their walls and skins as
    /// `printed` (the slice's) or else estimated from the depth under the surface, and their
    /// infill and wall count the part's zones' where they hold ([`Materials::at`]).
    pub fn from_lattice(lattice: &Lattice, n: [usize; 3], origin: V3, h: f64, materials: &Materials, printed: Option<&crate::sliced::Printed>) -> Grid {
        let mut grid = Grid { n, h: [h; 3], origin, gp: vec![[Gp::default(); 8]; n[0] * n[1] * n[2]], offset: Vec::new() };
        let a = h * 0.5;
        // each cell's cut octants' material centroids (from their centres, at their shares as
        // stored), weighted as the stiffness weighs them: summed here, finished below
        let mut offset = vec![[0.0f32; 3]; grid.cells()];
        // as solid plastic every infill is the same
        let zones = materials.as_printed && !materials.zones.is_empty();
        for k in 0..lattice.n[2] {
            for j in 0..lattice.n[1] {
                for i in 0..lattice.n[0] {
                    let d = lattice.at(i, j, k);
                    let g = lattice.gradient(i, j, k);
                    let len = (g[0] * g[0] + g[1] * g[1] + g[2] * g[2]).sqrt();
                    let nrm = if len > 1e-12 { g.map(|v| v / len) } else { [0.0, 0.0, 1.0] };
                    // a cut reaches `a |n|_1 / 2 <= 0.87 a` from the centre, the curvature's move at
                    // most `a / 4`: a sample farther than 1.3 a is whole or empty
                    let share = if d.abs() < 1.3 * a { lattice.share(i, j, k, nrm, len) } else if d < 0.0 { 1.0 } else { 0.0 };
                    let mut gp = classify(share, d, nrm, a, materials, materials.wall_thickness);
                    if zones && gp.fill > 0 {
                        let (infill, wall) = materials.at(lattice.pos(i, j, k));
                        if wall != materials.wall_thickness {
                            gp = classify(share, d, nrm, a, materials, wall);
                        }
                        gp.infill = infill;
                    }
                    if let Some(p) = printed.filter(|_| materials.as_printed && gp.fill > 0) {
                        let (wall, skin, dir) = p.shares(lattice.index(i, j, k), gp.fill as f64 / 255.0 * a * a * a);
                        gp.wall = (wall * 255.0).round() as u8;
                        gp.skin = (skin * 255.0).round() as u8;
                        if let Some(dir) = dir {
                            gp.angle = crate::material::angle_bin(dir);
                        }
                    }
                    let c = grid.cell(i / 2, j / 2, k / 2);
                    let q = (i & 1) | ((j & 1) << 1) | ((k & 1) << 2);
                    grid.gp[c][q] = gp;
                    if gp.fill > 0 && gp.fill < 255 {
                        let (w, m) = (gp.weight(), material_centroid(nrm, a, gp));
                        for x in 0..3 {
                            offset[c][x] += (w * m[x]) as f32;
                        }
                    }
                }
            }
        }
        for (o, gps) in offset.iter_mut().zip(&grid.gp) {
            *o = cell_offset(gps, *o, h).map(|v| v as f32);
        }
        grid.offset = offset;
        grid
    }

    /// The axes a coarser level halves: those with more than two cells. A thin part stops
    /// halving across its thickness and goes on along its length (semi-coarsening), so the
    /// coarsest level stays small; otherwise its few smoothing sweeps would not carry the
    /// correction across a long plate or beam.
    pub fn coarsenable(&self) -> [bool; 3] {
        self.n.map(|n| n > 2)
    }

    /// The shape one level coarser: cells twice the size along the [`Grid::coarsenable`]
    /// axes, from the same corner. It has no Gauss points: a coarse level's stiffness is the
    /// finer level's, taken through the interpolation between them (see [`crate::solve`]).
    pub fn coarser(&self) -> Grid {
        let halve = self.coarsenable();
        let n = std::array::from_fn(|a| if halve[a] { self.n[a].div_ceil(2) } else { self.n[a] });
        let h = std::array::from_fn(|a| if halve[a] { self.h[a] * 2.0 } else { self.h[a] });
        Grid { n, h, origin: self.origin, gp: Vec::new(), offset: Vec::new() }
    }
}

/// Filled shares this close to 0 or 1 snap to it.
/// A share under this (or this short of full) stands only where the upwind normal's plane
/// reaches the octant too ([`Lattice::share`]).
const SNAP: f64 = 0.1;

/// A sampled gradient shorter than this straddles a crease ([`Lattice::share`]). A round of two
/// cells across samples 0.943 on its own curve, which is no crease: the curvature's move
/// carries it.
const CREASE: f64 = 0.90;

/// Overlap of `[c - r, c + r]` with `[0, t]`, as a share of the first.
fn band(c: f64, r: f64, t: f64) -> f64 {
    if t <= 0.0 || r <= 0.0 {
        return if c >= 0.0 && c <= t { 1.0 } else { 0.0 };
    }
    ((c + r).min(t) - (c - r).max(0.0)).max(0.0) / (2.0 * r)
}

/// An octant of side `a` filled by `fill` ([`Lattice::share`]) whose centre lies `d` from the
/// surface (the field's unit gradient `n` there): its share stored to 1/255 (under half of
/// that is empty), and the shares of walls (`wall` thick there) and skin by its depth under the
/// surface across the layers and along them.
pub fn classify(fill: f64, d: f64, n: V3, a: f64, m: &Materials, wall: f64) -> Gp {
    let stored = (fill.clamp(0.0, 1.0) * 255.0).round();
    if stored < 1.0 {
        return Gp::default();
    }
    let depth = (-d).max(0.0);
    let nxy = (n[0] * n[0] + n[1] * n[1]).sqrt();
    let r = 0.5 * a;
    // across the layers: the horizontal distance to the outline
    let wall = if nxy > 0.05 && wall > 0.0 { band(depth / nxy, r / nxy.max(0.3), wall) } else { 0.0 };
    // along the layers: the vertical distance to a top (normal up) or bottom surface
    let thick = if n[2] > 0.0 { m.top_thickness } else { m.bottom_thickness };
    let nz = n[2].abs();
    let skin = if nz > 0.05 && thick > 0.0 { band(depth / nz, r / nz.max(0.3), thick) } else { 0.0 };
    let skin = skin.min(1.0 - wall);
    // the beads run along the outline: across the horizontal normal
    let angle = angle_bin(n[0].atan2(-n[1]));
    Gp { fill: stored as u8, wall: (wall * 255.0).round() as u8, skin: (skin * 255.0).round() as u8, angle, infill: 0 }
}

/// A normal component under this share of the normal's 1-norm is taken as zero: the cut is a
/// prism along that axis (two such, a slab).
const FLAT: f64 = 1e-4;

/// The part of the unit cube with `c . y <= b` (`c` ascending, summing to 1; `b` in 0..=1/2):
/// its volume and first moments. By inclusion and exclusion over the corners the plane passes:
/// each term a simplex from a corner (a triangle's prism where the smallest component is
/// zero, a slab where two are).
fn corner_cut(c: [f64; 3], b: f64) -> (f64, V3) {
    if c[1] < FLAT {
        let f = (b / c[2]).clamp(0.0, 1.0);
        return (f, [0.5 * f, 0.5 * f, 0.5 * f * f]);
    }
    if c[0] < FLAT {
        let s = c[1] + c[2];
        let (c2, b) = ([c[1] / s, c[2] / s], b / s);
        let (mut area, mut m) = (0.0, [0.0; 2]);
        for mask in 0..4usize {
            let r = b - (0..2).filter(|i| mask >> i & 1 == 1).map(|i| c2[i]).sum::<f64>();
            if r <= 0.0 {
                continue;
            }
            let sign = if mask.count_ones() % 2 == 0 { 1.0 } else { -1.0 };
            let t = sign * r * r / (2.0 * c2[0] * c2[1]);
            area += t;
            for i in 0..2 {
                m[i] += t * ((mask >> i & 1) as f64 + r / (3.0 * c2[i]));
            }
        }
        return (area, [0.5 * area, m[0], m[1]]);
    }
    let (mut vol, mut m) = (0.0, [0.0; 3]);
    for mask in 0..8usize {
        let r = b - (0..3).filter(|i| mask >> i & 1 == 1).map(|i| c[i]).sum::<f64>();
        if r <= 0.0 {
            continue;
        }
        let sign = if mask.count_ones() % 2 == 0 { 1.0 } else { -1.0 };
        let t = sign * r * r * r / (6.0 * c[0] * c[1] * c[2]);
        vol += t;
        for i in 0..3 {
            m[i] += t * ((mask >> i & 1) as f64 + r / (4.0 * c[i]));
        }
    }
    (vol, m)
}

/// The material of an octant of side `a` whose centre lies `d` from the surface (`n` the unit
/// outward normal: material where `n . x <= -d`, `x` from the centre): its filled share, exact
/// for a plane at any angle, and its centroid from the centre (mm). Past half full it is read
/// as the cube less the empty part, which keeps the inclusion and exclusion's terms few.
pub fn cut(d: f64, n: V3, a: f64) -> (f64, V3) {
    let mut axes = [0usize, 1, 2];
    axes.sort_by(|&i, &j| n[i].abs().total_cmp(&n[j].abs()));
    let s: f64 = n.iter().map(|v| v.abs()).sum();
    if s <= 0.0 {
        return if d <= 0.0 { (1.0, [0.0; 3]) } else { (0.0, [0.0; 3]) };
    }
    let c = axes.map(|i| n[i].abs() / s);
    let b = ((0.5 * a * s - d) / (a * s)).clamp(0.0, 1.0);
    let (vol, m) = if b <= 0.5 {
        corner_cut(c, b)
    } else {
        let (v, m) = corner_cut(c, 1.0 - b);
        (1.0 - v, m.map(|mi| 0.5 - (v - mi)))
    };
    let mut centroid = [0.0; 3];
    if vol > 0.0 {
        for (j, &i) in axes.iter().enumerate() {
            let sign = if n[i] < 0.0 { -1.0 } else { 1.0 };
            centroid[i] = sign * a * (m[j] / vol - 0.5);
        }
    }
    (vol, centroid)
}

/// An octant's material centroid from its centre (mm) at its share as stored (`gp.fill`,
/// after the snap and the rounding), so the points and the weights describe one shape: the
/// plane of the octant's sampled normal moved to hold that share (at a crease too, where the
/// share was read square to an axis: the blended normal still points the way the material
/// lies, and picking one axis would favour one of two equal faces). Zero for a full octant.
fn material_centroid(n: V3, a: f64, gp: Gp) -> V3 {
    let share = gp.fill as f64 / 255.0;
    if gp.fill >= 255 {
        return [0.0; 3];
    }
    // the share falls as the plane's distance grows: bisect for the stored one, to well under
    // what the offset's f32 keeps (about 24 halvings)
    let reach = 0.5 * a * (n[0].abs() + n[1].abs() + n[2].abs());
    let (mut lo, mut hi) = (-reach, reach);
    while hi - lo > 1e-7 * a {
        let mid = 0.5 * (lo + hi);
        if cut(mid, n, a).0 > share {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    cut(0.5 * (lo + hi), n, a).1
}

/// A cell's point offset (mm): the eight points moved together so their centroid, weighted as
/// the stiffness weighs them ([`Gp::weight`], an empty octant's fictitious share at its
/// centre), is the material's: each octant's centre plus its material centroid against its
/// Gauss point. `pulled`: the cut octants' material centroids (from their centres) summed with
/// those weights.
fn cell_offset(gps: &[Gp; 8], pulled: [f32; 3], h: f64) -> V3 {
    if gps.iter().all(|g| g.fill == 255) || gps.iter().all(|g| g.fill == 0) {
        return [0.0; 3];
    }
    let (mut sum, mut moved) = (0.0, pulled.map(f64::from));
    for (q, g) in gps.iter().enumerate() {
        let k = crate::element::corner(q);
        let w = g.weight();
        for a in 0..3 {
            let side = 2.0 * k[a] as f64 - 1.0;
            // the octant's centre and its point, from the cell's centre
            let centre = side * 0.25 * h;
            let point = side * crate::element::GP * 0.5 * h;
            moved[a] += w * (centre - point);
        }
        sum += w;
    }
    moved.map(|m| m / sum)
}

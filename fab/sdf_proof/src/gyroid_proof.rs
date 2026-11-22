//! The gyroid's declared bounds (`bounds.splash`, what `sdf_gyroid_bound` reads), proven by
//! computer over the part of the gyroid's period cube its symmetries map all of space to. The
//! proof takes half a minute of every core: it is this crate's program ([`PROVE`]), which the CI
//! runs on every pass (ci.toml `runs`), and records what it proved as a fingerprint
//! (`gyroid_proof.fingerprint`: the declarations' gyroid section and this file); the suite's
//! `the_gyroid_proof_is_current` fails on any change to either until the proof runs again.
//!
//! In u = k p (k = 2 pi / period) the field is |h| - wall k / 2 with h = G / max(N, 1), G the
//! gyroid function and N = |grad G|: the smaller of |G| (where N <= 1) and |f| with f = G / N
//! (where N >= 1). Proven here:
//! - everywhere: |grad G| <= GYROID_LG, the Hessian of G within GYROID_HG in size (so N moves
//!   at most GYROID_HG per unit), and sign(G) D^2 G at most GYROID_UP;
//! - wherever N >= 0.8 and |f| lies in a band (GYROID_BAND wide): |grad f| <= gyroid_l8 and
//!   sign(f) D^2 f at most gyroid_k8, both at that band;
//! - wherever N >= 1: |grad f| <= GYROID_LMAX (the field's own steepest).
//!
//! Branch and bound: a cell (half diagonal r about its centre) holds a bound when the centre's
//! exact value plus the cell's own spread is under it: Weyl for an eigenvalue (it moves no more
//! than the Hessian's Frobenius norm, which moves at most the third derivative's per unit) and
//! the Hessian's norm for the gradient, each over the whole cell by interval arithmetic on the
//! derivatives (forward jets of intervals, outward rounded); or the cell's interval Hessian or
//! gradient directly, whichever is smaller. A cell that cannot show it splits in eight.
use crate::BOUNDS;
use makepad_script_compute::kernel::{compile_with_modules, KernelRef};
use makepad_script_compute::Backend;
use makepad_script_compute_sched::kernel::KernelExt;
use std::f64::consts::{FRAC_PI_2, PI, TAU};

/// The command that proves the declarations again and records the new fingerprint.
pub const PROVE: &str = "makepad run fab-sdf-proof -- --prove-gyroid";

/// What the proof depends on: the declarations' gyroid section (its constants, tables and bound
/// function, from its heading to the operators') and this file, as FNV-1a 64.
fn fingerprint() -> String {
    let bounds = BOUNDS.source;
    let from = bounds.find("// The gyroid's:").expect("the declarations' gyroid section (`// The gyroid's:`)");
    let to = from + bounds[from..].find("// The operators':").expect("the section after the gyroid's (`// The operators':`)");
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for b in bounds[from..to].bytes().chain(include_str!("gyroid_proof.rs").bytes()) {
        h = (h ^ b as u64).wrapping_mul(0x100_0000_01b3);
    }
    format!("{h:016x}")
}

/// An interval, outward rounded.
#[derive(Clone, Copy, Debug)]
struct I {
    lo: f64,
    hi: f64,
}

const ULP: f64 = 4.0e-16;

fn out(lo: f64, hi: f64) -> I {
    I { lo: lo - lo.abs() * ULP - 1e-300, hi: hi + hi.abs() * ULP + 1e-300 }
}

impl I {
    fn p(x: f64) -> I {
        I { lo: x, hi: x }
    }
    fn add(self, o: I) -> I {
        out(self.lo + o.lo, self.hi + o.hi)
    }
    fn neg(self) -> I {
        I { lo: -self.hi, hi: -self.lo }
    }
    fn mul(self, o: I) -> I {
        let c = [self.lo * o.lo, self.lo * o.hi, self.hi * o.lo, self.hi * o.hi];
        out(c.iter().copied().fold(f64::INFINITY, f64::min), c.iter().copied().fold(f64::NEG_INFINITY, f64::max))
    }
    fn mag(self) -> f64 {
        self.lo.abs().max(self.hi.abs())
    }
    fn mid(self) -> f64 {
        0.5 * (self.lo + self.hi)
    }
    fn recip(self) -> I {
        assert!(self.lo > 0.0, "a reciprocal of {self:?}");
        out(1.0 / self.hi, 1.0 / self.lo)
    }
    fn sqrt(self) -> I {
        out(self.lo.max(0.0).sqrt(), self.hi.sqrt())
    }
    /// sin over the interval: its ends, and 1 or -1 where a peak lies inside.
    fn sin(self) -> I {
        if self.hi - self.lo >= TAU {
            return I { lo: -1.0, hi: 1.0 };
        }
        let (a, b) = (self.lo.sin(), self.hi.sin());
        let (mut lo, mut hi) = (a.min(b), a.max(b));
        if FRAC_PI_2 + ((self.lo - FRAC_PI_2) / TAU).ceil() * TAU <= self.hi {
            hi = 1.0;
        }
        if -FRAC_PI_2 + ((self.lo + FRAC_PI_2) / TAU).ceil() * TAU <= self.hi {
            lo = -1.0;
        }
        I { lo: (lo - 1e-15).max(-1.0), hi: (hi + 1e-15).min(1.0) }
    }
    fn cos(self) -> I {
        I { lo: self.lo + FRAC_PI_2, hi: self.hi + FRAC_PI_2 }.sin()
    }
}

/// The sorted pair and triple of axes each Hessian and third-derivative slot holds.
const H2: [[usize; 2]; 6] = [[0, 0], [0, 1], [0, 2], [1, 1], [1, 2], [2, 2]];
const T3: [[usize; 3]; 10] = [[0, 0, 0], [0, 0, 1], [0, 0, 2], [0, 1, 1], [0, 1, 2], [0, 2, 2], [1, 1, 1], [1, 1, 2], [1, 2, 2], [2, 2, 2]];

fn hi(i: usize, j: usize) -> usize {
    let (a, b) = (i.min(j), i.max(j));
    H2.iter().position(|s| *s == [a, b]).unwrap()
}

/// A function's value and derivatives to the third, each an interval over a box.
#[derive(Clone, Copy)]
struct J {
    v: I,
    d: [I; 3],
    h: [I; 6],
    t: [I; 10],
}

impl J {
    fn var(axis: usize, x: I) -> J {
        let z = I::p(0.0);
        let mut d = [z; 3];
        d[axis] = I::p(1.0);
        J { v: x, d, h: [z; 6], t: [z; 10] }
    }
    fn add(&self, o: &J) -> J {
        J { v: self.v.add(o.v), d: std::array::from_fn(|i| self.d[i].add(o.d[i])), h: std::array::from_fn(|i| self.h[i].add(o.h[i])), t: std::array::from_fn(|i| self.t[i].add(o.t[i])) }
    }
    fn sub(&self, o: &J) -> J {
        self.add(&o.neg())
    }
    fn neg(&self) -> J {
        J { v: self.v.neg(), d: self.d.map(I::neg), h: self.h.map(I::neg), t: self.t.map(I::neg) }
    }
    /// Leibniz: a derivative of a product is the sum over the splits of its axes.
    fn mul(&self, o: &J) -> J {
        let (a, b) = (self, o);
        let d = std::array::from_fn(|i| a.d[i].mul(b.v).add(a.v.mul(b.d[i])));
        let h = std::array::from_fn(|s| {
            let [i, j] = H2[s];
            a.h[s].mul(b.v).add(a.d[i].mul(b.d[j])).add(a.d[j].mul(b.d[i])).add(a.v.mul(b.h[s]))
        });
        let t = std::array::from_fn(|s| {
            let [i, j, k] = T3[s];
            a.t[s].mul(b.v)
                .add(a.h[hi(i, j)].mul(b.d[k]))
                .add(a.h[hi(i, k)].mul(b.d[j]))
                .add(a.h[hi(j, k)].mul(b.d[i]))
                .add(a.d[i].mul(b.h[hi(j, k)]))
                .add(a.d[j].mul(b.h[hi(i, k)]))
                .add(a.d[k].mul(b.h[hi(i, j)]))
                .add(a.v.mul(b.t[s]))
        });
        J { v: a.v.mul(b.v), d, h, t }
    }
    /// f(u) for a function of one variable with derivatives f[0..4] over u's value (Faa di
    /// Bruno to the third order).
    fn compose(&self, f: [I; 4]) -> J {
        let u = self;
        let d = std::array::from_fn(|i| f[1].mul(u.d[i]));
        let h = std::array::from_fn(|s| {
            let [i, j] = H2[s];
            f[2].mul(u.d[i]).mul(u.d[j]).add(f[1].mul(u.h[s]))
        });
        let t = std::array::from_fn(|s| {
            let [i, j, k] = T3[s];
            let pairs = u.h[hi(i, j)].mul(u.d[k]).add(u.h[hi(i, k)].mul(u.d[j])).add(u.h[hi(j, k)].mul(u.d[i]));
            f[3].mul(u.d[i]).mul(u.d[j]).mul(u.d[k]).add(f[2].mul(pairs)).add(f[1].mul(u.t[s]))
        });
        J { v: f[0], d, h, t }
    }
    fn sin(&self) -> J {
        let (s, c) = (self.v.sin(), self.v.cos());
        self.compose([s, c, s.neg(), c.neg()])
    }
    fn cos(&self) -> J {
        let (s, c) = (self.v.sin(), self.v.cos());
        self.compose([c, s.neg(), c.neg(), s])
    }
    fn sqrt(&self) -> J {
        let r = self.v.sqrt();
        let ir = r.recip();
        let v = self.v.recip();
        let f1 = ir.mul(I::p(0.5));
        let f2 = f1.mul(v).mul(I::p(-0.5));
        let f3 = f2.mul(v).mul(I::p(-1.5));
        self.compose([r, f1, f2, f3])
    }
    fn recip(&self) -> J {
        let v = self.v.recip();
        let v2 = v.mul(v);
        self.compose([v, v2.neg(), v2.mul(v).mul(I::p(2.0)), v2.mul(v2).mul(I::p(-6.0))])
    }
}

/// G, N and f = G / N over the box (or a point: zero-width intervals).
fn gyroid(b: [I; 3]) -> (J, J, J) {
    let x: [J; 3] = std::array::from_fn(|i| J::var(i, b[i]));
    let [(sx, cx), (sy, cy), (sz, cz)] = x.map(|x| (x.sin(), x.cos()));
    let g = sx.mul(&cy).add(&sy.mul(&cz)).add(&sz.mul(&cx));
    let gx = cx.mul(&cy).sub(&sz.mul(&sx));
    let gy = cy.mul(&cz).sub(&sx.mul(&sy));
    let gz = cz.mul(&cx).sub(&sy.mul(&sz));
    let q = gx.mul(&gx).add(&gy.mul(&gy)).add(&gz.mul(&gz));
    if q.v.lo <= 0.0 {
        let n = J { v: I { lo: 0.0, hi: q.v.hi.sqrt() * (1.0 + ULP) }, ..q };
        return (g, n, g);
    }
    let n = q.sqrt();
    let f = g.mul(&n.recip());
    (g, n, f)
}

/// A symmetric 3 x 3 matrix's least and largest eigenvalues (closed form).
fn extremes(m: [[f64; 3]; 3]) -> (f64, f64) {
    let p1 = m[0][1] * m[0][1] + m[0][2] * m[0][2] + m[1][2] * m[1][2];
    let q = (m[0][0] + m[1][1] + m[2][2]) / 3.0;
    let p2 = (m[0][0] - q).powi(2) + (m[1][1] - q).powi(2) + (m[2][2] - q).powi(2) + 2.0 * p1;
    let p = (p2 / 6.0).sqrt();
    if p < 1e-300 {
        return (q, q);
    }
    let b: [[f64; 3]; 3] = std::array::from_fn(|i| std::array::from_fn(|j| (m[i][j] - if i == j { q } else { 0.0 }) / p));
    let det = b[0][0] * (b[1][1] * b[2][2] - b[1][2] * b[2][1]) - b[0][1] * (b[1][0] * b[2][2] - b[1][2] * b[2][0]) + b[0][2] * (b[1][0] * b[2][1] - b[1][1] * b[2][0]);
    let phi = (det / 2.0).clamp(-1.0, 1.0).acos() / 3.0;
    (q + 2.0 * p * (phi + 2.0 * PI / 3.0).cos(), q + 2.0 * p * phi.cos())
}

/// What `bounds.splash` declares, at levels `ls`: per level gyroid_k8 and gyroid_l8 (step functions
/// of the band), and its constants: (band, LMAX, LG, HG, UP).
fn compile(src: &str) -> Result<KernelRef, Vec<makepad_script_compute::ShaderError>> {
    compile_with_modules(src, &[], Backend::Native, &[BOUNDS])
}

fn declared(ls: &[f32]) -> (Vec<[f32; 2]>, [f32; 5]) {
    let src = "use sdf.bounds.*\nlet lv = input(f32)\nlet out = output(vec4)\nlet cst = output(vec4)\nfn element(i) {\n let l = lv[i]\n out[i] = vec4(gyroid_k8(l), gyroid_l8(l), 0.0, 0.0)\n cst[i] = vec4(GYROID_BAND, GYROID_LMAX, GYROID_LG, GYROID_HG)\n}";
    let k = compile(src).unwrap_or_else(|e| panic!("the declarations' kernel: {e:?}"));
    let mut out = vec![0.0f32; ls.len() * 4];
    let mut cst = vec![0.0f32; ls.len() * 4];
    let mut c = k.call();
    c.input("lv", ls).unwrap();
    c.output("out", &mut out).unwrap();
    c.output("cst", &mut cst).unwrap();
    c.run(ls.len()).unwrap();
    let k = compile("use sdf.bounds.*\nlet out = output(f32)\nfn element(i) { out[i] = GYROID_UP }").unwrap_or_else(|e| panic!("{e:?}"));
    let mut up = vec![0.0f32; 1];
    let mut c = k.call();
    c.output("out", &mut up).unwrap();
    c.run(1).unwrap();
    (out.chunks(4).map(|v| [v[0], v[1]]).collect(), [cst[0], cst[1], cst[2], cst[3], up[0]])
}

struct Tables {
    band: f64,
    k8: Vec<f64>,
    l8: Vec<f64>,
    lmax: f64,
    lg: f64,
    hg: f64,
    up: f64,
}

/// Reads the declarations and checks their shape: gyroid_k8 and gyroid_l8 constant over each
/// band (from its lower edge up to, not including, the next one's) and never falling, so the
/// value at a level bounds every level under it as well.
fn tables() -> Tables {
    let (_, c) = declared(&[0.0]);
    let band = c[0] as f64;
    let nb = (2.0 / band).round() as usize;
    let sweep: Vec<f32> = (0..=nb * 64).map(|i| (i as f64 * band / 64.0) as f32).collect();
    let (v, _) = declared(&sweep);
    let mut t = Tables { band, k8: vec![0.0; nb], l8: vec![0.0; nb], lmax: c[1] as f64, lg: c[2] as f64, hg: c[3] as f64, up: c[4] as f64 };
    for j in 0..nb {
        t.k8[j] = v[j * 64 + 32][0] as f64;
        t.l8[j] = v[j * 64 + 32][1] as f64;
    }
    for (i, (l, d)) in sweep.iter().zip(&v).enumerate() {
        let j = ((*l as f64 / band) as usize).min(nb - 1);
        if (j as f64 * band) as f32 <= *l && *l < ((j + 1) as f64 * band) as f32 {
            assert!(d[0] as f64 == t.k8[j] && d[1] as f64 == t.l8[j], "the declared bounds at level {l} are not their band's ({d:?})");
        }
        if i > 0 {
            assert!(d[0] >= v[i - 1][0] && d[1] >= v[i - 1][1], "the declared bounds fall at level {l}");
        }
    }
    t
}

/// What a cell shows, or why it splits: the bound it could not show, what it reached and what
/// is declared.
enum Cell {
    Holds,
    Split(&'static str, f64, f64),
}

/// Per band the most the bounds reached in cells that held (how close the declarations are),
/// and the most of the gyroid function's own.
#[derive(Default, Clone)]
struct Most {
    l8: Vec<f64>,
    k8: Vec<f64>,
    g: [f64; 4],
    /// the splits per bound that made them
    splits: std::collections::BTreeMap<&'static str, usize>,
}

/// sign(f) D^2 f's largest eigenvalue over the cell (both signs where f may change sign):
/// the centre's plus Weyl (the third derivative's Frobenius norm over r), or the interval
/// Hessian's distance from the centre's, whichever is smaller.
fn side(f: &J, fc: &J, r: f64) -> f64 {
    let m: [[f64; 3]; 3] = std::array::from_fn(|i| std::array::from_fn(|j| fc.h[hi(i, j)].mid()));
    let (el, eh) = extremes(m);
    let t3 = f.t.iter().enumerate().map(|(s, v)| {
        let [i, j, k] = T3[s];
        let mult = if i == j && j == k { 1.0 } else if i == j || j == k { 3.0 } else { 6.0 };
        v.mag().powi(2) * mult
    }).sum::<f64>().sqrt();
    let spread = f.h.iter().enumerate().map(|(s, h)| {
        let mc = fc.h[s].mid();
        (h.hi - mc).max(mc - h.lo).max(0.0).powi(2) * if H2[s][0] == H2[s][1] { 1.0 } else { 2.0 }
    }).sum::<f64>().sqrt();
    let margin = (t3 * r).min(spread) + 1e-9 * (eh.abs() + el.abs() + 1.0);
    let mut k = f64::NEG_INFINITY;
    if f.v.hi > 0.0 {
        k = k.max(eh + margin);
    }
    if f.v.lo < 0.0 {
        k = k.max(-el + margin);
    }
    k
}

/// |grad f| over the cell: the centre's plus the Hessian's norm over r, or the interval
/// gradient's.
fn grad(f: &J, fc: &J, r: f64) -> f64 {
    let dc: Vec<f64> = fc.d.iter().map(|d| d.mid()).collect();
    let hf = f.h.iter().enumerate().map(|(s, h)| h.mag().powi(2) * if H2[s][0] == H2[s][1] { 1.0 } else { 2.0 }).sum::<f64>().sqrt();
    ((dc[0] * dc[0] + dc[1] * dc[1] + dc[2] * dc[2]).sqrt() * (1.0 + 1e-12) + hf * r).min(f.d.iter().map(|d| d.mag().powi(2)).sum::<f64>().sqrt())
}

/// The largest |eigenvalue| of the Hessian over the cell: the centre's plus the spread, or
/// Gershgorin's on the interval Hessian.
fn hess_norm(f: &J, fc: &J, r: f64) -> f64 {
    let m: [[f64; 3]; 3] = std::array::from_fn(|i| std::array::from_fn(|j| fc.h[hi(i, j)].mid()));
    let (el, eh) = extremes(m);
    let t3 = f.t.iter().map(|v| v.mag().powi(2)).sum::<f64>().sqrt() * 6f64.sqrt();
    let gersh = (0..3).map(|i| (0..3).map(|j| f.h[hi(i, j)].mag()).sum::<f64>()).fold(0.0, f64::max);
    (eh.abs().max(el.abs()) + t3 * r + 1e-9).min(gersh)
}

fn cell(c: [f64; 3], s: f64, t: &Tables, most: &mut Most) -> Cell {
    let half = 0.5 * s;
    let b = c.map(|x| out(x - half, x + half));
    let r = 3f64.sqrt() * half;
    let (g, n, f) = gyroid(b);
    let (gc, nc, fc) = gyroid(c.map(I::p));
    // the gyroid function's own: |grad G| = N, the Hessian's size, K+ of |G|
    let (lg, hg, up) = (grad(&g, &gc, r), hess_norm(&g, &gc, r), side(&g, &gc, r));
    for (what, got, declared) in [("|grad G|", lg, t.lg), ("|D2 G|", hg, t.hg), ("K+ |G|", up, t.up)] {
        if got > declared {
            return Cell::Split(what, got, declared);
        }
    }
    most.g = [most.g[0].max(lg), most.g[1].max(hg), most.g[2].max(up), most.g[3].max(nc.v.mid())];
    if n.v.hi < 0.8 {
        return Cell::Holds;
    }
    if n.v.lo <= 0.0 {
        return Cell::Split("N > 0", n.v.lo, 0.0);
    }
    // the cell's bands of |f| (past the last band nothing is declared)
    let (lo, hi) = if f.v.lo > 0.0 { (f.v.lo, f.v.hi) } else if f.v.hi < 0.0 { (-f.v.hi, -f.v.lo) } else { (0.0, f.v.mag()) };
    let nb = t.k8.len();
    let (gf, kf) = (grad(&f, &fc, r), side(&f, &fc, r));
    if n.v.hi >= 1.0 && gf > t.lmax {
        return Cell::Split("|grad f| (N >= 1)", gf, t.lmax);
    }
    let j0 = (lo / t.band) as usize;
    if j0 >= nb {
        return Cell::Holds;
    }
    // the bounds never fall with the level, so the lowest band's are the least
    if gf > t.l8[j0] {
        return Cell::Split("|grad f| (its band)", gf, t.l8[j0]);
    }
    if kf > t.k8[j0] {
        return Cell::Split("K+ |f| (its band)", kf, t.k8[j0]);
    }
    let j1 = ((hi / t.band) as usize).min(nb - 1);
    for j in j0..=j1 {
        most.l8[j] = most.l8[j].max(gf);
        most.k8[j] = most.k8[j].max(kf);
    }
    Cell::Holds
}

/// Proves the declarations over [0, 2 pi]^2 x [0, pi / 2], which every point of space maps to
/// by the gyroid's periods, its body-centring translation (pi, pi, pi) (G stays) and the point
/// reflection p -> (pi, pi, pi) - p (G turns sign; |G|, N, |f| and every bound here stay):
/// `n0`^2 x `n0` / 4 cells, each splitting at most `depth` times, on every core; the cells it
/// looked at.
fn prove(t: &Tables, n0: usize, depth: u32) -> Result<(Most, usize), String> {
    let nb = t.k8.len();
    let s0 = TAU / n0 as f64;
    let starts: Vec<[f64; 3]> = (0..n0 * n0 * (n0 / 4)).map(|i| [i % n0, (i / n0) % n0, i / (n0 * n0)].map(|k| (k as f64 + 0.5) * s0)).collect();
    let next = std::sync::atomic::AtomicUsize::new(0);
    // a failed cell stops every core
    let failed = std::sync::atomic::AtomicBool::new(false);
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
    let runs: Vec<Result<(Most, usize), String>> = std::thread::scope(|sc| {
        let hs: Vec<_> = (0..threads).map(|_| sc.spawn(|| {
            let mut most = Most { l8: vec![0.0; nb], k8: vec![f64::NEG_INFINITY; nb], g: [0.0; 4], splits: Default::default() };
            let mut cells = 0usize;
            loop {
                let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let Some(&start) = starts.get(i).filter(|_| !failed.load(std::sync::atomic::Ordering::Relaxed)) else { return Ok((most, cells)) };
                let mut stack = vec![(start, 0u32)];
                while let Some((c, d)) = stack.pop() {
                    cells += 1;
                    let s = s0 / (1u64 << d) as f64;
                    if let Cell::Split(what, got, declared) = cell(c, s, t, &mut most) {
                        *most.splits.entry(what).or_default() += 1;
                        if d >= depth {
                            failed.store(true, std::sync::atomic::Ordering::Relaxed);
                            let (g, n, f) = gyroid(c.map(I::p));
                            return Err(format!("a cell of {s:.2e} about {c:?} (G {:.3}, N {:.3}, f {:.3}) does not show {what} <= {declared} (it reaches {got:.4}), after {cells} cells", g.v.mid(), n.v.mid(), f.v.mid()));
                        }
                        for o in 0..8 {
                            stack.push((std::array::from_fn(|k| c[k] + if (o >> k) & 1 == 0 { -0.25 } else { 0.25 } * s), d + 1));
                        }
                    }
                }
            }
        })).collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let mut most = Most { l8: vec![0.0; nb], k8: vec![f64::NEG_INFINITY; nb], g: [0.0; 4], splits: Default::default() };
    let mut cells = 0;
    for run in runs {
        let (m, n) = run?;
        cells += n;
        for j in 0..nb {
            most.l8[j] = most.l8[j].max(m.l8[j]);
            most.k8[j] = most.k8[j].max(m.k8[j]);
        }
        for k in 0..4 {
            most.g[k] = most.g[k].max(m.g[k]);
        }
        for (what, n) in m.splits {
            *most.splits.entry(what).or_default() += n;
        }
    }
    Ok((most, cells))
}

/// The proof, then its fingerprint recorded ([`PROVE`]; the CI runs it on every pass, ci.toml
/// `runs`). The error names the bound that did not hold, or the fingerprint that was not recorded.
pub fn prove_and_record() -> Result<(), String> {
    let t = tables();
    let started = std::time::Instant::now();
    let (most, cells) = prove(&t, 16, 10).map_err(|e| format!("{e} (after {:.2} s)", started.elapsed().as_secs_f64()))?;
    println!("gyroid declarations proven over {cells} cells in {:.2} s: |grad G| {:.4} of {}, |D2 G| {:.3} of {}, K+ |G| {:.3} of {}; splits {:?}", started.elapsed().as_secs_f64(), most.g[0], t.lg, most.g[1], t.hg, most.g[2], t.up, most.splits);
    for j in 0..t.k8.len() {
        println!("  |f| {:.2}..{:.2}: L8 {:.3} of {}, K8 {:.3} of {}", j as f64 * t.band, (j + 1) as f64 * t.band, most.l8[j], t.l8[j], most.k8[j], t.k8[j]);
    }
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/src/gyroid_proof.fingerprint");
    let f = fingerprint();
    std::fs::write(path, format!("{f}\n")).map_err(|e| format!("recording the fingerprint {f} in {path}: {e}"))?;
    println!("proven: fingerprint {f} recorded in {path}");
    Ok(())
}

/// The suite's guard: the declarations and the proof are what it last proved.
#[test]
fn the_gyroid_proof_is_current() {
    let proven = include_str!("gyroid_proof.fingerprint").trim();
    let now = fingerprint();
    assert!(now == proven, "the gyroid's declarations or their proof (gyroid_proof.rs) changed since it last ran (fingerprint {now}, proven {proven}): prove them again, which records the new fingerprint: {PROVE}");
}

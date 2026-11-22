//! A Splash field's declared bounds over the ball of radius `ball` about a point: the Hessian's
//! (K+, K-), its Lipschitz bound L and how far its f32 value can be from the exact one, e. They
//! come from the field's own compiled program ([`SdfSplashExpr::compile_traced`]) and the prelude
//! functions' declarations (`bounds.splash`), never from samples.
//!
//! From the field's distance down, each call of a declared function takes its declared bound: a
//! field operand (`<a>_k`) the bound of the call that made that operand, the ball as it is in the
//! call's own frame. A call with a point `p` is a leaf: its point must be an affine map of the
//! field's (the program's own arithmetic, read from the values' definitions: sums, differences,
//! negations, and products and quotients by values that do not depend on the point), M its linear
//! part. Its stretch s bounds |M| (the least of the Frobenius norm and sqrt(|M|_1 |M|_inf)); the
//! map's own f32 arithmetic puts the point within d of the exact map's (each step's rounding over
//! the values it spans on the ball). The leaf's ball is then s ball + d, its Hessian bounds s^2
//! times its own, its L s times, and its error grows by L d. Every other argument must not depend
//! on the point. The lesser or the greater of two field operands (`min`, `max`) takes the declared
//! union's or intersection's bound, which are exactly those. Anything else (a distance no
//! declared call made, a point that is not an affine map, a call inside a branch) proves nothing:
//! [`NONE`] throughout, e too.

use makepad_csg_sdf::SdfSplashExpr;
use makepad_script_compute::kernel::{compile_with_modules, KernelRef};
use makepad_script_compute::lower::trace::{TraceArg, TraceArgKind, TraceCall, TraceDef, TraceDefKind};
use makepad_script_compute::module::Module;
use makepad_script_compute::Backend;
use makepad_script_compute_sched::kernel::KernelExt;
use std::collections::HashMap;
use std::sync::OnceLock;

pub mod gyroid_proof;

/// "No bound" (`BOUND_NONE` in `bounds.splash`).
pub const NONE: f32 = 1.0e30;
/// Twice the unit roundoff of f32: a rounding's error over a value bounded (in f32) by m is
/// under U m with room for that bound's own rounding.
const U: f32 = 1.192_092_9e-7;

/// The declarations, a module of the kernel language (`use sdf.bounds.*`).
pub const BOUNDS: Module<'static> = Module { path: "sdf.bounds", source: include_str!("bounds.splash") };

/// Where `bound_call.splash` takes an argument.
#[derive(Clone, Copy)]
enum Slot {
    /// A number free of the point: lane k of `s`.
    Number(usize),
    /// The point (lanes of `v0`), or a vector free of it (lanes of `v1`).
    Vector(usize),
    /// A field operand: its value in lane k of `s`, its bound in `v0` or `v1`.
    Operand(usize, usize),
    /// A number, vector or array free of the point that the bound needs only the size of: its
    /// largest magnitude in lane k of `s`.
    Free(usize),
}
use Slot::{Free, Number, Operand, Vector};

/// The prelude's functions that declare a bound, by their number in `bound_call.splash`, with
/// where each parameter goes.
const DECLARED: &[(&str, &[(&str, Slot)])] = &[
    ("sdf_sphere", &[("p", Vector(0)), ("r", Number(0))]),
    ("sdf_box", &[("p", Vector(0)), ("size", Vector(1))]),
    ("sdf_round_box", &[("p", Vector(0)), ("size", Vector(1)), ("r", Number(0))]),
    ("sdf_plane", &[("p", Vector(0)), ("n", Vector(1)), ("d", Number(0))]),
    ("sdf_gyroid", &[("p", Vector(0)), ("period", Number(0)), ("wall", Number(1))]),
    ("sdf_union", &[("a", Operand(0, 0)), ("b", Operand(1, 1))]),
    ("sdf_intersection", &[("a", Operand(0, 0)), ("b", Operand(1, 1))]),
    ("sdf_difference", &[("a", Operand(0, 0)), ("b", Operand(1, 1))]),
    ("smin", &[("a", Operand(0, 0)), ("b", Operand(1, 1)), ("k", Number(2))]),
    ("smooth_union", &[("a", Operand(0, 0)), ("b", Operand(1, 1)), ("k", Number(2))]),
    ("smooth_difference", &[("a", Operand(0, 0)), ("b", Operand(1, 1)), ("k", Number(2))]),
    ("smooth_intersection", &[("a", Operand(0, 0)), ("b", Operand(1, 1)), ("k", Number(2))]),
    ("sdf_round", &[("d", Operand(0, 0)), ("r", Number(1))]),
    ("sdf_shell", &[("d", Operand(0, 0)), ("t", Number(1))]),
    ("sdf_capsule", &[("p", Vector(0)), ("a", Free(1)), ("b", Free(2)), ("r", Number(0))]),
    ("sdf_round_cone", &[("p", Vector(0)), ("a", Free(2)), ("b", Free(3)), ("ra", Number(0)), ("rb", Number(1))]),
    ("sdf_tube", &[("p", Vector(0)), ("pts", Free(0))]),
];

/// The kernel that calls one declared bound.
fn bound_call() -> &'static KernelRef {
    static KERNEL: OnceLock<KernelRef> = OnceLock::new();
    KERNEL.get_or_init(|| {
        compile_with_modules(include_str!("bound_call.splash"), &[], Backend::Native, &[BOUNDS]).unwrap_or_else(|e| panic!("bound_call.splash: {e:?}"))
    })
}

/// The field's declared bounds over the ball of radius `ball` about `p`, with every parameter
/// given (by the field's parameter index, as [`SdfSplashExpr::params`] lists them): (distance,
/// K+, K-, L, e), [`NONE`] for a bound it has none of; None: the field has no trace to read.
pub fn bound(field: &SdfSplashExpr, params: &[f32], p: [f32; 3], ball: f32) -> Option<[f32; 5]> {
    let trace = field.trace()?.ok()?;
    let view = trace.view();
    let kernel = field.kernel();
    let mut by_name = vec![0.0f32; view.params.len()];
    for (i, v) in params.iter().enumerate().take(kernel.param_count()) {
        if let Some(k) = view.params.iter().position(|n| *n == kernel.param_view(i).name) {
            by_name[k] = *v;
        }
    }
    let mut values = vec![0u32; view.values];
    trace.run(&by_name, "sdf_points", &p, &mut values);
    // the host's own call of the field: its point and its distance
    let entry = view.calls.iter().rev().find(|c| c.name == "sdf_field")?;
    let d = f32::from_bits(values[entry.value as usize]);
    let none = Some([d, NONE, NONE, NONE, NONE]);
    let Some(at) = entry.args.first() else { return none };
    let (TraceArgKind::Vector, &[x, y, z]) = (at.kind, at.lanes) else { return none };
    let point = [x, y, z];
    // operands are defined before their uses: one pass finds what depends on the point
    let (mut defs, mut depends) = (HashMap::new(), HashMap::new());
    for def in view.defs {
        let on = |a: &u32| point.contains(a) || depends.get(a).copied().unwrap_or(true);
        let d = match def.kind {
            TraceDefKind::Const | TraceDefKind::Context => false,
            TraceDefKind::Memory => true,
            _ => def.args[..def.argc as usize].iter().any(on),
        };
        depends.insert(def.val, d || point.contains(&def.val));
        defs.insert(def.val, *def);
    }
    let calls = view.calls.iter().filter(|c| c.library && DECLARED.iter().any(|(n, _)| *n == c.name)).collect();
    let mut proof = Proof { values: &values, defs, depends, calls, point, ball, bounds: HashMap::new(), maps: HashMap::new() };
    match proof.bound_of(entry.value) {
        Some([kp, kn, l, e]) => Some([d, kp, kn, l, e]),
        None => none,
    }
}

/// `Bin::MinF` and `Bin::MaxF` as the kernels compute them.
fn min(a: f32, b: f32) -> f32 {
    if a < b { a } else { b }
}
fn max(a: f32, b: f32) -> f32 {
    if a > b { a } else { b }
}

/// A coefficient of an affine map, and whether the program knows it as a constant.
#[derive(Clone, Copy)]
struct Coef {
    v: f32,
    constant: bool,
}

/// A value as an affine map of the field's point over the ball: its linear part's rows (None:
/// exactly 0), a bound on its size over the ball, and on its f32 arithmetic's error (None:
/// exact).
#[derive(Clone, Copy)]
struct Aff {
    j: [Option<Coef>; 3],
    m: f32,
    e: Option<f32>,
}

struct Proof<'a> {
    /// Every value of the run at this point.
    values: &'a [u32],
    /// The definitions of the values the composition may use, which of them may depend on the
    /// field's point (anything not among them may).
    defs: HashMap<u32, TraceDef>,
    depends: HashMap<u32, bool>,
    calls: Vec<&'a TraceCall<'a>>,
    point: [u32; 3],
    ball: f32,
    bounds: HashMap<u32, Option<[f32; 4]>>,
    maps: HashMap<u32, Option<Aff>>,
}

impl Proof<'_> {
    fn value(&self, v: u32) -> f32 {
        f32::from_bits(self.values[v as usize])
    }

    fn depends(&self, v: u32) -> bool {
        self.depends.get(&v).copied().unwrap_or(true)
    }

    /// Whether every lane of `a` is free of the field's point.
    fn independent(&self, a: &TraceArg) -> bool {
        match a.kind {
            TraceArgKind::Literal => true,
            TraceArgKind::Float | TraceArgKind::Vector | TraceArgKind::Array => a.lanes.iter().all(|v| !self.depends(*v)),
            TraceArgKind::Other => false,
        }
    }

    /// The bound of the value `d` (the call that made it, or the lesser or the greater of two
    /// values the program took), or None.
    fn bound_of(&mut self, d: u32) -> Option<[f32; 4]> {
        if let Some(t) = self.bounds.get(&d) {
            return *t;
        }
        let t = self.call_bound(d).or_else(|| self.extreme_bound(d));
        self.bounds.insert(d, t);
        t
    }

    /// The bound of `min(a, b)` or `max(a, b)` of two bounded values: the declared union's or
    /// intersection's, which are exactly those.
    fn extreme_bound(&mut self, d: u32) -> Option<[f32; 4]> {
        let def = *self.defs.get(&d)?;
        let name = match def.kind {
            TraceDefKind::Min => "sdf_union",
            TraceDefKind::Max => "sdf_intersection",
            _ => return None,
        };
        let op = DECLARED.iter().position(|(n, _)| *n == name)?;
        let (a, b) = (def.args[0], def.args[1]);
        let v = [self.bound_of(a)?, self.bound_of(b)?];
        Some(run_declared(op, self.ball, [self.value(a), self.value(b), 0.0, 0.0], v))
    }

    fn call_bound(&mut self, d: u32) -> Option<[f32; 4]> {
        let call = *self.calls.iter().rev().find(|c| c.value == d)?;
        // a call inside a branch: its values are not this block's
        let seen = |a: &TraceArg| match a.kind {
            TraceArgKind::Literal => true,
            TraceArgKind::Float | TraceArgKind::Vector | TraceArgKind::Array => a.lanes.iter().all(|v| self.defs.contains_key(v)),
            TraceArgKind::Other => false,
        };
        if !self.defs.contains_key(&d) || !call.args.iter().all(seen) {
            return None;
        }
        let (op, params) = DECLARED.iter().enumerate().find_map(|(op, (n, p))| (*n == call.name).then_some((op, *p)))?;
        let arg = |name: &str| call.params.iter().position(|n| *n == name).map(|k| call.args[k]);
        // a leaf: its point's affine map from the field's, the map's stretch and how far its
        // rounding puts the point
        let mut leaf = None;
        if let Some(a) = arg("p") {
            let (TraceArgKind::Vector, &[x, y, z]) = (a.kind, a.lanes) else { return None };
            let rows = [self.affine(x)?, self.affine(y)?, self.affine(z)?];
            leaf = Some((stretch(&rows), rounding(&rows)));
        }
        let mut ball = self.ball;
        if let Some((s, dn)) = leaf {
            if let Some(s) = s {
                ball *= s;
            }
            if let Some(dn) = dn {
                ball += dn;
            }
        }
        let (mut s, mut v) = ([0.0f32; 4], [[0.0f32; 4]; 2]);
        for (name, slot) in params {
            let a = arg(name)?;
            match *slot {
                Number(k) => {
                    if !self.independent(&a) {
                        return None;
                    }
                    s[k] = match (a.kind, a.lanes) {
                        (TraceArgKind::Literal, _) => a.literal as f32,
                        (TraceArgKind::Float, &[x]) => self.value(x),
                        _ => return None,
                    };
                }
                Vector(k) => {
                    let (TraceArgKind::Vector, &[x, y, z]) = (a.kind, a.lanes) else { return None };
                    // the point may depend on the field's point, nothing else
                    if *name != "p" && !self.independent(&a) {
                        return None;
                    }
                    v[k] = [self.value(x), self.value(y), self.value(z), 0.0];
                }
                Operand(k, b) => {
                    let (TraceArgKind::Float, &[x]) = (a.kind, a.lanes) else { return None };
                    s[k] = self.value(x);
                    v[b] = self.bound_of(x)?;
                }
                Free(k) => {
                    if !self.independent(&a) {
                        return None;
                    }
                    s[k] = match a.kind {
                        TraceArgKind::Literal => (a.literal as f32).abs(),
                        _ => a.lanes.iter().fold(0.0, |m, x| max(m, self.value(*x).abs())),
                    };
                    // a NaN or an infinity bounds nothing
                    if !s[k].is_finite() {
                        return None;
                    }
                }
            }
        }
        let [mut kp, mut kn, mut l, mut e] = run_declared(op, ball, s, v);
        let Some((s, dn)) = leaf else { return Some([kp, kn, l, e]) };
        // the leaf in the field's frame: e + L d, then K s^2, L s
        if let Some(dn) = dn {
            e += l * dn;
        }
        if let Some(s) = s {
            let s2 = s * s;
            kp = min(kp * s2, NONE);
            kn = min(kn * s2, NONE);
            l = min(l * s, NONE);
        }
        Some([kp, kn, l, e])
    }

    /// `v` as an affine map of the field's point (f32 values; see [`Aff`]), or None.
    fn affine(&mut self, v: u32) -> Option<Aff> {
        if let Some(a) = self.maps.get(&v) {
            return *a;
        }
        let a = self.affine_of(v);
        self.maps.insert(v, a);
        a
    }

    fn affine_of(&mut self, v: u32) -> Option<Aff> {
        let def = *self.defs.get(&v)?;
        if !def.float {
            return None;
        }
        if let Some(c) = self.point.iter().position(|p| *p == v) {
            let mut j = [None; 3];
            j[c] = Some(Coef { v: 1.0, constant: true });
            return Some(Aff { j, m: self.value(v).abs() + self.ball, e: None });
        }
        if !self.depends(v) {
            return Some(Aff { j: [None; 3], m: self.value(v).abs(), e: None });
        }
        match def.kind {
            TraceDefKind::Neg => {
                let a = self.affine(def.args[0])?;
                Some(Aff { j: a.j.map(|c| c.map(|c| Coef { v: -c.v, ..c })), ..a })
            }
            TraceDefKind::Add | TraceDefKind::Sub => {
                let add = def.kind == TraceDefKind::Add;
                let (x, y) = (self.affine(def.args[0])?, self.affine(def.args[1])?);
                let mut j = [None; 3];
                for k in 0..3 {
                    j[k] = match (x.j[k], y.j[k]) {
                        (None, None) => None,
                        (Some(a), None) => Some(a),
                        (None, Some(c)) => Some(if add { c } else { Coef { v: -c.v, ..c } }),
                        (Some(a), Some(c)) => Some(Coef { v: if add { a.v + c.v } else { a.v - c.v }, constant: a.constant && c.constant }),
                    };
                }
                let m = x.m + y.m;
                // the sum's rounding: half an ulp of what it spans, at most
                let r = m * U;
                let e = match (x.e, y.e) {
                    (None, None) => r,
                    (Some(a), None) | (None, Some(a)) => a + r,
                    (Some(a), Some(c)) => a + c + r,
                };
                Some(Aff { j, m, e: Some(e) })
            }
            TraceDefKind::Mul | TraceDefKind::Div => {
                let mul = def.kind == TraceDefKind::Mul;
                let (x, y) = (def.args[0], def.args[1]);
                // one side free of the point (a quotient's divisor)
                let (a, s) = if !self.depends(y) {
                    (x, y)
                } else if mul && !self.depends(x) {
                    (y, x)
                } else {
                    return None;
                };
                let a = self.affine(a)?;
                let constant = self.defs.get(&s).is_some_and(|d| d.kind == TraceDefKind::Const);
                let s = self.value(s);
                let by = |x: f32, s: f32| if mul { x * s } else { x / s };
                let j = a.j.map(|c| c.map(|c| Coef { v: by(c.v, s), constant: c.constant && constant }));
                let sa = s.abs();
                let m = by(a.m, sa);
                let r = m * U;
                let e = match a.e {
                    None => r,
                    Some(ae) => by(ae, sa) + r,
                };
                Some(Aff { j, m, e: Some(e) })
            }
            _ => None,
        }
    }
}

/// A bound on the map's stretch |M| (rounded up for its own f32 arithmetic): the least of its
/// Frobenius norm and sqrt(|M|_1 |M|_inf); None: exactly 1 (a permutation with signs).
fn stretch(rows: &[Aff; 3]) -> Option<f32> {
    let sum = |s: Option<Coef>, x: Coef| Some(match s {
        Some(s) => Coef { v: s.v + x.v, constant: s.constant && x.constant },
        None => x,
    });
    let most = |s: Option<Coef>, x: Coef| Some(match s {
        Some(s) => Coef { v: max(s.v, x.v), constant: s.constant && x.constant },
        None => x,
    });
    let (mut sq, mut rmax, mut cols) = (None, None, [None; 3]);
    for r in rows {
        let mut rs = None;
        for (k, c) in r.j.iter().enumerate() {
            let Some(c) = *c else { continue };
            let a = Coef { v: c.v.abs(), ..c };
            rs = sum(rs, a);
            cols[k] = sum(cols[k], a);
            sq = sum(sq, Coef { v: c.v * c.v, ..c });
        }
        if let Some(rs) = rs {
            rmax = most(rmax, rs);
        }
    }
    let (Some(sq), Some(rmax)) = (sq, rmax) else { return Some(0.0) };
    let cmax = cols.into_iter().flatten().fold(None, most).unwrap_or(rmax);
    let one = |c: Coef| c.constant && c.v == 1.0;
    if one(rmax) && one(cmax) {
        return None;
    }
    Some(min((rmax.v * cmax.v).sqrt(), sq.v.sqrt()) * (1.0 + 64.0 * U))
}

/// How far the map's f32 arithmetic can put its point from the exact map's (rounded up); None:
/// nowhere (exact).
fn rounding(rows: &[Aff; 3]) -> Option<f32> {
    let sum = rows.iter().filter_map(|r| r.e).map(|e| e * e).reduce(|s, q| s + q)?;
    Some(sum.sqrt() * (1.0 + 64.0 * U))
}

/// Declared function `op` at these arguments: (K+, K-, L, e).
fn run_declared(op: usize, ball: f32, s: [f32; 4], v: [[f32; 4]; 2]) -> [f32; 4] {
    let mut out = [0.0f32; 4];
    let head = [op as f32, ball];
    let mut call = bound_call().call();
    call.input("head", &head).expect("a declared bound's function");
    call.input("s", &s).expect("a declared bound's numbers");
    call.input("v0", &v[0]).expect("a declared bound's first vector");
    call.input("v1", &v[1]).expect("a declared bound's second vector");
    call.output("out", &mut out).expect("a declared bound's result");
    call.set_work_limit(u64::MAX);
    call.run(1).expect("the declared bounds' kernel runs");
    out
}

//! Forward-mode derivatives of a field's program: each value that depends on the point gets its
//! three partials by the point's coordinates, defined beside it in the same block, so the program
//! keeps its shape (its branches, loops and the lanes it runs at) and gives the value and its
//! gradient in one pass.
//!
//! The rules are the ops' own: a sum's partials are the sums of its operands', a product's by the
//! product rule, a min or max takes the partials of the operand it picks and a select those of the
//! side it takes, so at a crease the gradient is the one of the side the value comes from. What
//! rounds to an integer (floor, ceil, a conversion) has none. A partial known to be 0 or a constant
//! stays one and emits nothing, so a transform's 0 and 1 terms cost nothing here either.
//!
//! Where values leave the registers they keep their partials: a variable gets three of its own and
//! the frame (a kernel's local arrays) three more frames' words, written beside each word. What the
//! host answers (a leaf) is told the partials of its point and answers its value's.

use makepad_script_compute::ir::{Bin, Block, Cmp, Fma, Op, Program, Region, Stmt, Ty, Un, Val, Var};
use std::collections::HashMap;

/// Where the partials start and where the host's leaves take and give theirs.
pub(crate) struct Seeds {
    /// The buffer whose words `base` 0, 1 and 2 are the point's coordinates.
    pub points: u8,
    pub leaves: Option<LeafSeeds>,
}

/// A leaf's point is written at word `site * 3 + c` of `at` (offset `at_off`), its value read at
/// word `site` of `value`. Beside them the code writes the point's partials at word
/// `site * 9 + k * 3 + c` of `tangent` (offset `tangent_off`) and reads the value's at word
/// `site * 3 + k` of `grad` (offset `at_off`), k the coordinate they are by.
#[derive(Clone, Copy)]
pub(crate) struct LeafSeeds {
    pub at: u8,
    pub value: u8,
    pub tangent: u8,
    pub grad: u8,
    pub at_off: Val,
    pub tangent_off: Val,
}

/// A partial: known to be 0, a known number, or a value of the program.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum D {
    Zero,
    Const(f64),
    V(Val),
}

/// The program's partials, as [`forward`] made them.
pub(crate) struct Partials {
    d: HashMap<u32, [D; 3]>,
}

impl Partials {
    /// The partials of `v` (0 for a value that does not depend on the point).
    pub fn of(&self, v: Val) -> [D; 3] {
        self.d.get(&v.0).copied().unwrap_or([D::Zero; 3])
    }
}

/// `body` with every value's partials beside it, `p`'s values, variables and frame grown for them.
pub(crate) fn forward(p: &mut Program, body: &[Stmt], seeds: &Seeds) -> (Block, Partials) {
    let active = Active::of(p, body, seeds);
    let frame = p.frame_words;
    let mut f = Fwd { p, seeds, frame, active, d: HashMap::new(), vars: HashMap::new(), out: Vec::new() };
    if f.active.frame {
        f.p.frame_words = frame * 4;
    }
    let out = f.block(body);
    (out, Partials { d: f.d })
}

/// Which variables, and whether the frame, carry a value that depends on the point: what needs
/// partials of its own before the code that reads them is reached.
struct Active {
    vals: Vec<bool>,
    vars: Vec<bool>,
    frame: bool,
}

fn float(t: Ty) -> bool {
    matches!(t, Ty::F32 | Ty::F64)
}

impl Active {
    fn of(p: &Program, body: &[Stmt], seeds: &Seeds) -> Active {
        let mut a = Active { vals: vec![false; p.vals.len()], vars: vec![false; p.vars.len()], frame: false };
        // a loop can carry a value round to code before it: to a fixpoint
        while a.pass(p, body, seeds) {}
        a
    }

    /// One walk; whether anything changed.
    fn pass(&mut self, p: &Program, b: &[Stmt], seeds: &Seeds) -> bool {
        let mut changed = false;
        for s in b {
            match s {
                Stmt::Def(v, op) => {
                    let on = float(p.vals[v.0 as usize]) && self.op(op, seeds);
                    if on && !self.vals[v.0 as usize] {
                        self.vals[v.0 as usize] = true;
                        changed = true;
                    }
                }
                Stmt::Set(r, x) if self.vals[x.0 as usize] && !self.vars[r.0 as usize] => {
                    self.vars[r.0 as usize] = true;
                    changed = true;
                }
                Stmt::Store { region: Region::Frame, val, .. } if self.vals[val.0 as usize] && !self.frame => {
                    self.frame = true;
                    changed = true;
                }
                Stmt::If(_, t, e) => {
                    changed |= self.pass(p, t, seeds);
                    changed |= self.pass(p, e, seeds);
                }
                Stmt::Loop { body, .. } => changed |= self.pass(p, body, seeds),
                _ => {}
            }
        }
        changed
    }

    fn op(&self, op: &Op, seeds: &Seeds) -> bool {
        let on = |v: &Val| self.vals[v.0 as usize];
        match op {
            Op::Get(r) => self.vars[r.0 as usize],
            Op::Un(u, a) => carries(*u) && on(a),
            Op::Bin(b, x, y) => matches!(b, Bin::AddF | Bin::SubF | Bin::MulF | Bin::DivF | Bin::MinF | Bin::MaxF | Bin::AddD | Bin::SubD | Bin::MulD | Bin::DivD | Bin::MinD | Bin::MaxD) && (on(x) || on(y)),
            Op::Sel(_, x, y) => on(x) || on(y),
            Op::Fma(k, a, b, c) => *k != Fma::MulAddI && (on(a) || on(b) || on(c)),
            Op::Load { region: Region::Buf(b), base, .. } => (*b == seeds.points && *base < 3) || seeds.leaves.is_some_and(|l| *b == l.value),
            Op::Load { region: Region::Frame, .. } => self.frame,
            _ => false,
        }
    }
}

/// Whether a unary op passes partials on (a rounding or a conversion to or from an integer
/// does not).
fn carries(u: Un) -> bool {
    matches!(u, Un::NegF | Un::AbsF | Un::SqrtF | Un::NegD | Un::AbsD | Un::SqrtD | Un::F2D | Un::D2F)
}

struct Fwd<'a> {
    p: &'a mut Program,
    seeds: &'a Seeds,
    /// The frame's own words: its partials' come after, a frame's worth per coordinate.
    frame: u32,
    active: Active,
    d: HashMap<u32, [D; 3]>,
    vars: HashMap<u32, [Var; 3]>,
    out: Block,
}

impl Fwd<'_> {
    fn block(&mut self, b: &[Stmt]) -> Block {
        let outer = std::mem::take(&mut self.out);
        for s in b {
            self.stmt(s);
        }
        std::mem::replace(&mut self.out, outer)
    }

    fn def(&mut self, ty: Ty, op: Op) -> Val {
        self.p.vals.push(ty);
        let v = Val(self.p.vals.len() as u32 - 1);
        self.out.push(Stmt::Def(v, op));
        v
    }

    fn ty(&self, v: Val) -> Ty {
        self.p.vals[v.0 as usize]
    }

    fn konst(&mut self, ty: Ty, c: f64) -> Val {
        if ty == Ty::F64 { self.def(ty, Op::ConstD(c)) } else { self.def(ty, Op::ConstF(c as f32)) }
    }

    fn val(&mut self, ty: Ty, d: D) -> Val {
        match d {
            D::Zero => self.konst(ty, 0.0),
            D::Const(c) => self.konst(ty, c),
            D::V(v) => v,
        }
    }

    fn bin(&mut self, ty: Ty, f: Bin, d: Bin, x: Val, y: Val) -> Val {
        self.def(ty, Op::Bin(if ty == Ty::F64 { d } else { f }, x, y))
    }

    fn add(&mut self, ty: Ty, a: D, b: D) -> D {
        match (a, b) {
            (D::Zero, x) | (x, D::Zero) => x,
            (D::Const(x), D::Const(y)) => D::Const(x + y),
            _ => {
                let (x, y) = (self.val(ty, a), self.val(ty, b));
                D::V(self.bin(ty, Bin::AddF, Bin::AddD, x, y))
            }
        }
    }

    fn neg(&mut self, ty: Ty, a: D) -> D {
        match a {
            D::Zero => D::Zero,
            D::Const(c) => D::Const(-c),
            D::V(v) => D::V(self.def(ty, Op::Un(if ty == Ty::F64 { Un::NegD } else { Un::NegF }, v))),
        }
    }

    fn sub(&mut self, ty: Ty, a: D, b: D) -> D {
        match (a, b) {
            (x, D::Zero) => x,
            (D::Zero, y) => self.neg(ty, y),
            (D::Const(x), D::Const(y)) => D::Const(x - y),
            _ => {
                let (x, y) = (self.val(ty, a), self.val(ty, b));
                D::V(self.bin(ty, Bin::SubF, Bin::SubD, x, y))
            }
        }
    }

    /// A partial times a value of the program.
    fn scale(&mut self, ty: Ty, a: D, by: Val) -> D {
        match a {
            D::Zero => D::Zero,
            D::Const(c) if c == 1.0 => D::V(by),
            D::Const(c) if c == -1.0 => self.neg(ty, D::V(by)),
            _ => {
                let x = self.val(ty, a);
                D::V(self.bin(ty, Bin::MulF, Bin::MulD, x, by))
            }
        }
    }

    fn over(&mut self, ty: Ty, a: D, by: Val) -> D {
        match a {
            D::Zero => D::Zero,
            _ => {
                let x = self.val(ty, a);
                D::V(self.bin(ty, Bin::DivF, Bin::DivD, x, by))
            }
        }
    }

    fn pick(&mut self, ty: Ty, c: Val, a: D, b: D) -> D {
        if a == b {
            return a;
        }
        let (x, y) = (self.val(ty, a), self.val(ty, b));
        D::V(self.def(ty, Op::Sel(c, x, y)))
    }

    fn cmp(&mut self, ty: Ty, c: Cmp, x: Val, y: Val) -> Val {
        self.def(Ty::Bool, if ty == Ty::F64 { Op::CmpD(c, x, y) } else { Op::CmpF(c, x, y) })
    }

    fn of(&self, v: Val) -> [D; 3] {
        self.d.get(&v.0).copied().unwrap_or([D::Zero; 3])
    }

    fn shadows(&mut self, r: Var) -> [Var; 3] {
        if let Some(s) = self.vars.get(&r.0) {
            return *s;
        }
        let ty = self.p.vars[r.0 as usize];
        let s = std::array::from_fn(|_| {
            self.p.vars.push(ty);
            Var(self.p.vars.len() as u32 - 1)
        });
        self.vars.insert(r.0, s);
        s
    }

    fn stmt(&mut self, s: &Stmt) {
        match s {
            Stmt::Def(v, op) => {
                self.out.push(s.clone());
                let ty = self.ty(*v);
                if !float(ty) {
                    return;
                }
                let d = self.partials(*v, ty, op);
                if d != [D::Zero; 3] {
                    self.d.insert(v.0, d);
                }
            }
            Stmt::Set(r, x) => {
                self.out.push(s.clone());
                if self.active.vars[r.0 as usize] {
                    let ty = self.p.vars[r.0 as usize];
                    let shadow = self.shadows(*r);
                    let d = self.of(*x);
                    for k in 0..3 {
                        let x = self.val(ty, d[k]);
                        self.out.push(Stmt::Set(shadow[k], x));
                    }
                }
            }
            Stmt::Store { region: Region::Frame, base, extent, off, val } => {
                self.out.push(s.clone());
                let ty = self.ty(*val);
                if self.active.frame && float(ty) {
                    let d = self.of(*val);
                    for k in 0..3 {
                        let x = self.val(ty, d[k]);
                        self.out.push(Stmt::Store { region: Region::Frame, base: base + self.frame * (k as u32 + 1), extent: *extent, off: *off, val: x });
                    }
                }
            }
            Stmt::Store { region: Region::Buf(b), base, val, .. } if self.seeds.leaves.is_some_and(|l| l.at == *b) => {
                self.out.push(s.clone());
                let l = self.seeds.leaves.unwrap();
                let ty = self.ty(*val);
                let d = self.of(*val);
                let (site, c) = (base / 3, base % 3);
                for k in 0..3u32 {
                    let x = self.val(ty, d[k as usize]);
                    self.out.push(Stmt::Store { region: Region::Buf(l.tangent), base: site * 9 + k * 3 + c, extent: u32::MAX, off: Some(l.tangent_off), val: x });
                }
            }
            Stmt::If(c, t, e) => {
                let (t, e) = (self.block(t), self.block(e));
                self.out.push(Stmt::If(*c, t, e));
            }
            Stmt::Loop { cap, body } => {
                let body = self.block(body);
                self.out.push(Stmt::Loop { cap: *cap, body });
            }
            _ => self.out.push(s.clone()),
        }
    }

    /// The partials of `v`, defined by `op`, emitted after it.
    fn partials(&mut self, v: Val, ty: Ty, op: &Op) -> [D; 3] {
        let zero = [D::Zero; 3];
        match *op {
            Op::Get(r) => {
                if !self.active.vars[r.0 as usize] {
                    return zero;
                }
                let shadow = self.shadows(r);
                std::array::from_fn(|k| D::V(self.def(ty, Op::Get(shadow[k]))))
            }
            Op::Un(u, a) => {
                let da = self.of(a);
                if da == zero {
                    return zero;
                }
                match u {
                    Un::NegF | Un::NegD => da.map(|d| self.neg(ty, d)),
                    Un::AbsF | Un::AbsD => {
                        let z = self.konst(ty, 0.0);
                        let below = self.cmp(ty, Cmp::Lt, a, z);
                        da.map(|d| {
                            let n = self.neg(ty, d);
                            self.pick(ty, below, n, d)
                        })
                    }
                    // d sqrt(a) = da / (2 sqrt(a)), none where the root is 0
                    Un::SqrtF | Un::SqrtD => {
                        let (z, half) = (self.konst(ty, 0.0), self.konst(ty, 0.5));
                        let up = self.cmp(ty, Cmp::Gt, v, z);
                        let k = self.bin(ty, Bin::DivF, Bin::DivD, half, v);
                        let k = self.def(ty, Op::Sel(up, k, z));
                        da.map(|d| self.scale(ty, d, k))
                    }
                    Un::F2D | Un::D2F => da.map(|d| match d {
                        D::Zero => D::Zero,
                        D::Const(c) => D::Const(c),
                        D::V(x) => D::V(self.def(ty, Op::Un(u, x))),
                    }),
                    _ => zero,
                }
            }
            Op::Bin(b, x, y) => {
                let (dx, dy) = (self.of(x), self.of(y));
                if dx == zero && dy == zero {
                    return zero;
                }
                match b {
                    Bin::AddF | Bin::AddD => std::array::from_fn(|k| self.add(ty, dx[k], dy[k])),
                    Bin::SubF | Bin::SubD => std::array::from_fn(|k| self.sub(ty, dx[k], dy[k])),
                    Bin::MulF | Bin::MulD => std::array::from_fn(|k| {
                        let (a, c) = (self.scale(ty, dx[k], y), self.scale(ty, dy[k], x));
                        self.add(ty, a, c)
                    }),
                    // d (x / y) = (dx - v dy) / y
                    Bin::DivF | Bin::DivD => std::array::from_fn(|k| {
                        let t = self.scale(ty, dy[k], v);
                        let t = self.sub(ty, dx[k], t);
                        self.over(ty, t, y)
                    }),
                    // the side the value comes from; at a tie the side that moves when the other is
                    // constant: taking the constant there drops the slope of every such term at once
                    // (on a box's face `max(q, 0)` and `min(max(q), 0)` both tie at 0: no gradient)
                    Bin::MinF | Bin::MinD | Bin::MaxF | Bin::MaxD => {
                        let tie_x = dy == zero;
                        let c = match (matches!(b, Bin::MinF | Bin::MinD), tie_x) {
                            (true, false) => Cmp::Lt,
                            (true, true) => Cmp::Le,
                            (false, false) => Cmp::Gt,
                            (false, true) => Cmp::Ge,
                        };
                        let c = self.cmp(ty, c, x, y);
                        std::array::from_fn(|k| self.pick(ty, c, dx[k], dy[k]))
                    }
                    _ => zero,
                }
            }
            Op::Sel(c, x, y) => {
                let (dx, dy) = (self.of(x), self.of(y));
                std::array::from_fn(|k| self.pick(ty, c, dx[k], dy[k]))
            }
            Op::Fma(f, a, b, c) => {
                if f == Fma::MulAddI {
                    return zero;
                }
                let (da, db, dc) = (self.of(a), self.of(b), self.of(c));
                std::array::from_fn(|k| {
                    let (m, n) = (self.scale(ty, da[k], b), self.scale(ty, db[k], a));
                    let ab = self.add(ty, m, n);
                    match f {
                        Fma::Add => self.add(ty, ab, dc[k]),
                        Fma::SubFrom => self.sub(ty, dc[k], ab),
                        _ => self.sub(ty, ab, dc[k]),
                    }
                })
            }
            Op::Load { region: Region::Buf(b), base, .. } if b == self.seeds.points && base < 3 => std::array::from_fn(|k| if k as u32 == base { D::Const(1.0) } else { D::Zero }),
            Op::Load { region: Region::Buf(b), base, .. } if self.seeds.leaves.is_some_and(|l| l.value == b) => {
                let l = self.seeds.leaves.unwrap();
                std::array::from_fn(|k| D::V(self.def(ty, Op::Load { region: Region::Buf(l.grad), base: base * 3 + k as u32, extent: u32::MAX, off: Some(l.at_off) })))
            }
            Op::Load { region: Region::Frame, base, extent, off } if self.active.frame => {
                std::array::from_fn(|k| D::V(self.def(ty, Op::Load { region: Region::Frame, base: base + self.frame * (k as u32 + 1), extent, off })))
            }
            _ => zero,
        }
    }
}

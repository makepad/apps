//! Expressions: the one parser and evaluator behind the SPICE reader's constant values
//! (`.param`, brace and quoted values) and the behavioural sources.
//!
//! An expression parses once into a flat list of operations in evaluation order, each naming
//! its operands by their earlier index, so nothing after the parser recurses and the parser's
//! own depth is bounded. A constant expression evaluates with its parameters' values
//! ([`Expr::value`]). A behavioural one compiles ([`Expr::compile`]) to a [`Tape`] over the
//! unknowns it reads: constants folded, unused operations dropped. Each evaluation of the
//! tape gives the value and its exact partial derivatives by forward-mode differentiation,
//! into scratch its owner keeps ([`Scratch`]), so an evaluation allocates nothing.
//!
//! The dialect is ngspice's reading. It is pinned here and never guessed per deck:
//! - Numbers take SPICE scale suffixes (`1k`, `2.2u`, `1meg`). Trailing unit letters are
//!   ignored.
//! - Operators, loosest first:
//!   - `c ? a : b`, right-associative;
//!   - `||`;
//!   - `&&`;
//!   - `==` and `!=`;
//!   - `<`, `<=`, `>` and `>=`;
//!   - `+` and `-`;
//!   - `*` and `/`;
//!   - unary `-`, `+` and `!`;
//!   - `^` and `**`, both power, right-associative and binding tighter than unary minus
//!     (`-2^2` is -4). LTspice reads `^` as exclusive or in its behavioural sources; here
//!     it is always power.
//! - Truth: a comparison or a logical operator gives 1 or 0, and a condition holds when it
//!   is not 0.
//! - Functions of one argument: `sin cos tan asin acos atan sinh cosh tanh exp ln log log10
//!   sqrt abs sgn u uramp floor ceil`.
//!   - `log` is the natural log, as ngspice and LTspice read it.
//!   - `u` is the unit step: 1 above 0, and 0 at and below it.
//!   - `uramp(x)` is `x u(x)`.
//! - Functions of two arguments: `min max pow pwr`. `pwr(x, y)` is `sgn(x) |x|^y`.
//! - `limit(x, lo, hi)` is `lo` below `lo`, `hi` above `hi`, else `x`.
//! - Names:
//!   - `pi`;
//!   - `time`, in seconds (0 at the operating point);
//!   - `temper`, the circuit temperature in °C;
//!   - `v(n)` and `v(a, b)`, node voltages;
//!   - `i(name)`, the branch current of a voltage source or an inductor;
//!   - any other name is a parameter.
//!
//!   Function names and these built-in names are case-insensitive. Parameter, node and
//!   device names are kept as written.
//! - A NaN reaching a comparison, a condition, `min`, `max`, `limit` or a step function
//!   stays a NaN, so a failed operand is never hidden by the branch it chooses. A branch
//!   not taken is never read.
//!
//! Derivatives: a step (`u`, `sgn`, `floor`, `ceil`, a comparison, a logical operator) has
//! slope 0. `abs` has slope 0 at 0. `min`, `max`, `limit` and `c ? a : b` take the slope of the
//! operand they choose; `min` and `max` choose the first operand on a tie.

use crate::spice::{decimal_end, number};
use std::fmt;

/// Expression nesting is parsed by recursion: bounded so a malformed text cannot overflow the
/// stack.
pub const EXPR_DEPTH: usize = 128;

type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Un {
    Neg,
    Not,
    Sin,
    Cos,
    Tan,
    Asin,
    Acos,
    Atan,
    Sinh,
    Cosh,
    Tanh,
    Exp,
    Ln,
    Log10,
    Sqrt,
    Abs,
    Sgn,
    U,
    Uramp,
    Floor,
    Ceil,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Bin {
    Add,
    Sub,
    Mul,
    Div,
    Pow,
    Pwr,
    Min,
    Max,
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
    And,
    Or,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tri {
    Select,
    Limit,
}

/// One operation; operands are indices of earlier operations.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Op {
    Num(f64),
    /// A parameter, by its name's index.
    Param(u32),
    Time,
    Temper,
    /// `v(a)` or `v(a, b)`: node names by index, `NONE` for an absent second node.
    V(u32, u32),
    /// `i(name)`: a device name by index.
    I(u32),
    /// A compiled tape's input, by its index in `Tape::inputs`.
    In(u32),
    Un(Un, u32),
    Bin(Bin, u32, u32),
    Tri(Tri, u32, u32, u32),
}

const NONE: u32 = u32::MAX;

const FUNCTIONS: &[(&str, usize)] = &[
    ("sin", 1),
    ("cos", 1),
    ("tan", 1),
    ("asin", 1),
    ("acos", 1),
    ("atan", 1),
    ("sinh", 1),
    ("cosh", 1),
    ("tanh", 1),
    ("exp", 1),
    ("ln", 1),
    ("log", 1),
    ("log10", 1),
    ("sqrt", 1),
    ("abs", 1),
    ("sgn", 1),
    ("u", 1),
    ("uramp", 1),
    ("floor", 1),
    ("ceil", 1),
    ("min", 2),
    ("max", 2),
    ("pow", 2),
    ("pwr", 2),
    ("limit", 3),
];

/// Whether `name` is a function of the dialect, ignoring case (the reader keeps the commas of
/// its argument list).
pub fn is_function(name: &str) -> bool {
    FUNCTIONS.iter().any(|(f, _)| f.eq_ignore_ascii_case(name))
}

/// A parsed expression: its text as written and its operations.
#[derive(Clone, Debug, PartialEq)]
pub struct Expr {
    text: String,
    ops: Vec<Op>,
    names: Vec<String>,
}

/// What a name in an expression refers to, for [`Expr::compile`] to resolve.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ref<'a> {
    Param(&'a str),
    Node(&'a str),
    /// The device whose branch current `i(name)` reads.
    Branch(&'a str),
}

/// A resolved name: a fixed value (a parameter, the ground node) or an unknown of the solution.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Leaf {
    Value(f64),
    Unknown(u32),
}

/// The values every evaluation reads besides the unknowns.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Env {
    /// Seconds; 0 at the operating point.
    pub time: f64,
    /// The circuit temperature, °C.
    pub temper: f64,
}

impl fmt::Display for Expr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

impl Expr {
    /// Parses `text` (without its braces or quotes).
    pub fn parse(text: &str) -> Result<Expr> {
        let text = text.trim();
        let mut p = Parser { src: text, pos: 0, ops: Vec::new(), names: Vec::new() };
        p.expression(0, 0)?;
        if p.peek().is_some() {
            return Err(format!("unexpected text in expression {text}"));
        }
        Ok(Expr { text: text.into(), ops: p.ops, names: p.names })
    }

    fn name(&self, n: u32) -> &str {
        &self.names[n as usize]
    }

    /// The text as written.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The names of the nodes the expression reads, in order, as often as it reads them.
    pub fn nodes(&self) -> impl Iterator<Item = &str> + '_ {
        self.ops.iter().flat_map(|op| match *op {
            Op::V(a, b) => [Some(a), (b != NONE).then_some(b)],
            _ => [None, None],
        }).flatten().map(|n| self.name(n))
    }

    /// Whether the expression reads no node, branch, time or temperature.
    pub fn is_constant(&self) -> bool {
        !self.ops.iter().any(|op| matches!(op, Op::V(..) | Op::I(_) | Op::Time | Op::Temper))
    }

    /// The constant value, with each parameter's value from `param`. An expression that reads a
    /// node, a branch, the time or the temperature, an unknown parameter, and a result that is
    /// not finite are errors.
    pub fn value(&self, param: &dyn Fn(&str) -> Option<f64>) -> Result<f64> {
        let mut vals = Vec::with_capacity(self.ops.len());
        for op in &self.ops {
            let v = match *op {
                Op::Num(v) => v,
                Op::Param(n) => {
                    let name = &self.names[n as usize];
                    param(name).ok_or_else(|| format!("unknown parameter {name}"))?
                }
                Op::Time | Op::Temper | Op::V(..) | Op::I(_) | Op::In(_) => {
                    return Err(format!("{} is not a constant", self.text));
                }
                op => scalar(op, |o| vals[o as usize]),
            };
            vals.push(v);
        }
        let v = vals.last().copied().unwrap_or(f64::NAN);
        if v.is_finite() {
            Ok(v)
        } else {
            Err("non-finite expression result".into())
        }
    }

    /// Replaces each parameter `param` knows with its value (a subcircuit binds its own).
    pub fn bind(&mut self, param: &dyn Fn(&str) -> Option<f64>) {
        for op in &mut self.ops {
            if let Op::Param(n) = *op {
                if let Some(v) = param(&self.names[n as usize]) {
                    *op = Op::Num(v);
                }
            }
        }
    }

    /// Renames the nodes and devices the expression reads (a subcircuit's instance scope).
    pub fn rename(&mut self, node: &dyn Fn(&str) -> String, device: &dyn Fn(&str) -> String) {
        for op in &self.ops {
            match *op {
                Op::V(a, b) => {
                    for n in [a, b] {
                        if n != NONE {
                            self.names[n as usize] = node(&self.names[n as usize]);
                        }
                    }
                }
                Op::I(d) => self.names[d as usize] = device(&self.names[d as usize]),
                _ => {}
            }
        }
    }

    /// Compiles to a tape: every name resolved by `resolve`, constants folded and operations
    /// that no longer reach the result dropped. The tape's inputs are the distinct unknowns it
    /// reads, in order of first use.
    pub fn compile(&self, resolve: &mut dyn FnMut(Ref) -> Result<Leaf>) -> Result<Tape> {
        let mut ops: Vec<Op> = Vec::with_capacity(self.ops.len());
        let mut inputs: Vec<u32> = Vec::new();
        let mut input_op: Vec<u32> = Vec::new();
        let mut map: Vec<u32> = Vec::with_capacity(self.ops.len());
        let mut leaf = |ops: &mut Vec<Op>, l: Leaf| -> u32 {
            match l {
                Leaf::Value(v) => push(ops, Op::Num(v)),
                Leaf::Unknown(u) => match inputs.iter().position(|&i| i == u) {
                    Some(k) => input_op[k],
                    None => {
                        inputs.push(u);
                        let at = push(ops, Op::In(inputs.len() as u32 - 1));
                        input_op.push(at);
                        at
                    }
                },
            }
        };
        for op in &self.ops {
            let name = |n: u32| self.name(n);
            let at = match *op {
                Op::Num(_) | Op::Time | Op::Temper => push(&mut ops, *op),
                Op::Param(n) => {
                    let l = resolve(Ref::Param(name(n)))?;
                    leaf(&mut ops, l)
                }
                Op::I(d) => {
                    let l = resolve(Ref::Branch(name(d)))?;
                    leaf(&mut ops, l)
                }
                Op::V(a, b) => {
                    let la = resolve(Ref::Node(name(a)))?;
                    let lb = if b == NONE { Leaf::Value(0.0) } else { resolve(Ref::Node(name(b)))? };
                    let ia = leaf(&mut ops, la);
                    if lb == Leaf::Value(0.0) {
                        ia
                    } else {
                        let ib = leaf(&mut ops, lb);
                        fold(&mut ops, Op::Bin(Bin::Sub, ia, ib))
                    }
                }
                Op::In(_) => return Err("an expression is compiled once".into()),
                Op::Un(f, a) => fold(&mut ops, Op::Un(f, map[a as usize])),
                Op::Bin(f, a, b) => fold(&mut ops, Op::Bin(f, map[a as usize], map[b as usize])),
                Op::Tri(f, a, b, c) => fold(&mut ops, Op::Tri(f, map[a as usize], map[b as usize], map[c as usize])),
            };
            map.push(at);
        }
        let root = *map.last().ok_or("empty expression")?;
        Ok(Tape::from_ops(ops, root, inputs))
    }
}

fn push(ops: &mut Vec<Op>, op: Op) -> u32 {
    ops.push(op);
    ops.len() as u32 - 1
}

/// Pushes `op`, or its value when every operand is a number.
fn fold(ops: &mut Vec<Op>, op: Op) -> u32 {
    let num = |i: u32| matches!(ops[i as usize], Op::Num(_));
    let constant = match op {
        Op::Un(_, a) => num(a),
        Op::Bin(_, a, b) => num(a) && num(b),
        // a choice folds when its condition does: the branch not taken may still vary
        Op::Tri(Tri::Select, c, a, b) => {
            if let Op::Num(c) = ops[c as usize] {
                if c.is_nan() {
                    return push(ops, Op::Num(c));
                }
                return if c != 0.0 { a } else { b };
            }
            false
        }
        Op::Tri(_, a, b, c) => num(a) && num(b) && num(c),
        _ => false,
    };
    if constant {
        let v = scalar(op, |o| if let Op::Num(v) = ops[o as usize] { v } else { f64::NAN });
        push(ops, Op::Num(v))
    } else {
        push(ops, op)
    }
}

/// A NaN in any of `xs` (for the operations that would otherwise hide one).
fn nan(xs: &[f64]) -> bool {
    xs.iter().any(|x| x.is_nan())
}

fn truth(b: bool) -> f64 {
    if b {
        1.0
    } else {
        0.0
    }
}

fn sgn(a: f64) -> f64 {
    if a > 0.0 {
        1.0
    } else if a < 0.0 {
        -1.0
    } else {
        a * 0.0
    }
}

fn un(f: Un, a: f64) -> f64 {
    match f {
        Un::Neg => -a,
        Un::Not => if a.is_nan() { a } else { truth(a == 0.0) },
        Un::Sin => a.sin(),
        Un::Cos => a.cos(),
        Un::Tan => a.tan(),
        Un::Asin => a.asin(),
        Un::Acos => a.acos(),
        Un::Atan => a.atan(),
        Un::Sinh => a.sinh(),
        Un::Cosh => a.cosh(),
        Un::Tanh => a.tanh(),
        Un::Exp => a.exp(),
        Un::Ln => a.ln(),
        Un::Log10 => a.log10(),
        Un::Sqrt => a.sqrt(),
        Un::Abs => a.abs(),
        Un::Sgn => sgn(a),
        Un::U => if a.is_nan() { a } else { truth(a > 0.0) },
        Un::Uramp => if a > 0.0 { a } else { a * 0.0 },
        Un::Floor => a.floor(),
        Un::Ceil => a.ceil(),
    }
}

/// d f(a) / da at `a`, with `v` = f(a).
fn un_slope(f: Un, a: f64, v: f64) -> f64 {
    match f {
        Un::Neg => -1.0,
        Un::Sin => a.cos(),
        Un::Cos => -a.sin(),
        Un::Tan => 1.0 + v * v,
        Un::Asin => 1.0 / (1.0 - a * a).sqrt(),
        Un::Acos => -1.0 / (1.0 - a * a).sqrt(),
        Un::Atan => 1.0 / (1.0 + a * a),
        Un::Sinh => a.cosh(),
        Un::Cosh => a.sinh(),
        Un::Tanh => 1.0 - v * v,
        Un::Exp => v,
        Un::Ln => 1.0 / a,
        Un::Log10 => 1.0 / (a * std::f64::consts::LN_10),
        Un::Sqrt => 0.5 / v,
        Un::Abs => sgn(a),
        Un::Uramp => truth(a > 0.0),
        Un::Not | Un::Sgn | Un::U | Un::Floor | Un::Ceil => 0.0,
    }
}

fn bin(f: Bin, a: f64, b: f64) -> f64 {
    let cmp = |r: bool| if nan(&[a, b]) { f64::NAN } else { truth(r) };
    match f {
        Bin::Add => a + b,
        Bin::Sub => a - b,
        Bin::Mul => a * b,
        Bin::Div => a / b,
        Bin::Pow => a.powf(b),
        Bin::Pwr => sgn(a) * a.abs().powf(b),
        Bin::Min => if nan(&[a, b]) { f64::NAN } else if a <= b { a } else { b },
        Bin::Max => if nan(&[a, b]) { f64::NAN } else if a >= b { a } else { b },
        Bin::Lt => cmp(a < b),
        Bin::Le => cmp(a <= b),
        Bin::Gt => cmp(a > b),
        Bin::Ge => cmp(a >= b),
        Bin::Eq => cmp(a == b),
        Bin::Ne => cmp(a != b),
        Bin::And => cmp(a != 0.0 && b != 0.0),
        Bin::Or => cmp(a != 0.0 || b != 0.0),
    }
}

/// (d f / da, d f / db) at (`a`, `b`), with `v` = f(a, b). A slope is read only when its
/// operand varies, so the slope of a fixed operand may be anything (`ln a` for a fixed exponent
/// of a negative base).
fn bin_slopes(f: Bin, a: f64, b: f64, v: f64) -> (f64, f64) {
    match f {
        Bin::Add => (1.0, 1.0),
        Bin::Sub => (1.0, -1.0),
        Bin::Mul => (b, a),
        Bin::Div => (1.0 / b, -v / b),
        Bin::Pow => (b * a.powf(b - 1.0), v * a.ln()),
        Bin::Pwr => (b * a.abs().powf(b - 1.0), v * a.abs().ln()),
        Bin::Min => if a <= b { (1.0, 0.0) } else { (0.0, 1.0) },
        Bin::Max => if a >= b { (1.0, 0.0) } else { (0.0, 1.0) },
        _ => (0.0, 0.0),
    }
}

/// The operand `Tri` chooses, from the values of its three operands.
fn tri_pick(f: Tri, a: f64, b: f64, c: f64) -> Option<u8> {
    match f {
        Tri::Select if a.is_nan() => None,
        Tri::Select => Some(if a != 0.0 { 1 } else { 2 }),
        Tri::Limit if nan(&[a, b, c]) => None,
        Tri::Limit => Some(if a < b { 1 } else if a > c { 2 } else { 0 }),
    }
}

/// An operation's operands, `NONE` past its count.
fn operands(op: Op) -> [u32; 3] {
    match op {
        Op::Un(_, a) => [a, NONE, NONE],
        Op::Bin(_, a, b) => [a, b, NONE],
        Op::Tri(_, a, b, c) => [a, b, c],
        _ => [NONE; 3],
    }
}

/// Which side of a corner its operands are on: the sign of `uramp`'s or `abs`'s operand, whether
/// `min` or `max` chooses its first operand, which of `limit`'s three pieces.
fn side(op: Op, v: impl Fn(u32) -> f64) -> f64 {
    match op {
        Op::Un(Un::Uramp | Un::Abs, a) => sgn(v(a)),
        Op::Bin(Bin::Min, a, b) => truth(v(a) <= v(b)),
        Op::Bin(Bin::Max, a, b) => truth(v(a) >= v(b)),
        Op::Tri(f @ Tri::Limit, a, b, c) => tri_pick(f, v(a), v(b), v(c)).map_or(f64::NAN, f64::from),
        _ => 0.0,
    }
}

/// The value of an operation whose operands' values `v` gives.
fn scalar(op: Op, v: impl Fn(u32) -> f64) -> f64 {
    match op {
        Op::Num(x) => x,
        Op::Un(f, a) => un(f, v(a)),
        Op::Bin(f, a, b) => bin(f, v(a), v(b)),
        Op::Tri(f, a, b, c) => match tri_pick(f, v(a), v(b), v(c)) {
            Some(0) => v(a),
            Some(1) => v(b),
            Some(_) => v(c),
            None => f64::NAN,
        },
        _ => f64::NAN,
    }
}

/// How a step's choice can change between two evaluations, for [`Tape::crossing`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Edge {
    /// A comparison `a < b` (and its kin) of the margin `a - b` with 0, by the operator.
    Margin(Bin),
    /// `u(x)` and `sgn(x)`: the sign of `x`.
    Sign,
    Floor,
    Ceil,
    /// Equality, logical operators, `!` and a choice's condition: a measure-zero change of an
    /// operand, or one that follows a step already watched; no crossing of its own.
    Quiet,
}

/// A compiled expression over the unknowns it reads.
#[derive(Clone, Debug, PartialEq)]
pub struct Tape {
    ops: Vec<Op>,
    inputs: Vec<u32>,
    /// The steps: each operation whose value jumps, with how its choice changes.
    steps: Vec<(u32, Edge)>,
    /// The corners: each operation whose slope jumps (`abs`, `uramp`, `min`, `max`, `limit`).
    corners: Vec<u32>,
    /// The operations the corners' operands need, in order ([`Tape::sides_at`] runs only these).
    corner_ops: Vec<u32>,
    /// Whether an operation reads the time (asked at every bypass check: kept, not searched).
    time: bool,
}

/// An evaluation's working values; sized once for a tape ([`Scratch::fit`]).
#[derive(Clone, Debug, Default)]
pub struct Scratch {
    vals: Vec<f64>,
    ders: Vec<f64>,
}

impl Scratch {
    /// Grows to hold `tape`'s evaluations (never shrinks; one scratch serves several tapes).
    pub fn fit(&mut self, tape: &Tape) {
        let n = tape.ops.len();
        if self.vals.len() < n {
            self.vals.resize(n, 0.0);
        }
        // a rate needs one direction even on a tape without inputs
        let k = tape.inputs.len().max(1);
        if self.ders.len() < n * k {
            self.ders.resize(n * k, 0.0);
        }
    }
}

impl Tape {
    fn from_ops(ops: Vec<Op>, root: u32, inputs: Vec<u32>) -> Tape {
        // keep what reaches the root, in order
        let mut live = vec![false; ops.len()];
        live[root as usize] = true;
        for i in (0..=root as usize).rev() {
            if !live[i] {
                continue;
            }
            match ops[i] {
                Op::Un(_, a) => live[a as usize] = true,
                Op::Bin(_, a, b) => {
                    live[a as usize] = true;
                    live[b as usize] = true;
                }
                Op::Tri(_, a, b, c) => {
                    for o in [a, b, c] {
                        live[o as usize] = true;
                    }
                }
                _ => {}
            }
        }
        let mut map = vec![NONE; ops.len()];
        let mut kept = Vec::new();
        let mut used = vec![NONE; inputs.len()];
        let mut kept_inputs = Vec::new();
        for (i, op) in ops.into_iter().enumerate().take(root as usize + 1) {
            if !live[i] {
                continue;
            }
            let m = |o: u32| map[o as usize];
            let op = match op {
                Op::In(k) => {
                    if used[k as usize] == NONE {
                        used[k as usize] = kept_inputs.len() as u32;
                        kept_inputs.push(inputs[k as usize]);
                    }
                    Op::In(used[k as usize])
                }
                Op::Un(f, a) => Op::Un(f, m(a)),
                Op::Bin(f, a, b) => Op::Bin(f, m(a), m(b)),
                Op::Tri(f, a, b, c) => Op::Tri(f, m(a), m(b), m(c)),
                op => op,
            };
            map[i] = kept.len() as u32;
            kept.push(op);
        }
        let steps = kept
            .iter()
            .enumerate()
            .filter_map(|(i, op)| {
                let edge = match *op {
                    Op::Bin(f @ (Bin::Lt | Bin::Le | Bin::Gt | Bin::Ge), ..) => Edge::Margin(f),
                    Op::Bin(Bin::Eq | Bin::Ne | Bin::And | Bin::Or, ..) => Edge::Quiet,
                    Op::Un(Un::U | Un::Sgn, _) => Edge::Sign,
                    Op::Un(Un::Floor, _) => Edge::Floor,
                    Op::Un(Un::Ceil, _) => Edge::Ceil,
                    Op::Un(Un::Not, _) | Op::Tri(Tri::Select, ..) => Edge::Quiet,
                    _ => return None,
                };
                Some((i as u32, edge))
            })
            .collect();
        let corner = |op: &Op| matches!(op, Op::Un(Un::Abs | Un::Uramp, _) | Op::Bin(Bin::Min | Bin::Max, ..) | Op::Tri(Tri::Limit, ..));
        let corners: Vec<u32> = (0..kept.len() as u32).filter(|&i| corner(&kept[i as usize])).collect();
        let mut need = vec![false; kept.len()];
        for i in (0..kept.len()).rev() {
            if need[i] || corner(&kept[i]) {
                for o in operands(kept[i]).into_iter().filter(|&o| o != NONE) {
                    need[o as usize] = true;
                }
            }
        }
        let corner_ops = (0..kept.len() as u32).filter(|&i| need[i as usize]).collect();
        let time = kept.contains(&Op::Time);
        Tape { ops: kept, inputs: kept_inputs, steps, corners, corner_ops, time }
    }

    /// The unknowns the tape reads, by input index: the order of the partials.
    pub fn inputs(&self) -> &[u32] {
        &self.inputs
    }

    /// Operations per evaluation.
    pub fn len(&self) -> usize {
        self.ops.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    /// The number of steps (operations whose value jumps).
    pub fn steps(&self) -> usize {
        self.steps.len()
    }

    /// The number of corners: operations whose slope jumps (`abs`, `uramp`, `min`, `max`,
    /// `limit`). A line kept at one point holds one side's slope, wrong past a corner by the jump
    /// times the distance.
    pub fn corners(&self) -> usize {
        self.corners.len()
    }

    /// After an evaluation into `s`: the side of each corner its operands are on, into
    /// `out[..corners()]`. Two evaluations are on the same sides exactly when these are equal bit
    /// for bit.
    pub fn sides(&self, s: &Scratch, out: &mut [f64]) {
        for (o, &c) in out.iter_mut().zip(&self.corners) {
            *o = side(self.ops[c as usize], |k| s.vals[k as usize]);
        }
    }

    /// The sides of the corners at the unknowns `x`, as [`Tape::sides`], evaluating only the
    /// operations their operands need (values alone).
    pub fn sides_at(&self, x: &[f64], env: Env, s: &mut Scratch, out: &mut [f64]) {
        let vals = &mut s.vals;
        for &i in &self.corner_ops {
            let v = match self.ops[i as usize] {
                Op::In(j) => x[self.inputs[j as usize] as usize],
                op @ (Op::Un(..) | Op::Bin(..) | Op::Tri(..)) => scalar(op, |o| vals[o as usize]),
                op => leaf_value(op, env),
            };
            vals[i as usize] = v;
        }
        self.sides(s, out);
    }

    /// Whether the value depends on `time`.
    pub fn reads_time(&self) -> bool {
        self.time
    }

    /// The value at the unknowns `x` (indexed by unknown), and each partial derivative by input
    /// into `grad[..inputs().len()]`. `s` must fit this tape.
    pub fn eval(&self, x: &[f64], env: Env, s: &mut Scratch, grad: &mut [f64]) -> f64 {
        let k = self.inputs.len();
        let v = self.forward(x, env, s, k, |op, row| {
            row.fill(0.0);
            if let Op::In(j) = op {
                row[j as usize] = 1.0;
            }
        });
        let last = self.ops.len() - 1;
        grad[..k].copy_from_slice(&s.ders[last * k..last * k + k]);
        v
    }

    /// The value at `x`, and its rate of change as the unknowns move at `xdot` (indexed by
    /// unknown) and the time at `tdot` seconds per second (1 along a transient, 0 at DC): the
    /// total time derivative of the expression along a solution. `s` must fit this tape.
    pub fn rate(&self, x: &[f64], xdot: &[f64], tdot: f64, env: Env, s: &mut Scratch) -> (f64, f64) {
        let v = self.forward(x, env, s, 1, |op, row| {
            row[0] = match op {
                Op::In(j) => xdot[self.inputs[j as usize] as usize],
                Op::Time => tdot,
                _ => 0.0,
            };
        });
        (v, s.ders[self.ops.len() - 1])
    }

    /// One forward pass: each operation's value, and its `k` derivatives by the directions the
    /// leaves are seeded with (`seed` writes a leaf's row: an input, the time, the temperature or
    /// a number).
    fn forward(&self, x: &[f64], env: Env, s: &mut Scratch, k: usize, seed: impl Fn(Op, &mut [f64])) -> f64 {
        let (vals, ders) = (&mut s.vals, &mut s.ders);
        for (i, op) in self.ops.iter().enumerate() {
            let (done, row) = ders.split_at_mut(i * k);
            let (done, row) = (&*done, &mut row[..k]);
            let d = |o: u32| operand(done, o, k);
            match *op {
                Op::In(j) => {
                    vals[i] = x[self.inputs[j as usize] as usize];
                    seed(*op, row);
                }
                Op::Un(f, a) => {
                    let va = vals[a as usize];
                    let v = un(f, va);
                    vals[i] = v;
                    let sa = un_slope(f, va, v);
                    for (r, &da) in row.iter_mut().zip(d(a)) {
                        *r = if da != 0.0 { sa * da } else { 0.0 };
                    }
                }
                Op::Bin(f, a, b) => {
                    let (va, vb) = (vals[a as usize], vals[b as usize]);
                    let v = bin(f, va, vb);
                    vals[i] = v;
                    let (sa, sb) = bin_slopes(f, va, vb, v);
                    for ((r, &da), &db) in row.iter_mut().zip(d(a)).zip(d(b)) {
                        let mut g = 0.0;
                        if da != 0.0 {
                            g += sa * da;
                        }
                        if db != 0.0 {
                            g += sb * db;
                        }
                        *r = g;
                    }
                }
                Op::Tri(f, a, b, c) => {
                    let pick = tri_pick(f, vals[a as usize], vals[b as usize], vals[c as usize]);
                    match pick.map(|p| [a, b, c][p as usize]) {
                        Some(o) => {
                            vals[i] = vals[o as usize];
                            row.copy_from_slice(d(o));
                        }
                        None => {
                            vals[i] = f64::NAN;
                            row.fill(f64::NAN);
                        }
                    }
                }
                op => {
                    vals[i] = leaf_value(op, env);
                    seed(op, row);
                }
            }
        }
        vals[self.ops.len() - 1]
    }

    /// The value alone at the unknowns `x`.
    pub fn value(&self, x: &[f64], env: Env, s: &mut Scratch) -> f64 {
        let vals = &mut s.vals;
        for (i, op) in self.ops.iter().enumerate() {
            let v = match *op {
                Op::In(j) => x[self.inputs[j as usize] as usize],
                op @ (Op::Un(..) | Op::Bin(..) | Op::Tri(..)) => scalar(op, |o| vals[o as usize]),
                op => leaf_value(op, env),
            };
            vals[i] = v;
        }
        vals[self.ops.len() - 1]
    }

    /// After an evaluation into `s`: each step's state into `out[..steps()]`, the value its
    /// choice follows. Two evaluations chose the same branches exactly when their states are
    /// equal bit for bit.
    pub fn branches(&self, s: &Scratch, out: &mut [f64]) {
        for (o, &(i, _)) in out.iter_mut().zip(&self.steps) {
            *o = match self.ops[i as usize] {
                Op::Tri(_, c, ..) => un(Un::Not, s.vals[c as usize]),
                _ => s.vals[i as usize],
            };
        }
    }

    /// After an evaluation into `s`: each step's continuous quantity into `out[..steps()]`
    /// (a comparison's margin `a - b`, the operand of a sign or rounding step), for
    /// [`Tape::crossing`].
    pub fn margins(&self, s: &Scratch, out: &mut [f64]) {
        for (o, &(i, edge)) in out.iter_mut().zip(&self.steps) {
            *o = match (edge, self.ops[i as usize]) {
                (Edge::Margin(_), Op::Bin(_, a, b)) => s.vals[a as usize] - s.vals[b as usize],
                (Edge::Sign | Edge::Floor | Edge::Ceil, Op::Un(_, a)) => s.vals[a as usize],
                _ => 0.0,
            };
        }
    }

    /// The earliest fraction in [0, 1] of the way from one evaluation to another at which a step
    /// changes its choice, interpolating each margin linearly; `m0` and `m1` are their
    /// [`Tape::margins`].
    pub fn crossing(&self, m0: &[f64], m1: &[f64]) -> Option<f64> {
        let mut first: Option<f64> = None;
        for ((&(_, edge), &x0), &x1) in self.steps.iter().zip(m0).zip(m1) {
            let f = match edge {
                Edge::Margin(op) => {
                    let side = |m: f64| bin(op, m, 0.0);
                    (side(x0) != side(x1)).then(|| x0 / (x0 - x1))
                }
                Edge::Sign => (sgn(x0) != sgn(x1)).then(|| x0 / (x0 - x1)),
                Edge::Floor if x0.floor() != x1.floor() => {
                    let edge = if x1 > x0 { x0.floor() + 1.0 } else { x0.floor() };
                    Some((edge - x0) / (x1 - x0))
                }
                Edge::Ceil if x0.ceil() != x1.ceil() => {
                    let edge = if x1 > x0 { x0.ceil() } else { x0.ceil() - 1.0 };
                    Some((edge - x0) / (x1 - x0))
                }
                _ => None,
            };
            if let Some(f) = f.filter(|f| f.is_finite()) {
                let f = f.clamp(0.0, 1.0);
                first = Some(first.map_or(f, |g| g.min(f)));
            }
        }
        first
    }
}

/// The partials of operation `o` among the rows of `k` before the current one.
fn operand(done: &[f64], o: u32, k: usize) -> &[f64] {
    &done[o as usize * k..o as usize * k + k]
}

fn leaf_value(op: Op, env: Env) -> f64 {
    match op {
        Op::Num(v) => v,
        Op::Time => env.time,
        Op::Temper => env.temper,
        _ => f64::NAN,
    }
}

struct Parser<'a> {
    src: &'a str,
    pos: usize,
    ops: Vec<Op>,
    names: Vec<String>,
}

impl Parser<'_> {
    fn peek(&mut self) -> Option<u8> {
        while self.src.as_bytes().get(self.pos).is_some_and(u8::is_ascii_whitespace) {
            self.pos += 1;
        }
        self.src.as_bytes().get(self.pos).copied()
    }

    fn eat(&mut self, b: u8) -> bool {
        if self.peek() == Some(b) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn at(&mut self, s: &str) -> bool {
        self.peek();
        self.src.get(self.pos..).is_some_and(|rest| rest.starts_with(s))
    }

    fn push(&mut self, op: Op) -> u32 {
        push(&mut self.ops, op)
    }

    fn name(&mut self, s: &str) -> u32 {
        self.names.push(s.into());
        self.names.len() as u32 - 1
    }

    /// A node or device name inside `v(...)` or `i(...)`: up to white space, a comma or `)`.
    fn ref_name(&mut self) -> Result<u32> {
        self.peek();
        let start = self.pos;
        while self.src.as_bytes().get(self.pos).is_some_and(|b| !b.is_ascii_whitespace() && !matches!(b, b',' | b')' | b'(')) {
            self.pos += 1;
        }
        let s = self.src.get(start..self.pos).filter(|s| !s.is_empty()).ok_or("missing name in v() or i()")?;
        Ok(self.name(s))
    }

    /// The infix operator at the cursor: (operation, its length, left and right binding power).
    fn infix(&mut self) -> Option<(Option<Bin>, usize, u8, u8)> {
        let two = [
            ("**", Bin::Pow, 17, 16),
            ("<=", Bin::Le, 9, 10),
            (">=", Bin::Ge, 9, 10),
            ("==", Bin::Eq, 7, 8),
            ("!=", Bin::Ne, 7, 8),
            ("&&", Bin::And, 5, 6),
            ("||", Bin::Or, 3, 4),
        ];
        for (s, op, l, r) in two {
            if self.at(s) {
                return Some((Some(op), 2, l, r));
            }
        }
        let (op, l, r) = match self.peek()? {
            b'^' => (Bin::Pow, 17, 16),
            b'*' => (Bin::Mul, 13, 14),
            b'/' => (Bin::Div, 13, 14),
            b'+' => (Bin::Add, 11, 12),
            b'-' => (Bin::Sub, 11, 12),
            b'<' => (Bin::Lt, 9, 10),
            b'>' => (Bin::Gt, 9, 10),
            // the choice: its own operands are parsed below
            b'?' => return Some((None, 1, 2, 2)),
            _ => return None,
        };
        Some((Some(op), 1, l, r))
    }

    fn expression(&mut self, min_bp: u8, depth: usize) -> Result<u32> {
        if depth >= EXPR_DEPTH {
            return Err("expression nesting limit exceeded".into());
        }
        let mut lhs = self.primary(depth)?;
        while let Some((op, len, left, right)) = self.infix() {
            if left < min_bp {
                break;
            }
            self.pos += len;
            lhs = match op {
                Some(op) => {
                    let rhs = self.expression(right, depth + 1)?;
                    self.push(Op::Bin(op, lhs, rhs))
                }
                None => {
                    let a = self.expression(0, depth + 1)?;
                    if !self.eat(b':') {
                        return Err("missing ':' in a choice c ? a : b".into());
                    }
                    let b = self.expression(right, depth + 1)?;
                    self.push(Op::Tri(Tri::Select, lhs, a, b))
                }
            };
        }
        Ok(lhs)
    }

    fn primary(&mut self, depth: usize) -> Result<u32> {
        match self.peek() {
            Some(b'+') => {
                self.pos += 1;
                self.expression(15, depth + 1)
            }
            Some(b'-') => {
                self.pos += 1;
                let a = self.expression(15, depth + 1)?;
                Ok(self.push(Op::Un(Un::Neg, a)))
            }
            Some(b'!') => {
                self.pos += 1;
                let a = self.expression(15, depth + 1)?;
                Ok(self.push(Op::Un(Un::Not, a)))
            }
            Some(b'(') => {
                self.pos += 1;
                let v = self.expression(0, depth + 1)?;
                if !self.eat(b')') {
                    return Err("missing ')' in expression".into());
                }
                Ok(v)
            }
            Some(b'0'..=b'9' | b'.') => {
                let rest = self.src.get(self.pos..).ok_or("invalid expression")?;
                let mut end = decimal_end(rest).ok_or("invalid number in expression")?;
                while rest.as_bytes().get(end).is_some_and(u8::is_ascii_alphabetic) {
                    end += 1;
                }
                let token = rest.get(..end).ok_or("invalid number in expression")?;
                self.pos += end;
                let v = number(token).ok_or_else(|| format!("invalid number {token}"))?;
                Ok(self.push(Op::Num(v)))
            }
            Some(b'a'..=b'z' | b'A'..=b'Z' | b'_') => {
                let start = self.pos;
                while self.src.as_bytes().get(self.pos).is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_' || *b == b'.') {
                    self.pos += 1;
                }
                let name = self.src.get(start..self.pos).ok_or("invalid name")?;
                let lower = name.to_ascii_lowercase();
                if !self.eat(b'(') {
                    return Ok(match lower.as_str() {
                        "pi" => self.push(Op::Num(std::f64::consts::PI)),
                        "time" => self.push(Op::Time),
                        "temper" => self.push(Op::Temper),
                        _ => {
                            let n = self.name(name);
                            self.push(Op::Param(n))
                        }
                    });
                }
                match lower.as_str() {
                    "v" => {
                        let a = self.ref_name()?;
                        let b = if self.eat(b',') { self.ref_name()? } else { NONE };
                        if !self.eat(b')') {
                            return Err("malformed v(): v(node) or v(node, node)".into());
                        }
                        return Ok(self.push(Op::V(a, b)));
                    }
                    "i" => {
                        let d = self.ref_name()?;
                        if !self.eat(b')') {
                            return Err("malformed i(): i(source)".into());
                        }
                        return Ok(self.push(Op::I(d)));
                    }
                    _ => {}
                }
                let mut args = [0u32; 3];
                let mut n = 0;
                loop {
                    let a = self.expression(0, depth + 1)?;
                    if n < args.len() {
                        args[n] = a;
                    }
                    n += 1;
                    if !self.eat(b',') {
                        break;
                    }
                }
                if !self.eat(b')') {
                    return Err(format!("malformed function {lower}"));
                }
                let arity = FUNCTIONS.iter().find(|(f, _)| *f == lower).map(|(_, a)| *a);
                let wrong = || format!("unknown function or wrong argument count: {lower}");
                if arity != Some(n) {
                    return Err(wrong());
                }
                let op = match (lower.as_str(), args) {
                    ("min", [a, b, _]) => Op::Bin(Bin::Min, a, b),
                    ("max", [a, b, _]) => Op::Bin(Bin::Max, a, b),
                    ("pow", [a, b, _]) => Op::Bin(Bin::Pow, a, b),
                    ("pwr", [a, b, _]) => Op::Bin(Bin::Pwr, a, b),
                    ("limit", [a, b, c]) => Op::Tri(Tri::Limit, a, b, c),
                    (f, [a, ..]) => Op::Un(
                        match f {
                            "sin" => Un::Sin,
                            "cos" => Un::Cos,
                            "tan" => Un::Tan,
                            "asin" => Un::Asin,
                            "acos" => Un::Acos,
                            "atan" => Un::Atan,
                            "sinh" => Un::Sinh,
                            "cosh" => Un::Cosh,
                            "tanh" => Un::Tanh,
                            "exp" => Un::Exp,
                            "ln" | "log" => Un::Ln,
                            "log10" => Un::Log10,
                            "sqrt" => Un::Sqrt,
                            "abs" => Un::Abs,
                            "sgn" => Un::Sgn,
                            "u" => Un::U,
                            "uramp" => Un::Uramp,
                            "floor" => Un::Floor,
                            "ceil" => Un::Ceil,
                            _ => return Err(wrong()),
                        },
                        a,
                    ),
                };
                Ok(self.push(op))
            }
            _ => Err("expected expression value".into()),
        }
    }
}

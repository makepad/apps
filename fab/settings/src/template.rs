//! The custom G-code template language of the vendor slicers' profiles, in
//! both dialects (R2 §4): text with `{expression}` macros, `{if}` blocks and
//! legacy `[name]` references.
//!
//! Fab runs the user's own installed start/change/end templates through
//! this interpreter. Templates are parsed once to an AST and evaluated
//! lazily (only the taken branch); `Dialect` selects the few behaviours
//! where the two dialects give different results.

use crate::config::{escape_cstyle, Config, Value};
use crate::options::{options, Kind};
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dialect {
    /// Single-expression macros, raw string literals, implicit float-vector
    /// indexing, enum vectors read as indices (plus `floor`,
    /// `filament_change`).
    Compact,
    /// The full statement language (locals, globals, assignments, escapes).
    Statements,
    /// the statement grammar with the compact dialect's value rules
    Merged,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Val {
    Bool(bool),
    Int(i32),
    Double(f64),
    Str(String),
}

impl Val {
    pub fn to_text(&self) -> String {
        match self {
            Val::Bool(b) => if *b { "true".into() } else { "false".into() },
            Val::Int(i) => i.to_string(),
            Val::Double(d) => fmt_g(*d, 6),
            Val::Str(s) => s.clone(),
        }
    }
    fn is_num(&self) -> bool {
        matches!(self, Val::Int(_) | Val::Double(_))
    }
    fn as_f64(&self) -> Option<f64> {
        match self {
            Val::Int(i) => Some(*i as f64),
            Val::Double(d) => Some(*d),
            _ => None,
        }
    }
}

/// C `%g` with the given precision, as the vendor templates print numbers.
pub fn fmt_g(x: f64, prec: usize) -> String {
    if x.is_nan() {
        return "nan".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "inf".into() } else { "-inf".into() };
    }
    if x == 0.0 {
        return if x.is_sign_negative() { "-0".into() } else { "0".into() };
    }
    let p = prec.max(1);
    // Exponent after rounding to p significant digits.
    let e_str = format!("{:.*e}", p - 1, x);
    let (mant, exp) = e_str.split_once('e').unwrap();
    let exp: i32 = exp.parse().unwrap();
    if exp < -4 || exp >= p as i32 {
        let mut m = mant.to_string();
        if m.contains('.') {
            while m.ends_with('0') {
                m.pop();
            }
            if m.ends_with('.') {
                m.pop();
            }
        }
        let sign = if exp < 0 { '-' } else { '+' };
        format!("{m}e{sign}{:02}", exp.abs())
    } else {
        let decimals = (p as i32 - 1 - exp).max(0) as usize;
        let mut s = format!("{:.*}", decimals, x);
        if s.contains('.') {
            while s.ends_with('0') {
                s.pop();
            }
            if s.ends_with('.') {
                s.pop();
            }
        }
        s
    }
}

/// One element of a template variable: its value inside `{}` and its text
/// as a legacy `[name]` reference.
#[derive(Clone, Debug)]
pub struct Elem {
    pub val: Val,
    pub legacy: String,
}

#[derive(Clone, Debug)]
pub struct TVar {
    pub vector: bool,
    pub elems: Vec<Elem>,
    /// A float vector (eligible for the vendor dialect's implicit indexing).
    pub float_vec: bool,
}

impl TVar {
    pub fn scalar(v: Val) -> Self {
        let legacy = match &v {
            Val::Bool(b) => if *b { "1".into() } else { "0".into() },
            other => other.to_text(),
        };
        Self { vector: false, elems: vec![Elem { val: v, legacy }], float_vec: false }
    }
    pub fn vector(vals: Vec<Val>) -> Self {
        let float_vec = vals.iter().all(|v| matches!(v, Val::Double(_)));
        let elems = vals
            .into_iter()
            .map(|v| {
                let legacy = match &v {
                    Val::Bool(b) => if *b { "1".into() } else { "0".into() },
                    other => other.to_text(),
                };
                Elem { val: v, legacy }
            })
            .collect();
        Self { vector: true, elems, float_vec }
    }
    pub fn f64(x: f64) -> Self {
        Self::scalar(Val::Double(x))
    }
    pub fn int(x: i64) -> Self {
        Self::scalar(Val::Int(x as i32))
    }
    pub fn boolean(x: bool) -> Self {
        Self::scalar(Val::Bool(x))
    }
    pub fn string(x: impl Into<String>) -> Self {
        Self::scalar(Val::Str(x.into()))
    }
    pub fn f64s(x: &[f64]) -> Self {
        Self::vector(x.iter().map(|v| Val::Double(*v)).collect())
    }
    pub fn ints(x: &[i64]) -> Self {
        Self::vector(x.iter().map(|v| Val::Int(*v as i32)).collect())
    }
    pub fn strings(x: &[String]) -> Self {
        Self::vector(x.iter().map(|v| Val::Str(v.clone())).collect())
    }
}

/// Convert a config option into a template variable.
pub fn var_from_config(key: &str, v: &Value, dialect: Dialect) -> TVar {
    let def = options().get(key);
    let kind = def.map(|d| d.kind).unwrap_or(match v {
        Value::Scalar(_) => Kind::Str,
        Value::Vector(_) => Kind::Strs,
    });
    let num = |s: &str| -> f64 {
        let t = s.trim().trim_end_matches('%');
        if t == "nil" {
            f64::NAN
        } else {
            let v = t.parse::<f64>().unwrap_or(0.0);
            if v.is_finite() { v } else { f64::NAN }
        }
    };
    let int = |s: &str| -> i32 {
        let t = s.trim();
        if t == "nil" {
            i32::MAX
        } else {
            t.parse::<f64>().map(|f| if f.is_finite() { f as i32 } else { i32::MAX }).unwrap_or(0)
        }
    };
    let boolv = |s: &str| -> bool { matches!(s.trim(), "1" | "true" | "nil") };
    let elem = |s: &str, k: Kind| -> Elem {
        let val = match k {
            Kind::Float | Kind::Floats | Kind::FloatOrPercent | Kind::FloatsOrPercents => Val::Double(num(s)),
            Kind::Percent => Val::Double(num(s)),
            Kind::Percents => Val::Double(if dialect == Dialect::Statements { num(s) / 100.0 } else { num(s) }),
            Kind::Int | Kind::Ints => Val::Int(int(s)),
            Kind::Bool | Kind::Bools => Val::Bool(boolv(s)),
            Kind::Enums => {
                let idx = def.and_then(|d| d.enum_values.iter().position(|e| e == s)).unwrap_or(0);
                if dialect != Dialect::Statements {
                    Val::Int(idx as i32)
                } else {
                    Val::Str(s.to_string())
                }
            }
            _ => Val::Str(s.to_string()),
        };
        let legacy = match k {
            Kind::Str if dialect != Dialect::Statements => escape_cstyle(s),
            _ => s.to_string(),
        };
        Elem { val, legacy }
    };
    match v {
        Value::Scalar(s) => TVar { vector: false, elems: vec![elem(s, kind)], float_vec: false },
        Value::Vector(items) => TVar {
            vector: true,
            elems: items.iter().map(|s| elem(s, kind)).collect(),
            float_vec: matches!(kind, Kind::Floats | Kind::FloatsOrPercents),
        },
    }
}

/// Where variables come from: per-call overrides, an optional shared base of
/// generator variables, the job config; then writable globals and locals.
pub struct Env<'a> {
    pub config: &'a Config,
    pub vars: HashMap<String, TVar>,
    pub base: Option<&'a HashMap<String, TVar>>,
    pub dialect: Dialect,
    /// The filament id that drives legacy and implicit indexing.
    pub current_extruder_id: usize,
    pub globals: HashMap<String, TVar>,
}

impl<'a> Env<'a> {
    pub fn new(config: &'a Config, dialect: Dialect) -> Self {
        Self { config, vars: HashMap::new(), base: None, dialect, current_extruder_id: 0, globals: HashMap::new() }
    }
    pub fn set(&mut self, name: &str, v: TVar) {
        self.vars.insert(name.to_string(), v);
    }
    fn lookup(&self, name: &str, locals: &HashMap<String, TVar>) -> Option<TVar> {
        if let Some(v) = self.vars.get(name) {
            return Some(v.clone());
        }
        if let Some(v) = self.base.and_then(|base| base.get(name)) {
            return Some(v.clone());
        }
        if let Some(v) = self.config.get(name) {
            return Some(var_from_config(name, v, self.dialect));
        }
        // A local shadows a global of the same name, as assignment already does.
        if let Some(v) = locals.get(name) {
            return Some(v.clone());
        }
        self.globals.get(name).cloned()
    }

    /// Expand a template.
    pub fn render(&mut self, src: &str) -> Result<String, TemplateError> {
        let t = parse(src, self.dialect)?;
        self.render_parsed(&t).map_err(|mut e| {
            e.line = src[..e.pos.min(src.len())].matches('\n').count() + 1;
            e
        })
    }

    pub fn render_parsed(&mut self, t: &Template) -> Result<String, TemplateError> {
        let mut out = String::new();
        let mut locals = HashMap::new();
        self.run(&t.nodes, &mut out, &mut locals)?;
        Ok(out)
    }

    fn run(&mut self, nodes: &[Node], out: &mut String, locals: &mut HashMap<String, TVar>) -> Result<(), TemplateError> {
        for n in nodes {
            match n {
                Node::Text(s) => out.push_str(s),
                Node::Legacy(r) => out.push_str(&self.legacy(r, locals)?),
                Node::Stmts(st) => {
                    for s in st {
                        self.stmt(s, out, locals)?;
                    }
                }
                Node::If(arms, els) => {
                    let mut taken = false;
                    for (c, body) in arms {
                        match self.eval(c, locals)? {
                            Val::Bool(true) => {
                                self.run(body, out, locals)?;
                                taken = true;
                                break;
                            }
                            Val::Bool(false) => {}
                            _ => return Err(TemplateError::at(c.pos, "Not a boolean expression")),
                        }
                    }
                    if !taken {
                        if let Some(e) = els {
                            self.run(e, out, locals)?;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn stmt(&mut self, s: &Stmt, out: &mut String, locals: &mut HashMap<String, TVar>) -> Result<(), TemplateError> {
        match s {
            Stmt::If { arms, els } => {
                for (cond, body) in arms {
                    match self.eval(cond, locals)? {
                        Val::Bool(true) => {
                            for stmt in body {
                                self.stmt(stmt, out, locals)?;
                            }
                            return Ok(());
                        }
                        Val::Bool(false) => {}
                        _ => return Err(TemplateError::at(cond.pos, "Not a boolean expression")),
                    }
                }
                if let Some(body) = els {
                    for stmt in body {
                        self.stmt(stmt, out, locals)?;
                    }
                }
            }
            Stmt::Expr(e) => {
                let v = self.eval(e, locals)?;
                if matches!(v, Val::Double(d) if !d.is_finite()) || matches!(v, Val::Int(i32::MAX)) {
                    return Err(TemplateError::at(e.pos, "a nil or non-finite value reaches the G-code"));
                }
                out.push_str(&v.to_text());
            }
            Stmt::Decl { global, name, rhs, pos } => {
                if self.vars.contains_key(name) || self.base.is_some_and(|base| base.contains_key(name)) || self.config.has(name) {
                    return Err(TemplateError::at(*pos, &format!("Cannot redefine read-only variable {name}")));
                }
                let v = self.rhs(rhs, locals)?;
                if *global {
                    self.globals.insert(name.clone(), v);
                } else {
                    locals.insert(name.clone(), v);
                }
            }
            Stmt::Assign { name, index, rhs, pos } => {
                let v = self.rhs(rhs, locals)?;
                let idx = match index {
                    Some(i) => Some(self.index_of(i, locals)?),
                    None => None,
                };
                let target = if locals.contains_key(name) {
                    locals.get_mut(name)
                } else {
                    self.globals.get_mut(name)
                };
                let Some(t) = target else {
                    return Err(TemplateError::at(*pos, &format!("Cannot modify a read-only variable {name}")));
                };
                match idx {
                    Some(i) => {
                        if i >= t.elems.len() {
                            return Err(TemplateError::at(*pos, "Index out of range"));
                        }
                        let nv = coerce(&t.elems[i].val, v.elems[0].val.clone());
                        t.elems[i] = Elem { legacy: nv.to_text(), val: nv };
                    }
                    None => {
                        if t.vector && v.vector {
                            *t = v;
                        } else {
                            let nv = coerce(&t.elems[0].val, v.elems[0].val.clone());
                            t.elems[0] = Elem { legacy: nv.to_text(), val: nv };
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn rhs(&mut self, r: &Rhs, locals: &mut HashMap<String, TVar>) -> Result<TVar, TemplateError> {
        Ok(match r {
            Rhs::Expr(e) => {
                // A bare vector variable copies the vector.
                if let ExprK::Var(name, None) = &e.k {
                    if let Some(v) = self.lookup(name, locals) {
                        if v.vector {
                            return Ok(v);
                        }
                    }
                }
                TVar::scalar(self.eval(e, locals)?)
            }
            Rhs::List(items) => {
                let mut vals = Vec::new();
                for i in items {
                    vals.push(self.eval(i, locals)?);
                }
                let any_str = vals.iter().any(|v| matches!(v, Val::Str(_)));
                let any_d = vals.iter().any(|v| matches!(v, Val::Double(_)));
                let vals = vals
                    .into_iter()
                    .map(|v| if any_str { Val::Str(v.to_text()) } else if any_d { Val::Double(v.as_f64().unwrap_or(0.0)) } else { v })
                    .collect();
                TVar::vector(vals)
            }
            Rhs::Repeat(n, v) => {
                let n = match self.eval(n, locals)? {
                    Val::Int(i) if i >= 0 => i as usize,
                    _ => return Err(TemplateError::at(0, "repeat count must be a non-negative int")),
                };
                let v = self.eval(v, locals)?;
                TVar::vector(vec![v; n])
            }
        })
    }

    fn index_of(&mut self, e: &Expr, locals: &mut HashMap<String, TVar>) -> Result<usize, TemplateError> {
        match self.eval(e, locals)? {
            Val::Int(i) if i >= 0 => Ok(i as usize),
            Val::Int(_) => Err(TemplateError::at(e.pos, "Negative vector index")),
            _ => Err(TemplateError::at(e.pos, "Non-integer index is not allowed to address a vector variable")),
        }
    }

    fn legacy(&mut self, r: &LegacyRef, locals: &mut HashMap<String, TVar>) -> Result<String, TemplateError> {
        let (var, idx) = match self.lookup(&r.name, locals) {
            Some(v) => {
                let idx = match &r.index {
                    Some(iname) => Some(self.int_var(iname, locals, r.pos)?),
                    None => None,
                };
                (v, idx)
            }
            None => {
                // name_N
                let Some((base, n)) = r.name.rsplit_once('_') else {
                    return Err(TemplateError::at(r.pos, &format!("Variable does not exist: {}", r.name)));
                };
                let Ok(n) = n.parse::<usize>() else {
                    return Err(TemplateError::at(r.pos, &format!("Variable does not exist: {}", r.name)));
                };
                let v = self.lookup(base, locals).ok_or_else(|| TemplateError::at(r.pos, &format!("Variable does not exist: {}", r.name)))?;
                (v, Some(n))
            }
        };
        if var.elems.is_empty() {
            return Err(TemplateError::at(r.pos, &format!("Indexing an empty vector variable {}", r.name)));
        }
        let i = if var.vector { idx.unwrap_or(self.current_extruder_id) } else { 0 };
        let e = var.elems.get(i).unwrap_or(&var.elems[0]);
        if e.legacy.trim() == "nil" || matches!(e.val, Val::Double(d) if !d.is_finite()) || matches!(e.val, Val::Int(i32::MAX)) {
            return Err(TemplateError::at(r.pos, "a nil or non-finite value reaches the G-code"));
        }
        Ok(e.legacy.clone())
    }

    fn int_var(&mut self, name: &str, locals: &HashMap<String, TVar>, pos: usize) -> Result<usize, TemplateError> {
        let v = self.lookup(name, locals).ok_or_else(|| TemplateError::at(pos, &format!("Variable does not exist: {name}")))?;
        match v.elems.first().map(|e| &e.val) {
            Some(Val::Int(i)) if *i >= 0 => Ok(*i as usize),
            _ => Err(TemplateError::at(pos, &format!("Index variable {name} is not a non-negative int"))),
        }
    }

    fn eval(&mut self, e: &Expr, locals: &mut HashMap<String, TVar>) -> Result<Val, TemplateError> {
        let err = |m: &str| TemplateError::at(e.pos, m);
        match &e.k {
            ExprK::Lit(v) => Ok(v.clone()),
            ExprK::Var(name, index) => {
                let v = self.lookup(name, locals).ok_or_else(|| err(&format!("Variable does not exist: {name}")))?;
                match index {
                    Some(ix) => {
                        if !v.vector {
                            return Err(err(&format!("Referencing a scalar variable {name} with an index")));
                        }
                        // Reading out of range gives element 0; the vendor
                        // dialect also reads a negative index that way
                        // (older templates index an unused nozzle's -1).
                        let i = match self.eval(ix, locals)? {
                            Val::Int(i) if i < 0 && self.dialect == Dialect::Compact => usize::MAX,
                            _ => self.index_of(ix, locals)?,
                        };
                        if v.elems.is_empty() {
                            return Err(err(&format!("Indexing an empty vector variable {name}")));
                        }
                        Ok(v.elems.get(i).unwrap_or(&v.elems[0]).val.clone())
                    }
                    None => {
                        if !v.vector {
                            return Ok(v.elems[0].val.clone());
                        }
                        if self.dialect == Dialect::Merged && v.float_vec && !v.elems.is_empty() {
                            let current = self.current_extruder_id;
                            let i = if v.elems.len() == 1 {
                                0
                            } else if options().get(&format!("filament_{name}")).is_some() {
                                current
                            } else {
                                match self.lookup("filament_map", locals) {
                                    None => current,
                                    Some(map) => match map.elems.get(current).map(|e| &e.val) {
                                        Some(Val::Int(i)) if *i > 0 => (*i - 1) as usize,
                                        _ => 0,
                                    },
                                }
                            };
                            return Ok(v.elems.get(i).unwrap_or(&v.elems[0]).val.clone());
                        }
                        if self.dialect == Dialect::Compact && v.float_vec && !v.elems.is_empty() {
                            // The vendor dialect's implicit index: element 0 of a
                            // one-element vector, else the raw 1-based extruder
                            // of the current filament.
                            if v.elems.len() == 1 {
                                return Ok(v.elems[0].val.clone());
                            }
                            let fm = self.lookup("filament_map", locals).and_then(|m| m.elems.get(self.current_extruder_id).map(|e| e.val.clone()));
                            let i = match fm {
                                Some(Val::Int(i)) if i >= 0 => i as usize,
                                _ => 0,
                            };
                            return Ok(v.elems.get(i).unwrap_or(&v.elems[0]).val.clone());
                        }
                        Err(err(&format!("Referencing a vector variable {name} when scalar is expected")))
                    }
                }
            }
            ExprK::Un(op, a) => {
                let v = self.eval(a, locals)?;
                match (op, v) {
                    ('-', Val::Int(i)) => Ok(Val::Int(i.wrapping_neg())),
                    ('-', Val::Double(d)) => Ok(Val::Double(-d)),
                    ('+', v) => Ok(v),
                    ('!', Val::Bool(b)) => Ok(Val::Bool(!b)),
                    _ => Err(err("Invalid unary operand")),
                }
            }
            ExprK::Bin(op, a, b) => {
                let x = self.eval(a, locals)?;
                let y = self.eval(b, locals)?;
                binop(*op, x, y).map_err(|m| err(&m))
            }
            ExprK::Tern(c, a, b) => match self.eval(c, locals)? {
                Val::Bool(true) => self.eval(a, locals),
                Val::Bool(false) => self.eval(b, locals),
                _ => Err(err("Not a boolean expression")),
            },
            ExprK::Call(f, args) => {
                if f == "is_nil" {
                    let Some(Expr { k: ExprK::Var(name, ix), .. }) = args.first() else {
                        return Err(err("is_nil expects a variable"));
                    };
                    let raw = self.config.get(name);
                    let i = match ix {
                        Some(ix) => self.index_of(ix, locals)?,
                        None => 0,
                    };
                    return Ok(Val::Bool(raw.map(|v| v.at(i) == "nil").unwrap_or(false)));
                }
                if f == "size" || f == "empty" {
                    let Some(Expr { k: ExprK::Var(name, None), .. }) = args.first() else {
                        return Err(err("size/empty expect a vector variable"));
                    };
                    let v = self.lookup(name, locals).ok_or_else(|| err(&format!("Variable does not exist: {name}")))?;
                    return Ok(if f == "size" { Val::Int(v.elems.len() as i32) } else { Val::Bool(v.elems.is_empty()) });
                }
                if f == "one_of" {
                    let s = match args.first().map(|a| self.eval(a, locals)).transpose()? {
                        Some(Val::Str(s)) => s,
                        _ => return Err(err("one_of expects a string")),
                    };
                    for a in &args[1..] {
                        let p = self.eval(a, locals)?.to_text();
                        if p == s {
                            return Ok(Val::Bool(true));
                        }
                    }
                    return Ok(Val::Bool(false));
                }
                let mut vals = Vec::with_capacity(args.len());
                for a in args {
                    vals.push(self.eval(a, locals)?);
                }
                call(f, vals, self).map_err(|m| err(&m))
            }
            ExprK::Interp(x, table) => {
                let x = self.eval(x, locals)?.as_f64().ok_or_else(|| err("interpolate_table expects a number"))?;
                let mut pts = Vec::new();
                for (a, b) in table {
                    let a = self.eval(a, locals)?.as_f64().ok_or_else(|| err("interpolate_table expects numbers"))?;
                    let b = self.eval(b, locals)?.as_f64().ok_or_else(|| err("interpolate_table expects numbers"))?;
                    pts.push((a, b));
                }
                interpolate(x, &pts).map(Val::Double).map_err(|m| err(&m))
            }
            ExprK::Match(a, pattern, negate) => {
                let s = match self.eval(a, locals)? {
                    Val::Str(s) => s,
                    _ => return Err(err("Left side of a regular expression match must be a string")),
                };
                let m = crate::regex_lite::full_match(pattern, &s).map_err(|m| err(&m))?;
                Ok(Val::Bool(m != *negate))
            }
        }
    }
}

fn coerce(target: &Val, v: Val) -> Val {
    match target {
        Val::Double(_) => Val::Double(v.as_f64().unwrap_or(0.0)),
        Val::Int(_) => Val::Int(v.as_f64().map(|f| f as i32).unwrap_or(0)),
        Val::Str(_) => Val::Str(v.to_text()),
        Val::Bool(_) => v,
    }
}

fn interpolate(x: f64, pts: &[(f64, f64)]) -> Result<f64, String> {
    if pts.is_empty() {
        return Err("empty interpolate_table".into());
    }
    let eps = 1e-8;
    if x < pts[0].0 - eps || x > pts[pts.len() - 1].0 + eps {
        return Err("interpolate_table: value out of range".into());
    }
    for w in pts.windows(2) {
        let ((x0, y0), (x1, y1)) = (w[0], w[1]);
        if x1 < x0 {
            return Err("interpolate_table: x not increasing".into());
        }
        if x <= x1 + eps {
            if (x1 - x0).abs() < eps {
                return Ok((y0 + y1) * 0.5);
            }
            return Ok(y0 + (y1 - y0) * ((x - x0) / (x1 - x0)).clamp(0.0, 1.0));
        }
    }
    Ok(pts[pts.len() - 1].1)
}

fn binop(op: Op, x: Val, y: Val) -> Result<Val, String> {
    use Op::*;
    match op {
        Add => {
            if matches!(x, Val::Str(_)) || matches!(y, Val::Str(_)) {
                return Ok(Val::Str(x.to_text() + &y.to_text()));
            }
            arith(x, y, |a, b| a.wrapping_add(b), |a, b| a + b)
        }
        Sub => arith(x, y, |a, b| a.wrapping_sub(b), |a, b| a - b),
        Mul => arith(x, y, |a, b| a.wrapping_mul(b), |a, b| a * b),
        Div => {
            if y.as_f64() == Some(0.0) {
                return Err("Division by zero".into());
            }
            if let (Val::Int(a), Val::Int(b)) = (&x, &y) {
                if *a == i32::MIN && *b == -1 {
                    return Err("Integer overflow".into());
                }
                return Ok(Val::Int(a / b));
            }
            arith(x, y, |a, b| a / b, |a, b| a / b)
        }
        Mod => {
            if y.as_f64() == Some(0.0) {
                return Err("Division by zero".into());
            }
            if let (Val::Int(a), Val::Int(b)) = (&x, &y) {
                return Ok(Val::Int(a.wrapping_rem(*b)));
            }
            arith(x, y, |a, b| a % b, |a, b| a % b)
        }
        And | Or => match (x, y) {
            (Val::Bool(a), Val::Bool(b)) => Ok(Val::Bool(if op == And { a && b } else { a || b })),
            _ => Err("Logical operator applied to non-boolean".into()),
        },
        Eq | Ne | Lt | Gt | Le | Ge => {
            let ord = compare(&x, &y, op)?;
            Ok(Val::Bool(ord))
        }
    }
}

fn arith(x: Val, y: Val, fi: impl Fn(i32, i32) -> i32, fd: impl Fn(f64, f64) -> f64) -> Result<Val, String> {
    match (&x, &y) {
        (Val::Int(a), Val::Int(b)) => Ok(Val::Int(fi(*a, *b))),
        _ if x.is_num() && y.is_num() => Ok(Val::Double(fd(x.as_f64().unwrap(), y.as_f64().unwrap()))),
        _ => Err("Non-numeric operand".into()),
    }
}

fn compare(x: &Val, y: &Val, op: Op) -> Result<bool, String> {
    use std::cmp::Ordering;
    let ord: Ordering = match (x, y) {
        (Val::Str(_), _) | (_, Val::Str(_)) => x.to_text().as_bytes().cmp(y.to_text().as_bytes()),
        (Val::Bool(a), Val::Bool(b)) => {
            return match op {
                Op::Eq => Ok(a == b),
                Op::Ne => Ok(a != b),
                _ => Err("Cannot compare the types.".into()),
            }
        }
        (Val::Bool(_), _) | (_, Val::Bool(_)) => return Err("Cannot compare the types.".into()),
        (Val::Int(a), Val::Int(b)) => a.cmp(b),
        _ => {
            let a = x.as_f64().unwrap();
            let b = y.as_f64().unwrap();
            if (a - b).abs() < 1e-8 {
                Ordering::Equal
            } else if a < b {
                Ordering::Less
            } else {
                Ordering::Greater
            }
        }
    };
    Ok(match op {
        Op::Eq => ord == Ordering::Equal,
        Op::Ne => ord != Ordering::Equal,
        Op::Lt => ord == Ordering::Less,
        Op::Gt => ord == Ordering::Greater,
        Op::Le => ord != Ordering::Greater,
        Op::Ge => ord != Ordering::Less,
        _ => unreachable!(),
    })
}

fn call(f: &str, a: Vec<Val>, env: &mut Env) -> Result<Val, String> {
    let num = |v: &Val| v.as_f64().ok_or_else(|| format!("{f}: expects a number"));
    match f {
        "min" | "max" => {
            if a.len() != 2 {
                return Err(format!("{f} takes two arguments"));
            }
            if let (Val::Int(x), Val::Int(y)) = (&a[0], &a[1]) {
                return Ok(Val::Int(if f == "min" { *x.min(y) } else { *x.max(y) }));
            }
            let (x, y) = (num(&a[0])?, num(&a[1])?);
            Ok(Val::Double(if f == "min" { x.min(y) } else { x.max(y) }))
        }
        "int" => Ok(match &a[0] {
            Val::Int(i) => Val::Int(*i),
            v => Val::Int(num(v)?.trunc() as i32),
        }),
        "round" => Ok(Val::Int(num(&a[0])?.round() as i32)),
        "ceil" => Ok(Val::Int(num(&a[0])?.ceil() as i32)),
        "floor" => Ok(Val::Int(num(&a[0])?.floor() as i32)),
        "digits" | "zdigits" => {
            let x = num(&a[0])?;
            let w = match a.get(1) {
                Some(Val::Int(w)) => (*w).clamp(0, 64) as usize,
                _ => return Err(format!("{f}: width must be an int")),
            };
            let z = f == "zdigits";
            Ok(Val::Str(match a.get(2) {
                Some(Val::Int(d)) => {
                    let d = (*d).clamp(0, 64) as usize;
                    if z {
                        format!("{:0w$.d$}", x, w = w, d = d)
                    } else {
                        format!("{:w$.d$}", x, w = w, d = d)
                    }
                }
                None => {
                    let r = x.round() as i64;
                    if z {
                        format!("{:0w$}", r, w = w)
                    } else {
                        format!("{:w$}", r, w = w)
                    }
                }
                _ => return Err(format!("{f}: decimals must be an int")),
            }))
        }
        "filament_change" => {
            let n = match &a[0] {
                Val::Int(i) if *i >= 0 => *i as usize,
                _ => return Err("filament_change expects a non-negative int".into()),
            };
            env.current_extruder_id = n;
            Ok(Val::Str(String::new()))
        }
        "random" => Err("random() needs a seeded context".into()),
        _ => Err(format!("Unknown function {f}")),
    }
}

// ---------------------------------------------------------------- AST

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    And,
    Or,
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
}

#[derive(Clone, Debug)]
pub struct Expr {
    pub k: ExprK,
    pub pos: usize,
}

#[derive(Clone, Debug)]
pub enum ExprK {
    Lit(Val),
    Var(String, Option<Box<Expr>>),
    Un(char, Box<Expr>),
    Bin(Op, Box<Expr>, Box<Expr>),
    Tern(Box<Expr>, Box<Expr>, Box<Expr>),
    Call(String, Vec<Expr>),
    Interp(Box<Expr>, Vec<(Expr, Expr)>),
    Match(Box<Expr>, String, bool),
}

#[derive(Clone, Debug)]
pub enum Rhs {
    Expr(Expr),
    List(Vec<Expr>),
    Repeat(Expr, Expr),
}

#[derive(Clone, Debug)]
pub enum Stmt {
    Expr(Expr),
    If { arms: Vec<(Expr, Vec<Stmt>)>, els: Option<Vec<Stmt>> },
    Decl { global: bool, name: String, rhs: Rhs, pos: usize },
    Assign { name: String, index: Option<Expr>, rhs: Rhs, pos: usize },
}

#[derive(Clone, Debug)]
pub struct LegacyRef {
    pub name: String,
    pub index: Option<String>,
    pub pos: usize,
}

#[derive(Clone, Debug)]
pub enum Node {
    Text(String),
    Legacy(LegacyRef),
    Stmts(Vec<Stmt>),
    If(Vec<(Expr, Vec<Node>)>, Option<Vec<Node>>),
}

#[derive(Clone, Debug)]
pub struct Template {
    pub nodes: Vec<Node>,
}

#[derive(Clone, Debug)]
pub struct TemplateError {
    pub pos: usize,
    pub msg: String,
    pub line: usize,
}

impl TemplateError {
    fn at(pos: usize, msg: &str) -> Self {
        Self { pos, msg: msg.to_string(), line: 0 }
    }
}

impl std::fmt::Display for TemplateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "template error at line {}: {}", self.line, self.msg)
    }
}

// ---------------------------------------------------------------- parser

const KEYWORDS: &[&str] = &[
    "and", "ceil", "digits", "zdigits", "empty", "if", "int", "is_nil", "local", "else", "elsif", "endif", "false", "global", "interpolate_table", "min", "max", "random", "repeat", "round", "not", "one_of", "or", "size", "true", "floor", "filament_change",
];

struct Parser<'s> {
    s: &'s [u8],
    src: &'s str,
    i: usize,
    dialect: Dialect,
}

pub fn parse(src: &str, dialect: Dialect) -> Result<Template, TemplateError> {
    let mut p = Parser { s: src.as_bytes(), src, i: 0, dialect };
    let (nodes, stop) = p.text_block(false).map_err(|e| p.locate(e))?;
    if stop.is_some() {
        return Err(p.locate(TemplateError::at(p.i, "elsif/else/endif without if")));
    }
    Ok(Template { nodes })
}

#[derive(PartialEq, Debug)]
enum Stop {
    Elsif,
    Else,
    Endif,
}

impl<'s> Parser<'s> {
    fn locate(&self, mut e: TemplateError) -> TemplateError {
        e.line = self.src[..e.pos.min(self.src.len())].matches('\n').count() + 1;
        e
    }

    fn err(&self, m: &str) -> TemplateError {
        TemplateError::at(self.i, m)
    }

    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }

    fn ws(&mut self) {
        while let Some(c) = self.peek() {
            if c == b' ' || c == b'\t' || c == b'\n' || c == b'\r' {
                self.i += 1;
            } else {
                break;
            }
        }
    }

    /// Is a keyword at the cursor (followed by a non-identifier char)?
    fn at_kw(&self, kw: &str) -> bool {
        let k = kw.as_bytes();
        if self.s.len() < self.i + k.len() || &self.s[self.i..self.i + k.len()] != k {
            return false;
        }
        match self.s.get(self.i + k.len()) {
            Some(c) => !(c.is_ascii_alphanumeric() || *c == b'_'),
            None => true,
        }
    }

    fn eat(&mut self, tok: &str) -> bool {
        self.ws();
        if self.s[self.i..].starts_with(tok.as_bytes()) {
            self.i += tok.len();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, tok: &str) -> Result<(), TemplateError> {
        if self.eat(tok) {
            Ok(())
        } else {
            Err(self.err(&format!("expected '{tok}'")))
        }
    }

    /// Text until end, or until `{elsif`/`{else`/`{endif` when inside an if.
    fn text_block(&mut self, in_if: bool) -> Result<(Vec<Node>, Option<Stop>), TemplateError> {
        let mut nodes = Vec::new();
        let mut text = String::new();
        while let Some(c) = self.peek() {
            match c {
                b'[' => {
                    if !text.is_empty() {
                        nodes.push(Node::Text(std::mem::take(&mut text)));
                    }
                    nodes.push(self.legacy()?);
                }
                b'{' => {
                    let save = self.i;
                    self.i += 1;
                    self.ws();
                    if in_if {
                        for (kw, st) in [("elsif", Stop::Elsif), ("else", Stop::Else), ("endif", Stop::Endif)] {
                            if self.at_kw(kw) {
                                if !text.is_empty() {
                                    nodes.push(Node::Text(std::mem::take(&mut text)));
                                }
                                self.i += kw.len();
                                return Ok((nodes, Some(st)));
                            }
                        }
                    }
                    if self.at_kw("if") {
                        if !text.is_empty() {
                            nodes.push(Node::Text(std::mem::take(&mut text)));
                        }
                        let start = self.i;
                        self.i += 2;
                        let statement_if = if self.dialect != Dialect::Compact {
                            self.expr()?;
                            self.ws();
                            self.at_kw("then")
                        } else {
                            false
                        };
                        if !statement_if {
                            self.i = start + 2;
                            nodes.push(self.if_block()?);
                            continue;
                        }
                        self.i = start;
                    }
                    if !in_if && (self.at_kw("elsif") || self.at_kw("else") || self.at_kw("endif")) {
                        self.i = save;
                        return Err(self.err("elsif/else/endif without if"));
                    }
                    if !text.is_empty() {
                        nodes.push(Node::Text(std::mem::take(&mut text)));
                    }
                    let st = self.statements(false)?;
                    self.ws();
                    self.expect("}")?;
                    nodes.push(Node::Stmts(st));
                }
                _ => {
                    // Copy a run of plain text (UTF-8 safe: '[' and '{' are ASCII).
                    let start = self.i;
                    while let Some(c) = self.peek() {
                        if c == b'[' || c == b'{' {
                            break;
                        }
                        self.i += 1;
                    }
                    text.push_str(&self.src[start..self.i]);
                }
            }
        }
        if !text.is_empty() {
            nodes.push(Node::Text(text));
        }
        Ok((nodes, None))
    }

    /// After `{if`: condition `}` body (`{elsif c}` body)* (`{else}` body)? `{endif}`.
    fn if_block(&mut self) -> Result<Node, TemplateError> {
        let mut arms = Vec::new();
        let mut els = None;
        let mut cond = self.expr()?;
        self.expect("}")?;
        loop {
            let (body, stop) = self.text_block(true)?;
            match stop {
                Some(Stop::Elsif) => {
                    arms.push((cond, body));
                    cond = self.expr()?;
                    self.expect("}")?;
                }
                Some(Stop::Else) => {
                    arms.push((cond, body));
                    self.expect("}")?;
                    let (b2, stop2) = self.text_block(true)?;
                    if stop2 != Some(Stop::Endif) {
                        return Err(self.err("expected endif after else"));
                    }
                    self.expect("}")?;
                    els = Some(b2);
                    break;
                }
                Some(Stop::Endif) => {
                    arms.push((cond, body));
                    self.expect("}")?;
                    break;
                }
                None => return Err(self.err("missing endif")),
            }
        }
        Ok(Node::If(arms, els))
    }

    fn legacy(&mut self) -> Result<Node, TemplateError> {
        let pos = self.i;
        self.i += 1; // '['
        self.ws();
        let name = self.ident().ok_or_else(|| self.err("expected a variable name after '['"))?;
        self.ws();
        let mut index = None;
        if self.peek() == Some(b'[') {
            self.i += 1;
            self.ws();
            index = Some(self.ident().ok_or_else(|| self.err("legacy index must name an int variable"))?);
            self.ws();
            if self.peek() != Some(b']') {
                return Err(self.err("expected ']'"));
            }
            self.i += 1;
            self.ws();
        }
        if self.peek() != Some(b']') {
            return Err(self.err("expected ']'"));
        }
        self.i += 1;
        // `[name_[idx]]`: the trailing underscore is part of the legacy form.
        let name = if index.is_some() { name.trim_end_matches('_').to_string() } else { name };
        Ok(Node::Legacy(LegacyRef { name, index, pos }))
    }

    fn ident(&mut self) -> Option<String> {
        let start = self.i;
        match self.peek() {
            Some(c) if c.is_ascii_alphabetic() || c == b'_' => self.i += 1,
            _ => return None,
        }
        while let Some(c) = self.peek() {
            if c.is_ascii_alphanumeric() || c == b'_' {
                self.i += 1;
            } else {
                break;
            }
        }
        Some(self.src[start..self.i].to_string())
    }

    fn statements(&mut self, in_if: bool) -> Result<Vec<Stmt>, TemplateError> {
        let mut out = Vec::new();
        loop {
            self.ws();
            while self.peek() == Some(b';') {
                self.i += 1;
                self.ws();
            }
            if self.peek() == Some(b'}') || self.peek().is_none() {
                break;
            }
            if in_if && (self.at_kw("elsif") || self.at_kw("else") || self.at_kw("endif")) {
                break;
            }
            let stmt = self.statement()?;
            let after_if = matches!(stmt, Stmt::If { .. });
            out.push(stmt);
            self.ws();
            if self.peek() == Some(b';') || after_if {
                continue;
            }
            break;
        }
        if out.is_empty() && !in_if {
            return Err(self.err("empty macro"));
        }
        Ok(out)
    }

    fn statement(&mut self) -> Result<Stmt, TemplateError> {
        self.ws();
        let pos = self.i;
        if self.dialect != Dialect::Compact && self.at_kw("if") {
            self.i += 2;
            let mut arms = Vec::new();
            let mut els = None;
            loop {
                let cond = self.expr()?;
                self.ws();
                if !self.at_kw("then") {
                    return Err(self.err("expected then"));
                }
                self.i += 4;
                arms.push((cond, self.statements(true)?));
                if !self.at_kw("elsif") {
                    break;
                }
                self.i += 5;
            }
            if self.at_kw("else") {
                self.i += 4;
                els = Some(self.statements(true)?);
            }
            if !self.at_kw("endif") {
                return Err(self.err("expected endif"));
            }
            self.i += 5;
            return Ok(Stmt::If { arms, els });
        }
        if self.dialect != Dialect::Compact && (self.at_kw("local") || self.at_kw("global")) {
            let global = self.at_kw("global");
            self.i += if global { 6 } else { 5 };
            self.ws();
            let name = self.ident().ok_or_else(|| self.err("expected a variable name"))?;
            self.expect("=")?;
            let rhs = self.rhs()?;
            return Ok(Stmt::Decl { global, name, rhs, pos });
        }
        if self.dialect != Dialect::Compact {
            // Assignment: ident [ '[' expr ']' ] '=' (not '==')
            let save = self.i;
            if let Some(name) = self.ident() {
                if !KEYWORDS.contains(&name.as_str()) {
                    self.ws();
                    let mut index = None;
                    if self.peek() == Some(b'[') {
                        self.i += 1;
                        index = Some(self.add()?);
                        self.expect("]")?;
                        self.ws();
                    }
                    if self.peek() == Some(b'=') && self.s.get(self.i + 1) != Some(&b'=') && self.s.get(self.i + 1) != Some(&b'~') {
                        self.i += 1;
                        let rhs = self.rhs()?;
                        return Ok(Stmt::Assign { name, index, rhs, pos });
                    }
                }
            }
            self.i = save;
        }
        Ok(Stmt::Expr(self.expr()?))
    }

    fn rhs(&mut self) -> Result<Rhs, TemplateError> {
        self.ws();
        if self.at_kw("repeat") {
            self.i += 6;
            self.expect("(")?;
            let n = self.expr()?;
            self.expect(",")?;
            let v = self.expr()?;
            self.expect(")")?;
            return Ok(Rhs::Repeat(n, v));
        }
        if self.peek() == Some(b'(') {
            // An initializer list, or a parenthesized scalar expression.
            let save = self.i;
            self.i += 1;
            self.ws();
            if self.peek() == Some(b')') {
                self.i += 1;
                return Ok(Rhs::List(Vec::new()));
            }
            let first = self.expr()?;
            self.ws();
            if self.peek() == Some(b',') {
                let mut items = vec![first];
                while self.eat(",") {
                    items.push(self.expr()?);
                }
                self.expect(")")?;
                return Ok(Rhs::List(items));
            }
            if self.peek() == Some(b')') {
                self.i += 1;
                // `(x)` alone is a one-element vector in a declaration.
                self.ws();
                if matches!(self.peek(), Some(b';') | Some(b'}') | None) || self.at_kw("elsif") || self.at_kw("else") || self.at_kw("endif") {
                    return Ok(Rhs::List(vec![first]));
                }
            }
            self.i = save;
        }
        Ok(Rhs::Expr(self.expr()?))
    }

    fn expr(&mut self) -> Result<Expr, TemplateError> {
        let c = self.or()?;
        self.ws();
        if self.peek() == Some(b'?') {
            let pos = self.i;
            self.i += 1;
            let a = self.expr()?;
            self.expect(":")?;
            let b = self.expr()?;
            return Ok(Expr { k: ExprK::Tern(Box::new(c), Box::new(a), Box::new(b)), pos });
        }
        Ok(c)
    }

    fn or(&mut self) -> Result<Expr, TemplateError> {
        let mut a = self.and()?;
        loop {
            self.ws();
            let pos = self.i;
            if self.eat("||") || (self.at_kw("or") && {
                self.i += 2;
                true
            }) {
                let b = self.and()?;
                a = Expr { k: ExprK::Bin(Op::Or, Box::new(a), Box::new(b)), pos };
            } else {
                return Ok(a);
            }
        }
    }

    fn and(&mut self) -> Result<Expr, TemplateError> {
        let mut a = self.eq()?;
        loop {
            self.ws();
            let pos = self.i;
            if self.eat("&&") || (self.at_kw("and") && {
                self.i += 3;
                true
            }) {
                let b = self.eq()?;
                a = Expr { k: ExprK::Bin(Op::And, Box::new(a), Box::new(b)), pos };
            } else {
                return Ok(a);
            }
        }
    }

    fn eq(&mut self) -> Result<Expr, TemplateError> {
        let mut a = self.rel()?;
        loop {
            self.ws();
            let pos = self.i;
            let op = if self.eat("==") {
                Op::Eq
            } else if self.eat("!=") || self.eat("<>") {
                Op::Ne
            } else if self.eat("=~") || self.eat("!~") {
                let negate = self.s[self.i - 2] == b'!';
                let pat = self.regex_literal()?;
                a = Expr { k: ExprK::Match(Box::new(a), pat, negate), pos };
                continue;
            } else {
                return Ok(a);
            };
            let b = self.rel()?;
            a = Expr { k: ExprK::Bin(op, Box::new(a), Box::new(b)), pos };
        }
    }

    fn regex_literal(&mut self) -> Result<String, TemplateError> {
        self.ws();
        if self.peek() != Some(b'/') {
            return Err(self.err("expected a /regex/"));
        }
        self.i += 1;
        let start = self.i;
        while let Some(c) = self.peek() {
            if c == b'\\' {
                self.i += 2;
                continue;
            }
            if c == b'/' {
                break;
            }
            self.i += 1;
        }
        let pat = self.src[start..self.i.min(self.src.len())].to_string();
        if self.peek() != Some(b'/') {
            return Err(self.err("unterminated regex"));
        }
        self.i += 1;
        Ok(pat)
    }

    fn rel(&mut self) -> Result<Expr, TemplateError> {
        let mut a = self.add()?;
        loop {
            self.ws();
            let pos = self.i;
            let op = if self.eat("<=") {
                Op::Le
            } else if self.eat(">=") {
                Op::Ge
            } else if self.s[self.i..].starts_with(b"<") && !self.s[self.i..].starts_with(b"<>") {
                self.i += 1;
                Op::Lt
            } else if self.eat(">") {
                Op::Gt
            } else {
                return Ok(a);
            };
            let b = self.add()?;
            a = Expr { k: ExprK::Bin(op, Box::new(a), Box::new(b)), pos };
        }
    }

    fn add(&mut self) -> Result<Expr, TemplateError> {
        let mut a = self.mul()?;
        loop {
            self.ws();
            let pos = self.i;
            let op = match self.peek() {
                Some(b'+') => Op::Add,
                Some(b'-') => Op::Sub,
                _ => return Ok(a),
            };
            self.i += 1;
            let b = self.mul()?;
            a = Expr { k: ExprK::Bin(op, Box::new(a), Box::new(b)), pos };
        }
    }

    fn mul(&mut self) -> Result<Expr, TemplateError> {
        let mut a = self.unary()?;
        loop {
            self.ws();
            let pos = self.i;
            let op = match self.peek() {
                Some(b'*') => Op::Mul,
                Some(b'/') => Op::Div,
                Some(b'%') => Op::Mod,
                _ => return Ok(a),
            };
            self.i += 1;
            let b = self.unary()?;
            a = Expr { k: ExprK::Bin(op, Box::new(a), Box::new(b)), pos };
        }
    }

    fn unary(&mut self) -> Result<Expr, TemplateError> {
        self.ws();
        let pos = self.i;
        match self.peek() {
            Some(b'-') => {
                self.i += 1;
                Ok(Expr { k: ExprK::Un('-', Box::new(self.unary()?)), pos })
            }
            Some(b'+') => {
                self.i += 1;
                Ok(Expr { k: ExprK::Un('+', Box::new(self.unary()?)), pos })
            }
            Some(b'!') if self.s.get(self.i + 1) != Some(&b'=') => {
                self.i += 1;
                Ok(Expr { k: ExprK::Un('!', Box::new(self.unary()?)), pos })
            }
            _ if self.at_kw("not") => {
                self.i += 3;
                Ok(Expr { k: ExprK::Un('!', Box::new(self.unary()?)), pos })
            }
            _ => self.primary(),
        }
    }

    fn primary(&mut self) -> Result<Expr, TemplateError> {
        self.ws();
        let pos = self.i;
        let lit = |v: Val| Ok(Expr { k: ExprK::Lit(v), pos });
        match self.peek() {
            Some(b'(') => {
                self.i += 1;
                let e = self.expr()?;
                self.expect(")")?;
                Ok(e)
            }
            Some(b'"') => {
                self.i += 1;
                let mut s = String::new();
                loop {
                    let start = self.i;
                    while let Some(c) = self.peek() {
                        if c == b'"' || c == b'\\' {
                            break;
                        }
                        self.i += 1;
                    }
                    s.push_str(&self.src[start..self.i]);
                    match self.peek() {
                        None => return Err(self.err("unterminated string")),
                        Some(b'"') => {
                            self.i += 1;
                            break;
                        }
                        _ => {
                            let next = self.s.get(self.i + 1).copied();
                            if self.dialect != Dialect::Compact {
                                match next {
                                    Some(b'n') => s.push('\n'),
                                    Some(b'r') => s.push('\r'),
                                    Some(b'"') => s.push('"'),
                                    Some(b'\\') => s.push('\\'),
                                    _ => return Err(self.err("Invalid escape sequence")),
                                }
                                self.i += 2;
                            } else if next == Some(b'"') {
                                // Raw in the vendor dialect, but an escaped
                                // quote does not end the string.
                                s.push_str("\\\"");
                                self.i += 2;
                            } else {
                                s.push('\\');
                                self.i += 1;
                            }
                        }
                    }
                }
                lit(Val::Str(s))
            }
            Some(c) if c.is_ascii_digit() || c == b'.' => {
                let start = self.i;
                while let Some(c) = self.peek() {
                    if c.is_ascii_digit() {
                        self.i += 1;
                    } else {
                        break;
                    }
                }
                let mut is_d = false;
                if self.peek() == Some(b'.') {
                    is_d = true;
                    self.i += 1;
                    while let Some(c) = self.peek() {
                        if c.is_ascii_digit() {
                            self.i += 1;
                        } else {
                            break;
                        }
                    }
                    if matches!(self.peek(), Some(b'e') | Some(b'E')) {
                        let save = self.i;
                        self.i += 1;
                        if matches!(self.peek(), Some(b'+') | Some(b'-')) {
                            self.i += 1;
                        }
                        if !matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                            self.i = save;
                        }
                        while let Some(c) = self.peek() {
                            if c.is_ascii_digit() {
                                self.i += 1;
                            } else {
                                break;
                            }
                        }
                    }
                }
                let t = &self.src[start..self.i];
                if is_d {
                    lit(Val::Double(t.parse().map_err(|_| self.err("bad number"))?))
                } else {
                    lit(Val::Int(t.parse().map_err(|_| self.err("bad integer"))?))
                }
            }
            _ => {
                let name = self.ident().ok_or_else(|| self.err("expected an expression"))?;
                match name.as_str() {
                    "true" => return lit(Val::Bool(true)),
                    "false" => return lit(Val::Bool(false)),
                    _ => {}
                }
                self.ws();
                if name == "interpolate_table" {
                    self.expect("(")?;
                    let x = self.expr()?;
                    let mut table = Vec::new();
                    while self.eat(",") {
                        self.expect("(")?;
                        let a = self.unary()?;
                        self.expect(",")?;
                        let b = self.unary()?;
                        self.expect(")")?;
                        table.push((a, b));
                    }
                    self.expect(")")?;
                    return Ok(Expr { k: ExprK::Interp(Box::new(x), table), pos });
                }
                if self.peek() == Some(b'(') && (KEYWORDS.contains(&name.as_str()) || name == "one_of") {
                    self.i += 1;
                    let mut args = Vec::new();
                    self.ws();
                    if self.peek() != Some(b')') {
                        loop {
                            args.push(self.expr()?);
                            self.ws();
                            if self.eat(",") {
                                self.ws();
                                if self.peek() == Some(b')') {
                                    break;
                                }
                                continue;
                            }
                            if name == "one_of" && self.peek() != Some(b')') {
                                continue;
                            }
                            break;
                        }
                    }
                    self.expect(")")?;
                    return Ok(Expr { k: ExprK::Call(name, args), pos });
                }
                if self.peek() == Some(b'[') {
                    self.i += 1;
                    let ix = self.add()?;
                    self.expect("]")?;
                    return Ok(Expr { k: ExprK::Var(name, Some(Box::new(ix))), pos });
                }
                Ok(Expr { k: ExprK::Var(name, None), pos })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merged_dialect_takes_compact_values_and_statement_grammar() {
        let mut cfg = Config::new();
        for (key, vals) in [
            ("filament_map", ["2", "1"]),
            ("nozzle_diameter", ["0.4", "0.6"]),
            ("retraction_length", ["1.5", "2.5"]),
            ("retract_before_wipe", ["15%", "25%"]),
            ("cooling_slowdown_logic", ["uniform_cooling", "consistent_surface"]),
        ] {
            cfg.set_vec(key, vals.map(String::from).to_vec());
        }
        cfg.set_str("sparse_infill_pattern", "grid");
        cfg.set_str("machine_start_gcode", "a\nb\r\"\\");
        let mut env = Env::new(&cfg, Dialect::Merged);
        assert_eq!(env.render("{local x = 1; x}|{retract_before_wipe[0]}|{cooling_slowdown_logic[1]}|{sparse_infill_pattern}").unwrap(), "1|15|1|grid");
        assert_eq!(env.render("{nozzle_diameter}|{retraction_length}").unwrap(), "0.6|1.5", "mapped extruder versus overridable filament key");
        assert_eq!(env.render("[machine_start_gcode]").unwrap(), "a\\nb\\r\\\"\\\\", "legacy strings remain escaped");
        assert_eq!(env.render(r#"{"a\nb\r\"\\"}"#).unwrap(), "a\nb\r\"\\");
        assert!(env.render(r#"{"\t"}"#).is_err());
        assert!(env.render("{nozzle_diameter[-1]}").is_err());
        for key in ["filament_map", "retract_before_wipe", "cooling_slowdown_logic"] {
            assert!(env.render(&format!("{{{key}}}")).is_err(), "{key} requires an explicit index");
        }
        assert_eq!(env.render("{floor(2.8)}{filament_change(1)}{nozzle_diameter}|{retraction_length}").unwrap(), "20.4|2.5");
        env.current_extruder_id = 20;
        assert_eq!(env.render("{nozzle_diameter}|{retraction_length}").unwrap(), "0.4|1.5", "out of range falls back to zero");
        let mut compact = Env::new(&cfg, Dialect::Compact);
        assert_eq!(compact.render("{nozzle_diameter}|{nozzle_diameter[-1]}|{retract_before_wipe[0]}|{cooling_slowdown_logic[1]}").unwrap(), "0.4|0.4|15|1");
        assert_eq!(compact.render(r#"{"a\nb"}"#).unwrap(), r"a\nb");
        assert!(compact.render("{local x = 1}").is_err());
        assert!(compact.render("{if true then 1 endif}").is_err());
        let mut statements = Env::new(&cfg, Dialect::Statements);
        assert_eq!(statements.render("{retract_before_wipe[0]}|{cooling_slowdown_logic[1]}").unwrap(), "0.15|consistent_surface");
        assert!(statements.render("{nozzle_diameter}").is_err());
        cfg.remove("filament_map");
        let mut env = Env::new(&cfg, Dialect::Merged);
        env.current_extruder_id = 1;
        assert_eq!(env.render("{nozzle_diameter}").unwrap(), "0.6", "missing map uses the current filament");
        cfg.set_vec("nozzle_diameter", vec!["0.8".into()]);
        let mut env = Env::new(&cfg, Dialect::Merged);
        env.current_extruder_id = 20;
        assert_eq!(env.render("{nozzle_diameter}").unwrap(), "0.8", "a singleton always uses zero");
    }

    #[test]
    fn statement_if_takes_one_branch() {
        let cfg = Config::new();
        for dialect in [Dialect::Statements, Dialect::Merged] {
            let mut env = Env::new(&cfg, dialect);
            for (a, b, expected) in [(true, true, "x"), (true, false, "x"), (false, true, "y"), (false, false, "z")] {
                env.set("a", TVar::boolean(a));
                env.set("b", TVar::boolean(b));
                for src in [
                    r#"{if a then "x" elsif b then "y" else "z" endif}"#,
                    r#"{if a then "x"; elsif b then "y"; else "z"; endif}"#,
                ] {
                    assert_eq!(env.render(src).unwrap(), expected, "{dialect:?}: a={a}, b={b}");
                }
            }
            for (src, expected) in [
                (r#"{if true then if false then "x" else "y" endif "z" else "w" endif}"#, "yz"),
                (r#"{if false then "x" else endif}"#, ""),
                ("{if true then elsif true then else endif}", ""),
                ("{if false then elsif true then else endif}", ""),
                ("{if false then endif}", ""),
                ("{local v = 1; if v == 1 then v = 2 endif v}", "2"),
                ("{if true then local v = 3 endif}{v}", "3"),
                ("{if false then 0 elsif false then 1 elsif true then 2 endif}", "2"),
            ] {
                assert_eq!(env.render(src).unwrap(), expected, "{dialect:?}: {src}");
            }
            assert_eq!(env.render("{if 1 then endif}").unwrap_err().msg, "Not a boolean expression");
            for src in ["{if true then}", "{if true then 1 2 endif}", "{if true then else else endif}"] {
                assert!(env.render(src).is_err(), "{dialect:?}: {src}");
            }
        }
    }

    #[test]
    fn statement_if_evaluates_only_the_taken_branch() {
        let cfg = Config::new();
        for dialect in [Dialect::Statements, Dialect::Merged] {
            let mut env = Env::new(&cfg, dialect);
            env.set("v", TVar::ints(&[1]));
            assert!(env.render("{v[-1]}").is_err());
            for src in [
                r#"{if true then "ok" elsif missing then v[-1] else v[-1] endif}"#,
                r#"{if false then v[-1] elsif true then "ok" else v[-1] endif}"#,
                r#"{if false then v[-1] else "ok" endif}"#,
            ] {
                assert_eq!(env.render(src).unwrap(), "ok", "{dialect:?}: {src}");
            }
            assert!(env.render("{if true then 1 else local = endif}").is_err(), "untaken branches must still parse");
        }
    }

    #[test]
    fn text_level_if_still_parses_in_both_dialects() {
        let cfg = Config::new();
        for dialect in [Dialect::Compact, Dialect::Statements, Dialect::Merged] {
            let mut env = Env::new(&cfg, dialect);
            assert_eq!(env.render("{if 1 == 1}a{else}b{endif}").unwrap(), "a");
            assert_eq!(env.render("{if false}{missing}{elsif true}{if false}b{else}a{endif}{else}c{endif}").unwrap(), "a");
            if dialect != Dialect::Compact {
                assert_eq!(env.render(r#"{if true}{if false then "b" else "a" endif}{endif}"#).unwrap(), "a");
            }
        }
    }

    #[test]
    fn a_string_statement_keeps_its_real_newlines() {
        let cfg = Config::new();
        for dialect in [Dialect::Statements, Dialect::Merged] {
            assert_eq!(Env::new(&cfg, dialect).render("{\"G1 X1\nG1 Y2\r\n\"}").unwrap(), "G1 X1\nG1 Y2\r\n");
        }
    }

    #[test]
    fn position_takes_an_element_assignment() {
        let cfg = Config::new();
        for dialect in [Dialect::Statements, Dialect::Merged] {
            let mut env = Env::new(&cfg, dialect);
            env.globals.insert("position".into(), TVar::f64s(&[1.0, 2.0, 3.0]));
            assert_eq!(env.render("{position[2] = position[2] + 2; position[2]}").unwrap(), "5");
            assert_eq!(env.render("{local position = 7; position}").unwrap(), "7", "{dialect:?}: a local shadows the global");
        }
    }

    #[test]
    fn a_one_element_vector_needs_no_semicolon_before_a_branch_keyword() {
        let cfg = Config::new();
        for dialect in [Dialect::Statements, Dialect::Merged] {
            let mut env = Env::new(&cfg, dialect);
            for src in [
                "{if true then local v = (1); endif v[0]}",
                "{if true then local v = (1) endif v[0]}",
                "{if true then local v = (1) else endif v[0]}",
                "{if true then local v = (1) elsif false then endif v[0]}",
            ] {
                assert_eq!(env.render(src).unwrap(), "1", "{dialect:?}: {src}");
            }
        }
    }
}

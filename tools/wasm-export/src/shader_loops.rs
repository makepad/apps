//! The static half of the browser safety gate: every loop of every shader
//! a web build ships must end after a number of passes known before it runs.
//!
//! A shader loop that does not end hangs the GPU; the browser's watchdog
//! then kills its GPU process, and a GPU process that dies takes every tab
//! of the browser with it. The film's shaders come from documents (and so
//! from people and AIs), so their loops are checked here, before a browser
//! ever runs them.
//!
//! The check reads the GLSL ES the film ships (its shader pack,
//! `makepad_platform::makepad_script::shader_pack`), not the compiler's IR: the pack is the
//! code the browser runs, whatever compiler (or pack cache) made it. The
//! compiler emits a guard into every loop it cannot bound
//! (`shader_control::LOOP_GUARD_MAX_ITERS` and the shared
//! `SHADER_ITERATION_BUDGET`); this check proves that guard is there in the
//! output instead of trusting the compiler to have emitted it.
//!
//! A loop passes when it is:
//! - **bounded**: a `for` whose counter starts at a constant, is compared
//!   with a constant, steps by a constant towards it, and is not written in
//!   the body (nor passed to an `out`/`inout` parameter); its trip count is
//!   computed; or
//! - **guarded**: any loop (`while`, `do`, a `for` with a run-time bound)
//!   whose body begins with a pass counter that starts at a constant, is
//!   incremented by one, and breaks when it passes a constant
//!   (`g++; ... if(g > 65536u || ...){break;}`), before any `continue`.
//!
//! Anything else (a `while` without such a guard, a data-dependent bound,
//! a counter written in the body) fails.

use std::collections::HashSet;

/// How one loop is bounded.
#[derive(Clone, Debug, PartialEq)]
pub enum LoopBound {
    /// A constant `for`: at most this many passes.
    Bounded(u64),
    /// A pass counter that breaks after this many passes.
    Guarded(u64),
    /// Not provably bounded: why.
    Unbounded(String),
}

/// One loop of a shader stage.
#[derive(Clone, Debug, PartialEq)]
pub struct LoopFinding {
    /// 1-based line of the loop keyword in the stage's source.
    pub line: u32,
    /// The loop's head, for a report (`for(uint l_i = ...){`).
    pub head: String,
    pub bound: LoopBound,
}

/// A shader of the pack that has an unbounded loop.
#[derive(Clone, Debug, PartialEq)]
pub struct UnboundedLoop {
    pub shader: String,
    /// `vertex` or `fragment`.
    pub stage: &'static str,
    pub line: u32,
    pub head: String,
    pub reason: String,
}

/// The loops of a whole pack.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PackLoops {
    pub shaders: usize,
    pub bounded: usize,
    pub guarded: usize,
    /// The largest constant trip count of a bounded loop.
    pub max_trips: u64,
    pub unbounded: Vec<UnboundedLoop>,
}

/// One shader of a pack: its name and the GLSL ES of its two stages
/// (`makepad_platform::makepad_script::shader_pack` reads a pack's).
pub struct Shader<'a> {
    pub name: &'a str,
    pub vertex: &'a str,
    pub fragment: &'a str,
}

/// Checks every loop of every shader.
pub fn check_shaders(shaders: &[Shader]) -> PackLoops {
    let mut out = PackLoops { shaders: shaders.len(), ..Default::default() };
    for e in shaders {
        for (stage, code) in [("vertex", e.vertex), ("fragment", e.fragment)] {
            for f in check_glsl(code) {
                match f.bound {
                    LoopBound::Bounded(n) => {
                        out.bounded += 1;
                        out.max_trips = out.max_trips.max(n);
                    }
                    LoopBound::Guarded(_) => out.guarded += 1,
                    LoopBound::Unbounded(reason) => out.unbounded.push(UnboundedLoop { shader: e.name.to_owned(), stage, line: f.line, head: f.head, reason }),
                }
            }
        }
    }
    out
}

#[derive(Clone, Debug, PartialEq)]
struct Tok {
    s: String,
    line: u32,
}

/// GLSL as tokens: identifiers, numbers, operators; comments and
/// preprocessor lines dropped.
fn tokenize(src: &str) -> Vec<Tok> {
    const OPS3: [&str; 2] = ["<<=", ">>="];
    const OPS2: [&str; 19] = ["++", "--", "+=", "-=", "*=", "/=", "%=", "&=", "|=", "^=", "==", "!=", "<=", ">=", "&&", "||", "^^", "<<", ">>"];
    let b = src.as_bytes();
    let mut out = Vec::new();
    let (mut i, mut line, mut line_start) = (0usize, 1u32, true);
    while i < b.len() {
        let c = b[i];
        if c == b'\n' {
            line += 1;
            line_start = true;
            i += 1;
            continue;
        }
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if line_start && c == b'#' {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        line_start = false;
        if c == b'/' && b.get(i + 1) == Some(&b'/') {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if c == b'/' && b.get(i + 1) == Some(&b'*') {
            i += 2;
            while i < b.len() && !(b[i] == b'*' && b.get(i + 1) == Some(&b'/')) {
                if b[i] == b'\n' {
                    line += 1;
                }
                i += 1;
            }
            i += 2;
            continue;
        }
        let start = i;
        if c.is_ascii_alphabetic() || c == b'_' {
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
        } else if c.is_ascii_digit() || (c == b'.' && b.get(i + 1).is_some_and(|d| d.is_ascii_digit())) {
            while i < b.len() {
                let d = b[i];
                let exp_sign = (d == b'+' || d == b'-') && matches!(b[i - 1], b'e' | b'E') && !src[start..i].starts_with("0x");
                if d.is_ascii_alphanumeric() || d == b'.' || exp_sign {
                    i += 1;
                } else {
                    break;
                }
            }
        } else if let Some(op) = OPS3.iter().chain(OPS2.iter()).find(|op| src[i..].starts_with(**op)) {
            i += op.len();
        } else {
            i += src[i..].chars().next().map_or(1, |ch| ch.len_utf8());
        }
        out.push(Tok { s: src[start..i].to_string(), line });
    }
    out
}

/// The index of the token closing the bracket at `open`.
fn matching(t: &[Tok], open: usize) -> Option<usize> {
    let (o, c) = match t[open].s.as_str() {
        "(" => ("(", ")"),
        "{" => ("{", "}"),
        "[" => ("[", "]"),
        _ => return None,
    };
    let mut depth = 0i32;
    for (k, tok) in t.iter().enumerate().skip(open) {
        if tok.s == o {
            depth += 1;
        } else if tok.s == c {
            depth -= 1;
            if depth == 0 {
                return Some(k);
            }
        }
    }
    None
}

/// A constant: a number literal (`7`, `7u`, `1.5`, `0x10`), negated, or in
/// one constructor (`uint(7)`, `float(-1.0)`).
fn constant(t: &[Tok]) -> Option<f64> {
    match t {
        [n] => number(&n.s),
        [m, n] if m.s == "-" => number(&n.s).map(|v| -v),
        [ty, open, inner @ .., close] if matches!(ty.s.as_str(), "int" | "uint" | "float") && open.s == "(" && close.s == ")" => constant(inner),
        [open, inner @ .., close] if open.s == "(" && close.s == ")" => constant(inner),
        _ => None,
    }
}

fn number(s: &str) -> Option<f64> {
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        return u64::from_str_radix(hex.trim_end_matches(['u', 'U']), 16).ok().map(|v| v as f64);
    }
    if !s.starts_with(|c: char| c.is_ascii_digit() || c == '.') {
        return None;
    }
    s.trim_end_matches(['u', 'U', 'f', 'F']).parse::<f64>().ok().filter(|v| v.is_finite())
}

fn is_ident(s: &str) -> bool {
    s.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// `t` without the parentheses that enclose all of it (`((a > b))` is `a > b`).
fn unparen(mut t: &[Tok]) -> &[Tok] {
    while t.len() >= 2 && t[0].s == "(" && matching(t, 0) == Some(t.len() - 1) {
        t = &t[1..t.len() - 1];
    }
    t
}

/// `t` split at top-level `sep` tokens.
fn split_top<'a>(t: &'a [Tok], sep: &str) -> Vec<&'a [Tok]> {
    let mut parts = Vec::new();
    let (mut depth, mut from) = (0i32, 0usize);
    for (k, tok) in t.iter().enumerate() {
        match tok.s.as_str() {
            "(" | "[" | "{" => depth += 1,
            ")" | "]" | "}" => depth -= 1,
            s if s == sep && depth == 0 => {
                parts.push(&t[from..k]);
                from = k + 1;
            }
            _ => {}
        }
    }
    parts.push(&t[from..]);
    parts
}

/// Functions with an `out` or `inout` parameter (a call can write an
/// argument).
fn out_param_fns(t: &[Tok]) -> HashSet<String> {
    let mut fns = HashSet::new();
    for k in 0..t.len().saturating_sub(1) {
        if is_ident(&t[k].s) && t[k + 1].s == "(" {
            if let Some(close) = matching(t, k + 1) {
                let defines = t.get(close + 1).is_some_and(|n| n.s == "{" || n.s == ";");
                if defines && t[k + 2..close].iter().any(|p| p.s == "out" || p.s == "inout") {
                    fns.insert(t[k].s.clone());
                }
            }
        }
    }
    fns
}

const ASSIGN_OPS: [&str; 11] = ["=", "+=", "-=", "*=", "/=", "%=", "&=", "|=", "^=", "<<=", ">>="];

/// Whether `body` writes `var`: an assignment to it, `++`/`--` on it, or
/// it passed to a function with `out`/`inout` parameters.
fn writes(body: &[Tok], var: &str, out_fns: &HashSet<String>) -> bool {
    let mut calls: Vec<(usize, bool)> = Vec::new();
    let mut depth = 0usize;
    for (k, tok) in body.iter().enumerate() {
        match tok.s.as_str() {
            "(" => {
                depth += 1;
                let callee = k > 0 && is_ident(&body[k - 1].s) && out_fns.contains(&body[k - 1].s);
                calls.push((depth, callee));
            }
            ")" => {
                calls.pop();
                depth = depth.saturating_sub(1);
            }
            s if s == var => {
                let next = body.get(k + 1).map(|n| n.s.as_str());
                let prev = if k > 0 { Some(body[k - 1].s.as_str()) } else { None };
                // `a.var` is a field, not the variable.
                if prev == Some(".") {
                    continue;
                }
                if next.is_some_and(|n| ASSIGN_OPS.contains(&n) || n == "++" || n == "--") {
                    return true;
                }
                if prev.is_some_and(|p| p == "++" || p == "--") {
                    return true;
                }
                if calls.iter().any(|(_, out)| *out) {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

/// A constant `for` head's trip count: `init; cond; step`.
fn for_trips(init: &[Tok], cond: &[Tok], step: &[Tok]) -> Result<(String, u64), String> {
    // init: [type] v = C
    // init: [qualifiers and type] v = C
    let Some(eq) = init.iter().position(|t| t.s == "=").filter(|&p| p >= 1 && init[..p].iter().all(|t| is_ident(&t.s))) else {
        return Err("the counter's start is not `v = constant`".into());
    };
    let var = init[eq - 1].s.clone();
    let Some(start) = constant(&init[eq + 1..]) else { return Err("the counter does not start at a constant".into()) };
    // cond: v op C, or C op v
    let flip = |op: &str| match op {
        "<" => ">",
        ">" => "<",
        "<=" => ">=",
        ">=" => "<=",
        o => o,
    }
    .to_string();
    let (op, limit) = match cond {
        [v, op, rest @ ..] if v.s == var && matches!(op.s.as_str(), "<" | ">" | "<=" | ">=" | "!=") => (op.s.clone(), constant(rest)),
        _ => match cond.iter().position(|t| matches!(t.s.as_str(), "<" | ">" | "<=" | ">=" | "!=")) {
            Some(k) if cond.len() == k + 2 && cond[k + 1].s == var => (flip(&cond[k].s), constant(&cond[..k])),
            _ => return Err("the condition is not `counter < constant`".into()),
        },
    };
    let Some(limit) = limit else { return Err("the counter's limit is not a constant".into()) };
    // step: v++, ++v, v--, --v, v += C, v -= C, v = v + C, v = v - C
    let step = match step {
        [v, op] if v.s == var && op.s == "++" => Some(1.0),
        [op, v] if v.s == var && op.s == "++" => Some(1.0),
        [v, op] if v.s == var && op.s == "--" => Some(-1.0),
        [op, v] if v.s == var && op.s == "--" => Some(-1.0),
        [v, op, rest @ ..] if v.s == var && op.s == "+=" => constant(rest),
        [v, op, rest @ ..] if v.s == var && op.s == "-=" => constant(rest).map(|c| -c),
        [v, eq, v2, op, rest @ ..] if v.s == var && eq.s == "=" && v2.s == var && op.s == "+" => constant(rest),
        [v, eq, v2, op, rest @ ..] if v.s == var && eq.s == "=" && v2.s == var && op.s == "-" => constant(rest).map(|c| -c),
        _ => None,
    };
    let Some(step) = step.filter(|s| *s != 0.0) else { return Err("the counter does not step by a constant".into()) };
    let span = limit - start;
    let trips = match op.as_str() {
        "<" | "<=" if span < 0.0 || (span == 0.0 && op == "<") => 0.0,
        "<" if step > 0.0 => (span / step).ceil(),
        "<=" if step > 0.0 => (span / step).floor() + 1.0,
        ">" | ">=" if span > 0.0 || (span == 0.0 && op == ">") => 0.0,
        ">" if step < 0.0 => (span / step).ceil(),
        ">=" if step < 0.0 => (span / step).floor() + 1.0,
        "!=" if span == 0.0 => 0.0,
        "!=" if (span / step) > 0.0 && (span / step).fract() == 0.0 => span / step,
        _ => return Err("the counter steps away from its limit".into()),
    };
    if !(0.0..=u64::MAX as f64).contains(&trips) {
        return Err("the trip count overflows".into());
    }
    Ok((var, trips as u64))
}

/// The guard at the top of a loop body: `g++; ...; if(... g > C ...){break;}`
/// with `g` declared from a constant before the loop. Returns the passes it
/// allows.
fn guard(body: &[Tok], before: &[Tok], out_fns: &HashSet<String>) -> Result<u64, String> {
    // Top-level statements up to the first `if`.
    let mut k = 0;
    let mut incremented: Vec<String> = Vec::new();
    let mut others: Vec<(usize, usize)> = Vec::new();
    while k < body.len() && body[k].s != "if" {
        let end = body[k..].iter().position(|t| t.s == ";").map(|p| k + p).ok_or("no guard at the top of the body")?;
        let st = &body[k..end];
        if st.iter().any(|t| matches!(t.s.as_str(), "continue" | "break" | "return" | "{" | "for" | "while" | "do")) {
            return Err("the body does not begin with a pass counter guard".into());
        }
        match st {
            [g, op] if is_ident(&g.s) && op.s == "++" => incremented.push(g.s.clone()),
            [op, g] if is_ident(&g.s) && op.s == "++" => incremented.push(g.s.clone()),
            [g, op, one] if is_ident(&g.s) && op.s == "+=" && number(&one.s) == Some(1.0) => incremented.push(g.s.clone()),
            // `g = g + 1`, the compiler's `g = (g + 1u)`
            [g, eq, rest @ ..] if is_ident(&g.s) && eq.s == "=" && matches!(unparen(rest), [g2, plus, one] if g2.s == g.s && plus.s == "+" && number(&one.s) == Some(1.0)) => incremented.push(g.s.clone()),
            _ => others.push((k, end)),
        }
        k = end + 1;
    }
    if k >= body.len() || body.get(k + 1).map(|t| t.s.as_str()) != Some("(") {
        return Err("the body does not begin with a pass counter guard".into());
    }
    let close = matching(body, k + 1).ok_or("unbalanced guard")?;
    let cond = &body[k + 2..close];
    let then = &body[close + 1..];
    let breaks = matches!(then, [b, s, ..] if b.s == "break" && s.s == ";") || matches!(then, [o, b, s, c, ..] if o.s == "{" && b.s == "break" && s.s == ";" && c.s == "}");
    if !breaks {
        return Err("the guard does not break".into());
    }
    let mut best: Option<u64> = None;
    for term in split_top(unparen(cond), "||") {
        let term = unparen(term);
        let (g, limit, inclusive) = match term {
            [g, op, rest @ ..] if is_ident(&g.s) && (op.s == ">" || op.s == ">=") => (g.s.clone(), constant(rest), op.s == ">="),
            _ => match term.iter().position(|t| t.s == "<" || t.s == "<=") {
                Some(p) if term.len() == p + 2 && is_ident(&term[p + 1].s) => (term[p + 1].s.clone(), constant(&term[..p]), term[p].s == "<="),
                _ => continue,
            },
        };
        let Some(limit) = limit else { continue };
        // Incremented once, and written by nothing else in the body.
        if incremented.iter().filter(|i| **i == g).count() != 1 || others.iter().any(|&(a, b)| writes(&body[a..b], &g, out_fns)) {
            continue;
        }
        if writes(&body[k..], &g, out_fns) {
            continue;
        }
        // Declared from a constant just before the loop.
        let Some(start) = declared_start(before, &g) else { continue };
        if limit < 0.0 {
            continue;
        }
        let passes = (limit - start + if inclusive { 0.0 } else { 1.0 }).max(0.0) as u64;
        best = Some(best.map_or(passes, |b: u64| b.min(passes)));
    }
    best.ok_or_else(|| "no pass counter that breaks at a constant".into())
}

/// The constant `g` was last declared with (`type g = C;`) in `before`, as
/// long as nothing between that declaration and the loop writes it.
fn declared_start(before: &[Tok], g: &str) -> Option<f64> {
    for k in (0..before.len()).rev() {
        if before[k].s == g && before.get(k + 1).is_some_and(|t| t.s == "=") && k > 0 && is_ident(&before[k - 1].s) {
            let end = before[k..].iter().position(|t| t.s == ";")? + k;
            let start = constant(&before[k + 2..end])?;
            if writes(&before[end..], g, &HashSet::new()) {
                return None;
            }
            return Some(start);
        }
    }
    None
}

/// Every loop of one GLSL source and how it is bounded.
pub fn check_glsl(src: &str) -> Vec<LoopFinding> {
    let t = tokenize(src);
    let out_fns = out_param_fns(&t);
    (0..t.len()).filter_map(|k| loop_at(&t, k, &out_fns).map(|(f, _)| f)).collect()
}

/// The loop starting at token `k` (`for`, `while`, `do`; not the tail of a
/// `do ... while`), with its body's token range.
fn loop_at(t: &[Tok], k: usize, out_fns: &HashSet<String>) -> Option<(LoopFinding, (usize, usize))> {
    let kw = t[k].s.as_str();
    if !matches!(kw, "for" | "while" | "do") {
        return None;
    }
    // The `while` of a `do ... while(...)` is its tail, not a loop.
    if kw == "while" && k > 0 && t[k - 1].s == "}" {
        if let Some(open) = (0..k).rev().find(|&o| t[o].s == "{" && matching(t, o) == Some(k - 1)) {
            if open > 0 && t[open - 1].s == "do" {
                return None;
            }
        }
    }
    let line = t[k].line;
    let head_text = |a: usize, b: usize| t[a..=b.min(t.len() - 1)].iter().map(|x| x.s.as_str()).collect::<Vec<_>>().join(" ");
    let unbounded = |head: String, reason: &str| Some((LoopFinding { line, head, bound: LoopBound::Unbounded(reason.to_string()) }, (k, k)));
    // The loop's head (`(...)`, none for `do`) and body.
    let (head, body_at) = if kw == "do" {
        (None, k + 1)
    } else {
        let Some(close) = t.get(k + 1).filter(|x| x.s == "(").and_then(|_| matching(t, k + 1)) else {
            return unbounded(head_text(k, k + 1), "malformed loop");
        };
        (Some((k + 2, close)), close + 1)
    };
    let range = match t.get(body_at) {
        Some(b) if b.s == "{" => match matching(t, body_at) {
            Some(end) => (body_at + 1, end),
            None => return unbounded(head_text(k, body_at), "unbalanced body"),
        },
        Some(_) => (body_at, t[body_at..].iter().position(|x| x.s == ";").map_or(t.len(), |p| body_at + p + 1)),
        None => (t.len(), t.len()),
    };
    let body = &t[range.0..range.1];
    let head_s = head_text(k, body_at);
    let before = &t[k.saturating_sub(64)..k];
    let mut reason = String::new();
    if kw == "for" {
        let (a, b) = head.unwrap();
        let parts = split_top(&t[a..b], ";");
        if parts.len() == 3 {
            match for_trips(parts[0], parts[1], parts[2]) {
                Ok((var, trips)) if !writes(body, &var, out_fns) => {
                    return Some((LoopFinding { line, head: head_s, bound: LoopBound::Bounded(trips) }, range));
                }
                Ok((var, _)) => reason = format!("the body writes the counter `{var}`"),
                Err(e) => reason = e,
            }
        } else {
            reason = "malformed for".into();
        }
    }
    let bound = match guard(body, before, out_fns) {
        Ok(passes) => LoopBound::Guarded(passes),
        Err(e) => LoopBound::Unbounded(if reason.is_empty() { e } else { format!("{reason}, and {e}") }),
    };
    Some((LoopFinding { line, head: head_s, bound }, range))
}

/// The static per-pixel cost of a fragment shader by its literal loops:
/// arithmetic operations and texture fetches, each loop's body counted its
/// constant trip count times, each call its callee's cost. A loop whose
/// passes depend on data (a guarded loop) counts one pass and marks the
/// cost `data_dependent`: its real cost is above this. An estimate to rank
/// passes by, not a time.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Cost {
    pub ops: f64,
    pub fetches: f64,
    /// Whether a guarded loop is in it (its cost is then a nominal floor).
    pub data_dependent: bool,
}

impl Cost {
    /// Ops plus fetches weighted as [`FETCH_WEIGHT`] ops each.
    pub fn total(&self) -> f64 {
        self.ops + self.fetches * FETCH_WEIGHT
    }

    fn add(&mut self, o: Cost, times: f64) {
        self.ops += o.ops * times;
        self.fetches += o.fetches * times;
        self.data_dependent |= o.data_dependent;
    }
}

/// A texture fetch against an arithmetic operation.
pub const FETCH_WEIGHT: f64 = 8.0;

const FETCHES: [&str; 8] = ["texture", "textureLod", "texelFetch", "textureGrad", "textureProj", "textureOffset", "texture2D", "textureLodOffset"];
const OPS: [&str; 22] = ["+", "-", "*", "/", "%", "+=", "-=", "*=", "/=", "<", ">", "<=", ">=", "==", "!=", "&&", "||", "?", "&", "|", "^", "!"];

/// [`Cost`] of `main` in a fragment shader's source.
pub fn fragment_cost(src: &str) -> Cost {
    let t = tokenize(src);
    let out_fns = out_param_fns(&t);
    // Function bodies by name.
    let mut fns: std::collections::HashMap<String, (usize, usize)> = std::collections::HashMap::new();
    for k in 1..t.len().saturating_sub(1) {
        if is_ident(&t[k].s) && is_ident(&t[k - 1].s) && t[k + 1].s == "(" {
            if let Some(close) = matching(&t, k + 1) {
                if t.get(close + 1).is_some_and(|b| b.s == "{") {
                    if let Some(end) = matching(&t, close + 1) {
                        fns.insert(t[k].s.clone(), (close + 2, end));
                    }
                }
            }
        }
    }
    let mut memo = std::collections::HashMap::new();
    let Some(&main) = fns.get("main") else { return Cost::default() };
    cost_range(&t, main, &fns, &out_fns, &mut memo, 0)
}

fn cost_range(
    t: &[Tok],
    (a, b): (usize, usize),
    fns: &std::collections::HashMap<String, (usize, usize)>,
    out_fns: &HashSet<String>,
    memo: &mut std::collections::HashMap<String, Cost>,
    depth: u32,
) -> Cost {
    let mut cost = Cost::default();
    let mut k = a;
    while k < b {
        if let Some((finding, body)) = loop_at(t, k, out_fns) {
            let times = match finding.bound {
                LoopBound::Bounded(n) => n as f64,
                LoopBound::Guarded(_) | LoopBound::Unbounded(_) => {
                    cost.data_dependent = true;
                    1.0
                }
            };
            if body.1 > body.0 {
                cost.add(cost_range(t, body, fns, out_fns, memo, depth), times);
                k = body.1 + 1;
                continue;
            }
        }
        let tok = t[k].s.as_str();
        if t.get(k + 1).is_some_and(|n| n.s == "(") && is_ident(tok) {
            // A fetch: the GLSL built-ins, and the backends' sampling
            // helpers (`sample2d`, `sample2d_bgra`, ...), counted as one.
            if FETCHES.contains(&tok) || tok.starts_with("sample2d") || tok.starts_with("sample3d") || tok.starts_with("sample_cube") {
                cost.fetches += 1.0;
            } else if let Some(&range) = fns.get(tok).filter(|_| depth < 32) {
                let callee = match memo.get(tok) {
                    Some(c) => *c,
                    None => {
                        let c = cost_range(t, range, fns, out_fns, memo, depth + 1);
                        memo.insert(tok.to_string(), c);
                        c
                    }
                };
                cost.add(callee, 1.0);
            } else if !matches!(tok, "if" | "return" | "vec2" | "vec3" | "vec4" | "float" | "int" | "uint" | "bool" | "mat2" | "mat3" | "mat4") {
                // A built-in function.
                cost.ops += 1.0;
            }
        } else if OPS.contains(&tok) {
            cost.ops += 1.0;
        }
        k += 1;
    }
    cost
}

/// The costliest fragment shaders, by [`Cost::total`]: (name, cost).
pub fn costliest(shaders: &[Shader], n: usize) -> Vec<(String, Cost)> {
    let mut all: Vec<(String, Cost)> = shaders.iter().map(|e| (e.name.to_owned(), fragment_cost(e.fragment))).collect();
    all.sort_by(|x, y| y.1.total().partial_cmp(&x.1.total()).unwrap_or(std::cmp::Ordering::Equal));
    // One line per shader name (its costliest variant).
    let mut seen = HashSet::new();
    all.retain(|(name, _)| seen.insert(name.clone()));
    all.truncate(n);
    all
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiler_guard_with_iteration_budget() {
        // the shape the shader compiler emits (shader_lower_control.rs write_loop_guard): the
        // pass counter, the shared iteration budget, one test of both, parenthesised
        let src = "void f() {\n    uint _mp_loop_guard_10 = 0u;\n    while (true) {\n        _mp_loop_guard_10 = (_mp_loop_guard_10 + 1u);\n        _mp_iter = (_mp_iter + 15u);\n        if (((_mp_loop_guard_10 > 65536u) || (_mp_iter > 4194304u))) { break; }\n        if ((l9_i >= l0_limit)) { break; }\n        l9_i = l9_i + 1.0;\n    }\n}\n";
        assert_eq!(bounds(src), vec![LoopBound::Guarded(65537)]);
    }

    fn bounds(src: &str) -> Vec<LoopBound> {
        check_glsl(src).into_iter().map(|f| f.bound).collect()
    }

    #[test]
    fn constant_for_loops_are_bounded() {
        assert_eq!(bounds("void f(){ for(uint l_i = uint(0); l_i < uint(48); l_i++){ x += 1.0; } }"), vec![LoopBound::Bounded(48)]);
        assert_eq!(bounds("for(int i = 10; i >= 0; i -= 2){}"), vec![LoopBound::Bounded(6)]);
        assert_eq!(bounds("for(float x = 0.0; x < 1.0; x += 0.25){}"), vec![LoopBound::Bounded(4)]);
        assert_eq!(bounds("for(int i = 0; 8 > i; ++i){}"), vec![LoopBound::Bounded(8)]);
        assert_eq!(bounds("for(int i = 5; i < 3; i++){}"), vec![LoopBound::Bounded(0)]);
    }

    #[test]
    fn the_compilers_guarded_loops_pass() {
        let src = "uint _mp_iter = 0u;\nvoid f(){\nuint _mp_loop_guard_0 = 0u;\nwhile(true){\n_mp_loop_guard_0++; _mp_iter += 3u; if(_mp_loop_guard_0 > 65536u || _mp_iter > 4194304u){break;}\nif(!(d > 0.0)){break;}\n}\n}";
        assert_eq!(bounds(src), vec![LoopBound::Guarded(65537)]);
        let src = "uint _mp_loop_guard_1_end = uint(n);\nuint _mp_loop_guard_1 = 0u;\nfor(uint l_i = uint(0); l_i < uint(_mp_loop_guard_1_end); l_i++){\n_mp_loop_guard_1++; _mp_iter += 1u; if(_mp_loop_guard_1 > 65536u || _mp_iter > 4194304u){break;}\n}";
        assert_eq!(bounds(src), vec![LoopBound::Guarded(65537)]);
    }

    #[test]
    fn unbounded_loops_fail() {
        let bad = |src: &str| matches!(bounds(src).as_slice(), [LoopBound::Unbounded(_)]);
        // No guard at all.
        assert!(bad("while(true){ if(d < 0.0){break;} d -= 0.1; }"));
        // A bound read from data.
        assert!(bad("for(int i = 0; i < count; i++){}"));
        // The counter written in the body.
        assert!(bad("for(int i = 0; i < 8; i++){ i = 0; }"));
        assert!(bad("for(int i = 0; i < 8; i++){ reset(i); }\nvoid reset(inout int v){ v = 0; }"));
        // Stepping away from the limit.
        assert!(bad("for(int i = 0; i < 8; i--){}"));
        // A do-while without a guard (its tail is not a second loop).
        assert!(bad("do { x *= 0.5; } while(x > 0.001);"));
        // A guard after a `continue` does not count every pass.
        assert!(bad("uint g = 0u;\nwhile(true){ if(skip){continue;} g++; if(g > 10u){break;} }"));
        // A guard whose counter is reset in the body.
        assert!(bad("uint g = 0u;\nwhile(true){ g++; if(g > 10u){break;} g = 0u; }"));
        // A guard on a counter from data.
        assert!(bad("uint g = n;\nwhile(true){ g++; if(g > limit){break;} }"));
    }

    #[test]
    fn costs_multiply_through_loops_and_calls() {
        let src = "vec4 blur(vec2 uv){ vec4 c = vec4(0.0); for(int i = 0; i < 8; i++){ c += texture(tex, uv + float(i) * d); } return c; }\nvoid main(){ o = blur(uv) * 0.5; }";
        let c = fragment_cost(src);
        assert_eq!(c.fetches, 8.0);
        assert!(!c.data_dependent);
        assert!(c.ops >= 8.0 * 3.0);
        let spin = fragment_cost("uint g = 0u;\nvoid main(){ uint g = 0u; while(true){ g++; if(g > 10u){break;} x = texture(t, uv).r; } }");
        assert!(spin.data_dependent);
        assert_eq!(spin.fetches, 1.0);
    }

    #[test]
    fn comments_and_fields_do_not_confuse_it() {
        assert_eq!(bounds("// while(true){}\n/* for(;;){} */ for(int i = 0; i < 4; i++){ s.i = 1.0; v[i] = 0.0; }"), vec![LoopBound::Bounded(4)]);
    }

    #[test]
    fn a_pack_with_an_unbounded_loop_fails() {
        let vertex = "void main(){ gl_Position = vec4(0.0); }";
        let shaders = [
            Shader { name: "Good", vertex, fragment: "void main(){ for(uint i = 0u; i < 4u; i++){} }" },
            Shader { name: "Spin", vertex, fragment: "uniform float d;\nvoid main(){ float x = d; while(x > 0.0){ x += 1.0; } }" },
        ];
        let loops = check_shaders(&shaders);
        assert_eq!((loops.shaders, loops.bounded, loops.guarded), (2, 1, 0));
        assert_eq!(loops.unbounded.len(), 1);
        assert_eq!((loops.unbounded[0].shader.as_str(), loops.unbounded[0].stage, loops.unbounded[0].line), ("Spin", "fragment", 2));
    }
}

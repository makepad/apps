//! Classic SPICE decks, parsed without reading files or executing control commands.
//!
//! Values in braces or quotes, and `.param` values, are expressions in the dialect `expr`
//! documents: ngspice's. A deck written for LTspice differs in one place: LTspice reads `^` as
//! exclusive or in its behavioural sources, and this reader reads it as power everywhere.

use crate::analysis::{AcSweep, Analysis, DcSweep, TranSpec};
use crate::circuit::{BKind, CtlKind, Method, ModelKind, SimCircuit, Source, Wave};
use crate::expr::{is_function, Expr};
use std::collections::{HashMap, HashSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Warning,
    Error,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SpiceDiag {
    /// One-based physical line of the statement's first line.
    pub line: usize,
    pub level: Level,
    pub message: String,
}

#[derive(Clone, Debug)]
pub struct Deck {
    pub title: String,
    pub circuit: SimCircuit,
    pub analyses: Vec<Analysis>,
    pub diags: Vec<SpiceDiag>,
}

type Params = HashMap<String, f64>;
type Result<T> = std::result::Result<T, String>;

/// The end of a decimal number at the start of `s` (an optional sign, digits with an optional
/// fraction, an optional exponent), before any scale suffix.
pub(crate) fn decimal_end(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    let mut i = usize::from(matches!(b.first(), Some(b'+' | b'-')));
    let start = i;
    while b.get(i).is_some_and(u8::is_ascii_digit) {
        i += 1;
    }
    let mut digits = i - start;
    if b.get(i) == Some(&b'.') {
        i += 1;
        let start = i;
        while b.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        digits += i - start;
    }
    if digits == 0 {
        return None;
    }
    if matches!(b.get(i), Some(b'e' | b'E')) {
        let mut j = i + 1;
        if matches!(b.get(j), Some(b'+' | b'-')) {
            j += 1;
        }
        let start = j;
        while b.get(j).is_some_and(u8::is_ascii_digit) {
            j += 1;
        }
        if j > start {
            i = j;
        }
    }
    Some(i)
}

/// A number with an optional scale suffix and ignored trailing unit letters.
pub fn number(s: &str) -> Option<f64> {
    let s = s.trim();
    let end = decimal_end(s)?;
    let tail = s.get(end..)?;
    if !tail.bytes().all(|b| b.is_ascii_alphabetic()) {
        return None;
    }
    let tail = tail.to_ascii_lowercase();
    let scale = [
        ("meg", 1e6), ("mil", 25.4e-6), ("t", 1e12), ("g", 1e9),
        ("k", 1e3), ("m", 1e-3), ("u", 1e-6), ("n", 1e-9),
        ("p", 1e-12), ("f", 1e-15), ("a", 1e-18),
    ].iter().find(|(suffix, _)| tail.starts_with(*suffix)).map_or(1.0, |(_, scale)| *scale);
    let v = s.get(..end)?.parse::<f64>().ok()? * scale;
    v.is_finite().then_some(v)
}

fn expression(text: &str, params: &Params) -> Result<f64> {
    let text = text.trim();
    let text = if let Some(inner) = text.strip_prefix('{') {
        inner.strip_suffix('}').ok_or("missing '}' in expression")?
    } else if let Some(inner) = text.strip_prefix('\'') {
        inner.strip_suffix('\'').ok_or("missing quote in expression")?
    } else {
        text
    };
    Expr::parse(text)?.value(&|name| params.get(name).copied())
}

fn value(token: &str, params: &Params) -> Result<f64> {
    if let Some(v) = number(token) {
        Ok(v)
    } else if token.starts_with('{') || token.starts_with('\'') {
        expression(token, params).map_err(|e| format!("{token}: {e}"))
    } else {
        Err(format!("invalid value {token}"))
    }
}

/// A brace or quoted value that reads the circuit (a node voltage, a branch current, the time or
/// the temperature), parsed; `None` for any other value.
fn circuit_expression(token: &str) -> Result<Option<Expr>> {
    let inner = token.strip_prefix('{').and_then(|t| t.strip_suffix('}')).or_else(|| token.strip_prefix('\'').and_then(|t| t.strip_suffix('\'')));
    let Some(inner) = inner else { return Ok(None) };
    let expr = Expr::parse(inner).map_err(|e| format!("{token}: {e}"))?;
    Ok((!expr.is_constant()).then_some(expr))
}

#[derive(Clone)]
struct Statement {
    line: usize,
    tokens: Vec<String>,
    /// The statement as written, continuations joined (a behavioural expression is read from it).
    text: String,
}

fn diagnostic(diags: &mut Vec<SpiceDiag>, line: usize, level: Level, message: impl Into<String>) {
    diags.push(SpiceDiag { line, level, message: message.into() });
}

fn lex(text: &str) -> Result<Vec<String>> {
    let mut tokens: Vec<String> = Vec::new();
    let mut chars = text.chars().peekable();
    let mut functions = Vec::new();
    let mut assignment = false;
    while let Some(c) = chars.next() {
        if c.is_whitespace() {
            continue;
        }
        if c == ',' {
            // Commas delimit deck fields, but remain operators inside an unwrapped
            // .param function call so min(1,2) survives token joining.
            if functions.last() == Some(&true) { tokens.push(",".into()); }
        } else if matches!(c, '=' | '(' | ')') {
            if c == '(' {
                let function = functions.last() == Some(&true) || (assignment && tokens.last().is_some_and(|t| t.rsplit(['+', '-', '*', '/', '^', '<', '>', '!', '&', '|', '?', ':']).next().is_some_and(is_function)));
                functions.push(function);
            } else if c == ')' {
                functions.pop();
            } else {
                assignment = true;
            }
            tokens.push(c.to_string());
        } else if c == '{' || c == '\'' {
            let mut token = String::from(c);
            let mut depth = 1usize;
            for ch in chars.by_ref() {
                token.push(ch);
                if c == '{' && ch == '{' {
                    depth += 1;
                } else if (c == '{' && ch == '}') || (c == '\'' && ch == '\'') {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
            }
            if depth != 0 {
                return Err("unterminated expression".into());
            }
            tokens.push(token.to_ascii_lowercase());
        } else {
            let mut token = String::from(c);
            while chars.peek().is_some_and(|ch| !ch.is_whitespace() && !matches!(ch, ',' | '=' | '(' | ')' | '{' | '\'')) {
                if let Some(ch) = chars.next() {
                    token.push(ch);
                }
            }
            tokens.push(token.to_ascii_lowercase());
        }
    }
    Ok(tokens)
}

fn statements(text: &str, diags: &mut Vec<SpiceDiag>) -> Vec<Statement> {
    let mut raw: Vec<(usize, String)> = Vec::new();
    for (index, line) in text.lines().enumerate().skip(1) {
        if line.starts_with('*') {
            continue;
        }
        let mut previous_space = false;
        let end = line.char_indices().find_map(|(i, ch)| {
            let comment = ch == ';' || (ch == '$' && previous_space);
            previous_space = ch.is_whitespace();
            comment.then_some(i)
        }).unwrap_or(line.len());
        let line = line.get(..end).unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix('+') {
            if let Some((_, prior)) = raw.last_mut() {
                prior.push(' ');
                prior.push_str(rest);
                continue;
            }
        }
        raw.push((index + 1, line.into()));
    }
    let mut out = Vec::new();
    let mut control = None;
    for (line, text) in raw {
        let command = text.split(|c: char| c.is_whitespace() || c == ',' || c == '(' || c == '=').next().unwrap_or("").to_ascii_lowercase();
        if control.is_some() {
            if command == ".endc" {
                control = None;
            }
            continue;
        }
        if command == ".end" {
            break;
        }
        if command == ".control" {
            diagnostic(diags, line, Level::Warning, "control block ignored");
            control = Some(line);
            continue;
        }
        if text.starts_with('+') {
            diagnostic(diags, line, Level::Error, "continuation without a statement");
            continue;
        }
        match lex(&text) {
            Ok(tokens) if !tokens.is_empty() => out.push(Statement { line, tokens, text }),
            Ok(_) => diagnostic(diags, line, Level::Error, "empty statement"),
            Err(e) => diagnostic(diags, line, Level::Error, e),
        }
    }
    if let Some(line) = control {
        diagnostic(diags, line, Level::Error, "unterminated control block");
    }
    out
}

fn identifier(s: &str) -> bool {
    let mut bytes = s.bytes();
    bytes.next().is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.')
}

// Join unwrapped parameter expressions up to the next name= pair. Wrapped expressions
// already occupy one token, including their function argument commas.
fn assignments(tokens: &[String]) -> Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        let name = token(tokens, i)?;
        if !identifier(name) || token(tokens, i + 1)? != "=" {
            return Err(format!("expected name=value at {name}"));
        }
        i += 2;
        let start = i;
        let mut depth = 0usize;
        while i < tokens.len() {
            if depth == 0 && i > start && tokens.get(i + 1).is_some_and(|t| t == "=") {
                break;
            }
            match token(tokens, i)? {
                "(" => depth += 1,
                ")" if depth > 0 => depth -= 1,
                ")" => return Err("unexpected ')' in parameter value".into()),
                "=" => return Err("missing parameter value".into()),
                _ => {}
            }
            i += 1;
        }
        if i == start || depth != 0 {
            return Err(format!("missing or unbalanced value for {name}"));
        }
        out.push((name.into(), tokens[start..i].join(" ")));
    }
    Ok(out)
}

fn define(tokens: &[String], params: &mut Params) -> Result<()> {
    let pairs = assignments(tokens)?;
    if pairs.is_empty() {
        return Err("missing parameter assignment".into());
    }
    let mut next = params.clone();
    for (key, text) in pairs {
        let v = expression(&text, &next)?;
        next.insert(key, v);
    }
    *params = next;
    Ok(())
}

fn token(tokens: &[String], i: usize) -> Result<&str> {
    tokens.get(i).map(String::as_str).ok_or_else(|| "missing argument".into())
}

fn name_token(tokens: &[String], i: usize) -> Result<&str> {
    let name = token(tokens, i)?;
    if name.is_empty() || name.contains(['=', '(', ')', '{', '}', '\'', ',']) {
        Err(format!("invalid name {name}"))
    } else {
        Ok(name)
    }
}

fn count(tokens: &[String], min: usize, max: usize) -> Result<()> {
    if (min..=max).contains(&tokens.len()) {
        Ok(())
    } else {
        Err(format!("wrong argument count for {}", token(tokens, 0).unwrap_or("statement")))
    }
}

#[derive(Clone)]
struct Subckt {
    ports: Vec<String>,
    defaults: Vec<(String, String)>,
    body: Vec<Statement>,
}

struct Scope {
    params: Params,
    prefix: String,
    ports: HashMap<String, String>,
    models: HashMap<String, String>,
}

impl Scope {
    fn name(&self, name: &str) -> String {
        if self.prefix.is_empty() { name.into() } else { format!("{}.{name}", self.prefix) }
    }

    fn node(&self, name: &str) -> String {
        if name == "0" || name == "gnd" {
            "0".into()
        } else {
            self.ports.get(name).cloned().unwrap_or_else(|| self.name(name))
        }
    }

    fn model(&self, name: &str) -> Result<String> {
        self.models.get(name).cloned().ok_or_else(|| format!("unknown model {name}"))
    }
}

#[derive(Clone, Copy, Default)]
struct Timing {
    step: f64,
    stop: f64,
}

struct Reader {
    deck: Deck,
    subckts: HashMap<String, Subckt>,
    timing: Timing,
}

/// Never panics; a statement with an error is skipped and reported.
pub fn parse(text: &str) -> Deck {
    let mut reader = Reader {
        deck: Deck {
            title: text.lines().next().unwrap_or("").trim().into(),
            circuit: SimCircuit::new(), analyses: Vec::new(), diags: Vec::new(),
        },
        subckts: HashMap::new(), timing: Timing::default(),
    };
    let statements = statements(text, &mut reader.deck.diags);
    let mut scope = Scope { params: Params::new(), prefix: String::new(), ports: HashMap::new(), models: HashMap::new() };
    let mut root = Vec::new();
    let mut i = 0;
    while let Some(stmt) = statements.get(i) {
        i += 1;
        match stmt.tokens.first().map(String::as_str) {
            Some(".subckt") => {
                let start = i;
                let mut depth = 1usize;
                let mut nested = false;
                while let Some(body) = statements.get(i) {
                    match body.tokens.first().map(String::as_str) {
                        Some(".subckt") => { depth += 1; nested = true; }
                        Some(".ends") => depth -= 1,
                        _ => {}
                    }
                    if depth == 0 { break; }
                    i += 1;
                }
                let result = if depth != 0 {
                    Err("unterminated subcircuit".into())
                } else if nested {
                    Err("nested subcircuit definitions unsupported".into())
                } else {
                    subcircuit(stmt, &statements[start..i], &statements[i])
                };
                match result {
                    Ok((name, sub)) => { reader.subckts.insert(name, sub); }
                    Err(e) => diagnostic(&mut reader.deck.diags, stmt.line, Level::Error, e),
                }
                if i < statements.len() { i += 1; }
            }
            Some(".param") => {
                if let Err(e) = define(&stmt.tokens[1..], &mut scope.params) {
                    diagnostic(&mut reader.deck.diags, stmt.line, Level::Error, e);
                }
            }
            Some(".model") => {
                if let Ok(name) = name_token(&stmt.tokens, 1) {
                    scope.models.insert(name.into(), name.into());
                }
                root.push(stmt.clone());
            }
            _ => root.push(stmt.clone()),
        }
    }
    if let Some(stmt) = root.iter().find(|s| s.tokens.first().is_some_and(|s| s == ".tran")) {
        if let Ok((timing, _, _)) = transient(&stmt.tokens, &scope.params) {
            reader.timing = timing;
        }
    }
    for stmt in &root {
        reader.statement(stmt, &scope, 0);
    }
    reader.deck.diags.sort_by_key(|d| d.line);
    reader.deck
}

fn subcircuit(header: &Statement, body: &[Statement], end: &Statement) -> Result<(String, Subckt)> {
    let tokens = &header.tokens;
    let name = name_token(tokens, 1)?;
    count(&end.tokens, 1, 2)?;
    if end.tokens.get(1).is_some_and(|n| n != name) {
        return Err("subcircuit .ends name mismatch".into());
    }
    let marker = tokens.iter().enumerate().skip(2).find(|(_, t)| *t == "params:").map_or(tokens.len(), |(i, _)| i);
    let ports = tokens[2..marker].to_vec();
    let mut seen = HashSet::new();
    if ports.iter().any(|p| p.contains(['=', '(', ')', '{', '}', '\'', ',']) || !seen.insert(p)) {
        return Err("invalid or duplicate subcircuit port".into());
    }
    let defaults = if marker < tokens.len() { assignments(&tokens[marker + 1..])? } else { Vec::new() };
    Ok((name.into(), Subckt { ports, defaults, body: body.to_vec() }))
}

impl Reader {
    fn statement(&mut self, stmt: &Statement, scope: &Scope, depth: usize) {
        match self.build(stmt, scope, depth) {
            Ok(warnings) => {
                for message in warnings {
                    diagnostic(&mut self.deck.diags, stmt.line, Level::Warning, message);
                }
            }
            Err(e) => diagnostic(&mut self.deck.diags, stmt.line, Level::Error, e),
        }
    }

    fn expand(&mut self, tokens: &[String], parent: &Scope, depth: usize) -> Result<()> {
        if depth >= 64 {
            return Err("subcircuit recursion deeper than 64".into());
        }
        let args = tokens.iter().position(|t| t == "=").map(|i| i.saturating_sub(1)).unwrap_or(tokens.len());
        if args < 2 {
            return Err("missing subcircuit name".into());
        }
        let subname = name_token(tokens, args - 1)?;
        let sub = self.subckts.get(subname).cloned().ok_or_else(|| format!("unknown subcircuit {subname}"))?;
        if args - 2 != sub.ports.len() {
            return Err(format!("wrong node count for subcircuit {subname}"));
        }
        let nodes = (1..args - 1).map(|i| name_token(tokens, i).map(|n| parent.node(n))).collect::<Result<Vec<_>>>()?;
        let mut overrides = parent.params.clone();
        let pairs = assignments(&tokens[args..])?;
        let mut keys = HashSet::new();
        for (key, text) in pairs {
            let v = expression(&text, &overrides)?;
            keys.insert(key.clone());
            overrides.insert(key, v);
        }
        let mut scope = Scope {
            params: overrides,
            prefix: parent.name(token(tokens, 0)?),
            ports: sub.ports.iter().cloned().zip(nodes).collect(),
            models: parent.models.clone(),
        };
        for (key, text) in &sub.defaults {
            if !keys.contains(key) {
                let v = expression(text, &scope.params)?;
                scope.params.insert(key.clone(), v);
            }
        }
        // Local declarations are visible throughout this instance, with .param evaluated
        // in declaration order, just as at the top level.
        for stmt in &sub.body {
            match stmt.tokens.first().map(String::as_str) {
                Some(".param") => {
                    if let Err(e) = define(&stmt.tokens[1..], &mut scope.params) {
                        diagnostic(&mut self.deck.diags, stmt.line, Level::Error, e);
                    }
                }
                Some(".model") => {
                    if let Ok(name) = name_token(&stmt.tokens, 1) {
                        scope.models.insert(name.into(), scope.name(name));
                    }
                }
                _ => {}
            }
        }
        for stmt in &sub.body {
            if stmt.tokens.first().is_some_and(|t| t == ".param") {
                continue;
            }
            self.statement(stmt, &scope, depth + 1);
        }
        Ok(())
    }

    fn build(&mut self, stmt: &Statement, scope: &Scope, depth: usize) -> Result<Vec<String>> {
        let t = &stmt.tokens;
        let command = token(t, 0)?;
        if command.starts_with('.') {
            return self.dot(t, scope);
        }
        name_token(t, 0)?;
        let mut warnings = Vec::new();
        let name = scope.name(command);
        let val = |i| value(token(t, i)?, &scope.params);
        let node = |i| name_token(t, i).map(|n| scope.node(n));
        match command.chars().next().ok_or("missing element name")? {
            'r' | 'c' | 'l' => {
                count(t, 4, usize::MAX)?;
                let (a, b) = (node(1)?, node(2)?);
                for (key, text) in assignments(&t[4..])? {
                    if key != "ic" {
                        return Err(format!("unsupported instance parameter {key}"));
                    }
                    expression(&text, &scope.params)?;
                    warnings.push("device initial condition ignored".into());
                }
                // a value that reads the circuit (v(), i(), time, temper) makes the element behavioural
                if let Some(mut expr) = circuit_expression(token(t, 3)?)? {
                    let kind = match command.as_bytes().first() {
                        Some(b'r') => BKind::R,
                        Some(b'c') => BKind::C,
                        _ => BKind::L,
                    };
                    expr.bind(&|name| scope.params.get(name).copied());
                    expr.rename(&|n| scope.node(n), &|d| scope.name(d));
                    self.deck.circuit.b(&name, kind, &a, &b, expr);
                } else {
                    let v = val(3)?;
                    match command.as_bytes().first() {
                        Some(b'r') => self.deck.circuit.r(&name, &a, &b, v),
                        Some(b'c') => self.deck.circuit.c(&name, &a, &b, v),
                        _ => self.deck.circuit.l(&name, &a, &b, v),
                    }
                }
            }
            'k' => {
                count(t, 4, 4)?;
                let (l1, l2, k) = (scope.name(name_token(t, 1)?), scope.name(name_token(t, 2)?), val(3)?);
                self.deck.circuit.k(&name, &l1, &l2, k);
            }
            'v' | 'i' => {
                count(t, 3, usize::MAX)?;
                let (p, n) = (node(1)?, node(2)?);
                let src = source(&t[3..], &scope.params, self.timing)?;
                if command.starts_with('v') {
                    self.deck.circuit.v(&name, &p, &n, src);
                } else {
                    self.deck.circuit.i(&name, &p, &n, src);
                }
            }
            'e' | 'g' | 'f' | 'h' => {
                if t.iter().skip(3).any(|s| matches!(s.as_str(), "poly" | "value" | "table")) {
                    return Err("unsupported controlled source form".into());
                }
                if command.starts_with('e') || command.starts_with('g') {
                    count(t, 6, 6)?;
                    let (p, n, cp, cn, gain) = (node(1)?, node(2)?, node(3)?, node(4)?, val(5)?);
                    let kind = if command.starts_with('e') { CtlKind::Vcvs } else { CtlKind::Vccs };
                    self.deck.circuit.vctl(&name, kind, &p, &n, &cp, &cn, gain);
                } else {
                    count(t, 5, 5)?;
                    let (p, n, ctrl, gain) = (node(1)?, node(2)?, scope.name(name_token(t, 3)?), val(4)?);
                    let kind = if command.starts_with('f') { CtlKind::Cccs } else { CtlKind::Ccvs };
                    self.deck.circuit.ictl(&name, kind, &p, &n, &ctrl, gain);
                }
            }
            'b' => {
                let (p, n) = (node(1)?, node(2)?);
                let (kind, text) = behavioural(&stmt.text)?;
                let mut expr = Expr::parse(&text.to_ascii_lowercase())?;
                expr.bind(&|name| scope.params.get(name).copied());
                expr.rename(&|n| scope.node(n), &|d| scope.name(d));
                self.deck.circuit.b(&name, kind, &p, &n, expr);
            }
            's' => {
                count(t, 6, 7)?;
                let (p, n, cp, cn) = (node(1)?, node(2)?, node(3)?, node(4)?);
                let model = scope.model(token(t, 5)?)?;
                let on = match t.get(6).map(String::as_str) {
                    Some("on") => true,
                    Some("off") | None => false,
                    Some(s) => return Err(format!("invalid switch state {s}")),
                };
                self.deck.circuit.sw(&name, &p, &n, &cp, &cn, &model, on);
            }
            't' => {
                count(t, 6, usize::MAX)?;
                let (p1, n1, p2, n2) = (node(1)?, node(2)?, node(3)?, node(4)?);
                let (mut z0, mut td, mut f, mut nl) = (None, None, None, 0.25);
                for (key, text) in assignments(&t[5..])? {
                    let v = expression(&text, &scope.params)?;
                    match key.as_str() {
                        "z0" | "zo" => z0 = Some(v),
                        "td" => td = Some(v),
                        "f" => f = Some(v),
                        "nl" => nl = v,
                        _ => return Err(format!("unsupported instance parameter {key}")),
                    }
                }
                // the delay, or nl wavelengths at f
                let td = td.or(f.map(|f| nl / f)).ok_or("a lossless line needs td=, or f= (with nl=)")?;
                let z0 = z0.ok_or("a lossless line needs z0=")?;
                self.deck.circuit.tline(&name, &p1, &n1, &p2, &n2, z0, td);
            }
            'o' => {
                count(t, 6, 6)?;
                let (p1, n1, p2, n2) = (node(1)?, node(2)?, node(3)?, node(4)?);
                let model = scope.model(token(t, 5)?)?;
                self.deck.circuit.ltra(&name, &p1, &n1, &p2, &n2, &model);
            }
            'w' => {
                count(t, 5, 6)?;
                let (p, n, ctrl) = (node(1)?, node(2)?, scope.name(name_token(t, 3)?));
                let model = scope.model(token(t, 4)?)?;
                let on = match t.get(5).map(String::as_str) {
                    Some("on") => true,
                    Some("off") | None => false,
                    Some(s) => return Err(format!("invalid switch state {s}")),
                };
                self.deck.circuit.csw(&name, &p, &n, &ctrl, &model, on);
            }
            'd' => {
                count(t, 4, 5)?;
                let (a, k, model) = (node(1)?, node(2)?, scope.model(token(t, 3)?)?);
                if t.len() == 5 && val(4)? != 1.0 {
                    warnings.push("area ignored".into());
                }
                self.deck.circuit.d(&name, &a, &k, &model);
            }
            'q' => {
                count(t, 5, 7)?;
                let model_at = if scope.models.contains_key(token(t, 4)?) { 4 } else { 5 };
                count(t, model_at + 1, model_at + 2)?;
                let (c, b, e) = (node(1)?, node(2)?, node(3)?);
                let s = if model_at == 4 { "0".into() } else { node(4)? };
                let model = scope.model(token(t, model_at)?)?;
                if t.len() > model_at + 1 && val(model_at + 1)? != 1.0 {
                    warnings.push("area ignored".into());
                }
                self.deck.circuit.q4(&name, &c, &b, &e, &s, &model);
            }
            'j' => {
                count(t, 5, 7)?;
                let (d, g, s) = (node(1)?, node(2)?, node(3)?);
                let model = scope.model(token(t, 4)?)?;
                let off = t.len() > 5 && t.last().is_some_and(|t| t == "off");
                let end = t.len() - usize::from(off);
                if end > 6 {
                    return Err("expected Jname d g s model [area] [off]".into());
                }
                let area = if end == 6 { val(5)? } else { 1.0 };
                self.deck.circuit.jfet(&name, &d, &g, &s, &model, area);
                self.deck.circuit.devices.j.last_mut().expect("the device just added").off = off;
            }
            'm' => {
                count(t, 6, usize::MAX)?;
                let (d, g, s, b) = (node(1)?, node(2)?, node(3)?, node(4)?);
                let model = scope.model(token(t, 5)?)?;
                // a bare `off` starts the device off; the rest are name=value
                let rest: Vec<String> = t[6..].iter().filter(|s| *s != "off").cloned().collect();
                let off = rest.len() != t.len() - 6;
                let mut set = Vec::new();
                for (key, text) in assignments(&rest)? {
                    let v = expression(&text, &scope.params)?;
                    match key.as_str() {
                        "w" | "l" | "ad" | "as" | "pd" | "ps" | "m" | "nrd" | "nrs" | "temp" => set.push((key, v)),
                        _ => warnings.push(format!("instance parameter {key} ignored")),
                    }
                }
                let get = |k: &str, default: f64| set.iter().rev().find(|(key, _)| key == k).map_or(default, |&(_, v)| v);
                self.deck.circuit.m(&name, &d, &g, &s, &b, &model, get("w", 100e-6), get("l", 100e-6));
                let m = self.deck.circuit.devices.m.last_mut().expect("the device just added");
                (m.ad, m.as_, m.pd, m.ps) = (get("ad", 0.0), get("as", 0.0), get("pd", 0.0), get("ps", 0.0));
                (m.mult, m.nrd, m.nrs, m.off) = (get("m", 1.0), get("nrd", 1.0), get("nrs", 1.0), off);
                m.temp = set.iter().rev().find(|(key, _)| key == "temp").map(|&(_, v)| v);
            }
            'x' => self.expand(t, scope, depth)?,
            letter => return Err(format!("unsupported element {letter}")),
        }
        Ok(warnings)
    }

    fn dot(&mut self, t: &[String], scope: &Scope) -> Result<Vec<String>> {
        let mut warnings = Vec::new();
        match token(t, 0)? {
            ".model" => {
                let name = scope.name(name_token(t, 1)?);
                let kind = match token(t, 2)? {
                    "d" => ModelKind::Diode,
                    "npn" => ModelKind::Npn,
                    "pnp" => ModelKind::Pnp,
                    "nmos" => ModelKind::Nmos,
                    "pmos" => ModelKind::Pmos,
                    "njf" => ModelKind::Njf,
                    "pjf" => ModelKind::Pjf,
                    "sw" => ModelKind::Sw,
                    "csw" => ModelKind::Csw,
                    "ltra" => ModelKind::Ltra,
                    kind => return Err(format!("unsupported model type {kind}")),
                };
                let mut args = &t[3..];
                if args.first().is_some_and(|t| t == "(") {
                    if !args.last().is_some_and(|t| t == ")") {
                        return Err("missing ')' in model".into());
                    }
                    args = &args[1..args.len() - 1];
                }
                let params = assignments(args)?.into_iter().map(|(k, text)| {
                    expression(&text, &scope.params).map(|v| (k, v))
                }).collect::<Result<Vec<_>>>()?;
                let params: Vec<_> = params.iter().map(|(k, v)| (k.as_str(), *v)).collect();
                self.deck.circuit.model(&name, kind, &params);
            }
            ".ic" | ".nodeset" => {
                let mut i = 1;
                let mut values = Vec::new();
                while i < t.len() {
                    if token(t, i)? != "v" || token(t, i + 1)? != "(" || token(t, i + 3)? != ")" || token(t, i + 4)? != "=" {
                        return Err("expected v(node)=value".into());
                    }
                    let n = scope.node(name_token(t, i + 2)?);
                    let v = value(token(t, i + 5)?, &scope.params)?;
                    values.push((n, v));
                    i += 6;
                }
                if values.is_empty() { return Err("missing initial condition".into()); }
                for (n, v) in values {
                    if token(t, 0)? == ".ic" { self.deck.circuit.set_ic(&n, v); }
                    else { self.deck.circuit.set_nodeset(&n, v); }
                }
            }
            ".temp" => {
                count(t, 2, 2)?;
                self.deck.circuit.temp_c = value(token(t, 1)?, &scope.params)?;
            }
            ".options" | ".option" => {
                let mut opts = self.deck.circuit.opts.clone();
                let mut temp = self.deck.circuit.temp_c;
                let mut i = 1;
                while i < t.len() {
                    let key = token(t, i)?;
                    i += 1;
                    let argument = if t.get(i).is_some_and(|t| t == "=") {
                        i += 1;
                        let argument = token(t, i)?;
                        i += 1;
                        Some(argument)
                    } else { None };
                    if key == "method" {
                        opts.method = match argument.ok_or("missing method value")? {
                            "trap" => Method::Trap,
                            "gear" => Method::Gear,
                            "euler" => Method::Euler,
                            method => return Err(format!("unknown integration method {method}")),
                        };
                        continue;
                    }
                    if !matches!(key, "reltol" | "abstol" | "vntol" | "chgtol" | "gmin" | "trtol" | "itl1" | "itl2" | "itl4" | "tnom" | "maxord" | "temp") {
                        if matches!(key, "=" | "(" | ")") { return Err("malformed option".into()); }
                        warnings.push(format!("unknown option {key}"));
                        continue;
                    }
                    let v = value(argument.ok_or_else(|| format!("missing value for option {key}"))?, &scope.params)?;
                    match key {
                        "reltol" => opts.reltol = v,
                        "abstol" => opts.abstol = v,
                        "vntol" => opts.vntol = v,
                        "chgtol" => opts.chgtol = v,
                        "gmin" => opts.gmin = v,
                        "trtol" => opts.trtol = v,
                        "itl1" => opts.itl1 = integer(v, u32::MAX)?,
                        "itl2" => opts.itl2 = integer(v, u32::MAX)?,
                        "itl4" => opts.itl4 = integer(v, u32::MAX)?,
                        "tnom" => opts.tnom = v,
                        "maxord" => opts.max_order = integer(v, 6)? as u8,
                        "temp" => temp = v,
                        _ => {}
                    }
                }
                self.deck.circuit.opts = opts;
                self.deck.circuit.temp_c = temp;
            }
            ".op" => {
                count(t, 1, 1)?;
                self.deck.analyses.push(Analysis::Op);
            }
            ".tran" => {
                let (_, spec, start) = transient(t, &scope.params)?;
                if start != 0.0 { warnings.push("tstart ignored".into()); }
                self.deck.analyses.push(Analysis::Tran(spec));
            }
            ".dc" => {
                if t.len() != 5 && t.len() != 9 {
                    return Err("expected one or two DC sweeps".into());
                }
                let mut sweeps = Vec::new();
                for args in t[1..].chunks_exact(4) {
                    let device = name_token(args, 0)?;
                    let leaf = if device.starts_with('x') { device.rsplit('.').next().unwrap_or(device) } else { device };
                    let key = match leaf.as_bytes().first() {
                        Some(b'r') => "value",
                        Some(b'v' | b'i') => "dc",
                        _ => return Err(format!("unsupported DC sweep device {device}")),
                    };
                    let a = value(token(args, 1)?, &scope.params)?;
                    let b = value(token(args, 2)?, &scope.params)?;
                    let step = value(token(args, 3)?, &scope.params)?;
                    let points = (b - a) / step;
                    if step == 0.0 || !points.is_finite() || points < 0.0 || points.round() >= u32::MAX as f64 {
                        return Err("invalid DC sweep".into());
                    }
                    sweeps.push(DcSweep::lin(&scope.name(device), key, a, b, step));
                }
                sweeps.reverse();
                self.deck.analyses.push(Analysis::Dc(sweeps));
            }
            ".ac" => {
                count(t, 5, 5)?;
                let n = integer(value(token(t, 2)?, &scope.params)?, u32::MAX)?;
                let fstart = value(token(t, 3)?, &scope.params)?;
                let fstop = value(token(t, 4)?, &scope.params)?;
                let kind = token(t, 1)?;
                if fstart < 0.0 || (kind != "lin" && fstart == 0.0) || fstop < fstart || !(fstop * (1.0 + 1e-9)).is_finite() {
                    return Err("invalid AC frequency range".into());
                }
                let sweep = match kind {
                    "dec" => AcSweep::Dec { n, fstart, fstop },
                    "oct" => AcSweep::Oct { n, fstart, fstop },
                    "lin" => AcSweep::Lin { n, fstart, fstop },
                    other => return Err(format!("unknown AC sweep {other}")),
                };
                self.deck.analyses.push(Analysis::Ac(sweep));
            }
            ".include" | ".lib" => return Err("files are not read".into()),
            ".save" | ".print" | ".plot" | ".probe" | ".meas" | ".measure" | ".four" | ".width" => warnings.push(format!("{} ignored", token(t, 0)?)),
            ".ends" => return Err(".ends without a subcircuit".into()),
            ".endc" => return Err(".endc without a control block".into()),
            _ => warnings.push(format!("unknown statement {}", token(t, 0)?)),
        }
        Ok(warnings)
    }
}

/// A B line's kind and expression, from its text as written: `Bname n+ n- V=expr` or `I=expr`,
/// the expression to the end of the line, in braces or quotes or bare.
fn behavioural(text: &str) -> Result<(BKind, &str)> {
    let mut rest = text;
    for _ in 0..3 {
        rest = rest.trim_start();
        let end = rest.find(char::is_whitespace).ok_or("missing V=expr or I=expr")?;
        rest = &rest[end..];
    }
    let rest = rest.trim_start();
    let kind = match rest.as_bytes().first() {
        Some(b'v' | b'V') => BKind::V,
        Some(b'i' | b'I') => BKind::I,
        _ => return Err("missing V=expr or I=expr".into()),
    };
    let expr = rest[1..].trim_start().strip_prefix('=').ok_or("missing V=expr or I=expr")?.trim();
    let expr = if let Some(inner) = expr.strip_prefix('{') {
        inner.strip_suffix('}').ok_or("missing '}' in expression")?
    } else if let Some(inner) = expr.strip_prefix('\'') {
        inner.strip_suffix('\'').ok_or("missing quote in expression")?
    } else {
        expr
    };
    Ok((kind, expr))
}

fn integer(v: f64, max: u32) -> Result<u32> {
    if v >= 1.0 && v <= max as f64 && v.fract() == 0.0 {
        Ok(v as u32)
    } else {
        Err(format!("expected positive integer at most {max}, got {v}"))
    }
}

fn transient(t: &[String], params: &Params) -> Result<(Timing, TranSpec, f64)> {
    let uic = t.last().is_some_and(|s| s == "uic");
    let t = if uic { &t[..t.len() - 1] } else { t };
    count(t, 3, 5)?;
    let step = value(token(t, 1)?, params)?;
    let stop = value(token(t, 2)?, params)?;
    let start = t.get(3).map(|t| value(t, params)).transpose()?.unwrap_or(0.0);
    let max_step = t.get(4).map(|t| value(t, params)).transpose()?;
    if step <= 0.0 || stop <= 0.0 || start < 0.0 || max_step.is_some_and(|v| v <= 0.0) {
        return Err("invalid transient time or step".into());
    }
    Ok((Timing { step, stop }, TranSpec { tstop: stop, max_step, uic }, start))
}

fn source_keyword(t: &str) -> bool {
    matches!(t, "dc" | "ac" | "pulse" | "sin" | "exp" | "pwl" | "sffm" | "am")
}

fn source(tokens: &[String], params: &Params, timing: Timing) -> Result<Source> {
    let mut src = Source::wave(Wave::None);
    let mut has_ac = false;
    let mut has_wave = false;
    let mut i = 0;
    while i < tokens.len() {
        let keyword = token(tokens, i)?;
        i += 1;
        match keyword {
            "dc" => {
                if src.dc.is_some() { return Err("duplicate DC source value".into()); }
                src.dc = Some(value(token(tokens, i)?, params)?);
                i += 1;
            }
            "ac" => {
                if has_ac { return Err("duplicate AC source value".into()); }
                has_ac = true;
                src.ac_mag = value(token(tokens, i)?, params)?;
                i += 1;
                if let Some(t) = tokens.get(i).filter(|t| !source_keyword(t)) {
                    src.ac_phase = value(t, params)?;
                    i += 1;
                }
            }
            "pulse" | "sin" | "exp" | "pwl" | "sffm" | "am" => {
                if has_wave { return Err("multiple source waveforms".into()); }
                has_wave = true;
                let parens = tokens.get(i).is_some_and(|t| t == "(");
                if parens { i += 1; }
                let mut args = Vec::new();
                while let Some(t) = tokens.get(i) {
                    if (parens && t == ")") || (!parens && source_keyword(t)) { break; }
                    args.push(value(t, params)?);
                    i += 1;
                }
                if parens {
                    if token(tokens, i)? != ")" { return Err("missing ')' in source waveform".into()); }
                    i += 1;
                }
                src.wave = waveform(keyword, &args, timing)?;
            }
            _ => {
                if src.dc.is_some() || i != 1 { return Err(format!("unexpected source token {keyword}")); }
                src.dc = Some(value(keyword, params)?);
            }
        }
    }
    if src.dc.is_none() && !has_wave { src.dc = Some(0.0); }
    Ok(src)
}

fn waveform(kind: &str, args: &[f64], timing: Timing) -> Result<Wave> {
    let (min, max) = match kind {
        "pulse" => (2, 7),
        "sin" | "exp" => (2, 6),
        "pwl" => (2, usize::MAX),
        "sffm" => (2, 5),
        "am" => (4, 5),
        _ => return Err(format!("unknown waveform {kind}")),
    };
    if args.len() < min || args.len() > max { return Err(format!("wrong argument count for {kind}")); }
    let arg = |i: usize, default: f64| args.get(i).copied().unwrap_or(default);
    let freq = if timing.stop > 0.0 { 1.0 / timing.stop } else { 0.0 };
    let wave = match kind {
        "pulse" => Wave::Pulse { v1: arg(0, 0.0), v2: arg(1, 0.0), td: arg(2, 0.0), tr: arg(3, timing.step), tf: arg(4, timing.step), pw: arg(5, timing.stop), per: arg(6, timing.stop) },
        "sin" => Wave::Sin { vo: arg(0, 0.0), va: arg(1, 0.0), freq: arg(2, freq), td: arg(3, 0.0), theta: arg(4, 0.0), phase: arg(5, 0.0) },
        "exp" => Wave::Exp { v1: arg(0, 0.0), v2: arg(1, 0.0), td1: arg(2, 0.0), tau1: arg(3, timing.step), td2: arg(4, arg(2, 0.0) + timing.step), tau2: arg(5, timing.step) },
        "pwl" => {
            if args.len() % 2 != 0 { return Err("odd PWL argument count".into()); }
            let points: Vec<_> = args.chunks_exact(2).map(|p| (p[0], p[1])).collect();
            if points.windows(2).any(|w| w[1].0 < w[0].0) { return Err("decreasing PWL times".into()); }
            Wave::Pwl(points)
        }
        "sffm" => Wave::Sffm { vo: arg(0, 0.0), va: arg(1, 0.0), fc: arg(2, freq), mdi: arg(3, 0.0), fs: arg(4, freq) },
        "am" => Wave::Am { va: arg(0, 0.0), vo: arg(1, 0.0), fm: arg(2, 0.0), fc: arg(3, 0.0), td: arg(4, 0.0) },
        _ => return Err(format!("unknown waveform {kind}")),
    };
    // Zero EXP time constants are well-defined by Wave at t > td (an instantaneous
    // edge); zero pulse slopes/periods and zero frequencies are also handled there.
    let valid = match &wave {
        Wave::Pulse { td, tr, tf, pw, .. } => (td + tr + pw + tf).is_finite(),
        Wave::Sin { freq, .. } => freq.is_finite(),
        Wave::Exp { td2, .. } => td2.is_finite(),
        Wave::Sffm { fc, fs, .. } => fc.is_finite() && fs.is_finite(),
        Wave::Noise { sigma, dt, base, .. } => sigma.is_finite() && dt.is_finite() && *dt > 0.0 && base.is_finite() && *base > 0.0,
        Wave::Am { .. } | Wave::Pwl(_) | Wave::None => true,
    };
    if !valid || !wave.at(0.0).is_some_and(f64::is_finite) {
        return Err(format!("invalid {kind} waveform arguments"));
    }
    Ok(wave)
}

//! The requirements document's reader: literal `require` and `waive` statements read from the
//! Splash syntax tree, each with its span, every mistake a located error.
//!
//! Cost: one parse of the text (a second, line by line, only when the whole text does not parse)
//! and O(statements) after it.

use crate::{Bound, CountOf, Dim, Family, Measure, ReqDoc, Requirement, Value, Waiver};
use fab_elec_core::{Diag, DocId, Sel};
use makepad_script::syntax::{self, Field, Kind, Node};
use makepad_script::units::UnitTable;
use std::ops::Range;

/// The document's units: the board document's lengths, times up to hours, and masses.
pub fn units() -> UnitTable {
    UnitTable::standard()
        .with("mil", 254.0, 10_000_000.0)
        .with("inch", 254.0, 10_000.0)
        .with("um", 1.0, 1e6)
        .with("min", 60.0, 1.0)
        .with("h", 3600.0, 1.0)
        .with("g", 1.0, 1.0)
        .with("kg", 1000.0, 1.0)
}

const MEASURES: &str = "board(\"unit\").size|layers|parts|unplaced|unrouted, checks(@erc|@drc|@system|@case|@custom|@all).errors|warnings, case(\"unit\").fits, bench(\"bench\"), bench(\"bench\").expect(\"name\"), print.time, print.filament";

/// Read a requirements document. Statements that read are kept even when others do not; the
/// errors are located in `doc`. Names repeated in this document are errors at the second.
pub fn read(doc: DocId, text: &str) -> (ReqDoc, Vec<Diag>) {
    let mut out = ReqDoc { doc, requirements: Vec::new(), waivers: Vec::new() };
    let mut errors = Vec::new();
    let units = units();
    match syntax::parse_with(text, &units) {
        Ok(tree) => {
            for n in tree.statements() {
                Read { doc, text, offset: 0, errors: &mut errors }.statement(n, &mut out);
            }
        }
        Err(_) => {
            // Statements are one line each, so a broken line need not hide the others.
            let mut offset = 0u32;
            for line in text.split_inclusive('\n') {
                match syntax::parse_with(line, &units) {
                    Ok(tree) => {
                        for n in tree.statements() {
                            Read { doc, text: line, offset, errors: &mut errors }.statement(n, &mut out);
                        }
                    }
                    Err(e) => {
                        let start = offset + e.at as u32;
                        let size = line.get(e.at..).and_then(|s| s.chars().next()).map_or(0, |c| c.len_utf8()) as u32;
                        errors.push(located(doc, start..start + size, format!("syntax: {}", e.message)));
                    }
                }
                offset += line.len() as u32;
            }
        }
    }
    errors.sort_by_key(|e| match e.sel.first() { Some(Sel::Source { start, .. }) => *start, _ => 0 });
    (out, errors)
}

fn located(doc: DocId, span: Range<u32>, message: impl Into<String>) -> Diag {
    Diag::error(message, vec![Sel::Source { doc, start: span.start, end: span.end }])
}

struct Read<'a> {
    doc: DocId,
    text: &'a str,
    offset: u32,
    errors: &'a mut Vec<Diag>,
}

impl Read<'_> {
    fn span(&self, n: &Node) -> Range<u32> {
        self.offset + n.span.start as u32..self.offset + n.span.end as u32
    }

    fn error<T>(&mut self, n: &Node, message: impl Into<String>) -> Option<T> {
        let span = self.span(n);
        self.errors.push(located(self.doc, span, message));
        None
    }

    fn statement(&mut self, n: &Node, out: &mut ReqDoc) {
        match n.as_call() {
            Some(("require", args)) => {
                if let Some(r) = self.require(n, args) {
                    if out.requirements.iter().any(|o| o.name == r.name) {
                        self.error::<()>(&args[0], format!("two requirements are named {:?}: give each its own words", r.name));
                    } else {
                        out.requirements.push(r);
                    }
                }
            }
            Some((word @ ("waive" | "propose_waiver"), args)) => {
                let proposed = word == "propose_waiver";
                if let Some(w) = self.waive(n, args, word, proposed) {
                    if out.waivers.iter().any(|o| o.name == w.name && o.proposed == proposed) {
                        let twice = if proposed { "has two proposed waivers" } else { "is waived twice" };
                        self.error::<()>(&args[0], format!("{:?} {twice}", w.name));
                    } else {
                        out.waivers.push(w);
                    }
                }
            }
            _ => {
                self.error::<()>(n, "a requirements document holds require(name, measure, bound), waive(name, reason) and propose_waiver(name, reason) statements");
            }
        }
    }

    fn name(&mut self, n: &Node, what: &str) -> Option<String> {
        match &n.kind {
            Kind::Str(s) if !s.trim().is_empty() => Some(s.trim().to_owned()),
            Kind::Str(_) => self.error(n, format!("{what} is empty: write it in the person's words")),
            _ => self.error(n, format!("{what} must be a string")),
        }
    }

    fn require(&mut self, n: &Node, args: &[Node]) -> Option<Requirement> {
        if !(2..=3).contains(&args.len()) {
            return self.error(n, "require takes a name, a measure and a bound: require(\"fits\", board(\"main\").size, {max: [60mm 40mm]})");
        }
        let name = self.name(&args[0], "the requirement's name")?;
        let measure = match measure(&args[1], self.text) {
            Ok(m) => m,
            Err(e) => return self.error(&args[1], format!("{name:?}: {e}")),
        };
        let dim = measure.dim();
        let bound = match (args.get(2), dim) {
            (None, Dim::Pass) => Bound::Holds,
            (Some(b), Dim::Pass) => return self.error(b, format!("{name:?}: {} passes or fails and takes no bound", args[1].text(self.text))),
            (None, _) => return self.error(n, format!("{name:?}: give a bound: {{max: ..}}, {{min: ..}}, both, or {{is: ..}}")),
            (Some(b), dim) => match bound(b, self.text, dim) {
                Ok(bound) => bound,
                Err((at, e)) => {
                    let span = self.offset + at.start as u32..self.offset + at.end as u32;
                    self.errors.push(located(self.doc, span, format!("{name:?}: {e}")));
                    return None;
                }
            },
        };
        Some(Requirement {
            name,
            measure,
            bound,
            span: self.span(n),
            measure_text: args[1].text(self.text).to_owned(),
            bound_text: args.get(2).map_or(String::new(), |b| b.text(self.text).to_owned()),
        })
    }

    fn waive(&mut self, n: &Node, args: &[Node], word: &str, proposed: bool) -> Option<Waiver> {
        if args.len() != 2 {
            return self.error(n, format!("{word} takes the requirement's name and the reason: {word}(\"fits\", \"why\")"));
        }
        let name = self.name(&args[0], "the waived requirement's name")?;
        let reason = self.name(&args[1], "the reason")?;
        Some(Waiver { name, reason, span: self.span(n), proposed })
    }
}

/// A string argument, the only one a call of the catalog takes.
fn one_string(args: &[Node], call: &str) -> Result<String, String> {
    match args {
        [n] => n.string().map(str::to_owned).filter(|s| !s.is_empty()).ok_or_else(|| format!("{call}(..) takes a name as a string")),
        _ => Err(format!("{call}(..) takes one name as a string")),
    }
}

/// A measure of the catalog, from its call chain.
fn measure(n: &Node, text: &str) -> Result<Measure, String> {
    let unknown = || format!("{} is not a measure; the measures are {MEASURES}", n.text(text));
    match &n.kind {
        Kind::Ident(path) => match path.as_str() {
            "print.time" => Ok(Measure::PrintTime),
            "print.filament" => Ok(Measure::PrintFilament),
            _ => Err(unknown()),
        },
        Kind::Member { base, name, .. } => {
            let Some((call, args)) = base.as_call() else { return Err(unknown()) };
            match (call, name.as_str()) {
                ("board", field) => {
                    let unit = one_string(args, "board")?;
                    match field {
                        "size" => Ok(Measure::BoardSize(unit)),
                        "layers" => Ok(Measure::BoardLayers(unit)),
                        "parts" => Ok(Measure::BoardParts(unit)),
                        "unplaced" => Ok(Measure::BoardUnplaced(unit)),
                        "unrouted" => Ok(Measure::BoardUnrouted(unit)),
                        _ => Err(format!("a board has size, layers, parts, unplaced and unrouted, not {field}")),
                    }
                }
                ("checks", field) => {
                    let count = match field {
                        "errors" => CountOf::Errors,
                        "warnings" => CountOf::Warnings,
                        _ => return Err(format!("checks(..) counts errors or warnings, not {field}")),
                    };
                    let (family, unit) = checks_args(args)?;
                    Ok(Measure::Checks { family, unit, count })
                }
                ("case", "fits") => Ok(Measure::CaseFits(one_string(args, "case")?)),
                ("case", field) => Err(format!("a case has fits, not {field}")),
                _ => Err(unknown()),
            }
        }
        Kind::Call { callee, args, .. } => {
            match &callee.kind {
                Kind::Ident(name) if name == "bench" => Ok(Measure::Bench(one_string(args, "bench")?)),
                Kind::Member { base, name, .. } if name == "expect" => match base.as_call() {
                    Some(("bench", bench_args)) => Ok(Measure::BenchExpect { bench: one_string(bench_args, "bench")?, expect: one_string(args, "expect")? }),
                    _ => Err(unknown()),
                },
                _ => Err(unknown()),
            }
        }
        _ => Err(unknown()),
    }
}

/// `checks(@family)` or `checks(@family, {unit: "u"})`.
fn checks_args(args: &[Node]) -> Result<(Family, Option<String>), String> {
    let family_of = |n: &Node| {
        n.name().and_then(|s| Family::SPELLINGS.iter().find(|(f, _)| *f == s).map(|(_, f)| *f))
            .ok_or_else(|| "checks(..) takes a family: @erc, @drc, @system, @route, @case, @custom or @all".to_owned())
    };
    match args {
        [f] => Ok((family_of(f)?, None)),
        [f, opts] => {
            let family = family_of(f)?;
            let Kind::Object { ty: None, fields, .. } = &opts.kind else { return Err("checks(..)'s options are an object: {unit: \"main\"}".into()) };
            let mut unit = None;
            for field in fields {
                match (field.name.as_deref(), field.value.string()) {
                    (Some("unit"), Some(s)) if unit.is_none() && !s.is_empty() => unit = Some(s.to_owned()),
                    (Some("unit"), _) => return Err("checks(..)'s unit is one unit's name, given once".into()),
                    (other, _) => return Err(format!("checks(..) takes only unit, not {}", other.unwrap_or("a nameless field"))),
                }
            }
            Ok((family, unit))
        }
        _ => Err("checks(..) takes a family and optionally {unit: \"main\"}".into()),
    }
}

/// The unit family of a literal: the registered suffix it ends with, longest first.
fn suffix<'t>(literal: &'t str) -> Option<&'t str> {
    let table = units();
    let best = table.entries().iter().filter(|e| literal.ends_with(&e.suffix)).max_by_key(|e| e.suffix.len())?;
    Some(&literal[literal.len() - best.suffix.len()..])
}

/// A mistake in a bound: where, and what.
type Fail = (Range<usize>, String);

/// A number of `dim`, in its base unit: a length for a size, seconds, grams, or a plain count.
fn number(n: &Node, text: &str, dim: Dim) -> Result<f64, Fail> {
    number_at(n, text, dim).map_err(|e| (n.span.clone(), e))
}

fn number_at(n: &Node, text: &str, dim: Dim) -> Result<f64, String> {
    let (v, literal) = match &n.kind {
        Kind::Number(v) => (*v, n),
        Kind::Unary { op, operand } if op == "-" => match operand.kind {
            Kind::Number(v) => (-v, operand.as_ref()),
            _ => return Err(format!("{} is not a number", n.text(text))),
        },
        _ => return Err(format!("{} is not a number", n.text(text))),
    };
    let unit = suffix(literal.text(text));
    let (ok, want) = match dim {
        Dim::Size => (matches!(unit, Some("m" | "cm" | "mm" | "mil" | "inch" | "um")), "a length (mm)"),
        Dim::Time => (matches!(unit, Some("s" | "ms" | "min" | "h")), "a time (s, min, h)"),
        Dim::Mass => (matches!(unit, Some("g" | "kg")), "a mass (g, kg)"),
        Dim::Count => (unit.is_none() && v.fract() == 0.0, "a whole number without a unit"),
        Dim::Pass => (false, "nothing"),
    };
    if !ok || !v.is_finite() {
        return Err(format!("{} must be {want}", n.text(text)));
    }
    if v < 0.0 {
        return Err(format!("{} must not be negative", n.text(text)));
    }
    Ok(v)
}

/// A bound value of `dim`: `[w h]` for a size, else one number.
fn value(n: &Node, text: &str, dim: Dim) -> Result<Value, Fail> {
    if dim == Dim::Size {
        let not_size = || (n.span.clone(), format!("{} must be a size, [w h] in lengths", n.text(text)));
        let Kind::Array { items, .. } = &n.kind else { return Err(not_size()) };
        let [w, h] = items.as_slice() else { return Err(not_size()) };
        return Ok(Value::Size([number(w, text, dim)?, number(h, text, dim)?]));
    }
    Ok(Value::Scalar(number(n, text, dim)?))
}

/// `{min max}`, `{is tol}`.
fn bound(n: &Node, text: &str, dim: Dim) -> Result<Bound, Fail> {
    let whole = |message: &str| (n.span.clone(), message.to_owned());
    let Kind::Object { ty: None, fields, .. } = &n.kind else { return Err(whole("the bound is an object: {max: ..}, {min: ..} or {is: ..}")) };
    let field = |key: &str| -> Result<Option<&Field>, Fail> {
        let mut found = fields.iter().filter(|f| f.name.as_deref() == Some(key));
        let first = found.next();
        if found.next().is_some() { return Err(whole(&format!("{key} is given twice"))); }
        Ok(first)
    };
    if let Some(f) = fields.iter().find(|f| !matches!(f.name.as_deref(), Some("min" | "max" | "is" | "tol"))) {
        return Err(whole(&format!("a bound takes min, max, is and tol, not {}", f.name.as_deref().unwrap_or("a nameless field"))));
    }
    let get = |key: &str| -> Result<Option<Value>, Fail> { field(key)?.map(|f| value(&f.value, text, dim)).transpose() };
    let (min, max, is) = (get("min")?, get("max")?, get("is")?);
    let tol = field("tol")?;
    match (min, max, is) {
        (None, None, None) => Err(whole("a bound needs max, min, both, or is")),
        (None, None, Some(value)) => {
            // A size's tolerance is one length for both sides.
            let tol = tol.map(|f| number(&f.value, text, dim)).transpose()?;
            Ok(Bound::Is { value, tol })
        }
        (_, _, Some(_)) => Err(whole("is stands alone (with tol); use min and max for a range")),
        (min, max, None) => {
            if tol.is_some() {
                return Err(whole("tol goes with is"));
            }
            let above = |a: Value, b: Value| match (a, b) {
                (Value::Scalar(a), Value::Scalar(b)) => a > b,
                (Value::Size(a), Value::Size(b)) => a[0] > b[0] || a[1] > b[1],
                _ => false,
            };
            if min.zip(max).is_some_and(|(lo, hi)| above(lo, hi)) {
                return Err(whole("min is above max"));
            }
            Ok(Bound::Range { min, max })
        }
    }
}

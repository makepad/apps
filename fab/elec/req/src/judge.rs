//! Each requirement held against the facts: pass, fail, unknown with why, or waived with the
//! person's reason. A fact that is not current (a check still running, a slice of an older plate)
//! shows its last value but answers unknown: unknown never counts as met.

use crate::{Bound, CountOf, Dim, Family, Measure, ReqDoc, Requirement, Value};
use fab_elec_core::{Diag, DocId, Sel, Severity};
use std::ops::Range;

/// What Fab's producers measured, as plain data. The host gathers it off the UI thread.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Facts {
    /// Every unit of the design, by name.
    pub units: Vec<UnitFacts>,
    /// The findings list; None before any check has run.
    pub checks: Option<CheckFacts>,
    /// Per board unit: its case checks.
    pub cases: Vec<CaseFacts>,
    /// Every bench of the design, with its last counted check.
    pub benches: Vec<BenchFacts>,
    /// The slice estimate; None before the plate was sliced.
    pub print: Option<PrintFacts>,
    /// Which producers outside the design this host reads.
    pub producers: Producers,
}

/// Which of the producers that run outside the design the host reads: the case checks, the
/// bench's counted check, the slice estimate. A requirement on one it does not read answers
/// unknown, "not read here", never "not run" (a host without the app's case, bench or slicer).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Producers {
    pub cases: bool,
    pub benches: bool,
    pub print: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct UnitFacts {
    pub unit: String,
    /// None: the unit has no board.
    pub board: Option<BoardMeasures>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BoardMeasures {
    /// The outline's bounding box in metres; None without an outline.
    pub size: Option<[f64; 2]>,
    pub layers: u32,
    pub parts: u32,
    pub unplaced: u32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CheckFacts {
    /// False while a check still runs on the newest edit.
    pub current: bool,
    /// The checks still running, in words.
    pub pending: Vec<String>,
    pub findings: Vec<FindingFact>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FindingFact {
    /// The finding's stable id.
    pub id: u64,
    /// Its unit's name; None for a finding across units.
    pub unit: Option<String>,
    pub family: Family,
    pub severity: Severity,
    /// An `Unconnected` finding: a connection the board does not make yet.
    pub unconnected: bool,
}

/// One board's case checks, once they have run on it; a board without an entry has not been
/// checked yet.
#[derive(Clone, Debug, PartialEq)]
pub struct CaseFacts {
    pub unit: String,
    /// Whether the plate has case parts for this board.
    pub has_case: bool,
    /// False while the case checks have not run on the newest design and plate.
    pub current: bool,
    /// The error findings of the case checks, by stable id.
    pub errors: Vec<u64>,
    /// Why the newest case evaluation did not check this board, when it failed.
    pub failed: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BenchFacts {
    pub bench: String,
    /// The last counted check (Check bench); None before one ran.
    pub run: Option<BenchRun>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BenchRun {
    /// False once the design or the firmware changed after it ran.
    pub current: bool,
    /// When it ran, in the host's clock (seconds).
    pub at: f64,
    pub outcomes: Vec<Outcome>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Outcome {
    pub name: String,
    pub verdict: Verdict,
    /// The judged value as the bench words it (`4.98 mA`), when there is one.
    pub value: Option<String>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Pass,
    Fail,
    NotJudged,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PrintFacts {
    /// False once the plate changed after the estimate.
    pub current: bool,
    /// When it was estimated, in the host's clock (seconds).
    pub at: f64,
    pub seconds: f64,
    pub grams: f64,
}

/// A requirement's answer.
#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    Pass,
    Fail,
    /// It cannot be measured now; why, in words the person acts on.
    Unknown(String),
    /// The person accepted it with a reason; `was` is what it answers without the waiver.
    Waived { reason: String, was: Box<Status> },
}

impl Status {
    pub fn word(&self) -> &'static str {
        match self {
            Status::Pass => "pass",
            Status::Fail => "fail",
            Status::Unknown(_) => "unknown",
            Status::Waived { .. } => "waived",
        }
    }
}

/// Where a row's evidence is shown.
#[derive(Clone, Debug, PartialEq)]
pub enum Focus {
    /// A unit's board.
    Unit(String),
    /// Findings in the Checks list (the row's `findings`).
    Findings,
    /// A board's case.
    Case(String),
    /// A bench in the Bench editor.
    Bench(String),
    /// The slice.
    Print,
}

/// One requirement, judged.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub name: String,
    pub doc: DocId,
    pub span: Range<u32>,
    /// The measure as written.
    pub measure: String,
    /// The bound in words: `max 60 x 40 mm`, `holds`.
    pub bound: String,
    pub status: Status,
    /// The measured value in words, the last one when stale.
    pub value: Option<String>,
    /// What measured it: `board outline`, `checks`, `case checks`, `Check bench`, `slice`.
    pub source: &'static str,
    /// The value is from before the newest edit.
    pub stale: bool,
    /// When it was measured, in the host's clock, for producers that run on request.
    pub measured_at: Option<f64>,
    /// The findings behind it, by stable id.
    pub findings: Vec<u64>,
    pub focus: Focus,
    /// A waiver the AI proposed, waiting for the person: its reason. It waives nothing.
    pub proposed: Option<String>,
}

/// Unmeasured words and the source statement that supplied them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WishRow {
    pub doc: DocId,
    pub words: String,
    pub span: Range<u32>,
}

/// Every requirement of the project, judged, in document order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Report {
    pub rows: Vec<Row>,
    pub wishes: Vec<WishRow>,
    pub brief: Option<WishRow>,
    /// Located mistakes across documents: a name used twice, a waiver of no requirement.
    pub notes: Vec<Diag>,
    pub pass: usize,
    pub fail: usize,
    pub unknown: usize,
    pub waived: usize,
    /// Rows with a proposed waiver waiting for the person (counted in their own status too).
    pub proposed: usize,
}

impl Report {
    /// Every requirement passes or is waived: nothing fails and nothing is unknown.
    pub fn met(&self) -> bool {
        !self.rows.is_empty() && self.fail == 0 && self.unknown == 0
    }

    /// Rows that pass or are waived, of all rows.
    pub fn met_count(&self) -> (usize, usize) {
        (self.pass + self.waived, self.rows.len())
    }

    pub fn row(&self, name: &str) -> Option<&Row> {
        self.rows.iter().find(|r| r.name == name)
    }
}

/// What a measure read: a value or a pass, with whether it is current.
enum Got {
    Value(Value),
    Holds { ok: bool, shown: String },
    Unknown(String),
}

struct Measured {
    got: Got,
    /// None: current. Some(why): the value is from before the newest edit.
    stale: Option<String>,
    source: &'static str,
    at: Option<f64>,
    findings: Vec<u64>,
    focus: Focus,
}

impl Measured {
    fn new(got: Got, source: &'static str, focus: Focus) -> Measured {
        Measured { got, stale: None, source, at: None, findings: Vec::new(), focus }
    }
    fn unknown(why: impl Into<String>, source: &'static str, focus: Focus) -> Measured {
        Measured::new(Got::Unknown(why.into()), source, focus)
    }
}

/// Judge every requirement of `docs` against `facts`. O(requirements x findings + facts).
pub fn judge(docs: &[ReqDoc], facts: &Facts) -> Report {
    judge_with_unavailable(docs, facts, |_| None)
}

/// Judge with a reason for each measure whose producer has no current evidence for this
/// design. The ordinary cross-document naming and waiver rules apply to those rows too.
pub fn judge_with_unavailable<'a>(docs: &[ReqDoc], facts: &Facts, unavailable: impl Fn(&Measure) -> Option<&'a str>) -> Report {
    let mut report = Report::default();
    let mut seen: Vec<&str> = Vec::new();
    for doc in docs {
        report.wishes.extend(doc.wishes.iter().map(|w| WishRow { doc: doc.doc, words: w.words.clone(), span: w.span.clone() }));
        if let Some(b) = &doc.brief {
            if report.brief.is_some() {
                report.notes.push(Diag::error("the project has one brief: this one repeats it", vec![source(doc.doc, &b.span)]));
            } else {
                report.brief = Some(WishRow { doc: doc.doc, words: b.words.clone(), span: b.span.clone() });
            }
        }
        for r in &doc.requirements {
            if seen.contains(&r.name.as_str()) {
                report.notes.push(Diag::error(format!("two requirements are named {:?}: give each its own words", r.name), vec![source(doc.doc, &r.span)]));
                continue;
            }
            seen.push(&r.name);
            report.rows.push(row(doc.doc, r, facts, unavailable(&r.measure)));
        }
    }
    for doc in docs {
        for w in &doc.waivers {
            if w.proposed {
                match report.rows.iter_mut().find(|row| row.name == w.name) {
                    Some(row) => {
                        row.proposed = Some(w.reason.clone());
                        report.proposed += 1;
                    }
                    None => report.notes.push(Diag::warning(format!("propose_waiver names {:?}, which no requirement is named", w.name), vec![source(doc.doc, &w.span)])),
                }
                continue;
            }
            match report.rows.iter_mut().find(|row| row.name == w.name) {
                Some(row) if !matches!(row.status, Status::Waived { .. }) => {
                    let was = std::mem::replace(&mut row.status, Status::Pass);
                    row.status = Status::Waived { reason: w.reason.clone(), was: Box::new(was) };
                }
                Some(_) => report.notes.push(Diag::error(format!("{:?} is waived twice", w.name), vec![source(doc.doc, &w.span)])),
                None => report.notes.push(Diag::warning(format!("waive names {:?}, which no requirement is named", w.name), vec![source(doc.doc, &w.span)])),
            }
        }
    }
    for row in &report.rows {
        match row.status {
            Status::Pass => report.pass += 1,
            Status::Fail => report.fail += 1,
            Status::Unknown(_) => report.unknown += 1,
            Status::Waived { .. } => report.waived += 1,
        }
    }
    report
}

fn source(doc: DocId, span: &Range<u32>) -> Sel {
    Sel::Source { doc, start: span.start, end: span.end }
}

fn row(doc: DocId, r: &Requirement, facts: &Facts, unavailable: Option<&str>) -> Row {
    let dim = r.measure.dim();
    let m = measure(&r.measure, facts);
    let got = unavailable.map_or(m.got, |why| Got::Unknown(why.to_owned()));
    let (status, value) = match &got {
        Got::Unknown(why) => (Status::Unknown(why.clone()), None),
        Got::Value(v) => {
            let shown = show(*v, dim);
            match &m.stale {
                Some(why) => (Status::Unknown(format!("stale: {why}")), Some(shown)),
                None => (if holds(*v, &r.bound, dim) { Status::Pass } else { Status::Fail }, Some(shown)),
            }
        }
        Got::Holds { ok, shown } => match &m.stale {
            Some(why) => (Status::Unknown(format!("stale: {why}")), Some(shown.clone())),
            None => (if *ok { Status::Pass } else { Status::Fail }, Some(shown.clone())),
        },
    };
    Row {
        name: r.name.clone(),
        doc,
        span: r.span.clone(),
        measure: r.measure_text.clone(),
        bound: bound_words(&r.bound, dim),
        status,
        value,
        source: m.source,
        stale: m.stale.is_some(),
        measured_at: m.at,
        findings: m.findings,
        focus: m.focus,
        proposed: None,
    }
}

/// A unit's facts, or why there are none.
fn unit<'f>(facts: &'f Facts, name: &str) -> Result<&'f UnitFacts, String> {
    facts.units.iter().find(|u| u.unit == name).ok_or_else(|| {
        let names: Vec<&str> = facts.units.iter().map(|u| u.unit.as_str()).collect();
        if names.is_empty() { format!("the design has no unit named {name:?}: it has none") } else { format!("the design has no unit named {name:?}; it has {}", names.join(", ")) }
    })
}

fn resolve_board<'f>(facts: &'f Facts, name: Option<&str>, call: &str) -> Result<(&'f str, &'f BoardMeasures), String> {
    if let Some(name) = name {
        let unit = unit(facts, name)?;
        let board = unit.board.as_ref().ok_or_else(|| format!("{name} has no board"))?;
        return Ok((&unit.unit, board));
    }
    let boards: Vec<_> = facts.units.iter().filter_map(|u| Some((u.unit.as_str(), u.board.as_ref()?))).collect();
    match boards.as_slice() {
        [only] => Ok(*only),
        [] if facts.units.len() == 1 => Err(format!("{} has no board", facts.units[0].unit)),
        [] => Err("the design has no board yet".into()),
        many => {
            let names: Vec<_> = many.iter().map(|(unit, _)| *unit).collect();
            Err(format!("the design has {} boards ({}): name one in {call}(\"unit\")", many.len(), names.join(", ")))
        }
    }
}

fn checks_stale(c: &CheckFacts) -> Option<String> {
    (!c.current).then(|| if c.pending.is_empty() { "a check still runs on the newest edit".to_owned() } else { format!("still running: {}", c.pending.join(", ")) })
}

fn measure(m: &Measure, facts: &Facts) -> Measured {
    let count = |n: u32| Got::Value(Value::Scalar(n as f64));
    match m {
        Measure::BoardSize(u) => match resolve_board(facts, u.as_deref(), "board") {
            Ok((u, b)) => match b.size {
                Some(size) => Measured::new(Got::Value(Value::Size(size)), "board outline", Focus::Unit(u.into())),
                None => Measured::unknown(format!("{u}'s board has no outline yet"), "board outline", Focus::Unit(u.into())),
            },
            Err(why) => Measured::unknown(why, "board outline", u.clone().map_or(Focus::Findings, Focus::Unit)),
        },
        Measure::BoardLayers(u) | Measure::BoardParts(u) | Measure::BoardUnplaced(u) => match resolve_board(facts, u.as_deref(), "board") {
            Ok((u, b)) => {
                let n = match m {
                    Measure::BoardLayers(_) => b.layers,
                    Measure::BoardParts(_) => b.parts,
                    _ => b.unplaced,
                };
                Measured::new(count(n), "board", Focus::Unit(u.into()))
            }
            Err(why) => Measured::unknown(why, "board", u.clone().map_or(Focus::Findings, Focus::Unit)),
        },
        Measure::BoardUnrouted(u) => {
            let (u, _) = match resolve_board(facts, u.as_deref(), "board") {
                Ok(board) => board,
                Err(why) => return Measured::unknown(why, "checks", u.clone().map_or(Focus::Findings, Focus::Unit)),
            };
            let Some(c) = &facts.checks else { return Measured::unknown("the checks have not run yet", "checks", Focus::Findings) };
            let ids: Vec<u64> = c.findings.iter().filter(|f| f.unconnected && f.unit.as_deref() == Some(u)).map(|f| f.id).collect();
            let mut out = Measured::new(count(ids.len() as u32), "checks", Focus::Findings);
            out.stale = checks_stale(c);
            out.findings = ids;
            out
        }
        Measure::Checks { family, unit: only, count: of } => {
            if facts.units.is_empty() {
                return Measured::unknown("nothing to check yet: the project has no circuit", "checks", Focus::Findings);
            }
            if let Some(u) = only {
                if let Err(why) = unit(facts, u) {
                    return Measured::unknown(why, "checks", Focus::Findings);
                }
            }
            let Some(c) = &facts.checks else { return Measured::unknown("the checks have not run yet", "checks", Focus::Findings) };
            let severity = match of {
                CountOf::Errors => Severity::Error,
                CountOf::Warnings => Severity::Warning,
            };
            let ids: Vec<u64> = c.findings.iter()
                .filter(|f| family.counts(f.family) && f.severity == severity && only.as_ref().is_none_or(|u| f.unit.as_ref() == Some(u)))
                .map(|f| f.id).collect();
            let mut out = Measured::new(count(ids.len() as u32), "checks", Focus::Findings);
            out.stale = checks_stale(c);
            out.findings = ids;
            out
        }
        Measure::CaseFits(u) => {
            let (u, _) = match resolve_board(facts, u.as_deref(), "case") {
                Ok(board) => board,
                Err(why) => return Measured::unknown(why, "case checks", u.clone().map_or(Focus::Findings, Focus::Case)),
            };
            if !facts.producers.cases {
                return Measured::unknown("the case checks are not read here", "case checks", Focus::Case(u.into()));
            }
            match facts.cases.iter().find(|c| c.unit == u) {
                None => Measured::unknown(format!("the case checks have not run on {u} yet"), "case checks", Focus::Case(u.into())),
                Some(c) if !c.has_case => Measured::unknown(format!("{u} has no case on this plate"), "case checks", Focus::Case(u.into())),
                Some(c) => {
                    let shown = match c.errors.len() {
                        0 => "no case errors".to_owned(),
                        1 => "1 case error".to_owned(),
                        n => format!("{n} case errors"),
                    };
                    let mut out = Measured::new(Got::Holds { ok: c.errors.is_empty(), shown }, "case checks", Focus::Case(u.into()));
                    out.stale = (!c.current).then(|| c.failed.as_ref().map_or_else(
                        || "the case checks have not run on the newest edit".to_owned(),
                        |why| format!("the case could not be rebuilt: {why}")));
                    out.findings = c.errors.clone();
                    out
                }
            }
        }
        Measure::Bench(b) | Measure::BenchExpect { bench: b, .. } => {
            let focus = Focus::Bench(b.clone());
            let Some(bench) = facts.benches.iter().find(|f| f.bench == *b) else {
                let names: Vec<&str> = facts.benches.iter().map(|f| f.bench.as_str()).collect();
                let why = if names.is_empty() { format!("the design has no bench named {b:?}: it has none") } else { format!("the design has no bench named {b:?}; it has {}", names.join(", ")) };
                return Measured::unknown(why, "Check bench", focus);
            };
            if !facts.producers.benches {
                return Measured::unknown("Check bench results are not read here", "Check bench", focus);
            }
            let Some(run) = &bench.run else { return Measured::unknown(format!("not checked yet: run Check bench on {b}"), "Check bench", focus) };
            let got = match m {
                Measure::BenchExpect { expect, .. } => match run.outcomes.iter().find(|o| o.name == *expect) {
                    None => Got::Unknown(format!("{b} has no expectation named {expect:?}")),
                    Some(o) if o.verdict == Verdict::NotJudged => Got::Unknown(format!("{expect:?} was not judged in the last Check bench")),
                    Some(o) => Got::Holds { ok: o.verdict == Verdict::Pass, shown: o.value.clone().unwrap_or_else(|| if o.verdict == Verdict::Pass { "passes".into() } else { "fails".into() }) },
                },
                _ => {
                    let pass = run.outcomes.iter().filter(|o| o.verdict == Verdict::Pass).count();
                    let fail = run.outcomes.iter().filter(|o| o.verdict == Verdict::Fail).count();
                    let total = run.outcomes.len();
                    if total == 0 {
                        Got::Unknown(format!("{b} has no expectations"))
                    } else if fail == 0 && pass < total {
                        Got::Unknown(format!("{} of {total} expectations were not judged in the last Check bench", total - pass))
                    } else {
                        Got::Holds { ok: fail == 0, shown: format!("{pass} of {total} pass") }
                    }
                }
            };
            let mut out = Measured::new(got, "Check bench", focus);
            out.stale = (!run.current).then(|| "the design or firmware changed since the last Check bench".to_owned());
            out.at = Some(run.at);
            out
        }
        Measure::PrintTime | Measure::PrintFilament => {
            if !facts.producers.print {
                return Measured::unknown("the slice estimate is not read here", "slice", Focus::Print);
            }
            let Some(p) = &facts.print else { return Measured::unknown("not sliced yet: slice the plate to measure", "slice", Focus::Print) };
            let v = if *m == Measure::PrintTime { p.seconds } else { p.grams };
            let mut out = Measured::new(Got::Value(Value::Scalar(v)), "slice", Focus::Print);
            out.stale = (!p.current).then(|| "the plate changed since the slice".to_owned());
            out.at = Some(p.at);
            out
        }
    }
}

/// The default `is` tolerance of a dimension, for target `t`.
fn default_tol(dim: Dim, t: f64) -> f64 {
    match dim {
        Dim::Size => 0.05e-3,
        Dim::Count | Dim::Pass => 0.0,
        Dim::Time | Dim::Mass => 0.01 * t.abs(),
    }
}

fn holds(v: Value, bound: &Bound, dim: Dim) -> bool {
    let pairs = |a: Value, b: Value| -> Vec<(f64, f64)> {
        match (a, b) {
            (Value::Scalar(a), Value::Scalar(b)) => vec![(a, b)],
            (Value::Size(a), Value::Size(b)) => vec![(a[0], b[0]), (a[1], b[1])],
            _ => vec![(f64::NAN, f64::NAN)],
        }
    };
    match bound {
        Bound::Holds => true,
        Bound::Range { min, max } => {
            min.is_none_or(|lo| pairs(v, lo).iter().all(|(v, lo)| v >= lo))
                && max.is_none_or(|hi| pairs(v, hi).iter().all(|(v, hi)| v <= hi))
        }
        Bound::Is { value, tol } => pairs(v, *value).iter().all(|(v, t)| (v - t).abs() <= tol.unwrap_or_else(|| default_tol(dim, *t))),
    }
}

/// Up to two decimals, without trailing zeros.
fn num(v: f64) -> String {
    let s = format!("{v:.2}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" { "0".into() } else { s.into() }
}

fn time(s: f64) -> String {
    if s < 60.0 {
        return format!("{} s", num(s));
    }
    let minutes = (s / 60.0).round() as u64;
    match (minutes / 60, minutes % 60) {
        (0, m) => format!("{m} min"),
        (h, 0) => format!("{h} h"),
        (h, m) => format!("{h} h {m} min"),
    }
}

/// A value in words, in the unit a person reads it in.
pub(crate) fn show(v: Value, dim: Dim) -> String {
    match (v, dim) {
        (Value::Size([w, h]), _) => format!("{} x {} mm", num(w * 1e3), num(h * 1e3)),
        (Value::Scalar(n), Dim::Count) => format!("{}", n.round() as i64),
        (Value::Scalar(s), Dim::Time) => time(s),
        (Value::Scalar(g), Dim::Mass) if g >= 1000.0 => format!("{} kg", num(g / 1000.0)),
        (Value::Scalar(g), Dim::Mass) => format!("{} g", num(g)),
        (Value::Scalar(x), _) => num(x),
    }
}

fn bound_words(bound: &Bound, dim: Dim) -> String {
    match bound {
        Bound::Holds => "holds".into(),
        Bound::Range { min: None, max: Some(hi) } => format!("max {}", show(*hi, dim)),
        Bound::Range { min: Some(lo), max: None } => format!("min {}", show(*lo, dim)),
        Bound::Range { min: Some(lo), max: Some(hi) } => format!("{} to {}", show(*lo, dim), show(*hi, dim)),
        Bound::Range { min: None, max: None } => "any".into(),
        Bound::Is { value, tol: None } => show(*value, dim),
        Bound::Is { value, tol: Some(t) } => {
            let tol = if dim == Dim::Size { format!("{} mm", num(t * 1e3)) } else { show(Value::Scalar(*t), dim) };
            format!("{} ±{tol}", show(*value, dim))
        }
    }
}

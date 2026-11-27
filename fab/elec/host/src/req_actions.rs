//! The project's requirements (`*.req.splash`, `fab_elec_req`) held against what the design and
//! its checks measure: `elec.req.list`, and the reading check of a requirements document's text.
//! The host gathers the design's facts; the app adds the case, bench and slice facts it reads.

use crate::edit_actions::check_family;
use fab_elec_circuit::{CheckKind, Violation};
use fab_elec_core::{Diag, DocId, Sel, Severity};
use fab_elec_design::Design;
use fab_elec_design::action::{ActionDef, FieldDef, FieldKind, Risk};
use fab_elec_lang::DocKind;
use fab_elec_req::{BenchFacts, BenchRun, BoardMeasures, CaseFacts, CheckFacts, Facts, Family, FindingFact, Outcome, PrintFacts, Producers, ReqDoc, Report, Status, UnitFacts, Verdict};
use fab_elec_view::instruments::bench_setup::BenchCheck;
use fab_elec_view::{DesignSnapshot, DocsView, ReqShown};
use makepad_widgets_core::makepad_script::syntax::write::quote;
use std::collections::HashSet;
use std::fmt::Write;
use std::sync::Arc;

pub const REQ_LIST: ActionDef = ActionDef {
    name: "elec.req.list",
    label: "List requirements",
    group: "Requirements",
    help: "The project's requirements, the person's targets in their words, each held against what Fab measures: pass, fail, unknown with why, or waived with the person's reason. Unknown never counts as met. Read it after an edit and before saying the work is done.",
    returns: "met: bool  true when the project has requirements and none fails or is unknown (a waived one counts as met)
documents: int  the requirements documents read; 0: the project has none
pass: int
fail: int
unknown: int  each says why, and what would measure it
waived: int
proposed: int  waivers proposed for the person to accept or decline; absent when none
total: int  requirements over every page
next: int  the from of the next page, when more remain
mistakes: [{..}]  what the requirements documents get wrong: a statement with an error is left out until it is fixed
  doc: text
  line: int
  severity: choice error|warning
  message: text
requirements: [{..}]  in document order
  name: text  the person's words
  status: choice pass|fail|unknown|waived
  value: text  what was measured, with its unit (52 x 31 mm); absent when nothing was
  bound: text  the bound in words (max 60 x 40 mm, holds)
  source: text  what measures it: board outline, board, checks, case checks, Check bench, slice
  why: text  why it is unknown, and what would measure it
  stale: bool  true when the value is from before the newest change; the status is then unknown
  reason: text  a waived requirement's reason, the person's
  proposed: text  the reason of a waiver proposed for the person to accept or decline; it waives nothing yet
  was: choice pass|fail|unknown  a waived requirement's status without its waiver
  doc: text  its document
  line: int  where it is written
units: [text]  circuit unit names, absent when none
wishes: [{..}]  the person's wishes, listed and never judged; absent when none
  words: text
  doc: text
  line: int
brief: {..}  the person's brief, verbatim; absent when none
  words: text
  doc: text
  line: int",
    fields: &[FieldDef {
        name: "from", kind: FieldKind::Int { min: 0, max: i64::MAX }, required: false, default: "0",
        help: "The first requirement of the page: a reply's next.",
    }],
    inputs: &[], risk: Risk::Read, keys: "",
};

/// What a requirement edit answers: its document's text step, as `fab.elec_text` answers one.
const EDIT_RETURNS: &str = "seq: int  the edit's step
label: text
doc: text  the requirements document changed
rev: int  its revision after the change
lines: [[int]]  the lines changed, each [first last] from 1
requirements_changed: [text]  what the edit did to the person's requirements: say each line to the person
requirements: {..}  once judged on the edit, as every elec edit answers it: met, changed, waiting";

const NAME: FieldDef = FieldDef {
    name: "name", kind: FieldKind::Text, required: true, default: "",
    help: "The requirement's name, the person's words, as elec.req.list gives it.",
};

pub const REQ_ADD: ActionDef = ActionDef {
    name: "elec.req.add",
    label: "Add requirement",
    group: "Requirements",
    help: "Add a requirement to the project's requirements document, in the person's words: what Fab measures and its bound, as Splash text. Write only numbers the person gave. It goes in after the last requirement, one undo step; refused, located, when it does not read as one requirement.",
    returns: EDIT_RETURNS,
    fields: &[
        NAME,
        FieldDef {
            name: "measure", kind: FieldKind::Text, required: true, default: "",
            help: "What Fab measures, as Splash: board.size | .layers | .parts | .unplaced | .unrouted (board(\"u\").* to name a board); checks(@erc|@drc|@system|@route|@case|@custom|@all, {unit: \"u\"}).errors | .warnings (the unit optional); case.fits (case(\"u\").fits to name a board); bench(\"b\"); bench(\"b\").expect(\"e\"); print.time; print.filament.",
        },
        FieldDef {
            name: "bound", kind: FieldKind::Text, required: false, default: "",
            help: "The bound, as Splash: {max: 2}, {is: 2}, {max: [60mm 40mm]}, {max: 3h}, {max: 40g}. None for case fits and benches, which hold or not. Put electrical targets without a bench in wish(..), or use bench(\"b\").expect(\"e\") once the bench exists.",
        },
    ],
    inputs: &[], risk: Risk::Edit, keys: "",
};

pub const REQ_WISH: ActionDef = ActionDef {
    name: "elec.req.wish", label: "Add wish", group: "Requirements",
    help: "Write one of the person's wishes in their words: what they want that Fab does not measure. It is listed and never judged. Never turn a wish into a requirement with a bound the person did not give.",
    returns: EDIT_RETURNS,
    fields: &[FieldDef { name: "words", kind: FieldKind::Text, required: true, default: "", help: "The wish in the person's words." }],
    inputs: &[], risk: Risk::Edit, keys: "",
};

pub const REQ_BRIEF: ActionDef = ActionDef {
    name: "elec.req.brief", label: "Save brief", group: "Requirements",
    help: "The person's brief, in their words, written from the Requirements tab. Only the person writes it: from a tool it is refused.",
    returns: EDIT_RETURNS,
    fields: &[FieldDef { name: "words", kind: FieldKind::Text, required: true, default: "", help: "The person's brief verbatim. Empty words remove it." }],
    inputs: &[], risk: Risk::Edit, keys: "",
};

pub const REQ_REMOVE: ActionDef = ActionDef {
    name: "elec.req.remove",
    label: "Remove requirement",
    group: "Requirements",
    help: "Remove a requirement and its waiver from its document, when the person asks. One undo step.",
    returns: EDIT_RETURNS,
    fields: &[NAME],
    inputs: &[], risk: Risk::Edit, keys: "",
};

pub const REQ_WAIVE: ActionDef = ActionDef {
    name: "elec.req.waive",
    label: "Waive requirement",
    group: "Requirements",
    help: "Waive a requirement for the person's reason, only when they say so, in their words. From the AI it is a proposal: the row shows it, the person accepts or declines it in the Requirements editor, and until they accept it waives nothing; say so in your reply. Proposing again replaces the reason. One undo step.",
    returns: EDIT_RETURNS,
    fields: &[NAME, FieldDef {
        name: "reason", kind: FieldKind::Text, required: true, default: "",
        help: "Why the person accepts it unmet, in their words.",
    }],
    inputs: &[], risk: Risk::Edit, keys: "",
};

pub const REQ_UNWAIVE: ActionDef = ActionDef {
    name: "elec.req.unwaive",
    label: "Unwaive requirement",
    group: "Requirements",
    help: "Take a requirement's waiver out: it is judged again like any other. One undo step.",
    returns: EDIT_RETURNS,
    fields: &[NAME],
    inputs: &[], risk: Risk::Edit, keys: "",
};

pub static ACTIONS: &[ActionDef] = &[REQ_LIST, REQ_ADD, REQ_WISH, REQ_BRIEF, REQ_REMOVE, REQ_WAIVE, REQ_UNWAIVE];

/// Whether `action` is one of the requirement edits [`req_edit`] writes.
pub fn is_req_edit(action: &str) -> bool {
    [REQ_ADD.name, REQ_WISH.name, REQ_BRIEF.name, REQ_REMOVE.name, REQ_WAIVE.name, REQ_UNWAIVE.name].contains(&action)
}

/// One undoable requirement edit: replace an existing document's text, or add a document.
#[derive(Debug, PartialEq)]
pub enum ReqEdit {
    Text { doc: String, rev: u64, text: String },
    New { doc: String, text: String },
}

/// Write the named requirement's document, or add to the first requirements document. The
/// first circuit names a project's first requirements document, or `device` on a plate without a circuit.
/// Only a person grants a waiver.
pub fn req_edit(docs: &DocsView, action: &str, args: &[(String, String)], person: bool) -> Result<ReqEdit, String> {
    use fab_elec_req::write;
    let arg = |key: &str| args.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str());
    let brief = action == REQ_BRIEF.name;
    if brief && !person { return Err("the brief is the person's own words: they write it in the Requirements tab".into()); }
    let name = arg("name").unwrap_or("").trim();
    let appends = action == REQ_ADD.name || action == REQ_WISH.name;
    let write = |text: &str, doc: &ReqDoc| match action {
        "elec.req.brief" => write::brief(text, doc, arg("words").ok_or("words: required field is missing")?),
        "elec.req.wish" => write::wish(text, doc, arg("words").unwrap_or("")),
        "elec.req.add" => write::add(text, doc, name, arg("measure").unwrap_or(""), arg("bound")),
        "elec.req.remove" => write::remove(text, doc, name),
        "elec.req.waive" => write::waive(text, doc, name, arg("reason").unwrap_or(""), !person),
        "elec.req.unwaive" => write::unwaive(text, doc, name),
        _ => Err(format!("{action} is not a requirements edit")),
    };
    let texts: Vec<_> = docs.texts.iter().filter(|t| DocKind::of_name(&t.name) == Some(DocKind::Requirements)).collect();
    let Some(&first) = texts.first() else {
        if !appends && !brief { return Err("the project has no requirements document yet".into()); }
        if brief && arg("words").is_some_and(|s| s.trim().is_empty()) { return Err("there is no brief to save".into()); }
        let stem = docs.texts.iter().find_map(|t| t.name.strip_suffix(DocKind::Circuit.suffix()))
            .unwrap_or("device");
        let doc = format!("{stem}{}", DocKind::Requirements.suffix());
        let empty = ReqDoc::default();
        let text = write("", &empty)?;
        check_text(empty.doc, &doc, "", &text, !person)?;
        return Ok(ReqEdit::New { doc, text });
    };
    let read = |t: &fab_elec_view::DocText| fab_elec_req::read(t.doc, &t.text).0;
    let target = if appends { first } else {
        texts.iter().copied().find(|t| {
            let doc = read(t);
            if brief { doc.brief.is_some() } else { doc.requirements.iter().any(|r| r.name == name) }
        }).unwrap_or(first)
    };
    let text = write(&target.text, &read(target))?;
    Ok(ReqEdit::Text { doc: target.name.clone(), rev: target.rev, text })
}

/// Requirements listed per page of `elec.req.list`.
const REQ_PAGE: usize = 40;

/// Every requirements document of `docs`, read, with the mistakes found in them. O(text).
pub fn requirements(docs: &DocsView) -> (Vec<ReqDoc>, Vec<Diag>) {
    let mut read = Vec::new();
    let mut mistakes = Vec::new();
    for text in docs.texts.iter().filter(|text| DocKind::of_name(&text.name) == Some(DocKind::Requirements)) {
        let (doc, errors) = fab_elec_req::read(text.doc, &text.text);
        read.push(doc);
        mistakes.extend(errors);
    }
    (read, mistakes)
}

/// What the design and its checks measure, from the shown state (the preview's when one is live),
/// as `elec.check.list` reads it. The case, bench and slice producers are the app's: this marks
/// none of them read, and lists the benches by name only ([`app_facts`] adds them).
/// One pass over benches in addition to the board measures and findings below.
pub fn design_facts(snap: &DesignSnapshot) -> Facts {
    // A finding that waits for a pour's refill is not shown yet: the checks are not current
    // until it is, so no requirement passes on a list that may still grow.
    let mut pending: Vec<String> = crate::edit_actions::pending_checks(snap.stale).into_iter().map(str::to_owned).collect();
    if !snap.awaiting.is_empty() {
        pending.push("pour refills".into());
    }
    let mut facts = measured_facts(snap.live(), &snap.violations.value, &snap.awaiting, pending);
    facts.benches = snap.benches.iter().map(|bench| BenchFacts { bench: bench.out.name.clone(), run: None }).collect();
    facts
}

/// Board measures and current, scoped findings for exactly this design. Walk each board's
/// outline, placements and components once. Each finding looks up its unit and refill status.
pub(crate) fn measured_facts(design: &Design, violations: &[Violation], awaiting: &[u64], pending: Vec<String>) -> Facts {
    let units = design.units.iter().map(|unit| UnitFacts {
        unit: unit.name.clone(),
        board: unit.board.as_ref().map(|board| {
            let outline = board.outline_bbox();
            let size = (!outline.is_empty()).then(|| [
                (f64::from(outline.max.x) - f64::from(outline.min.x)) * 1e-9,
                (f64::from(outline.max.y) - f64::from(outline.min.y)) * 1e-9,
            ]);
            let placed: HashSet<_> = board.placed.iter().map(|p| p.comp).collect();
            let unplaced = unit.circuit.comps.iter()
                .filter(|c| c.part.kind != fab_elec_parts::PartKind::PowerFlag && !placed.contains(&c.id)).count();
            BoardMeasures {
                size,
                layers: u32::from(board.copper_count()),
                parts: board.placed.len() as u32,
                unplaced: unplaced as u32,
            }
        }),
    }).collect();
    let unit_of = |v: &Violation| v.unit.or_else(|| v.items.iter().find_map(Sel::unit)).and_then(|id| design.unit(id)).map(|u| u.name.clone());
    let findings = violations.iter()
        .filter(|v| awaiting.binary_search(&v.id).is_err())
        .map(|v| FindingFact {
            id: v.id,
            unit: unit_of(v),
            // The case checks' findings are Mech findings of rule `case.*` (`fab_elec_mech::checks`).
            family: match check_family(&v.kind) {
                _ if v.rule.starts_with("case.") => Family::Case,
                "erc" => Family::Erc,
                "system" => Family::System,
                "route" => Family::Route,
                "custom" => Family::Custom,
                _ => Family::Drc,
            },
            severity: v.severity,
            unconnected: matches!(v.kind, CheckKind::Unconnected),
        })
        .collect();
    Facts {
        units,
        checks: Some(CheckFacts { current: pending.is_empty(), pending, findings }),
        ..Facts::default()
    }
}

/// What only the app knows, as its producers last reported it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AppFacts {
    /// The newest case check run the app asked for (one per plate build); None before the first.
    pub case_run: Option<u64>,
    /// The evaluator's reason when the newest case evaluation failed; None once one succeeds.
    pub case_failed: Option<String>,
    /// Per bench by name, its last counted check ([`bench_run`]), `current` as of now.
    pub benches: Vec<(String, BenchRun)>,
    /// The whole plate's slice figures; `current` false once the plate changed after them.
    pub print: Option<PrintFacts>,
}

/// [`design_facts`] with the app's producers, every one read. A board's case results come from
/// the snapshot (`DesignSnapshot::cases`, with its case findings): current when they are of the
/// newest case run and the board's committed generation, so a result is never read against
/// findings the snapshot does not hold yet. Their errors are the board's case findings of error
/// severity. O(design_facts + case boards x findings + benches).
pub fn app_facts(snap: &DesignSnapshot, app: &AppFacts) -> Facts {
    let mut facts = design_facts(snap);
    let findings = facts.checks.as_ref().map_or(&[][..], |checks| checks.findings.as_slice());
    let cases = snap.cases.iter().filter_map(|held| {
        let unit = snap.design.unit(held.unit)?;
        let board = unit.board.as_ref()?;
        let errors = findings.iter()
            .filter(|f| f.family == Family::Case && f.severity == Severity::Error && f.unit.as_deref() == Some(unit.name.as_str()))
            .map(|f| f.id).collect();
        let current = app.case_run == Some(held.run) && held.generation == board.generation;
        Some(CaseFacts { unit: unit.name.clone(), has_case: held.cased, current, errors, failed: app.case_failed.clone().filter(|_| !current) })
    }).collect();
    facts.cases = cases;
    for bench in &mut facts.benches {
        bench.run = app.benches.iter().find(|(name, _)| *name == bench.bench).map(|(_, run)| run.clone());
    }
    facts.print = app.print.clone();
    facts.producers = Producers { cases: true, benches: true, print: true };
    facts
}

/// A bench's counted check (Check bench, `elec.bench.run`) as requirement facts: each
/// expectation's final outcome and its value as the bench writes it. An expectation the run did
/// not reach is not judged, never a pass. None while the check runs. O(expectations).
pub fn bench_run(check: &BenchCheck, current: bool, at: f64) -> Option<BenchRun> {
    use fab_elec_bench::expect::Outcome as Judged;
    use fab_elec_circuit::{QtyUnit, Quantity};
    let report = check.report.as_ref()?;
    let text = |si: f64, unit: &str| {
        let unit = match unit { "V" => QtyUnit::Volt, "A" => QtyUnit::Ampere, "W" => QtyUnit::Watt, "Hz" => QtyUnit::Hertz, "s" => QtyUnit::Second, _ => QtyUnit::Count };
        if unit == QtyUnit::Count { format!("{si}") } else { fab_elec_lang::units::literal_text(&Quantity { si, unit }) }
    };
    let outcomes = check.expects.iter().enumerate().map(|(i, expect)| {
        let state = report.states.get(i);
        Outcome {
            name: expect.name.clone(),
            verdict: match state.map(|state| state.outcome()) {
                Some(Judged::Pass) => Verdict::Pass,
                Some(Judged::Fail) => Verdict::Fail,
                Some(Judged::NotJudged) | None => Verdict::NotJudged,
            },
            value: state.filter(|state| state.value.is_finite()).map(|state| text(state.value, expect.unit())),
        }
    }).collect();
    Some(BenchRun { current, at, outcomes })
}

/// The project's requirements judged against `facts`, with the documents' own mistakes first
/// among the notes. O(text + requirements x facts).
pub fn report(docs: &DocsView, facts: &Facts) -> (Report, usize) {
    let (read, mut mistakes) = requirements(docs);
    let mut report = fab_elec_req::judge(&read, facts);
    mistakes.append(&mut report.notes);
    report.notes = mistakes;
    (report, read.len())
}

/// The requirements as the editor and the chip show them; None when the project has no
/// requirements document. O(text + requirements x facts).
pub fn shown(docs: &DocsView, facts: &Facts) -> Option<ReqShown> {
    let (report, documents) = report(docs, facts);
    if documents == 0 {
        return None;
    }
    let position = |doc, start| {
        let (name, line) = source_line(docs, doc, start);
        (Arc::from(name), line)
    };
    let at = report.rows.iter().map(|r| position(r.doc, r.span.start)).collect();
    let wish_at = report.wishes.iter().map(|w| position(w.doc, w.span.start)).collect();
    let brief_at = report.brief.as_ref().map(|b| position(b.doc, b.span.start));
    let mistakes = report.notes.iter().map(|note| {
        let (doc, line) = located(docs, &note.sel);
        if doc.is_empty() { note.message.clone() } else { format!("{doc}:{line}: {}", note.message) }
    }).collect();
    Some(ReqShown { report, at, wish_at, brief_at, mistakes })
}

/// `elec.req.list` over `facts`: totals, the documents' mistakes and one page of rows.
/// O(text + requirements x facts).
pub fn req_list(docs: &DocsView, facts: &Facts, args: &[(String, String)]) -> Result<String, Diag> {
    REQ_LIST.check_args(args)?;
    let from = args.iter().find(|(key, _)| key == "from").map_or(Ok(0), |(_, text)| text.trim().replace('_', "").parse::<usize>())
        .map_err(|_| Diag::error("from: expected a requirement index", vec![]))?;
    let (report, documents) = report(docs, facts);
    let total = report.rows.len();
    let mut out = String::new();
    write!(out, "{{met: {} documents: {documents} pass: {} fail: {} unknown: {} waived: {} total: {total}",
        report.met(), report.pass, report.fail, report.unknown, report.waived).unwrap();
    if report.proposed > 0 {
        write!(out, " proposed: {}", report.proposed).unwrap();
    }
    let end = total.min(from.saturating_add(REQ_PAGE));
    if end < total {
        write!(out, " next: {end}").unwrap();
    }
    if !report.notes.is_empty() {
        out.push_str(" mistakes: [");
        for note in &report.notes {
            let (doc, line) = located(docs, &note.sel);
            let severity = if note.severity == Severity::Error { "error" } else { "warning" };
            write!(out, "{{doc: {} line: {line} severity: {severity} message: {}}} ", quote(doc), quote(&note.message)).unwrap();
        }
        out.push(']');
    }
    let words = |w: &fab_elec_req::WishRow| {
        let (doc, line) = source_line(docs, w.doc, w.span.start);
        format!("{{words: {} doc: {} line: {line}}}", quote(&w.words), quote(doc))
    };
    if !report.wishes.is_empty() {
        write!(out, " wishes: [{}]", report.wishes.iter().map(words).collect::<Vec<_>>().join(" ")).unwrap();
    }
    if let Some(b) = &report.brief { write!(out, " brief: {}", words(b)).unwrap(); }
    out.push_str(" requirements: [");
    for row in report.rows.get(from..end).unwrap_or_default() {
        write!(out, "{{name: {} status: {}", quote(&row.name), row.status.word()).unwrap();
        if let Some(value) = &row.value {
            write!(out, " value: {}", quote(value)).unwrap();
        }
        write!(out, " bound: {} source: {}", quote(&row.bound), quote(row.source)).unwrap();
        let (status, reason) = match &row.status {
            Status::Waived { reason, was } => (&**was, Some(reason)),
            status => (status, None),
        };
        if let Status::Unknown(why) = status {
            write!(out, " why: {}", quote(why)).unwrap();
        }
        if row.stale {
            out.push_str(" stale: true");
        }
        if let Some(reason) = reason {
            write!(out, " reason: {} was: {}", quote(reason), status.word()).unwrap();
        }
        if let Some(proposed) = &row.proposed {
            write!(out, " proposed: {}", quote(proposed)).unwrap();
        }
        let (doc, line) = located(docs, &[Sel::Source { doc: row.doc, start: row.span.start, end: row.span.end }]);
        write!(out, " doc: {} line: {line}}} ", quote(doc)).unwrap();
    }
    out.push(']');
    if !facts.units.is_empty() {
        let names: Vec<_> = facts.units.iter().map(|u| quote(&u.unit)).collect();
        write!(out, " units: [{}]", names.join(" ")).unwrap();
    }
    out.push('}');
    Ok(out)
}

/// A requirement as an edit's answer compares it before and after: its status word, and whether
/// its value waits for its producer to run again (stale).
#[derive(Clone, Debug, PartialEq)]
pub struct ReqMark {
    pub name: String,
    pub word: &'static str,
    pub stale: bool,
}

/// Each row of `report` as [`req_delta`] compares it, in report order. O(rows).
pub fn marks(report: &Report) -> Vec<ReqMark> {
    report.rows.iter().map(|row| ReqMark { name: row.name.clone(), word: row.status.word(), stale: row.stale }).collect()
}

/// The `requirements` field of an AI edit's answer, in `elec.req.list`'s words: the rows shown
/// when the edit was sent (`before`) against the report judged on its commit (None: the project
/// has no requirements now). `met` as `elec.req.list` gives it; `changed`, each row whose status
/// the edit moved (`was: new` for a row it wrote, `now: removed` for one it took out), with its
/// value, bound and an unknown's why; `waiting`, each row not waived whose value is from before
/// the edit until its producer runs again (the checks, the case checks, Check bench, the slice),
/// with why. A row that only went stale waits; it is not counted as changed. None when there are
/// no requirements before or after. O(rows log rows).
pub fn req_delta(before: &[ReqMark], after: Option<&Report>) -> Option<String> {
    let rows = after.map_or(&[][..], |report| report.rows.as_slice());
    if before.is_empty() && rows.is_empty() {
        return None;
    }
    let was: std::collections::HashMap<&str, &ReqMark> = before.iter().map(|mark| (mark.name.as_str(), mark)).collect();
    let (mut changed, mut waiting) = (Vec::new(), Vec::new());
    for row in rows {
        let why = match &row.status {
            Status::Unknown(why) => Some(why.as_str()),
            _ => None,
        };
        if row.stale && !matches!(row.status, Status::Waived { .. }) {
            let why = why.map_or("", |why| why.strip_prefix("stale: ").unwrap_or(why));
            waiting.push(format!("{{name: {} why: {}}}", quote(&row.name), quote(why)));
            continue;
        }
        let from = was.get(row.name.as_str()).map_or("new", |mark| mark.word);
        if from == row.status.word() {
            continue;
        }
        let mut line = format!("{{name: {} was: {from} now: {}", quote(&row.name), row.status.word());
        if let Some(value) = &row.value {
            write!(line, " value: {}", quote(value)).unwrap();
        }
        write!(line, " bound: {}", quote(&row.bound)).unwrap();
        if let Some(why) = why {
            write!(line, " why: {}", quote(why)).unwrap();
        }
        line.push('}');
        changed.push(line);
    }
    let now: std::collections::HashSet<&str> = rows.iter().map(|row| row.name.as_str()).collect();
    for mark in before.iter().filter(|mark| !now.contains(mark.name.as_str())) {
        changed.push(format!("{{name: {} was: {} now: removed}}", quote(&mark.name), mark.word));
    }
    let mut out = format!("{{met: {}", !rows.is_empty() && after.is_some_and(Report::met));
    if !changed.is_empty() {
        write!(out, " changed: [{}]", changed.join(" ")).unwrap();
    }
    if !waiting.is_empty() {
        write!(out, " waiting: [{}]", waiting.join(" ")).unwrap();
    }
    out.push('}');
    Some(out)
}

/// What a requirements document's new text does to the person's requirements, one plain line
/// each, for the AI to say in its reply as it says `expects_changed`: a requirement added, taken
/// out, or measured or bounded otherwise; a waiver proposed or taken out. Statements that do not
/// read are not compared (the text's check refuses the ones a change adds). O(text).
pub fn requirements_changed(before: &str, after: &str) -> Vec<String> {
    let (was, _) = fab_elec_req::read(DocId(0), before);
    let (now, _) = fab_elec_req::read(DocId(0), after);
    let stated = |r: &fab_elec_req::Requirement| if r.bound_text.is_empty() { r.measure_text.clone() } else { format!("{} {}", r.measure_text, r.bound_text) };
    let mut lines = Vec::new();
    let mut wishes = |new: &[fab_elec_req::Wish], old: &[fab_elec_req::Wish], action: &str| {
        let mut remaining = std::collections::HashMap::new();
        for w in old { *remaining.entry(w.words.as_str()).or_insert(0usize) += 1; }
        for w in new {
            match remaining.get_mut(w.words.as_str()) {
                Some(n) if *n > 0 => *n -= 1,
                _ => lines.push(format!("{action} `{}`", w.words)),
            }
        }
    };
    wishes(&now.wishes, &was.wishes, "added the wish");
    wishes(&was.wishes, &now.wishes, "took out the person's wish");
    for r in &now.requirements {
        match was.requirements.iter().find(|w| w.name == r.name) {
            None => lines.push(format!("added the requirement `{}`: {}", r.name, stated(r))),
            Some(w) if w.measure_text != r.measure_text => lines.push(format!("changed the person's requirement `{}`: {} → {}", r.name, stated(w), stated(r))),
            Some(w) if w.bound_text != r.bound_text => lines.push(format!("changed the person's requirement `{}`: {} → {}", r.name, w.bound_text, r.bound_text)),
            Some(_) => {}
        }
    }
    for w in was.requirements.iter().filter(|w| !now.requirements.iter().any(|r| r.name == w.name)) {
        lines.push(format!("took out the person's requirement `{}`: {}", w.name, stated(w)));
    }
    let same = |a: &fab_elec_req::Waiver, b: &fab_elec_req::Waiver| a.name == b.name && a.reason == b.reason && a.proposed == b.proposed;
    for v in now.waivers.iter().filter(|v| v.proposed && !was.waivers.iter().any(|w| same(w, v))) {
        lines.push(format!("proposed waiving `{}` because {}: the person accepts or declines it", v.name, v.reason));
    }
    for w in was.waivers.iter().filter(|w| !w.proposed && !now.waivers.iter().any(|v| same(v, w))) {
        lines.push(format!("took out the person's waiver of `{}` ({})", w.name, w.reason));
    }
    lines
}

/// A requirements document's new text, for `fab.elec_text`: refused when it adds a mistake the
/// text it replaces did not have (each located, one per line), so a person's half-typed line
/// does not stop an edit elsewhere. O(text).
pub fn check_text(doc: DocId, name: &str, before: &str, after: &str, ai: bool) -> Result<(), String> {
    let (old_doc, was) = fab_elec_req::read(doc, before);
    let (new_doc, now) = fab_elec_req::read(doc, after);
    // Only the person waives: an AI's text may propose a waiver, take one out, or leave them be.
    if ai {
        let old = old_doc.brief.as_ref().map(|b| &before[b.span.start as usize..b.span.end as usize]);
        let new = new_doc.brief.as_ref().map(|b| &after[b.span.start as usize..b.span.end as usize]);
        if old != new {
            let line = new_doc.brief.as_ref().map(|b| line_of(after, b.span.start as usize))
                .or_else(|| old_doc.brief.as_ref().map(|b| line_of(before, b.span.start as usize))).unwrap();
            return Err(format!("{name}:{line}: the brief is the person's own words: leave brief(..) as it is"));
        }
        let granted = |d: &ReqDoc| d.waivers.iter().filter(|w| !w.proposed).map(|w| (w.name.clone(), w.reason.clone())).collect::<Vec<_>>();
        let before = granted(&old_doc);
        if let Some(w) = new_doc.waivers.iter().find(|w| !w.proposed && !before.contains(&(w.name.clone(), w.reason.clone()))) {
            return Err(format!("{name}:{}: only the person waives: write propose_waiver({}, {}) for them to accept, or ask them",
                line_of(after, w.span.start as usize), quote(&w.name), quote(&w.reason)));
        }
    }
    let mut old: Vec<&str> = was.iter().map(|d| d.message.as_str()).collect();
    let added_diags: Vec<&Diag> = now.iter().filter(|d| {
        match old.iter().position(|m| *m == d.message) {
            Some(at) => { old.swap_remove(at); false }
            None => true,
        }
    }).collect();
    let mut added: Vec<String> = added_diags.iter().map(|d| {
        let line = d.sel.iter().find_map(|sel| match sel {
            Sel::Source { start, .. } => Some(line_of(after, *start as usize)),
            _ => None,
        });
        match line {
            Some(line) => format!("{name}:{line}: {}", d.message),
            None => format!("{name}: {}", d.message),
        }
    }).collect();
    if added_diags.iter().any(|d| fab_elec_req::is_unknown_measure(&d.message)) {
        added.push(format!("measures: {}", fab_elec_req::MEASURES));
    }
    if added.is_empty() { Ok(()) } else { Err(added.join("\n")) }
}

/// The document and 1-based line a selection names, by its first source site.
pub fn located<'a>(docs: &'a DocsView, sel: &[Sel]) -> (&'a str, usize) {
    sel.iter().find_map(|sel| match sel {
        Sel::Source { doc, start, .. } => docs.texts.iter().find(|text| text.doc == *doc).map(|text| (text.name.as_str(), line_of(&text.text, *start as usize))),
        _ => None,
    }).unwrap_or(("", 0))
}

fn source_line(docs: &DocsView, doc: DocId, start: u32) -> (&str, usize) {
    located(docs, &[Sel::Source { doc, start, end: start }])
}

fn line_of(text: &str, at: usize) -> usize {
    text.as_bytes()[..at.min(text.len())].iter().filter(|&&b| b == b'\n').count() + 1
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Cmd, Engine, Mailbox};
    use fab_elec_board::{BoardOp, Factories};
    use fab_elec_core::{BoardId, Contour, P};
    use fab_elec_design::{By, DesignOp};
    use fab_elec_view::DocText;
    use std::sync::atomic::AtomicU64;
    use std::sync::Arc;

    const NAME: &str = "ledctl.req.splash";

    fn docs(text: &str) -> DocsView {
        DocsView { texts: vec![DocText { doc: DocId(9), name: NAME.into(), text: text.into(), rev: 0 }], ..DocsView::default() }
    }

    fn status<'a>(report: &'a Report, name: &str) -> &'a Status {
        &report.row(name).unwrap_or_else(|| panic!("{name} is listed")).status
    }

    #[test]
    fn check_text_lists_measures_once_per_refusal_and_keeps_lines_located() {
        use fab_elec_req::{MEASURES, REQUIRE_EXAMPLE};
        let name = "device.req.splash";
        let unknown = ["voltage", "current", "power"].map(|m| format!(r#"require("{m}", {m}, {{max: 1}})"#)).join("\n");
        let refusal = check_text(DocId(9), name, "", &unknown, true).unwrap_err();
        assert_eq!(refusal.matches(MEASURES).count(), 1, "unknown measures must share one catalog");
        for (i, measure) in ["voltage", "current", "power"].iter().enumerate() {
            assert_eq!(refusal.lines().nth(i), Some(format!(r#"{name}:{}: "{measure}": {measure} is not a measure"#, i + 1).as_str()), "unknown measure {measure} must retain its line");
        }

        let named = r#"require("Board at most 60 x 40 mm", measure: board("u").size, max: [60mm 40mm])
require("2-layer board", measure: board("u").layers, max: 2)
require("ERC clean", measure: checks(@erc).errors, max: 0)"#;
        let refusal = check_text(DocId(9), name, "", named, true).unwrap_err();
        assert_eq!(refusal.matches(MEASURES).count(), 0, "known measures with named arguments need no catalog");
        let expected: Vec<_> = ["Board at most 60 x 40 mm", "2-layer board", "ERC clean"].iter().enumerate().map(|(i, title)|
            format!(r#"{name}:{}: "{title}": require(..) takes positional arguments, not measure:, max:; drop the labels and put the bound in {{..}}, e.g. {REQUIRE_EXAMPLE}"#, i + 1)).collect();
        assert_eq!(refusal, expected.join("\n"), "each bad line must say where and how to repair its positional syntax");

        let mut facts = Facts::default();
        let docs = docs("");
        assert!(!req_list(&docs, &facts, &[]).unwrap().contains(" units:"), "a brief-only design must not advertise unit names");
        facts.units.push(UnitFacts { unit: "ledctl".into(), board: None });
        assert!(req_list(&docs, &facts, &[]).unwrap().ends_with(r#"requirements: [] units: ["ledctl"]}"#), "the requirements list must include circuit names after its rows");
    }

    /// Why: `elec.req.list` holds each requirement against what the design and its checks
    /// measure, an edit moves the rows it touches, a check still running never passes, and a
    /// document's mistakes are located and refused only when an edit adds them.
    #[test]
    fn lists_requirements_against_the_design_and_its_checks() {
        let design = fab_elec_fixtures::ledctl();
        let unit = BoardId(0);
        let outline = design.unit(unit).and_then(|u| u.board.as_ref()).expect("ledctl has a board").outline_bbox();
        let mut engine = Engine::new(design, Arc::new(Factories::new()), Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)), "fixture".into());
        let snap = engine.snapshot();
        let docs = docs(fab_elec_fixtures::LEDCTL_REQ);
        let unconnected = snap.violations.value.iter().filter(|v| matches!(v.kind, CheckKind::Unconnected)).count();
        assert!(unconnected > 0 && snap.stale == Default::default(), "fixture setup: ledctl's checks have run and leave nets to route");

        let (report, documents) = report(&docs, &design_facts(&snap));
        assert_eq!(documents, 1);
        let row = |name: &str| report.row(name).unwrap();
        assert_eq!((status(&report, "fits a 60 x 40 case"), row("fits a 60 x 40 case").value.as_deref()), (&Status::Pass, Some("50 x 30 mm")), "the outline's box is the size");
        for name in ["two layers", "every part placed", "ERC clean"] {
            assert_eq!(status(&report, name), &Status::Pass, "{name}: the board and the ERC findings measure it");
        }
        assert_eq!((status(&report, "fully routed"), row("fully routed").value.clone()), (&Status::Fail, Some(unconnected.to_string())), "unrouted counts the board's Unconnected findings");
        assert_eq!(status(&report, "DRC clean"), &Status::Fail, "the unrouted nets are DRC errors");
        assert_eq!(status(&report, "sits in its case"), &Status::Unknown("the case checks are not read here".into()), "the host alone does not read the case checks");
        assert!(matches!(status(&report, "prints in 3 hours"), Status::Waived { was, .. } if matches!(**was, Status::Unknown(_))), "a waiver keeps what the row would be");

        // The app's producers: the case run the snapshot holds, the bench's last counted check
        // and the slice figures.
        let generation = snap.design.unit(unit).and_then(|u| u.board.as_ref()).unwrap().generation;
        let mut cased = DesignSnapshot::clone(&snap);
        cased.cases = Arc::new(vec![fab_elec_view::CaseHeld { unit, generation, run: 3, cased: true }]);
        // (the engine made from a design alone has no documents, so no bench views: take ledctl's)
        let bringup = fab_elec_fixtures::ledctl_eval().benches.into_iter().find(|b| b.name == "bringup").expect("ledctl's bench");
        let expects = bringup.expects.clone();
        cased.benches = Arc::new(vec![fab_elec_lang::BenchView { out: bringup, edits: vec![], insert: fab_elec_lang::BenchEdit::Literal }]);
        let judged = |verdict, value: f64| fab_elec_bench::expect::CheckState { verdict, done: true, value, at: 1e-3, failed: None, worst: 0.0, sel: vec![] };
        let states = expects.iter().map(|e| if e.name == "3V3 up" { judged(fab_elec_bench::expect::Verdict::Pass, 3.3) } else { judged(fab_elec_bench::expect::Verdict::Pending, f64::NAN) }).collect();
        let counted = fab_elec_bench::BenchReport { states, diags: vec![], points: 10, t: 0.02, error: None, rescues: vec![], stopped: false };
        let check = BenchCheck { bench: "bringup".into(), expects: expects.clone(), tstop: 0.02, report: None, earlier: false, reached: Default::default() };
        assert_eq!(bench_run(&check, true, 5.0), None, "a check still running gives no run");
        let run = bench_run(&BenchCheck { report: Some(counted), ..check }, true, 5.0).expect("a finished check");
        assert_eq!(run.outcomes.iter().map(|o| (o.name.as_str(), o.verdict, o.value.as_deref())).find(|o| o.0 == "3V3 up"), Some(("3V3 up", Verdict::Pass, Some("3.3V"))), "a judged expectation, its value as the bench writes it");
        assert!(run.outcomes.iter().filter(|o| o.name != "3V3 up").all(|o| o.verdict == Verdict::NotJudged && o.value.is_none()), "an expectation the run did not reach is not judged");
        let app = AppFacts { case_run: Some(3), case_failed: None, benches: vec![("bringup".into(), run.clone())], print: Some(PrintFacts { current: true, at: 6.0, seconds: 2.5 * 3600.0, grams: 45.0 }) };
        let (with_app, _) = super::report(&docs, &app_facts(&cased, &app));
        assert_eq!(status(&with_app, "sits in its case"), &Status::Pass, "the newest case run on this board, without case errors, fits");
        assert_eq!((status(&with_app, "3V3 holds"), with_app.row("3V3 holds").unwrap().value.as_deref()), (&Status::Pass, Some("3.3V")), "one expectation of the counted check");
        assert!(matches!(status(&with_app, "LEDs light"), Status::Unknown(why) if why.contains("not judged")), "a bench with expectations not reached is unknown, never a pass");
        assert_eq!((status(&with_app, "under 40 g of filament"), with_app.row("under 40 g of filament").unwrap().stale), (&Status::Fail, false), "the slice figures are read");
        let newer = AppFacts { case_run: Some(4), ..app.clone() };
        let (asked, _) = super::report(&docs, &app_facts(&cased, &newer));
        assert!(matches!(status(&asked, "sits in its case"), Status::Unknown(why) if why.starts_with("stale:")), "a newer case run not in the snapshot yet makes the case row stale");
        let failed = AppFacts { case_failed: Some("roof: no solid".into()), ..newer.clone() };
        let (failed, _) = super::report(&docs, &app_facts(&cased, &failed));
        assert!(matches!(status(&failed, "sits in its case"), Status::Unknown(why) if why.contains("the case could not be rebuilt: roof: no solid")),
            "a failed case evaluation says why on the case row");
        let (recovered, _) = super::report(&docs, &app_facts(&cased, &AppFacts { case_failed: Some("roof: no solid".into()), ..app.clone() }));
        assert_eq!(status(&recovered, "sits in its case"), &Status::Pass, "a current case run reads current even beside an old failure");
        let (unchecked, _) = super::report(&docs, &app_facts(&snap, &app));
        assert!(matches!(status(&unchecked, "sits in its case"), Status::Unknown(why) if why.contains("have not run on ledctl")), "a board the snapshot holds no case run for is not checked yet");

        let answer = req_list(&docs, &design_facts(&snap), &[]).unwrap();
        assert!(answer.starts_with("{met: false documents: 1 pass: 4 fail: 2 unknown: 4 waived: 1 total: 11 requirements: ["), "totals lead, no page left: {answer}");
        assert!(answer.contains("{name: \"fits a 60 x 40 case\" status: pass value: \"50 x 30 mm\" bound: \"max 60 x 40 mm\" source: \"board outline\" doc: \"ledctl.req.splash\" line: 2}"), "a row in words, with where it is written: {answer}");
        assert!(answer.contains("reason: \"first prototype: time is no concern\" was: unknown"), "a waived row gives the person's reason: {answer}");
        let page = req_list(&docs, &design_facts(&snap), &[("from".into(), "10".into())]).unwrap();
        assert_eq!((page.matches("{name: ").count(), page.contains("next:")), (1, false), "from pages the rows: {page}");

        // A larger outline: the size row fails at once; the checks rows wait for a check still
        // running, showing their last count.
        let wide = Contour::rect(outline.min, P::new(outline.min.x + 70_000_000, outline.min.y + 50_000_000), 0);
        let mailbox = Mailbox::default();
        mailbox.push(Cmd::Commit {
            by: By::Person, ticket: None, place: vec![], label: "Outline".into(), circuits: vec![], expect: None,
            ops: vec![DesignOp::Board { unit, op: BoardOp::SetOutline(vec![wide]) }],
        });
        let step = engine.step(mailbox.take(), &mut |_| {});
        assert!(step.rejected.is_empty(), "the outline edit is taken");
        let edited = engine.snapshot();
        let (after, _) = super::report(&docs, &design_facts(&edited));
        assert_eq!((status(&after, "fits a 60 x 40 case"), after.row("fits a 60 x 40 case").unwrap().value.as_deref()), (&Status::Fail, Some("70 x 50 mm")), "the outline edit flips the size row");
        // An edit's answer says what it moved: the rows shown when it was sent against its commit's.
        let delta = req_delta(&marks(&report), Some(&after)).expect("requirements before and after");
        assert!(delta.starts_with("{met: false changed: [{name: \"fits a 60 x 40 case\" was: pass now: fail value: \"70 x 50 mm\" bound: \"max 60 x 40 mm\"}]")
            && delta.matches("was:").count() == 1, "only the row the edit moved, with its value and bound: {delta}");
        // While DRC runs, the snapshot the engine publishes has that stage stale.
        let mut running = DesignSnapshot::clone(&edited);
        running.stale = fab_elec_view::StageFlags::DRC;
        let (after, _) = super::report(&docs, &design_facts(&running));
        let drc = after.row("DRC clean").unwrap();
        assert!(matches!(&drc.status, Status::Unknown(why) if why.contains("still running")) && drc.stale && drc.value.is_some(),
            "a check still running makes its rows unknown, with the last count shown: {:?}", drc);
        assert_eq!(status(&after, "ERC clean"), &drc.status, "every checks row waits for the checks, a passing one too");
        let delta = req_delta(&marks(&report), Some(&after)).unwrap();
        assert!(delta.contains("waiting: [") && delta.contains("{name: \"DRC clean\" why: \"") && delta.contains("{name: \"ERC clean\" why: \"")
            && !delta.contains("name: \"DRC clean\" was:") && !delta.contains("name: \"ERC clean\" was:"),
            "rows whose producer runs again wait, with why; going stale is no change: {delta}");

        // Mistakes: located; an edit is refused only for one it adds.
        let broken = format!("{}require(\"thin\", board(\"ledctl\").volume, {{max: 1}})\n", fab_elec_fixtures::LEDCTL_REQ);
        let answer = req_list(&self::docs(&broken), &design_facts(&snap), &[]).unwrap();
        assert!(answer.contains("total: 11 mistakes: [{doc: \"ledctl.req.splash\" line: 14 severity: error message: "), "the broken statement is left out and located: {answer}");
        let refused = check_text(DocId(9), NAME, fab_elec_fixtures::LEDCTL_REQ, &broken, true).unwrap_err();
        assert!(refused.starts_with("ledctl.req.splash:14: "), "a change that adds a mistake is refused, located: {refused}");
        let elsewhere = broken.replace("{max: 2}", "{max: 4}");
        assert_eq!(check_text(DocId(9), NAME, &broken, &elsewhere, true), Ok(()), "a mistake already there does not stop an edit elsewhere");

        // The edits: each its document's new text, from the revision it read; what does not read
        // is refused before anything is sent.
        let args = |pairs: &[(&str, &str)]| pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect::<Vec<_>>();
        let edit = |change| match change {
            ReqEdit::Text { doc, rev, text } => (doc, rev, text),
            ReqEdit::New { .. } => panic!("an existing requirements document must be edited in place"),
        };
        let (doc, rev, added) = edit(req_edit(&docs, "elec.req.add", &args(&[("name", "fits a tin"), ("measure", "board(\"ledctl\").size"), ("bound", "{max: [80mm 50mm]}")]), false).unwrap());
        assert_eq!((doc.as_str(), rev), (NAME, 0), "an add writes the requirements document, from the revision it read");
        let (after, _) = super::report(&self::docs(&added), &design_facts(&snap));
        assert_eq!((status(&after, "fits a tin"), after.rows.len()), (&Status::Pass, 12), "the added requirement is judged with the rest");
        assert!(req_delta(&marks(&report), Some(&after)).unwrap().contains("changed: [{name: \"fits a tin\" was: new now: pass value: \"50 x 30 mm\""),
            "a row the edit wrote is new");
        assert_eq!(requirements_changed(fab_elec_fixtures::LEDCTL_REQ, &added), ["added the requirement `fits a tin`: board(\"ledctl\").size {max: [80mm 50mm]}"],
            "the AI says what it added");
        assert_eq!(req_delta(&[], None), None, "no requirements before or after: no field");
        let refused = req_edit(&docs, "elec.req.add", &args(&[("name", "x"), ("measure", "board(\"ledctl\").volume"), ("bound", "{max: 1}")]), false).unwrap_err();
        assert!(refused.contains("does not read as one requirement"), "a measure the reader does not know is refused: {refused}");
        let (_, _, waived) = edit(req_edit(&docs, "elec.req.waive", &args(&[("name", "DRC clean"), ("reason", "routing comes next")]), true).unwrap());
        let (after, _) = super::report(&self::docs(&waived), &design_facts(&snap));
        assert!(matches!(status(&after, "DRC clean"), Status::Waived { reason, .. } if reason == "routing comes next"), "a waiver with the person's reason");
        let (_, _, proposed) = edit(req_edit(&docs, "elec.req.waive", &args(&[("name", "DRC clean"), ("reason", "routing comes next")]), false).unwrap());
        let (after, _) = super::report(&self::docs(&proposed), &design_facts(&snap));
        let row = after.row("DRC clean").unwrap();
        assert!(row.status == Status::Fail && row.proposed.as_deref() == Some("routing comes next"), "an AI's waive is a proposal: the row still fails: {row:?}");
        assert!(req_list(&self::docs(&proposed), &design_facts(&snap), &[]).unwrap().contains("proposed: \"routing comes next\""), "the list shows the proposal");
        let smuggled = check_text(DocId(9), NAME, fab_elec_fixtures::LEDCTL_REQ, &waived, true).unwrap_err();
        assert!(smuggled.contains("only the person waives"), "an AI text change that writes a waiver is refused: {smuggled}");
        assert_eq!(check_text(DocId(9), NAME, fab_elec_fixtures::LEDCTL_REQ, &waived, false), Ok(()), "the person's own text may waive");
        assert_eq!(check_text(DocId(9), NAME, fab_elec_fixtures::LEDCTL_REQ, &proposed, true), Ok(()), "an AI text change may propose");
        let (_, _, unwaived) = edit(req_edit(&self::docs(&waived), "elec.req.unwaive", &args(&[("name", "DRC clean")]), true).unwrap());
        assert_eq!(unwaived, fab_elec_fixtures::LEDCTL_REQ, "unwaive undoes waive's text");
        let (_, _, removed) = edit(req_edit(&docs, "elec.req.remove", &args(&[("name", "two layers")]), false).unwrap());
        assert!(!removed.contains("two layers") && removed.len() < fab_elec_fixtures::LEDCTL_REQ.len(), "remove takes the statement's line");
        let (gone, _) = super::report(&self::docs(&removed), &design_facts(&snap));
        assert!(req_delta(&marks(&report), Some(&gone)).unwrap().contains("{name: \"two layers\" was: pass now: removed}"), "a row the edit took out is removed");
        // What the AI's text does to the person's requirements, in lines it repeats.
        let bounded = fab_elec_fixtures::LEDCTL_REQ.replace("layers, {max: 2}", "layers, {max: 4}");
        assert_eq!(requirements_changed(fab_elec_fixtures::LEDCTL_REQ, &bounded), ["changed the person's requirement `two layers`: {max: 2} → {max: 4}"], "a bound moved");
        assert_eq!(requirements_changed(fab_elec_fixtures::LEDCTL_REQ, &removed), ["took out the person's requirement `two layers`: board(\"ledctl\").layers {max: 2}"], "a requirement taken out");
        assert_eq!(requirements_changed(fab_elec_fixtures::LEDCTL_REQ, &proposed), ["proposed waiving `DRC clean` because routing comes next: the person accepts or declines it"], "a proposal");
        assert_eq!(requirements_changed(&waived, fab_elec_fixtures::LEDCTL_REQ), ["took out the person's waiver of `DRC clean` (routing comes next)"], "a waiver taken out");
        assert!(requirements_changed(fab_elec_fixtures::LEDCTL_REQ, fab_elec_fixtures::LEDCTL_REQ).is_empty(), "the same text says nothing");
        let project = DocsView { texts: ["part.part.splash", "ledctl.ckt.splash", "aux.ckt.splash"].iter().enumerate()
            .map(|(i, name)| DocText { doc: DocId(i as u32), name: (*name).into(), rev: 0, text: "".into() }).collect(),
            ..Default::default() };
        let target = args(&[("name", "two layers"), ("measure", r#"board("ledctl").layers"#), ("bound", "{max: 2}")]);
        assert_eq!(req_edit(&project, REQ_ADD.name, &target, false),
            Ok(ReqEdit::New { doc: "ledctl.req.splash".into(), text: "require(\"two layers\", board(\"ledctl\").layers, {max: 2})\n".into() }),
            "the first circuit names the new document, containing the requested statement alone");
        assert!(matches!(req_edit(&DocsView::default(), REQ_ADD.name, &target, true), Ok(ReqEdit::New { doc, .. }) if doc == "device.req.splash"),
            "without a circuit, Add creates device.req.splash");
        let wish = args(&[("words", " warm light ")]);
        let (name, _, wished) = edit(req_edit(&docs, REQ_WISH.name, &wish, false).unwrap());
        assert!(name == NAME && requirements_changed(fab_elec_fixtures::LEDCTL_REQ, &wished) == ["added the wish `warm light`"],
            "Wish appends to the existing document and narrates the addition");
        let Ok(ReqEdit::New { doc, text }) = req_edit(&project, REQ_WISH.name, &wish, true) else { panic!("Wish can create the first document") };
        assert!(doc == NAME && fab_elec_req::read(DocId(9), &text).0.wishes[0].words == "warm light",
            "Wish uses Add's first-circuit naming rule and trims the words");
        assert!(req_edit(&docs, REQ_WISH.name, &args(&[("words", " ")]), true).is_err(), "an empty wish is refused");
        assert_eq!(requirements_changed(&text.repeat(2), &text), ["took out the person's wish `warm light`"],
            "removing one of two identical wishes is narrated once");
        let original = "// start\nbrief(\"  Warm light  \")\n";
        let brief_args = |s: &str| args(&[("words", s)]);
        let words = "  Warm \"desk\" light\nwith a switch.  ";
        let save = |docs: &DocsView, s, person| req_edit(docs, REQ_BRIEF.name, &brief_args(s), person);
        let Ok(ReqEdit::New { doc: file, text: new }) = save(&DocsView::default(), words, true) else { panic!("the first brief creates a requirements file") };
        assert!(file == "device.req.splash" && fab_elec_req::read(DocId(0), &new).0.brief.unwrap().words == words,
            "the empty plate stores the person's quotes, newline and surrounding spaces verbatim");
        assert_eq!(save(&DocsView::default(), " \n", true).unwrap_err(), "there is no brief to save", "an empty brief creates no file");
        let (file, _, inserted) = edit(save(&docs, words, true).unwrap());
        assert!(file == NAME && inserted == format!("{new}{}", fab_elec_fixtures::LEDCTL_REQ),
            "a brief goes before the first requirements document's existing statements");
        let mut two = docs.clone();
        two.texts.push(DocText { doc: DocId(10), name: "other.req.splash".into(), text: original.into(), rev: 7 });
        let (file, rev, replaced) = edit(save(&two, words, true).unwrap());
        assert!(file == "other.req.splash" && rev == 7 && replaced == format!("// start\n{new}"),
            "replacing the existing brief edits its own document and preserves the surrounding text");
        assert_eq!(edit(save(&two, " \n", true).unwrap()), ("other.req.splash".into(), 7, "// start\n".into()),
            "whitespace-only words remove the existing brief in place");
        assert_eq!(save(&two, words, false).unwrap_err(), "the brief is the person's own words: they write it in the Requirements tab",
            "a tool cannot write the person's brief");
        for (before, after) in [("// start\n", original), (original, "// start\n"),
            (original, "// start\nbrief(\"Cold light\")\n"), (original, "// start\nbrief( \"  Warm light  \")\n")] {
            assert_eq!(check_text(DocId(9), NAME, before, after, true).unwrap_err(),
                format!("{NAME}:2: the brief is the person's own words: leave brief(..) as it is"),
                "adding, removing or rewriting the brief is refused at its source line");
        }
        let with_req = format!("require(\"layers\", board(\"ledctl\").layers, {{max: 2}})\n{original}{text}");
        assert!(check_text(DocId(9), NAME, original, &with_req, true).is_ok(),
            "moving an unchanged brief while adding a requirement is allowed");
        let list = req_list(&self::docs(&with_req), &design_facts(&snap), &args(&[("from", "1")])).unwrap();
        let reply = makepad_widgets_core::makepad_script::syntax::parse_value(&list).unwrap();
        let brief = &reply.field("brief").unwrap().value;
        let [wish] = reply.field("wishes").unwrap().value.items().unwrap() else { panic!("one wish on every page") };
        for (row, words, line) in [(brief, "  Warm light  ", 3.), (wish, "warm light", 4.)] {
            assert!(row.field("words").unwrap().value.string() == Some(words) && row.field("doc").unwrap().value.string() == Some(NAME)
                && row.field("line").unwrap().value.number() == Some(line), "the unpaged words retain their text, document and line");
        }
        assert!(reply.field("total").unwrap().value.number() == Some(1.) && reply.field("requirements").unwrap().value.items().unwrap().is_empty(),
            "paging counts only measured requirements");
    }
}

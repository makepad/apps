//! The next action available on the committed electronics design.
use crate::DesignSnapshot;
use fab_elec_circuit::CheckKind;
use fab_elec_core::Severity;
use fab_elec_design::Unit;
use fab_elec_lang::CatalogKind;
use fab_elec_parts::PartKind;
use fab_elec_req::{Bound, Measure, Report, ReqDoc, Value};
use std::collections::HashSet;

/// The family a check belongs to, as `elec.check.list` names it.
pub fn check_family(kind: &CheckKind) -> &'static str {
    use CheckKind::*;
    match kind {
        PinConflict | UndrivenInput | PowerNotDriven | UnconnectedPin | SinglePinNet | DuplicateRefdes
        | MissingFootprint | VoltageDomain | RailBudget | OpenPort => "erc",
        Mech | LinkPinMap | LinkMismatch | SystemVolts | SystemSource => "system",
        SharpCorner => "route",
        Custom(_) => "custom",
        _ => "drc",
    }
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum FlowStage {
    Requirements, Architecture, Parts, Schematic, FixErc, Place, Route, Done,
}

impl FlowStage {
    pub fn name(self) -> &'static str {
        match self {
            Self::Requirements => "requirements",
            Self::Architecture => "architecture",
            Self::Parts => "parts",
            Self::Schematic => "schematic",
            Self::FixErc => "fix_erc",
            Self::Place => "place",
            Self::Route => "route",
            Self::Done => "done",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NextStep {
    pub stage: FlowStage,
    pub button: &'static str,
    pub summary: String,
    pub unit: Option<String>,
}

/// Derived once per committed snapshot, never per frame. O(units + blocks + components +
/// catalog entries + requirement rows + findings). Route and Done wait for settled connectivity.
pub fn next_step(snap: &DesignSnapshot, req: Option<&Report>, last_accepted_slot: Option<&str>) -> Option<NextStep> {
    use FlowStage::*;
    let primary = snap.design.units.iter().find(|u| u.board.is_some());
    let step = |stage, button, summary: &str| Some(NextStep {
        stage, button, summary: summary.to_owned(), unit: primary.map(|u| u.name.clone()),
    });
    let Some(report) = req.filter(|r| !r.rows.is_empty()) else {
        return step(Requirements, "Draft requirements", "Turn the brief into requirements");
    };
    if !snap.design.units.iter().any(|u| u.circuit.tree.len() > 1 || !u.circuit.comps.is_empty()) {
        return step(Architecture, "Draft architecture", "Sketch the blocks and their connections");
    }
    if snap.design.units.iter().any(|u| u.circuit.tree.iter().any(|b|
        b.def.and_then(|id| u.circuit.block_defs.get(id.0 as usize)).is_some_and(|def| def.planned)
    )) {
        let picked = last_accepted_slot == Some("parts") || snap.catalog.as_ref().is_some_and(|catalog|
            catalog.entries.iter().any(|e| e.lib.is_empty() && matches!(e.kind, CatalogKind::Part | CatalogKind::Passive)));
        return if picked {
            step(Schematic, "Draft schematic", "Wire the chosen parts inside the planned blocks")
        } else {
            step(Parts, "Choose parts", "Choose parts for the planned blocks")
        };
    }
    // Both lists are sorted by finding id.
    let mut awaiting = snap.awaiting.iter().copied().peekable();
    for finding in snap.violations.value.iter() {
        while awaiting.next_if(|&id| id < finding.id).is_some() {}
        if awaiting.peek() != Some(&finding.id) && finding.severity == Severity::Error && check_family(&finding.kind) == "erc" {
            return step(FixErc, "Fix ERC", "Resolve the schematic’s electrical errors");
        }
    }
    let unit = primary?;
    let board = unit.board.as_ref()?;
    let placed: HashSet<_> = board.placed.iter().map(|p| p.comp).collect();
    if board.outline.is_empty() || unit.circuit.comps.iter().any(|c| c.part.kind != PartKind::PowerFlag && !placed.contains(&c.id)) {
        return step(Place, "Place parts", "Place the components on the board");
    }
    if snap.checking(unit.id) { return None; }
    let unrouted = snap.of(&snap.conn, unit.id)?.value.unrouted.len();
    if unrouted != 0 {
        return step(Route, "Route board", &format!("Route {unrouted} unrouted {}", if unrouted == 1 { "net" } else { "nets" }));
    }
    if report.met() {
        step(Done, "Export", "Board routed · all requirements met")
    } else {
        let n = report.fail + report.unknown;
        step(Done, "Export", &format!("Board routed · {n} {} not met", if n == 1 { "requirement" } else { "requirements" }))
    }
}

/// The initial rectangular outline in millimetres. Unitless bounds belong to the only board.
pub fn initial_outline_mm(unit: &Unit, req_docs: &[ReqDoc], single_board: bool) -> (f64, f64) {
    let mut lower = [0.0_f64; 2];
    let mut upper = None::<[f64; 2]>;
    let mut has_min = false;
    for rule in req_docs.iter().flat_map(|doc| &doc.requirements) {
        let Measure::BoardSize(target) = &rule.measure else { continue };
        if !target.as_ref().map_or(single_board, |name| name == &unit.name) { continue; }
        match &rule.bound {
            Bound::Is { value: Value::Size([w,h]), .. } => return (w * 1000.0, h * 1000.0),
            Bound::Range { min, max } => {
                if let Some(Value::Size(size)) = min {
                    has_min = true;
                    for i in 0..2 { lower[i] = lower[i].max(size[i] * 1000.0); }
                }
                if let Some(Value::Size(size)) = max {
                    let limit = upper.get_or_insert([f64::INFINITY; 2]);
                    for i in 0..2 { limit[i] = limit[i].min(size[i] * 1000.0); }
                }
            }
            _ => {}
        }
    }
    if let Some(max) = upper {
        let side = |i: usize| ((max[i] * 0.85 / 5.0).floor() * 5.0)
            .clamp((if has_min { lower[i] } else { 20.0 }).min(max[i]), max[i]);
        return (side(0), side(1));
    }
    let area_mm2: f64 = unit.circuit.comps.iter().filter_map(|c| c.part.footprint.as_ref())
        .map(|fp| fp.courtyard.area() * 1e-12).sum();
    let h = (2.0 * area_mm2 * 3.0 / 5.0).sqrt();
    let round = |len: f64| (len / 5.0).ceil() * 5.0;
    (round(h * 5.0 / 3.0).max(lower[0]).max(30.0), round(h).max(lower[1]).max(20.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{snapshot::tests::snapshot, StateOf, Tagged};
    use fab_elec_board::Connectivity;
    use fab_elec_circuit::Violation;
    use fab_elec_core::{DocId, NetId};
    use fab_elec_req::{BoardMeasures, Facts, UnitFacts};
    use std::sync::Arc;

    fn req(text: &str) -> ReqDoc {
        let (doc, errors) = fab_elec_req::read(DocId(0), text);
        assert!(errors.is_empty(), "test requirements must read: {errors:?}");
        doc
    }

    fn size_report(limit: &str) -> Report {
        let facts = Facts {
            units: vec![UnitFacts {
                unit: "pair".into(),
                board: Some(BoardMeasures { size: Some([0.020, 0.010]), layers: 2, parts: 2, unplaced: 0 }),
            }],
            ..Facts::default()
        };
        fab_elec_req::judge(&[req(&format!("require(\"fits\", board.size, {{max: [{limit} 10mm]}})"))], &facts)
    }

    fn placed_pair() -> DesignSnapshot {
        let mut sources = fab_elec_lang::Sources::new();
        sources.add("pair.ckt.splash", r#"let e = mod.elec
let pair = e.block({name: "Pair" ports: {a: e.port(@passive) b: e.port(@passive)}
    build: fn(p, o) { let r = e.r(1kohm, p.a, p.b, {package: "0402"}); let s = e.r(2kohm, p.a, p.b, {package: "0402"}) }})
let a = e.net({name: "A"})
let b = e.net({name: "B"})
let load = pair({a: a b: b})
"#).unwrap();
        sources.add("pair.pcb.splash", r#"board("pair")
outline([[[0mm, 0mm], [20mm, 0mm], [20mm, 10mm], [0mm, 10mm], [0mm, 0mm]]])
place("load.r", [5mm, 5mm])
place("load.s", [10mm, 5mm])
"#).unwrap();
        let evaluated = fab_elec_lang::evaluate(&sources, &fab_elec_lang::std_libs()).expect("the placed pair must evaluate");
        let mut snap = snapshot(Arc::new(evaluated.design));
        let unit = &snap.design.units[0];
        let board = unit.board.as_ref().unwrap();
        snap.conn.push(Tagged { unit: Some(unit.id), of: StateOf::Committed, generation: board.generation,
            value: Arc::new(Connectivity { unrouted: vec![NetId(0), NetId(1)], ..Connectivity::default() }) });
        snap
    }

    fn check(snap: &DesignSnapshot, report: Option<&Report>, slot: Option<&str>, expected: FlowStage, button: &str) {
        let next = next_step(snap, report, slot).expect("the committed facts determine the next stage");
        assert_eq!((next.stage, next.button), (expected, button), "wrong offer for {expected:?}");
    }

    fn erc_error(snap: &DesignSnapshot) -> Violation {
        Violation::new(CheckKind::PowerNotDriven, Severity::Error, Some(snap.design.units[0].id), vec![], "power", "undriven input".into())
    }

    #[test]
    fn next_step_progresses_cleanly_through_all_stages_from_committed_state() {
        use FlowStage::*;
        let empty = snapshot(Arc::default());
        let report = size_report("20mm");
        check(&empty, None, None, Requirements, "Draft requirements");
        check(&empty, Some(&report), None, Architecture, "Draft architecture");
        let mut snap = placed_pair();
        Arc::make_mut(&mut Arc::make_mut(&mut snap.design).units[0].circuit).block_defs[0].planned = true;
        check(&snap, Some(&report), None, Parts, "Choose parts");
        check(&snap, Some(&report), Some("parts"), Schematic, "Draft schematic");
        check(&snap, Some(&report), None, Parts, "Choose parts");
        let part = fab_elec_lang::from_part_docs("", &[("chosen.part.splash".into(),
            Arc::from("let e = mod.elec\ne.define(\"pick\", {kind: @ic symbol: e.f.ic_symbol(\"pick\", {left: [[\"IN\" @in]]})})\n"))]);
        snap.catalog = Some(Arc::new(fab_elec_lang::Catalog { entries: part }));
        check(&snap, Some(&report), None, Schematic, "Draft schematic");
        Arc::make_mut(&mut Arc::make_mut(&mut snap.design).units[0].circuit).block_defs[0].planned = false;
        let board = Arc::make_mut(Arc::make_mut(&mut snap.design).units[0].board.as_mut().unwrap());
        let outline = std::mem::take(&mut board.outline);
        let error = erc_error(&snap);
        snap.violations.value = Arc::new(vec![error.clone()]);
        check(&snap, Some(&report), None, FixErc, "Fix ERC");
        snap.awaiting = Arc::new(vec![error.id]);
        check(&snap, Some(&report), None, Place, "Place parts");
        Arc::make_mut(Arc::make_mut(&mut snap.design).units[0].board.as_mut().unwrap()).outline = outline;
        let unit = snap.design.units[0].id;
        snap.checking.push(unit);
        assert!(next_step(&snap, Some(&report), None).is_none(), "pending whole-board checks cannot offer Route or Export");
        snap.checking.clear();
        check(&snap, Some(&report), None, Route, "Route board");
        assert_eq!(next_step(&snap, Some(&report), None).unwrap().summary, "Route 2 unrouted nets",
            "the Route offer must name the action and the settled net count");
        Arc::make_mut(&mut snap.conn[0].value).unrouted.pop();
        assert_eq!(next_step(&snap, Some(&report), None).unwrap().summary, "Route 1 unrouted net",
            "the last unrouted net must use the singular");
        Arc::make_mut(&mut snap.conn[0].value).unrouted.clear();
        check(&snap, Some(&report), None, Done, "Export");
        assert_eq!(next_step(&snap, Some(&report), None).unwrap().summary, "Board routed · all requirements met",
            "a routed design with all targets met should say so");
    }

    #[test]
    fn next_step_allows_skipping_or_redoing_without_wedging() {
        let mut snap = placed_pair();
        let report = size_report("15mm");
        let board = Arc::make_mut(Arc::make_mut(&mut snap.design).units[0].board.as_mut().unwrap());
        let part = board.placed.pop().unwrap();
        check(&snap, Some(&report), Some("requirements"), FlowStage::Place, "Place parts");
        Arc::make_mut(Arc::make_mut(&mut snap.design).units[0].board.as_mut().unwrap()).placed.push(part);
        Arc::make_mut(&mut snap.conn[0].value).unrouted.clear();
        let done = next_step(&snap, Some(&report), None).unwrap();
        assert_eq!((done.stage, done.summary.as_str()), (FlowStage::Done, "Board routed · 1 requirement not met"),
            "a routed board that exceeds its size limit must report the open requirement");
        snap.violations.value = Arc::new(vec![erc_error(&snap)]);
        check(&snap, Some(&report), None, FlowStage::FixErc, "Fix ERC");
    }

    #[test]
    fn initial_outline_mm_respects_board_size_requirement_bound() {
        let snap = placed_pair();
        let unit = &snap.design.units[0];
        let bounds = req(r#"require("box", board.size, {max: [60mm 40mm]})
require("other", board("other").size, {is: [7mm 9mm]})"#);
        assert_eq!(initial_outline_mm(unit, std::slice::from_ref(&bounds), true), (50.0, 30.0), "the single-board maximum sets the initial rectangle");
        assert_eq!(initial_outline_mm(unit, std::slice::from_ref(&bounds), false), (30.0, 20.0), "other units and ambiguous unitless bounds must leave a nonzero fallback");
        let minimum = req(r#"require("minimum", board("pair").size, {min: [56mm 38mm]})"#);
        assert_eq!(initial_outline_mm(unit, &[bounds.clone(), minimum.clone()], true), (56.0, 38.0), "the intersection must respect the minimum");
        let exact = req(r#"require("exact", board("pair").size, {is: [58mm 39mm]})"#);
        assert_eq!(initial_outline_mm(unit, &[bounds, minimum, exact], true), (58.0, 39.0), "the exact size must win over the range");
    }
}

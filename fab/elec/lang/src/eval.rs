//! Evaluation (C5): a project's documents in, a typed `Design` and its source map out.
//!
//! Each circuit document runs in its own VM with `mod.elec` (the `e.` circuit builders, the `f.`
//! part drawing, `param`, rules) and the electrical unit suffixes; library documents are
//! modules the circuit `use`s. Builders record what they make with where they were called; the
//! lowering turns the records into a `Circuit` per unit, names every item by its bindings, and
//! fills the `SourceMap`. Board documents are statements with literals only and lower straight
//! into the unit's `Board` (ELANG design, gate 3).
//!
//! Units: one per present circuit document, `BoardId` by first assignment order, named by
//! the file stem (`ledctl.ckt.splash` is unit `ledctl`); its board is `<stem>.pcb.splash` when the
//! project has one, else an empty board with the circuit's nets.
//!
//! Errors are located: each `Diag` carries `Sel::Source` for the range it is about, and its
//! message names the binding (`reg.c_in: ...`).
//!
//! Cost: one VM per circuit document; the std library is parsed and run once per document (the
//! parts cache of architect Q3 comes with the per-unit states).

use crate::catalog::ProjectDefs;
use crate::circuit::{self, Phases, UnitOut};
use crate::map::{PinSite, SourceMap, SourceRange, ValueSite};
use crate::source::{Doc, DocKind, Libs, Sources, located};
use fab_elec_bench::{BenchSpec, expect::Expect};
use fab_elec_board::Board;
use fab_elec_circuit::{Circuit, RefKind};
use fab_elec_core::{BENCH_SCOPE_BASE, BoardId, CompId, CompRef, Diag, DocId, PinIx, Sel, Xform3};
use fab_elec_design::{Assembly, Design, Frame, Link, Mount, ProbeOf, Tip, Unit, UnitKind};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// What evaluation gives: the design, where everything in it is written, how each unit came out,
/// and notes (warnings and located errors of documents no unit owns).
#[derive(Clone, PartialEq, Debug)]
pub struct Evaluated {
    /// Library snapshot used to build or reuse the units.
    pub libs: Libs,
    /// The names a mesh found when these units were evaluated: the project's assets and the part
    /// library's models ([`Sources::mesh_assets`]).
    pub assets: Vec<String>,
    pub design: Design,
    pub map: SourceMap,
    /// One entry per present circuit document, in document order.
    pub units: Vec<UnitEval>,
    /// Every circuit document assigned a unit id, in assignment order; never pruned.
    pub unit_ids: Vec<(DocId, BoardId)>,
    /// Benches in document, then call order.
    pub benches: Vec<BenchOut>,
    /// Warnings that do not stop the design, and located errors of documents no unit owns.
    pub notes: Vec<Diag>,
    /// The literal system document, lowered against this evaluation's units.
    pub system: SysEval,
    /// Lazy library catalog inputs, shared across circuit-only evaluations.
    pub defs: Arc<ProjectDefs>,
}

/// The system pass has independent diagnostics and never keeps stale component references.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct SysEval {
    pub doc: Option<DocId>,
    pub diags: Vec<Diag>,
    pub took: Duration,
}

/// The system declarations lowered against the current units.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct SystemLowered {
    pub links: Vec<Link>,
    pub mounts: Vec<Mount>,
    pub system: SysEval,
}

/// The circuit a bench runs on.
#[derive(Clone, PartialEq, Debug)]
pub enum BenchScope {
    /// A unit of the design: the bench addresses it by its id.
    Design(BoardId),
    /// The bench's own circuit, built by its `circuit` function. `circuit.unit` is the bench's
    /// scope id, from the bench id range; `generation` moves only when this circuit changes.
    Own { circuit: Arc<Circuit>, generation: u64 },
}

/// A bench bound to a design unit or its own circuit.
#[derive(Clone, PartialEq, Debug)]
pub struct BenchOut {
    pub name: String,
    pub doc: DocId,
    /// What the bench runs on.
    pub scope: BenchScope,
    pub spec: BenchSpec,
    pub expects: Vec<Expect>,
    pub probes: Vec<BenchProbe>,
    pub stop: Option<f64>,
    /// The `run` option `live`: the bench's take opens in Record live (real time, heard as it is
    /// made) instead of Loop or Continuous; the person can still switch, to Loop for full quality.
    pub live: bool,
    /// The `panel` option: how the bench's controls panel shows the board's switches and pots.
    /// None keeps the panel generated from the design.
    pub panel: Option<BenchPanel>,
    pub state: UnitState,
}

impl BenchOut {
    /// The id the bench's refs carry: the design unit's, or the bench's own scope id.
    pub fn scope_id(&self) -> BoardId {
        match &self.scope {
            BenchScope::Design(id) => *id,
            BenchScope::Own { circuit, .. } => circuit.unit,
        }
    }

    /// The circuit the bench runs on: the design unit's (None when the design has no such
    /// unit), or its own.
    pub fn circuit<'a>(&'a self, design: &'a Design) -> Option<&'a Circuit> {
        match &self.scope {
            BenchScope::Design(id) => design.unit(*id).map(|unit| unit.circuit.as_ref()),
            BenchScope::Own { circuit, .. } => Some(circuit.as_ref()),
        }
    }
}

/// A bench's `panel` option, data only (Fab's AI may write it): which switches and pots go in
/// which group, in what order, under what label. It arranges and never hides: every switch and
/// potentiometer it does not name keeps its generated control, in the groups after these.
///
/// ```text
/// bench("board", {circuit: "drum16" panel: [
///     {group: "Steps" cols: 8 controls: ["SW1" "SW2" "SW3" "SW4" "SW5" "SW6" "SW7" "SW8"]}
///     {group: "Kick" controls: [{part: "RV1" label: "Decay"} {part: "RV2" label: "Tune"}]}
/// ]}, fn(b, c) { ... })
/// ```
///
/// A group needs `group` (its name) and `controls`; `cols` wraps it after that many columns
/// (one row without it). A control is a reference designator, or `{part, label}` to relabel it.
/// Each named part is a switch or potentiometer of the bench's circuit, named once.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct BenchPanel {
    pub groups: Vec<PanelGroup>,
}

/// One group of a [`BenchPanel`], in the order written.
#[derive(Clone, PartialEq, Debug)]
pub struct PanelGroup {
    pub name: String,
    /// Columns before the group wraps; None: one row.
    pub cols: Option<u16>,
    pub controls: Vec<PanelEntry>,
}

/// A named control: its part, and the label the panel gives it (None keeps the generated one).
#[derive(Clone, PartialEq, Debug)]
pub struct PanelEntry {
    pub comp: CompId,
    pub label: Option<String>,
}

/// A scope channel in call order, identified by what it reads.
#[derive(Clone, PartialEq, Debug)]
pub struct BenchProbe {
    pub of: ProbeOf,
    pub label: String,
    pub tip: Tip,
    /// Where its named connection sits on the drawings: a pin of its net, `"<refdes>.<pad>"`
    /// (`at:`); None lets each drawing pick one.
    pub at: Option<String>,
}

/// One unit's evaluation.
#[derive(Clone, PartialEq, Debug)]
pub struct UnitEval {
    pub id: BoardId,
    pub name: String,
    /// Its circuit document, then its board document when present, then project parts and blocks.
    pub docs: Vec<DocId>,
    /// Texts of the last evaluation attempt, in `docs` order (including failed attempts).
    pub texts: Vec<Arc<str>>,
    pub state: UnitState,
    /// True when unchanged inputs reused the previous state and lowering without a VM run.
    pub reused: bool,
    /// Warnings from this unit, retained when it is reused.
    pub notes: Vec<Diag>,
    /// How long building or reusing it took (building runs the VM and lowers its output):
    /// what the explorer's 100 ms after a pause (I2) is spent on.
    pub took: std::time::Duration,
    /// Where that went, when it built.
    pub phases: Option<Phases>,
}

/// How a unit came out of an evaluation (architect change 2).
#[derive(Clone, PartialEq, Debug)]
pub enum UnitState {
    /// Built from its documents as they are now.
    Built,
    /// Its documents have mistakes: the design keeps the unit's last good lowering and its map
    /// entries (the host shows it with the stale look) until they are fixed.
    Kept(Vec<Diag>),
    /// Its documents have mistakes and it was never built: the design has no unit for it.
    Failed(Vec<Diag>),
}

impl UnitState {
    /// The located mistakes that keep the unit from building.
    pub fn diags(&self) -> &[Diag] {
        match self {
            UnitState::Built => &[],
            UnitState::Kept(d) | UnitState::Failed(d) => d,
        }
    }
}

/// The std library's documents, built into the crate: the part and footprint generators.
/// `std.part.splash` runs first; the package families and the blocks follow in name order.
const STD: [(&str, &str); 18] = [
    ("std.part.splash", include_str!("../std/std.part.splash")),
    ("array.part.splash", include_str!("../std/array.part.splash")),
    ("bga.part.splash", include_str!("../std/bga.part.splash")),
    ("can.part.splash", include_str!("../std/can.part.splash")),
    ("catalog.part.splash", include_str!("../std/catalog.part.splash")),
    ("conn.part.splash", include_str!("../std/conn.part.splash")),
    ("flat2.part.splash", include_str!("../std/flat2.part.splash")),
    ("mech.part.splash", include_str!("../std/mech.part.splash")),
    ("module.part.splash", include_str!("../std/module.part.splash")),
    ("qfn.part.splash", include_str!("../std/qfn.part.splash")),
    ("qfp.part.splash", include_str!("../std/qfp.part.splash")),
    ("sc70.part.splash", include_str!("../std/sc70.part.splash")),
    ("soic.part.splash", include_str!("../std/soic.part.splash")),
    ("tab.part.splash", include_str!("../std/tab.part.splash")),
    ("tht.part.splash", include_str!("../std/tht.part.splash")),
    ("to.part.splash", include_str!("../std/to.part.splash")),
    ("xtal.part.splash", include_str!("../std/xtal.part.splash")),
    ("core.blocks.splash", include_str!("../std/core.blocks.splash")),
];

/// The libraries every project has: `std`.
pub fn std_libs() -> Libs {
    let mut libs = Libs::new();
    for (name, text) in STD {
        libs.add("std", name, text).expect("the std documents have distinct electronics names");
    }
    libs
}

/// Why an evaluation gives no design.
#[derive(Clone, Debug, PartialEq)]
pub enum EvalError {
    /// No unit was built or kept (`evaluate_from`), or the edit was refused or its result failed a
    /// request (`splice_verified`): every mistake, located.
    Diags(Vec<Diag>),
    /// The caller's cancel flag was set: the work stopped before a unit or a bench document, and
    /// gives nothing, not a partial design.
    Cancelled,
}

impl EvalError {
    /// The mistakes; none when cancelled.
    pub fn diags(&self) -> &[Diag] {
        match self {
            EvalError::Diags(diags) => diags,
            EvalError::Cancelled => &[],
        }
    }

    /// Every mistake, for a caller that lists them: a cancelled run says so in one.
    pub fn into_diags(self) -> Vec<Diag> {
        match self {
            EvalError::Diags(diags) => diags,
            EvalError::Cancelled => vec![EvalError::Cancelled.into_diag()],
        }
    }

    /// The first mistake, for a caller that reports one: a cancelled run says so.
    pub fn into_diag(self) -> Diag {
        match self {
            EvalError::Diags(diags) => diags.into_iter().next().unwrap_or_else(|| Diag::error("evaluation failed", vec![])),
            EvalError::Cancelled => Diag::error("the evaluation was cancelled", vec![]),
        }
    }
}

/// The cancel flag of a caller that never cancels: never set.
pub static NO_CANCEL: AtomicBool = AtomicBool::new(false);

/// Evaluate a project's documents against the installed libraries, from nothing.
pub fn evaluate(sources: &Sources, libs: &Libs) -> Result<Evaluated, Vec<Diag>> {
    // A flag of its own, which nothing can set: this evaluation is never cancelled.
    let never = AtomicBool::new(false);
    evaluate_from(None, sources, libs, &never).map_err(|e| match e {
        EvalError::Diags(diags) => diags,
        EvalError::Cancelled => unreachable!("a flag nothing can set"),
    })
}

/// Evaluate again after an edit. A unit whose documents now have mistakes keeps its lowering from
/// `prev`; units keep their `BoardId`s (a new circuit document takes the next free one, architect
/// change 6). `prev.map` must describe `sources` as they are now (every edit since `prev` was
/// followed): board and system statements keep the ids their map entries start at. `Err` when no unit
/// is built or kept (every mistake, located), or when `cancel` is set: it is read (relaxed) before each
/// unit and each bench document, so a cancel takes effect within one unit's or one document's work.
pub fn evaluate_from(prev: Option<&Evaluated>, sources: &Sources, libs: &Libs, cancel: &AtomicBool) -> Result<Evaluated, EvalError> {
    let mut map = SourceMap::new();
    let mut units = Vec::new();
    let mut evals = Vec::new();
    let mut notes = Vec::new();
    let mut unit_ids = prev.map_or_else(Vec::new, |p| p.unit_ids.clone());
    let mut next_free = unit_ids.iter().map(|(_, id)| usize::from(id.0) + 1).max().unwrap_or(0);
    let present = sources.docs();
    let circuits = present.iter().copied().filter(|d| d.kind == DocKind::Circuit);
    let parts: Vec<&Doc> = present.iter().copied().filter(|d| d.kind == DocKind::Part).collect();
    let mut blocks: Vec<&Doc> = present.iter().copied().filter(|d| d.kind == DocKind::Blocks).collect();
    blocks.sort_by(|a, b| a.name.cmp(&b.name));
    let assets = sources.mesh_assets();
    for doc in circuits {
        if cancel.load(Ordering::Relaxed) {
            return Err(EvalError::Cancelled);
        }
        let before = prev.and_then(|p| p.units.iter().find(|u| u.docs.first() == Some(&doc.id)));
        let id = match unit_ids.iter().find(|(d, _)| *d == doc.id) {
            Some((_, id)) => *id,
            None => {
                // Exhausted attempts retain a sentinel; no unit is built with it.
                let id = BoardId(next_free.min(usize::from(BENCH_SCOPE_BASE)) as u16);
                next_free += 1;
                unit_ids.push((doc.id, id));
                id
            }
        };
        let stem = doc.name.strip_suffix(DocKind::Circuit.suffix()).unwrap_or(&doc.name).to_string();
        let board_doc = sources.find(&format!("{stem}{}", DocKind::Board.suffix())).and_then(|b| sources.doc(b));
        let docs: Vec<DocId> = std::iter::once(doc.id).chain(board_doc.map(|b| b.id)).chain(parts.iter().chain(&blocks).map(|d| d.id)).collect();
        let start = std::time::Instant::now();
        let texts: Vec<Arc<str>> = std::iter::once(doc.text.clone()).chain(board_doc.map(|b| b.text.clone())).chain(parts.iter().chain(&blocks).map(|d| d.text.clone())).collect();
        if id.0 >= BENCH_SCOPE_BASE {
            let diags = vec![located(doc.id, 0..0, "the project has more units than unit ids")];
            evals.push(UnitEval { id, name: stem, docs, texts, state: UnitState::Failed(diags), reused: false, notes: vec![], took: start.elapsed(), phases: None });
            continue;
        }
        if let Some((p, b)) = prev.zip(before).filter(|(p, b)| {
            b.name == stem && b.docs == docs && b.texts.len() == texts.len()
                && b.texts.iter().zip(&texts).all(|(a, b)| Arc::ptr_eq(a, b))
                && p.libs.same(libs)
                && p.assets.as_slice() == assets
        }) {
            for d in &b.docs {
                for (sel, range) in p.map.of_doc(*d) {
                    if sel.unit().is_some_and(|unit| unit != b.id) {
                        continue;
                    }
                    map.insert(sel, range);
                }
            }
            if let Some(added) = p.map.added.get(&b.id) { map.added.insert(b.id, added.clone()); }
            for (comp, site) in p.map.value_sites_of(b.id) {
                map.set_value_site(comp, site);
            }
            for ((comp, pin), sites) in p.map.pin_sites_of(b.id) {
                map.set_pin_sites(comp, pin, sites);
            }
            for (part, site) in p.map.body_sites_of(b.id) {
                map.set_body_site(b.id, part, site);
            }
            if let Some(unit) = p.design.units.iter().find(|u| u.id == b.id) {
                units.push(unit.clone());
            }
            notes.extend(b.notes.iter().cloned());
            let mut reused = b.clone();
            reused.reused = true;
            reused.took = start.elapsed();
            evals.push(reused);
            continue;
        }
        let built = build_unit(id, &stem, doc, board_doc, sources, libs, prev);
        let took = start.elapsed();
        match built {
            Ok((unit, entries, added, values, pins, bodies, phases, unit_notes)) => {
                if !added.is_empty() { map.added.insert(id, added); }
                notes.extend(unit_notes.iter().cloned());
                for (sel, range) in entries {
                    map.insert(sel, range);
                }
                for (comp, site) in values {
                    map.set_value_site(comp, site);
                }
                for ((comp, pin), sites) in pins {
                    map.set_pin_sites(comp, pin, sites);
                }
                for (part, site) in bodies {
                    map.set_body_site(id, part, site);
                }
                units.push(unit);
                evals.push(UnitEval { id, name: stem, docs, texts, state: UnitState::Built, reused: false, notes: unit_notes, took, phases: Some(phases) });
            }
            Err(diags) => {
                let kept = prev.zip(before).and_then(|(p, b)| Some((p, b, p.design.units.iter().find(|u| u.id == b.id)?)));
                match kept {
                    Some((p, b, unit)) => {
                        for d in &b.docs {
                            for (sel, range) in p.map.of_doc(*d) {
                                if sel.unit().is_some_and(|unit| unit != b.id) {
                                    continue;
                                }
                                map.insert(sel, range);
                            }
                        }
                        if let Some(added) = p.map.added.get(&b.id) { map.added.insert(b.id, added.clone()); }
                        for (comp, site) in p.map.value_sites_of(b.id) {
                            map.set_value_site(comp, site);
                        }
                        for ((comp, pin), sites) in p.map.pin_sites_of(b.id) {
                            map.set_pin_sites(comp, pin, sites);
                        }
                        for (part, site) in p.map.body_sites_of(b.id) {
                            map.set_body_site(b.id, part, site);
                        }
                        units.push(unit.clone());
                        evals.push(UnitEval { id, name: stem, docs, texts, state: UnitState::Kept(diags), reused: false, notes: vec![], took, phases: None });
                    }
                    None => evals.push(UnitEval { id, name: stem, docs, texts, state: UnitState::Failed(diags), reused: false, notes: vec![], took, phases: None }),
                }
            }
        }
    }
    for doc in present.iter().filter(|d| d.kind == DocKind::Board) {
        let stem = doc.name.strip_suffix(DocKind::Board.suffix()).unwrap_or(&doc.name);
        let circuit = format!("{stem}{}", DocKind::Circuit.suffix());
        if sources.find(&circuit).is_none() {
            notes.push(located(doc.id, 0..0, format!("{} has no circuit document {circuit}", doc.name)));
        }
    }
    let benches = crate::bench::evaluate(prev, sources, libs, &units, &evals, &mut map, cancel).ok_or(EvalError::Cancelled)?;
    for unit in &mut units {
        // A bench-only edit reuses the circuit and can remove its last supply.
        unit.supplied.clear();
        for bench in &benches {
            if bench.scope == BenchScope::Design(unit.id) && !matches!(bench.state, UnitState::Failed(_)) {
                unit.supplied.extend(bench.spec.supplies.iter().map(|s| s.net.net).filter(|net| net.ix() < unit.circuit.nets.len()));
            }
        }
        unit.supplied.sort_unstable();
        unit.supplied.dedup();
    }
    if units.is_empty() && evals.iter().any(|e| !e.state.diags().is_empty()) {
        return Err(EvalError::Diags(evals.into_iter().flat_map(|e| e.state.diags().to_vec()).collect()));
    }
    let (links, mounts, system) = lower_system(sources, &units, prev.map(|p| &p.map), &mut map);
    if sources.all_docs().iter().any(|d| !d.present) {
        map.retain_docs(|doc| sources.doc(doc).is_some() || libs.doc(doc).is_some());
    }
    classify_unit_refs(&mut units, &benches, &mut map);
    let design = Design {
        units,
        links,
        assembly: Assembly { mounts, spacers: vec![], order: vec![] },
        bindings: vec![],
        generation: 0,
    };
    Ok(Evaluated {
        libs: libs.clone(),
        assets: assets.to_vec(),
        design,
        map,
        units: evals,
        unit_ids,
        benches,
        notes,
        system,
        defs: prev.filter(|prev| prev.libs.same(libs) && prev.defs.matches(sources, libs))
            .map_or_else(|| Arc::new(ProjectDefs::new(sources, libs)), |prev| prev.defs.clone()),
    })
}

/// Classify automatic refs against the same project-wide call sites the splice sees, including
/// project blocks instantiated by a bench's own circuit. Two linear passes over components.
/// A reused circuit is copied only when a tag changes; a bench's snapshot of a copied circuit
/// then names the copy, as a reused evaluation's does.
fn classify_unit_refs(units: &mut [Unit], benches: &[BenchOut], map: &mut SourceMap) {
    let site = |board, comp| map.locate(&Sel::Comp(CompRef { board, comp }));
    let bench_circuits = benches.iter().filter_map(|b| match &b.scope {
        BenchScope::Own { circuit, .. } => Some(circuit),
        BenchScope::Design(_) => None,
    });
    let mut comps_per_call = HashMap::new();
    for circuit in units.iter().map(|u| &u.circuit).chain(bench_circuits) {
        for comp in &circuit.comps {
            if let Some(at) = site(circuit.unit, comp.id) {
                *comps_per_call.entry(at).or_insert(0usize) += 1;
            }
        }
    }
    let mut copied = Vec::new();
    for unit in units {
        let before = unit.circuit.clone();
        for i in 0..unit.circuit.comps.len() {
            let comp = &unit.circuit.comps[i];
            if comp.ref_kind == RefKind::Stored { continue; }
            // A kept circuit can outlive its source document and have no editable site.
            let kind = site(unit.id, comp.id).map_or(RefKind::Shared, |at| circuit::automatic_ref_kind(&at, &comps_per_call));
            if kind != comp.ref_kind {
                Arc::make_mut(&mut unit.circuit).comps[i].ref_kind = kind;
            }
        }
        if !Arc::ptr_eq(&before, &unit.circuit) { copied.push((before, unit.circuit.clone())); }
    }
    if copied.is_empty() { return; }
    for stimulus in map.bench_stimuli.values_mut() {
        if let crate::bench::Stimulus::Circuit(circuit) = stimulus {
            if let Some((_, now)) = copied.iter().find(|(before, _)| Arc::ptr_eq(before, circuit)) {
                *circuit = now.clone();
            }
        }
    }
}

fn lower_system(sources: &Sources, units: &[Unit], ledger_map: Option<&SourceMap>, map: &mut SourceMap) -> (Vec<Link>, Vec<Mount>, SysEval) {
    let mut mounts = units.iter().map(|u| Mount { unit: u.id, parent: Frame::World, xform: Xform3::IDENTITY }).collect();
    let mut links = Vec::new();
    let mut system = SysEval::default();
    for doc in sources.docs().iter().filter(|d| d.kind == DocKind::System) {
        if system.doc.is_some() {
            system.diags.push(crate::source::located(doc.id, 0..doc.text.len() as u32, "a project has only one system document"));
            continue;
        }
        let start = Instant::now();
        let ledger = ledger_map.map(|m| crate::sys_doc::SysLedger::of_map(m, doc.id));
        let lowered = crate::sys_doc::lower(doc.id, &doc.text, units, ledger.as_ref());
        links = lowered.links;
        mounts = lowered.mounts;
        for (sel, span) in lowered.spans {
            map.insert(sel, SourceRange { doc: doc.id, span });
        }
        system = SysEval { doc: Some(doc.id), diags: lowered.diags, took: start.elapsed() };
    }
    (links, mounts, system)
}

/// Re-lower after a text step on the system document (with `map` followed through the edit),
/// or after the host adopts a unit's new circuit. Keeps link identities from `map` and replaces
/// only the system document's link entries.
///
/// Cost: one parse and lowering of the system document, O(statements + units), no VM.
pub fn relower_system(sources: &Sources, units: &[Unit], map: &mut SourceMap) -> SystemLowered {
    // Read the ledger while the old entries still exist, staging only the new system spans.
    let mut lowered_map = SourceMap::new();
    let (links, mounts, system) = lower_system(sources, units, Some(map), &mut lowered_map);
    if let Some(doc) = system.doc {
        for (sel, _) in map.of_doc(doc) {
            if matches!(sel, Sel::Link(_)) { map.remove(&sel); }
        }
        for (sel, range) in lowered_map.of_doc(doc) {
            map.insert(sel, range);
        }
    }
    SystemLowered { links, mounts, system }
}

/// Check that the system text, using `map`'s ledger against `design.units`, agrees with the
/// model's links (including ids and order) and mounts, without changing the map. The host runs
/// this before saving and recovers a mismatch with `relower_system`.
///
/// Cost: one lowering of the system document, O(statements + units + links), no VM.
pub fn check_system(sources: &Sources, design: &Design, map: &SourceMap) -> Result<(), Diag> {
    let mut lowered_map = SourceMap::new();
    // The text's own diags (a dangling end) are content the model shares, not a mismatch.
    let (links, mounts, system) = lower_system(sources, &design.units, Some(map), &mut lowered_map);
    let source = system.doc.and_then(|doc| sources.doc(doc));
    let error = |sel: Sel, text_link: bool, message: String| {
        let mut diag = match source {
            Some(doc) => {
                let range = text_link.then(|| map.locate(&sel)).flatten().filter(|r| r.doc == doc.id);
                let span = range.map_or(0..doc.text.len() as u32, |r| r.span);
                crate::source::located(doc.id, span, message)
            }
            None => Diag::error(message, vec![]),
        };
        diag.sel.push(sel);
        diag
    };
    for i in 0..links.len().max(design.links.len()) {
        let (text, model) = (links.get(i), design.links.get(i));
        if text == model { continue; }
        let (id, text_link, message) = match (text, model) {
            (Some(text), Some(model)) => {
                if text.id != model.id {
                    if !links.iter().any(|l| l.id == model.id) {
                        return Err(error(Sel::Link(model.id), false, format!("link {} is in the model but not in the text", model.id.0)));
                    }
                    if !design.links.iter().any(|l| l.id == text.id) {
                        return Err(error(Sel::Link(text.id), true, format!("link {} is in the text but not in the model", text.id.0)));
                    }
                }
                let detail = if text.id != model.id {
                    format!("at position {}, the text has link {}, the model link {}", i + 1, text.id.0, model.id.0)
                } else if text.kind != model.kind {
                    format!("the text has a {}, the model a {}", crate::sys_doc::link_kind(text.kind), crate::sys_doc::link_kind(model.kind))
                } else if text.a != model.a {
                    format!("end a differs: text {:?}, model {:?}", text.a, model.a)
                } else if text.b != model.b {
                    format!("end b differs: text {:?}, model {:?}", text.b, model.b)
                } else {
                    format!("pin map differs: text {:?}, model {:?}", text.pinmap, model.pinmap)
                };
                (text.id, true, format!("link {}: {detail}", text.id.0))
            }
            (Some(text), None) => (text.id, true, format!("link {} is in the text but not in the model", text.id.0)),
            (None, Some(model)) => (model.id, false, format!("link {} is in the model but not in the text", model.id.0)),
            (None, None) => unreachable!(),
        };
        return Err(error(Sel::Link(id), text_link, message));
    }
    for i in 0..mounts.len().max(design.assembly.mounts.len()) {
        let (text, model) = (mounts.get(i), design.assembly.mounts.get(i));
        if text == model { continue; }
        let (unit, detail) = match (text, model) {
            (Some(text), Some(model)) => {
                let detail = if text.unit != model.unit {
                    format!("differs: text unit {:?}, model unit {:?}", text.unit, model.unit)
                } else if text.parent != model.parent {
                    format!("differs: text on {:?}, model on {:?}", text.parent, model.parent)
                } else if text.xform.t != model.xform.t {
                    format!("differs: text at {:?}, model at {:?}", text.xform.t, model.xform.t)
                } else {
                    format!("differs: text rotation {:?}, model rotation {:?}", text.xform.q, model.xform.q)
                };
                (text.unit, detail)
            }
            (Some(text), None) => (text.unit, "is in the text but not in the model".into()),
            (None, Some(model)) => (model.unit, "is in the model but not in the text".into()),
            (None, None) => unreachable!(),
        };
        let name = design.unit(unit).map_or_else(|| format!("unit {}", unit.0), |u| u.name.clone());
        return Err(error(Sel::Unit(unit), false, format!("{name}'s mount {detail}")));
    }
    Ok(())
}

/// One unit from its circuit document and, when the project has one, its board document.
type Built = (Unit, Vec<(Sel, SourceRange)>, Vec<(CompId, u32)>, Vec<(CompRef, ValueSite)>, Vec<((CompRef, PinIx), Vec<PinSite>)>, Vec<(String, crate::map::BodySite)>, Phases, Vec<Diag>);

fn build_unit(id: BoardId, stem: &str, doc: &Doc, board_doc: Option<&Doc>, sources: &Sources, libs: &Libs, prev: Option<&Evaluated>) -> Result<Built, Vec<Diag>> {
    let UnitOut { circuit, notes, map: mut entries, added, values, pins, bodies, phases } = circuit::run_unit(id, stem, doc, sources, libs)?;
    let (board, factory) = match board_doc {
        Some(b) => {
            let ledger = prev.and_then(|p| {
                let unit = p.units.iter().find(|u| u.id == id && u.docs.get(1) == Some(&b.id))?;
                let board = p.design.units.iter().find(|u| u.id == unit.id)?.board.as_ref()?;
                Some(crate::board_doc::Ledger::of_map(&p.map, id, b.id, board.next_item))
            });
            let lowered = crate::board_doc::lower(b.id, &b.text, &circuit, ledger.as_ref())?;
            entries.extend(lowered.spans.into_iter().map(|(sel, span)| (sel, SourceRange { doc: b.id, span })));
            (lowered.board, lowered.factory)
        }
        None => {
            let mut board = Board::new(id, &circuit.name);
            board.mirror(&circuit);
            (board, None)
        }
    };
    let unit = Unit {
        id,
        name: circuit.name.clone(),
        kind: UnitKind::Board,
        circuit: Arc::new(circuit),
        supplied: Vec::new(),
        board: Some(Arc::new(board)),
        factory,
    };
    Ok((unit, entries, added, values, pins, bodies, phases, notes))
}
